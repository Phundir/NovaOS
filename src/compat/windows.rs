//! 10.4.2 -- The Windows subsystem.
//!
//! ## Why this one is harder than the Linux one, and it is not the format
//!
//! A static Linux binary talks to the kernel directly: `syscall` with a
//! number in RAX. A Windows binary essentially never does. It calls
//! `kernel32.dll`, which calls `ntdll.dll`, which issues the syscall --
//! and the numbers `ntdll` uses are undocumented and change between
//! builds of Windows. Reimplementing them would be reimplementing a
//! moving target nobody publishes.
//!
//! So the translation happens at the **NT API** level, which is
//! documented and stable, rather than at the syscall number level:
//!
//! ```text
//!   programme --> kernel32!WriteFile   (absent, and named as such)
//!             --> ntdll!NtWriteFile    <-- the layer this file describes
//!             --> syscall 0x08         <-- undocumented, not reproduced
//! ```
//!
//! What this subsystem therefore offers is: a complete PE32+ parser, the
//! path translation, the handle model, and a table mapping the NT calls
//! onto Nova operations. What it does **not** offer is the DLLs. A PE
//! importing `kernel32.dll` is refused by name -- exactly as
//! `dev build --lang rust` is refused for want of a compiler.
//!
//! A PE with **no imports** whose code issues `syscall` with the NT
//! numbers below does run, and the self-test builds one. That is a narrow
//! class, and it is a real one: it is what a freestanding Windows
//! executable looks like.
//!
//! ## The calling convention is not the same, and that part is real work
//!
//! Windows x64 passes RCX, RDX, R8, R9 then the stack. The `syscall`
//! instruction destroys RCX -- it puts the return address there -- so
//! `ntdll` moves RCX into R10 before issuing it, and the kernel reads
//! R10, RDX, R8, R9. Nova reads RDI, RSI, RDX, R10, R8, R9. The
//! registers therefore have to be **moved**, which is the one place where
//! this subsystem does more than renumber.

use alloc::string::String;
use alloc::vec::Vec;

use crate::sdk::asm::Object;
use crate::syscall::SyscallFrame;

use super::pe::{self, Pe};

// ============================================================
//  NTSTATUS
// ============================================================

/// The status codes this subsystem returns.
///
/// Real NT values: a program compares against the constants its own
/// headers gave it, and an invented code would be read as success by the
/// `NT_SUCCESS` macro if it happened to have a clear top bit.
pub mod status {
    pub const SUCCESS: u64 = 0x0000_0000;
    pub const UNSUCCESSFUL: u64 = 0xC000_0001;
    pub const NOT_IMPLEMENTED: u64 = 0xC000_0002;
    pub const INVALID_HANDLE: u64 = 0xC000_0008;
    pub const INVALID_PARAMETER: u64 = 0xC000_000D;
    pub const NO_SUCH_FILE: u64 = 0xC000_000F;
    pub const ACCESS_DENIED: u64 = 0xC000_0022;
    pub const OBJECT_NAME_NOT_FOUND: u64 = 0xC000_0034;
    pub const NO_MEMORY: u64 = 0xC000_0017;
}

/// Is a status a success? The NT rule: the top bit clear.
pub fn succeeded(value: u64) -> bool {
    value & 0x8000_0000 == 0
}

// ============================================================
//  The NT calls
// ============================================================

/// How far a call is carried.
pub use super::linux::Support;

/// One entry of the NT table.
pub type Call = (u64, &'static str, Support);

// The numbers are this subsystem's own, and that is a decision worth
// stating: the real ones differ between Windows builds, so copying one
// build's numbers would be copying a version rather than an interface.
// A program built for Nova's Windows subsystem uses these; a program
// built for Windows uses `ntdll`, which is absent, and is refused by
// name at load time rather than mis-dispatched here.
pub const NT_WRITE_FILE: u64 = 0x0008;
pub const NT_READ_FILE: u64 = 0x0006;
pub const NT_CREATE_FILE: u64 = 0x0055;
pub const NT_CLOSE: u64 = 0x000F;
pub const NT_DELETE_FILE: u64 = 0x00D2;
pub const NT_ALLOCATE_VIRTUAL_MEMORY: u64 = 0x0018;
pub const NT_FREE_VIRTUAL_MEMORY: u64 = 0x001E;
pub const NT_TERMINATE_PROCESS: u64 = 0x002C;
pub const NT_DELAY_EXECUTION: u64 = 0x0034;
pub const NT_YIELD_EXECUTION: u64 = 0x0046;
pub const NT_QUERY_SYSTEM_TIME: u64 = 0x0057;
pub const NT_QUERY_INFORMATION_PROCESS: u64 = 0x0019;
pub const NT_CREATE_THREAD: u64 = 0x004E;
pub const NT_CREATE_PROCESS: u64 = 0x00B2;

pub static CALLS: &[Call] = &[
    (NT_READ_FILE, "NtReadFile", Support::Full),
    (NT_WRITE_FILE, "NtWriteFile", Support::Full),
    (NT_CLOSE, "NtClose", Support::Full),
    (NT_CREATE_FILE, "NtCreateFile", Support::Full),
    (NT_DELETE_FILE, "NtDeleteFile", Support::Full),
    (
        NT_ALLOCATE_VIRTUAL_MEMORY,
        "NtAllocateVirtualMemory",
        Support::Full,
    ),
    (NT_FREE_VIRTUAL_MEMORY, "NtFreeVirtualMemory", Support::Full),
    (NT_TERMINATE_PROCESS, "NtTerminateProcess", Support::Full),
    (NT_DELAY_EXECUTION, "NtDelayExecution", Support::Full),
    (NT_YIELD_EXECUTION, "NtYieldExecution", Support::Full),
    (NT_QUERY_SYSTEM_TIME, "NtQuerySystemTime", Support::Full),
    (
        NT_QUERY_INFORMATION_PROCESS,
        "NtQueryInformationProcess",
        Support::Stub,
    ),
    // A thread shares its address space with its process, and Nova gives
    // every task its own stack slot out of one hierarchy of pages: two
    // tasks would share everything, which is not a thread, it is a race.
    (NT_CREATE_THREAD, "NtCreateThread", Support::Absent),
    (NT_CREATE_PROCESS, "NtCreateProcess", Support::Absent),
];

pub fn call_name(number: u64) -> Option<&'static str> {
    CALLS
        .iter()
        .find(|(code, _, _)| *code == number)
        .map(|(_, name, _)| *name)
}

pub fn support(number: u64) -> Option<Support> {
    CALLS
        .iter()
        .find(|(code, _, _)| *code == number)
        .map(|(_, _, level)| *level)
}

/// Libraries whose absence makes a PE unrunnable.
///
/// Listed rather than "any import at all", so the refusal names the
/// missing piece. A program importing only its own DLL gets a different
/// message from one importing `kernel32`, and the two call for different
/// answers from whoever reads it.
pub static SYSTEM_LIBRARIES: &[&str] = &[
    "kernel32.dll",
    "ntdll.dll",
    "user32.dll",
    "gdi32.dll",
    "advapi32.dll",
    "msvcrt.dll",
    "ucrtbase.dll",
    "shell32.dll",
    "ole32.dll",
    "ws2_32.dll",
];

// ============================================================
//  Paths
// ============================================================

/// Translates a Windows path into a Nova one.
///
/// Three shapes have to be handled, and the third is the one a naive
/// translation misses:
///
/// * `C:\Users\nova\a.txt` -- a drive letter and backslashes;
/// * `\??\C:\...` or `\\?\C:\...` -- the NT object-manager form, which is
///   what `NtCreateFile` actually receives;
/// * `a\b.txt` -- relative, which resolves under the application's root.
///
/// The drive letter maps onto the Nova volume letter of the same name
/// when there is one, and onto the application's root otherwise. `C:` is
/// the system volume, which Nova calls `/`.
pub fn translate_path(app_id: &str, path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }

    // The NT prefixes, longest first: `\\?\` contains `\?` and stripping
    // the short one first would leave a stray backslash.
    let mut rest = path;
    for prefix in ["\\\\?\\", "\\??\\", "\\\\.\\"] {
        if let Some(stripped) = rest.strip_prefix(prefix) {
            rest = stripped;
            break;
        }
    }

    // `NUL` is a device, not a file: writes vanish. Same treatment as
    // `/dev/null` on the Linux side, and for the same reason.
    let bare = rest.trim_end_matches('\\');
    if bare.eq_ignore_ascii_case("NUL") || bare.eq_ignore_ascii_case("\\\\.\\NUL") {
        return None;
    }

    let bytes = rest.as_bytes();
    let (drive, remainder) = if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
    {
        (Some(bytes[0].to_ascii_uppercase()), &rest[2..])
    } else {
        (None, rest)
    };

    // Backslashes become slashes. Done on the remainder only: a drive
    // letter has none, and running it through would turn `C:` into
    // something with a colon in the middle of a Nova path.
    let mut nova = String::new();
    for character in remainder.chars() {
        nova.push(if character == '\\' { '/' } else { character });
    }

    let translated = match drive {
        // `C:` is the system volume, which Nova calls `/`.
        Some(b'C') | None if nova.starts_with('/') => nova,
        // Another letter: the Nova volume of that letter, if it is
        // mounted. Nova already names its volumes by letter, so the two
        // models meet here without an invention.
        Some(letter) if letter != b'C' => {
            let mounted = crate::fs::disk_list()
                .iter()
                .any(|disk| disk.letter.eq_ignore_ascii_case(&(letter as char)));
            if mounted {
                alloc::format!("{}:{}", letter as char, nova)
            } else {
                return None;
            }
        }
        // Relative: under the application's own root, inside its sandbox.
        _ => {
            return Some(crate::fs::normalize(&alloc::format!(
                "/apps/{}/rootfs/{}",
                app_id,
                nova.trim_start_matches('/')
            )));
        }
    };

    Some(crate::fs::normalize(&translated))
}

// ============================================================
//  Conversion of an image
// ============================================================

/// Why a PE cannot be run by this subsystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadRefusal {
    Malformed(pe::PeError),
    /// The image is a .NET assembly: its code is bytecode for a runtime
    /// that does not exist here.
    Managed,
    /// It imports libraries this system does not have.
    MissingLibraries(Vec<String>),
    EntryOutsideCode,
    NoSection,
    TooManyWritableSections(usize),
    TooLarge(u64),
}

impl LoadRefusal {
    pub fn message(&self) -> String {
        match self {
            LoadRefusal::Malformed(error) => error.message(),
            LoadRefusal::Managed => {
                String::from("assemblage .NET : son code est du bytecode, pas du natif")
            }
            LoadRefusal::MissingLibraries(names) => {
                alloc::format!("bibliotheques absentes : {}", names.join(", "))
            }
            LoadRefusal::EntryOutsideCode => {
                String::from("point d'entree hors d'une section executable")
            }
            LoadRefusal::NoSection => String::from("aucune section chargeable"),
            LoadRefusal::TooManyWritableSections(count) => alloc::format!(
                "{} sections inscriptibles : l'image Nova n'en porte qu'une",
                count
            ),
            LoadRefusal::TooLarge(size) => alloc::format!(
                "image de {} octets : au-dela de ce qu'un creneau accepte",
                size
            ),
        }
    }
}

const MAX_IMAGE: u64 = 0x0018_0000;

/// Converts a PE32+ with no system imports into a Nova object.
pub fn to_object(image: &Pe) -> Result<Object, LoadRefusal> {
    if image.managed {
        return Err(LoadRefusal::Managed);
    }

    let missing: Vec<String> = image
        .libraries()
        .into_iter()
        .filter(|library| SYSTEM_LIBRARIES.contains(&library.as_str()))
        .collect();
    if !missing.is_empty() {
        return Err(LoadRefusal::MissingLibraries(missing));
    }

    let sections = image.loadable();
    if sections.is_empty() {
        return Err(LoadRefusal::NoSection);
    }
    if !image.entry_is_executable() {
        return Err(LoadRefusal::EntryOutsideCode);
    }

    let writable: Vec<&pe::Section> = sections.iter().filter(|s| s.writable()).collect();
    if writable.len() > 1 {
        return Err(LoadRefusal::TooManyWritableSections(writable.len()));
    }

    let span = sections
        .iter()
        .map(|section| {
            section.virtual_address as u64 + section.virtual_size.max(section.raw_size) as u64
        })
        .max()
        .unwrap_or(0);
    if span > MAX_IMAGE {
        return Err(LoadRefusal::TooLarge(span));
    }

    // Executable sections become `.text`, writable ones `.data`,
    // uninitialised ones `.bss`. Same shape as the ELF conversion, and
    // for the same reason: the Nova image has exactly those three, and a
    // section that fitted none of them would have to be given a home it
    // does not have.
    let mut text = Vec::new();
    let mut text_base = None;
    let mut data = Vec::new();
    let mut data_base = None;
    let mut bss_size = 0u64;

    for section in sections.iter() {
        let span = section.virtual_size.max(section.raw_size) as usize;
        if section.is_bss() && section.raw_size == 0 {
            bss_size += span as u64;
            continue;
        }
        let (target, base) = if section.writable() {
            data_base.get_or_insert(section.virtual_address);
            (&mut data, data_base.unwrap())
        } else {
            text_base.get_or_insert(section.virtual_address);
            (&mut text, text_base.unwrap())
        };
        let offset = (section.virtual_address - base) as usize;
        if offset + span > target.len() {
            target.resize(offset + span, 0);
        }
    }

    let text_base = text_base.ok_or(LoadRefusal::NoSection)?;

    Ok(Object {
        text,
        data,
        bss_size,
        symbols: Vec::new(),
        fixups: Vec::new(),
        entry: (image.entry_rva - text_base) as u64,
        instructions: 0,
    })
}

/// Copies the raw bytes of the sections into the object.
pub fn fill(object: &mut Object, image: &Pe, data: &[u8]) {
    let sections = image.loadable();
    let text_base = sections
        .iter()
        .find(|section| !section.writable() && !(section.is_bss() && section.raw_size == 0))
        .map(|section| section.virtual_address);
    let data_base = sections
        .iter()
        .find(|section| section.writable())
        .map(|section| section.virtual_address);

    for section in sections.iter() {
        if section.raw_size == 0 {
            continue;
        }
        let (target, base) = if section.writable() {
            (&mut object.data, data_base)
        } else {
            (&mut object.text, text_base)
        };
        let Some(base) = base else {
            continue;
        };
        let offset = (section.virtual_address - base) as usize;
        let start = section.raw_offset as usize;
        let end = start + section.raw_size as usize;
        if end > data.len() || offset + section.raw_size as usize > target.len() {
            continue;
        }
        target[offset..offset + section.raw_size as usize].copy_from_slice(&data[start..end]);
    }
}

// ============================================================
//  The call itself
// ============================================================

/// Runs one NT call on behalf of a foreign task.
///
/// ## The registers move
///
/// Windows x64 puts its first four arguments in RCX, RDX, R8, R9. The
/// `syscall` instruction overwrites RCX with the return address, so the
/// convention is that the caller has already moved RCX into R10. The
/// kernel therefore reads **R10, RDX, R8, R9**, and the fifth argument
/// onward come from the stack -- which this subsystem does not read: no
/// NT call in the table above takes more than four.
pub fn call(frame: &mut SyscallFrame, state: &mut super::TaskState) -> u64 {
    let number = frame.rax;
    let args = [frame.r10, frame.rdx, frame.r8, frame.r9];
    state.calls += 1;

    let result = match number {
        NT_WRITE_FILE => write(args[0] as i64, args[1], args[2], state),
        NT_READ_FILE => read(args[0] as i64, args[1], args[2], state),
        NT_CREATE_FILE => create_file(args[0], args[1], args[2], state),
        NT_CLOSE => {
            if state.close(args[0] as i64) {
                status::SUCCESS
            } else {
                status::INVALID_HANDLE
            }
        }
        NT_DELETE_FILE => delete_file(args[0], state),
        NT_ALLOCATE_VIRTUAL_MEMORY => allocate(args[0], args[1]),
        NT_FREE_VIRTUAL_MEMORY => free(args[0], args[1]),
        NT_DELAY_EXECUTION => delay(args[0]),
        NT_YIELD_EXECUTION => {
            crate::scheduler::yield_now();
            status::SUCCESS
        }
        NT_QUERY_SYSTEM_TIME => query_time(args[0]),
        NT_QUERY_INFORMATION_PROCESS => status::NOT_IMPLEMENTED,
        NT_TERMINATE_PROCESS => {
            crate::serial_println!(
                "[windows] PID {} se termine avec le statut 0x{:x} apres {} appel(s)",
                crate::scheduler::current_pid(),
                args[1],
                state.calls
            );
            super::forget(crate::scheduler::current_pid());
            crate::scheduler::exit_current_with(args[1] as i64);
        }
        other => {
            state.refused += 1;
            crate::serial_println!(
                "[windows] appel 0x{:04x} ({}) non gere - STATUS_NOT_IMPLEMENTED",
                other,
                call_name(other).unwrap_or("inconnu")
            );
            status::NOT_IMPLEMENTED
        }
    };

    if !succeeded(result) {
        state.errors += 1;
    }
    result
}

/// Handle of the console, which every process is born holding.
///
/// NT handles are opaque and `ntdll` gets them from the process
/// environment block. There is no PEB here, so the console handle is a
/// constant -- and it is documented rather than magic.
pub const HANDLE_STDOUT: i64 = 1;
pub const HANDLE_STDERR: i64 = 2;

fn write(handle: i64, buffer: u64, length: u64, state: &mut super::TaskState) -> u64 {
    if handle == HANDLE_STDOUT || handle == HANDLE_STDERR {
        return match crate::syscall::invoke_user(
            crate::syscall::domain_core::SYS_DEBUG_PRINT,
            [buffer, length, 0, 0, 0, 0],
        ) {
            Ok(_) => status::SUCCESS,
            Err(_) => status::INVALID_PARAMETER,
        };
    }

    let Some(index) = state.descriptor(handle) else {
        return status::INVALID_HANDLE;
    };
    let path = state.descriptors[index].path.clone();
    if path.is_empty() {
        return status::SUCCESS;
    }
    let Ok(bytes) = crate::syscall::read_user_bytes(buffer, length) else {
        return status::INVALID_PARAMETER;
    };
    let who = crate::scheduler::current_credentials();
    match crate::fs::write_file(
        &path,
        &bytes,
        who,
        crate::fs::perm::Mode::private_file(),
        crate::idt::ticks(),
    ) {
        Ok(_) => status::SUCCESS,
        Err(_) => status::ACCESS_DENIED,
    }
}

fn read(handle: i64, buffer: u64, length: u64, state: &mut super::TaskState) -> u64 {
    let Some(index) = state.descriptor(handle) else {
        return status::INVALID_HANDLE;
    };
    let path = state.descriptors[index].path.clone();
    if path.is_empty() {
        return status::SUCCESS;
    }
    let offset = state.descriptors[index].offset as usize;
    let who = crate::scheduler::current_credentials();
    let Ok(content) = crate::fs::read_file(&path, who) else {
        return status::NO_SUCH_FILE;
    };
    if offset >= content.len() {
        return status::SUCCESS;
    }
    let end = (offset + length as usize).min(content.len());
    match crate::syscall::write_user_bytes(buffer, &content[offset..end]) {
        Ok(written) => {
            state.descriptors[index].offset += written;
            status::SUCCESS
        }
        Err(_) => status::INVALID_PARAMETER,
    }
}

/// `NtCreateFile(handle_out, path, access)`.
///
/// Reduced from the real eleven parameters: the object attributes, the
/// I/O status block, the allocation size, the file attributes, the share
/// mode, the disposition and the create options are all things this
/// system has no equivalent for, and taking them only to ignore them
/// would suggest they were honoured.
fn create_file(handle_out: u64, path_ptr: u64, access: u64, state: &mut super::TaskState) -> u64 {
    let Ok(path) = crate::syscall::read_user_cstr(path_ptr, 512) else {
        return status::INVALID_PARAMETER;
    };
    // Bit 1 of the NT access mask is GENERIC_WRITE in its simplified
    // form; the full mask is a bit field this subsystem does not
    // reproduce.
    let writable = access & 2 != 0;

    let nova_path = match translate_path(&state.app_id, &path) {
        Some(translated) => translated,
        // `NUL`, or a drive that is not mounted.
        None if path.eq_ignore_ascii_case("NUL") => String::new(),
        None => return status::OBJECT_NAME_NOT_FOUND,
    };

    if !nova_path.is_empty() {
        if !crate::security::check_path(crate::scheduler::current_pid(), &nova_path, writable)
            .allowed()
        {
            return status::ACCESS_DENIED;
        }
        if !writable
            && crate::fs::stat(&nova_path, crate::scheduler::current_credentials()).is_err()
        {
            return status::OBJECT_NAME_NOT_FOUND;
        }
    }

    let Some(handle) = state.open(nova_path, writable) else {
        return status::NO_MEMORY;
    };
    match crate::syscall::write_user_bytes(handle_out, &handle.to_le_bytes()) {
        Ok(_) => status::SUCCESS,
        Err(_) => {
            state.close(handle);
            status::INVALID_PARAMETER
        }
    }
}

fn delete_file(path_ptr: u64, state: &super::TaskState) -> u64 {
    let Ok(path) = crate::syscall::read_user_cstr(path_ptr, 512) else {
        return status::INVALID_PARAMETER;
    };
    let Some(nova_path) = translate_path(&state.app_id, &path) else {
        return status::OBJECT_NAME_NOT_FOUND;
    };
    if !crate::security::check_path(crate::scheduler::current_pid(), &nova_path, true).allowed() {
        return status::ACCESS_DENIED;
    }
    match crate::fs::delete(
        &nova_path,
        crate::scheduler::current_credentials(),
        crate::idt::ticks(),
    ) {
        Ok(()) => status::SUCCESS,
        Err(crate::fs::FsError::NotFound) => status::NO_SUCH_FILE,
        Err(_) => status::ACCESS_DENIED,
    }
}

/// `NtAllocateVirtualMemory(base_out, size)`.
fn allocate(base_out: u64, size: u64) -> u64 {
    match crate::syscall::invoke_user(
        crate::syscall::domain_mem::SYS_MEM_ALLOC,
        [size, 0, 0, 0, 0, 0],
    ) {
        Ok(address) => match crate::syscall::write_user_bytes(base_out, &address.to_le_bytes()) {
            Ok(_) => status::SUCCESS,
            Err(_) => status::INVALID_PARAMETER,
        },
        Err(_) => status::NO_MEMORY,
    }
}

fn free(base: u64, size: u64) -> u64 {
    match crate::syscall::invoke_user(
        crate::syscall::domain_mem::SYS_MEM_FREE,
        [base, size, 0, 0, 0, 0],
    ) {
        Ok(_) => status::SUCCESS,
        Err(_) => status::INVALID_PARAMETER,
    }
}

/// `NtDelayExecution(interval)`.
///
/// The interval is a 64-bit count of hundreds of nanoseconds, negative
/// for a relative delay. That sign convention is the whole subtlety: a
/// positive value is an absolute date since 1601, and treating it as a
/// duration would sleep for four centuries.
fn delay(interval: u64) -> u64 {
    let Ok(bytes) = crate::syscall::read_user_bytes(interval, 8) else {
        return status::INVALID_PARAMETER;
    };
    let value = i64::from_le_bytes(bytes[..8].try_into().unwrap_or([0; 8]));
    if value >= 0 {
        // An absolute date. Refused rather than approximated: waiting
        // until a date the machine cannot place is not a delay.
        return status::NOT_IMPLEMENTED;
    }
    let milliseconds = (value.unsigned_abs()) / 10_000;

    match crate::syscall::invoke_user(
        crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
        [milliseconds, 0, 0, 0, 0, 0],
    ) {
        Ok(_) => status::SUCCESS,
        Err(_) => status::INVALID_PARAMETER,
    }
}

/// Seconds between 1 January 1601 and 1 January 1970.
///
/// The NT epoch is the start of the 400-year Gregorian cycle containing
/// 1601, which is why the number is not round. Getting it wrong shifts
/// every timestamp by centuries, and a program that formats the result
/// shows it immediately -- which is the one mercy of this constant.
pub const NT_EPOCH_OFFSET: u64 = 11_644_473_600;

fn query_time(target: u64) -> u64 {
    let unix = crate::rtc::now().unwrap_or(0);
    // Hundreds of nanoseconds since 1601.
    let nt = (unix + NT_EPOCH_OFFSET).saturating_mul(10_000_000);
    match crate::syscall::write_user_bytes(target, &nt.to_le_bytes()) {
        Ok(_) => status::SUCCESS,
        Err(_) => status::INVALID_PARAMETER,
    }
}

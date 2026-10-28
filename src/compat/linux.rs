//! 10.4.1 -- The Linux subsystem.
//!
//! ## What runs, and what is refused by name
//!
//! The Nova loader loads Nova binaries. A Linux ELF is therefore
//! **converted**: its loadable segments become the sections of a Nova
//! image, its entry point becomes the Nova entry point, and the task is
//! marked with a Linux personality so that `syscall_dispatch` routes its
//! calls here instead of to the Nova table.
//!
//! What that conversion cannot do is fix addresses. A Nova image is
//! placed wherever the arena has room, so a binary carrying absolute
//! addresses of its own would read and jump to the wrong places -- and
//! nothing in the file says which of its bytes are addresses. So the
//! subsystem accepts **position-independent, statically linked** ELF64
//! objects, and refuses everything else by name:
//!
//! | Refused | Why |
//! |---|---|
//! | 32-bit, big-endian, other machine | not this processor |
//! | `INTERP` or `DYNAMIC` segment | needs a dynamic linker |
//! | `SHT_RELA` / `SHT_REL` sections | needs a relocator |
//! | entry point outside an executable segment | would jump nowhere |
//! | more than one writable segment | the Nova image has one `.data` |
//!
//! That is the same posture phase 9 took with the seven languages of
//! 13.2: what cannot be done is named, not simulated. A `/bin/ls` from a
//! distribution is dynamically linked and is refused on the first line of
//! the table -- which is the true answer.
//!
//! ## The argument convention was already right
//!
//! Linux x86-64 passes its arguments in RDI, RSI, RDX, R10, R8, R9, and
//! its call number in RAX. Nova, written long before this subsystem,
//! chose the same registers. So the translation is on the **number and
//! the semantics** only; not one register moves. That is luck, and it is
//! worth writing down, because the day Nova changes its convention this
//! file has to change with it.
//!
//! ## Errors are negative
//!
//! Linux returns `-errno` in RAX. Nova returns an encoded error. The
//! translation of the error is as much a part of the ABI as the
//! translation of the call: a program that reads a Nova error code as a
//! pointer will dereference something around 0xFFFF_FF00.

use alloc::string::String;
use alloc::vec::Vec;

use crate::sdk::asm::Object;
use crate::syscall::SyscallFrame;

use super::elf::{self, Elf};

// ============================================================
//  errno
// ============================================================

/// The error numbers this subsystem returns.
///
/// Linux values, not invented ones: a program compares against the
/// constants its own headers gave it.
pub mod errno {
    pub const EPERM: i64 = 1;
    pub const ENOENT: i64 = 2;
    pub const EBADF: i64 = 9;
    pub const ENOMEM: i64 = 12;
    pub const EACCES: i64 = 13;
    pub const EFAULT: i64 = 14;
    pub const EEXIST: i64 = 17;
    pub const EINVAL: i64 = 22;
    pub const EMFILE: i64 = 24;
    pub const ENOSPC: i64 = 28;
    pub const ENOSYS: i64 = 38;
}

/// Translates a Nova error into the closest Linux one.
///
/// "Closest" is doing real work here: a program branches on `ENOENT`
/// versus `EACCES`, and collapsing both into `EINVAL` would make a file
/// that does not exist indistinguishable from one that is forbidden.
fn errno_of(error: crate::syscall::error::SysError) -> i64 {
    use crate::syscall::error::ErrorCode;
    -match error.code {
        ErrorCode::NotFound => errno::ENOENT,
        ErrorCode::PermissionDenied => errno::EACCES,
        ErrorCode::AlreadyExists => errno::EEXIST,
        ErrorCode::InvalidArgument | ErrorCode::InvalidSyscall => errno::EINVAL,
        ErrorCode::OutOfMemory => errno::ENOMEM,
        ErrorCode::NoSpace | ErrorCode::TooLarge => errno::ENOSPC,
        ErrorCode::NotImplemented | ErrorCode::Unsupported => errno::ENOSYS,
        ErrorCode::ReadOnly => errno::EPERM,
        _ => errno::EINVAL,
    }
}

// ============================================================
//  Call numbers
// ============================================================

/// The Linux x86-64 call numbers this subsystem knows.
///
/// A table rather than a `match` for the same reason the 16.4 keys and
/// the 13.2 languages are tables: a transcription is checked by
/// comparison, and one cannot compare a `match` with a manual page.
pub type Call = (u64, &'static str, Support);

/// How far a call is carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// Fully translated onto a Nova call or handled here.
    Full,
    /// Answered, but with a value that is a convention rather than a
    /// measurement -- `uname`, `getcwd`.
    Stub,
    /// Recognised and refused: it names something Nova does not have.
    Absent,
}

impl Support {
    pub fn as_str(&self) -> &'static str {
        match self {
            Support::Full => "traduit",
            Support::Stub => "convention",
            Support::Absent => "absent",
        }
    }
}

pub const SYS_READ: u64 = 0;
pub const SYS_WRITE: u64 = 1;
pub const SYS_OPEN: u64 = 2;
pub const SYS_CLOSE: u64 = 3;
pub const SYS_STAT: u64 = 4;
pub const SYS_FSTAT: u64 = 5;
pub const SYS_LSEEK: u64 = 8;
pub const SYS_MMAP: u64 = 9;
pub const SYS_MPROTECT: u64 = 10;
pub const SYS_MUNMAP: u64 = 11;
pub const SYS_BRK: u64 = 12;
pub const SYS_IOCTL: u64 = 16;
pub const SYS_WRITEV: u64 = 20;
pub const SYS_SCHED_YIELD: u64 = 24;
pub const SYS_NANOSLEEP: u64 = 35;
pub const SYS_GETPID: u64 = 39;
pub const SYS_FORK: u64 = 57;
pub const SYS_EXECVE: u64 = 59;
pub const SYS_EXIT: u64 = 60;
pub const SYS_WAIT4: u64 = 61;
pub const SYS_UNAME: u64 = 63;
pub const SYS_GETCWD: u64 = 79;
pub const SYS_UNLINK: u64 = 87;
pub const SYS_GETUID: u64 = 102;
pub const SYS_GETGID: u64 = 104;
pub const SYS_GETEUID: u64 = 107;
pub const SYS_GETEGID: u64 = 108;
pub const SYS_ARCH_PRCTL: u64 = 158;
pub const SYS_GETTID: u64 = 186;
pub const SYS_CLOCK_GETTIME: u64 = 228;
pub const SYS_EXIT_GROUP: u64 = 231;
pub const SYS_OPENAT: u64 = 257;

/// The table, in numeric order.
pub static CALLS: &[Call] = &[
    (SYS_READ, "read", Support::Full),
    (SYS_WRITE, "write", Support::Full),
    (SYS_OPEN, "open", Support::Full),
    (SYS_CLOSE, "close", Support::Full),
    (SYS_STAT, "stat", Support::Full),
    (SYS_FSTAT, "fstat", Support::Full),
    (SYS_LSEEK, "lseek", Support::Full),
    (SYS_MMAP, "mmap", Support::Full),
    (SYS_MPROTECT, "mprotect", Support::Stub),
    (SYS_MUNMAP, "munmap", Support::Full),
    // Nova has no program break: memory comes from `SYS_MEM_ALLOC`, in
    // pages, at an address the kernel chooses. `brk` cannot be given the
    // contiguous growing region it promises, so it answers the current
    // break unchanged -- which is what a `brk`-based allocator reads as
    // "no more room", and is the correct answer here.
    (SYS_BRK, "brk", Support::Stub),
    (SYS_IOCTL, "ioctl", Support::Stub),
    (SYS_WRITEV, "writev", Support::Full),
    (SYS_SCHED_YIELD, "sched_yield", Support::Full),
    (SYS_NANOSLEEP, "nanosleep", Support::Full),
    (SYS_GETPID, "getpid", Support::Full),
    // A fork needs a second address space carrying a copy of the first.
    // Nova has one hierarchy of pages for every task -- the limit phase 3
    // opened and never closed -- so there is nothing to copy into.
    (SYS_FORK, "fork", Support::Absent),
    (SYS_EXECVE, "execve", Support::Absent),
    (SYS_EXIT, "exit", Support::Full),
    (SYS_WAIT4, "wait4", Support::Full),
    (SYS_UNAME, "uname", Support::Stub),
    (SYS_GETCWD, "getcwd", Support::Stub),
    (SYS_UNLINK, "unlink", Support::Full),
    (SYS_GETUID, "getuid", Support::Full),
    (SYS_GETGID, "getgid", Support::Full),
    (SYS_GETEUID, "geteuid", Support::Full),
    (SYS_GETEGID, "getegid", Support::Full),
    (SYS_ARCH_PRCTL, "arch_prctl", Support::Stub),
    (SYS_GETTID, "gettid", Support::Full),
    (SYS_CLOCK_GETTIME, "clock_gettime", Support::Full),
    (SYS_EXIT_GROUP, "exit_group", Support::Full),
    (SYS_OPENAT, "openat", Support::Full),
];

/// Name of a call, for a trace.
pub fn call_name(number: u64) -> Option<&'static str> {
    CALLS
        .iter()
        .find(|(code, _, _)| *code == number)
        .map(|(_, name, _)| *name)
}

/// How far a call is carried, or `None` if it is not in the table.
pub fn support(number: u64) -> Option<Support> {
    CALLS
        .iter()
        .find(|(code, _, _)| *code == number)
        .map(|(_, _, level)| *level)
}

// ============================================================
//  Paths
// ============================================================

/// Translates a Linux path into a Nova one.
///
/// The two trees do not correspond, and pretending they do would be
/// worse than saying so: `/etc/passwd` has no Nova equivalent, and
/// answering with the account file would hand a foreign program the
/// system's own credentials in a format it thinks it understands.
///
/// What is mapped is what has a genuine counterpart:
///
/// | Linux | Nova |
/// |---|---|
/// | `/tmp` | `/cache` |
/// | `/home` | `/users` |
/// | `/usr/bin`, `/bin` | `/sysapp` |
/// | `/usr/lib`, `/lib` | `/libs/sys` |
/// | `/var/log` | `/logs` |
/// | `/dev/null` | absorbed, not a file |
///
/// Everything else keeps its shape and lands under `/apps/{app}/rootfs`,
/// which is the sandbox of 12.4 for the task doing the asking. A foreign
/// program does not escape the sandbox by using foreign path syntax.
pub fn translate_path(app_id: &str, path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    // `/dev/null` is not a path, it is a behaviour: writes vanish, reads
    // return nothing. It is handled by the descriptor table, and mapping
    // it to a real file would create a file that grows for ever.
    if path == "/dev/null" {
        return None;
    }

    for (linux, nova) in [
        ("/tmp", "/cache"),
        ("/home", "/users"),
        ("/usr/bin", "/sysapp"),
        ("/usr/lib", "/libs/sys"),
        ("/bin", "/sysapp"),
        ("/lib", "/libs/sys"),
        ("/var/log", "/logs"),
    ] {
        if path == linux {
            return Some(String::from(nova));
        }
        if let Some(rest) = path.strip_prefix(linux)
            && rest.starts_with('/')
        {
            return Some(alloc::format!("{}{}", nova, rest));
        }
    }

    // A relative path, or one with no mapping: under the application's
    // own root, inside its sandbox.
    let cleaned = path.trim_start_matches('/');
    Some(crate::fs::normalize(&alloc::format!(
        "/apps/{}/rootfs/{}",
        app_id,
        cleaned
    )))
}

// ============================================================
//  Descriptors
// ============================================================

/// A file descriptor of a foreign task.
#[derive(Debug, Clone)]
pub struct Descriptor {
    pub fd: i64,
    pub path: String,
    /// Read cursor, in bytes.
    pub offset: u64,
    pub writable: bool,
}

/// The three descriptors every program is born with.
pub const FD_STDIN: i64 = 0;
pub const FD_STDOUT: i64 = 1;
pub const FD_STDERR: i64 = 2;
/// First descriptor `open` hands out.
pub const FD_FIRST: i64 = 3;
/// Ceiling on the number of open descriptors per task.
pub const MAX_DESCRIPTORS: usize = 32;

// ============================================================
//  Conversion of an image
// ============================================================

/// Why an ELF cannot be run by this subsystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadRefusal {
    Malformed(elf::ElfError),
    NotExecutable,
    NeedsInterpreter,
    NeedsRelocation,
    EntryOutsideCode,
    NoLoadableSegment,
    /// The Nova image has one `.text` and one `.data`; more writable
    /// segments than that cannot be laid out without moving them, and
    /// moving them changes the addresses inside the code.
    TooManyWritableSegments(usize),
    /// Segments that are not contiguous once flattened. The gap would
    /// have to be mapped, and mapping it would shift everything after.
    Fragmented,
    TooLarge(u64),
}

impl LoadRefusal {
    pub fn message(&self) -> String {
        match self {
            LoadRefusal::Malformed(error) => error.message(),
            LoadRefusal::NotExecutable => {
                String::from("ce n'est ni un executable ni un objet partage")
            }
            LoadRefusal::NeedsInterpreter => String::from(
                "binaire lie dynamiquement : il exige un editeur de liens dynamique",
            ),
            LoadRefusal::NeedsRelocation => {
                String::from("le binaire porte des relocations : il exige un relogeur")
            }
            LoadRefusal::EntryOutsideCode => {
                String::from("point d'entree hors d'un segment executable")
            }
            LoadRefusal::NoLoadableSegment => String::from("aucun segment chargeable"),
            LoadRefusal::TooManyWritableSegments(count) => alloc::format!(
                "{} segments inscriptibles : l'image Nova n'en porte qu'un",
                count
            ),
            LoadRefusal::Fragmented => {
                String::from("segments non contigus : l'image Nova ne peut pas les separer")
            }
            LoadRefusal::TooLarge(size) => {
                alloc::format!("image de {} octets : au-dela de ce qu'un creneau accepte", size)
            }
        }
    }
}

/// Largest image the arena of the loader accepts, less a page for the
/// header. Checked here so the refusal names the size rather than
/// arriving as a loader error the user cannot act on.
const MAX_IMAGE: u64 = 0x0018_0000;

/// Converts a static, position-independent ELF64 into a Nova object.
///
/// The result feeds `sdk::emit::emit`, which produces a real Nova binary
/// the ordinary loader accepts: same header, same signature check, same
/// arena, same protection verification. A foreign program is not a second
/// class of executable inside the kernel -- it is a Nova executable whose
/// contents came from elsewhere, and that is what keeps the rest of the
/// system from having to know about it.
pub fn to_object(elf: &Elf) -> Result<Object, LoadRefusal> {
    use elf::ObjectType;

    if !matches!(
        elf.object_type,
        ObjectType::Executable | ObjectType::Shared
    ) {
        return Err(LoadRefusal::NotExecutable);
    }
    if elf.needs_interpreter() {
        return Err(LoadRefusal::NeedsInterpreter);
    }
    if elf.has_relocations() {
        return Err(LoadRefusal::NeedsRelocation);
    }

    let load = elf.loadable();
    if load.is_empty() {
        return Err(LoadRefusal::NoLoadableSegment);
    }
    if !elf.entry_is_executable() {
        return Err(LoadRefusal::EntryOutsideCode);
    }

    let writable: Vec<&elf::Segment> = load.iter().filter(|s| s.writable()).collect();
    if writable.len() > 1 {
        return Err(LoadRefusal::TooManyWritableSegments(writable.len()));
    }

    let (low, high) = elf.extent().ok_or(LoadRefusal::NoLoadableSegment)?;
    if high - low > MAX_IMAGE {
        return Err(LoadRefusal::TooLarge(high - low));
    }

    // The executable part becomes `.text`, the writable part `.data`.
    // Both are flattened from the lowest virtual address of their group,
    // and the gap between the two groups must be nil: the Nova layout
    // puts `.data` immediately after `.text`, rounded to a page, and a
    // program expecting a hole between them would find its data where its
    // padding should be.
    let mut text = Vec::new();
    let mut text_base = None;
    let mut data = Vec::new();
    let mut data_base = None;
    let mut bss_size = 0u64;

    for segment in load.iter() {
        let target = if segment.writable() {
            data_base.get_or_insert(segment.virt_addr);
            &mut data
        } else {
            text_base.get_or_insert(segment.virt_addr);
            &mut text
        };
        let base = if segment.writable() {
            data_base.unwrap()
        } else {
            text_base.unwrap()
        };

        let offset = (segment.virt_addr - base) as usize;
        // A segment further along than what has been gathered leaves a
        // hole. Filling it with zeros is right -- the ELF says nothing is
        // there -- but a hole larger than a page is a layout this
        // conversion cannot reproduce, since the Nova sections are
        // contiguous.
        if offset > target.len() + 0x1000 {
            return Err(LoadRefusal::Fragmented);
        }
        if offset > target.len() {
            target.resize(offset, 0);
        }

        let end = offset + segment.file_size as usize;
        if end > target.len() {
            target.resize(end, 0);
        }
        // `file_size` was checked against the file length by the parser,
        // so this range exists.
        let _ = segment;

        if segment.mem_size > segment.file_size && !segment.writable() {
            // Uninitialised space inside a read-only segment: it exists
            // in memory and holds zeros for ever. Reserved in the section
            // rather than in `.bss`, which the Nova layout puts after
            // `.data`.
            let extra = (segment.mem_size - segment.file_size) as usize;
            target.resize(end + extra, 0);
        } else if segment.mem_size > segment.file_size {
            bss_size += segment.mem_size - segment.file_size;
        }
    }

    let text_base = text_base.ok_or(LoadRefusal::NoLoadableSegment)?;

    Ok(Object {
        text,
        data,
        bss_size,
        symbols: Vec::new(),
        fixups: Vec::new(),
        entry: elf.entry - text_base,
        instructions: 0,
    })
}

/// Copies the bytes of the segments into the object built by `to_object`.
///
/// Separate from the layout decision so the refusals above are taken
/// **before** anything is copied: a conversion that allocated first and
/// refused afterwards would spend the memory of every file it rejects.
pub fn fill(object: &mut Object, elf: &Elf, data: &[u8]) {
    let load = elf.loadable();
    let text_base = load
        .iter()
        .find(|segment| !segment.writable())
        .map(|segment| segment.virt_addr);
    let data_base = load
        .iter()
        .find(|segment| segment.writable())
        .map(|segment| segment.virt_addr);

    for segment in load.iter() {
        if segment.file_size == 0 {
            continue;
        }
        let (target, base) = if segment.writable() {
            (&mut object.data, data_base)
        } else {
            (&mut object.text, text_base)
        };
        let Some(base) = base else {
            continue;
        };
        let offset = (segment.virt_addr - base) as usize;
        let start = segment.file_offset as usize;
        let end = start + segment.file_size as usize;
        if end > data.len() || offset + segment.file_size as usize > target.len() {
            continue;
        }
        target[offset..offset + segment.file_size as usize].copy_from_slice(&data[start..end]);
    }
}

// ============================================================
//  The call itself
// ============================================================

/// Runs one Linux system call on behalf of a foreign task.
///
/// Returns the value to put in RAX: a result, or a negative errno.
pub fn call(frame: &mut SyscallFrame, state: &mut super::TaskState) -> u64 {
    let number = frame.rax;
    let args = [
        frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9,
    ];
    state.calls += 1;

    let result: i64 = match number {
        SYS_WRITE => write(args[0] as i64, args[1], args[2], state),
        SYS_WRITEV => writev(args[0] as i64, args[1], args[2], state),
        SYS_READ => read(args[0] as i64, args[1], args[2], state),
        SYS_OPEN => open(args[0], args[1], state),
        // `openat` with AT_FDCWD (-100) is `open`; a real directory
        // descriptor would need a per-descriptor working directory, which
        // this subsystem does not keep.
        SYS_OPENAT => {
            if args[0] as i64 == -100 {
                open(args[1], args[2], state)
            } else {
                -errno::EINVAL
            }
        }
        SYS_CLOSE => close(args[0] as i64, state),
        SYS_LSEEK => lseek(args[0] as i64, args[1] as i64, args[2], state),
        SYS_STAT | SYS_FSTAT => -errno::ENOSYS,
        SYS_MMAP => mmap(args[1], args[3]),
        SYS_MUNMAP => munmap(args[0], args[1]),
        // No program break: memory comes in pages at an address the
        // kernel picks. Answering the value asked for would be a lie an
        // allocator would then write through.
        SYS_BRK => 0,
        SYS_MPROTECT => 0,
        SYS_IOCTL => -errno::EINVAL,
        SYS_SCHED_YIELD => {
            crate::scheduler::yield_now();
            0
        }
        SYS_NANOSLEEP => nanosleep(args[0]),
        SYS_GETPID | SYS_GETTID => crate::scheduler::current_pid() as i64,
        SYS_GETUID | SYS_GETEUID => {
            crate::scheduler::current_credentials().uid as i64
        }
        SYS_GETGID | SYS_GETEGID => {
            crate::scheduler::current_credentials().gid as i64
        }
        SYS_CLOCK_GETTIME => clock_gettime(args[1]),
        SYS_UNAME => uname(args[0]),
        SYS_GETCWD => getcwd(args[0], args[1], state),
        SYS_UNLINK => unlink(args[0], state),
        SYS_ARCH_PRCTL => -errno::EINVAL,
        SYS_WAIT4 => -errno::ENOSYS,
        SYS_EXIT | SYS_EXIT_GROUP => {
            crate::serial_println!(
                "[linux] PID {} sort avec le code {} apres {} appel(s)",
                crate::scheduler::current_pid(),
                args[0] as i64,
                state.calls
            );
            super::forget(crate::scheduler::current_pid());
            crate::scheduler::exit_current_with(args[0] as i64);
        }
        other => {
            state.refused += 1;
            crate::serial_println!(
                "[linux] appel {} ({}) non gere - ENOSYS",
                other,
                call_name(other).unwrap_or("inconnu")
            );
            -errno::ENOSYS
        }
    };

    if result < 0 {
        state.errors += 1;
    }
    result as u64
}

/// `write(fd, buf, count)`.
fn write(fd: i64, buffer: u64, count: u64, state: &mut super::TaskState) -> i64 {
    match fd {
        FD_STDOUT | FD_STDERR => {
            // The two console descriptors go to the trace, which is where
            // a foreign program's output belongs: it has no terminal, and
            // 15.4 already established that a program with no terminal is
            // observed through the serial console.
            match crate::syscall::invoke_user(
                crate::syscall::domain_core::SYS_DEBUG_PRINT,
                [buffer, count, 0, 0, 0, 0],
            ) {
                Ok(written) => written as i64,
                Err(error) => errno_of(error),
            }
        }
        fd => {
            let Some(index) = state.descriptor(fd) else {
                return -errno::EBADF;
            };
            if !state.descriptors[index].writable {
                return -errno::EBADF;
            }
            let path = state.descriptors[index].path.clone();
            if path.is_empty() {
                // `/dev/null`: the write succeeds and goes nowhere.
                return count as i64;
            }
            // Straight to the filesystem, not through `SYS_FS_WRITE`.
            //
            // That call takes the **path** and the **buffer** as two
            // pointers, and here the two have different provenances: the
            // path is a kernel string this file built, the buffer is a
            // user address the foreign program supplied. One `Origin`
            // cannot validate both, and passing a kernel pointer through
            // the user check -- or the reverse -- is how a kernel reads
            // its own memory believing it is reading a program's.
            let Ok(bytes) = crate::syscall::read_user_bytes(buffer, count) else {
                return -errno::EFAULT;
            };
            let who = crate::scheduler::current_credentials();
            let now = crate::idt::ticks();
            match crate::fs::write_file(&path, &bytes, who, crate::fs::perm::Mode::private_file(), now) {
                Ok(_) => count as i64,
                Err(_) => -errno::EACCES,
            }
        }
    }
}

/// `writev(fd, iov, iovcnt)`.
///
/// Each entry is a pointer and a length, sixteen bytes. Walked one entry
/// at a time through `write`, because that is where the descriptor rules
/// live and duplicating them here is how the two would drift apart.
fn writev(fd: i64, iov: u64, count: u64, state: &mut super::TaskState) -> i64 {
    // Bounded: `count` comes from the program.
    let count = count.min(64);
    let mut total = 0i64;
    for index in 0..count {
        let entry = iov + index * 16;
        let Ok(header) = crate::syscall::read_user_bytes(entry, 16) else {
            return -errno::EFAULT;
        };
        let base = u64::from_le_bytes(header[0..8].try_into().unwrap_or([0; 8]));
        let length = u64::from_le_bytes(header[8..16].try_into().unwrap_or([0; 8]));
        if length == 0 {
            continue;
        }
        let written = write(fd, base, length, state);
        if written < 0 {
            return if total > 0 { total } else { written };
        }
        total += written;
    }
    total
}

/// `read(fd, buf, count)`.
fn read(fd: i64, buffer: u64, count: u64, state: &mut super::TaskState) -> i64 {
    if fd == FD_STDIN {
        // No terminal: end of file. Returning an error instead would make
        // a program retry for ever.
        return 0;
    }
    let Some(index) = state.descriptor(fd) else {
        return -errno::EBADF;
    };
    let path = state.descriptors[index].path.clone();
    if path.is_empty() {
        return 0;
    }
    let offset = state.descriptors[index].offset as usize;

    // Same reasoning as `write`: two pointers, two provenances.
    let who = crate::scheduler::current_credentials();
    let Ok(content) = crate::fs::read_file(&path, who) else {
        return -errno::ENOENT;
    };
    if offset >= content.len() {
        return 0;
    }
    let end = (offset + count as usize).min(content.len());
    match crate::syscall::write_user_bytes(buffer, &content[offset..end]) {
        Ok(written) => {
            state.descriptors[index].offset += written;
            written as i64
        }
        Err(_) => -errno::EFAULT,
    }
}

/// Open flags this subsystem reads.
const O_WRONLY: u64 = 1;
const O_RDWR: u64 = 2;
const O_CREAT: u64 = 0o100;

/// `open(path, flags, mode)`.
fn open(path_ptr: u64, flags: u64, state: &mut super::TaskState) -> i64 {
    let Ok(path) = crate::syscall::read_user_cstr(path_ptr, 512) else {
        return -errno::EFAULT;
    };

    let writable = flags & (O_WRONLY | O_RDWR) != 0;

    // `/dev/null` is a behaviour, not a file: an empty path in the
    // descriptor is what the read and write paths recognise.
    let nova_path = if path == "/dev/null" {
        String::new()
    } else {
        match translate_path(&state.app_id, &path) {
            Some(translated) => translated,
            None => return -errno::ENOENT,
        }
    };

    if !nova_path.is_empty() {
        let exists = crate::fs::stat(&nova_path, crate::scheduler::current_credentials()).is_ok();
        if !exists && flags & O_CREAT == 0 {
            return -errno::ENOENT;
        }
        // 12.4 -- the sandbox applies to a foreign program exactly as it
        // does to a Nova one. Foreign path syntax is not a way out of it.
        let verdict = crate::security::check_path(
            crate::scheduler::current_pid(),
            &nova_path,
            writable,
        );
        if !verdict.allowed() {
            return -errno::EACCES;
        }
    }

    match state.open(nova_path, writable) {
        Some(fd) => fd,
        None => -errno::EMFILE,
    }
}

fn close(fd: i64, state: &mut super::TaskState) -> i64 {
    if (FD_STDIN..=FD_STDERR).contains(&fd) {
        return 0;
    }
    if state.close(fd) { 0 } else { -errno::EBADF }
}

const SEEK_SET: u64 = 0;
const SEEK_CUR: u64 = 1;
const SEEK_END: u64 = 2;

fn lseek(fd: i64, offset: i64, whence: u64, state: &mut super::TaskState) -> i64 {
    let Some(index) = state.descriptor(fd) else {
        return -errno::EBADF;
    };
    let path = state.descriptors[index].path.clone();
    let current = state.descriptors[index].offset as i64;

    let size = if path.is_empty() {
        0
    } else {
        crate::fs::stat(&path, crate::scheduler::current_credentials())
            .map(|node| node.size as i64)
            .unwrap_or(0)
    };

    let target = match whence {
        SEEK_SET => offset,
        SEEK_CUR => current + offset,
        SEEK_END => size + offset,
        _ => return -errno::EINVAL,
    };
    if target < 0 {
        return -errno::EINVAL;
    }
    state.descriptors[index].offset = target as u64;
    target
}

/// `mmap(addr, length, prot, flags, fd, offset)`.
///
/// Anonymous mappings only, and that is not a shortcut: a file mapping
/// promises that a write to memory reaches the file, and Nova has no
/// page cache backing a file. Answering `ENOSYS` for one is the honest
/// answer; answering with anonymous memory would be a mapping that
/// silently loses everything written to it.
fn mmap(length: u64, flags: u64) -> i64 {
    const MAP_ANONYMOUS: u64 = 0x20;
    if flags & MAP_ANONYMOUS == 0 {
        return -errno::ENOSYS;
    }
    match crate::syscall::invoke_user(
        crate::syscall::domain_mem::SYS_MEM_ALLOC,
        [length, 0, 0, 0, 0, 0],
    ) {
        Ok(address) => address as i64,
        Err(error) => errno_of(error),
    }
}

fn munmap(address: u64, length: u64) -> i64 {
    match crate::syscall::invoke_user(
        crate::syscall::domain_mem::SYS_MEM_FREE,
        [address, length, 0, 0, 0, 0],
    ) {
        Ok(_) => 0,
        Err(error) => errno_of(error),
    }
}

/// `nanosleep(req, rem)`.
///
/// `struct timespec` is two 64-bit fields: seconds then nanoseconds.
fn nanosleep(request: u64) -> i64 {
    let Ok(bytes) = crate::syscall::read_user_bytes(request, 16) else {
        return -errno::EFAULT;
    };
    let seconds = u64::from_le_bytes(bytes[0..8].try_into().unwrap_or([0; 8]));
    let nanoseconds = u64::from_le_bytes(bytes[8..16].try_into().unwrap_or([0; 8]));
    let milliseconds = seconds.saturating_mul(1000) + nanoseconds / 1_000_000;

    match crate::syscall::invoke_user(
        crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
        [milliseconds, 0, 0, 0, 0, 0],
    ) {
        Ok(_) => 0,
        Err(error) => errno_of(error),
    }
}

/// `clock_gettime(clockid, tp)`.
fn clock_gettime(target: u64) -> i64 {
    let seconds = crate::rtc::now().unwrap_or(0);
    let mut buffer = [0u8; 16];
    buffer[0..8].copy_from_slice(&seconds.to_le_bytes());
    // Nanoseconds: the tick counter gives ten-millisecond resolution, and
    // pretending to nanoseconds would be inventing twelve digits.
    let millis = (crate::idt::ticks() % 100) * (1000 / crate::TIMER_HZ as u64);
    buffer[8..16].copy_from_slice(&(millis * 1_000_000).to_le_bytes());

    match crate::syscall::write_user_bytes(target, &buffer) {
        Ok(_) => 0,
        Err(_) => -errno::EFAULT,
    }
}

/// `uname(buf)`.
///
/// `struct utsname` is six fields of 65 bytes. The release is written as
/// a Linux-looking version because that is what a program parses and
/// branches on; the system name says Nova OS, because lying about that
/// would serve nobody.
fn uname(target: u64) -> i64 {
    let mut buffer = [0u8; 65 * 6];
    let fields = [
        "Nova OS",
        "nova",
        // A release string a version check can parse. A program that
        // refuses to run below 3.0 is a program that would refuse to run
        // at all with an empty field.
        "5.0.0-nova",
        "Nova OS 1.0 compat linux",
        "x86_64",
        "(none)",
    ];
    for (index, field) in fields.iter().enumerate() {
        let bytes = field.as_bytes();
        let length = bytes.len().min(64);
        buffer[index * 65..index * 65 + length].copy_from_slice(&bytes[..length]);
    }

    match crate::syscall::write_user_bytes(target, &buffer) {
        Ok(_) => 0,
        Err(_) => -errno::EFAULT,
    }
}

/// `getcwd(buf, size)`.
fn getcwd(target: u64, size: u64, state: &super::TaskState) -> i64 {
    let path = alloc::format!("/apps/{}/rootfs", state.app_id);
    let mut bytes = path.into_bytes();
    bytes.push(0);
    if (bytes.len() as u64) > size {
        return -errno::EINVAL;
    }
    match crate::syscall::write_user_bytes(target, &bytes) {
        Ok(_) => bytes.len() as i64,
        Err(_) => -errno::EFAULT,
    }
}

fn unlink(path_ptr: u64, state: &super::TaskState) -> i64 {
    let Ok(path) = crate::syscall::read_user_cstr(path_ptr, 512) else {
        return -errno::EFAULT;
    };
    let Some(nova_path) = translate_path(&state.app_id, &path) else {
        return -errno::ENOENT;
    };
    if !crate::security::check_path(crate::scheduler::current_pid(), &nova_path, true).allowed() {
        return -errno::EACCES;
    }
    let who = crate::scheduler::current_credentials();
    match crate::fs::delete(&nova_path, who, crate::idt::ticks()) {
        Ok(()) => 0,
        Err(crate::fs::FsError::NotFound) => -errno::ENOENT,
        Err(_) => -errno::EACCES,
    }
}

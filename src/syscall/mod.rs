//! System call gate and complete system API (spec 6).
//!
//! The `syscall`/`sysret` mechanism replaces the old `int 0x80` software
//! gate: it avoids walking the IDT and reading the TSS, at the price of
//! one constraint -- the CPU does not change stack, it is up to the
//! kernel to do it (see boot/syscall_entry.asm).
//!
//! ## Layout
//!
//! This file holds only the **mechanism**: MSR configuration, register
//! frame, validation of the user pointers, dispatch. Each domain of spec
//! 6.2 to 6.6 lives in its own module and publishes a table:
//!
//! ```text
//!   0x00xx  domain_core   service: trace, last error, description
//!   0x01xx  domain_fs     filesystem                 spec 6.2
//!   0x02xx  domain_proc   processes                  spec 6.3
//!   0x03xx  domain_mem    memory                     spec 6.4
//!   0x04xx  domain_ipc    communication              spec 6.5
//!   0x05xx  domain_user   accounts and permissions   spec 6.6
//!   0x06xx  domain_hw     hardware                   spec 6.6
//!   0x07xx  domain_time   time                       spec 6.6
//!   0x08xx  domain_net    network                    spec 6.6
//! ```
//!
//! ## Calling convention
//!
//! ```text
//!   RAX          call number
//!   RDI RSI RDX  arguments 1 to 3
//!   R10 R8 R9    arguments 4 to 6
//!   RAX          result: positive or zero on success,
//!                opposite of the error code otherwise
//! ```
//!
//! `R10` replaces `RCX` because the `syscall` instruction overwrites the
//! latter with the return address -- it is the same constraint as under
//! Linux, and for the same hardware reason.
//!
//! ## Two origins, one implementation
//!
//! A call may come from ring 3 through the `syscall` instruction, or from
//! the kernel itself through `invoke`. The difference comes down to one
//! point: the provenance of the pointers. A pointer from ring 3 must be
//! proven to be a user pointer before being dereferenced; a pointer from
//! the kernel is so by construction. All the rest -- table, rights,
//! modes, errors -- is rigorously the same code, which makes the API
//! testable without going through a ring 3 program, without creating a
//! privileged path ring 3 could take.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::arch::asm;

use crate::fs::perm::Credentials;
use crate::memory;
use crate::process::Pid;
use crate::scheduler;
use crate::vga::SpinLock;

pub mod domain_core;
pub mod domain_fs;
pub mod domain_hw;
pub mod domain_ipc;
pub mod domain_mem;
pub mod domain_net;
pub mod domain_proc;
pub mod domain_time;
pub mod domain_user;
pub mod error;
pub mod ported;
pub mod selftest;
pub mod table;

pub use error::{ErrorCode, SysError, SysResult};
pub use table::Mode;

// ============================================================
//  MSR
// ============================================================

const IA32_EFER: u32 = 0xC000_0080;
const IA32_STAR: u32 = 0xC000_0081;
const IA32_LSTAR: u32 = 0xC000_0082;
const IA32_FMASK: u32 = 0xC000_0084;
const IA32_GS_BASE: u32 = 0xC000_0101;
const IA32_KERNEL_GS_BASE: u32 = 0xC000_0102;

/// EFER.SCE -- allows the `syscall` instruction.
const EFER_SCE: u64 = 1 << 0;

unsafe fn read_msr(msr: u32) -> u64 {
    let (high, low): (u32, u32);
    unsafe {
        asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high,
             options(nomem, nostack, preserves_flags));
    }
    ((high as u64) << 32) | (low as u64)
}

unsafe fn write_msr(msr: u32, value: u64) {
    unsafe {
        asm!("wrmsr", in("ecx") msr, in("eax") value as u32, in("edx") (value >> 32) as u32,
             options(nostack, preserves_flags));
    }
}

// ============================================================
//  Per-core structure
// ============================================================

/// Reachable from assembly through `gs:`. The field order is frozen:
/// syscall_entry.asm hard-codes their offsets.
#[repr(C)]
pub struct PerCpu {
    /// Top of the kernel stack adopted on entry to a system call.
    pub kernel_rsp: u64,
    /// Save slot of the user RSP.
    pub user_rsp: u64,
    pub cpu_id: u64,
}

impl PerCpu {
    const fn new() -> Self {
        PerCpu {
            kernel_rsp: 0,
            user_rsp: 0,
            cpu_id: 0,
        }
    }
}

static mut PERCPU: [PerCpu; crate::gdt::MAX_CPUS] =
    [const { PerCpu::new() }; crate::gdt::MAX_CPUS];

/// Fills in the kernel stack used by the system calls of this core.
///
/// # Safety
/// `stack_top` must name the top of a valid stack, 16-byte aligned, not
/// used elsewhere.
pub unsafe fn set_kernel_stack(cpu_id: usize, stack_top: u64) {
    unsafe {
        let percpu = (&raw mut PERCPU as *mut PerCpu).add(cpu_id);
        (*percpu).kernel_rsp = stack_top;
        (*percpu).cpu_id = cpu_id as u64;
    }
}

/// Top of the kernel stack published for this core, or 0 if no task has
/// been elected on it yet.
///
/// Used by `scheduler::stack_headroom`: it is the only stack bound
/// readable without taking the scheduler lock, which matters for code
/// called from a deep recursion.
pub fn kernel_stack(cpu_id: usize) -> u64 {
    if cpu_id >= crate::gdt::MAX_CPUS {
        return 0;
    }
    // The field is only written by `set_kernel_stack`, on the core
    // querying it and outside any interrupt: the read cannot see a
    // half-laid value.
    unsafe {
        let percpu = (&raw const PERCPU as *const PerCpu).add(cpu_id);
        (*percpu).kernel_rsp
    }
}

// ============================================================
//  Call frame
// ============================================================

/// Image of the registers at the moment of the call, as syscall_entry.asm
/// builds it. The field order matches exactly the order of the pushes,
/// reversed.
#[repr(C)]
#[derive(Debug)]
pub struct SyscallFrame {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    /// Resume address in user space.
    pub rip: u64,
    pub rflags: u64,
    pub user_rsp: u64,
}

impl SyscallFrame {
    /// Arguments in convention order: RDI, RSI, RDX, R10, R8, R9.
    fn args(&self) -> [u64; 6] {
        [self.rdi, self.rsi, self.rdx, self.r10, self.r8, self.r9]
    }
}

// ============================================================
//  Context of a call
// ============================================================

/// Provenance of a call, and hence the status of its pointers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// `syscall` instruction executed in ring 3.
    User,
    /// Direct call from the kernel -- self-test, internal service.
    Kernel,
}

/// Everything a domain handler needs to know.
pub struct Call {
    pub number: u16,
    pub args: [u64; 6],
    pub origin: Origin,
    pub mode: Mode,
    pub pid: Pid,
}

/// Maximum size of a transfer between the two spaces, in bytes.
///
/// An outright bound is worth more than an allocation dictated by ring 3:
/// without it, a program would ask for a read of several gibibytes and
/// the kernel would try to allocate it before discovering it cannot.
/// 64 KiB covers a configuration file, a directory listing and an export
/// buffer; beyond that, the caller loops on an offset.
pub const TRANSFER_MAX: u64 = 64 * 1024;

impl Call {
    pub fn arg(&self, index: usize) -> u64 {
        self.args.get(index).copied().unwrap_or(0)
    }

    /// Access context of the caller.
    ///
    /// 5.6.1: derived from the process, never supplied by it. It is the
    /// difference between a kernel that knows who calls it and a kernel
    /// that believes what it is told.
    ///
    /// **The origin says nothing of the identity.** A call emitted from
    /// the kernel by a scheduled task carries the identity of that task,
    /// not that of the kernel: without that, a task that has just given
    /// up its rights through `SYS_USER_SETID` would get them back on the
    /// first internal call. Only the boot code, which is not a task, acts
    /// for the kernel -- and `current_credentials` already returns that.
    pub fn who(&self) -> Credentials {
        scheduler::current_credentials()
    }

    /// Does this call have the right to put its caller to sleep?
    pub fn may_block(&self) -> bool {
        self.mode == Mode::Async
    }

    /// Refuses a blocking operation asked from a synchronous call.
    pub fn require_async(&self, what: &str) -> SysResult<()> {
        if self.may_block() {
            Ok(())
        } else {
            Err(SysError::with(ErrorCode::NotAsync, what))
        }
    }

    /// Reads a byte range supplied by the caller.
    pub fn read_bytes(&self, ptr: u64, len: u64) -> SysResult<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        if len > TRANSFER_MAX {
            return Err(SysError::with(ErrorCode::TooLarge, "tampon d'entree"));
        }
        self.validate(ptr, len, false)?;

        let mut buffer = alloc::vec![0u8; len as usize];
        unsafe {
            core::ptr::copy_nonoverlapping(ptr as *const u8, buffer.as_mut_ptr(), len as usize);
        }
        Ok(buffer)
    }

    /// Reads a string supplied by the caller -- a path, a channel name.
    pub fn read_str(&self, ptr: u64, len: u64) -> SysResult<String> {
        let bytes = self.read_bytes(ptr, len)?;
        // The encoding check is not cosmetic: the whole filesystem
        // handles `&str`, and an invalid UTF-8 sequence would cause a
        // conversion panic instead of an error.
        String::from_utf8(bytes).map_err(|_| error::invalid("chaine non UTF-8"))
    }

    /// Writes a kernel buffer into the space of the caller.
    ///
    /// Returns the number of bytes actually transferred, which may be
    /// less than the source: it is up to the caller to compare with the
    /// size announced to it elsewhere.
    pub fn write_bytes(&self, ptr: u64, len: u64, source: &[u8]) -> SysResult<u64> {
        if len == 0 || source.is_empty() {
            return Ok(0);
        }
        let count = (len as usize).min(source.len());
        self.validate(ptr, count as u64, true)?;
        unsafe {
            core::ptr::copy_nonoverlapping(source.as_ptr(), ptr as *mut u8, count);
        }
        Ok(count as u64)
    }

    fn validate(&self, ptr: u64, len: u64, writable: bool) -> SysResult<()> {
        match self.origin {
            // A pointer from the kernel is a kernel pointer: validating
            // it as a user pointer would always refuse it.
            Origin::Kernel => {
                if ptr == 0 {
                    Err(error::invalid("pointeur nul"))
                } else {
                    Ok(())
                }
            }
            Origin::User => validate_user_range(ptr, len, writable),
        }
    }
}

// ============================================================
//  User memory, outside a call
// ============================================================

/// Reads a byte range from the address space of the current ring 3 task.
///
/// `Call::read_bytes` does the same thing inside a system call, where the
/// origin is known. The compatibility subsystems of 10.4 are outside that
/// path -- they translate a foreign ABI before any Nova call exists -- and
/// need the same bounds check without a `Call` to hang it on.
///
/// The check is the point: these pointers come from a **foreign program**
/// this kernel did not compile, so they are no more trustworthy than
/// those of a Nova one, and arguably less.
pub fn read_user_bytes(ptr: u64, len: u64) -> SysResult<Vec<u8>> {
    if len == 0 {
        return Ok(Vec::new());
    }
    if len > TRANSFER_MAX {
        return Err(SysError::with(ErrorCode::TooLarge, "tampon d'entree"));
    }
    validate_user_range(ptr, len, false)?;

    let mut buffer = alloc::vec![0u8; len as usize];
    unsafe {
        core::ptr::copy_nonoverlapping(ptr as *const u8, buffer.as_mut_ptr(), len as usize);
    }
    Ok(buffer)
}

/// Reads a NUL-terminated string from user space, up to `max` bytes.
///
/// Byte by byte, each one validated: the length is not known in advance,
/// so a range check cannot be done once at the start. A version that
/// validated `max` bytes and then scanned would refuse a perfectly valid
/// short string sitting at the end of a mapped page.
pub fn read_user_cstr(ptr: u64, max: u64) -> SysResult<String> {
    let mut bytes = Vec::new();
    for offset in 0..max {
        let address = ptr.saturating_add(offset);
        validate_user_range(address, 1, false)?;
        let byte = unsafe { (address as *const u8).read_volatile() };
        if byte == 0 {
            return String::from_utf8(bytes).map_err(|_| error::invalid("chaine non UTF-8"));
        }
        bytes.push(byte);
    }
    Err(SysError::with(ErrorCode::TooLarge, "chaine sans terminateur"))
}

/// Writes a kernel buffer into the address space of the current ring 3
/// task.
pub fn write_user_bytes(ptr: u64, source: &[u8]) -> SysResult<u64> {
    if source.is_empty() {
        return Ok(0);
    }
    validate_user_range(ptr, source.len() as u64, true)?;
    unsafe {
        core::ptr::copy_nonoverlapping(source.as_ptr(), ptr as *mut u8, source.len());
    }
    Ok(source.len() as u64)
}

// ============================================================
//  Validation of the user pointers
// ============================================================

/// Upper limit of the canonical user space.
pub const USER_SPACE_LIMIT: u64 = 0x0000_8000_0000_0000;

/// Checks that a range supplied by user space really lies in the lower
/// half of the address space and is actually mapped.
///
/// Without this check, a ring 3 program could make the kernel read an
/// arbitrary address -- the kernel, unlike it, has access to it.
pub fn validate_user_range(ptr: u64, len: u64, writable: bool) -> SysResult<()> {
    if len == 0 {
        return Err(error::invalid("plage vide"));
    }
    if ptr >= USER_SPACE_LIMIT || ptr.saturating_add(len) > USER_SPACE_LIMIT {
        return Err(error::denied("adresse hors de l'espace utilisateur"));
    }

    // A task with a private address space (11.3) is demand-paged: a page
    // it never touched is not in the tables yet. Resolving the range first
    // gives it the same answer as if it had been touched -- and refuses
    // what its regions do not cover.
    // SAFETY: the current task holds its space for the whole call.
    if let Some(space) = unsafe { crate::vm::current() }
        && space.populate(ptr, len, writable).is_err()
    {
        return Err(error::invalid("adresse non mappee"));
    }

    // Every page touched must resolve **and** carry the USER bit across
    // the four levels. Settling for a successful translation would let a
    // low address mapped for the kernel's own use through: ring 3 cannot
    // reach it, but the kernel can -- and that is exactly the hole this
    // validation exists to close.
    let mut addr = ptr & !0xFFF;
    let end = ptr + len;
    while addr < end {
        match unsafe { memory::translate(addr) } {
            Some(t) if t.user => {
                if writable && !t.writable {
                    return Err(error::denied("page utilisateur en lecture seule"));
                }
            }
            Some(_) => return Err(error::denied("page reservee au noyau")),
            None => return Err(error::invalid("adresse non mappee")),
        }
        addr += 4096;
    }
    Ok(())
}

// ============================================================
//  Last error, per task
// ============================================================

/// What `SYS_ERROR_LAST` returns.
#[derive(Clone, Copy)]
pub struct LastError {
    pub number: u16,
    pub error: SysError,
}

/// One entry per task having failed at least once.
///
/// Kept outside `Process`: the scheduler copies its tasks on every
/// snapshot, and lodging a hundred bytes of error there would weigh down
/// every walk for a rare use.
static LAST_ERRORS: SpinLock<Option<BTreeMap<Pid, LastError>>> = SpinLock::new(None);

fn record_error(pid: Pid, number: u16, error: SysError) {
    let mut slot = LAST_ERRORS.lock();
    slot.get_or_insert_with(BTreeMap::new)
        .insert(pid, LastError { number, error });
}

/// Last error met by a task.
pub fn last_error(pid: Pid) -> Option<LastError> {
    let slot = LAST_ERRORS.lock();
    slot.as_ref()?.get(&pid).copied()
}

/// Diagnostic counters.
static CALL_COUNT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static ERROR_COUNT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static BLOCKED_COUNT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Reports that a call really put its caller to sleep -- a measure of the
/// real use of the asynchronous mode.
pub fn count_block() {
    BLOCKED_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
}

// ============================================================
//  5.1.3 -- Dispatch
// ============================================================

/// Runs a call, whatever its provenance.
fn execute(number: u64, args: [u64; 6], origin: Origin) -> SysResult<u64> {
    CALL_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    if number > u16::MAX as u64 {
        return Err(SysError::with(
            ErrorCode::InvalidSyscall,
            "numero au-dela de 16 bits",
        ));
    }
    let number = number as u16;

    let Some(entry) = table::lookup(number) else {
        return Err(SysError::with(
            ErrorCode::InvalidSyscall,
            table::domain_name((number >> 8) as usize),
        ));
    };

    let mut call = Call {
        number,
        args,
        origin,
        mode: entry.mode,
        pid: scheduler::current_pid(),
    };

    (entry.handler)(&mut call)
}

/// Rust entry point called by syscall_entry.asm.
///
/// Runs in ring 0, on the kernel stack, with interrupts re-armed.
#[unsafe(no_mangle)]
pub extern "C" fn syscall_dispatch(frame: &mut SyscallFrame) -> u64 {
    // 10.4 -- A task loaded by a compatibility subsystem speaks another
    // ABI. Its RAX carries a Linux or an NT call number, which means
    // something quite different in the Nova table -- Linux `write` is 1,
    // and 1 is `SYS_DEBUG_PRINT` here. Dispatching it through the Nova
    // table would not fail: it would do the wrong thing quietly.
    //
    // The lookup is a table probe on the PID, taken before anything else,
    // and it costs nothing for the ordinary case: `personality_of`
    // returns `None` for every task the subsystems did not load.
    // 11.5 -- A thread of the Linux subsystem: found per core without a
    // lock, dispatched on the Linux numbering.
    //
    // A thread of a ported program (spec 6.7) speaks the Nova interface of
    // its own domains, never the Linux numbering: see `ported`.
    if let Some(thread) = crate::posix::current_thread() {
        let value = match thread.process.abi {
            crate::posix::Abi::Nova => ported::dispatch(&thread, frame),
            crate::posix::Abi::Linux => crate::linux::syscall::dispatch(&thread, frame),
        };
        drop(thread);
        scheduler::preempt_if_needed();
        return value;
    }

    let pid = scheduler::current_pid();
    if let Some(personality) = crate::compat::personality_of(pid) {
        let value = crate::compat::dispatch(personality, frame);
        scheduler::preempt_if_needed();
        return value;
    }

    let number = frame.rax;
    let result = execute(number, frame.args(), Origin::User);

    let value = match result {
        Ok(value) => value,
        Err(error) => {
            ERROR_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            let pid = scheduler::current_pid();
            record_error(pid, number as u16, error);

            // An unknown number is logged: it is almost always a program
            // compiled against another version of the API, and silence
            // would make the diagnosis impossible.
            if error.code == ErrorCode::InvalidSyscall {
                crate::serial_println!(
                    "[syscall] numero inconnu 0x{:04x} depuis RIP 0x{:x} (PID {})",
                    number,
                    frame.rip,
                    pid
                );
            }
            error.encoded()
        }
    };

    // Preemption point at the exit of the system call.
    //
    // The timer raises `NEED_RESCHEDULE` while a ring 3 task runs, but
    // nothing consumed that flag: a user program cannot call
    // `preempt_if_needed`. Consuming it here makes ring 3 code
    // preemptible **at the system call boundaries**, which is enough to
    // share the CPU between several active user tasks.
    //
    // The switch is safe here because each task owns its own kernel
    // stack: the system call frame, of which `frame` is part, stays
    // intact while other tasks run.
    scheduler::preempt_if_needed();

    value
}

/// Calls the system API **on behalf of a ring 3 caller**.
///
/// Same table and same handlers as `invoke`; what changes is that the
/// pointers in `args` are read and written as user pointers, with the
/// bounds check that goes with them.
///
/// This is what the compatibility subsystems of 10.4 need: a Linux
/// `write(1, buf, len)` carries a pointer into the address space of the
/// foreign program, and translating the call number without translating
/// the provenance of the pointer would have the kernel read a user
/// address as if it were its own.
pub fn invoke_user(number: u16, args: [u64; 6]) -> SysResult<u64> {
    let result = execute(number as u64, args, Origin::User);
    if let Err(error) = result {
        ERROR_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        record_error(scheduler::current_pid(), number, error);
    }
    result
}

/// Calls the system API from the kernel.
///
/// Same table, same rights, same modes: only the provenance of the
/// pointers changes. That is what lets the self-test exercise the eighty
/// calls without writing eighty ring 3 programs.
pub fn invoke(number: u16, args: [u64; 6]) -> SysResult<u64> {
    let result = execute(number as u64, args, Origin::Kernel);
    if let Err(error) = result {
        ERROR_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        record_error(scheduler::current_pid(), number, error);
    }
    result
}

// ============================================================
//  2.8.1 -- Configuration
// ============================================================

unsafe extern "C" {
    fn syscall_entry();
}

/// Arms the system call mechanism on the current core.
///
/// # Safety
/// The GDT must be loaded with the layout sysret expects, and the kernel
/// stack of the core filled in beforehand.
pub unsafe fn init(cpu_id: usize) {
    unsafe {
        // Allows the syscall instruction.
        let efer = read_msr(IA32_EFER);
        write_msr(IA32_EFER, efer | EFER_SCE);

        // STAR: selector bases. Bits 31-0 are unused in long mode (they
        // carried the 32-bit target address).
        //
        // The user base carries RPL 3 (0x1B, as Linux's __USER32_CS):
        // Intel forces RPL 3 on the selectors sysret loads, AMD copies
        // STAR[63:48] + 8 into SS as it is. With a bare 0x18, every
        // sysret on an AMD processor left SS = 0x20 (RPL 0) in ring 3,
        // and the next exception taken from ring 3 built an iretq frame
        // (CS 0x2B, SS 0x20) that the processor refuses with #GP(0x20).
        // TCG never checked; the first run under KVM on a Ryzen did.
        let star = ((USER_BASE_SELECTOR_U64 | 3) << 48) | ((KERNEL_BASE_SELECTOR_U64) << 32);
        write_msr(IA32_STAR, star);

        // LSTAR: address of the handler.
        write_msr(IA32_LSTAR, syscall_entry as *const () as u64);

        // FMASK: bits of RFLAGS cleared on entry. Masking IF guarantees
        // that no interrupt occurs during the swapgs and the stack
        // switch, a window where the kernel has no valid stack.
        write_msr(IA32_FMASK, RFLAGS_IF | RFLAGS_DF | RFLAGS_AC);

        // --- GS invariant ---
        //
        // `swapgs` swaps GS.base and IA32_KERNEL_GS_BASE, knowing nothing
        // of the privilege level. The correctness of the mechanism
        // therefore rests entirely on a convention, which must hold at
        // **every** instant:
        //
        //   in ring 0: GS.base = core structure, KERNEL_GS_BASE = 0
        //   in ring 3: GS.base = 0, KERNEL_GS_BASE = core structure
        //
        // and every privilege crossing must execute a `swapgs`.
        //
        // The initial version only laid down KERNEL_GS_BASE, relying on
        // the fact that a core starts in ring 0 with GS.base at zero.
        // That held as long as a single task went to ring 3: as soon as
        // two do so on the same core, a task preempted in the middle of a
        // system call leaves GS.base at the kernel value, the next one
        // crosses `iretq` without correcting it, and its first `syscall`
        // executed a `swapgs` that put GS.base back to zero -- a page
        // fault on `gs:0x8` at the very first instruction of the handler.
        //
        // Laying down both MSRs here makes the convention true from the
        // start; it is `user::enter_ring3` that maintains it the other way.
        let percpu = (&raw const PERCPU as *const PerCpu).add(cpu_id);
        write_msr(IA32_GS_BASE, percpu as u64);
        write_msr(IA32_KERNEL_GS_BASE, 0);
    }
}

const KERNEL_BASE_SELECTOR_U64: u64 = crate::gdt::KERNEL_CODE_SELECTOR as u64;
const USER_BASE_SELECTOR_U64: u64 = crate::gdt::USER_BASE_SELECTOR as u64;

const RFLAGS_IF: u64 = 1 << 9;
const RFLAGS_DF: u64 = 1 << 10;
const RFLAGS_AC: u64 = 1 << 18;

/// Checks the well-formedness of the tables at boot.
///
/// Separate from `init`, which runs on every core: the tables are static,
/// checking them once is enough.
pub fn check_tables() -> Result<(), &'static str> {
    table::validate()
}

/// Statistics: (calls handled, errors returned, blocks).
pub fn stats() -> (u64, u64, u64) {
    use core::sync::atomic::Ordering;
    (
        CALL_COUNT.load(Ordering::Relaxed),
        ERROR_COUNT.load(Ordering::Relaxed),
        BLOCKED_COUNT.load(Ordering::Relaxed),
    )
}

pub fn dump_state() {
    let (calls, errors, blocks) = stats();
    crate::serial_println!(
        "[syscall] gestionnaire a 0x{:016x}",
        syscall_entry as *const () as u64
    );
    crate::serial_println!(
        "[syscall] STAR 0x{:016x} - LSTAR arme - FMASK masque IF/DF/AC",
        ((USER_BASE_SELECTOR_U64) << 48) | ((KERNEL_BASE_SELECTOR_U64) << 32)
    );
    table::dump();
    crate::serial_println!(
        "[syscall] {} appels traites, {} erreurs, {} blocages asynchrones",
        calls,
        errors,
        blocks
    );
}

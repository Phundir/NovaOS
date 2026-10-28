//! Signals: actions, masks, delivery at the end of a system call.
//!
//! ## Where a signal is delivered
//!
//! Only on the way out of a system call, onto the `SyscallFrame` that is
//! about to be restored. A thread blocked in the kernel is woken by the
//! signal (its wait returns `EINTR`) and so reaches that point at once; a
//! thread computing in ring 3 reaches it at its next call. What that
//! leaves out is interrupting a pure computation -- a timer signal for a
//! profiler, say. The threads libraries (musl's cancellation and
//! `__synccall`) only signal threads that are, or will shortly be, in a
//! system call.
//!
//! Delivering at a system call boundary has a convenient consequence:
//! RCX and R11 hold nothing there (the `syscall` instruction clobbered
//! them), so the `sysret` that returns from `rt_sigreturn` loses nothing
//! by clobbering them again.
//!
//! ## The frame
//!
//! The kernel's `struct rt_sigframe` for x86-64, laid out on the thread's
//! stack (or its alternate stack), below the 128-byte red zone:
//!
//! ```text
//!   fpstate   512-byte FXSAVE image, 64-aligned
//!   ...
//!   +312  siginfo      128 bytes
//!   +8    ucontext     uc_flags, uc_link, uc_stack, mcontext (256), mask
//!   +0    pretcode     -> sa_restorer, which calls rt_sigreturn
//! ```
//!
//! The handler runs with RDI = signal, RSI = &siginfo, RDX = &ucontext,
//! and RSP 8 mod 16, as if called.

use alloc::sync::Arc;
use core::sync::atomic::Ordering;

use super::errno::*;
use super::{Process, Thread};
use crate::syscall::SyscallFrame;

pub const SIGHUP: u32 = 1;
pub const SIGINT: u32 = 2;
pub const SIGQUIT: u32 = 3;
pub const SIGILL: u32 = 4;
pub const SIGTRAP: u32 = 5;
pub const SIGABRT: u32 = 6;
pub const SIGBUS: u32 = 7;
pub const SIGFPE: u32 = 8;
pub const SIGKILL: u32 = 9;
pub const SIGUSR1: u32 = 10;
pub const SIGSEGV: u32 = 11;
pub const SIGUSR2: u32 = 12;
pub const SIGPIPE: u32 = 13;
pub const SIGALRM: u32 = 14;
pub const SIGTERM: u32 = 15;
pub const SIGCHLD: u32 = 17;
pub const SIGCONT: u32 = 18;
pub const SIGSTOP: u32 = 19;
pub const SIGTSTP: u32 = 20;
pub const SIGURG: u32 = 23;
pub const SIGWINCH: u32 = 28;
pub const NSIG: u32 = 64;

const SA_SIGINFO: u64 = 4;
const SA_ONSTACK: u64 = 0x0800_0000;
const SA_RESTORER: u64 = 0x0400_0000;
const SA_RESTART: u64 = 0x1000_0000;
const SA_NODEFER: u64 = 0x4000_0000;
const SA_RESETHAND: u64 = 0x8000_0000;

const SIG_DFL: u64 = 0;
const SIG_IGN: u64 = 1;

/// Can never be caught, blocked or ignored.
const UNBLOCKABLE: u64 = bit(SIGKILL) | bit(SIGSTOP);

const fn bit(sig: u32) -> u64 {
    1u64 << (sig - 1)
}

#[derive(Clone, Copy, Default)]
pub struct Action {
    pub handler: u64,
    pub flags: u64,
    pub restorer: u64,
    pub mask: u64,
}

#[derive(Clone)]
pub struct Actions {
    table: [Action; NSIG as usize],
}

impl Actions {
    pub fn new() -> Self {
        Actions { table: [Action::default(); NSIG as usize] }
    }

    /// What `execve` keeps: ignored signals stay ignored, handlers go
    /// back to the default (the code they pointed at is gone).
    pub fn after_exec(&self) -> Self {
        let mut next = Actions::new();
        for (i, a) in self.table.iter().enumerate() {
            if a.handler == SIG_IGN {
                next.table[i].handler = SIG_IGN;
            }
        }
        next
    }

    pub fn handler(&self, sig: u32) -> u64 {
        self.table.get(sig as usize - 1).map_or(SIG_DFL, |a| a.handler)
    }
}

impl Default for Actions {
    fn default() -> Self {
        Self::new()
    }
}

/// Does the default action of `sig` end the process?
fn default_kills(sig: u32) -> bool {
    !matches!(sig, SIGCHLD | SIGCONT | SIGURG | SIGWINCH | SIGSTOP | SIGTSTP | 21 | 22)
}

// ============================================================
//  Calls
// ============================================================

/// `rt_sigaction(sig, act, oact, sigsetsize)`. The user structure is
/// the kernel's: handler, flags, restorer, mask -- 32 bytes.
pub fn rt_sigaction(t: &Arc<Thread>, sig: u64, act: u64, oact: u64, size: u64) -> LxResult<u64> {
    if size != 8 {
        return Err(EINVAL);
    }
    if sig == 0 || sig > NSIG as u64 {
        return Err(EINVAL);
    }
    let sig = sig as u32;
    let new: Option<[u64; 4]> = if act != 0 { Some(crate::vm::read_user(act).map_err(|_| EFAULT)?) } else { None };
    if new.is_some() && (sig == SIGKILL || sig == SIGSTOP) {
        return Err(EINVAL);
    }
    let old = {
        let mut actions = t.process.signals.lock();
        let slot = &mut actions.table[sig as usize - 1];
        let old = *slot;
        if let Some([handler, flags, restorer, mask]) = new {
            *slot = Action { handler, flags, restorer, mask: mask & !UNBLOCKABLE };
        }
        old
    };
    if oact != 0 {
        crate::vm::write_user(oact, &[old.handler, old.flags, old.restorer, old.mask]).map_err(|_| EFAULT)?;
    }
    // Setting SIG_IGN discards what is pending (POSIX).
    if let Some([SIG_IGN, ..]) = new {
        t.pending.fetch_and(!bit(sig), Ordering::AcqRel);
        t.process.pending.fetch_and(!bit(sig), Ordering::AcqRel);
    }
    Ok(0)
}

/// `rt_sigprocmask(how, set, oset, sigsetsize)`.
pub fn rt_sigprocmask(t: &Arc<Thread>, how: u64, set: u64, oset: u64, size: u64) -> LxResult<u64> {
    if size != 8 {
        return Err(EINVAL);
    }
    let old = t.sigmask.load(Ordering::Acquire);
    if set != 0 {
        let value: u64 = crate::vm::read_user(set).map_err(|_| EFAULT)?;
        let new = match how {
            0 => old | value,
            1 => old & !value,
            2 => value,
            _ => return Err(EINVAL),
        };
        t.sigmask.store(new & !UNBLOCKABLE, Ordering::Release);
    }
    if oset != 0 {
        crate::vm::write_user(oset, &old).map_err(|_| EFAULT)?;
    }
    Ok(0)
}

/// `sigaltstack(ss, old_ss)`: stack_t is { sp, flags (int), size }.
pub fn sigaltstack(t: &Arc<Thread>, ss: u64, old: u64) -> LxResult<u64> {
    const SS_ONSTACK: u32 = 1;
    const SS_DISABLE: u32 = 2;
    let current = *t.altstack.lock();
    if old != 0 {
        let flags = if t.on_altstack.load(Ordering::Relaxed) { SS_ONSTACK } else { current.2 };
        let raw: [u64; 3] = [current.0, flags as u64, current.1];
        crate::vm::write_user(old, &raw).map_err(|_| EFAULT)?;
    }
    if ss != 0 {
        if t.on_altstack.load(Ordering::Relaxed) {
            return Err(EPERM);
        }
        let raw: [u64; 3] = crate::vm::read_user(ss).map_err(|_| EFAULT)?;
        let flags = raw[1] as u32;
        if flags & SS_DISABLE != 0 {
            *t.altstack.lock() = (0, 0, SS_DISABLE);
        } else {
            if raw[2] < 2048 {
                return Err(ENOMEM);
            }
            *t.altstack.lock() = (raw[0], raw[2], 0);
        }
    }
    Ok(0)
}

/// Sends `sig` to one thread (`tgkill`, `tkill`).
pub fn send_to_thread(tid: u64, sig: u32) -> LxResult<u64> {
    if sig > NSIG {
        return Err(EINVAL);
    }
    let target = super::thread_by_tid(tid).ok_or(ESRCH)?;
    if sig == 0 {
        return Ok(0);
    }
    target.pending.fetch_or(bit(sig), Ordering::AcqRel);
    crate::scheduler::wake(tid);
    Ok(0)
}

/// Sends `sig` to a whole process (`kill`).
pub fn send_to_process(process: &Arc<Process>, sig: u32) -> LxResult<u64> {
    if sig > NSIG {
        return Err(EINVAL);
    }
    if sig == 0 {
        return Ok(0);
    }
    // A signal whose action is to die kills at once: the process may be
    // computing and never reach the system call where signals are
    // delivered. SIGKILL cannot be caught at all.
    let handler = process.signals.lock().handler(sig);
    if sig == SIGKILL || (handler == SIG_DFL && default_kills(sig)) {
        // Killing one's own process: the caller dies with it, now.
        if let Some(me) = super::current_thread()
            && me.process.tgid == process.tgid
        {
            super::exit_group_signal(&me, sig);
        }
        super::terminate(process, sig);
        return Ok(0);
    }
    if handler == SIG_IGN || (handler == SIG_DFL && !default_kills(sig)) {
        return Ok(0);
    }
    process.pending.fetch_or(bit(sig), Ordering::AcqRel);
    for tid in process.thread_ids() {
        crate::scheduler::wake(tid);
    }
    Ok(0)
}

/// Is a signal waiting that should interrupt a blocking call of the
/// current thread?
pub fn interrupting() -> bool {
    let Some(t) = super::current_thread() else {
        return false;
    };
    let pending = t.pending.load(Ordering::Acquire) | t.process.pending.load(Ordering::Acquire);
    let deliverable = pending & !t.sigmask.load(Ordering::Acquire);
    if deliverable == 0 {
        return t.process.is_exiting();
    }
    // Ignored signals do not interrupt, whether ignored explicitly or by
    // default (SIGCHLD, SIGWINCH, SIGURG).
    let actions = t.process.signals.lock();
    (0..NSIG).any(|i| {
        let handler = actions.table[i as usize].handler;
        deliverable & (1 << i) != 0 && handler != SIG_IGN && !(handler == SIG_DFL && !default_kills(i + 1))
    }) || t.process.is_exiting()
}

// ============================================================
//  Delivery
// ============================================================

/// Takes the lowest deliverable signal of the thread, if any.
fn take_signal(t: &Arc<Thread>) -> Option<u32> {
    let mask = t.sigmask.load(Ordering::Acquire);
    loop {
        let own = t.pending.load(Ordering::Acquire) & !mask;
        if own != 0 {
            let sig = own.trailing_zeros() + 1;
            if t.pending.fetch_and(!bit(sig), Ordering::AcqRel) & bit(sig) != 0 {
                return Some(sig);
            }
            continue;
        }
        let shared = t.process.pending.load(Ordering::Acquire) & !mask;
        if shared == 0 {
            return None;
        }
        let sig = shared.trailing_zeros() + 1;
        if t.process.pending.fetch_and(!bit(sig), Ordering::AcqRel) & bit(sig) != 0 {
            return Some(sig);
        }
    }
}

#[repr(C, align(16))]
struct FxArea([u8; 512]);

const FRAME_SIZE: u64 = 440;
const UC: u64 = 8;
const MCONTEXT: u64 = UC + 40;
const SIGMASK: u64 = UC + 296;
const SIGINFO: u64 = 312;

/// Called on the way out of every system call of a Linux thread, with the
/// value about to be returned in RAX and the call's number.
pub fn deliver_pending(t: &Arc<Thread>, frame: &mut SyscallFrame, result: &mut u64, number: u64) {
    while let Some(sig) = take_signal(t) {
        let action = t.process.signals.lock().table[sig as usize - 1];
        if action.handler == SIG_IGN {
            continue;
        }
        if action.handler == SIG_DFL {
            if default_kills(sig) {
                crate::serial_println!("[posix] processus {} : signal {} fatal", t.process.tgid, sig);
                super::exit_group_signal(t, sig);
            }
            continue;
        }
        // Restart the call rather than fail it, if the handler asked.
        let mut rax = *result;
        let mut rip = frame.rip;
        if (rax as i64) == -EINTR && action.flags & SA_RESTART != 0 {
            rax = number;
            rip -= 2;
        }
        if build_frame(t, frame, sig, &action, rax, rip).is_err() {
            crate::serial_println!("[posix] processus {} : pile inutilisable pour le signal {}", t.process.tgid, sig);
            super::exit_group_signal(t, SIGSEGV);
        }
        *result = frame.rax;
        if action.flags & SA_RESETHAND != 0 {
            t.process.signals.lock().table[sig as usize - 1] = Action::default();
        }
        // One frame at a time: the next signal comes at the next exit.
        break;
    }
}

fn build_frame(t: &Arc<Thread>, frame: &mut SyscallFrame, sig: u32, action: &Action, rax: u64, rip: u64) -> Result<(), ()> {
    if action.flags & SA_RESTORER == 0 || action.restorer == 0 {
        return Err(());
    }
    let (alt_base, alt_size, alt_flags) = *t.altstack.lock();
    let use_alt = action.flags & SA_ONSTACK != 0 && alt_flags == 0 && alt_size != 0 && !t.on_altstack.load(Ordering::Relaxed);
    let top = if use_alt { alt_base + alt_size } else { frame.user_rsp - 128 };

    // FPU image first, 64-aligned; the frame below it, RSP = 8 mod 16.
    let fpstate = (top - 512) & !63;
    let base = ((fpstate - FRAME_SIZE) & !15) - 8;

    let mut fx = FxArea([0; 512]);
    unsafe { core::arch::asm!("fxsave [{}]", in(reg) fx.0.as_mut_ptr(), options(nostack)) };
    crate::vm::copy_to_user(fpstate, &fx.0).map_err(|_| ())?;

    let mut out = [0u8; FRAME_SIZE as usize];
    let mut put = |at: u64, v: u64| out[at as usize..at as usize + 8].copy_from_slice(&v.to_le_bytes());
    put(0, action.restorer);
    // ucontext: flags, link, stack
    put(UC, 0);
    put(UC + 8, 0);
    put(UC + 16, alt_base);
    put(UC + 24, if use_alt { 1 } else { 0 });
    put(UC + 32, alt_size);
    // sigcontext
    let regs = [
        frame.r8, frame.r9, frame.r10, frame.r11, frame.r12, frame.r13, frame.r14, frame.r15, frame.rdi, frame.rsi,
        frame.rbp, frame.rbx, frame.rdx, rax, frame.rcx, frame.user_rsp, rip, frame.rflags,
    ];
    for (i, v) in regs.iter().enumerate() {
        put(MCONTEXT + i as u64 * 8, *v);
    }
    put(MCONTEXT + 144, 0x33 | (0x2b << 48)); // cs, gs, fs, ss
    put(MCONTEXT + 192, fpstate);
    let old_mask = t.sigmask.load(Ordering::Acquire);
    put(SIGMASK, old_mask);
    // siginfo: signo, errno, code (SI_USER = 0 / SI_TKILL = -6)
    out[SIGINFO as usize..SIGINFO as usize + 4].copy_from_slice(&(sig as i32).to_le_bytes());
    out[SIGINFO as usize + 8..SIGINFO as usize + 12].copy_from_slice(&(-6i32).to_le_bytes());
    crate::vm::copy_to_user(base, &out).map_err(|_| ())?;

    let mut mask = old_mask | action.mask;
    if action.flags & SA_NODEFER == 0 {
        mask |= bit(sig);
    }
    t.sigmask.store(mask & !UNBLOCKABLE, Ordering::Release);
    if use_alt {
        t.on_altstack.store(true, Ordering::Relaxed);
    }

    frame.user_rsp = base;
    frame.rip = action.handler;
    frame.rdi = sig as u64;
    frame.rsi = base + SIGINFO;
    frame.rdx = base + UC;
    frame.rax = 0;
    // Direction flag clear on handler entry, as the ABI requires.
    frame.rflags &= !(1 << 10);
    let _ = SA_SIGINFO;
    Ok(())
}

/// `rt_sigreturn`: restores the context saved by `build_frame`. The frame
/// sits 8 bytes below the stack pointer (the handler's `ret` popped the
/// return address).
pub fn rt_sigreturn(t: &Arc<Thread>, frame: &mut SyscallFrame) -> u64 {
    let base = frame.user_rsp - 8;
    let mut raw = [0u8; FRAME_SIZE as usize];
    if crate::vm::copy_from_user(&mut raw, base).is_err() {
        super::exit_group_signal(t, SIGSEGV);
    }
    let get = |at: u64| u64::from_le_bytes(raw[at as usize..at as usize + 8].try_into().unwrap());
    let r = |i: u64| get(MCONTEXT + i * 8);
    frame.r8 = r(0);
    frame.r9 = r(1);
    frame.r10 = r(2);
    frame.r11 = r(3);
    frame.r12 = r(4);
    frame.r13 = r(5);
    frame.r14 = r(6);
    frame.r15 = r(7);
    frame.rdi = r(8);
    frame.rsi = r(9);
    frame.rbp = r(10);
    frame.rbx = r(11);
    frame.rdx = r(12);
    let rax = r(13);
    frame.rcx = r(14);
    frame.user_rsp = r(15);
    let rip = r(16);
    // Only the arithmetic flags, DF, TF and AC come back from user
    // memory; IF and IOPL stay the kernel's business.
    frame.rflags = (frame.rflags & !0x4_0DD5) | (r(17) & 0x4_0DD5) | 0x202;
    // A non-canonical RIP would make `sysret` fault in ring 0.
    if rip >= crate::vm::USER_TOP {
        super::exit_group_signal(t, SIGSEGV);
    }
    frame.rip = rip;
    let fpstate = get(MCONTEXT + 192);
    if fpstate != 0 {
        let mut fx = FxArea([0; 512]);
        if crate::vm::copy_from_user(&mut fx.0, fpstate).is_ok() {
            // Reserved MXCSR bits set would make FXRSTOR fault.
            let mxcsr = u32::from_le_bytes(fx.0[24..28].try_into().unwrap()) & 0xFFFF;
            fx.0[24..28].copy_from_slice(&mxcsr.to_le_bytes());
            unsafe { core::arch::asm!("fxrstor [{}]", in(reg) fx.0.as_ptr(), options(nostack)) };
        }
    }
    t.sigmask.store(get(SIGMASK) & !UNBLOCKABLE, Ordering::Release);
    t.on_altstack.store(false, Ordering::Relaxed);
    rax
}

/// A fault in ring 3 of a Linux thread: the process dies of the matching
/// signal. Handlers for synchronous faults are not called -- the fault
/// comes through an exception, not a system call, and this subsystem
/// delivers at system calls only.
pub fn fatal_exception(vector: u8) -> ! {
    let t = super::current_thread().expect("exception fatale hors thread Linux");
    super::dump_mm_history();
    // A page fault checked the page cache already (`vm::page_fault`).
    if vector != 14 {
        t.process.space.verify_cached_pages();
    }
    let sig = match vector {
        0 | 16 | 19 => SIGFPE,
        6 => SIGILL,
        3 | 1 => SIGTRAP,
        17 => SIGBUS,
        _ => SIGSEGV,
    };
    super::exit_group_signal(&t, sig)
}

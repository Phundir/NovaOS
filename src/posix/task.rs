//! Threads: `clone`, futexes, and the calls about the running task.
//!
//! ## A thread is a task sharing a space
//!
//! `clone(CLONE_VM | CLONE_THREAD | ...)` creates a Nova task with the same
//! `vm::Space`, the same process (descriptors, signal handlers, working
//! directory), and the parent's registers -- except RAX, 0 in the child,
//! and RSP, the stack the parent allocated for it. Its TLS base comes
//! from `CLONE_SETTLS`. That is all a threads library needs; musl's
//! `pthread_create` is a `clone` and a few words written in the new stack.
//!
//! What is refused: a `clone` without `CLONE_VM` (that is a `fork`, and
//! the Web Browser port runs in one process), and namespace flags.
//!
//! ## Futexes
//!
//! A futex is an address: `FUTEX_WAIT` sleeps if the word there still
//! holds the expected value, `FUTEX_WAKE` wakes sleepers of that address.
//! The waiter registers **before** reading the word; a waker writes the
//! word, then wakes. Whatever the interleaving, either the waiter reads
//! the new value and does not sleep, or the waker finds it registered.
//! The word is read with no lock held: reading user memory may fault and
//! sleep, which a spin lock must never do.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use super::errno::*;
use super::{Process, Thread};
use crate::syscall::SyscallFrame;
use crate::vga::SpinLock;
use crate::vm::{Space, UserRegs};

pub const CLONE_VM: u64 = 0x100;
pub const CLONE_FS: u64 = 0x200;
pub const CLONE_FILES: u64 = 0x400;
pub const CLONE_SIGHAND: u64 = 0x800;
pub const CLONE_PIDFD: u64 = 0x1000;
pub const CLONE_VFORK: u64 = 0x4000;
pub const CLONE_THREAD: u64 = 0x10000;
pub const CLONE_SYSVSEM: u64 = 0x40000;
pub const CLONE_SETTLS: u64 = 0x80000;
pub const CLONE_PARENT_SETTID: u64 = 0x100000;
pub const CLONE_CHILD_CLEARTID: u64 = 0x200000;
pub const CLONE_DETACHED: u64 = 0x400000;
pub const CLONE_CHILD_SETTID: u64 = 0x1000000;
const CLONE_NAMESPACES: u64 = 0x7E02_0000;

/// `clone(flags, newsp, parent_tid, child_tid, tls)`.
pub fn clone(t: &Arc<Thread>, frame: &SyscallFrame, flags: u64, newsp: u64, ptid: u64, ctid: u64, tls: u64) -> LxResult<u64> {
    if flags & CLONE_VM == 0 || flags & CLONE_THREAD == 0 {
        // A new process: fork, vfork, posix_spawn. Not provided.
        return Err(ENOSYS);
    }
    if flags & CLONE_NAMESPACES != 0 || flags & CLONE_SIGHAND == 0 {
        return Err(EINVAL);
    }
    let process = t.process.clone();
    if process.is_exiting() {
        return Err(EAGAIN);
    }
    let regs = UserRegs {
        rax: 0,
        rbx: frame.rbx,
        rcx: frame.rip,
        rdx: frame.rdx,
        rsi: frame.rsi,
        rdi: frame.rdi,
        rbp: frame.rbp,
        r8: frame.r8,
        r9: frame.r9,
        r10: frame.r10,
        r11: frame.rflags,
        r12: frame.r12,
        r13: frame.r13,
        r14: frame.r14,
        r15: frame.r15,
        rip: frame.rip,
        rsp: if newsp != 0 { newsp } else { frame.user_rsp },
        rflags: frame.rflags,
    };
    let fs_base = if flags & CLONE_SETTLS != 0 { tls } else { current_fs_base() };
    let parent_mask = t.sigmask.load(Ordering::Relaxed);
    let parent_gs = crate::scheduler::gs_base_of(t.tid);
    let space = process.space.clone();
    let mut failure = 0;
    let tid = crate::vm::spawn_user_with(&process.name.clone(), crate::process::BinaryType::Cus, space, regs, fs_base, |tid| {
        let child = Arc::new(Thread::new(tid, process.clone()));
        child.sigmask.store(parent_mask, Ordering::Relaxed);
        if flags & CLONE_CHILD_CLEARTID != 0 {
            child.clear_child_tid.store(ctid, Ordering::Relaxed);
        }
        // Written before the child runs, into the shared space: the
        // parent's pthread library reads the TID right after clone.
        if flags & CLONE_PARENT_SETTID != 0 && crate::vm::write_user(ptid, &(tid as u32)).is_err() {
            failure = EFAULT;
        }
        if flags & CLONE_CHILD_SETTID != 0 {
            let _ = crate::vm::write_user(ctid, &(tid as u32));
        }
        super::register_thread(&process, tid);
        crate::scheduler::set_gs_base(tid, parent_gs);
        crate::scheduler::set_linux(tid, Some(child));
    });
    if failure != 0 {
        return Err(failure);
    }
    Ok(tid)
}

fn current_fs_base() -> u64 {
    let (lo, hi): (u32, u32);
    unsafe {
        core::arch::asm!("rdmsr", in("ecx") 0xC000_0100u32, out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags));
    }
    ((hi as u64) << 32) | lo as u64
}

/// `arch_prctl(code, addr)`.
pub fn arch_prctl(code: u64, addr: u64) -> LxResult<u64> {
    const ARCH_SET_GS: u64 = 0x1001;
    const ARCH_SET_FS: u64 = 0x1002;
    const ARCH_GET_FS: u64 = 0x1003;
    const ARCH_GET_GS: u64 = 0x1004;
    match code {
        ARCH_SET_FS => {
            if addr >= crate::vm::USER_TOP {
                return Err(EPERM);
            }
            crate::scheduler::set_current_fs_base(addr);
            Ok(0)
        }
        ARCH_GET_FS => {
            crate::vm::write_user(addr, &current_fs_base()).map_err(|_| EFAULT)?;
            Ok(0)
        }
        ARCH_SET_GS => {
            if addr >= crate::vm::USER_TOP {
                return Err(EPERM);
            }
            crate::scheduler::set_current_gs_base(addr);
            Ok(0)
        }
        ARCH_GET_GS => {
            let base = crate::scheduler::gs_base_of(crate::scheduler::current_pid());
            crate::vm::write_user(addr, &base).map_err(|_| EFAULT)?;
            Ok(0)
        }
        _ => Err(EINVAL),
    }
}

// ============================================================
//  Futexes
// ============================================================

const FUTEX_WAIT: u64 = 0;
const FUTEX_WAKE: u64 = 1;
const FUTEX_REQUEUE: u64 = 3;
const FUTEX_CMP_REQUEUE: u64 = 4;
const FUTEX_WAKE_OP: u64 = 5;
const FUTEX_WAIT_BITSET: u64 = 9;
const FUTEX_WAKE_BITSET: u64 = 10;
const FUTEX_PRIVATE: u64 = 128;
const FUTEX_CLOCK_REALTIME: u64 = 256;

struct Waiter {
    pid: u64,
    bitset: u32,
    woken: Arc<AtomicBool>,
}

/// Waiters by (space, address).
static FUTEXES: SpinLock<BTreeMap<(u64, u64), Vec<Waiter>>> = SpinLock::new(BTreeMap::new());

fn key(space: &Arc<Space>, addr: u64) -> (u64, u64) {
    (Arc::as_ptr(space) as u64, addr)
}

/// Wakes up to `count` waiters of `addr` whose bitset meets `mask`.
pub fn futex_wake(space: &Arc<Space>, addr: u64, count: u32, mask: u32) -> u64 {
    let woken: Vec<u64> = {
        let mut table = FUTEXES.lock();
        let k = key(space, addr);
        let Some(list) = table.get_mut(&k) else {
            return 0;
        };
        let mut woken = Vec::new();
        list.retain(|w| {
            if woken.len() < count as usize && w.bitset & mask != 0 {
                w.woken.store(true, Ordering::Release);
                woken.push(w.pid);
                false
            } else {
                true
            }
        });
        if list.is_empty() {
            table.remove(&k);
        }
        woken
    };
    for pid in &woken {
        crate::scheduler::wake(*pid);
    }
    woken.len() as u64
}

fn futex_wait(t: &Arc<Thread>, addr: u64, expected: u32, deadline: Option<u64>, bitset: u32) -> LxResult<u64> {
    if addr % 4 != 0 {
        return Err(EINVAL);
    }
    if bitset == 0 {
        return Err(EINVAL);
    }
    let space = &t.process.space;
    let k = key(space, addr);
    let woken = Arc::new(AtomicBool::new(false));
    FUTEXES.lock().entry(k).or_default().push(Waiter { pid: t.tid, bitset, woken: woken.clone() });

    let remove = || {
        let mut table = FUTEXES.lock();
        // The waiter may have been requeued: look everywhere it can be.
        let mut empty = Vec::new();
        for (key, list) in table.iter_mut() {
            list.retain(|w| !Arc::ptr_eq(&w.woken, &woken));
            if list.is_empty() {
                empty.push(*key);
            }
        }
        for key in empty {
            table.remove(&key);
        }
    };

    let current = match crate::vm::read_user::<u32>(addr) {
        Ok(v) => v,
        Err(_) => {
            remove();
            return Err(EFAULT);
        }
    };
    if current != expected {
        remove();
        return Err(EAGAIN);
    }
    let result = super::wait::wait_until(&[], deadline, || woken.load(Ordering::Acquire).then_some(()));
    if !woken.load(Ordering::Acquire) {
        remove();
    }
    match result {
        Ok(()) => Ok(0),
        Err(super::wait::WaitError::TimedOut) => Err(ETIMEDOUT),
        Err(super::wait::WaitError::Interrupted) => Err(EINTR),
    }
}

/// Moves waiters of `from` to `to`, after waking `wake` of them.
fn futex_requeue(space: &Arc<Space>, from: u64, to: u64, wake: u32, requeue: u32) -> u64 {
    let woken = futex_wake(space, from, wake, u32::MAX);
    let mut table = FUTEXES.lock();
    let Some(mut list) = table.remove(&key(space, from)) else {
        return woken;
    };
    let moved: Vec<Waiter> = if list.len() > requeue as usize {
        let rest = list.split_off(requeue as usize);
        table.insert(key(space, from), rest);
        list
    } else {
        list
    };
    let count = moved.len() as u64;
    table.entry(key(space, to)).or_default().extend(moved);
    woken + count
}

fn read_timeout(addr: u64, absolute: bool, realtime: bool) -> LxResult<Option<u64>> {
    if addr == 0 {
        return Ok(None);
    }
    let ts: super::time::Timespec = crate::vm::read_user(addr).map_err(|_| EFAULT)?;
    let ns = ts.to_ns().ok_or(EINVAL)?;
    Ok(Some(if !absolute {
        super::time::deadline_after_ns(ns)
    } else if realtime {
        super::time::deadline_at_realtime(ns)
    } else {
        super::time::deadline_at_monotonic(ns)
    }))
}

pub fn futex(t: &Arc<Thread>, addr: u64, op: u64, val: u64, timeout: u64, addr2: u64, val3: u64) -> LxResult<u64> {
    let cmd = op & !(FUTEX_PRIVATE | FUTEX_CLOCK_REALTIME);
    let realtime = op & FUTEX_CLOCK_REALTIME != 0;
    let space = &t.process.space;
    match cmd {
        FUTEX_WAIT => {
            let deadline = read_timeout(timeout, false, false)?;
            futex_wait(t, addr, val as u32, deadline, u32::MAX)
        }
        FUTEX_WAIT_BITSET => {
            let deadline = read_timeout(timeout, true, realtime)?;
            futex_wait(t, addr, val as u32, deadline, val3 as u32)
        }
        FUTEX_WAKE => Ok(futex_wake(space, addr, val as u32, u32::MAX)),
        FUTEX_WAKE_BITSET => {
            if val3 as u32 == 0 {
                return Err(EINVAL);
            }
            Ok(futex_wake(space, addr, val as u32, val3 as u32))
        }
        FUTEX_REQUEUE => Ok(futex_requeue(space, addr, addr2, val as u32, timeout as u32)),
        FUTEX_CMP_REQUEUE => {
            let current: u32 = crate::vm::read_user(addr).map_err(|_| EFAULT)?;
            if current != val3 as u32 {
                return Err(EAGAIN);
            }
            Ok(futex_requeue(space, addr, addr2, val as u32, timeout as u32))
        }
        FUTEX_WAKE_OP => {
            // *addr2 = op(*addr2, oparg); wake addr; if cmp(old) wake addr2.
            let encoded = val3 as u32;
            let opcode = (encoded >> 28) & 7;
            let shift = encoded & (8 << 28) != 0;
            let cmp = (encoded >> 24) & 15;
            let mut oparg = (encoded >> 12) & 0xFFF;
            let cmparg = encoded & 0xFFF;
            if shift {
                oparg = 1u32.checked_shl(oparg).unwrap_or(0);
            }
            let old: u32 = crate::vm::read_user(addr2).map_err(|_| EFAULT)?;
            let new = match opcode {
                0 => oparg,
                1 => old.wrapping_add(oparg),
                2 => old | oparg,
                3 => old & !oparg,
                4 => old ^ oparg,
                _ => return Err(ENOSYS),
            };
            crate::vm::write_user(addr2, &new).map_err(|_| EFAULT)?;
            let mut woken = futex_wake(space, addr, val as u32, u32::MAX);
            let hit = match cmp {
                0 => old == cmparg,
                1 => old != cmparg,
                2 => (old as i32) < cmparg as i32,
                3 => (old as i32) <= cmparg as i32,
                4 => (old as i32) > cmparg as i32,
                5 => (old as i32) >= cmparg as i32,
                _ => return Err(ENOSYS),
            };
            if hit {
                woken += futex_wake(space, addr2, timeout as u32, u32::MAX);
            }
            Ok(woken)
        }
        _ => Err(ENOSYS),
    }
}

/// Number of futex addresses with waiters -- diagnostics.
pub fn futex_waiters() -> usize {
    FUTEXES.lock().values().map(|l| l.len()).sum()
}

// ============================================================
//  Small calls about the task
// ============================================================

pub fn sched_getaffinity(size: u64, mask: u64) -> LxResult<u64> {
    let cores = crate::smp_core_count().clamp(1, 64);
    if size < 8 {
        return Err(EINVAL);
    }
    let bits: u64 = if cores == 64 { u64::MAX } else { (1u64 << cores) - 1 };
    let mut out = alloc::vec![0u8; size.min(128) as usize];
    out[..8].copy_from_slice(&bits.to_le_bytes());
    crate::vm::copy_to_user(mask, &out).map_err(|_| EFAULT)?;
    Ok(out.len() as u64)
}

/// `prlimit64(pid, resource, new, old)`: limits are reported generous
/// and constant; setting one is accepted except for descriptors, which
/// resize the table.
pub fn prlimit(t: &Arc<Thread>, resource: u64, new: u64, old: u64) -> LxResult<u64> {
    const RLIMIT_STACK: u64 = 3;
    const RLIMIT_NOFILE: u64 = 7;
    const RLIMIT_AS: u64 = 9;
    const INFINITY: u64 = u64::MAX;
    let current: [u64; 2] = match resource {
        RLIMIT_NOFILE => {
            let limit = t.process.files.lock().limit as u64;
            [limit, 65536]
        }
        RLIMIT_STACK => [super::loader::STACK_SIZE, INFINITY],
        RLIMIT_AS => [INFINITY, INFINITY],
        0..=15 => [INFINITY, INFINITY],
        _ => return Err(EINVAL),
    };
    if old != 0 {
        crate::vm::write_user(old, &current).map_err(|_| EFAULT)?;
    }
    if new != 0 {
        let wanted: [u64; 2] = crate::vm::read_user(new).map_err(|_| EFAULT)?;
        if resource == RLIMIT_NOFILE {
            t.process.files.lock().limit = wanted[0].clamp(64, 65536) as usize;
        }
    }
    Ok(0)
}

pub fn process_of(t: &Arc<Thread>) -> &Arc<Process> {
    &t.process
}

pub fn prctl(t: &Arc<Thread>, option: u64, arg: u64) -> LxResult<u64> {
    const PR_SET_PDEATHSIG: u64 = 1;
    const PR_GET_DUMPABLE: u64 = 3;
    const PR_SET_DUMPABLE: u64 = 4;
    const PR_SET_NAME: u64 = 15;
    const PR_GET_NAME: u64 = 16;
    const PR_SET_SECCOMP: u64 = 22;
    const PR_SET_NO_NEW_PRIVS: u64 = 38;
    const PR_GET_NO_NEW_PRIVS: u64 = 39;
    const PR_SET_PTRACER: u64 = 0x5961_6d61;
    const PR_SET_VMA: u64 = 0x5356_4d41;
    match option {
        // The thread's name, as Linux keeps it (`comm`, per thread, 15
        // bytes): the scheduler's name of the task. Firefox names its
        // threads -- "Renderer", "Socket Thread", "MediaDecoderSt" -- and
        // without them every line of a profile or of the wake-up
        // statistics said "web-browser". Bytes outside printable ASCII
        // become '?': the name goes to the serial console.
        PR_SET_NAME => {
            let mut raw = [0u8; 16];
            crate::vm::copy_from_user(&mut raw, arg).map_err(|_| EFAULT)?;
            let name: alloc::string::String = raw[..15]
                .iter()
                .take_while(|&&b| b != 0)
                .map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { '?' })
                .collect();
            crate::scheduler::set_name(crate::scheduler::current_pid(), &name);
            Ok(0)
        }
        PR_GET_NAME => {
            let mut name = [0u8; 16];
            let current = crate::scheduler::current_info().map(|info| info.name);
            let bytes = current.as_deref().unwrap_or(t.process.name.as_str()).as_bytes();
            let n = bytes.len().min(15);
            name[..n].copy_from_slice(&bytes[..n]);
            crate::vm::copy_to_user(arg, &name).map_err(|_| EFAULT)?;
            Ok(0)
        }
        PR_GET_DUMPABLE => Ok(1),
        PR_SET_DUMPABLE | PR_SET_PDEATHSIG | PR_SET_NO_NEW_PRIVS | PR_SET_PTRACER | PR_SET_VMA => Ok(0),
        PR_GET_NO_NEW_PRIVS => Ok(0),
        PR_SET_SECCOMP => Err(EINVAL),
        _ => Err(EINVAL),
    }
}

/// `membarrier`: a barrier on every core running this process. A TLB
/// shootdown is exactly that -- an IPI every core has acknowledged -- so
/// the expedited barrier reuses it: aimed at the cores running this
/// process for the private one, at all of them for the global one.
pub fn membarrier(t: &Arc<Thread>, cmd: u64) -> LxResult<u64> {
    const QUERY: u64 = 0;
    const GLOBAL: u64 = 1;
    const PRIVATE_EXPEDITED: u64 = 8;
    const REGISTER_PRIVATE_EXPEDITED: u64 = 16;
    match cmd {
        QUERY => Ok(GLOBAL | PRIVATE_EXPEDITED | REGISTER_PRIVATE_EXPEDITED),
        REGISTER_PRIVATE_EXPEDITED => Ok(0),
        GLOBAL => {
            crate::memory::shootdown_and_wait();
            Ok(0)
        }
        PRIVATE_EXPEDITED => {
            crate::memory::shootdown_space_and_wait(t.process.space.cr3());
            Ok(0)
        }
        _ => Err(EINVAL),
    }
}

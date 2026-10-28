//! Processes: `fork`, `vfork`, `execve`, and waiting for children.
//!
//! Firefox runs its web pages in content processes, and launches them the
//! way Chromium's IPC library does: `fork`, a few `dup2` in the child,
//! then `execve` of its own binary with `-contentproc`. A single-process
//! mode exists but is legacy: Firefox 140 still starts content processes
//! for some browsers even with it (observed under `strace`). So Nova runs
//! Firefox the way it is built, and this module is what that takes.
//!
//! ## fork
//!
//! A new process with a copy of the caller's address space
//! (`vm::Space::fork`: private pages shared copy-on-write, shared ones
//! shared), a copy of its descriptor table (same open descriptions), its
//! handlers, its working directory. Only the calling thread is copied, as
//! POSIX specifies. The child's first thread resumes from the same point
//! with RAX = 0.
//!
//! ## vfork
//!
//! `clone(CLONE_VM | CLONE_VFORK)`, as musl's `posix_spawn` issues it: the
//! child **borrows** the parent's space, on a stack of its own, and the
//! parent sleeps until the child calls `execve` or exits.
//!
//! ## execve
//!
//! The new image is loaded into a fresh space first; only once that has
//! worked is anything of the old process discarded, so a failed `execve`
//! returns an error to an intact caller. The process keeps its PID, its
//! parent, and the descriptors not marked close-on-exec. It is a new
//! `Process` object nonetheless -- name, executable, space and handlers
//! all change -- which replaces the old one in the registry and in the
//! calling task.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use super::errno::*;
use super::{Brk, PROCESSES, Process, Thread, loader, signal, wait};
use crate::syscall::SyscallFrame;
use crate::vga::SpinLock;
use crate::vm::UserRegs;

/// Children that have ended and not yet been waited for: pid -> (parent,
/// wait status).
static ZOMBIES: SpinLock<BTreeMap<u64, (u64, u32)>> = SpinLock::new(BTreeMap::new());
/// Woken whenever a child ends.
static CHILD_EXITED: wait::WaitQueue = wait::WaitQueue::new();

/// Called by `finish_process`: a child of a Linux process becomes a
/// zombie for its parent's `wait`, and the parent gets SIGCHLD.
pub fn child_ended(process: &Arc<Process>) {
    if process.parent == 0 {
        return;
    }
    let parent = super::process(process.parent);
    PROCESSES.lock().remove(&process.tgid);
    if let Some(parent) = parent {
        ZOMBIES.lock().insert(process.tgid, (process.parent, process.wait_status()));
        let _ = signal::send_to_process(&parent, signal::SIGCHLD);
    }
    CHILD_EXITED.notify_all();
}

fn regs_from(frame: &SyscallFrame, rsp: u64) -> UserRegs {
    UserRegs {
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
        rsp,
        rflags: frame.rflags,
    }
}

fn current_fs_base() -> u64 {
    let (lo, hi): (u32, u32);
    unsafe {
        core::arch::asm!("rdmsr", in("ecx") 0xC000_0100u32, out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags));
    }
    ((hi as u64) << 32) | lo as u64
}

/// `fork`, `vfork`, and `clone` without `CLONE_THREAD`.
pub fn fork(t: &Arc<Thread>, frame: &SyscallFrame, flags: u64, newsp: u64, ptid: u64, ctid: u64, tls: u64) -> LxResult<u64> {
    use super::task::*;
    let share_vm = flags & CLONE_VM != 0;
    let vfork = flags & CLONE_VFORK != 0;
    if share_vm && !vfork {
        // A process sharing memory without waiting: nothing here uses it.
        return Err(EINVAL);
    }
    let parent = t.process.clone();
    let space = if share_vm { parent.space.clone() } else { parent.space.fork().ok_or(ENOMEM)? };
    let (mut files, cwd, umask, actions, brk) = parent.snapshot();
    if flags & CLONE_FILES != 0 {
        // Shared tables are a thread thing; a new process gets a copy.
        files = files.clone_table();
    }
    let fs_base = if flags & CLONE_SETTLS != 0 { tls } else { current_fs_base() };
    let rsp = if newsp != 0 { newsp } else { frame.user_rsp };
    let regs = regs_from(frame, rsp);
    let mask = t.sigmask.load(Ordering::Relaxed);
    let parent_gs = crate::scheduler::gs_base_of(t.tid);
    let parent_tgid = parent.tgid;
    let mut child_process: Option<Arc<Process>> = None;

    let tid = crate::vm::spawn_user_with(&parent.name.clone(), crate::process::BinaryType::Cus, space.clone(), regs, fs_base, |tid| {
        let process = Process::create(
            tid,
            parent.name.clone(),
            parent.exe.clone(),
            parent.cmdline.clone(),
            space,
            files,
            cwd,
            actions,
            Brk { start: brk.start, end: brk.end },
            parent_tgid,
            parent.abi,
        );
        process.umask.store(umask, Ordering::Relaxed);
        let thread = Arc::new(Thread::new(tid, process.clone()));
        thread.sigmask.store(mask, Ordering::Relaxed);
        if flags & CLONE_CHILD_CLEARTID != 0 {
            thread.clear_child_tid.store(ctid, Ordering::Relaxed);
        }
        if flags & CLONE_PARENT_SETTID != 0 {
            let _ = crate::vm::write_user(ptid, &(tid as u32));
        }
        PROCESSES.lock().insert(tid, process.clone());
        crate::scheduler::set_gs_base(tid, parent_gs);
        crate::scheduler::set_linux(tid, Some(thread));
        child_process = Some(process);
    });
    FORKS.fetch_add(1, Ordering::Relaxed);

    if vfork && let Some(child) = child_process {
        // The parent waits for the child to let go of the shared space.
        let _ = wait::wait_until(&[&child.ended], None, || child.vfork_released.load(Ordering::Acquire).then_some(()));
    }
    Ok(tid)
}

static FORKS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static EXECS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

pub fn counters() -> (u64, u64) {
    (FORKS.load(Ordering::Relaxed), EXECS.load(Ordering::Relaxed))
}

// ============================================================
//  execve
// ============================================================

/// Reads a NULL-terminated array of string pointers.
fn read_strings(addr: u64) -> LxResult<Vec<String>> {
    let mut out = Vec::new();
    let mut total = 0usize;
    if addr == 0 {
        return Ok(out);
    }
    for i in 0..65536u64 {
        let ptr: u64 = crate::vm::read_user(addr + i * 8).map_err(|_| EFAULT)?;
        if ptr == 0 {
            return Ok(out);
        }
        let bytes = crate::vm::read_user_cstr(ptr, 128 * 1024).map_err(|_| EFAULT)?;
        total += bytes.len() + 1;
        if total > 2 << 20 {
            return Err(E2BIG);
        }
        out.push(String::from_utf8_lossy(&bytes).to_string());
    }
    Err(E2BIG)
}

/// `execve(path, argv, envp)`. On success the frame is the new image's
/// start and the return value is 0; the caller must not touch `t` again
/// as a thread of the old image.
pub fn execve(t: &Arc<Thread>, frame: &mut SyscallFrame, path_addr: u64, argv_addr: u64, envp_addr: u64) -> LxResult<u64> {
    let path = super::files::read_path(path_addr)?;
    let mut argv = read_strings(argv_addr)?;
    let envp = read_strings(envp_addr)?;
    let base = if path.starts_with('/') { String::from("/") } else { t.process.cwd() };
    let mut target = match super::vfs::walk(&base, &path, true) {
        Ok(found) => found.path,
        Err(error) => {
            crate::serial_println!("[posix] execve \"{}\" : chemin refuse, errno {}", path, -error);
            return Err(error);
        }
    };

    // "#!interpreter [arg]": the interpreter runs, with the script's path
    // in place of argv[0]. One level, as Linux allows four but nothing
    // here nests scripts.
    let mut head = [0u8; 256];
    let found = super::vfs::walk("/", &target, true)?;
    let n = found.inode.read_at(0, &mut head).unwrap_or(0);
    if n > 2 && &head[..2] == b"#!" {
        let line_end = head[..n].iter().position(|b| *b == b'\n').unwrap_or(n);
        let line = String::from_utf8_lossy(&head[2..line_end]).trim().to_string();
        let mut parts = line.splitn(2, char::is_whitespace);
        let interpreter = parts.next().ok_or(ENOEXEC)?.to_string();
        let arg = parts.next().map(|a| a.trim().to_string()).filter(|a| !a.is_empty());
        let mut new_argv = alloc::vec![interpreter.clone()];
        if let Some(a) = arg {
            new_argv.push(a);
        }
        new_argv.push(target.clone());
        new_argv.extend(argv.iter().skip(1).cloned());
        argv = new_argv;
        target = interpreter;
    }

    // Load first: a failure leaves the caller untouched.
    let space = crate::vm::Space::new().ok_or(ENOMEM)?;
    let argv_refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
    let envp_refs: Vec<&str> = envp.iter().map(|s| s.as_str()).collect();
    let image = match loader::load(&space, &target, &argv_refs, &envp_refs, t.process.abi) {
        Ok(image) => image,
        Err(error) => {
            crate::serial_println!("[posix] execve \"{}\" refuse : errno {}", target, -error);
            return Err(error);
        }
    };

    // Point of no return.
    let old = t.process.clone();
    super::kill_other_threads(&old, t.tid, 0);
    let mut files = old.take_files();
    let closed = files.close_on_exec();
    drop(closed);
    let mut cmdline = Vec::new();
    for a in &argv {
        cmdline.extend_from_slice(a.as_bytes());
        cmdline.push(0);
    }
    let name = image.exe.rsplit('/').next().unwrap_or("?").to_string();
    // The scheduler task still carries the name of the program that was
    // spawned; after an exec it would credit this program's time to it.
    crate::scheduler::set_name(t.tid, &name);
    let actions = old.signals.lock().after_exec();
    let process = Process::create(
        old.tgid,
        name,
        image.exe.clone(),
        cmdline,
        space.clone(),
        files,
        old.cwd(),
        actions,
        Brk { start: image.brk, end: image.brk },
        old.parent,
        old.abi,
    );
    process.umask.store(old.umask.load(Ordering::Relaxed), Ordering::Relaxed);
    process.pending.store(old.pending.load(Ordering::Relaxed), Ordering::Relaxed);
    process.calls.store(old.calls.load(Ordering::Relaxed), Ordering::Relaxed);
    // The calling thread may not be the first one: it is the only one
    // left either way.
    *process.threads.lock() = alloc::vec![t.tid];
    let thread = Arc::new(Thread::new(t.tid, process.clone()));
    thread.sigmask.store(t.sigmask.load(Ordering::Relaxed), Ordering::Relaxed);
    PROCESSES.lock().insert(old.tgid, process);

    // The calling task becomes the new image's thread: space, thread and
    // TLS base replaced in the scheduler and on this core at once.
    crate::scheduler::set_user_context(t.tid, Some(space.clone()), 0);
    crate::scheduler::set_linux(t.tid, Some(thread.clone()));
    crate::vm::activate(Some(&space));
    super::activate(Some(&thread));
    crate::scheduler::set_current_fs_base(0);
    crate::scheduler::set_current_gs_base(0);

    // A vfork parent was waiting for exactly this.
    old.vfork_released.store(true, Ordering::Release);
    old.ended.notify_all();

    let regs = image.regs;
    *frame = SyscallFrame {
        rax: 0,
        rbx: 0,
        rcx: 0,
        rdx: 0,
        rsi: 0,
        rdi: 0,
        rbp: 0,
        r8: 0,
        r9: 0,
        r10: 0,
        r11: 0,
        r12: 0,
        r13: 0,
        r14: 0,
        r15: 0,
        rip: regs.rip,
        rflags: 0x202,
        user_rsp: regs.rsp,
    };
    // Floating point state of the old image means nothing to the new one.
    unsafe {
        core::arch::asm!("fninit", options(nomem, nostack));
        let mxcsr: u32 = 0x1F80;
        core::arch::asm!("ldmxcsr [{}]", in(reg) &mxcsr, options(nostack));
    }
    EXECS.fetch_add(1, Ordering::Relaxed);
    Ok(0)
}

// ============================================================
//  Waiting
// ============================================================

const WNOHANG: u64 = 1;

/// Finds (and removes, unless `peek`) a zombie child of `me` matching
/// `pid` (-1 any, > 0 that one).
fn take_zombie(me: u64, pid: i64, peek: bool) -> Option<(u64, u32)> {
    let mut zombies = ZOMBIES.lock();
    let found = zombies
        .iter()
        .find(|(child, (parent, _))| *parent == me && (pid <= 0 || **child == pid as u64))
        .map(|(child, (_, status))| (*child, *status))?;
    if !peek {
        zombies.remove(&found.0);
    }
    Some(found)
}

fn has_children(me: u64, pid: i64) -> bool {
    let live = PROCESSES.lock().values().any(|p| p.parent == me && (pid <= 0 || p.tgid == pid as u64));
    live || ZOMBIES.lock().iter().any(|(c, (p, _))| *p == me && (pid <= 0 || *c == pid as u64))
}

pub fn wait4(t: &Arc<Thread>, pid: i64, status_addr: u64, options: u64) -> LxResult<u64> {
    let me = t.process.tgid;
    if !has_children(me, pid) {
        return Err(ECHILD);
    }
    let result = if options & WNOHANG != 0 {
        match take_zombie(me, pid, false) {
            Some(z) => z,
            None => return Ok(0),
        }
    } else {
        wait::wait_until(&[&CHILD_EXITED], None, || take_zombie(me, pid, false)).map_err(|_| EINTR)?
    };
    if status_addr != 0 {
        crate::vm::write_user(status_addr, &result.1).map_err(|_| EFAULT)?;
    }
    Ok(result.0)
}

/// `waitid(idtype, id, infop, options)`.
pub fn waitid(t: &Arc<Thread>, idtype: u64, id: u64, info: u64, options: u64) -> LxResult<u64> {
    const P_ALL: u64 = 0;
    const P_PID: u64 = 1;
    const WNOWAIT: u64 = 0x0100_0000;
    let pid = match idtype {
        P_ALL => -1,
        P_PID => id as i64,
        _ => return Err(EINVAL),
    };
    let me = t.process.tgid;
    if !has_children(me, pid) {
        return Err(ECHILD);
    }
    let peek = options & WNOWAIT != 0;
    let found = if options & WNOHANG != 0 {
        take_zombie(me, pid, peek)
    } else {
        Some(wait::wait_until(&[&CHILD_EXITED], None, || take_zombie(me, pid, peek)).map_err(|_| EINTR)?)
    };
    if info != 0 {
        // siginfo_t: signo, errno, code, pid, uid, status.
        let mut out = [0u8; 128];
        if let Some((child, status)) = found {
            let (code, value) = if status & 0x7F == 0 { (1i32, (status >> 8) as i32) } else { (2i32, (status & 0x7F) as i32) };
            out[0..4].copy_from_slice(&(signal::SIGCHLD as i32).to_le_bytes());
            out[8..12].copy_from_slice(&code.to_le_bytes());
            out[16..20].copy_from_slice(&(child as i32).to_le_bytes());
            out[24..28].copy_from_slice(&value.to_le_bytes());
        }
        crate::vm::copy_to_user(info, &out).map_err(|_| EFAULT)?;
    }
    Ok(0)
}

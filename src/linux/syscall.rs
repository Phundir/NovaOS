//! System call dispatch of the Linux subsystem.
//!
//! One `match` on the x86-64 Linux call number, translated into the
//! kernel's POSIX objects (`crate::posix`): this table is all the Linux
//! personality is. Arguments come in RDI, RSI, RDX, R10, R8, R9 -- the
//! same registers as Nova's own convention -- and the result goes back in
//! RAX, `-errno` on failure. A ported program (spec 6.7) never gets here:
//! its calls are Nova numbers, served by `syscall::ported` on the same
//! objects.
//!
//! Calls that name something this system does not have (`fork`, `execve`
//! of a second program, `ptrace`, namespaces) return `ENOSYS`, and the
//! first occurrence per number is logged with the program's name: an
//! unimplemented call a program depends on then shows up in the serial
//! log instead of as a silent misbehaviour.
//!
//! `posix::TRACE` logs every call with its result, in the spirit of
//! `strace`. It is off by default.

use alloc::collections::BTreeSet;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::posix::errno::*;
use crate::posix::files::{self, AT_FDCWD};
use crate::posix::task::{membarrier, prctl};
use crate::posix::time::{clock_gettime, clock_nanosleep, gettimeofday, nanosleep};
use crate::posix::{Thread, TRACE, mm, net, poll, signal, task};
use crate::syscall::SyscallFrame;
use crate::vga::SpinLock;

/// Calls per system call number, since boot: what the latency traces
/// compare before and after an input (`gui::cpu_breakdown`). One row per
/// core: a single counter incremented by eight cores at every
/// `clock_gettime` would bounce its cache line between them.
pub const COUNTED: usize = 512;
static PER_NUMBER: [[AtomicU64; COUNTED]; crate::gdt::MAX_CPUS] =
    [const { [const { AtomicU64::new(0) }; COUNTED] }; crate::gdt::MAX_CPUS];

/// A copy of the per-number counters, summed over the cores.
pub fn counts() -> [u64; COUNTED] {
    core::array::from_fn(|n| PER_NUMBER.iter().map(|row| row[n].load(Ordering::Relaxed)).sum())
}
static UNKNOWN_SEEN: SpinLock<BTreeSet<u64>> = SpinLock::new(BTreeSet::new());

pub fn call_count() -> u64 {
    PER_NUMBER.iter().flatten().map(|c| c.load(Ordering::Relaxed)).sum()
}

/// Entry point, from `syscall::syscall_dispatch`.
pub fn dispatch(t: &Arc<Thread>, frame: &mut SyscallFrame) -> u64 {
    let number = frame.rax;
    let cpu = crate::smp::current_cpu_id().min(crate::gdt::MAX_CPUS - 1);
    if let Some(counter) = PER_NUMBER[cpu].get(number as usize) {
        counter.fetch_add(1, Ordering::Relaxed);
    }
    let a = [frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9];

    // rt_sigreturn rewrites the whole frame, RAX included.
    if number == 15 {
        return signal::rt_sigreturn(t, frame);
    }
    // execve replaces the thread itself: on success nothing of `t` may be
    // used again -- no trace, no signal delivery on the old image.
    if number == 59 {
        return match crate::posix::process::execve(t, frame, a[0], a[1], a[2]) {
            Ok(v) => v,
            Err(e) => (-e) as u64,
        };
    }

    let result = call(t, frame, number, a);
    let mut value = match result {
        Ok(v) => v,
        Err(e) => (-e) as u64,
    };
    t.note_call(number, &a, value);
    if matches!(number, 9 | 10 | 11 | 12 | 25 | 28) {
        let mut history = t.process.mm_history.lock();
        if history.len() == 48 {
            history.pop_front();
        }
        history.push_back((number, [a[0], a[1], a[2], a[3]], value));
    }
    if TRACE.load(Ordering::Relaxed) {
        crate::serial_println!(
            "[strace {}] {}({:#x}, {:#x}, {:#x}, {:#x}) = {}",
            t.tid,
            name(number),
            a[0],
            a[1],
            a[2],
            a[3],
            value as i64
        );
    }
    signal::deliver_pending(t, frame, &mut value, number);
    value
}

fn unknown(t: &Arc<Thread>, number: u64) -> LxResult<u64> {
    if UNKNOWN_SEEN.lock().insert(number) {
        crate::serial_println!("[linux] {} : appel {} ({}) non fourni - ENOSYS", t.process.name, number, name(number));
    }
    Err(ENOSYS)
}

fn i(v: u64) -> i64 {
    v as i64
}

fn call(t: &Arc<Thread>, frame: &mut SyscallFrame, n: u64, a: [u64; 6]) -> LxResult<u64> {
    match n {
        // --- files ---
        0 => files::read(t, i(a[0]), a[1], a[2]),
        1 => files::write(t, i(a[0]), a[1], a[2]),
        2 => files::openat(t, AT_FDCWD, a[0], a[1], a[2]),
        3 => files::close(t, i(a[0])),
        4 => files::fstatat(t, AT_FDCWD, a[0], a[1], 0),
        5 => files::fstat(t, i(a[0]), a[1]),
        6 => files::fstatat(t, AT_FDCWD, a[0], a[1], 0x100),
        7 => poll::poll(t, a[0], a[1], a[2] as i32 as i64),
        8 => files::lseek(t, i(a[0]), i(a[1]), a[2]),
        9 => mm::mmap(t, a[0], a[1], a[2], a[3], a[4] as i32 as i64, a[5]),
        10 => mm::mprotect(t, a[0], a[1], a[2]),
        11 => mm::munmap(t, a[0], a[1]),
        12 => mm::brk(t, a[0]),
        13 => signal::rt_sigaction(t, a[0], a[1], a[2], a[3]),
        14 => signal::rt_sigprocmask(t, a[0], a[1], a[2], a[3]),
        16 => files::ioctl(t, i(a[0]), a[1] & 0xFFFF_FFFF, a[2]),
        17 => files::pread(t, i(a[0]), a[1], a[2], i(a[3])),
        18 => files::pwrite(t, i(a[0]), a[1], a[2], i(a[3])),
        19 => files::readv(t, i(a[0]), a[1], a[2], -1),
        20 => files::writev(t, i(a[0]), a[1], a[2], -1),
        21 => files::faccessat(t, AT_FDCWD, a[0], a[1], 0),
        22 => files::pipe2(t, a[0], 0),
        23 => poll::select(t, a[0], a[1], a[2], a[3], a[4]),
        24 => {
            crate::scheduler::yield_now();
            Ok(0)
        }
        25 => mm::mremap(t, a[0], a[1], a[2], a[3], a[4]),
        26 => mm::msync(t, a[0], a[1]),
        27 => Err(ENOSYS),
        28 => mm::madvise(t, a[0], a[1], a[2]),
        32 => files::dup(t, i(a[0])),
        33 => files::dup2(t, i(a[0]), i(a[1])),
        34 | 130 => {
            // pause, rt_sigsuspend: until a signal.
            let saved = t.sigmask.load(Ordering::Acquire);
            if n == 130 {
                let mask: u64 = crate::vm::read_user(a[0]).map_err(|_| EFAULT)?;
                t.sigmask.store(mask, Ordering::Release);
            }
            let _ = crate::posix::wait::wait_until::<()>(&[], None, || None);
            if n == 130 {
                // The handler runs with the suspend mask; restored after.
                let _ = saved;
            }
            Err(EINTR)
        }
        35 => nanosleep(a[0], a[1]),
        39 => Ok(t.process.tgid),
        40 => files::sendfile(t, i(a[0]), i(a[1]), a[2], a[3]),
        41 => net::socket(t, a[0], a[1], a[2]),
        42 => net::connect(t, i(a[0]), a[1], a[2]),
        43 => net::accept(t, i(a[0]), a[1], a[2], 0),
        44 => net::sendto(t, i(a[0]), a[1], a[2], a[3], a[4], a[5]),
        45 => net::recvfrom(t, i(a[0]), a[1], a[2], a[3], a[4], a[5]),
        46 => net::sendmsg(t, i(a[0]), a[1], a[2]),
        47 => net::recvmsg(t, i(a[0]), a[1], a[2]),
        48 => net::shutdown(t, i(a[0]), a[1]),
        49 => net::bind(t, i(a[0]), a[1], a[2]),
        50 => net::listen(t, i(a[0]), a[1]),
        51 => net::getsockname(t, i(a[0]), a[1], a[2]),
        52 => net::getpeername(t, i(a[0]), a[1], a[2]),
        53 => net::socketpair(t, a[0], a[1], a[2], a[3]),
        54 => net::setsockopt(t, i(a[0]), a[1], a[2], a[3], a[4]),
        55 => net::getsockopt(t, i(a[0]), a[1], a[2], a[3], a[4]),
        56 => {
            if a[0] & task::CLONE_THREAD != 0 {
                task::clone(t, frame, a[0], a[1], a[2], a[3], a[4])
            } else {
                crate::posix::process::fork(t, frame, a[0], a[1], a[2], a[3], a[4])
            }
        }
        57 => crate::posix::process::fork(t, frame, signal::SIGCHLD as u64, 0, 0, 0, 0),
        58 => crate::posix::process::fork(t, frame, task::CLONE_VM | task::CLONE_VFORK | signal::SIGCHLD as u64, 0, 0, 0, 0),
        322 | 435 => unknown(t, n),
        60 => crate::posix::exit_thread(t, a[0] as i32 as i64 & 0xFF),
        61 => crate::posix::process::wait4(t, a[0] as i32 as i64, a[1], a[2]),
        247 => crate::posix::process::waitid(t, a[0], a[1], a[2], a[3]),
        62 => {
            let pid = a[0] as i32 as i64;
            if pid == t.process.tgid as i64 || pid == 0 {
                signal::send_to_process(&t.process, a[1] as u32)
            } else if let Some(p) = crate::posix::process(pid.unsigned_abs()) {
                signal::send_to_process(&p, a[1] as u32)
            } else {
                Err(ESRCH)
            }
        }
        63 => crate::posix::misc::uname(t, a[0]),
        72 => files::fcntl(t, i(a[0]), a[1], a[2]),
        73 => Ok(0), // flock
        74 | 75 | 306 => files::fsync(t, i(a[0])),
        // sync: what the volume holds in its cache reaches the disk.
        162 => {
            let _ = crate::fs::flush_all();
            Ok(0)
        }
        76 => files::truncate(t, a[0], i(a[1])),
        77 => files::ftruncate(t, i(a[0]), i(a[1])),
        79 => files::getcwd(t, a[0], a[1]),
        80 => files::chdir(t, a[0]),
        81 => files::fchdir(t, i(a[0])),
        82 => files::renameat(t, AT_FDCWD, a[0], AT_FDCWD, a[1], 0),
        83 => files::mkdirat(t, AT_FDCWD, a[0], a[1]),
        84 => files::unlinkat(t, AT_FDCWD, a[0], 0x200),
        85 => files::openat(t, AT_FDCWD, a[0], 0o1101, a[1]),
        86 => files::linkat(t, AT_FDCWD, a[0], AT_FDCWD, a[1], 0),
        87 => files::unlinkat(t, AT_FDCWD, a[0], 0),
        88 => files::symlinkat(t, a[0], AT_FDCWD, a[1]),
        89 => files::readlinkat(t, AT_FDCWD, a[0], a[1], a[2]),
        90 => files::fchmodat(t, AT_FDCWD, a[0], a[1]),
        91 => files::fchmod(t, i(a[0]), a[1]),
        92..=94 | 260 => Ok(0), // chown family: one user
        95 => Ok(t.process.umask.swap(a[0] as u32 & 0o777, Ordering::AcqRel) as u64),
        96 => gettimeofday(a[0]),
        97 => task::prlimit(t, a[0], 0, a[1]),
        98 => crate::posix::misc::getrusage(t, a[0], a[1]),
        99 => crate::posix::misc::sysinfo(a[0]),
        100 => Ok(crate::posix::time::monotonic_ns() / 10_000_000),
        102 | 104 | 107 | 108 => Ok(0), // uid, gid, euid, egid: root
        105 | 106 | 109 | 113..=114 | 116 | 117 | 119 | 122 | 123 | 126 => Ok(0),
        110 => Ok(1),
        111 | 121 | 124 => Ok(t.process.tgid),
        112 => Ok(t.process.tgid),
        115 => Ok(0), // getgroups: none
        118 | 120 => {
            for addr in [a[0], a[1], a[2]] {
                crate::vm::write_user(addr, &0u32).map_err(|_| EFAULT)?;
            }
            Ok(0)
        }
        125 => Err(EINVAL),
        127 => {
            let pending = t.pending.load(Ordering::Acquire) | t.process.pending.load(Ordering::Acquire);
            crate::vm::write_user(a[0], &pending).map_err(|_| EFAULT)?;
            Ok(0)
        }
        128 => Err(EAGAIN), // rt_sigtimedwait: nothing queued
        131 => signal::sigaltstack(t, a[0], a[1]),
        137 => files::statfs(t, a[0], a[1]),
        138 => files::fstatfs(t, i(a[0]), a[1]),
        140 => Ok(20),
        141 | 142 | 144 => Ok(0),
        143 => {
            crate::vm::write_user(a[1], &0u32).map_err(|_| EFAULT)?;
            Ok(0)
        }
        145 => Ok(0),
        146 | 147 => Ok(0),
        149..=152 => Ok(0), // mlock family
        157 => prctl(t, a[0], a[1]),
        158 => task::arch_prctl(a[0], a[1]),
        160 => task::prlimit(t, a[0], a[1], 0),
        186 => Ok(t.tid),
        187 | 221 => Ok(0), // readahead, fadvise64
        200 => signal::send_to_thread(a[0], a[1] as u32),
        201 => {
            let now = crate::posix::time::realtime_ns() / 1_000_000_000;
            if a[0] != 0 {
                crate::vm::write_user(a[0], &now).map_err(|_| EFAULT)?;
            }
            Ok(now)
        }
        202 => task::futex(t, a[0], a[1], a[2], a[3], a[4], a[5]),
        203 => Ok(0),
        204 => task::sched_getaffinity(a[1], a[2]),
        213 => files::epoll_create1(t, 0),
        217 => files::getdents64(t, i(a[0]), a[1], a[2]),
        218 => {
            t.clear_child_tid.store(a[0], Ordering::Release);
            Ok(t.tid)
        }
        228 => clock_gettime(a[0], a[1]),
        229 => {
            if a[1] != 0 {
                let res = crate::posix::time::Timespec { sec: 0, nsec: 1 };
                crate::vm::write_user(a[1], &res).map_err(|_| EFAULT)?;
            }
            Ok(0)
        }
        230 => clock_nanosleep(a[0], a[1], a[2], a[3]),
        231 => crate::posix::exit_group(t, a[0] as i32 as i64 & 0xFF),
        232 => poll::epoll_wait(t, i(a[0]), a[1], a[2] as i32 as i64, a[3] as i32 as i64),
        233 => poll::epoll_ctl(t, i(a[0]), a[1], a[2] as i32 as i64, a[3]),
        234 => {
            let target = crate::posix::thread_by_tid(a[1]).ok_or(ESRCH)?;
            if target.process.tgid != a[0] {
                return Err(ESRCH);
            }
            signal::send_to_thread(a[1], a[2] as u32)
        }
        257 => files::openat(t, a[0] as i32 as i64, a[1], a[2], a[3]),
        258 => files::mkdirat(t, a[0] as i32 as i64, a[1], a[2]),
        262 => files::fstatat(t, a[0] as i32 as i64, a[1], a[2], a[3]),
        263 => files::unlinkat(t, a[0] as i32 as i64, a[1], a[2]),
        264 => files::renameat(t, a[0] as i32 as i64, a[1], a[2] as i32 as i64, a[3], 0),
        265 => files::linkat(t, a[0] as i32 as i64, a[1], a[2] as i32 as i64, a[3], a[4]),
        266 => files::symlinkat(t, a[0], a[1] as i32 as i64, a[2]),
        267 => files::readlinkat(t, a[0] as i32 as i64, a[1], a[2], a[3]),
        268 => files::fchmodat(t, a[0] as i32 as i64, a[1], a[2]),
        269 | 439 => files::faccessat(t, a[0] as i32 as i64, a[1], a[2], if n == 439 { a[3] } else { 0 }),
        270 => poll::pselect6(t, a[0], a[1], a[2], a[3], a[4], a[5]),
        271 => poll::ppoll(t, a[0], a[1], a[2], a[3]),
        273 => {
            t.robust_list.store(a[0], Ordering::Relaxed);
            Ok(0)
        }
        274 => Err(ENOSYS),
        280 => files::utimensat(t, a[0] as i32 as i64, a[1], a[2], a[3]),
        281 => poll::epoll_pwait(t, i(a[0]), a[1], a[2] as i32 as i64, a[3] as i32 as i64, a[4]),
        284 => files::eventfd2(t, a[0], 0),
        285 => files::fallocate(t, i(a[0]), a[1], i(a[2]), i(a[3])),
        288 => net::accept(t, i(a[0]), a[1], a[2], a[3]),
        290 => files::eventfd2(t, a[0], a[1]),
        291 => files::epoll_create1(t, a[0]),
        292 => files::dup3(t, i(a[0]), i(a[1]), a[2]),
        293 => files::pipe2(t, a[0], a[1]),
        295 => files::readv(t, i(a[0]), a[1], a[2], i(a[3])),
        296 => files::writev(t, i(a[0]), a[1], a[2], i(a[3])),
        302 => {
            if a[0] != 0 && a[0] != t.process.tgid {
                return Err(ESRCH);
            }
            task::prlimit(t, a[1], a[2], a[3])
        }
        309 => {
            let cpu = crate::smp::current_cpu_id() as u32;
            if a[0] != 0 {
                crate::vm::write_user(a[0], &cpu).map_err(|_| EFAULT)?;
            }
            if a[1] != 0 {
                crate::vm::write_user(a[1], &0u32).map_err(|_| EFAULT)?;
            }
            Ok(0)
        }
        316 => files::renameat(t, a[0] as i32 as i64, a[1], a[2] as i32 as i64, a[3], a[4]),
        317 => Err(EINVAL), // seccomp: no filter support
        318 => crate::posix::misc::getrandom(a[0], a[1], a[2]),
        319 => files::memfd_create(t, a[0], a[1]),
        324 => membarrier(t, a[0]),
        327 => files::readv(t, i(a[0]), a[1], a[2], i(a[3])),
        328 => files::writev(t, i(a[0]), a[1], a[2], i(a[3])),
        332 => files::statx(t, a[0] as i32 as i64, a[1], a[2], a[3], a[4]),
        334 => Err(ENOSYS), // rseq: optional everywhere
        436 => files::close_range(t, a[0], a[1], a[2]),
        441 => poll::epoll_pwait(t, i(a[0]), a[1], a[2] as i32 as i64, -1, a[4]),
        _ => unknown(t, n),
    }
}

/// Name of a call, for the trace and the ENOSYS log.
pub fn name(n: u64) -> &'static str {
    match n {
        0 => "read",
        1 => "write",
        2 => "open",
        3 => "close",
        4 => "stat",
        5 => "fstat",
        6 => "lstat",
        7 => "poll",
        8 => "lseek",
        9 => "mmap",
        10 => "mprotect",
        11 => "munmap",
        12 => "brk",
        13 => "rt_sigaction",
        14 => "rt_sigprocmask",
        16 => "ioctl",
        17 => "pread64",
        19 => "readv",
        20 => "writev",
        21 => "access",
        24 => "sched_yield",
        25 => "mremap",
        28 => "madvise",
        35 => "nanosleep",
        39 => "getpid",
        40 => "sendfile",
        41 => "socket",
        42 => "connect",
        46 => "sendmsg",
        47 => "recvmsg",
        53 => "socketpair",
        56 => "clone",
        57 => "fork",
        58 => "vfork",
        59 => "execve",
        60 => "exit",
        72 => "fcntl",
        89 => "readlink",
        157 => "prctl",
        158 => "arch_prctl",
        186 => "gettid",
        202 => "futex",
        217 => "getdents64",
        218 => "set_tid_address",
        228 => "clock_gettime",
        231 => "exit_group",
        232 => "epoll_wait",
        233 => "epoll_ctl",
        257 => "openat",
        262 => "newfstatat",
        271 => "ppoll",
        273 => "set_robust_list",
        281 => "epoll_pwait",
        290 => "eventfd2",
        291 => "epoll_create1",
        293 => "pipe2",
        302 => "prlimit64",
        318 => "getrandom",
        319 => "memfd_create",
        322 => "execveat",
        324 => "membarrier",
        332 => "statx",
        334 => "rseq",
        435 => "clone3",
        _ => "?",
    }
}

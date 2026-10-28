//! Spec 6.7 -- the Nova interface of ported programs.
//!
//! ## What speaks it
//!
//! A program linked against the Nova C library (musl whose system calls
//! are Nova calls, `tools/ports/nova-libc/`), started from a linked image
//! (spec 3.5) or `exec`'d by such a program: its process has
//! `linux::Abi::Nova`, and every `syscall` it makes lands here. The call
//! number is a Nova number -- domains 09 (`SYS_OBJ_*`), 0A
//! (`SYS_THREAD_*`), 0B (`SYS_SIG_*`) and the extensions of 00, 02, 03,
//! 07 and 08. No Linux number is ever decoded for such a process: the
//! Linux personality (`linux::syscall`) is a separate door, for programs
//! built for Linux.
//!
//! ## What serves it
//!
//! The objects the calls act on -- descriptor tables, files and folders,
//! pipes, event counters, shareable memory, sockets, `epoll` sets,
//! threads and futexes, address spaces, signals -- are the kernel's POSIX
//! objects (`crate::posix`). Nothing here goes through the Linux
//! personality (`crate::linux`), which is another numbering in front of
//! the same objects, for programs built for Linux. One implementation of
//! each object, two tables in front of it: that is what makes this
//! module a table and not a second kernel.
//!
//! ## Conventions (spec 6.7)
//!
//! * Arguments in RDI, RSI, RDX, R10, R8, R9, number in RAX, as for every
//!   Nova call.
//! * The structures exchanged are musl's (`struct stat`, `struct
//!   sockaddr`, `struct msghdr`, `struct epoll_event`...): the program
//!   keeps musl's user ABI, which is what lets a program written for
//!   POSIX be compiled unchanged.
//! * A call that groups several operations takes the operation first
//!   (`op`), then the operation's own arguments.
//! * An error is returned as `-errno`, musl's numbering.
//!
//! The numbers and the operation codes are repeated in the C library's
//! `nova/abi.h`; the two must move together.

use alloc::sync::Arc;
use core::sync::atomic::Ordering;

use super::SyscallFrame;
use crate::posix::errno::*;
use crate::posix::{files, mm, net, poll, signal, task, Thread};

/// Call numbers (spec 6.7).
pub mod nr {
    pub const RANDOM: u64 = 0x0004;
    pub const PROC_YIELD: u64 = 0x0208;
    pub const PROC_FORK: u64 = 0x0210;
    pub const PROC_EXEC: u64 = 0x0211;
    pub const PROC_WAIT_ANY: u64 = 0x0212;
    pub const PROC_CWD: u64 = 0x0213;
    pub const PROC_LIMITS: u64 = 0x0214;
    pub const PROC_EXIT_ALL: u64 = 0x0215;
    pub const PROC_IDENTITY: u64 = 0x0216;
    pub const PROC_SCHED: u64 = 0x0217;
    pub const PROC_CONTROL: u64 = 0x0218;
    pub const MEM_MAP: u64 = 0x0310;
    pub const MEM_UNMAP: u64 = 0x0311;
    pub const MEM_REMAP: u64 = 0x0312;
    pub const MEM_ADVISE: u64 = 0x0313;
    pub const MEM_BARRIER: u64 = 0x0314;
    pub const MEM_PROTECT_MAP: u64 = 0x0315;
    pub const MEM_BREAK: u64 = 0x0316;
    pub const TIME_CLOCK: u64 = 0x0708;
    pub const TIME_PAGE: u64 = 0x0709;
    pub const NET_SOCKET: u64 = 0x0810;
    pub const NET_PAIR: u64 = 0x0811;
    pub const NET_BIND: u64 = 0x0812;
    pub const NET_LISTEN: u64 = 0x0813;
    pub const NET_ACCEPT: u64 = 0x0814;
    pub const NET_CONNECT_TO: u64 = 0x0815;
    pub const NET_SEND_MSG: u64 = 0x0816;
    pub const NET_RECV_MSG: u64 = 0x0817;
    pub const NET_OPTION: u64 = 0x0818;
    pub const NET_SHUTDOWN: u64 = 0x0819;
    pub const NET_ADDRESS: u64 = 0x081A;
    pub const OBJ_OPEN: u64 = 0x0901;
    pub const OBJ_CLOSE: u64 = 0x0902;
    pub const OBJ_READ: u64 = 0x0903;
    pub const OBJ_WRITE: u64 = 0x0904;
    pub const OBJ_SEEK: u64 = 0x0905;
    pub const OBJ_STAT: u64 = 0x0906;
    pub const OBJ_DUP: u64 = 0x0907;
    pub const OBJ_CONTROL: u64 = 0x0908;
    pub const OBJ_DIR_READ: u64 = 0x0909;
    pub const OBJ_NAME_OP: u64 = 0x090A;
    pub const OBJ_RESIZE: u64 = 0x090B;
    pub const OBJ_SYNC: u64 = 0x090C;
    pub const OBJ_PIPE: u64 = 0x090D;
    pub const OBJ_EVENT: u64 = 0x090E;
    pub const OBJ_MEMORY: u64 = 0x090F;
    pub const OBJ_WAIT: u64 = 0x0910;
    pub const OBJ_WATCH_CREATE: u64 = 0x0911;
    pub const OBJ_WATCH_CTL: u64 = 0x0912;
    pub const OBJ_WATCH_WAIT: u64 = 0x0913;
    pub const OBJ_COPY: u64 = 0x0914;
    pub const THREAD_CREATE: u64 = 0x0A01;
    pub const THREAD_EXIT: u64 = 0x0A02;
    pub const THREAD_SELF: u64 = 0x0A03;
    pub const THREAD_SET_TLS: u64 = 0x0A04;
    pub const THREAD_WAIT_ADDR: u64 = 0x0A05;
    pub const THREAD_WAKE_ADDR: u64 = 0x0A06;
    pub const THREAD_SET_NAME: u64 = 0x0A07;
    pub const THREAD_SLEEP_NS: u64 = 0x0A08;
    pub const THREAD_CLEAR_ADDR: u64 = 0x0A09;
    pub const THREAD_ROBUST: u64 = 0x0A0A;
    pub const SIG_ACTION: u64 = 0x0B01;
    pub const SIG_MASK: u64 = 0x0B02;
    pub const SIG_SEND: u64 = 0x0B03;
    pub const SIG_RETURN: u64 = 0x0B04;
    pub const SIG_ALTSTACK: u64 = 0x0B05;
    pub const SIG_WAIT: u64 = 0x0B06;
}

/// Operation codes of the grouped calls (first argument).
pub mod op {
    // OBJ_STAT
    pub const STAT_AT: u64 = 0;
    pub const STAT_X: u64 = 1;
    pub const STAT_FS: u64 = 2;
    pub const STAT_FS_FD: u64 = 3;
    pub const STAT_FD: u64 = 4;
    // OBJ_SYNC
    pub const SYNC_OBJECT: u64 = 0;
    pub const SYNC_ALL: u64 = 2;
    // OBJ_DUP (fourth argument)
    pub const DUP: u64 = 0;
    pub const DUP_TO: u64 = 1;
    pub const DUP_TO_FLAGS: u64 = 2;
    // OBJ_CONTROL
    pub const CTL_FCNTL: u64 = 0;
    pub const CTL_IOCTL: u64 = 1;
    pub const CTL_FLOCK: u64 = 2;
    pub const CTL_ADVISE: u64 = 3;
    pub const CTL_ALLOCATE: u64 = 4;
    pub const CTL_READAHEAD: u64 = 5;
    // OBJ_NAME_OP
    pub const NAME_MKDIR: u64 = 0;
    pub const NAME_UNLINK: u64 = 1;
    pub const NAME_RENAME: u64 = 2;
    pub const NAME_LINK: u64 = 3;
    pub const NAME_SYMLINK: u64 = 4;
    pub const NAME_READLINK: u64 = 5;
    pub const NAME_ACCESS: u64 = 6;
    pub const NAME_CHMOD: u64 = 7;
    pub const NAME_FCHMOD: u64 = 8;
    pub const NAME_CHOWN: u64 = 9;
    pub const NAME_FCHOWN: u64 = 10;
    pub const NAME_UTIMENS: u64 = 11;
    pub const NAME_TRUNCATE: u64 = 12;
    // OBJ_WAIT
    pub const WAIT_POLL: u64 = 0;
    pub const WAIT_SELECT: u64 = 1;
    // OBJ_COPY
    pub const COPY_SENDFILE: u64 = 0;
    // OBJ_READ, OBJ_WRITE: fifth argument, a mode
    pub const IO_VECTOR: u64 = 1;
    // THREAD_SLEEP_NS
    pub const SLEEP_RELATIVE: u64 = 0;
    pub const SLEEP_CLOCK: u64 = 1;
    // THREAD_ROBUST
    pub const ROBUST_SET: u64 = 0;
    pub const ROBUST_GET: u64 = 1;
    // MEM_ADVISE
    pub const MEM_ADVICE: u64 = 0;
    pub const MEM_SYNC: u64 = 1;
    pub const MEM_RESIDENT: u64 = 2;
    // PROC_WAIT_ANY
    pub const WAIT_PID: u64 = 0;
    pub const WAIT_ID: u64 = 1;
    // PROC_CWD
    pub const CWD_GET: u64 = 0;
    pub const CWD_SET: u64 = 1;
    pub const CWD_SET_FD: u64 = 2;
    // PROC_LIMITS
    pub const LIMIT_RESOURCE: u64 = 0;
    pub const LIMIT_USAGE: u64 = 1;
    pub const LIMIT_SYSINFO: u64 = 2;
    pub const LIMIT_TIMES: u64 = 3;
    // PROC_IDENTITY
    pub const ID_PID: u64 = 0;
    pub const ID_PPID: u64 = 1;
    pub const ID_UID: u64 = 2;
    pub const ID_EUID: u64 = 3;
    pub const ID_GID: u64 = 4;
    pub const ID_EGID: u64 = 5;
    pub const ID_RESUID: u64 = 6;
    pub const ID_RESGID: u64 = 7;
    pub const ID_GROUPS: u64 = 8;
    pub const ID_SETSID: u64 = 9;
    pub const ID_SETPGID: u64 = 10;
    pub const ID_PGID: u64 = 11;
    pub const ID_SID: u64 = 12;
    pub const ID_UMASK: u64 = 13;
    pub const ID_UNAME: u64 = 14;
    pub const ID_CAPGET: u64 = 15;
    pub const ID_SET: u64 = 16;
    // PROC_SCHED
    pub const SCHED_GET_AFFINITY: u64 = 0;
    pub const SCHED_SET_AFFINITY: u64 = 1;
    pub const SCHED_CPU: u64 = 2;
    pub const SCHED_GET_PARAM: u64 = 3;
    pub const SCHED_GET_POLICY: u64 = 4;
    pub const SCHED_SET_POLICY: u64 = 5;
    pub const SCHED_GET_PRIORITY: u64 = 6;
    pub const SCHED_SET_PRIORITY: u64 = 7;
    pub const SCHED_PRIORITY_RANGE: u64 = 8;
    // SIG_SEND
    pub const SEND_PROCESS: u64 = 0;
    pub const SEND_THREAD: u64 = 1;
    pub const SEND_GROUP_THREAD: u64 = 2;
    // SIG_WAIT
    pub const SIG_TIMED: u64 = 0;
    pub const SIG_SUSPEND: u64 = 1;
    pub const SIG_PAUSE: u64 = 2;
    pub const SIG_PENDING: u64 = 3;
    // TIME_CLOCK
    pub const CLOCK_GET: u64 = 0;
    pub const CLOCK_RESOLUTION: u64 = 1;
    pub const CLOCK_TIMEOFDAY: u64 = 2;
    pub const CLOCK_SECONDS: u64 = 3;
    // NET_OPTION
    pub const OPTION_SET: u64 = 0;
    pub const OPTION_GET: u64 = 1;
    // NET_ADDRESS
    pub const ADDRESS_LOCAL: u64 = 0;
    pub const ADDRESS_PEER: u64 = 1;
}

/// Calls served since boot, per call and per core, all ported processes
/// together: what the latency traces compare before and after an input
/// (`linux::gui`). One row per core, as for the Linux calls: a single
/// counter bumped by eight cores at every call would bounce its cache
/// line between them.
pub const COUNTED: usize = 12 * 32;
static PER_NUMBER: [[core::sync::atomic::AtomicU64; COUNTED]; crate::gdt::MAX_CPUS] =
    [const { [const { core::sync::atomic::AtomicU64::new(0) }; COUNTED] }; crate::gdt::MAX_CPUS];

/// Counter of call `n`: 32 per domain, domains 00 to 0B. Slot 0 (never a
/// call: `SYS_RANDOM` is 0x0004) takes what falls outside.
fn slot(n: u64) -> usize {
    let (domain, low) = ((n >> 8) as usize, (n & 0xFF) as usize);
    if n < 0x0C00 && low < 32 { domain * 32 + low } else { 0 }
}

/// The call a counter slot stands for.
pub fn number_of(slot: usize) -> u64 {
    (((slot / 32) << 8) | (slot % 32)) as u64
}

/// A copy of the per-call counters, summed over the cores.
pub fn counts() -> [u64; COUNTED] {
    core::array::from_fn(|n| PER_NUMBER.iter().map(|row| row[n].load(Ordering::Relaxed)).sum())
}

pub fn call_count() -> u64 {
    PER_NUMBER.iter().flatten().map(|c| c.load(Ordering::Relaxed)).sum()
}

/// Entry point, from `syscall_dispatch`, for a thread of a process whose
/// interface is `Abi::Nova`.
pub fn dispatch(t: &Arc<Thread>, frame: &mut SyscallFrame) -> u64 {
    let n = frame.rax;
    let cpu = crate::smp::current_cpu_id().min(crate::gdt::MAX_CPUS - 1);
    PER_NUMBER[cpu][slot(n)].fetch_add(1, Ordering::Relaxed);
    t.process.calls.fetch_add(1, Ordering::Relaxed);
    let a = [frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9];

    // These two replace the calling frame whole.
    if n == nr::SIG_RETURN {
        return signal::rt_sigreturn(t, frame);
    }
    if n == nr::PROC_EXEC {
        return match crate::posix::process::execve(t, frame, a[0], a[1], a[2]) {
            Ok(v) => v,
            Err(e) => (-e) as u64,
        };
    }

    let mut value = match call(t, frame, n, a) {
        Ok(v) => v,
        Err(e) => (-e) as u64,
    };
    t.note_call(n, &a, value);
    if matches!(n, nr::MEM_MAP | nr::MEM_UNMAP | nr::MEM_REMAP | nr::MEM_ADVISE | nr::MEM_PROTECT_MAP | nr::MEM_BREAK) {
        let mut history = t.process.mm_history.lock();
        if history.len() == 48 {
            history.pop_front();
        }
        history.push_back((n, [a[0], a[1], a[2], a[3]], value));
    }
    if crate::posix::TRACE.load(Ordering::Relaxed) {
        crate::serial_println!(
            "[natif {}] {}({:#x}, {:#x}, {:#x}, {:#x}) = {}",
            t.tid,
            name(n),
            a[0],
            a[1],
            a[2],
            a[3],
            value as i64
        );
    }
    signal::deliver_pending(t, frame, &mut value, n);
    value
}

fn i(v: u64) -> i64 {
    v as i64
}

/// A 32-bit signed argument (a descriptor, `AT_FDCWD`, a timeout).
fn i32a(v: u64) -> i64 {
    v as i32 as i64
}

fn write<T: Copy>(addr: u64, value: T) -> LxResult<u64> {
    if addr != 0 {
        crate::vm::write_user(addr, &value).map_err(|_| EFAULT)?;
    }
    Ok(0)
}

fn call(t: &Arc<Thread>, frame: &mut SyscallFrame, n: u64, a: [u64; 6]) -> LxResult<u64> {
    match n {
        // ---- domain 00 ----
        nr::RANDOM => crate::posix::misc::getrandom(a[0], a[1], a[2]),

        // ---- domain 0C: the GPU (spec 6.8), shared with the Nova table ----
        0x0C01..=0x0C10 => crate::gpu::api::ported(n, a),

        // ---- domain 09: open objects ----
        nr::OBJ_OPEN => files::openat(t, i32a(a[0]), a[1], a[2], a[3]),
        nr::OBJ_CLOSE => {
            if a[0] == a[1] && a[2] == 0 {
                files::close(t, i32a(a[0]))
            } else {
                files::close_range(t, a[0], a[1], a[2])
            }
        }
        nr::OBJ_READ => match (a[4] & op::IO_VECTOR != 0, i(a[3]) >= 0) {
            (false, false) => files::read(t, i32a(a[0]), a[1], a[2]),
            (false, true) => files::pread(t, i32a(a[0]), a[1], a[2], i(a[3])),
            (true, _) => files::readv(t, i32a(a[0]), a[1], a[2], i(a[3])),
        },
        nr::OBJ_WRITE => match (a[4] & op::IO_VECTOR != 0, i(a[3]) >= 0) {
            (false, false) => files::write(t, i32a(a[0]), a[1], a[2]),
            (false, true) => files::pwrite(t, i32a(a[0]), a[1], a[2], i(a[3])),
            (true, _) => files::writev(t, i32a(a[0]), a[1], a[2], i(a[3])),
        },
        nr::OBJ_SEEK => files::lseek(t, i32a(a[0]), i(a[1]), a[2]),
        nr::OBJ_STAT => match a[0] {
            op::STAT_AT => files::fstatat(t, i32a(a[1]), a[2], a[3], a[4]),
            op::STAT_X => files::statx(t, i32a(a[1]), a[2], a[3], a[4], a[5]),
            op::STAT_FS => files::statfs(t, a[1], a[2]),
            op::STAT_FS_FD => files::fstatfs(t, i32a(a[1]), a[2]),
            op::STAT_FD => files::fstat(t, i32a(a[1]), a[2]),
            _ => Err(EINVAL),
        },
        nr::OBJ_DUP => match a[3] {
            op::DUP => files::dup(t, i32a(a[0])),
            op::DUP_TO => files::dup2(t, i32a(a[0]), i32a(a[1])),
            op::DUP_TO_FLAGS => files::dup3(t, i32a(a[0]), i32a(a[1]), a[2]),
            _ => Err(EINVAL),
        },
        nr::OBJ_CONTROL => match a[0] {
            op::CTL_FCNTL => files::fcntl(t, i32a(a[1]), a[2], a[3]),
            op::CTL_IOCTL => files::ioctl(t, i32a(a[1]), a[2] & 0xFFFF_FFFF, a[3]),
            // One user, one machine: advisory locks and hints cost nothing.
            op::CTL_FLOCK | op::CTL_ADVISE | op::CTL_READAHEAD => Ok(0),
            op::CTL_ALLOCATE => files::fallocate(t, i32a(a[1]), a[2], i(a[3]), i(a[4])),
            _ => Err(EINVAL),
        },
        nr::OBJ_DIR_READ => files::getdents64(t, i32a(a[0]), a[1], a[2]),
        nr::OBJ_NAME_OP => match a[0] {
            op::NAME_MKDIR => files::mkdirat(t, i32a(a[1]), a[2], a[3]),
            op::NAME_UNLINK => files::unlinkat(t, i32a(a[1]), a[2], a[3]),
            op::NAME_RENAME => files::renameat(t, i32a(a[1]), a[2], i32a(a[3]), a[4], a[5]),
            op::NAME_LINK => files::linkat(t, i32a(a[1]), a[2], i32a(a[3]), a[4], a[5]),
            op::NAME_SYMLINK => files::symlinkat(t, a[1], i32a(a[2]), a[3]),
            op::NAME_READLINK => files::readlinkat(t, i32a(a[1]), a[2], a[3], a[4]),
            op::NAME_ACCESS => files::faccessat(t, i32a(a[1]), a[2], a[3], a[4]),
            op::NAME_CHMOD => files::fchmodat(t, i32a(a[1]), a[2], a[3]),
            op::NAME_FCHMOD => files::fchmod(t, i32a(a[1]), a[2]),
            // One account per session (spec 5 is enforced by Nova's own
            // tree): ownership of a program's files does not change.
            op::NAME_CHOWN | op::NAME_FCHOWN => Ok(0),
            op::NAME_UTIMENS => files::utimensat(t, i32a(a[1]), a[2], a[3], a[4]),
            op::NAME_TRUNCATE => files::truncate(t, a[1], i(a[2])),
            _ => Err(EINVAL),
        },
        nr::OBJ_RESIZE => files::ftruncate(t, i32a(a[0]), i(a[1])),
        // Whatever the volume holds in its cache reaches the disk: a
        // database committing must get exactly that.
        nr::OBJ_SYNC => match a[0] {
            op::SYNC_OBJECT => files::fsync(t, i32a(a[1])),
            op::SYNC_ALL => {
                let _ = crate::fs::flush_all();
                Ok(0)
            }
            _ => Err(EINVAL),
        },
        nr::OBJ_PIPE => files::pipe2(t, a[0], a[1]),
        nr::OBJ_EVENT => files::eventfd2(t, a[0], a[1]),
        nr::OBJ_MEMORY => files::memfd_create(t, a[0], a[1]),
        nr::OBJ_WAIT => match a[0] {
            op::WAIT_POLL => poll::ppoll(t, a[1], a[2], a[3], a[4]),
            op::WAIT_SELECT => poll::pselect6(t, a[1], a[2], a[3], a[4], a[5], 0),
            _ => Err(EINVAL),
        },
        nr::OBJ_WATCH_CREATE => files::epoll_create1(t, a[0]),
        nr::OBJ_WATCH_CTL => poll::epoll_ctl(t, i32a(a[0]), a[1], i32a(a[2]), a[3]),
        nr::OBJ_WATCH_WAIT => poll::epoll_pwait(t, i32a(a[0]), a[1], i32a(a[2]), i32a(a[3]), a[4]),
        nr::OBJ_COPY => match a[0] {
            op::COPY_SENDFILE => files::sendfile(t, i32a(a[1]), i32a(a[2]), a[3], a[4]),
            _ => Err(EINVAL),
        },

        // ---- domain 0A: threads ----
        nr::THREAD_CREATE => {
            if a[0] & task::CLONE_THREAD == 0 {
                return Err(EINVAL);
            }
            task::clone(t, frame, a[0], a[1], a[2], a[3], a[4])
        }
        nr::THREAD_EXIT => crate::posix::exit_thread(t, a[0] as i32 as i64 & 0xFF),
        nr::THREAD_SELF => Ok(t.tid),
        nr::THREAD_SET_TLS => task::arch_prctl(a[0], a[1]),
        nr::THREAD_WAIT_ADDR | nr::THREAD_WAKE_ADDR => task::futex(t, a[0], a[1], a[2], a[3], a[4], a[5]),
        nr::THREAD_SET_NAME => task::prctl(t, 15, a[0]),
        nr::THREAD_SLEEP_NS => match a[0] {
            op::SLEEP_RELATIVE => crate::posix::time::nanosleep(a[1], a[2]),
            op::SLEEP_CLOCK => crate::posix::time::clock_nanosleep(a[1], a[2], a[3], a[4]),
            _ => Err(EINVAL),
        },
        nr::THREAD_CLEAR_ADDR => {
            t.clear_child_tid.store(a[0], Ordering::Release);
            Ok(t.tid)
        }
        nr::THREAD_ROBUST => match a[0] {
            op::ROBUST_SET => {
                t.robust_list.store(a[1], Ordering::Relaxed);
                Ok(0)
            }
            _ => Err(ENOSYS),
        },

        // ---- domain 02: processes ----
        nr::PROC_YIELD => {
            crate::scheduler::yield_now();
            Ok(0)
        }
        nr::PROC_FORK => {
            if a[0] & task::CLONE_THREAD != 0 {
                return Err(EINVAL);
            }
            crate::posix::process::fork(t, frame, a[0], a[1], a[2], a[3], a[4])
        }
        nr::PROC_WAIT_ANY => match a[0] {
            op::WAIT_PID => crate::posix::process::wait4(t, a[1] as i32 as i64, a[2], a[3]),
            op::WAIT_ID => crate::posix::process::waitid(t, a[1], a[2], a[3], a[4]),
            _ => Err(EINVAL),
        },
        nr::PROC_CWD => match a[0] {
            op::CWD_GET => files::getcwd(t, a[1], a[2]),
            op::CWD_SET => files::chdir(t, a[1]),
            op::CWD_SET_FD => files::fchdir(t, i32a(a[1])),
            _ => Err(EINVAL),
        },
        nr::PROC_LIMITS => match a[0] {
            op::LIMIT_RESOURCE => {
                if a[1] != 0 && a[1] != t.process.tgid {
                    return Err(ESRCH);
                }
                task::prlimit(t, a[2], a[3], a[4])
            }
            op::LIMIT_USAGE => crate::posix::misc::getrusage(t, a[1], a[2]),
            op::LIMIT_SYSINFO => crate::posix::misc::sysinfo(a[1]),
            op::LIMIT_TIMES => Ok(crate::posix::time::monotonic_ns() / 10_000_000),
            _ => Err(EINVAL),
        },
        nr::PROC_EXIT_ALL => crate::posix::exit_group(t, a[0] as i32 as i64 & 0xFF),
        nr::PROC_IDENTITY => identity(t, a),
        nr::PROC_SCHED => match a[0] {
            op::SCHED_GET_AFFINITY => task::sched_getaffinity(a[2], a[3]),
            op::SCHED_CPU => {
                write(a[1], crate::smp::current_cpu_id() as u32)?;
                write(a[2], 0u32)
            }
            op::SCHED_GET_PARAM => write(a[2], 0u32),
            // The priority is the binary type's (spec 7.1): fixed.
            op::SCHED_GET_PRIORITY => Ok(20),
            op::SCHED_SET_AFFINITY | op::SCHED_GET_POLICY | op::SCHED_SET_POLICY | op::SCHED_SET_PRIORITY
            | op::SCHED_PRIORITY_RANGE => Ok(0),
            _ => Err(EINVAL),
        },
        nr::PROC_CONTROL => task::prctl(t, a[0], a[1]),

        // ---- domain 03: memory ----
        nr::MEM_MAP => mm::mmap(t, a[0], a[1], a[2], a[3], i32a(a[4]), a[5]),
        nr::MEM_UNMAP => mm::munmap(t, a[0], a[1]),
        nr::MEM_REMAP => mm::mremap(t, a[0], a[1], a[2], a[3], a[4]),
        nr::MEM_ADVISE => match a[0] {
            op::MEM_ADVICE => mm::madvise(t, a[1], a[2], a[3]),
            // Shared file pages are the file itself (tmpfs, the cache).
            op::MEM_SYNC => crate::posix::mm::msync(t, a[1], a[2]),
            _ => Err(ENOSYS),
        },
        nr::MEM_BARRIER => task::membarrier(t, a[0]),
        nr::MEM_PROTECT_MAP => mm::mprotect(t, a[0], a[1], a[2]),
        nr::MEM_BREAK => mm::brk(t, a[0]),

        // ---- domain 07: time ----
        nr::TIME_CLOCK => match a[0] {
            op::CLOCK_GET => crate::posix::time::clock_gettime(a[1], a[2]),
            op::CLOCK_RESOLUTION => write(a[2], crate::posix::time::Timespec { sec: 0, nsec: 1 }),
            op::CLOCK_TIMEOFDAY => crate::posix::time::gettimeofday(a[1]),
            op::CLOCK_SECONDS => {
                let now = crate::posix::time::realtime_ns() / 1_000_000_000;
                write(a[1], now)?;
                Ok(now)
            }
            _ => Err(EINVAL),
        },

        // ---- domain 08: sockets ----
        nr::NET_SOCKET => net::socket(t, a[0], a[1], a[2]),
        nr::NET_PAIR => net::socketpair(t, a[0], a[1], a[2], a[3]),
        nr::NET_BIND => net::bind(t, i32a(a[0]), a[1], a[2]),
        nr::NET_LISTEN => net::listen(t, i32a(a[0]), a[1]),
        nr::NET_ACCEPT => net::accept(t, i32a(a[0]), a[1], a[2], a[3]),
        nr::NET_CONNECT_TO => net::connect(t, i32a(a[0]), a[1], a[2]),
        nr::NET_SEND_MSG => net::sendmsg(t, i32a(a[0]), a[1], a[2]),
        nr::NET_RECV_MSG => net::recvmsg(t, i32a(a[0]), a[1], a[2]),
        nr::NET_OPTION => match a[0] {
            op::OPTION_SET => net::setsockopt(t, i32a(a[1]), a[2], a[3], a[4], a[5]),
            op::OPTION_GET => net::getsockopt(t, i32a(a[1]), a[2], a[3], a[4], a[5]),
            _ => Err(EINVAL),
        },
        nr::NET_SHUTDOWN => net::shutdown(t, i32a(a[0]), a[1]),
        nr::NET_ADDRESS => match a[0] {
            op::ADDRESS_LOCAL => net::getsockname(t, i32a(a[1]), a[2], a[3]),
            op::ADDRESS_PEER => net::getpeername(t, i32a(a[1]), a[2], a[3]),
            _ => Err(EINVAL),
        },

        // ---- domain 0B: signals ----
        nr::SIG_ACTION => signal::rt_sigaction(t, a[0], a[1], a[2], a[3]),
        nr::SIG_MASK => signal::rt_sigprocmask(t, a[0], a[1], a[2], a[3]),
        nr::SIG_SEND => match a[0] {
            op::SEND_PROCESS => {
                let pid = a[1] as i32 as i64;
                if pid == t.process.tgid as i64 || pid == 0 {
                    signal::send_to_process(&t.process, a[2] as u32)
                } else if let Some(p) = crate::posix::process(pid.unsigned_abs()) {
                    signal::send_to_process(&p, a[2] as u32)
                } else {
                    Err(ESRCH)
                }
            }
            op::SEND_THREAD => signal::send_to_thread(a[1], a[2] as u32),
            op::SEND_GROUP_THREAD => {
                let target = crate::posix::thread_by_tid(a[2]).ok_or(ESRCH)?;
                if target.process.tgid != a[1] {
                    return Err(ESRCH);
                }
                signal::send_to_thread(a[2], a[3] as u32)
            }
            _ => Err(EINVAL),
        },
        nr::SIG_ALTSTACK => signal::sigaltstack(t, a[0], a[1]),
        nr::SIG_WAIT => match a[0] {
            // Nothing is queued for a synchronous wait.
            op::SIG_TIMED => Err(EAGAIN),
            op::SIG_SUSPEND | op::SIG_PAUSE => {
                if a[0] == op::SIG_SUSPEND {
                    let mask: u64 = crate::vm::read_user(a[1]).map_err(|_| EFAULT)?;
                    t.sigmask.store(mask, Ordering::Release);
                }
                let _ = crate::posix::wait::wait_until::<()>(&[], None, || None);
                Err(EINTR)
            }
            op::SIG_PENDING => {
                let pending = t.pending.load(Ordering::Acquire) | t.process.pending.load(Ordering::Acquire);
                write(a[1], pending)
            }
            _ => Err(EINVAL),
        },

        _ => {
            crate::serial_println!("[natif] {} : appel 0x{:04x} ({}) inconnu - ENOSYS", t.process.name, n, name(n));
            Err(ENOSYS)
        }
    }
}

/// `PROC_IDENTITY`: who the process is. One account per session: the
/// program runs as root of its own view, in a session of its own.
fn identity(t: &Arc<Thread>, a: [u64; 6]) -> LxResult<u64> {
    match a[0] {
        op::ID_PID | op::ID_PGID | op::ID_SID | op::ID_SETSID => Ok(t.process.tgid),
        op::ID_PPID => Ok(if t.process.parent != 0 { t.process.parent } else { 1 }),
        op::ID_UID | op::ID_EUID | op::ID_GID | op::ID_EGID | op::ID_GROUPS | op::ID_SETPGID | op::ID_SET => Ok(0),
        op::ID_RESUID | op::ID_RESGID => {
            for addr in [a[1], a[2], a[3]] {
                write(addr, 0u32)?;
            }
            Ok(0)
        }
        op::ID_UMASK => Ok(t.process.umask.swap(a[1] as u32 & 0o777, Ordering::AcqRel) as u64),
        op::ID_UNAME => crate::posix::misc::uname(t, a[1]),
        op::ID_CAPGET => Err(EINVAL),
        _ => Err(EINVAL),
    }
}

/// Symbolic name of a call, for the trace and the logs.
pub fn name(n: u64) -> &'static str {
    match n {
        nr::RANDOM => "SYS_RANDOM",
        nr::PROC_YIELD => "SYS_PROC_YIELD",
        nr::PROC_FORK => "SYS_PROC_FORK",
        nr::PROC_EXEC => "SYS_PROC_EXEC",
        nr::PROC_WAIT_ANY => "SYS_PROC_WAIT_ANY",
        nr::PROC_CWD => "SYS_PROC_CWD",
        nr::PROC_LIMITS => "SYS_PROC_LIMITS",
        nr::PROC_EXIT_ALL => "SYS_PROC_EXIT_ALL",
        nr::PROC_IDENTITY => "SYS_PROC_IDENTITY",
        nr::PROC_SCHED => "SYS_PROC_SCHED",
        nr::PROC_CONTROL => "SYS_PROC_CONTROL",
        nr::MEM_MAP => "SYS_MEM_MAP",
        nr::MEM_UNMAP => "SYS_MEM_UNMAP",
        nr::MEM_REMAP => "SYS_MEM_REMAP",
        nr::MEM_ADVISE => "SYS_MEM_ADVISE",
        nr::MEM_BARRIER => "SYS_MEM_BARRIER",
        nr::MEM_PROTECT_MAP => "SYS_MEM_PROTECT_MAP",
        nr::MEM_BREAK => "SYS_MEM_BREAK",
        nr::TIME_CLOCK => "SYS_TIME_CLOCK_NS",
        nr::TIME_PAGE => "SYS_TIME_PAGE",
        nr::NET_SOCKET => "SYS_NET_SOCKET",
        nr::NET_PAIR => "SYS_NET_PAIR",
        nr::NET_BIND => "SYS_NET_BIND",
        nr::NET_LISTEN => "SYS_NET_LISTEN",
        nr::NET_ACCEPT => "SYS_NET_ACCEPT",
        nr::NET_CONNECT_TO => "SYS_NET_CONNECT_TO",
        nr::NET_SEND_MSG => "SYS_NET_SEND_MSG",
        nr::NET_RECV_MSG => "SYS_NET_RECV_MSG",
        nr::NET_OPTION => "SYS_NET_OPTION",
        nr::NET_SHUTDOWN => "SYS_NET_SHUTDOWN",
        nr::NET_ADDRESS => "SYS_NET_ADDRESS",
        nr::OBJ_OPEN => "SYS_OBJ_OPEN",
        nr::OBJ_CLOSE => "SYS_OBJ_CLOSE",
        nr::OBJ_READ => "SYS_OBJ_READ",
        nr::OBJ_WRITE => "SYS_OBJ_WRITE",
        nr::OBJ_SEEK => "SYS_OBJ_SEEK",
        nr::OBJ_STAT => "SYS_OBJ_STAT",
        nr::OBJ_DUP => "SYS_OBJ_DUP",
        nr::OBJ_CONTROL => "SYS_OBJ_CONTROL",
        nr::OBJ_DIR_READ => "SYS_OBJ_DIR_READ",
        nr::OBJ_NAME_OP => "SYS_OBJ_NAME_OP",
        nr::OBJ_RESIZE => "SYS_OBJ_RESIZE",
        nr::OBJ_SYNC => "SYS_OBJ_SYNC",
        nr::OBJ_PIPE => "SYS_OBJ_PIPE",
        nr::OBJ_EVENT => "SYS_OBJ_EVENT",
        nr::OBJ_MEMORY => "SYS_OBJ_MEMORY",
        nr::OBJ_WAIT => "SYS_OBJ_WAIT",
        nr::OBJ_WATCH_CREATE => "SYS_OBJ_WATCH_CREATE",
        nr::OBJ_WATCH_CTL => "SYS_OBJ_WATCH_CTL",
        nr::OBJ_WATCH_WAIT => "SYS_OBJ_WATCH_WAIT",
        nr::OBJ_COPY => "SYS_OBJ_COPY",
        nr::THREAD_CREATE => "SYS_THREAD_CREATE",
        nr::THREAD_EXIT => "SYS_THREAD_EXIT",
        nr::THREAD_SELF => "SYS_THREAD_SELF",
        nr::THREAD_SET_TLS => "SYS_THREAD_SET_TLS",
        nr::THREAD_WAIT_ADDR => "SYS_THREAD_WAIT_ADDR",
        nr::THREAD_WAKE_ADDR => "SYS_THREAD_WAKE_ADDR",
        nr::THREAD_SET_NAME => "SYS_THREAD_SET_NAME",
        nr::THREAD_SLEEP_NS => "SYS_THREAD_SLEEP_NS",
        nr::THREAD_CLEAR_ADDR => "SYS_THREAD_CLEAR_ADDR",
        nr::THREAD_ROBUST => "SYS_THREAD_ROBUST",
        nr::SIG_ACTION => "SYS_SIG_ACTION",
        nr::SIG_MASK => "SYS_SIG_MASK",
        nr::SIG_SEND => "SYS_SIG_SEND",
        nr::SIG_RETURN => "SYS_SIG_RETURN",
        nr::SIG_ALTSTACK => "SYS_SIG_ALTSTACK",
        nr::SIG_WAIT => "SYS_SIG_WAIT",
        _ => "?",
    }
}

/* Nova OS -- the system calls of the Nova C library (spec 6.7).
 *
 * musl reaches its kernel through one place on x86_64, the __syscallN
 * functions of arch/x86_64/syscall_arch.h, plus six assembly files. In
 * the Nova C library the __syscallN functions call __nova_syscall below,
 * which turns musl's request (named by the Linux number musl's sources
 * use internally) into the Nova call that serves it -- one of the domains
 * of spec 6.7 -- and issues it. The `syscall()` function a program calls
 * by hand (Rust's standard library does, for futexes and random bytes)
 * comes through here too. Nothing leaves the library with a Linux number:
 * the kernel would not understand it from a Nova program.
 *
 * What the kernel has no service for returns -ENOSYS without a call,
 * which is what musl and the programs above it expect from an old kernel.
 *
 * The structures passed are musl's own, unchanged (spec 6.7: the user
 * ABI of musl is kept). */
#include <features.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <sys/uio.h>
#include <time.h>
#include "syscall.h"
#include "nova_abi.h"

#define CLONE_VM     0x00000100
#define CLONE_VFORK  0x00004000
#define CLONE_THREAD 0x00010000
#define SIGCHLD_NR   17

static inline long nova(long nr, long a, long b, long c, long d, long e, long f)
{
	unsigned long ret;
	register long r10 __asm__("r10") = d;
	register long r8 __asm__("r8") = e;
	register long r9 __asm__("r9") = f;
	__asm__ __volatile__ ("syscall" : "=a"(ret) : "a"(nr), "D"(a), "S"(b), "d"(c),
			      "r"(r10), "r"(r8), "r"(r9) : "rcx", "r11", "memory");
	return ret;
}

/* A timeout in milliseconds (negative: none) as a timespec pointer. */
static struct timespec *ms_to_ts(long ms, struct timespec *ts)
{
	if (ms < 0) return 0;
	ts->tv_sec = ms / 1000;
	ts->tv_nsec = ms % 1000 * 1000000;
	return ts;
}

static long ts_to_ms(const struct timespec *ts)
{
	if (!ts) return -1;
	return ts->tv_sec * 1000 + (ts->tv_nsec + 999999) / 1000000;
}

static int is_wait_futex(long op)
{
	switch (op & 127) {
	case 0: /* FUTEX_WAIT */
	case 6: /* FUTEX_LOCK_PI */
	case 8: /* FUTEX_TRYLOCK_PI */
	case 9: /* FUTEX_WAIT_BITSET */
	case 11: /* FUTEX_WAIT_REQUEUE_PI */
		return 1;
	}
	return 0;
}

hidden long __nova_syscall(long n, long a, long b, long c, long d, long e, long f)
{
	struct timespec ts;
	switch (n) {
	/* ---- objects ---- */
	case SYS_read: return nova(NOVA_OBJ_READ, a, b, c, -1, 0, 0);
	case SYS_write: return nova(NOVA_OBJ_WRITE, a, b, c, -1, 0, 0);
	case SYS_pread64: return d < 0 ? -EINVAL : nova(NOVA_OBJ_READ, a, b, c, d, 0, 0);
	case SYS_pwrite64: return d < 0 ? -EINVAL : nova(NOVA_OBJ_WRITE, a, b, c, d, 0, 0);
	case SYS_readv: return nova(NOVA_OBJ_READ, a, b, c, -1, NOVA_IO_VECTOR, 0);
	case SYS_writev: return nova(NOVA_OBJ_WRITE, a, b, c, -1, NOVA_IO_VECTOR, 0);
	case SYS_preadv: return d < 0 ? -EINVAL : nova(NOVA_OBJ_READ, a, b, c, d, NOVA_IO_VECTOR, 0);
	case SYS_pwritev: return d < 0 ? -EINVAL : nova(NOVA_OBJ_WRITE, a, b, c, d, NOVA_IO_VECTOR, 0);
	case SYS_preadv2: return nova(NOVA_OBJ_READ, a, b, c, d < 0 ? -1 : d, NOVA_IO_VECTOR, 0);
	case SYS_pwritev2: return nova(NOVA_OBJ_WRITE, a, b, c, d < 0 ? -1 : d, NOVA_IO_VECTOR, 0);
	case SYS_open: return nova(NOVA_OBJ_OPEN, AT_FDCWD, a, b, c, 0, 0);
	case SYS_openat: return nova(NOVA_OBJ_OPEN, a, b, c, d, 0, 0);
	case SYS_creat: return nova(NOVA_OBJ_OPEN, AT_FDCWD, a, O_CREAT|O_WRONLY|O_TRUNC, b, 0, 0);
	case SYS_close: return nova(NOVA_OBJ_CLOSE, a, a, 0, 0, 0, 0);
	case SYS_close_range: return nova(NOVA_OBJ_CLOSE, a, b, c, 0, 0, 0);
	case SYS_lseek: return nova(NOVA_OBJ_SEEK, a, b, c, 0, 0, 0);
	case SYS_stat: return nova(NOVA_OBJ_STAT, NOVA_STAT_AT, AT_FDCWD, a, b, 0, 0);
	case SYS_lstat: return nova(NOVA_OBJ_STAT, NOVA_STAT_AT, AT_FDCWD, a, b, AT_SYMLINK_NOFOLLOW, 0);
	case SYS_fstat: return nova(NOVA_OBJ_STAT, NOVA_STAT_FD, a, b, 0, 0, 0);
	case SYS_newfstatat: return nova(NOVA_OBJ_STAT, NOVA_STAT_AT, a, b, c, d, 0);
	case SYS_statx: return nova(NOVA_OBJ_STAT, NOVA_STAT_X, a, b, c, d, e);
	case SYS_statfs: return nova(NOVA_OBJ_STAT, NOVA_STAT_FS, a, b, 0, 0, 0);
	case SYS_fstatfs: return nova(NOVA_OBJ_STAT, NOVA_STAT_FS_FD, a, b, 0, 0, 0);
	case SYS_dup: return nova(NOVA_OBJ_DUP, a, 0, 0, NOVA_DUP, 0, 0);
	case SYS_dup2: return nova(NOVA_OBJ_DUP, a, b, 0, NOVA_DUP_TO, 0, 0);
	case SYS_dup3: return nova(NOVA_OBJ_DUP, a, b, c, NOVA_DUP_TO_FLAGS, 0, 0);
	case SYS_fcntl: return nova(NOVA_OBJ_CONTROL, NOVA_CTL_FCNTL, a, b, c, 0, 0);
	case SYS_ioctl: return nova(NOVA_OBJ_CONTROL, NOVA_CTL_IOCTL, a, b, c, 0, 0);
	case SYS_flock: return nova(NOVA_OBJ_CONTROL, NOVA_CTL_FLOCK, a, b, 0, 0, 0);
	case SYS_fadvise64: return nova(NOVA_OBJ_CONTROL, NOVA_CTL_ADVISE, a, b, c, d, 0);
	case SYS_fallocate: return nova(NOVA_OBJ_CONTROL, NOVA_CTL_ALLOCATE, a, b, c, d, 0);
	case SYS_readahead: return nova(NOVA_OBJ_CONTROL, NOVA_CTL_READAHEAD, a, b, c, 0, 0);
	case SYS_getdents64: return nova(NOVA_OBJ_DIR_READ, a, b, c, 0, 0, 0);
	case SYS_mkdir: return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_MKDIR, AT_FDCWD, a, b, 0, 0);
	case SYS_mkdirat: return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_MKDIR, a, b, c, 0, 0);
	case SYS_rmdir: return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_UNLINK, AT_FDCWD, a, AT_REMOVEDIR, 0, 0);
	case SYS_unlink: return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_UNLINK, AT_FDCWD, a, 0, 0, 0);
	case SYS_unlinkat: return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_UNLINK, a, b, c, 0, 0);
	case SYS_rename: return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_RENAME, AT_FDCWD, a, AT_FDCWD, b, 0);
	case SYS_renameat: return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_RENAME, a, b, c, d, 0);
	case SYS_renameat2: return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_RENAME, a, b, c, d, e);
	case SYS_link: return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_LINK, AT_FDCWD, a, AT_FDCWD, b, 0);
	case SYS_linkat: return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_LINK, a, b, c, d, e);
	case SYS_symlink: return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_SYMLINK, a, AT_FDCWD, b, 0, 0);
	case SYS_symlinkat: return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_SYMLINK, a, b, c, 0, 0);
	case SYS_readlink: return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_READLINK, AT_FDCWD, a, b, c, 0);
	case SYS_readlinkat: return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_READLINK, a, b, c, d, 0);
	case SYS_access: return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_ACCESS, AT_FDCWD, a, b, 0, 0);
	case SYS_faccessat: return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_ACCESS, a, b, c, 0, 0);
	case SYS_faccessat2: return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_ACCESS, a, b, c, d, 0);
	case SYS_chmod: return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_CHMOD, AT_FDCWD, a, b, 0, 0);
	case SYS_fchmodat: return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_CHMOD, a, b, c, 0, 0);
	case SYS_fchmod: return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_FCHMOD, a, b, 0, 0, 0);
	case SYS_chown: case SYS_lchown: case SYS_fchownat:
		return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_CHOWN, a, b, c, d, e);
	case SYS_fchown: return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_FCHOWN, a, b, c, 0, 0);
	case SYS_utimensat: return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_UTIMENS, a, b, c, d, 0);
	case SYS_truncate: return nova(NOVA_OBJ_NAME_OP, NOVA_NAME_TRUNCATE, a, b, 0, 0, 0);
	case SYS_ftruncate: return nova(NOVA_OBJ_RESIZE, a, b, 0, 0, 0, 0);
	case SYS_fsync: case SYS_fdatasync: case SYS_syncfs:
		return nova(NOVA_OBJ_SYNC, NOVA_SYNC_OBJECT, a, 0, 0, 0, 0);
	case SYS_sync: return nova(NOVA_OBJ_SYNC, NOVA_SYNC_ALL, 0, 0, 0, 0, 0);
	case SYS_pipe: return nova(NOVA_OBJ_PIPE, a, 0, 0, 0, 0, 0);
	case SYS_pipe2: return nova(NOVA_OBJ_PIPE, a, b, 0, 0, 0, 0);
	case SYS_eventfd: return nova(NOVA_OBJ_EVENT, a, 0, 0, 0, 0, 0);
	case SYS_eventfd2: return nova(NOVA_OBJ_EVENT, a, b, 0, 0, 0, 0);
	case SYS_memfd_create: return nova(NOVA_OBJ_MEMORY, a, b, 0, 0, 0, 0);
	case SYS_poll: return nova(NOVA_OBJ_WAIT, NOVA_WAIT_POLL, a, b, (long)ms_to_ts(c, &ts), 0, 0);
	case SYS_ppoll: return nova(NOVA_OBJ_WAIT, NOVA_WAIT_POLL, a, b, c, d, 0);
	case SYS_select: {
		const struct timeval *tv = (const struct timeval *)e;
		struct timespec *t = 0;
		if (tv) {
			ts.tv_sec = tv->tv_sec;
			ts.tv_nsec = tv->tv_usec * 1000;
			t = &ts;
		}
		return nova(NOVA_OBJ_WAIT, NOVA_WAIT_SELECT, a, b, c, d, (long)t);
	}
	case SYS_pselect6: return nova(NOVA_OBJ_WAIT, NOVA_WAIT_SELECT, a, b, c, d, e);
	case SYS_epoll_create: return nova(NOVA_OBJ_WATCH_CREATE, 0, 0, 0, 0, 0, 0);
	case SYS_epoll_create1: return nova(NOVA_OBJ_WATCH_CREATE, a, 0, 0, 0, 0, 0);
	case SYS_epoll_ctl: return nova(NOVA_OBJ_WATCH_CTL, a, b, c, d, 0, 0);
	case SYS_epoll_wait: return nova(NOVA_OBJ_WATCH_WAIT, a, b, c, d, 0, 0);
	case SYS_epoll_pwait: return nova(NOVA_OBJ_WATCH_WAIT, a, b, c, d, e, 0);
	case SYS_epoll_pwait2: return nova(NOVA_OBJ_WATCH_WAIT, a, b, c, ts_to_ms((const struct timespec *)d), e, 0);
	case SYS_sendfile: return nova(NOVA_OBJ_COPY, NOVA_COPY_SENDFILE, a, b, c, d, 0);

	/* ---- threads ---- */
	case SYS_clone:
		/* A thread needs a fresh stack to return on: that is __clone's
		 * work (clone.s). Here, only what returns on the caller's own
		 * stack -- a copy of the process. */
		if (a & (CLONE_THREAD|CLONE_VM)) return -ENOSYS;
		return nova(NOVA_PROC_FORK, a, b, c, d, e, 0);
	case SYS_exit: return nova(NOVA_THREAD_EXIT, a, 0, 0, 0, 0, 0);
	case SYS_gettid: return nova(NOVA_THREAD_SELF, 0, 0, 0, 0, 0, 0);
	case SYS_arch_prctl: return nova(NOVA_THREAD_SET_TLS, a, b, 0, 0, 0, 0);
	case SYS_futex:
		return nova(is_wait_futex(b) ? NOVA_THREAD_WAIT_ADDR : NOVA_THREAD_WAKE_ADDR, a, b, c, d, e, f);
	case SYS_nanosleep: return nova(NOVA_THREAD_SLEEP_NS, NOVA_SLEEP_RELATIVE, a, b, 0, 0, 0);
	case SYS_clock_nanosleep: return nova(NOVA_THREAD_SLEEP_NS, NOVA_SLEEP_CLOCK, a, b, c, d, 0);
	case SYS_set_tid_address: return nova(NOVA_THREAD_CLEAR_ADDR, a, 0, 0, 0, 0, 0);
	case SYS_set_robust_list: return nova(NOVA_THREAD_ROBUST, NOVA_ROBUST_SET, a, b, 0, 0, 0);
	case SYS_get_robust_list: return nova(NOVA_THREAD_ROBUST, NOVA_ROBUST_GET, a, b, c, 0, 0);
	case SYS_sched_yield: return nova(NOVA_PROC_YIELD, 0, 0, 0, 0, 0, 0);

	/* ---- processes ---- */
	case SYS_fork: return nova(NOVA_PROC_FORK, SIGCHLD_NR, 0, 0, 0, 0, 0);
	/* vfork proper is vfork.s: from C, the child would return on the
	 * parent's stack. A copy of the process is what C can do. */
	case SYS_vfork: return nova(NOVA_PROC_FORK, SIGCHLD_NR, 0, 0, 0, 0, 0);
	case SYS_execve: return nova(NOVA_PROC_EXEC, a, b, c, 0, 0, 0);
	case SYS_exit_group: return nova(NOVA_PROC_EXIT_ALL, a, 0, 0, 0, 0, 0);
	case SYS_wait4: return nova(NOVA_PROC_WAIT_ANY, NOVA_WAIT_PID, a, b, c, d, 0);
	case SYS_waitid: return nova(NOVA_PROC_WAIT_ANY, NOVA_WAIT_ID, a, b, c, d, e);
	case SYS_getcwd: return nova(NOVA_PROC_CWD, NOVA_CWD_GET, a, b, 0, 0, 0);
	case SYS_chdir: return nova(NOVA_PROC_CWD, NOVA_CWD_SET, a, 0, 0, 0, 0);
	case SYS_fchdir: return nova(NOVA_PROC_CWD, NOVA_CWD_SET_FD, a, 0, 0, 0, 0);
	case SYS_getrlimit: return nova(NOVA_PROC_LIMITS, NOVA_LIMIT_RESOURCE, 0, a, 0, b, 0);
	case SYS_setrlimit: return nova(NOVA_PROC_LIMITS, NOVA_LIMIT_RESOURCE, 0, a, b, 0, 0);
	case SYS_prlimit64: return nova(NOVA_PROC_LIMITS, NOVA_LIMIT_RESOURCE, a, b, c, d, 0);
	case SYS_getrusage: return nova(NOVA_PROC_LIMITS, NOVA_LIMIT_USAGE, a, b, 0, 0, 0);
	case SYS_sysinfo: return nova(NOVA_PROC_LIMITS, NOVA_LIMIT_SYSINFO, a, 0, 0, 0, 0);
	case SYS_times: return nova(NOVA_PROC_LIMITS, NOVA_LIMIT_TIMES, a, 0, 0, 0, 0);
	case SYS_getpid: return nova(NOVA_PROC_IDENTITY, NOVA_ID_PID, 0, 0, 0, 0, 0);
	case SYS_getppid: return nova(NOVA_PROC_IDENTITY, NOVA_ID_PPID, 0, 0, 0, 0, 0);
	case SYS_getuid: return nova(NOVA_PROC_IDENTITY, NOVA_ID_UID, 0, 0, 0, 0, 0);
	case SYS_geteuid: return nova(NOVA_PROC_IDENTITY, NOVA_ID_EUID, 0, 0, 0, 0, 0);
	case SYS_getgid: return nova(NOVA_PROC_IDENTITY, NOVA_ID_GID, 0, 0, 0, 0, 0);
	case SYS_getegid: return nova(NOVA_PROC_IDENTITY, NOVA_ID_EGID, 0, 0, 0, 0, 0);
	case SYS_getresuid: return nova(NOVA_PROC_IDENTITY, NOVA_ID_RESUID, a, b, c, 0, 0);
	case SYS_getresgid: return nova(NOVA_PROC_IDENTITY, NOVA_ID_RESGID, a, b, c, 0, 0);
	case SYS_getgroups: return nova(NOVA_PROC_IDENTITY, NOVA_ID_GROUPS, a, b, 0, 0, 0);
	case SYS_setsid: return nova(NOVA_PROC_IDENTITY, NOVA_ID_SETSID, 0, 0, 0, 0, 0);
	case SYS_setpgid: return nova(NOVA_PROC_IDENTITY, NOVA_ID_SETPGID, a, b, 0, 0, 0);
	case SYS_getpgid: case SYS_getpgrp: return nova(NOVA_PROC_IDENTITY, NOVA_ID_PGID, a, 0, 0, 0, 0);
	case SYS_getsid: return nova(NOVA_PROC_IDENTITY, NOVA_ID_SID, a, 0, 0, 0, 0);
	case SYS_umask: return nova(NOVA_PROC_IDENTITY, NOVA_ID_UMASK, a, 0, 0, 0, 0);
	case SYS_uname: return nova(NOVA_PROC_IDENTITY, NOVA_ID_UNAME, a, 0, 0, 0, 0);
	case SYS_capget: return nova(NOVA_PROC_IDENTITY, NOVA_ID_CAPGET, a, b, 0, 0, 0);
	case SYS_setuid: case SYS_setgid: case SYS_setreuid: case SYS_setregid:
	case SYS_setresuid: case SYS_setresgid: case SYS_setfsuid: case SYS_setfsgid:
	case SYS_setgroups:
		return nova(NOVA_PROC_IDENTITY, NOVA_ID_SET, a, b, c, 0, 0);
	case SYS_sched_getaffinity: return nova(NOVA_PROC_SCHED, NOVA_SCHED_GET_AFFINITY, a, b, c, 0, 0);
	case SYS_sched_setaffinity: return nova(NOVA_PROC_SCHED, NOVA_SCHED_SET_AFFINITY, a, b, c, 0, 0);
	case SYS_getcpu: return nova(NOVA_PROC_SCHED, NOVA_SCHED_CPU, a, b, 0, 0, 0);
	case SYS_sched_getparam: return nova(NOVA_PROC_SCHED, NOVA_SCHED_GET_PARAM, a, b, 0, 0, 0);
	case SYS_sched_getscheduler: return nova(NOVA_PROC_SCHED, NOVA_SCHED_GET_POLICY, a, 0, 0, 0, 0);
	case SYS_sched_setscheduler: case SYS_sched_setparam:
		return nova(NOVA_PROC_SCHED, NOVA_SCHED_SET_POLICY, a, b, c, 0, 0);
	case SYS_getpriority: return nova(NOVA_PROC_SCHED, NOVA_SCHED_GET_PRIORITY, a, b, 0, 0, 0);
	case SYS_setpriority: return nova(NOVA_PROC_SCHED, NOVA_SCHED_SET_PRIORITY, a, b, c, 0, 0);
	case SYS_sched_get_priority_max: case SYS_sched_get_priority_min:
		return nova(NOVA_PROC_SCHED, NOVA_SCHED_PRIORITY_RANGE, a, 0, 0, 0, 0);
	case SYS_prctl: return nova(NOVA_PROC_CONTROL, a, b, c, d, e, 0);
	/* Memory is never swapped out: locking it is already true. */
	case SYS_mlock: case SYS_munlock: case SYS_mlockall: case SYS_munlockall:
		return 0;

	/* ---- memory ---- */
	case SYS_mmap: return nova(NOVA_MEM_MAP, a, b, c, d, e, f);
	case SYS_munmap: return nova(NOVA_MEM_UNMAP, a, b, 0, 0, 0, 0);
	case SYS_mremap: return nova(NOVA_MEM_REMAP, a, b, c, d, e, 0);
	case SYS_mprotect: return nova(NOVA_MEM_PROTECT_MAP, a, b, c, 0, 0, 0);
	case SYS_brk: return nova(NOVA_MEM_BREAK, a, 0, 0, 0, 0, 0);
	case SYS_madvise: return nova(NOVA_MEM_ADVISE, NOVA_MEM_ADVICE, a, b, c, 0, 0);
	case SYS_msync: return nova(NOVA_MEM_ADVISE, NOVA_MEM_SYNC, a, b, c, 0, 0);
	case SYS_mincore: return nova(NOVA_MEM_ADVISE, NOVA_MEM_RESIDENT, a, b, c, 0, 0);
	case SYS_membarrier: return nova(NOVA_MEM_BARRIER, a, b, 0, 0, 0, 0);

	/* ---- time ---- */
	case SYS_clock_gettime: return nova(NOVA_TIME_CLOCK, NOVA_CLOCK_GET, a, b, 0, 0, 0);
	case SYS_clock_getres: return nova(NOVA_TIME_CLOCK, NOVA_CLOCK_RESOLUTION, a, b, 0, 0, 0);
	case SYS_gettimeofday: return nova(NOVA_TIME_CLOCK, NOVA_CLOCK_TIMEOFDAY, a, b, 0, 0, 0);
	case SYS_time: return nova(NOVA_TIME_CLOCK, NOVA_CLOCK_SECONDS, a, 0, 0, 0, 0);

	/* ---- sockets ---- */
	case SYS_socket: return nova(NOVA_NET_SOCKET, a, b, c, 0, 0, 0);
	case SYS_socketpair: return nova(NOVA_NET_PAIR, a, b, c, d, 0, 0);
	case SYS_bind: return nova(NOVA_NET_BIND, a, b, c, 0, 0, 0);
	case SYS_listen: return nova(NOVA_NET_LISTEN, a, b, 0, 0, 0, 0);
	case SYS_accept: return nova(NOVA_NET_ACCEPT, a, b, c, 0, 0, 0);
	case SYS_accept4: return nova(NOVA_NET_ACCEPT, a, b, c, d, 0, 0);
	case SYS_connect: return nova(NOVA_NET_CONNECT_TO, a, b, c, 0, 0, 0);
	case SYS_sendmsg: return nova(NOVA_NET_SEND_MSG, a, b, c, 0, 0, 0);
	case SYS_recvmsg: return nova(NOVA_NET_RECV_MSG, a, b, c, 0, 0, 0);
	case SYS_sendto: {
		/* One call for both: a message with one piece. */
		struct iovec iov = { (void *)b, c };
		struct msghdr m = { .msg_name = (void *)e, .msg_namelen = f, .msg_iov = &iov, .msg_iovlen = 1 };
		return nova(NOVA_NET_SEND_MSG, a, (long)&m, d, 0, 0, 0);
	}
	case SYS_recvfrom: {
		socklen_t *alen = (socklen_t *)f;
		struct iovec iov = { (void *)b, c };
		struct msghdr m = { .msg_name = (void *)e, .msg_namelen = alen ? *alen : 0, .msg_iov = &iov, .msg_iovlen = 1 };
		long r = nova(NOVA_NET_RECV_MSG, a, (long)&m, d, 0, 0, 0);
		if (r >= 0 && alen) *alen = m.msg_namelen;
		return r;
	}
	case SYS_shutdown: return nova(NOVA_NET_SHUTDOWN, a, b, 0, 0, 0, 0);
	case SYS_setsockopt: return nova(NOVA_NET_OPTION, NOVA_OPTION_SET, a, b, c, d, e);
	case SYS_getsockopt: return nova(NOVA_NET_OPTION, NOVA_OPTION_GET, a, b, c, d, e);
	case SYS_getsockname: return nova(NOVA_NET_ADDRESS, NOVA_ADDRESS_LOCAL, a, b, c, 0, 0);
	case SYS_getpeername: return nova(NOVA_NET_ADDRESS, NOVA_ADDRESS_PEER, a, b, c, 0, 0);

	/* ---- signals ---- */
	case SYS_rt_sigaction: return nova(NOVA_SIG_ACTION, a, b, c, d, 0, 0);
	case SYS_rt_sigprocmask: return nova(NOVA_SIG_MASK, a, b, c, d, 0, 0);
	case SYS_rt_sigreturn: return nova(NOVA_SIG_RETURN, 0, 0, 0, 0, 0, 0);
	case SYS_sigaltstack: return nova(NOVA_SIG_ALTSTACK, a, b, 0, 0, 0, 0);
	case SYS_kill: return nova(NOVA_SIG_SEND, NOVA_SEND_PROCESS, a, b, 0, 0, 0);
	case SYS_tkill: return nova(NOVA_SIG_SEND, NOVA_SEND_THREAD, a, b, 0, 0, 0);
	case SYS_tgkill: return nova(NOVA_SIG_SEND, NOVA_SEND_GROUP_THREAD, a, b, c, 0, 0);
	case SYS_rt_sigtimedwait: return nova(NOVA_SIG_WAIT, NOVA_SIG_TIMED, a, b, c, d, 0);
	case SYS_rt_sigsuspend: return nova(NOVA_SIG_WAIT, NOVA_SIG_SUSPEND, a, b, 0, 0, 0);
	case SYS_pause: return nova(NOVA_SIG_WAIT, NOVA_SIG_PAUSE, 0, 0, 0, 0, 0);
	case SYS_rt_sigpending: return nova(NOVA_SIG_WAIT, NOVA_SIG_PENDING, a, b, 0, 0, 0);

	/* ---- the rest ---- */
	case SYS_getrandom: return nova(NOVA_RANDOM, a, b, c, 0, 0, 0);
	}
	return -ENOSYS;
}

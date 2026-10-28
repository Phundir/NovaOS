//! The POSIX objects of the kernel: processes with descriptors, threads,
//! files, pipes, sockets, memory mappings, signals -- what a program
//! written for POSIX needs of a kernel, as Nova's own objects.
//!
//! ## Two doors, one implementation
//!
//! A **ported program** (spec 6.7) -- the Web Browser first among them --
//! is linked against the Nova C library and calls the Nova domains 09
//! to 0B and their extensions (`syscall::ported`). A **Linux program**
//! run unchanged goes through the Linux personality (`crate::linux`),
//! whose only job is to translate Linux call numbers. Both land here:
//! this module does not know Linux numbering, and nothing of a ported
//! program passes through `crate::linux`.
//!
//! These objects were first written for the Linux subsystem of phase
//! 11.5 and lived under `src/linux/`, which put every call of the native
//! browser through code filed as Linux. They are the kernel's own; the
//! Linux personality is the translation above them (DECISIONS, "Le
//! navigateur devient natif, par une libc Nova").
//!
//! * a process owns a **private address space** (`vm::Space`, 11.3), in
//!   which the ELF and its interpreter are mapped by region, paged in on
//!   first touch from the files of its tree;
//! * a process is a **thread group**: each thread is a Nova task sharing
//!   the space, the descriptor table and the signal handlers;
//! * the files come from their own tree (`vfs`), whose root is a folder
//!   of the Nova volume (`vfs::novafs`);
//! * every call returns `-errno`, musl's numbering (spec 6.7).
//!
//! ## Identifiers
//!
//! A thread's TID is its Nova PID; a process's PID (TGID) is the TID of
//! its first thread. The two numbering spaces are the same, so a
//! `/proc/<pid>` and a Nova task list agree.

pub mod errno;
pub mod fd;
pub mod audio;
pub mod gui;
pub mod files;
pub mod loader;
pub mod misc;
pub mod mm;
pub mod net;
pub mod pipe;
pub mod poll;
pub mod process;
pub mod selftest;
pub mod signal;
pub mod task;
pub mod time;
pub mod vfs;
pub mod wait;

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};

use crate::vga::SpinLock;
use crate::vm::Space;
use errno::*;
use fd::{Console, Description, FdTable};

/// Logs every call of every POSIX program with its result, in the
/// spirit of `strace`, whichever door it came through. Off by default.
pub static TRACE: AtomicBool = AtomicBool::new(false);

// ============================================================
//  Processes and threads
// ============================================================

pub struct Brk {
    pub start: u64,
    pub end: u64,
}

/// Which system interface a process speaks at its `syscall` instruction.
///
/// The objects behind it -- descriptors, files, sockets, threads, memory
/// regions, signals -- are the same for both (this module and its
/// neighbours); only the numbering and the calling conventions differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Abi {
    /// The Linux personality (spec 1: the compatibility layer), for
    /// programs built for Linux and run unchanged.
    Linux,
    /// The Nova API of ported programs (spec 6.7): a program linked
    /// against the Nova C library, started from a linked image (spec
    /// 3.5). No Linux call number is ever looked at for it.
    Nova,
}

pub struct Process {
    pub tgid: u64,
    /// The interface this process speaks, inherited by `fork`, `clone`
    /// and `execve`.
    pub abi: Abi,
    /// System calls made, for the line printed at its end.
    pub calls: AtomicU64,
    pub name: String,
    /// Absolute path of the executable, for `/proc/self/exe`.
    pub exe: String,
    /// `argv`, NUL-separated, for `/proc/self/cmdline`.
    pub cmdline: Vec<u8>,
    pub space: Arc<Space>,
    pub files: SpinLock<FdTable>,
    cwd: SpinLock<String>,
    pub umask: AtomicU32,
    pub signals: SpinLock<signal::Actions>,
    threads: SpinLock<Vec<u64>>,
    pub brk: SpinLock<Brk>,
    /// Set once by the first thread to call `exit_group` (or die).
    exiting: AtomicBool,
    exit_code: AtomicI64,
    /// Signal the process died of, 0 for a normal exit (`wait` status).
    killed_by: AtomicU32,
    /// Woken when the process has ended.
    pub ended: wait::WaitQueue,
    /// Signals sent to the process as a whole, not to one thread.
    pub pending: AtomicU64,
    /// PID of the Linux process that created it by `fork`, 0 for one
    /// started by Nova (`spawn`).
    pub parent: u64,
    /// A `vfork` child borrows its parent's space: the parent sleeps
    /// until the child calls `execve` or exits, which sets this.
    pub vfork_released: AtomicBool,
    /// The last memory calls (number, 4 arguments, result), printed when
    /// the process dies of a fault: the region it missed was usually
    /// shaped by one of them.
    pub mm_history: SpinLock<alloc::collections::VecDeque<(u64, [u64; 4], u64)>>,
}

pub struct Thread {
    pub tid: u64,
    pub process: Arc<Process>,
    /// `set_tid_address` / `CLONE_CHILD_CLEARTID`: zeroed and futex-woken
    /// at thread exit, which is how `pthread_join` learns of it.
    pub clear_child_tid: AtomicU64,
    pub sigmask: AtomicU64,
    pub pending: AtomicU64,
    /// Alternate signal stack: (base, size, flags).
    pub altstack: SpinLock<(u64, u64, u32)>,
    pub robust_list: AtomicU64,
    /// Set while the thread runs a signal handler on the alternate stack.
    pub on_altstack: AtomicBool,
    /// The last calls of this thread, (number, first four arguments,
    /// result): printed when it dies of a fault, beside the memory
    /// history of its process. Only the thread itself writes it, so its
    /// lock is never contended.
    pub recent: SpinLock<alloc::collections::VecDeque<(u64, [u64; 4], u64)>>,
}

/// Calls kept in `Thread::recent`.
pub const RECENT_CALLS: usize = 32;


impl Process {
    pub fn cwd(&self) -> String {
        self.cwd.lock().clone()
    }

    pub fn set_cwd(&self, path: String) {
        *self.cwd.lock() = path;
    }

    pub fn thread_ids(&self) -> Vec<u64> {
        self.threads.lock().clone()
    }

    pub fn is_exiting(&self) -> bool {
        self.exiting.load(Ordering::Acquire)
    }

    pub fn exit_code(&self) -> i64 {
        self.exit_code.load(Ordering::Acquire)
    }

    /// Status as `wait4` encodes it: code << 8, or the signal number.
    pub fn wait_status(&self) -> u32 {
        match self.killed_by.load(Ordering::Acquire) {
            0 => ((self.exit_code() as u32) & 0xFF) << 8,
            sig => sig & 0x7F,
        }
    }

    /// A process with everything a fresh one has; `fork`, `execve` and
    /// `spawn` differ only in what they pass.
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        tgid: u64,
        name: String,
        exe: String,
        cmdline: Vec<u8>,
        space: Arc<Space>,
        files: FdTable,
        cwd: String,
        signals: signal::Actions,
        brk: Brk,
        parent: u64,
        abi: Abi,
    ) -> Arc<Process> {
        Arc::new(Process {
            tgid,
            abi,
            calls: AtomicU64::new(0),
            name,
            exe,
            cmdline,
            space,
            files: SpinLock::new(files),
            cwd: SpinLock::new(cwd),
            umask: AtomicU32::new(0o022),
            signals: SpinLock::new(signals),
            threads: SpinLock::new(alloc::vec![tgid]),
            brk: SpinLock::new(brk),
            exiting: AtomicBool::new(false),
            exit_code: AtomicI64::new(0),
            killed_by: AtomicU32::new(0),
            ended: wait::WaitQueue::new(),
            pending: AtomicU64::new(0),
            parent,
            vfork_released: AtomicBool::new(false),
            mm_history: SpinLock::new(alloc::collections::VecDeque::new()),
        })
    }

    /// Descriptor table, working directory, umask and handlers, for the
    /// copy `fork` makes.
    pub fn snapshot(&self) -> (FdTable, String, u32, signal::Actions, Brk) {
        let files = self.files.lock().clone_table();
        let brk = self.brk.lock();
        (
            files,
            self.cwd(),
            self.umask.load(Ordering::Relaxed),
            self.signals.lock().clone(),
            Brk { start: brk.start, end: brk.end },
        )
    }

    /// Takes the descriptor table out (`execve` moves it to the new
    /// image, minus close-on-exec descriptors).
    pub fn take_files(&self) -> FdTable {
        core::mem::take(&mut *self.files.lock())
    }

    /// Descriptor number and what it points at, for `/proc/self/fd`.
    pub fn fd_descriptions(&self) -> Vec<(i64, String)> {
        let entries = self.files.lock().list();
        entries.into_iter().map(|(fd, e)| (fd, e.desc.file.describe())).collect()
    }

    /// `/proc/self/maps`, in the kernel's format.
    pub fn maps_text(&self) -> String {
        let mut out = String::new();
        for v in self.space.regions() {
            let (offset, inode) = match &v.backing {
                crate::vm::Backing::File { offset, shared, .. } => (*offset, if *shared { 's' } else { 'p' }),
                crate::vm::Backing::Anon => (0, 'p'),
            };
            let name = v.name.as_str();
            out.push_str(&alloc::format!(
                "{:012x}-{:012x} {}{}{}{} {:08x} 00:00 0 {}\n",
                v.start,
                v.end,
                if v.prot.readable() { 'r' } else { '-' },
                if v.prot.writable() { 'w' } else { '-' },
                if v.prot.executable() { 'x' } else { '-' },
                inode,
                offset,
                name
            ));
        }
        out
    }

    pub fn status_text(&self) -> String {
        let (_, resident, _) = self.space.stats();
        alloc::format!(
            "Name:\t{}\nState:\tR (running)\nTgid:\t{}\nPid:\t{}\nPPid:\t1\nUid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\n\
             VmRSS:\t{} kB\nThreads:\t{}\nSigQ:\t0/0\n",
            self.name,
            self.tgid,
            self.tgid,
            resident * 4,
            self.threads.lock().len()
        )
    }

    pub fn stat_text(&self) -> String {
        let (_, resident, _) = self.space.stats();
        alloc::format!(
            "{} ({}) R 1 {} {} 0 -1 0 0 0 0 0 0 0 0 0 20 0 {} 0 0 0 {} 0\n",
            self.tgid,
            self.name,
            self.tgid,
            self.tgid,
            self.threads.lock().len(),
            resident
        )
    }
}

// ============================================================
//  Registry
// ============================================================

static PROCESSES: SpinLock<BTreeMap<u64, Arc<Process>>> = SpinLock::new(BTreeMap::new());

/// Thread running on each core, as a raw `Arc` pointer: the syscall path
/// finds it without a lock. Valid while that thread runs (its task holds
/// a reference).
static CURRENT: [AtomicU64; crate::gdt::MAX_CPUS] = [const { AtomicU64::new(0) }; crate::gdt::MAX_CPUS];

/// Called by the scheduler on every switch, with the incoming task's
/// thread (or none).
pub fn activate(thread: Option<&Arc<Thread>>) {
    let cpu = crate::smp::current_cpu_id().min(crate::gdt::MAX_CPUS - 1);
    let ptr = thread.map_or(0, |t| Arc::as_ptr(t) as u64);
    CURRENT[cpu].store(ptr, Ordering::Release);
}

/// The Linux thread running on this core, if the current task is one.
pub fn current_thread() -> Option<Arc<Thread>> {
    let cpu = crate::smp::current_cpu_id().min(crate::gdt::MAX_CPUS - 1);
    let ptr = CURRENT[cpu].load(Ordering::Acquire) as *const Thread;
    if ptr.is_null() {
        return None;
    }
    // SAFETY: the pointer comes from an Arc the running task holds; one
    // more strong reference for the caller.
    unsafe {
        Arc::increment_strong_count(ptr);
        Some(Arc::from_raw(ptr))
    }
}

pub fn current_tgid() -> Option<u64> {
    current_thread().map(|t| t.process.tgid)
}

pub fn thread_by_tid(tid: u64) -> Option<Arc<Thread>> {
    crate::scheduler::linux_of(tid)
}

pub fn process(tgid: u64) -> Option<Arc<Process>> {
    PROCESSES.lock().get(&tgid).cloned()
}

pub fn tgids() -> Vec<u64> {
    PROCESSES.lock().keys().copied().collect()
}

pub fn process_count() -> usize {
    PROCESSES.lock().len()
}

// ============================================================
//  Console
// ============================================================

/// Output of the standard descriptors: to the serial console, bytes
/// outside printable ASCII replaced, as for every line the kernel prints.
pub fn console_write(buf: &[u8]) {
    let mut line = String::with_capacity(buf.len());
    for &b in buf {
        line.push(match b {
            b'\n' | b'\t' => b as char,
            0x20..=0x7E => b as char,
            _ => '?',
        });
    }
    crate::serial_print!("{}", line);
    CONSOLE_BYTES.fetch_add(buf.len() as u64, Ordering::Relaxed);
}

static CONSOLE_BYTES: AtomicU64 = AtomicU64::new(0);

/// A write on a pipe or socket with no reader: SIGPIPE to the writer,
/// whose default action ends the process (unless ignored, the usual
/// choice of network programs).
pub fn raise_sigpipe() {
    if let Some(t) = current_thread() {
        let _ = signal::send_to_thread(t.tid, signal::SIGPIPE);
    }
}

pub fn console_bytes() -> u64 {
    CONSOLE_BYTES.load(Ordering::Relaxed)
}

// ============================================================
//  Starting a program
// ============================================================

/// Starts `path` with `argv` and `envp`, as a new process speaking `abi`,
/// as the scheduler type `kind` (a linked image runs as the type its
/// header declares). Returns its PID once its first thread is ready to
/// run.
pub fn spawn_with(path: &str, argv: &[&str], envp: &[&str], abi: Abi, kind: crate::process::BinaryType) -> LxResult<u64> {
    let space = Space::new().ok_or(ENOMEM)?;
    let image = loader::load(&space, path, argv, envp, abi)?;
    // A linked image runs as the type its header declares (spec 3.1).
    let kind = image.format.map(|f| f.scheduler_type()).unwrap_or(kind);
    let name = path.rsplit('/').next().unwrap_or(path).to_string();
    let mut cmdline = Vec::new();
    for a in argv {
        cmdline.extend_from_slice(a.as_bytes());
        cmdline.push(0);
    }

    let mut files = FdTable::new();
    for _ in 0..3 {
        files.insert(Description::new(Arc::new(Console), fd::O_RDWR), false)?;
    }

    // The first thread's task is created dormant: its TID is the PID,
    // known only once the task exists.
    let exe = image.exe.clone();
    let space_for_process = space.clone();
    let brk_start = image.brk;
    let tid = crate::vm::spawn_user_with(&name.clone(), kind, space, image.regs, 0, move |tid| {
        let process = Process::create(
            tid,
            name,
            exe,
            cmdline,
            space_for_process,
            files,
            String::from("/"),
            signal::Actions::new(),
            Brk { start: brk_start, end: brk_start },
            0,
            abi,
        );
        let thread = Arc::new(Thread::new(tid, process.clone()));
        PROCESSES.lock().insert(tid, process.clone());
        crate::scheduler::set_linux(tid, Some(thread));
    });
    STARTED.fetch_add(1, Ordering::Relaxed);
    Ok(tid)
}

static STARTED: AtomicU64 = AtomicU64::new(0);

impl Thread {
    pub fn new(tid: u64, process: Arc<Process>) -> Thread {
        Thread {
            tid,
            process,
            clear_child_tid: AtomicU64::new(0),
            sigmask: AtomicU64::new(0),
            pending: AtomicU64::new(0),
            altstack: SpinLock::new((0, 0, 2)),
            robust_list: AtomicU64::new(0),
            on_altstack: AtomicBool::new(false),
            recent: SpinLock::new(alloc::collections::VecDeque::with_capacity(RECENT_CALLS)),
        }
    }

    /// Notes a call just served (`syscall::ported`, `syscall`).
    pub fn note_call(&self, number: u64, args: &[u64; 6], result: u64) {
        let mut recent = self.recent.lock();
        if recent.len() == RECENT_CALLS {
            recent.pop_front();
        }
        recent.push_back((number, [args[0], args[1], args[2], args[3]], result));
    }
}

/// Adds a thread created by `clone` to its process.
pub fn register_thread(process: &Arc<Process>, tid: u64) {
    process.threads.lock().push(tid);
}

/// Waits for the end of a process and returns its exit status in the
/// shell's convention: the code, or 128 + signal.
pub fn wait_exit(tgid: u64, timeout_ticks: Option<u64>) -> Option<i64> {
    let process = process(tgid)?;
    // Deadlines of the Linux waits count core 0's milliseconds.
    let per_tick = (crate::FINE_HZ / crate::TIMER_HZ) as u64;
    let deadline = timeout_ticks.map(|t| crate::idt::fine_ticks().saturating_add(t.saturating_mul(per_tick)));
    let finished = wait::wait_until(&[&process.ended], deadline, || {
        (process.is_exiting() && process.threads.lock().is_empty()).then_some(())
    });
    match finished {
        Ok(()) => {
            PROCESSES.lock().remove(&tgid);
            Some(process.exit_code())
        }
        Err(_) => None,
    }
}

// ============================================================
//  Ending threads and processes
// ============================================================

/// Ends the calling thread (`exit`). The last thread out ends the process.
pub fn exit_thread(thread: &Arc<Thread>, code: i64) -> ! {
    // pthread_join waits on this word: zero it and wake the joiner.
    let ctid = thread.clear_child_tid.swap(0, Ordering::AcqRel);
    if ctid != 0 {
        let _ = crate::vm::write_user(ctid, &0u32);
        task::futex_wake(&thread.process.space, ctid, 1, u32::MAX);
    }
    let process = thread.process.clone();
    let last = {
        let mut threads = process.threads.lock();
        threads.retain(|t| *t != thread.tid);
        threads.is_empty()
    };
    if last {
        finish_process(&process, code);
    }
    reap_later(thread.tid);
    crate::scheduler::exit_current_with(code)
}

/// Ends every thread of the process (`exit_group`).
pub fn exit_group(thread: &Arc<Thread>, code: i64) -> ! {
    let process = thread.process.clone();
    if !process.exiting.swap(true, Ordering::AcqRel) {
        process.exit_code.store(code, Ordering::Release);
    }
    kill_other_threads(&process, thread.tid, code);
    exit_thread(thread, code)
}

/// The process dies of `sig` (a fatal signal, a fault): same as
/// `exit_group`, with the status a parent's `wait` reads as "killed".
pub fn exit_group_signal(thread: &Arc<Thread>, sig: u32) -> ! {
    let process = thread.process.clone();
    if !process.exiting.swap(true, Ordering::AcqRel) {
        process.exit_code.store(128 + sig as i64, Ordering::Release);
        process.killed_by.store(sig, Ordering::Release);
    }
    kill_other_threads(&process, thread.tid, 128 + sig as i64);
    exit_thread(thread, 128 + sig as i64)
}

/// Ends every thread of `process` but `keep`.
pub fn kill_other_threads(process: &Arc<Process>, keep: u64, code: i64) {
    let others: Vec<u64> = process.threads.lock().iter().copied().filter(|t| *t != keep).collect();
    for tid in others {
        crate::scheduler::kill(tid, code);
        process.threads.lock().retain(|t| *t != tid);
        reap_later(tid);
    }
}

/// Prints the memory history of the current ported process (fatal fault).
pub fn dump_mm_history() {
    let Some(t) = current_thread() else { return };
    let history = t.process.mm_history.lock().clone();
    // The history holds the numbers of the process's own interface.
    let (tag, name): (&str, fn(u64) -> &'static str) = match t.process.abi {
        Abi::Linux => ("linux", crate::linux::syscall::name),
        Abi::Nova => ("natif", crate::syscall::ported::name),
    };
    crate::serial_println!("[{}] processus {} : {} derniers appels memoire", tag, t.process.tgid, history.len());
    for (n, a, r) in history {
        crate::serial_println!("[{}]   {}({:#x}, {:#x}, {:#x}, {:#x}) = {:#x}", tag, name(n), a[0], a[1], a[2], a[3], r);
    }
    let recent = t.recent.lock().clone();
    crate::serial_println!("[{}] thread {} : {} derniers appels", tag, t.tid, recent.len());
    for (n, a, r) in recent {
        crate::serial_println!("[{}]   {}({:#x}, {:#x}, {:#x}, {:#x}) = {:#x}", tag, name(n), a[0], a[1], a[2], a[3], r);
    }
}

/// Kills a whole process from outside it (`kill` with a fatal signal).
/// Its threads may be anywhere -- blocked, computing, on other cores: a
/// fatal signal cannot wait for them to reach a system call.
pub fn terminate(process: &Arc<Process>, sig: u32) {
    if process.exiting.swap(true, Ordering::AcqRel) {
        return;
    }
    process.exit_code.store(128 + sig as i64, Ordering::Release);
    process.killed_by.store(sig, Ordering::Release);
    kill_other_threads(process, 0, 128 + sig as i64);
    finish_process(process, 128 + sig as i64);
}

fn finish_process(process: &Arc<Process>, code: i64) {
    if !process.exiting.swap(true, Ordering::AcqRel) {
        process.exit_code.store(code, Ordering::Release);
    }
    // Descriptors closed now, not when the last Arc goes: a pipe's other
    // end must see the end of file as soon as the writer is gone.
    let entries = {
        let mut files = process.files.lock();
        let list = files.list();
        for (fd, _) in &list {
            let _ = files.remove(*fd);
        }
        list
    };
    drop(entries);
    match process.abi {
        Abi::Linux => crate::serial_println!("[linux] processus {} ({}) termine - code {}", process.tgid, process.name, process.exit_code()),
        // A ported program never reaches the Linux dispatcher: its calls
        // all went through the Nova domains (`syscall::ported`).
        Abi::Nova => crate::serial_println!(
            "[natif] processus {} ({}) termine - code {} - {} appels Nova, aucun appel Linux",
            process.tgid,
            process.name,
            process.exit_code(),
            process.calls.load(Ordering::Relaxed)
        ),
    }
    process.vfork_released.store(true, Ordering::Release);
    process.ended.notify_all();
    process::child_ended(process);
}

// ============================================================
//  Reaping
// ============================================================

/// Dead threads whose tasks still hold a kernel stack and a reference to
/// the space. Reaped by a kernel task, as soon as no core names them.
static TO_REAP: SpinLock<Vec<u64>> = SpinLock::new(Vec::new());
static REAPER_WAKE: wait::WaitQueue = wait::WaitQueue::new();

fn reap_later(tid: u64) {
    TO_REAP.lock().push(tid);
    REAPER_WAKE.notify_all();
}

fn reaper() -> ! {
    loop {
        let pending: Vec<u64> = TO_REAP.lock().clone();
        let mut done = Vec::new();
        for tid in pending {
            if crate::scheduler::try_reap(tid).is_some() || !crate::scheduler::exists(tid) {
                done.push(tid);
            }
        }
        {
            let mut list = TO_REAP.lock();
            list.retain(|t| !done.contains(t));
        }
        let more = !TO_REAP.lock().is_empty();
        // Something still running: look again next tick. Otherwise sleep
        // until a thread dies.
        let deadline = more.then(|| crate::idt::fine_ticks() + (crate::FINE_HZ / crate::TIMER_HZ) as u64);
        let _ = wait::wait_until(&[&REAPER_WAKE], deadline, || (!more && !TO_REAP.lock().is_empty()).then_some(()));
    }
}

// ============================================================
//  Initialisation
// ============================================================

/// Assembles the file tree and starts the reaper. Called once, by
/// `kernel_main`, after the Nova filesystem and before any POSIX program.
pub fn init() {
    time::init();
    vfs::init();
    crate::scheduler::spawn("posix-reaper", crate::process::BinaryType::Dsa, reaper);
    let mounts = vfs::mounts();
    crate::serial_println!(
        "[posix] objets POSIX prets - {} points de montage, horloge TSC a {} kHz",
        mounts.len(),
        time::tsc_per_ms()
    );
}

/// A test image that opens the browser at login (`/nova/open-browser-at-login`,
/// `tools/ports/test-window.sh`) opens it here, as the menu does, and
/// waits for it (at most `timeout_ticks`). Returns its status, or None
/// when the image asks for nothing or has no browser.
pub fn autostart_browser(timeout_ticks: u64) -> Option<i64> {
    vfs::walk("/", "/nova/open-browser-at-login", true).ok()?;
    vfs::walk("/", BROWSER_IMAGE, true).ok()?;
    let pid = launch_web_browser()?;
    let status = wait_exit(pid, Some(timeout_ticks));
    crate::serial_println!("[natif] Web Browser : fin, statut {:?}", status);
    Some(status.unwrap_or(-1))
}

/// Starts the Web Browser in a Nova window, if the root image has it.
/// Monotonic instant of the last browser launch, for the startup
/// measurements the window prints (`gui`).
pub static BROWSER_LAUNCH_NS: AtomicU64 = AtomicU64::new(0);

/// Milliseconds since the last browser launch.
pub fn ms_since_browser_launch() -> u64 {
    (time::monotonic_ns().saturating_sub(BROWSER_LAUNCH_NS.load(Ordering::Relaxed))) / 1_000_000
}

/// Gives `/home/nova` to the session's own space in the Nova tree, so
/// that what the browser writes there -- its profile, its bookmarks, its
/// history -- is on the disk and not in memory. Without a session, or
/// without a filesystem, `/home` stays the tmpfs of `vfs::init`.
pub(crate) fn mount_home() {
    let Some(user) = crate::gui::session_user() else {
        return;
    };
    let home = alloc::format!("/users/{}/apps/web-browser", user);
    let who = crate::fs::perm::Credentials::kernel();
    let now = crate::idt::ticks();
    if crate::fs::stat(&home, who).is_err()
        && crate::fs::create_directory(&home, who, crate::fs::perm::Mode::private_dir(), now).is_err()
    {
        return;
    }
    if let Some(node) = vfs::novafs::mount_at(&home) {
        vfs::mount("/home/nova", node, "novafs");
        crate::serial_println!("[posix] /home/nova sur {} - le profil survit a l'extinction", home);
    }
}

/// The Web Browser as a Nova program (spec 3.5, 6.7): a linked image,
/// run through the Nova C library, no Linux call anywhere.
pub const BROWSER_IMAGE: &str = "/usr/lib/web-browser/web-browser.cus";
/// The profile every session starts from, prepared with the image.
const PREPARED_PROFILE: &str = "/usr/lib/web-browser/nova/profile";

/// Gives a ported program the Nova C library (spec 6.7): `/sys/runtimes`
/// of its view is the Nova tree's own, where the library lives for every
/// program. Returns whether it is there.
fn mount_runtimes() -> bool {
    match vfs::novafs::mount_at("/sys/runtimes") {
        Some(node) => {
            vfs::mount("/sys/runtimes", node, "novafs");
            true
        }
        None => false,
    }
}

/// Starts `path` as a program of the Nova interface: a linked image (spec
/// 3.5), or an ELF linked against the Nova C library.
pub fn spawn_native(path: &str, argv: &[&str], envp: &[&str]) -> LxResult<u64> {
    if !mount_runtimes() {
        crate::serial_println!("[natif] /sys/runtimes absent : pas de bibliotheque C Nova");
        return Err(ENOENT);
    }
    spawn_with(path, argv, envp, Abi::Nova, crate::process::BinaryType::Cus)
}

/// Copies the tree under `from` into the folder `into`, in the program
/// view: what `cp -a` did in the old shell launcher, done by the kernel
/// that launches a native program.
fn copy_tree(from: &Arc<dyn vfs::Inode>, into: &Arc<dyn vfs::Inode>) -> LxResult<()> {
    for entry in from.readdir()? {
        if entry.name == "." || entry.name == ".." {
            continue;
        }
        let child = from.lookup(&entry.name)?;
        match child.stat().kind() {
            vfs::Kind::Dir => {
                let made = match into.lookup(&entry.name) {
                    Ok(existing) => existing,
                    Err(_) => into.create(&entry.name, vfs::Kind::Dir, 0o755, "")?,
                };
                copy_tree(&child, &made)?;
            }
            vfs::Kind::Symlink => {
                let _ = into.create(&entry.name, vfs::Kind::Symlink, 0o777, &child.readlink()?);
            }
            vfs::Kind::File => {
                let made = into.create(&entry.name, vfs::Kind::File, 0o644, "")?;
                let mut buf = files::user_buffer(64 * 1024)?;
                let mut at = 0u64;
                loop {
                    let n = child.read_at(at, &mut buf)?;
                    if n == 0 {
                        break;
                    }
                    made.write_at(at, &buf[..n])?;
                    at += n as u64;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// First launch of a session: the prepared profile becomes the session's.
fn prepare_profile() {
    if vfs::walk("/", "/home/nova/profile", true).is_ok() {
        return;
    }
    let (Ok(prepared), Ok(home)) = (vfs::walk("/", PREPARED_PROFILE, true), vfs::walk("/", "/home/nova", true)) else {
        return;
    };
    let result = home
        .inode
        .create("profile", vfs::Kind::Dir, 0o700, "")
        .and_then(|profile| copy_tree(&prepared.inode, &profile));
    match result {
        Ok(()) => crate::serial_println!("[natif] profil du navigateur prepare dans /home/nova/profile"),
        Err(e) => crate::serial_println!("[natif] profil du navigateur : copie interrompue, errno {}", e),
    }
}

fn launch_native_browser() -> Option<u64> {
    prepare_profile();
    let argv = ["web-browser", "--no-remote", "--profile", "/home/nova/profile"];
    let envp = [
        "PATH=/usr/local/bin:/usr/bin:/bin",
        "HOME=/home/nova",
        "LANG=fr_FR.UTF-8",
        "TERM=dumb",
        "MOZ_HEADLESS=1",
        "NOVA_DISPLAY=1",
        "MOZ_CRASHREPORTER_DISABLE=1",
        "MOZ_DISABLE_CONTENT_SANDBOX=1",
        "MOZ_DISABLE_GMP_SANDBOX=1",
        "MOZ_DISABLE_RDD_SANDBOX=1",
        "MOZ_DISABLE_SOCKET_PROCESS_SANDBOX=1",
        "MOZ_DISABLE_UTILITY_SANDBOX=1",
    ];
    match spawn_native(BROWSER_IMAGE, &argv, &envp) {
        Ok(pid) => {
            crate::serial_println!("[natif] Web Browser lance - {} (processus {})", BROWSER_IMAGE, pid);
            Some(pid)
        }
        Err(e) => {
            crate::serial_println!("[natif] Web Browser : lancement refuse, errno {}", e);
            None
        }
    }
}

/// Starts the Web Browser: the linked image, a Nova program. `None` when
/// the image is not there -- the menu then opens the native browser
/// (`apps::browser`). There is no fallback through the Linux
/// personality: the browser is a Nova program or it is not started.
pub fn launch_web_browser() -> Option<u64> {
    BROWSER_LAUNCH_NS.store(time::monotonic_ns(), Ordering::Relaxed);
    mount_home();
    if vfs::walk("/", BROWSER_IMAGE, true).is_err() {
        crate::serial_println!("[natif] pas d'image liee du navigateur ({})", BROWSER_IMAGE);
        return None;
    }
    launch_native_browser()
}

pub fn started_count() -> u64 {
    STARTED.load(Ordering::Relaxed)
}

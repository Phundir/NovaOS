//! Phase 10.4 -- Compatibility subsystems (spec 18, long term).
//!
//! | Module | Covers |
//! |---|---|
//! | `elf` | ELF64 parsing |
//! | `linux` | 10.4.1 -- image conversion, call translation, paths |
//! | `pe` | PE32+ parsing |
//! | `windows` | 10.4.2 -- image conversion, NT translation, paths |
//!
//! ## A foreign program is a Nova program whose contents came from
//! elsewhere
//!
//! There is no second loader, no second scheduler and no second arena. A
//! foreign executable is **converted** into a Nova image -- the loadable
//! parts of the ELF or PE become `.text`, `.data` and `.bss` -- and then
//! goes through `loader::load` like anything else: same header, same
//! signature check, same arena, same `verify_protection`, same sandbox of
//! 12.4.
//!
//! That is the design decision of this phase, and everything else follows
//! from it. A separate path would mean a second place where an image is
//! mapped, a second place where W^X is enforced, and a second place to
//! forget to enforce it.
//!
//! ## What the personality is for
//!
//! Two programs, identical to the loader, do not mean the same thing by
//! `syscall`. Linux `write` is 1; Nova's 1 is `SYS_DEBUG_PRINT`. So the
//! task carries a **personality**, and `syscall_dispatch` looks it up
//! before it looks anything up in the Nova table. The lookup is a probe
//! on a small table and returns `None` for every task the subsystems did
//! not load, which is nearly all of them.
//!
//! ## What does not run, and why saying so matters
//!
//! Neither subsystem runs the binaries one actually finds on the two
//! systems:
//!
//! * a Linux distribution binary is **dynamically linked** -- it wants
//!   `ld.so` and a libc, neither of which exists here;
//! * a Windows program imports **kernel32.dll**, which does not exist
//!   either, and whose absence is visible in the import directory before
//!   a single instruction runs.
//!
//! Both are refused **by name**, with the missing piece written out. That
//! is the posture phase 9 took with the seven languages of 13.2, and it
//! is the only one that does not mislead: a subsystem that loaded such a
//! binary and crashed on the first call would look like a bug in the
//! program.
//!
//! What does run is the class of **freestanding** executables: a static,
//! position-independent ELF64 that calls the kernel directly, and a PE32+
//! with no system imports that does the same. That class is small and it
//! is real -- it is what the self-test builds, with the assembler of
//! phase 9, and runs in ring 3.

pub mod elf;
pub mod linux;
pub mod pe;
pub mod selftest;
pub mod windows;

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::process::Pid;
use crate::syscall::SyscallFrame;
use crate::vga::SpinLock;

use linux::Descriptor;

/// Which ABI a task speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Personality {
    Linux,
    Windows,
}

impl Personality {
    pub fn as_str(&self) -> &'static str {
        match self {
            Personality::Linux => "linux",
            Personality::Windows => "windows",
        }
    }
}

/// Everything a foreign task carries beyond what a Nova task has.
pub struct TaskState {
    pub pid: Pid,
    pub personality: Personality,
    /// Identifier under which the image was installed -- it is what
    /// decides the sandbox root of 12.4 and the path translation.
    pub app_id: String,
    pub descriptors: Vec<Descriptor>,
    next_fd: i64,
    pub calls: u64,
    pub errors: u64,
    pub refused: u64,
}

impl TaskState {
    fn new(pid: Pid, personality: Personality, app_id: &str) -> Self {
        TaskState {
            pid,
            personality,
            app_id: String::from(app_id),
            descriptors: Vec::new(),
            next_fd: linux::FD_FIRST,
            calls: 0,
            errors: 0,
            refused: 0,
        }
    }

    /// Index of a descriptor, by number.
    pub fn descriptor(&self, fd: i64) -> Option<usize> {
        self.descriptors.iter().position(|entry| entry.fd == fd)
    }

    /// Opens a descriptor on a Nova path. An empty path is the null
    /// device: reads give nothing, writes go nowhere.
    pub fn open(&mut self, path: String, writable: bool) -> Option<i64> {
        if self.descriptors.len() >= linux::MAX_DESCRIPTORS {
            return None;
        }
        let fd = self.next_fd;
        self.next_fd += 1;
        self.descriptors.push(Descriptor {
            fd,
            path,
            offset: 0,
            writable,
        });
        Some(fd)
    }

    /// Closes a descriptor. Returns whether it existed.
    ///
    /// The number is **not** reused: a program that closed a descriptor
    /// and kept using it would silently reach whatever was opened next,
    /// and the bug would appear far from its cause.
    pub fn close(&mut self, fd: i64) -> bool {
        match self.descriptor(fd) {
            Some(index) => {
                self.descriptors.remove(index);
                true
            }
            None => false,
        }
    }
}

/// Foreign tasks currently alive.
///
/// A `Vec` under a lock rather than a fixed table: foreign tasks are rare
/// -- there are none at all on an ordinary boot -- and reserving thirty-two
/// slots with a descriptor table each would cost memory nobody uses.
static TASKS: SpinLock<Vec<TaskState>> = SpinLock::new(Vec::new());

static LOADED: AtomicU64 = AtomicU64::new(0);

/// Personalities granted, by PID, kept after the task's death: a short
/// foreign program may finish before its launcher looks at it, and since
/// ring 3 is preempted for real (11.3) that happens.
static GRANTED: SpinLock<Vec<(Pid, Personality)>> = SpinLock::new(Vec::new());

/// The personality `pid` was started with, alive or not.
pub fn granted_personality(pid: Pid) -> Option<Personality> {
    GRANTED.lock().iter().find(|(p, _)| *p == pid).map(|(_, k)| *k)
}
static REFUSED: AtomicU64 = AtomicU64::new(0);
static CALLS: AtomicU64 = AtomicU64::new(0);

/// Which ABI this task speaks, if it is not a Nova one.
///
/// Called on **every** system call of the machine, so it must be cheap
/// when the answer is no: the common case is a lock, a linear scan of an
/// empty vector, and a return.
pub fn personality_of(pid: Pid) -> Option<Personality> {
    let tasks = TASKS.lock();
    if tasks.is_empty() {
        return None;
    }
    tasks
        .iter()
        .find(|task| task.pid == pid)
        .map(|task| task.personality)
}

/// Routes one foreign system call.
///
/// The state is taken **out** of the table for the duration of the call
/// and put back afterwards, rather than held under the lock. A call can
/// block -- `nanosleep`, a read that waits on the disk -- and holding a
/// lock across a task switch is what `DECISIONS.md` forbids in the
/// clearest terms it has.
pub fn dispatch(personality: Personality, frame: &mut SyscallFrame) -> u64 {
    let pid = crate::scheduler::current_pid();
    CALLS.fetch_add(1, Ordering::Relaxed);

    let Some(mut state) = take(pid) else {
        // The task died between the personality probe and here, or the
        // table was emptied. Returning an error is the only safe answer:
        // dispatching into the Nova table with a foreign call number
        // would do something, and it would be the wrong thing.
        return match personality {
            Personality::Linux => (-linux::errno::ENOSYS) as u64,
            Personality::Windows => windows::status::UNSUCCESSFUL,
        };
    };

    let result = match personality {
        Personality::Linux => linux::call(frame, &mut state),
        Personality::Windows => windows::call(frame, &mut state),
    };

    put_back(state);
    result
}

/// Removes the state of a task from the table and hands it over.
fn take(pid: Pid) -> Option<TaskState> {
    let mut tasks = TASKS.lock();
    let index = tasks.iter().position(|task| task.pid == pid)?;
    Some(tasks.remove(index))
}

/// Puts a state back after a call.
///
/// A task that exited during the call is gone from nowhere -- `forget`
/// was called before `exit_current_with`, and this never runs. That
/// ordering is why `exit` does not have to worry about the state coming
/// back to life.
fn put_back(state: TaskState) {
    TASKS.lock().push(state);
}

/// Drops the state of a task.
///
/// Called by the exit paths of both subsystems, and by the collector for
/// a task that died another way -- a fault, a kill. Without the second
/// case a dead PID would keep its personality, and a later task reusing
/// that PID would have its Nova calls translated as Linux ones.
pub fn forget(pid: Pid) {
    TASKS.lock().retain(|task| task.pid != pid);
}

/// Drops the state of every task that is no longer alive.
///
/// Returns how many went.
pub fn collect_dead() -> usize {
    let doomed: Vec<Pid> = {
        let tasks = TASKS.lock();
        tasks
            .iter()
            .filter(|task| crate::scheduler::task_info(task.pid).is_none())
            .map(|task| task.pid)
            .collect()
    };
    let count = doomed.len();
    for pid in doomed {
        forget(pid);
    }
    count
}

/// Number of foreign tasks alive.
pub fn live_count() -> usize {
    TASKS.lock().len()
}

/// Counters: (images loaded, images refused, calls translated).
pub fn counters() -> (u64, u64, u64) {
    (
        LOADED.load(Ordering::Relaxed),
        REFUSED.load(Ordering::Relaxed),
        CALLS.load(Ordering::Relaxed),
    )
}

// ============================================================
//  Identification
// ============================================================

/// What a byte slice looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Nova,
    Elf,
    Pe,
    Unknown,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Nova => "Nova",
            Kind::Elf => "ELF",
            Kind::Pe => "PE",
            Kind::Unknown => "inconnu",
        }
    }
}

/// Recognises a file by its first bytes.
///
/// Nova first: its five magic numbers are checked by `binfmt`, and a file
/// that is one of them is not a foreign binary however much its later
/// bytes might resemble one.
pub fn identify(data: &[u8]) -> Kind {
    if data.len() >= 4
        && crate::binfmt::header::BinaryFormat::from_magic(&data[..4]).is_some()
    {
        return Kind::Nova;
    }
    if elf::is_elf(data) {
        return Kind::Elf;
    }
    if pe::is_pe(data) {
        return Kind::Pe;
    }
    Kind::Unknown
}

// ============================================================
//  Loading
// ============================================================

/// Why a foreign image was refused.
#[derive(Debug, Clone)]
pub enum CompatError {
    NotForeign,
    Linux(linux::LoadRefusal),
    Windows(windows::LoadRefusal),
    /// The conversion produced a Nova image the emitter refused.
    Emission(String),
    /// The Nova loader refused the converted image.
    Loader(String),
}

impl CompatError {
    pub fn message(&self) -> String {
        match self {
            CompatError::NotForeign => String::from("ni ELF ni PE"),
            CompatError::Linux(refusal) => refusal.message(),
            CompatError::Windows(refusal) => refusal.message(),
            CompatError::Emission(detail) => alloc::format!("emission refusee : {}", detail),
            CompatError::Loader(detail) => alloc::format!("chargement refuse : {}", detail),
        }
    }
}

/// Converts a foreign binary into a Nova image.
///
/// Returns the bytes of a complete `.cus`, which the ordinary loader
/// accepts. Exposed on its own so a caller can convert without running --
/// which is what `spm` would do to install a foreign program, and what
/// the self-test does to check the conversion before it risks a task.
pub fn convert(data: &[u8], app_id: &str) -> Result<Vec<u8>, CompatError> {
    use crate::binfmt::header::{BinaryFormat, SignatureType};
    use crate::binfmt::identity::Version;
    use crate::sdk::emit::{self, EmitSpec};

    let mut object = match identify(data) {
        Kind::Elf => {
            let parsed = elf::parse(data)
                .map_err(|error| CompatError::Linux(linux::LoadRefusal::Malformed(error)))?;
            let mut object = linux::to_object(&parsed).map_err(CompatError::Linux)?;
            linux::fill(&mut object, &parsed, data);
            object
        }
        Kind::Pe => {
            let parsed = pe::parse(data)
                .map_err(|error| CompatError::Windows(windows::LoadRefusal::Malformed(error)))?;
            let mut object = windows::to_object(&parsed).map_err(CompatError::Windows)?;
            windows::fill(&mut object, &parsed, data);
            object
        }
        _ => return Err(CompatError::NotForeign),
    };

    // The object has no symbols and no fixups: the addresses inside a
    // foreign image are the foreign image's business, and the refusals
    // above are what guarantee it has none that need fixing.
    object.symbols.clear();
    object.fixups.clear();

    // A `.cus` and not a `.dsa`: 12.4 gives `.cus` the narrowest sandbox
    // that still allows a program to do something, and a foreign binary
    // is the last thing that should be given system trust. Self-signed,
    // for the same reason -- there is no authority for it.
    emit::emit(
        &object,
        &EmitSpec {
            format: BinaryFormat::Cus,
            signature: SignatureType::SelfSigned,
            app_id,
            author: "compat",
            group: "os.nova.compat",
            display_name: app_id,
            version: Version::new(1, 0, 0, 0),
            permissions: 0,
            flags: 0,
            ports: &[],
            stack_size: 64 * 1024,
            authority: None,
        },
    )
    .map_err(|error| CompatError::Emission(error.message()))
}

/// Converts, loads and starts a foreign binary.
///
/// The task is registered with its personality **before** it is resumed,
/// which is the whole ordering constraint of this function: a task that
/// ran one instruction before its personality existed would have that
/// instruction's system call dispatched through the Nova table.
pub fn load_and_spawn(data: &[u8], app_id: &str) -> Result<Pid, CompatError> {
    let personality = match identify(data) {
        Kind::Elf => Personality::Linux,
        Kind::Pe => Personality::Windows,
        _ => return Err(CompatError::NotForeign),
    };

    let nova = match convert(data, app_id) {
        Ok(bytes) => bytes,
        Err(error) => {
            REFUSED.fetch_add(1, Ordering::Relaxed);
            crate::serial_println!(
                "[compat] REFUS de \"{}\" ({}) : {}",
                app_id,
                personality.as_str(),
                error.message()
            );
            return Err(error);
        }
    };

    // The digest has to be in the trust store before the loader looks:
    // the image is self-signed, and `signature::verify` checks a
    // self-signed binary against its own digest. Same mechanism the GUI
    // modules and the phase 3 samples use.
    let (image, pid) = match crate::binfmt::loader::load_and_spawn(&nova) {
        Ok(pair) => pair,
        Err(error) => {
            REFUSED.fetch_add(1, Ordering::Relaxed);
            crate::serial_println!(
                "[compat] REFUS de \"{}\" au chargement : {}",
                app_id,
                error.message()
            );
            return Err(CompatError::Loader(String::from(error.message())));
        }
    };

    // Registered before anything runs. `spawn_dormant` inside the loader
    // means the task is not schedulable yet, which is what makes this
    // ordering achievable at all.
    TASKS
        .lock()
        .push(TaskState::new(pid, personality, app_id));
    {
        let mut granted = GRANTED.lock();
        if granted.len() == 32 {
            granted.remove(0);
        }
        granted.push((pid, personality));
    }

    // `LoadedImage` has no `Drop`: abandoning it leaks its frames, its
    // mappings and its arena slot in silence. `keep` is not optional --
    // it is an invariant of `DECISIONS.md`.
    crate::binfmt::loader::keep(pid, image);
    LOADED.fetch_add(1, Ordering::Relaxed);

    crate::serial_println!(
        "[compat] \"{}\" charge en {} - PID {}",
        app_id,
        personality.as_str(),
        pid
    );
    Ok(pid)
}

// ============================================================
//  State
// ============================================================

pub fn dump_state() {
    let (loaded, refused, calls) = counters();
    let full_linux = linux::CALLS
        .iter()
        .filter(|(_, _, level)| *level == linux::Support::Full)
        .count();
    let full_nt = windows::CALLS
        .iter()
        .filter(|(_, _, level)| *level == linux::Support::Full)
        .count();

    crate::serial_println!(
        "[compat] {} image(s) chargee(s), {} refusee(s), {} appel(s) traduit(s) - {} vivante(s)",
        loaded,
        refused,
        calls,
        live_count()
    );
    crate::serial_println!(
        "[compat] linux : {} appels sur {} traduits - windows : {} sur {}",
        full_linux,
        linux::CALLS.len(),
        full_nt,
        windows::CALLS.len()
    );
    for (_, name, level) in linux::CALLS
        .iter()
        .filter(|(_, _, level)| *level == linux::Support::Absent)
    {
        crate::serial_println!("[compat]   linux {} : {}", name, level.as_str());
    }
    for (_, name, level) in windows::CALLS
        .iter()
        .filter(|(_, _, level)| *level == linux::Support::Absent)
    {
        crate::serial_println!("[compat]   windows {} : {}", name, level.as_str());
    }
}

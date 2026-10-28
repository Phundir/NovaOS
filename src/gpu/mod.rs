//! The GPU core (spec 8.5): the part of the Nova graphics stack that does
//! not depend on the card.
//!
//! ## What it owns
//!
//! Between the programs (`SYS_GPU_*`, spec 6.8) and a backend (the built-in
//! Venus one, or a vendor's signed module), the core keeps everything that
//! must hold whatever the card:
//!
//! - **handles**: 32-bit names, unique for the whole boot, each tied to the
//!   process that created it. A handle of another process is unknown, not
//!   forbidden: nothing tells a program another's objects exist;
//! - **accounting**: the GPU memory a process holds is counted, and capped
//!   (`QUOTA`);
//! - **validation**: every command buffer is checked whole before the
//!   backend sees it (`cmd`), every shader before it is created (`spirv`);
//! - **submissions and fences**: each context numbers its submissions; a
//!   fence is "the submission numbered N of that context", signalled once
//!   the context has completed N. A submission waiting on another
//!   context's fence is held until that fence signals;
//! - **deferred destruction**: an object destroyed while submissions that
//!   may use it are in flight goes only once they complete. A program can
//!   destroy anything at any time without the GPU reading freed memory;
//! - **the watchdog**: a submission not completed within `WATCHDOG_MS`
//!   loses its context (`ENOTRECOVERABLE`), without touching the screen or
//!   other programs -- each context has its own device on the host;
//! - **cleanup**: the contexts of a process that died go, whatever the way
//!   it died, found by the GPU task's sweep.
//!
//! ## Locks
//!
//! The table of contexts is a `SpinLock`, held only to find a context. Each
//! context is behind a `SleepLock`: backend calls wait for the host, and
//! must be able to sleep. The completion counter of a context is atomic,
//! outside its lock, so that waiting for a fence never waits for a lock.
//! The GPU task only *tries* the context locks: a context busy creating a
//! pipeline is polled at the next turn, never waited for.

pub mod api;
pub mod backend;
pub mod cmd;
mod contexts;
pub mod lock;
pub mod module;
mod objects;
pub mod present;
pub mod screen;
pub mod selftest;
pub mod spirv;
pub mod venus;
mod work;

pub use contexts::{context_create, context_destroy, mem_alloc, mem_free, mem_map, mem_unmap};
pub use objects::{fence_create, fence_view, fence_wait, import, object_create, object_destroy, share, Create, FenceView, PipelineRequest};
pub use work::submit;
use contexts::{close_context, defer, run_deferred};
use work::{service_contexts, sweep};

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::posix::wait::WaitQueue;
use crate::process::Pid;
use crate::vga::SpinLock;
use backend::{Context as BackendContext, Device, ImageDesc, MemKind, ObjectKind, Work, WorkState};
use lock::SleepLock;

/// GPU memory a process may hold (spec 6.8, "plafonnée").
pub const QUOTA: u64 = 2 << 30;
/// Contexts, and objects, a process may hold.
const MAX_CONTEXTS: usize = 16;
const MAX_OBJECTS: usize = 16384;
/// Submissions in flight per context before `SUBMIT` waits.
const MAX_IN_FLIGHT: usize = 16;
/// The watchdog (spec 6.8): a submission not done by then loses its
/// context.
pub const WATCHDOG_MS: u64 = 2000;
/// How often the GPU task looks for the contexts of dead processes.
pub const SWEEP_MS: u64 = 200;

/// Why a call failed, before it becomes an errno.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// No GPU backend.
    Absent,
    /// A handle that is not the caller's, or not of the right kind.
    BadHandle,
    /// Malformed arguments, or a command buffer that failed validation.
    Invalid(&'static str),
    NoMemory,
    /// The quota or a count limit.
    Limit,
    Unsupported,
    /// The context is lost.
    Lost,
    TimedOut,
    Interrupted,
}

pub type Result<T> = core::result::Result<T, Error>;

impl From<backend::Error> for Error {
    fn from(error: backend::Error) -> Error {
        match error {
            backend::Error::Unsupported => Error::Unsupported,
            backend::Error::NoMemory => Error::NoMemory,
            backend::Error::Lost => Error::Lost,
            backend::Error::Device(reason) => Error::Invalid(reason),
        }
    }
}

// ============================================================
//  Owners
// ============================================================

/// Who holds GPU objects: a kernel task, or a POSIX process (all its
/// threads share them).
#[derive(Clone)]
pub enum Owner {
    Task(Pid),
    Process(u64, Weak<crate::posix::Process>),
}

impl Owner {
    /// The caller.
    pub fn current() -> Owner {
        match crate::posix::current_thread() {
            Some(thread) => Owner::Process(thread.process.tgid, Arc::downgrade(&thread.process)),
            None => Owner::Task(crate::scheduler::current_pid()),
        }
    }

    /// A key comparing owners: the two kinds never collide.
    pub fn key(&self) -> (u8, u64) {
        match self {
            Owner::Task(pid) => (0, *pid as u64),
            Owner::Process(tgid, _) => (1, *tgid),
        }
    }

    pub fn alive(&self) -> bool {
        match self {
            Owner::Task(pid) => crate::scheduler::exists(*pid) && !crate::scheduler::is_dying(*pid),
            Owner::Process(_, process) => process.upgrade().is_some_and(|p| !p.is_exiting()),
        }
    }

    /// The address space the owner's mappings live in.
    fn space(&self) -> Option<Arc<crate::vm::Space>> {
        match self {
            Owner::Task(pid) => crate::scheduler::space_of(*pid),
            Owner::Process(_, process) => process.upgrade().map(|p| p.space.clone()),
        }
    }
}

// ============================================================
//  The device
// ============================================================

/// The backend in use. Set once, at start.
static DEVICE: SpinLock<Option<Arc<dyn Device>>> = SpinLock::new(None);
static DESCRIPTION: SpinLock<Option<backend::Description>> = SpinLock::new(None);

/// Every change a waiter may be waiting for: a submission completed, a
/// context lost, a held submission released.
pub static EVENTS: WaitQueue = WaitQueue::new();

pub fn device() -> Option<Arc<dyn Device>> {
    DEVICE.lock().clone()
}

pub fn description() -> Option<backend::Description> {
    DESCRIPTION.lock().clone()
}

pub fn present() -> bool {
    DEVICE.lock().is_some()
}

// ============================================================
//  Contexts
// ============================================================

/// A GPU memory allocation of a program.
struct Memory {
    backend: backend::Id,
    size: u64,
    kind: MemKind,
    /// The pages lent to the program's address space, if mapped: the
    /// pager is kept to find the regions again, wherever they moved.
    user: Option<(Arc<dyn crate::vm::Pager>, u64)>,
    /// Kernel view, for a kernel owner.
    kernel: Option<u64>,
    window: Option<backend::Window>,
    /// Buffers and images placed in it: memory goes only once they have.
    users: u32,
}

/// A fence: submission `target` of context `ctx`.
struct Fence {
    target: u64,
}

/// What waits for submissions in flight to complete before going.
enum Deferred {
    Object(ObjectKind, backend::Id),
    Memory(backend::Id),
}

/// A submission held until the fences it waits on signal.
struct Held {
    serial: u64,
    commands: Vec<backend::Command>,
    waits: Vec<(Arc<Shared>, u64)>,
}

/// Where a live object's memory comes from, for the accounting.
struct ObjectExtra {
    kind: ObjectKind,
    /// Memory handle it lives in (0: memory of its own).
    memory: u32,
    /// Bytes of memory of its own (a dedicated image).
    own_bytes: u64,
}

/// Everything behind a context's lock.
struct Ctx {
    backend: Box<dyn BackendContext>,
    objects: BTreeMap<u32, cmd::Object>,
    extras: BTreeMap<u32, ObjectExtra>,
    memory: BTreeMap<u32, Memory>,
    fences: BTreeMap<u32, Fence>,
    /// `(serial, work, started at, in milliseconds of the TSC)`, oldest
    /// first.
    in_flight: VecDeque<(u64, Work, u64)>,
    held: VecDeque<Held>,
    deferred: Vec<(u64, Deferred)>,
    closed: bool,
    /// What `SYS_GPU_PRESENT` copies images through (`present`).
    present: Option<present::Staging>,
}

/// A context, as the table holds it.
pub struct Shared {
    pub handle: u32,
    pub owner: Owner,
    pub name: String,
    /// Last submission numbered, and last completed. Submissions complete
    /// in order: one queue per context.
    submitted: AtomicU64,
    completed: AtomicU64,
    lost: AtomicBool,
    state: SleepLock<Ctx>,
}

impl Shared {
    pub fn completed(&self) -> u64 {
        self.completed.load(Ordering::Acquire)
    }

    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    /// Has submission `serial` completed (or will it never)?
    pub fn reached(&self, serial: u64) -> bool {
        self.completed() >= serial || self.is_lost()
    }
}

/// What a handle names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HandleKind {
    Context,
    Memory,
    Object,
    Fence,
}

struct Tables {
    contexts: BTreeMap<u32, Arc<Shared>>,
    /// Every live handle: its owner, its kind and its context.
    handles: BTreeMap<u32, ((u8, u64), HandleKind, u32)>,
    /// Bytes of GPU memory per owner.
    usage: BTreeMap<(u8, u64), u64>,
    /// Images shared by `SYS_GPU_SHARE`: token -> (export name, description).
    shares: BTreeMap<u64, (u32, ImageDesc, u32)>,
}

static TABLES: SpinLock<Tables> = SpinLock::new(Tables {
    contexts: BTreeMap::new(),
    handles: BTreeMap::new(),
    usage: BTreeMap::new(),
    shares: BTreeMap::new(),
});

/// Handles are never reused within a boot: a stale handle can only be
/// unknown, never someone else's new object.
static NEXT_HANDLE: AtomicU32 = AtomicU32::new(1);

static SUBMISSIONS: AtomicU64 = AtomicU64::new(0);
static LOST_CONTEXTS: AtomicU64 = AtomicU64::new(0);
static REFUSED: AtomicU64 = AtomicU64::new(0);

fn new_handle(owner: &Owner, kind: HandleKind, context: u32) -> Result<u32> {
    let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    if handle == 0 || handle == u32::MAX {
        return Err(Error::Limit);
    }
    let mut tables = TABLES.lock();
    let key = owner.key();
    let held = tables.handles.values().filter(|(o, k, _)| *o == key && *k != HandleKind::Context).count();
    if held >= MAX_OBJECTS {
        return Err(Error::Limit);
    }
    tables.handles.insert(handle, (key, kind, context));
    Ok(handle)
}

fn drop_handle(handle: u32) {
    TABLES.lock().handles.remove(&handle);
}

/// The context `handle` names, if it is the caller's.
fn context_of(owner: &Owner, handle: u32) -> Result<Arc<Shared>> {
    let tables = TABLES.lock();
    match tables.handles.get(&handle) {
        Some((key, HandleKind::Context, _)) if *key == owner.key() => {
            tables.contexts.get(&handle).cloned().ok_or(Error::BadHandle)
        }
        _ => Err(Error::BadHandle),
    }
}

/// Checks that `handle` is the caller's, of `kind`, in `context`.
fn check_handle(owner: &Owner, handle: u32, kind: HandleKind, context: u32) -> Result<()> {
    match TABLES.lock().handles.get(&handle) {
        Some((key, k, c)) if *key == owner.key() && *k == kind && *c == context => Ok(()),
        _ => Err(Error::BadHandle),
    }
}

fn charge(owner: &Owner, bytes: u64) -> Result<()> {
    let mut tables = TABLES.lock();
    let used = tables.usage.entry(owner.key()).or_insert(0);
    if used.saturating_add(bytes) > QUOTA {
        return Err(Error::Limit);
    }
    *used += bytes;
    Ok(())
}

fn refund(owner: &Owner, bytes: u64) {
    let mut tables = TABLES.lock();
    if let Some(used) = tables.usage.get_mut(&owner.key()) {
        *used = used.saturating_sub(bytes);
    }
}

/// GPU memory an owner holds, in bytes.
pub fn usage_of(owner: &Owner) -> u64 {
    TABLES.lock().usage.get(&owner.key()).copied().unwrap_or(0)
}

// ============================================================
//  The GPU task
// ============================================================

/// Longest wait for the boot self-tests before the backend comes up anyway.
const BOOT_WAIT_MS: u64 = 120_000;

fn task() -> ! {
    let mut line = crate::drivers::irq::IrqLine::attach(crate::drivers::VECTOR_GPU);
    // The boot self-tests first (see `start`); the GPU's own self-test
    // waits for `READY`, so it is the one left running. Meanwhile the
    // device's interrupts are relayed: the screen's transfers need it.
    let deadline = crate::idt::fine_ticks() + BOOT_WAIT_MS;
    while crate::selftests_pending().0 > 1 && crate::idt::fine_ticks() < deadline {
        if let Some(line) = line.as_mut() {
            line.wait(20);
        } else {
            let _ = crate::syscall::invoke(crate::syscall::domain_time::SYS_TIME_SLEEP_MS, [20, 0, 0, 0, 0, 0]);
        }
        crate::graphics::virtio_gpu::context::service();
    }
    bring_up();
    let mut last_sweep = crate::idt::fine_ticks();
    loop {
        match line.as_mut() {
            Some(line) => {
                line.wait(10);
            }
            None => {
                let _ = crate::syscall::invoke(crate::syscall::domain_time::SYS_TIME_SLEEP_MS, [10, 0, 0, 0, 0, 0]);
            }
        }
        crate::graphics::virtio_gpu::context::service();
        if service_contexts() {
            EVENTS.notify_all();
        }
        let now = crate::idt::fine_ticks();
        if now.saturating_sub(last_sweep) >= SWEEP_MS {
            last_sweep = now;
            sweep();
        }
    }
}

static STARTED: AtomicBool = AtomicBool::new(false);
static READY: AtomicBool = AtomicBool::new(false);

/// Has `start` finished -- with a backend or without?
pub fn started() -> bool {
    READY.load(Ordering::Acquire)
}

/// Starts the GPU task, which brings the backend up.
///
/// Called by `init.dsa` with the drivers. The backend itself comes up in
/// the task, once the boot self-tests are done: opening the first Venus
/// session waits a tenth of a second for the host, and nothing needs the
/// GPU before the session opens -- while several self-tests count the
/// free frames of the whole machine around what they do, and a service
/// starting beside them shifted the others' allocations into their
/// window (DECISIONS, "Le GPU se met en route apres les autotests").
pub fn start() {
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    crate::scheduler::spawn("drv-gpu", crate::process::BinaryType::Dsa, task);
}

/// Brings the built-in backend up, if the machine has one, and loads the
/// vendors' modules.
fn bring_up() {
    let built_in = crate::graphics::virtio_gpu::probe().is_ok() && crate::graphics::virtio_gpu::context::available();
    // Vendors' modules (spec 3.6), loaded whatever happens: a module is
    // verified and logged even when the built-in backend serves the GPU,
    // and only used when it does not.
    let module = module::load_all(built_in);
    if !built_in {
        match module {
            Some(device) => install(Box::new(device), crate::idt::fine_ticks()),
            None => crate::serial_println!("[gpu] absent - pas de virtio-gpu 3D (virtio-vga-gl, venus=true) ni de module : rendu logiciel"),
        }
        READY.store(true, Ordering::Release);
        return;
    }
    let start = crate::idt::fine_ticks();
    match venus::backend::VirtioDevice::probe() {
        Ok(device) => install(Box::new(device), start),
        Err(reason) => crate::serial_println!("[gpu] absent - {} : rendu logiciel", reason),
    }
    READY.store(true, Ordering::Release);
}

/// Makes `device` the GPU of the system.
fn install(device: Box<dyn Device>, start: u64) {
    let device: Arc<dyn Device> = Arc::from(device);
    let description = device.describe();
    crate::serial_println!(
        "[gpu] pret en {} ms - {} ({:04x}:{:04x}), {} Mio locaux, {} Mio visibles, images jusqu'a {}, partage {}",
        crate::idt::fine_ticks() - start,
        description.name,
        description.vendor_id,
        description.device_id,
        description.local_memory >> 20,
        description.visible_memory >> 20,
        description.max_image_size,
        if description.sharing { "oui" } else { "non" }
    );
    *DESCRIPTION.lock() = Some(description);
    *DEVICE.lock() = Some(device);
}

/// Contexts open, all owners together.
pub fn context_count() -> usize {
    TABLES.lock().contexts.len()
}

/// Contexts whose owner is dead: the sweep's work not done yet.
pub fn orphan_count() -> usize {
    TABLES.lock().contexts.values().filter(|c| !c.owner.alive()).count()
}

pub fn dump_state() {
    let tables = TABLES.lock();
    let objects = tables.handles.values().filter(|(_, k, _)| *k == HandleKind::Object).count();
    let memory: u64 = tables.usage.values().sum();
    crate::serial_println!(
        "[gpu] {} contexte(s), {} objet(s), {} Mio comptes, {} soumission(s), {} refusee(s), {} contexte(s) perdu(s)",
        tables.contexts.len(),
        objects,
        memory >> 20,
        SUBMISSIONS.load(Ordering::Relaxed),
        REFUSED.load(Ordering::Relaxed),
        LOST_CONTEXTS.load(Ordering::Relaxed)
    );
    crate::serial_println!(
        "[gpu] anneaux Venus : {} lot(s) ecrits, {} reveil(s) du fil de l'hote, {} session(s) sans anneau",
        venus::RING_BATCHES.load(Ordering::Relaxed),
        venus::RING_WAKEUPS.load(Ordering::Relaxed),
        venus::RING_REFUSED.load(Ordering::Relaxed)
    );
}

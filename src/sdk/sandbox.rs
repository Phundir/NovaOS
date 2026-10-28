//! 9.3 -- Test sandbox (spec 15.4).
//!
//! ## What 15.4 asks for, word by word
//!
//! "Every run creates an isolated context: dedicated memory space,
//! virtualised FS (read-only overlay on the real FS), loopback network
//! only. The app runs as `.ras` automatically. On shutdown: sandbox
//! destroyed, no trace on the host OS."
//!
//! Five requirements, and each has its own sub-module here. None is
//! decorative:
//!
//! ### 9.3.1 -- Dedicated memory space
//!
//! An arena of **its own**, at `0x3000_0000`, disjoint from the three
//! that already exist: loader images, user stacks, `SYS_MEM_ALLOC` heap.
//! Four disjoint ranges rather than one allocator, for the same reason as
//! in phase 5.4: a leak in one must not mask the exhaustion of another.
//! The page budget is **capped** per sandbox: a test allocating in a loop
//! exhausts its own share and nothing else.
//!
//! ### 9.3.2 -- Virtualised FS, read-only overlay
//!
//! Two layers. Below, the real filesystem, **never written**. Above, one
//! directory per sandbox under `/cache/sandbox/{id}/`, where everything
//! the program writes lands. A read consults the upper layer first, then
//! the lower one. A write never goes down.
//!
//! The point that counts, and that is not obvious: reading the lower
//! layer is itself **bounded by the roots of 12.4**. An overlay that let
//! everything be read would give a test binary a complete view of the
//! disk -- exactly what a sandbox exists to prevent.
//!
//! A file deleted in the sandbox cannot be deleted underneath: a
//! **tombstone** is laid instead, a marker that masks the lower layer's
//! entry without touching it. That is the only way to make `delete`
//! observable without writing on the host.
//!
//! ### 9.3.3 -- Loopback network only
//!
//! There is no loopback interface in `net`: the phase 6 stack talks to an
//! e1000 card, and to nothing else. The sandbox provides one, entirely in
//! memory -- ports, queues, and one rule: any address outside
//! `127.0.0.0/8` is refused. This is not a restriction laid on the real
//! stack, it is a separate stack; a sandbox's traffic cannot reach the
//! driver, because there is no path leading it there.
//!
//! ### 9.3.4 -- Running as `.ras`
//!
//! 15.4 imposes it, and 3.4 makes it binding: a `.ras` is **unsigned**
//! (`combination_allowed` admits only `(Ras, None)`) and caps at three
//! permissions (12.4). Replaying the project binary as a `.ras` is
//! therefore not relabelling: it is rebuilding it with its permissions
//! trimmed and its signature removed. A `.dsa` tested in a sandbox loses
//! `KERNEL_SPACE`, and that is the point.
//!
//! ### 9.3.5 -- No trace
//!
//! `destroy` returns the frames, erases the upper tree, closes the ports.
//! The invariant the self-test checks is the phase 5 one: **the number of
//! free frames must return to its earlier value**, and the sandbox
//! directory must no longer exist.

use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::header::{BinaryFormat, SignatureType};
use crate::fs::FsError;
use crate::fs::perm::{Credentials, Mode};
use crate::frame::{self, Frame};
use crate::memory;
use crate::process::Pid;
use crate::vga::SpinLock;

/// Root of the upper layers. Under `/cache/`, which is group-writable: a
/// sandbox needs no privilege to exist.
pub const OVERLAY_ROOT: &str = "/cache/sandbox";

/// Base of the sandbox memory arena (9.3.1).
pub const ARENA_BASE: u64 = 0x3000_0000;
/// 128 MiB of virtual addresses.
pub const ARENA_SIZE: u64 = 0x0800_0000;
/// Page cap per sandbox: 256 pages, that is 1 MiB.
pub const PAGE_BUDGET: usize = 256;

const PAGE: u64 = 4096;

/// Suffix of a tombstone.
///
/// A name that cannot collide with a real file: the filesystem already
/// refuses empty names, and no binary produces a `.tombe` extension.
const TOMBSTONE: &str = ".tombe";

// ============================================================
//  Errors
// ============================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxError {
    /// Path outside the readable roots of 12.4.
    OutsideRoots(String),
    /// The filesystem is not mounted.
    NoFilesystem,
    /// Write refused by the volume.
    Filesystem(FsError),
    /// Page budget exhausted (9.3.1).
    OutOfPages,
    /// Address outside 127.0.0.0/8 (9.3.3).
    NotLoopback([u8; 4]),
    /// Port already open in this sandbox.
    PortBusy(u16),
    /// No port open at that number.
    PortClosed(u16),
    /// Queue of a port full.
    QueueFull(u16),
    /// The binary cannot be replayed as a `.ras`.
    NotRunnable(String),
    /// Unknown sandbox identifier.
    Unknown(u32),
}

impl SandboxError {
    pub fn message(&self) -> String {
        match self {
            SandboxError::OutsideRoots(path) => {
                alloc::format!("chemin hors du bac a sable : {}", path)
            }
            SandboxError::NoFilesystem => String::from("systeme de fichiers absent"),
            SandboxError::Filesystem(error) => {
                alloc::format!("systeme de fichiers : {}", error.message())
            }
            SandboxError::OutOfPages => String::from("budget de pages epuise"),
            SandboxError::NotLoopback(address) => alloc::format!(
                "adresse hors loopback : {}.{}.{}.{}",
                address[0],
                address[1],
                address[2],
                address[3]
            ),
            SandboxError::PortBusy(port) => alloc::format!("port {} deja ouvert", port),
            SandboxError::PortClosed(port) => alloc::format!("port {} ferme", port),
            SandboxError::QueueFull(port) => alloc::format!("file du port {} pleine", port),
            SandboxError::NotRunnable(reason) => {
                alloc::format!("binaire non rejouable en .ras : {}", reason)
            }
            SandboxError::Unknown(id) => alloc::format!("sandbox {} inconnue", id),
        }
    }
}

// ============================================================
//  9.3.3 -- Loopback
// ============================================================

/// Maximum length of a port queue.
const QUEUE_DEPTH: usize = 16;

/// Is an address inside `127.0.0.0/8`?
///
/// The first byte is enough -- that is the definition of the block -- and
/// checking it that way rather than comparing with `127.0.0.1` allows the
/// conventions where a service listens on `127.0.0.53`.
pub fn is_loopback(address: [u8; 4]) -> bool {
    address[0] == 127
}

/// A port open in the loopback stack.
struct Port {
    number: u16,
    queue: Vec<Vec<u8>>,
    received: u64,
    dropped: u64,
}

/// The network stack of a sandbox: ports and nothing else.
#[derive(Default)]
struct Loopback {
    ports: Vec<Port>,
    sent: u64,
    refused: u64,
}

impl Loopback {
    fn bind(&mut self, number: u16) -> Result<(), SandboxError> {
        if self.ports.iter().any(|port| port.number == number) {
            return Err(SandboxError::PortBusy(number));
        }
        self.ports.push(Port {
            number,
            queue: Vec::new(),
            received: 0,
            dropped: 0,
        });
        Ok(())
    }

    fn send(&mut self, address: [u8; 4], number: u16, payload: &[u8]) -> Result<(), SandboxError> {
        if !is_loopback(address) {
            self.refused += 1;
            return Err(SandboxError::NotLoopback(address));
        }
        let Some(port) = self.ports.iter_mut().find(|port| port.number == number) else {
            self.refused += 1;
            return Err(SandboxError::PortClosed(number));
        };
        if port.queue.len() >= QUEUE_DEPTH {
            port.dropped += 1;
            return Err(SandboxError::QueueFull(number));
        }
        port.queue.push(payload.to_vec());
        port.received += 1;
        self.sent += 1;
        Ok(())
    }

    fn receive(&mut self, number: u16) -> Result<Option<Vec<u8>>, SandboxError> {
        let Some(port) = self.ports.iter_mut().find(|port| port.number == number) else {
            return Err(SandboxError::PortClosed(number));
        };
        if port.queue.is_empty() {
            return Ok(None);
        }
        Ok(Some(port.queue.remove(0)))
    }
}

// ============================================================
//  Context
// ============================================================

/// One region of a sandbox memory arena.
struct Region {
    base: u64,
    pages: usize,
    frames: Vec<Frame>,
}

/// An isolated test context.
pub struct Sandbox {
    pub id: u32,
    pub owner: Pid,
    /// Root of the upper layer.
    pub root: String,
    /// Roots readable in the lower layer (12.4).
    pub readable: Vec<String>,
    regions: Vec<Region>,
    pages_used: usize,
    loopback: Loopback,
    /// Files written in the upper layer, for the report.
    pub written: usize,
    /// Reads served by the lower layer.
    pub passthrough: usize,
}

struct Registry {
    sandboxes: Vec<Sandbox>,
    next_id: u32,
    /// Next address never yet handed out in the arena.
    cursor: u64,
    created: u64,
    destroyed: u64,
}

impl Registry {
    const fn new() -> Self {
        Registry {
            sandboxes: Vec::new(),
            next_id: 1,
            cursor: ARENA_BASE,
            created: 0,
            destroyed: 0,
        }
    }

    fn overlaps(&self, base: u64, pages: usize) -> bool {
        let end = base + pages as u64 * PAGE;
        self.sandboxes.iter().any(|sandbox| {
            sandbox.regions.iter().any(|region| {
                let region_end = region.base + region.pages as u64 * PAGE;
                base < region_end && region.base < end
            })
        })
    }

    fn find_slot(&mut self, pages: usize) -> Option<u64> {
        let span = pages as u64 * PAGE;
        let limit = ARENA_BASE + ARENA_SIZE;

        for start in [self.cursor, ARENA_BASE] {
            let mut base = start;
            while base + span <= limit {
                if !self.overlaps(base, pages) {
                    self.cursor = base + span;
                    return Some(base);
                }
                base += PAGE;
            }
        }
        None
    }

    fn index(&self, id: u32) -> Option<usize> {
        self.sandboxes.iter().position(|sandbox| sandbox.id == id)
    }
}

static REGISTRY: SpinLock<Registry> = SpinLock::new(Registry::new());

// ============================================================
//  Life cycle
// ============================================================

/// Opens a sandbox for a given application.
///
/// The readable roots are those 12.4 grants to its format, as
/// `fs::tree::sandbox_roots` computes them. This is the first place in
/// the system where that table really serves a purpose: the limit "the
/// path sandbox of 12.4 is described, not enforced", opened by phase 4,
/// closes here -- for the SDK, and for it alone.
pub fn create(
    owner: Pid,
    format: BinaryFormat,
    app_id: &str,
    now: u64,
) -> Result<u32, SandboxError> {
    if !crate::fs::is_ready() {
        return Err(SandboxError::NoFilesystem);
    }
    let who = Credentials::kernel();

    let mut registry = REGISTRY.lock();
    let id = registry.next_id;
    registry.next_id += 1;
    registry.created += 1;
    drop(registry);

    let root = alloc::format!("{}/{}", OVERLAY_ROOT, id);

    // The overlay root first: it may be missing on the very first call,
    // and creating it here avoids making `install_layout` a prerequisite.
    if crate::fs::stat(OVERLAY_ROOT, who).is_err() {
        crate::fs::create_directory(OVERLAY_ROOT, who, Mode::shared_dir(), now)
            .map_err(SandboxError::Filesystem)?;
    }
    // An identifier reused after a reboot could land on a directory left
    // by an interrupted run. It is purged before use: the phase 7 lesson,
    // purge on the way in as much as on the way out.
    if crate::fs::stat(&root, who).is_ok() {
        purge_tree(&root, now);
    }
    crate::fs::create_directory(&root, who, Mode::shared_dir(), now)
        .map_err(SandboxError::Filesystem)?;

    let readable = crate::fs::tree::sandbox_roots(format, app_id);

    let mut registry = REGISTRY.lock();
    registry.sandboxes.push(Sandbox {
        id,
        owner,
        root,
        readable,
        regions: Vec::new(),
        pages_used: 0,
        loopback: Loopback::default(),
        written: 0,
        passthrough: 0,
    });

    Ok(id)
}

/// 9.3.5 -- Destroys a sandbox and leaves nothing behind.
///
/// Returns `(pages returned, entries erased)`. The frames are returned
/// after extraction under the lock, never during: `crate::fs` may put the
/// task to sleep, and holding a lock across I/O is what `DECISIONS.md`
/// forbids.
pub fn destroy(id: u32, now: u64) -> Result<(usize, usize), SandboxError> {
    let (regions, root) = {
        let mut registry = REGISTRY.lock();
        let Some(index) = registry.index(id) else {
            return Err(SandboxError::Unknown(id));
        };
        let sandbox = registry.sandboxes.remove(index);
        registry.destroyed += 1;
        (sandbox.regions, sandbox.root)
    };

    let mut pages = 0;
    for region in regions.iter() {
        for offset in 0..region.pages {
            unsafe { memory::unmap_page(region.base + offset as u64 * PAGE) };
        }
        for physical in region.frames.iter() {
            unsafe { frame::deallocate_frame(*physical) };
        }
        pages += region.pages;
    }

    let removed = purge_tree(&root, now);
    Ok((pages, removed))
}

/// Erases a subtree, leaves first.
///
/// Returns the number of entries removed. A directory that resists does
/// not stop the purge: better to erase what can be erased and say so than
/// to stop at the first refusal and leave all the rest.
fn purge_tree(path: &str, now: u64) -> usize {
    let who = Credentials::kernel();
    let mut removed = 0;

    if let Ok(entries) = crate::fs::list(path, who) {
        for entry in entries {
            let child = alloc::format!("{}/{}", path, entry.name);
            // A directory is recognised by `list` succeeding on it.
            // Asking `stat` for its type would require knowing the node
            // representation, which this module has no business knowing.
            if crate::fs::list(&child, who).is_ok() {
                removed += purge_tree(&child, now);
            }
            if crate::fs::delete(&child, who, now).is_ok() {
                removed += 1;
            }
        }
    }
    if crate::fs::delete(path, who, now).is_ok() {
        removed += 1;
    }
    removed
}

// ============================================================
//  9.3.1 -- Dedicated memory
// ============================================================

/// Allocates pages in the sandbox arena.
///
/// The frames are taken **then** mapped, and a failure part way through
/// returns everything already taken. Same rule as in 5.4.1: a partial
/// allocation is invisible to the caller and lost to the system.
pub fn allocate(id: u32, bytes: u64) -> Result<u64, SandboxError> {
    let pages = bytes.div_ceil(PAGE).max(1) as usize;

    let mut registry = REGISTRY.lock();
    let Some(index) = registry.index(id) else {
        return Err(SandboxError::Unknown(id));
    };
    if registry.sandboxes[index].pages_used + pages > PAGE_BUDGET {
        return Err(SandboxError::OutOfPages);
    }
    let Some(base) = registry.find_slot(pages) else {
        return Err(SandboxError::OutOfPages);
    };

    let mut frames = Vec::new();
    for offset in 0..pages {
        let Some(physical) = frame::allocate_zeroed_frame() else {
            for taken in frames.iter() {
                unsafe { frame::deallocate_frame(*taken) };
            }
            return Err(SandboxError::OutOfPages);
        };
        // Read-write, never executable: nothing a test drops in its heap
        // has any business executing, and W^X admits no exception.
        if unsafe { memory::map_user_page(base + offset as u64 * PAGE, physical.start_addr(), true, false) }
            .is_err()
        {
            unsafe { frame::deallocate_frame(physical) };
            for taken in frames.iter() {
                unsafe { frame::deallocate_frame(*taken) };
            }
            return Err(SandboxError::OutOfPages);
        }
        frames.push(physical);
    }

    let sandbox = &mut registry.sandboxes[index];
    sandbox.pages_used += pages;
    sandbox.regions.push(Region {
        base,
        pages,
        frames,
    });
    Ok(base)
}

/// Pages currently held by a sandbox.
pub fn pages_used(id: u32) -> Option<usize> {
    let registry = REGISTRY.lock();
    registry
        .index(id)
        .map(|index| registry.sandboxes[index].pages_used)
}

// ============================================================
//  9.3.2 -- Virtualised FS
// ============================================================

/// Equivalent path in the upper layer.
fn upper_path(root: &str, path: &str) -> String {
    let normalized = crate::fs::normalize(path);
    alloc::format!("{}{}", root, normalized)
}

/// Does the path fall under a readable root?
fn within(readable: &[String], path: &str) -> bool {
    let normalized = crate::fs::normalize(path);
    readable.iter().any(|root| {
        let root = crate::fs::normalize(root);
        normalized == root
            || normalized.starts_with(&alloc::format!("{}/", root.trim_end_matches('/')))
    })
}

/// Path of the tombstone of an entry.
fn tombstone_path(root: &str, path: &str) -> String {
    alloc::format!("{}{}", upper_path(root, path), TOMBSTONE)
}

/// Creates the missing directories of a path in the upper layer.
fn ensure_parents(path: &str, now: u64) -> Result<(), SandboxError> {
    let who = Credentials::kernel();
    let Some((parent, _)) = path.rsplit_once('/') else {
        return Ok(());
    };
    if parent.is_empty() || crate::fs::stat(parent, who).is_ok() {
        return Ok(());
    }
    ensure_parents(parent, now)?;
    match crate::fs::create_directory(parent, who, Mode::shared_dir(), now) {
        Ok(_) => Ok(()),
        // A race between two creations of the same parent is not an
        // error: what matters is that the directory exists.
        Err(_) if crate::fs::stat(parent, who).is_ok() => Ok(()),
        Err(error) => Err(SandboxError::Filesystem(error)),
    }
}

/// Reads a file through the overlay.
///
/// Upper layer first, lower layer next -- and only if the path falls
/// under a 12.4 root.
pub fn read(id: u32, path: &str) -> Result<Vec<u8>, SandboxError> {
    let (root, readable) = context(id)?;
    let who = Credentials::kernel();

    let upper = upper_path(&root, path);
    if let Ok(bytes) = crate::fs::read_file(&upper, who) {
        return Ok(bytes);
    }
    // An entry deleted in the sandbox must not reappear from the lower
    // layer: the tombstone masks it.
    if crate::fs::stat(&tombstone_path(&root, path), who).is_ok() {
        return Err(SandboxError::Filesystem(FsError::NotFound));
    }
    if !within(&readable, path) {
        return Err(SandboxError::OutsideRoots(String::from(path)));
    }
    let bytes = crate::fs::read_file(path, who).map_err(SandboxError::Filesystem)?;

    let mut registry = REGISTRY.lock();
    if let Some(index) = registry.index(id) {
        registry.sandboxes[index].passthrough += 1;
    }
    Ok(bytes)
}

/// Writes a file. Always in the upper layer, never below.
pub fn write(id: u32, path: &str, bytes: &[u8], now: u64) -> Result<(), SandboxError> {
    let (root, _) = context(id)?;
    let who = Credentials::kernel();
    let upper = upper_path(&root, path);

    ensure_parents(&upper, now)?;
    // Writing cancels a previous deletion: without this, a file rewritten
    // after removal would stay invisible.
    let _ = crate::fs::delete(&tombstone_path(&root, path), who, now);
    crate::fs::write_file(&upper, bytes, who, Mode::shared_file(), now)
        .map_err(SandboxError::Filesystem)?;

    let mut registry = REGISTRY.lock();
    if let Some(index) = registry.index(id) {
        registry.sandboxes[index].written += 1;
    }
    Ok(())
}

/// Deletes an entry from the sandbox point of view.
///
/// If it exists in the upper layer, it really disappears. If it exists
/// only below, a tombstone is laid: the host is never touched, and the
/// sandbox no longer sees it.
pub fn delete(id: u32, path: &str, now: u64) -> Result<(), SandboxError> {
    let (root, readable) = context(id)?;
    let who = Credentials::kernel();
    let upper = upper_path(&root, path);

    if crate::fs::stat(&upper, who).is_ok() {
        return crate::fs::delete(&upper, who, now).map_err(SandboxError::Filesystem);
    }
    if !within(&readable, path) {
        return Err(SandboxError::OutsideRoots(String::from(path)));
    }
    if crate::fs::stat(path, who).is_err() {
        return Err(SandboxError::Filesystem(FsError::NotFound));
    }
    let tombstone = tombstone_path(&root, path);
    ensure_parents(&tombstone, now)?;
    crate::fs::write_file(&tombstone, b"", who, Mode::shared_file(), now)
        .map(|_| ())
        .map_err(SandboxError::Filesystem)
}

/// Does the path exist for the sandbox?
pub fn exists(id: u32, path: &str) -> bool {
    let Ok((root, readable)) = context(id) else {
        return false;
    };
    let who = Credentials::kernel();
    if crate::fs::stat(&upper_path(&root, path), who).is_ok() {
        return true;
    }
    if crate::fs::stat(&tombstone_path(&root, path), who).is_ok() {
        return false;
    }
    within(&readable, path) && crate::fs::stat(path, who).is_ok()
}

/// Is the path readable in the lower layer?
///
/// Exposed for the self-test: checking the refusal requires being able to
/// query the rule without causing any I/O.
pub fn readable_from_host(id: u32, path: &str) -> bool {
    match context(id) {
        Ok((_, readable)) => within(&readable, path),
        Err(_) => false,
    }
}

fn context(id: u32) -> Result<(String, Vec<String>), SandboxError> {
    let registry = REGISTRY.lock();
    let Some(index) = registry.index(id) else {
        return Err(SandboxError::Unknown(id));
    };
    let sandbox = &registry.sandboxes[index];
    Ok((sandbox.root.clone(), sandbox.readable.clone()))
}

// ============================================================
//  9.3.3 -- Network
// ============================================================

/// Opens a loopback port.
pub fn bind(id: u32, port: u16) -> Result<(), SandboxError> {
    let mut registry = REGISTRY.lock();
    let Some(index) = registry.index(id) else {
        return Err(SandboxError::Unknown(id));
    };
    registry.sandboxes[index].loopback.bind(port)
}

/// Sends to an address. Refuses anything outside `127.0.0.0/8`.
pub fn send(id: u32, address: [u8; 4], port: u16, payload: &[u8]) -> Result<(), SandboxError> {
    let mut registry = REGISTRY.lock();
    let Some(index) = registry.index(id) else {
        return Err(SandboxError::Unknown(id));
    };
    registry.sandboxes[index]
        .loopback
        .send(address, port, payload)
}

/// Removes the next message of a port.
pub fn recv(id: u32, port: u16) -> Result<Option<Vec<u8>>, SandboxError> {
    let mut registry = REGISTRY.lock();
    let Some(index) = registry.index(id) else {
        return Err(SandboxError::Unknown(id));
    };
    registry.sandboxes[index].loopback.receive(port)
}

/// Loopback network counters: (sent, refused, open ports).
pub fn net_stats(id: u32) -> Option<(u64, u64, usize)> {
    let registry = REGISTRY.lock();
    let index = registry.index(id)?;
    let loopback = &registry.sandboxes[index].loopback;
    Some((loopback.sent, loopback.refused, loopback.ports.len()))
}

// ============================================================
//  9.3.4 -- Replay as .ras
// ============================================================

/// What a binary becomes when it enters a sandbox.
#[derive(Debug, Clone)]
pub struct RasPlan {
    pub original_format: BinaryFormat,
    pub original_signature: SignatureType,
    /// Permissions after trimming to the `.ras` ceiling of 12.4.
    pub permissions: u64,
    /// Bits lost in the transformation, for the report.
    pub dropped: u64,
    /// Flags kept: a `.ras` exposes no API and replaces no component,
    /// whatever it declares.
    pub flags: u32,
    /// Does the `.ras` keep its internal ports? Never: 12.3 puts the
    /// format on the deny-list of all communication, and
    /// `binfmt::check_rules` refuses a `.ras` whose port table is not
    /// empty. The field exists so the report can say what was removed.
    pub ports_dropped: usize,
}

/// Computes the transformation imposed by 15.4.
///
/// Touches nothing: it returns the plan. It is `sdk::cli` that rebuilds
/// the binary, because rebuilding assumes knowing the source, which the
/// sandbox has no business knowing.
pub fn plan_ras(
    format: BinaryFormat,
    signature: SignatureType,
    permissions: u64,
    flags: u32,
    ports: usize,
) -> RasPlan {
    use crate::binfmt::header::flags as header_flags;
    use crate::binfmt::identity::Permissions;

    let ceiling = Permissions::ceiling(BinaryFormat::Ras);
    let kept = permissions & ceiling;

    // A `.ras` exposes no API (3.4) and replaces no component: keeping
    // those bits would produce a binary `check_rules` would refuse, hence
    // a test impossible to launch.
    let stripped = header_flags::EXPOSES_API | header_flags::REPLACES_COMPONENT
        | header_flags::AUTO_PAUSE
        | header_flags::DEFAULT_CAPABLE;

    RasPlan {
        original_format: format,
        original_signature: signature,
        permissions: kept,
        dropped: permissions & !ceiling,
        flags: flags & !stripped,
        ports_dropped: ports,
    }
}

// ============================================================
//  State
// ============================================================

/// Identifiers of the sandboxes currently alive.
///
/// Only used by the startup purge, which has to tell a directory left by
/// a previous session from one that belongs to a sandbox alive right now.
/// The two look identical on disk, and deleting the second one pulls the
/// overlay out from under a test that is still running.
pub fn live_ids() -> Vec<u32> {
    let registry = REGISTRY.lock();
    registry.sandboxes.iter().map(|sandbox| sandbox.id).collect()
}

/// Global counters: (alive, created, destroyed).
pub fn stats() -> (usize, u64, u64) {
    let registry = REGISTRY.lock();
    (
        registry.sandboxes.len(),
        registry.created,
        registry.destroyed,
    )
}

pub fn dump_state() {
    let registry = REGISTRY.lock();
    crate::serial_println!(
        "[sandbox] {} vivante(s) - {} creee(s), {} detruite(s)",
        registry.sandboxes.len(),
        registry.created,
        registry.destroyed
    );
    for sandbox in registry.sandboxes.iter() {
        crate::serial_println!(
            "[sandbox]   {} PID {} - {} page(s), {} ecriture(s), {} lecture(s) hote, {} port(s)",
            sandbox.root,
            sandbox.owner,
            sandbox.pages_used,
            sandbox.written,
            sandbox.passthrough,
            sandbox.loopback.ports.len()
        );
    }
}

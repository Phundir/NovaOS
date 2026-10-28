//! 3.2 -- Nova OS binary loader (spec 3.4).
//!
//! Takes a file in memory, makes it an image executable in ring 3, and
//! attaches a scheduling task to it. Four steps, in this order -- the
//! order matters, each one assumes the previous:
//!
//! 1. **parsing** (3.1) then **signature verification** (3.3). A refused
//!    image consumes not a single frame;
//! 2. **reservation of a slot** in the user arena, then copy of the
//!    sections into fresh frames;
//! 3. **relocation** (3.2.2) if the slot obtained is not the base the
//!    binary was linked for;
//! 4. **mapping** with the final rights, then creation of the task and
//!    transfer of control (3.2.3).
//!
//! ## Address space
//!
//! Every task shares a single page hierarchy: phase 2 has no per-process
//! address space. Two images therefore cannot occupy the same virtual
//! range, and that is precisely what makes relocation necessary now
//! rather than incidental. The arena cuts the lower half into disjoint
//! slots:
//!
//! ```text
//!   0x00400000   statically linked demonstration program (phase 2.8)
//!   0x00800000   stack of that program
//!   0x01000000   image arena       -- one 2 MiB slot per binary
//!   0x10000000   stack arena       -- one 1 MiB slot per task
//! ```
//!
//! Isolation therefore stays **conventional**: nothing stops a ring 3
//! task from reading the page of another, since they are all present in
//! the same hierarchy. Real separation will come with a CR3 per process
//! -- see the known limits of the roadmap.
//!
//! ## W^X
//!
//! A page receives either the write right or the execute right, never
//! both. It is parsing (3.1) that guarantees no section claims both, and
//! the splitting into pages that guarantees two sections with distinct
//! rights do not end up together. The NO_EXECUTE bit is only effective if
//! EFER.NXE has been armed: `memory::enable_nx`.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use super::header::{BinaryFormat, RelocKind, SectionKind, SignatureType, flags};
use super::identity::{AppIdentity, Permissions};
use super::signature::{self, Verdict};
use super::{BinaryHeader, ParseError, RuleViolation};
use crate::frame::{self, Frame};
use crate::memory::{self, phys_to_virt};
use crate::process::{BinaryType, Pid};
use crate::scheduler;
use crate::vga::SpinLock;

// ============================================================
//  User arena
// ============================================================

/// Base of the arena of loaded images.
pub const IMAGE_ARENA_BASE: u64 = 0x0100_0000;
/// Size of an image slot. Caps the size of a binary.
pub const IMAGE_SLOT_SIZE: u64 = 0x0020_0000; // 2 MiB
/// Number of image slots.
const IMAGE_SLOTS: usize = 32;

/// Base of the user stack arena.
pub const STACK_ARENA_BASE: u64 = 0x1000_0000;
/// Size of a stack slot, guard included.
pub const STACK_SLOT_SIZE: u64 = 0x0010_0000; // 1 MiB
/// Minimum stack granted, whatever the header asks for.
const MIN_STACK: u64 = 16 * 1024;
/// Guard page left unmapped under each stack: an overflow produces an
/// identifiable page fault instead of silently crushing what lies
/// beneath.
const STACK_GUARD: u64 = 4096;

const PAGE: u64 = 4096;

// ============================================================
//  Errors
// ============================================================

#[derive(Debug, Clone)]
pub enum LoadError {
    /// The file is not a well-formed Nova OS binary.
    Malformed(ParseError),
    /// Well formed, but violates a rule of the specification.
    Refused(Vec<RuleViolation>),
    /// Invalid signature or origin not established.
    Untrusted(signature::Reason),
    /// The image exceeds the size of a slot.
    TooLarge(u64),
    /// Stack requested beyond what a slot can offer.
    StackTooLarge(u64),
    /// No free slot left in the arena.
    ArenaExhausted,
    /// No physical memory left.
    OutOfFrames,
    /// Failure to install a mapping.
    MappingFailed(&'static str),
    /// Relocation type this kernel cannot apply.
    UnsupportedReloc,
}

impl LoadError {
    pub fn message(&self) -> &'static str {
        match self {
            LoadError::Malformed(_) => "binaire malforme",
            LoadError::Refused(_) => "binaire refuse par la specification",
            LoadError::Untrusted(_) => "signature invalide",
            LoadError::TooLarge(_) => "image plus grande qu'un creneau",
            LoadError::StackTooLarge(_) => "pile demandee trop grande",
            LoadError::ArenaExhausted => "plus de creneau libre dans l'arene",
            LoadError::OutOfFrames => "plus de memoire physique disponible",
            LoadError::MappingFailed(_) => "echec de mapping",
            LoadError::UnsupportedReloc => "type de relocation non supporte",
        }
    }

    /// Distinction from the spec: *rejected* = malformed, *refused* = well
    /// formed but forbidden. SPM must be able to explain the difference.
    pub fn is_rejection(&self) -> bool {
        matches!(self, LoadError::Malformed(_))
    }
}

// ============================================================
//  Loaded image
// ============================================================

/// An image installed in memory, ready to run.
pub struct LoadedImage {
    pub identity: AppIdentity,
    /// Type the scheduler handles (3.4.1).
    pub binary_type: BinaryType,
    /// Effective base, after the slot is assigned.
    pub base: u64,
    /// Base the binary had been linked for.
    pub link_base: u64,
    pub entry_point: u64,
    pub image_size: u64,
    pub stack_base: u64,
    /// Initial value of RSP in ring 3: the top of the mapped area, less a
    /// margin and aligned on 16 bytes as the System V ABI demands.
    /// Pushing at the exact end address would write into the next page,
    /// which is not mapped.
    pub stack_top: u64,
    pub stack_size: u64,
    /// Number of fixes applied (3.2.2).
    pub relocations_applied: usize,
    /// Offset between link base and effective base.
    pub delta: i64,
    pub trust: Verdict,
    pub permissions: Permissions,
    pub flags: u32,
    /// Physical frames held by the image, for the release.
    frames: Vec<Frame>,
    /// Pages mapped, image and stack together.
    pages: Vec<u64>,
    slot: usize,
    stack_slot: usize,
}

impl LoadedImage {
    pub fn format(&self) -> BinaryFormat {
        self.identity.format
    }

    /// Fixed priority derived from the format (3.4.1, spec 7.3).
    pub fn priority(&self) -> u8 {
        self.binary_type.priority()
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
}

// ============================================================
//  Global state
// ============================================================

struct Arena {
    images: [bool; IMAGE_SLOTS],
    stacks: [bool; IMAGE_SLOTS],
    loaded: u64,
    refused: u64,
    rejected: u64,
}

impl Arena {
    const fn new() -> Self {
        Arena {
            images: [false; IMAGE_SLOTS],
            stacks: [false; IMAGE_SLOTS],
            loaded: 0,
            refused: 0,
            rejected: 0,
        }
    }

    fn take_image_slot(&mut self) -> Option<usize> {
        let slot = self.images.iter().position(|used| !used)?;
        self.images[slot] = true;
        Some(slot)
    }

    fn take_stack_slot(&mut self) -> Option<usize> {
        let slot = self.stacks.iter().position(|used| !used)?;
        self.stacks[slot] = true;
        Some(slot)
    }
}

/// Single lock of the loader.
///
/// It protects more than the arena: every task shares one page hierarchy,
/// and `map_user_page` creates intermediate tables as needed. Two
/// simultaneous loads on two cores could allocate two PDPTs for the same
/// index and lose one. Serialising the loads is the simplest answer, and
/// their frequency justifies nothing finer.
static ARENA: SpinLock<Arena> = SpinLock::new(Arena::new());

/// Launch parameters, laid down before the task becomes eligible.
struct Launch {
    entry: u64,
    stack_top: u64,
    format: BinaryFormat,
    trust: Verdict,
    name: String,
}

static LAUNCHES: SpinLock<Option<BTreeMap<Pid, Launch>>> = SpinLock::new(None);

/// Images whose task is still running.
///
/// A launched image can be neither freed nor forgotten: its frames and
/// its mappings must outlive the task. Keeping them here rather than
/// abandoning them holds the exact inventory of what the loader owns --
/// the day `SYS_PROC_WAIT` cleans up a dead task (phase 5.3.3), it is in
/// this table that it will look for what to give back.
static RESIDENT: SpinLock<Option<Vec<(Pid, LoadedImage)>>> = SpinLock::new(None);

/// Hands an image to the kernel for the whole lifetime of its task.
///
/// The image is filed under the PID running it: that is what lets
/// `SYS_PROC_WAIT` know what to give back when that task dies. Without
/// that association, the loader knew *what it held* but not *for whom*,
/// and could therefore free nothing.
pub fn keep(pid: Pid, image: LoadedImage) {
    let mut resident = RESIDENT.lock();
    resident.get_or_insert_with(Vec::new).push((pid, image));
}

/// 5.3.3 -- Gives back the image of a dead task.
///
/// Returns the number of pages given back, or `None` if that PID had no
/// resident image -- the normal case of a kernel task.
///
/// # Safety
/// The task must be dead and never elected again: its pages disappear,
/// and any code still there would be executed into the void.
pub unsafe fn release_for(pid: Pid) -> Option<usize> {
    let image = {
        let mut resident = RESIDENT.lock();
        let images = resident.as_mut()?;
        let position = images.iter().position(|(owner, _)| *owner == pid)?;
        images.swap_remove(position).1
    };

    let pages = image.page_count();
    unsafe { unload(image) };
    Some(pages)
}

/// Number of images currently resident, and total of the pages they
/// occupy.
pub fn resident_stats() -> (usize, usize) {
    let resident = RESIDENT.lock();
    match resident.as_ref() {
        Some(images) => (
            images.len(),
            images.iter().map(|(_, image)| image.page_count()).sum(),
        ),
        None => (0, 0),
    }
}

// ============================================================
//  3.2.1 -- Loading
// ============================================================

/// Parses, verifies and installs a binary in memory.
///
/// Creates no task: see `spawn` for the transfer of control.
pub fn load(data: &[u8]) -> Result<LoadedImage, LoadError> {
    // --- 3.1: structural and regulatory parsing ---
    let inspection = match super::inspect(data) {
        Ok(inspection) => inspection,
        Err(error) => {
            ARENA.lock().rejected += 1;
            return Err(LoadError::Malformed(error));
        }
    };
    if !inspection.is_admissible() {
        ARENA.lock().refused += 1;
        return Err(LoadError::Refused(inspection.violations));
    }

    // --- 3.3: signature ---
    let header = inspection.header;
    let trust = signature::verify(&header, data);
    if let Verdict::Rejected(reason) = trust {
        ARENA.lock().refused += 1;
        return Err(LoadError::Untrusted(reason));
    }

    // --- sizing ---
    let image_pages = header.image_size.div_ceil(PAGE);
    if image_pages * PAGE > IMAGE_SLOT_SIZE {
        return Err(LoadError::TooLarge(header.image_size));
    }

    let stack_size = header.stack_size.max(MIN_STACK).next_multiple_of(PAGE);
    if stack_size + STACK_GUARD > STACK_SLOT_SIZE {
        return Err(LoadError::StackTooLarge(header.stack_size));
    }

    let mut arena = ARENA.lock();
    let slot = arena.take_image_slot().ok_or(LoadError::ArenaExhausted)?;
    let stack_slot = match arena.take_stack_slot() {
        Some(s) => s,
        None => {
            arena.images[slot] = false;
            return Err(LoadError::ArenaExhausted);
        }
    };

    let base = IMAGE_ARENA_BASE + slot as u64 * IMAGE_SLOT_SIZE;
    let stack_top = STACK_ARENA_BASE + (stack_slot as u64 + 1) * STACK_SLOT_SIZE;
    let stack_base = stack_top - stack_size;

    let mut image = match install(&header, data, base, stack_base, stack_size, trust) {
        Ok(image) => image,
        Err(error) => {
            arena.images[slot] = false;
            arena.stacks[stack_slot] = false;
            return Err(error);
        }
    };

    image.slot = slot;
    image.stack_slot = stack_slot;
    arena.loaded += 1;
    drop(arena);

    Ok(image)
}

/// Copies, relocates and maps. Any error gives back the frames already taken.
fn install(
    header: &BinaryHeader,
    data: &[u8],
    base: u64,
    stack_base: u64,
    stack_size: u64,
    trust: Verdict,
) -> Result<LoadedImage, LoadError> {
    let image_pages = header.image_size.div_ceil(PAGE) as usize;
    let stack_pages = (stack_size / PAGE) as usize;

    // --- frame allocation ---
    // Every frame is taken in one block: better to fail before having
    // installed a single mapping than to have to take a half-built image
    // apart.
    let mut frames: Vec<Frame> = Vec::with_capacity(image_pages + stack_pages);
    for _ in 0..(image_pages + stack_pages) {
        match frame::allocate_zeroed_frame() {
            Some(f) => frames.push(f),
            None => {
                release_frames(&frames);
                return Err(LoadError::OutOfFrames);
            }
        }
    }

    // --- 3.2.1: copy of the sections ---
    // The write goes through the physical window: the user pages do not
    // exist yet, and we certainly do not want to create them writable
    // only to close them afterwards.
    let link_base = header.load_base;
    for section in header.loadable_sections() {
        if !section.kind.has_file_content() || section.file_size == 0 {
            continue;
        }
        let image_offset = section.virt_addr - link_base;
        let source = &data[section.file_offset as usize
            ..section.file_offset as usize + section.file_size as usize];
        write_image(&frames[..image_pages], image_offset, source);
    }
    // The uninitialised area has nothing to do: the frames come out of
    // the allocator already zeroed.

    // --- 3.2.2: relocations ---
    let delta = base as i64 - link_base as i64;
    let mut relocations_applied = 0;

    if delta != 0 {
        for reloc in header.relocations.iter() {
            match reloc.kind {
                RelocKind::Abs64 => {
                    let offset = reloc.virt_addr - link_base;
                    let old = read_image_u64(&frames[..image_pages], offset);
                    let new = old
                        .wrapping_add(delta as u64)
                        .wrapping_add(reloc.addend as u64);
                    write_image(&frames[..image_pages], offset, &new.to_le_bytes());
                    relocations_applied += 1;
                }
            }
        }
    }

    // --- per-page rights ---
    let permissions = page_permissions(header, image_pages);

    // --- 3.2.1: final mapping ---
    let mut pages: Vec<u64> = Vec::with_capacity(image_pages + stack_pages);

    for (index, frame) in frames[..image_pages].iter().enumerate() {
        let virt = base + index as u64 * PAGE;
        let (writable, executable) = permissions[index];
        if let Err(reason) =
            unsafe { memory::map_user_page(virt, frame.start_addr(), writable, executable) }
        {
            unmap_all(&pages);
            release_frames(&frames);
            return Err(LoadError::MappingFailed(reason));
        }
        pages.push(virt);
    }

    // --- stack: writable, never executable ---
    for (index, frame) in frames[image_pages..].iter().enumerate() {
        let virt = stack_base + index as u64 * PAGE;
        if let Err(reason) =
            unsafe { memory::map_user_page(virt, frame.start_addr(), true, false) }
        {
            unmap_all(&pages);
            release_frames(&frames);
            return Err(LoadError::MappingFailed(reason));
        }
        pages.push(virt);
    }

    let entry_point = (header.entry_point as i64 + delta) as u64;

    Ok(LoadedImage {
        identity: header.identity.clone(),
        binary_type: header.format().scheduler_type(),
        base,
        link_base,
        entry_point,
        image_size: header.image_size,
        stack_base,
        stack_top: (stack_base + stack_size - 16) & !0xF,
        stack_size,
        relocations_applied,
        delta,
        trust,
        permissions: header.permissions,
        flags: header.flags,
        frames,
        pages,
        slot: 0,
        stack_slot: 0,
    })
}

/// Final rights of each page of the image.
///
/// A page covered by no section stays read-only and non-executable: it is
/// padding, nobody has a reason to write into it or jump to it.
fn page_permissions(header: &BinaryHeader, image_pages: usize) -> Vec<(bool, bool)> {
    let mut permissions = alloc::vec![(false, false); image_pages];
    let link_base = header.load_base;

    for section in header.loadable_sections() {
        let Some((first, last)) = section.page_span() else {
            continue;
        };
        let base_page = link_base / PAGE;
        for page in first..=last {
            let index = (page - base_page) as usize;
            if index >= image_pages {
                continue;
            }
            // Parsing has already guaranteed that no section combines W
            // and X, and that no page carries two sections with different
            // rights: the union therefore cannot produce an RWX page.
            permissions[index].0 |= section.is_writable();
            permissions[index].1 |= section.is_executable();
        }
    }

    permissions
}

// ============================================================
//  Access to the image under construction
// ============================================================

/// Writes into the image through the physical window, crossing page
/// boundaries as needed -- two consecutive frames of the image have no
/// reason to be contiguous in physical memory.
fn write_image(frames: &[Frame], offset: u64, source: &[u8]) {
    let mut written = 0usize;
    while written < source.len() {
        let position = offset + written as u64;
        let page = (position / PAGE) as usize;
        if page >= frames.len() {
            return;
        }
        let in_page = (position % PAGE) as usize;
        let take = (PAGE as usize - in_page).min(source.len() - written);

        let destination = (phys_to_virt(frames[page].start_addr()) + in_page as u64) as *mut u8;
        unsafe {
            core::ptr::copy_nonoverlapping(source.as_ptr().add(written), destination, take);
        }
        written += take;
    }
}

/// Reads a 64-bit word in the image, crossing a page boundary as needed.
fn read_image_u64(frames: &[Frame], offset: u64) -> u64 {
    let mut bytes = [0u8; 8];
    for (i, slot) in bytes.iter_mut().enumerate() {
        let position = offset + i as u64;
        let page = (position / PAGE) as usize;
        if page >= frames.len() {
            return 0;
        }
        let address = phys_to_virt(frames[page].start_addr()) + position % PAGE;
        *slot = unsafe { (address as *const u8).read() };
    }
    u64::from_le_bytes(bytes)
}

fn release_frames(frames: &[Frame]) {
    for frame in frames {
        unsafe { frame::deallocate_frame(*frame) };
    }
}

fn unmap_all(pages: &[u64]) {
    for page in pages {
        unsafe { memory::unmap_page(*page) };
    }
}

/// Gives back to the allocator everything an image holds.
///
/// # Safety
/// No task must still be running in this image.
pub unsafe fn unload(image: LoadedImage) {
    unmap_all(&image.pages);
    release_frames(&image.frames);

    let mut arena = ARENA.lock();
    arena.images[image.slot] = false;
    arena.stacks[image.stack_slot] = false;
}

// ============================================================
//  3.2.3 -- Transfer of control
// ============================================================

/// Creates the task of a loaded image and makes it eligible.
///
/// 3.4.1 and 3.4.2: priority and core follow from the format alone, with
/// no possible intervention by the caller -- spec 7.1 declares them fixed.
pub fn spawn(image: &LoadedImage) -> Pid {
    spawn_with(image, |_| {})
}

/// `spawn`, with `before` called on the new PID before the task can run
/// -- the debugger attaches there, so the first trap finds it (16.4).
pub fn spawn_with(image: &LoadedImage, before: impl FnOnce(Pid)) -> Pid {
    let name = task_name(&image.identity.app_id);

    // 3.2.4: rules proper to the type, applied before the task can be
    // elected.
    apply_type_rules(image);

    let (pid, cpu) = scheduler::spawn_dormant(&name, image.binary_type, user_task_entry);

    // 5.6.1 and 5.6.4: the task inherits the account of its creator, and
    // carries the flags declared by the binary header. It is on those two
    // pieces of information that the access checks of the `SYS_USER_*` and
    // `SYS_NET_*` domains rest: without them, the kernel would have to
    // take a ring 3 program at its word.
    scheduler::adopt(pid, image.flags);

    // 6.4.1: the identity of the binary is attached to the task. That is
    // what makes the rules of 12.1 enforceable -- without it, IPC would
    // see only two PIDs, and a PID says neither the group nor the format.
    // It is laid down by the loader, never by the program: 12.1 wants it
    // "immutable at run time".
    crate::apps::register(pid, image.identity.clone(), image.permissions);

    {
        let mut launches = LAUNCHES.lock();
        let map = launches.get_or_insert_with(BTreeMap::new);
        map.insert(
            pid,
            Launch {
                entry: image.entry_point,
                stack_top: image.stack_top,
                format: image.format(),
                trust: image.trust,
                name: name.clone(),
            },
        );
    }

    crate::serial_println!(
        "[load] tache {} \"{}\" ({}) - priorite {}, coeur {} ({}), tranche {} tics",
        pid,
        name,
        image.format().extension(),
        image.priority(),
        cpu,
        crate::process::affinity_label(image.binary_type, crate::smp_core_count()),
        image.binary_type.time_slice(true)
    );

    // 3.2.4.3: a `.gs` declaring the automatic switch starts in the
    // foreground; it is the compositor that will send it to the
    // background in phase 7. The slice then goes from 20 to 2 ticks.
    if image.format() == BinaryFormat::Gs && image.flags & flags::AUTO_PAUSE != 0 {
        scheduler::set_foreground(pid, true);
    }

    before(pid);
    scheduler::resume(pid);
    pid
}

/// Loads then launches in a single operation.
pub fn load_and_spawn(data: &[u8]) -> Result<(LoadedImage, Pid), LoadError> {
    let image = load(data)?;
    let pid = spawn(&image);
    Ok((image, pid))
}

/// Body of every task issued from a loaded binary.
///
/// It does nothing but find its parameters again and cross the privilege
/// boundary: everything that follows runs in ring 3.
fn user_task_entry() -> ! {
    let pid = scheduler::current_pid();

    let launch = {
        let mut launches = LAUNCHES.lock();
        launches.as_mut().and_then(|map| map.remove(&pid))
    };

    let Some(launch) = launch else {
        crate::serial_println!(
            "[load] tache {} sans parametres de lancement - abandon",
            pid
        );
        scheduler::exit_current()
    };

    // 3.2.4.4 -- Warning mandatory **at launch**, not only at install:
    // a `.ras` must never start in silence.
    if launch.format == BinaryFormat::Ras {
        crate::vga::set_color(crate::vga::Color::White, crate::vga::Color::Red);
        crate::kprintln!(
            " ATTENTION : \"{}\" est un .ras - acces direct au materiel, aucune isolation ",
            launch.name
        );
        crate::vga::set_color(crate::vga::Color::LightGray, crate::vga::Color::Black);
        crate::serial_println!(
            "[load] AVERTISSEMENT .ras : \"{}\" demarre sans aucune garantie (spec 3.4)",
            launch.name
        );
    } else if launch.trust.requires_warning() {
        crate::vga::set_color(crate::vga::Color::Yellow, crate::vga::Color::Black);
        crate::kprintln!(
            "[load] \"{}\" : {} - origine non etablie",
            launch.name,
            launch.trust.as_str()
        );
        crate::vga::set_color(crate::vga::Color::LightGray, crate::vga::Color::Black);
    }

    crate::serial_println!(
        "[load] tache {} entre en ring 3 a 0x{:x} (pile 0x{:x}) sur le coeur {}",
        pid,
        launch.entry,
        launch.stack_top,
        crate::smp::current_cpu_id()
    );

    // The image was mapped by whichever core ran the loader, and this one
    // may still hold the translation of the previous occupant of the same
    // arena slot. Synchronising here is the point that matters: a stale
    // entry costs nothing anywhere else in the kernel, and costs the
    // whole program right here.
    crate::memory::sync_tlb();

    unsafe { crate::user::enter_ring3(launch.entry, launch.stack_top) }
}

/// Task name derived from the app_id: the last segment is enough to
/// identify the application in a process table.
fn task_name(app_id: &str) -> String {
    let short = app_id.rsplit('.').next().unwrap_or(app_id);
    if short.is_empty() {
        String::from("anonyme")
    } else {
        String::from(short)
    }
}

// ============================================================
//  3.2.4 -- Per-type rules (spec 3.4)
// ============================================================

/// Logs and applies what the format imposes at launch.
///
/// The admission conditions -- signature demanded, permission ceiling --
/// have already done their work: an image arriving here respects them.
/// What is left is what affects **execution**.
fn apply_type_rules(image: &LoadedImage) {
    match image.format() {
        // 3.2.4.1 -- SYSTEM signature mandatory, kernel space access.
        BinaryFormat::Dsa => {
            let kernel_space = image.permissions.contains(Permissions::KERNEL_SPACE);
            crate::serial_println!(
                "[load]   .dsa : signature {} - kernel space {} - API publique {}",
                image.identity.signature.as_str(),
                if kernel_space { "declare" } else { "non declare" },
                if image.flags & flags::EXPOSES_API != 0 {
                    "exposee"
                } else {
                    "non exposee"
                }
            );
        }

        // 3.2.4.2 -- SYSTEM or ENTERPRISE, may replace a component.
        BinaryFormat::Ss => {
            crate::serial_println!(
                "[load]   .ss : signature {} - remplacement de composant {}",
                image.identity.signature.as_str(),
                if image.flags & flags::REPLACES_COMPONENT != 0 {
                    "declare"
                } else {
                    "non declare"
                }
            );
        }

        // 3.2.4.3 -- foreground/background switch, CPU cut to 2 %.
        BinaryFormat::Gs => {
            let auto = image.flags & flags::AUTO_PAUSE != 0;
            crate::serial_println!(
                "[load]   .gs : bascule automatique {} - tranche {} tics au premier plan, {} en arriere-plan",
                if auto { "active" } else { "inactive" },
                image.binary_type.time_slice(true),
                image.binary_type.time_slice(false)
            );
        }

        BinaryFormat::Cus => {
            crate::serial_println!(
                "[load]   .cus : signature {} - application par defaut {}",
                image.identity.signature.as_str(),
                if image.flags & flags::DEFAULT_CAPABLE != 0 {
                    "possible"
                } else {
                    "non demandee"
                }
            );
        }

        // 3.2.4.4 -- warning mandatory, never the default application.
        // The warning itself is emitted at the effective launch, in
        // `user_task_entry`.
        BinaryFormat::Ras => {
            crate::serial_println!(
                "[load]   .ras : application par defaut {} - isolation IPC totale (spec 12.3)",
                if image.format().can_be_default() {
                    "possible"
                } else {
                    "INTERDITE"
                }
            );
        }
    }
}

// ============================================================
//  Diagnostics
// ============================================================

/// Statistics: (images loaded, refused, rejected).
pub fn stats() -> (u64, u64, u64) {
    let arena = ARENA.lock();
    (arena.loaded, arena.refused, arena.rejected)
}

/// Logs the detail of an installed image.
pub fn dump(image: &LoadedImage) {
    crate::serial_println!(
        "[load]   image        : 0x{:x}-0x{:x} ({} pages) - liee pour 0x{:x}",
        image.base,
        image.base + image.image_size,
        image.page_count(),
        image.link_base
    );
    crate::serial_println!(
        "[load]   entree       : 0x{:x} - decalage {}0x{:x}",
        image.entry_point,
        if image.delta < 0 { "-" } else { "+" },
        image.delta.unsigned_abs()
    );
    crate::serial_println!(
        "[load]   relocations  : {} appliquees",
        image.relocations_applied
    );
    crate::serial_println!(
        "[load]   pile         : 0x{:x}-0x{:x} ({} Kio) - RSP initial 0x{:x}, garde a 0x{:x}",
        image.stack_base,
        image.stack_base + image.stack_size,
        image.stack_size / 1024,
        image.stack_top,
        image.stack_base - STACK_GUARD
    );
    crate::serial_println!(
        "[load]   confiance    : {} - priorite {} ({})",
        image.trust.as_str(),
        image.priority(),
        image.binary_type.as_str()
    );
}

/// Checks the effective rights of the pages of an image.
///
/// Reads the page hierarchy back rather than the intentions of the
/// loader: it is the only way to prove that W^X is really in place, and
/// not merely requested.
pub fn verify_protection(image: &LoadedImage) -> bool {
    let mut ok = true;

    for page in image.pages.iter() {
        let Some(translation) = (unsafe { memory::translate(*page) }) else {
            crate::serial_println!("[load] ECHEC : page 0x{:x} non mappee", page);
            ok = false;
            continue;
        };
        if !translation.user {
            crate::serial_println!(
                "[load] ECHEC : page 0x{:x} inaccessible depuis le ring 3",
                page
            );
            ok = false;
        }
        if translation.writable && translation.executable {
            crate::serial_println!(
                "[load] ECHEC : page 0x{:x} a la fois modifiable et executable",
                page
            );
            ok = false;
        }
    }

    // The guard page under the stack must certainly not be mapped.
    let guard = image.stack_base - STACK_GUARD;
    if unsafe { memory::translate(guard) }.is_some() {
        crate::serial_println!("[load] ECHEC : page de garde 0x{:x} mappee", guard);
        ok = false;
    }

    ok
}

/// Checks that a relocation has really been laid down, by reading the
/// fixed word back from the installed image.
///
/// # Safety
/// The address must belong to a loaded and still mapped image.
pub unsafe fn read_user_u64(virt: u64) -> Option<u64> {
    let translation = unsafe { memory::translate(virt) }?;
    let address = phys_to_virt(translation.phys);
    Some(unsafe { (address as *const u64).read_unaligned() })
}

/// Does the section type carry content to load? Exposed for the sample
/// generator.
pub fn is_loadable(kind: SectionKind) -> bool {
    kind.is_loadable()
}

/// Signature allowed for this format -- relay to 3.3.5, used by SPM.
pub fn signature_allowed(format: BinaryFormat, signature: SignatureType) -> bool {
    signature::combination_allowed(format, signature)
}

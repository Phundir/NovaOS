//! 5.4 -- Domain 0x03, memory (spec 6.4).
//!
//! ## One more arena
//!
//! The loader already owns two virtual arenas: the images at
//! 0x0100_0000, the stacks at 0x1000_0000. This one starts at 0x2000_0000
//! and serves the allocations programs ask for while running. Three
//! disjoint ranges rather than a single allocator: each has its
//! granularity, its lifetime and its rights, and confusing them would
//! make a leak in one mask the exhaustion of another.
//!
//! Allocation is **by pages**. A program wanting a fine allocator builds
//! one on top, as every standard library does above `mmap`: the kernel
//! has no business knowing the notion of an object.
//!
//! ## What `SHARE` can and cannot do
//!
//! Every task today shares a single page hierarchy -- a limit opened by
//! phase 3, and still open. A region allocated by one task is therefore
//! **already** readable by the others: `SHARE` cannot make reachable what
//! already is.
//!
//! What it does nonetheless makes sense: it **records an authorisation**
//! and **notifies the recipient** by an IPC message carrying the address
//! and the size. It is the protocol both parties will follow when a CR3
//! per process makes the sharing real -- the day isolation becomes
//! effective, only the body of this call will change, not its contract.
//! Saying so here stops a program taking de facto access for a guarantee.
//!
//! ## `LOCK`, with no swap
//!
//! There is no virtual memory paged to disk. `LOCK` can therefore prevent
//! nothing: it records the intention on the region, which the future
//! eviction mechanism will have to respect. Returning `NOT_IMPLEMENTED`
//! would be wrong -- the effect asked for, "this region will not leave
//! RAM", is indeed obtained, only for a reason the program does not know.

use alloc::vec::Vec;

use super::error::{self, ErrorCode, SysError, SysResult};
use super::table::{Entry, entry};
use super::Call;
use crate::frame::{self, Frame};
use crate::memory;
use crate::process::Pid;
use crate::vga::SpinLock;

pub const SYS_MEM_ALLOC: u16 = 0x0301;
pub const SYS_MEM_FREE: u16 = 0x0302;
pub const SYS_MEM_RESIZE: u16 = 0x0303;
pub const SYS_MEM_SHARE: u16 = 0x0304;
pub const SYS_MEM_PROTECT: u16 = 0x0305;
pub const SYS_MEM_LOCK: u16 = 0x0307;
/// Absent from the spec: state of the arena, indispensable to check that
/// a free really gives back what an allocation took.
pub const SYS_MEM_STAT: u16 = 0x0308;

pub static TABLE: &[Entry] = &[
    entry!(SYS_MEM_ALLOC, "ALLOC", Sync, alloc_region),
    entry!(SYS_MEM_FREE, "FREE", Sync, free_region),
    entry!(SYS_MEM_RESIZE, "RESIZE", Sync, resize_region),
    entry!(SYS_MEM_SHARE, "SHARE", Sync, share_region),
    entry!(SYS_MEM_PROTECT, "PROTECT", Sync, protect_region),
    entry!(SYS_MEM_LOCK, "LOCK", Sync, lock_region),
    entry!(SYS_MEM_STAT, "STAT", Sync, stat),
];

/// Base of the arena of the user allocations.
pub const HEAP_ARENA_BASE: u64 = 0x2000_0000;
/// Size of the arena: 256 MiB of virtual addresses.
pub const HEAP_ARENA_SIZE: u64 = 0x1000_0000;
/// Cap of a single allocation: 16 MiB.
///
/// An outright bound stops an aberrant value making the allocator walk
/// millions of pages before failing.
pub const ALLOC_MAX: u64 = 16 * 1024 * 1024;

const PAGE: u64 = 4096;

// --- rights asked for by PROTECT ---
const PROT_READ: u64 = 1 << 0;
const PROT_WRITE: u64 = 1 << 1;
const PROT_EXEC: u64 = 1 << 2;

/// A region allocated to a task.
struct Region {
    owner: Pid,
    base: u64,
    pages: usize,
    frames: Vec<Frame>,
    prot: u64,
    locked: bool,
    /// Tasks allowed to use it -- see the header on `SHARE`.
    shared: Vec<Pid>,
}

struct Arena {
    regions: Vec<Region>,
    /// Next address never yet handed out.
    ///
    /// The holes left by the frees are reused by the walk of `find_slot`;
    /// this cursor serves only to avoid starting from the beginning every
    /// time.
    cursor: u64,
    allocated_pages: usize,
    peak_pages: usize,
}

impl Arena {
    const fn new() -> Self {
        Arena {
            regions: Vec::new(),
            cursor: HEAP_ARENA_BASE,
            allocated_pages: 0,
            peak_pages: 0,
        }
    }

    fn overlaps(&self, base: u64, pages: usize) -> bool {
        let end = base + pages as u64 * PAGE;
        self.regions.iter().any(|region| {
            let region_end = region.base + region.pages as u64 * PAGE;
            base < region_end && region.base < end
        })
    }

    /// Looks for a free virtual range.
    fn find_slot(&mut self, pages: usize) -> Option<u64> {
        let span = pages as u64 * PAGE;
        let limit = HEAP_ARENA_BASE + HEAP_ARENA_SIZE;

        for start in [self.cursor, HEAP_ARENA_BASE] {
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

    fn find(&self, base: u64) -> Option<usize> {
        self.regions.iter().position(|region| region.base == base)
    }
}

static ARENA: SpinLock<Arena> = SpinLock::new(Arena::new());

/// Translates a rights mask into a `(writable, executable)` pair.
///
/// W^X: a writable page never becomes executable, whatever the caller
/// asks. It is the same rule the loader applies to the sections of an
/// image, and it admits no exception -- allowing it here would make the
/// whole protection avoidable with one allocation.
fn page_rights(prot: u64) -> (bool, bool) {
    let writable = prot & PROT_WRITE != 0;
    let executable = prot & PROT_EXEC != 0 && !writable;
    (writable, executable)
}

// ============================================================
//  5.4.1 -- ALLOC / FREE / RESIZE
// ============================================================

/// `ALLOC(size, rights)` -- returns the base address.
///
/// The default rights, when the argument is zero, are read and write
/// without execute: it is what a heap expects.
fn alloc_region(call: &mut Call) -> SysResult<u64> {
    let size = call.arg(0);
    if size == 0 {
        return Err(error::invalid("taille nulle"));
    }
    if size > ALLOC_MAX {
        return Err(SysError::with(ErrorCode::TooLarge, "allocation"));
    }
    let prot = if call.arg(1) == 0 {
        PROT_READ | PROT_WRITE
    } else {
        call.arg(1)
    };
    if prot & !(PROT_READ | PROT_WRITE | PROT_EXEC) != 0 {
        return Err(error::invalid("droit inconnu"));
    }

    let pages = size.div_ceil(PAGE) as usize;
    let mut arena = ARENA.lock();
    let base = arena
        .find_slot(pages)
        .ok_or_else(|| SysError::with(ErrorCode::OutOfMemory, "arene utilisateur saturee"))?;

    // The frames are taken **then** mapped. A failure part way through
    // gives back everything already taken: a partial allocation left in
    // place would be invisible to the caller and lost to the system.
    let mut frames = Vec::with_capacity(pages);
    let (writable, executable) = page_rights(prot);

    for index in 0..pages {
        let Some(physical) = frame::allocate_zeroed_frame() else {
            rollback(&frames, base, index);
            return Err(SysError::with(ErrorCode::OutOfMemory, "plus de cadre libre"));
        };
        let virt = base + index as u64 * PAGE;
        if unsafe { memory::map_user_page(virt, physical.start_addr(), writable, executable) }
            .is_err()
        {
            unsafe { frame::deallocate_frame(physical) };
            rollback(&frames, base, index);
            return Err(SysError::with(
                ErrorCode::OutOfMemory,
                "table de pages indisponible",
            ));
        }
        frames.push(physical);
    }

    arena.allocated_pages += pages;
    arena.peak_pages = arena.peak_pages.max(arena.allocated_pages);
    arena.regions.push(Region {
        owner: call.pid,
        base,
        pages,
        frames,
        prot,
        locked: false,
        shared: Vec::new(),
    });

    Ok(base)
}

/// Gives back the frames and the mappings of an aborted allocation.
fn rollback(frames: &[Frame], base: u64, count: usize) {
    for index in 0..count {
        unsafe { memory::unmap_page(base + index as u64 * PAGE) };
    }
    for physical in frames.iter() {
        unsafe { frame::deallocate_frame(*physical) };
    }
}

/// Checks that the region really belongs to the caller.
fn owned(arena: &Arena, index: usize, call: &Call) -> SysResult<()> {
    let region = &arena.regions[index];
    if region.owner == call.pid || call.origin == super::Origin::Kernel {
        Ok(())
    } else {
        Err(error::denied("region d'une autre tache"))
    }
}

/// `FREE(base)`.
fn free_region(call: &mut Call) -> SysResult<u64> {
    let base = call.arg(0);
    let mut arena = ARENA.lock();
    let index = arena
        .find(base)
        .ok_or_else(|| error::missing("aucune region a cette adresse"))?;
    owned(&arena, index, call)?;

    let region = arena.regions.swap_remove(index);
    for offset in 0..region.pages {
        unsafe { memory::unmap_page(region.base + offset as u64 * PAGE) };
    }
    for physical in region.frames.iter() {
        unsafe { frame::deallocate_frame(*physical) };
    }
    arena.allocated_pages -= region.pages;
    Ok(region.pages as u64 * PAGE)
}

/// `RESIZE(base, new_size)` -- returns the base, possibly moved.
///
/// Growing happens in place when the following pages are free, and by
/// copying otherwise. Shrinking gives back the surplus pages. Returning
/// the base rather than nothing leaves the caller to update its pointers,
/// which it alone can do.
fn resize_region(call: &mut Call) -> SysResult<u64> {
    let base = call.arg(0);
    let size = call.arg(1);
    if size == 0 {
        return Err(error::invalid("taille nulle : utiliser FREE"));
    }
    if size > ALLOC_MAX {
        return Err(SysError::with(ErrorCode::TooLarge, "allocation"));
    }
    let wanted = size.div_ceil(PAGE) as usize;

    let (current_pages, prot) = {
        let arena = ARENA.lock();
        let index = arena
            .find(base)
            .ok_or_else(|| error::missing("aucune region a cette adresse"))?;
        owned(&arena, index, call)?;
        (arena.regions[index].pages, arena.regions[index].prot)
    };

    if wanted == current_pages {
        return Ok(base);
    }

    if wanted < current_pages {
        let mut arena = ARENA.lock();
        let index = arena.find(base).ok_or_else(|| error::missing("region"))?;
        let region = &mut arena.regions[index];
        for offset in wanted..region.pages {
            unsafe { memory::unmap_page(region.base + offset as u64 * PAGE) };
        }
        let released: Vec<Frame> = region.frames.split_off(wanted);
        region.pages = wanted;
        for physical in released.iter() {
            unsafe { frame::deallocate_frame(*physical) };
        }
        arena.allocated_pages -= current_pages - wanted;
        return Ok(base);
    }

    // Growing in place: possible only if the pages that follow belong to
    // nobody.
    let extend_in_place = {
        let arena = ARENA.lock();
        let tail = base + current_pages as u64 * PAGE;
        tail + ((wanted - current_pages) as u64 * PAGE) <= HEAP_ARENA_BASE + HEAP_ARENA_SIZE
            && !arena.overlaps(tail, wanted - current_pages)
    };

    if extend_in_place {
        let (writable, executable) = page_rights(prot);
        let mut added = Vec::new();
        for offset in current_pages..wanted {
            let Some(physical) = frame::allocate_zeroed_frame() else {
                rollback(&added, base + current_pages as u64 * PAGE, added.len());
                return Err(SysError::with(ErrorCode::OutOfMemory, "plus de cadre libre"));
            };
            let virt = base + offset as u64 * PAGE;
            if unsafe { memory::map_user_page(virt, physical.start_addr(), writable, executable) }
            .is_err()
        {
                unsafe { frame::deallocate_frame(physical) };
                rollback(&added, base + current_pages as u64 * PAGE, added.len());
                return Err(SysError::with(ErrorCode::OutOfMemory, "table de pages"));
            }
            added.push(physical);
        }

        let mut arena = ARENA.lock();
        let index = arena.find(base).ok_or_else(|| error::missing("region"))?;
        arena.regions[index].frames.extend(added);
        arena.regions[index].pages = wanted;
        arena.allocated_pages += wanted - current_pages;
        return Ok(base);
    }

    // Moving: allocate elsewhere, copy, give back the old one.
    let mut moved = Call {
        number: call.number,
        args: [size, prot, 0, 0, 0, 0],
        origin: call.origin,
        mode: call.mode,
        pid: call.pid,
    };
    let new_base = alloc_region(&mut moved)?;

    unsafe {
        core::ptr::copy_nonoverlapping(
            base as *const u8,
            new_base as *mut u8,
            current_pages * PAGE as usize,
        );
    }

    let mut release = Call {
        number: call.number,
        args: [base, 0, 0, 0, 0, 0],
        origin: call.origin,
        mode: call.mode,
        pid: call.pid,
    };
    free_region(&mut release)?;
    Ok(new_base)
}

// ============================================================
//  5.4.2 -- SHARE
// ============================================================

/// `SHARE(base, size, pid)` -- authorises and notifies.
///
/// See the module header: with a single address space, this call does not
/// open an access, it establishes the protocol for one.
fn share_region(call: &mut Call) -> SysResult<u64> {
    let base = call.arg(0);
    let target = call.arg(2);

    if !crate::scheduler::exists(target) {
        return Err(error::missing("PID destinataire"));
    }

    let (size, pages) = {
        let mut arena = ARENA.lock();
        let index = arena
            .find(base)
            .ok_or_else(|| error::missing("aucune region a cette adresse"))?;
        owned(&arena, index, call)?;
        let region = &mut arena.regions[index];
        if !region.shared.contains(&target) {
            region.shared.push(target);
        }
        (region.pages as u64 * PAGE, region.pages)
    };
    let _ = pages;

    // Notification: the address and the size, in the clear, in a message
    // of type `Data`. The recipient reads them as two 64-bit integers.
    let mut payload = [0u8; 16];
    payload[0..8].copy_from_slice(&base.to_le_bytes());
    payload[8..16].copy_from_slice(&size.to_le_bytes());

    let message = crate::ipc::Message::new(
        call.pid,
        crate::ipc::MessageKind::Data,
        SYS_MEM_SHARE as u32,
        &payload,
    );
    crate::ipc::send(target, message).map_err(|e| error::from_ipc(e, "notification de partage"))?;

    Ok(size)
}

// ============================================================
//  5.4.3 and 5.4.4 -- PROTECT / LOCK
// ============================================================

/// `PROTECT(base, size, rights)`.
///
/// The size is ignored when it covers the whole region: the rights apply
/// per whole region, because splitting a region into two sets of rights
/// would demand cutting it, and therefore making `FREE` ambiguous about
/// what it frees.
fn protect_region(call: &mut Call) -> SysResult<u64> {
    let base = call.arg(0);
    let prot = call.arg(2);
    if prot & !(PROT_READ | PROT_WRITE | PROT_EXEC) != 0 {
        return Err(error::invalid("droit inconnu"));
    }
    if prot & PROT_READ == 0 {
        // The x86-64 hardware has no "writable but unreadable" page:
        // announcing that we did it would be lying.
        return Err(SysError::with(
            ErrorCode::Unsupported,
            "une page utilisateur est toujours lisible",
        ));
    }

    let mut arena = ARENA.lock();
    let index = arena
        .find(base)
        .ok_or_else(|| error::missing("aucune region a cette adresse"))?;
    owned(&arena, index, call)?;

    let (writable, executable) = page_rights(prot);

    let region = &mut arena.regions[index];
    for offset in 0..region.pages {
        let virt = region.base + offset as u64 * PAGE;
        unsafe { memory::protect_user_page(virt, writable, executable) }
            .map_err(|reason| SysError::with_two(ErrorCode::Corrupt, "PROTECT", reason))?;
    }
    region.prot = prot;
    Ok(0)
}

/// `LOCK(base, size, locked)`.
fn lock_region(call: &mut Call) -> SysResult<u64> {
    let base = call.arg(0);
    let locked = call.arg(2) != 0;

    let mut arena = ARENA.lock();
    let index = arena
        .find(base)
        .ok_or_else(|| error::missing("aucune region a cette adresse"))?;
    owned(&arena, index, call)?;
    arena.regions[index].locked = locked;
    Ok(0)
}

/// Layout of the state of the arena.
///
/// ```text
///   0x00  8  pages allocated
///   0x08  8  peak of pages allocated
///   0x10  8  live regions
///   0x18  8  regions of the caller
/// ```
pub const MEM_STAT_SIZE: usize = 0x20;

fn stat(call: &mut Call) -> SysResult<u64> {
    let arena = ARENA.lock();
    let mine = arena
        .regions
        .iter()
        .filter(|region| region.owner == call.pid)
        .count();

    let mut record = [0u8; MEM_STAT_SIZE];
    record[0x00..0x08].copy_from_slice(&(arena.allocated_pages as u64).to_le_bytes());
    record[0x08..0x10].copy_from_slice(&(arena.peak_pages as u64).to_le_bytes());
    record[0x10..0x18].copy_from_slice(&(arena.regions.len() as u64).to_le_bytes());
    record[0x18..0x20].copy_from_slice(&(mine as u64).to_le_bytes());
    drop(arena);

    call.write_bytes(call.arg(0), call.arg(1), &record)
}

// ============================================================
//  Cleanup
// ============================================================

/// Gives back every region of a dead task.
///
/// Called when a task is reaped: without that, a program allocating
/// without freeing would leave its pages tied up forever, and the count
/// of free frames would never come back to its original value -- the
/// invariant the self-test checks on every boot.
pub fn release_for(pid: Pid) -> usize {
    let released: Vec<Region> = {
        let mut arena = ARENA.lock();
        let mut taken = Vec::new();
        let mut index = 0;
        while index < arena.regions.len() {
            if arena.regions[index].owner == pid {
                taken.push(arena.regions.swap_remove(index));
            } else {
                index += 1;
            }
        }
        arena.allocated_pages -= taken.iter().map(|r| r.pages).sum::<usize>();
        taken
    };

    let mut pages = 0;
    for region in released.iter() {
        for offset in 0..region.pages {
            unsafe { memory::unmap_page(region.base + offset as u64 * PAGE) };
        }
        for physical in region.frames.iter() {
            unsafe { frame::deallocate_frame(*physical) };
        }
        pages += region.pages;
    }
    pages
}

/// Statistics: (pages allocated, peak, regions).
pub fn arena_stats() -> (usize, usize, usize) {
    let arena = ARENA.lock();
    (arena.allocated_pages, arena.peak_pages, arena.regions.len())
}

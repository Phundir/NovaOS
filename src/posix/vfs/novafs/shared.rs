//! Shared mappings of the files of the Nova volume (`MAP_SHARED`).
//!
//! ## Why
//!
//! A writable shared mapping is one memory with the file: what a process
//! writes through it, every other mapping of the page sees at once, and
//! so does `read`. SQLite's write-ahead log stands on exactly that -- its
//! `-shm` file is an index that every connection maps and updates -- and
//! the browser keeps its databases in the session's home, on this volume.
//! Without shared pages the mapping was refused (`EACCES`), which SQLite
//! reports as `SQLITE_IOERR_SHMMAP`: every new database of the profile
//! failed to open.
//!
//! ## How
//!
//! Each page of a file that some process maps shared and writable gets
//! one frame here, filled from the file on first use; every shared
//! mapping of that page maps that frame (`vm::Pager::shared_frame`). The
//! frame is then the truth about the page:
//!
//! * `write` copies its bytes into the frames of the pages it covers,
//!   after writing them to the disk (`write_through`);
//! * `read` takes the bytes of those pages from their frames
//!   (`read_overlay`), and a private mapping copies from them too;
//! * the frames reach the disk at `fsync` of the file and at `msync`
//!   (`write_back`).
//!
//! ## Limits
//!
//! Nothing writes the frames back on its own: bytes written through a
//! mapping and never synced last as long as the system runs. SQLite syncs
//! what it must keep and rebuilds its index when it is missing; a program
//! that counted on the kernel flushing its mapped pages would lose them
//! at shutdown. Frames are never freed, like the page cache's: a space
//! may still map one and the `vm` keeps no reverse map to know. A file
//! truncated or deleted drops its entries; the frames stay with whoever
//! maps them.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use super::{LETTER, PAGE};
use crate::vga::SpinLock;

/// Frame of each shared page, by (node, page of the file).
static SHARED: SpinLock<BTreeMap<(u64, u64), u64>> = SpinLock::new(BTreeMap::new());

/// Pages held, for the statistics line.
pub fn count() -> usize {
    SHARED.lock().len()
}

/// Does any page of node `id` have a shared frame?
pub fn has_any(id: u64) -> bool {
    SHARED.lock().range((id, 0)..(id + 1, 0)).next().is_some()
}

/// The shared frame of page `page` of the node, if it has one.
pub fn lookup(id: u64, page: u64) -> Option<u64> {
    SHARED.lock().get(&(id, page)).copied()
}

/// The shared frame of page `page` of a file of `size` bytes, made and
/// filled from the disk if it has none yet. What lies past the end of
/// the file reads as zeros: a shared mapping may reach past it (the
/// file is grown with `ftruncate`, then mapped whole).
pub fn frame(id: u64, size: u64, page: u64) -> Option<u64> {
    if let Some(phys) = lookup(id, page) {
        return Some(phys);
    }
    let frame = crate::frame::allocate_zeroed_frame()?;
    let start = page * PAGE;
    if start < size {
        let count = (size - start).min(PAGE) as usize;
        // SAFETY: a frame just allocated, 4 KiB, mapped nowhere yet.
        let bytes = unsafe { core::slice::from_raw_parts_mut(frame.virt_addr() as *mut u8, count) };
        if crate::fs::read_range_node(LETTER, id, start, bytes).is_err() {
            // SAFETY: allocated above, never mapped.
            unsafe { crate::frame::deallocate_frame(frame) };
            return None;
        }
    }
    let mut shared = SHARED.lock();
    if let Some(phys) = shared.get(&(id, page)) {
        // Another mapping made it while this one read the disk.
        let phys = *phys;
        drop(shared);
        // SAFETY: allocated above, never mapped.
        unsafe { crate::frame::deallocate_frame(frame) };
        return Some(phys);
    }
    shared.insert((id, page), frame.start_addr());
    Some(frame.start_addr())
}

/// The frames of node `id` whose pages meet [offset, offset + len), with
/// their page numbers.
fn frames_in(id: u64, offset: u64, len: u64) -> Vec<(u64, u64)> {
    if len == 0 {
        return Vec::new();
    }
    let first = offset / PAGE;
    let last = (offset + len - 1) / PAGE;
    SHARED.lock().range((id, first)..=(id, last)).map(|(&(_, page), &phys)| (page, phys)).collect()
}

/// `data`, just written at `offset` of the file, goes into the shared
/// frames it covers: the mappings see what `write` wrote.
pub fn write_through(id: u64, offset: u64, data: &[u8]) {
    for (page, phys) in frames_in(id, offset, data.len() as u64) {
        let page_start = page * PAGE;
        let from = offset.max(page_start);
        let to = (offset + data.len() as u64).min(page_start + PAGE);
        let target = crate::memory::phys_to_virt(phys) as *mut u8;
        // SAFETY: a shared frame, never freed; the range stays inside
        // its 4 KiB and inside `data`.
        unsafe {
            core::ptr::copy_nonoverlapping(
                data[(from - offset) as usize..].as_ptr(),
                target.add((from - page_start) as usize),
                (to - from) as usize,
            );
        }
    }
}

/// `buf` holds the file's bytes from `offset`, read from the disk: the
/// pages that have a shared frame are taken from it instead, since what
/// was written through a mapping has not reached the disk.
pub fn read_overlay(id: u64, offset: u64, buf: &mut [u8]) {
    for (page, phys) in frames_in(id, offset, buf.len() as u64) {
        let page_start = page * PAGE;
        let from = offset.max(page_start);
        let to = (offset + buf.len() as u64).min(page_start + PAGE);
        let source = crate::memory::phys_to_virt(phys) as *const u8;
        // SAFETY: a shared frame, never freed; the range stays inside
        // its 4 KiB and inside `buf`.
        unsafe {
            core::ptr::copy_nonoverlapping(
                source.add((from - page_start) as usize),
                buf[(from - offset) as usize..].as_mut_ptr(),
                (to - from) as usize,
            );
        }
    }
}

/// Writes the shared frames of a file of `size` bytes to the disk
/// (`fsync`, `msync`). Only the bytes inside the file: a frame reaching
/// past its end does not grow it.
pub fn write_back(id: u64, size: u64) -> Result<(), crate::fs::FsError> {
    let now = crate::idt::ticks();
    for (page, phys) in frames_in(id, 0, size) {
        let start = page * PAGE;
        let count = (size - start).min(PAGE) as usize;
        // SAFETY: a shared frame, never freed; `count` fits in it.
        let bytes = unsafe { core::slice::from_raw_parts(crate::memory::phys_to_virt(phys) as *const u8, count) };
        crate::fs::write_range_node(LETTER, id, start, bytes, now)?;
    }
    Ok(())
}

/// The file now ends at `size`: the pages past it lose their entries,
/// and the bytes past the end of the last one read as zeros again.
pub fn truncate(id: u64, size: u64) {
    let mut shared = SHARED.lock();
    let first_gone = size.div_ceil(PAGE);
    let gone: Vec<(u64, u64)> = shared.range((id, first_gone)..(id + 1, 0)).map(|(k, _)| *k).collect();
    for key in gone {
        shared.remove(&key);
    }
    if size % PAGE != 0
        && let Some(&phys) = shared.get(&(id, size / PAGE))
    {
        let keep = (size % PAGE) as usize;
        let at = crate::memory::phys_to_virt(phys) as *mut u8;
        // SAFETY: a shared frame, never freed; the range is its tail.
        unsafe { core::ptr::write_bytes(at.add(keep), 0, PAGE as usize - keep) };
    }
}

/// The file is gone: its pages lose their entries.
pub fn forget(id: u64) {
    truncate(id, 0);
}

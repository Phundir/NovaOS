//! The shared memory region of virtio-gpu: where host memory shows up in
//! the guest.
//!
//! ## What the region is
//!
//! `hostmem=8G` on QEMU's command line gives the device a 64-bit BAR of
//! that size, described by a `SHARED_MEMORY_CFG` capability. It is not
//! video memory -- the guest has none under virtio -- but a window: when
//! the driver asks `RESOURCE_MAP_BLOB(resource, offset)`, the host maps
//! the blob's memory (a Vulkan allocation of the host's GPU driver, or a
//! shared-memory buffer) at that offset of the BAR, and the guest reaches
//! it with ordinary loads and stores. That is how a program writes vertex
//! data the GPU reads, and reads back an image the GPU drew, with no copy.
//!
//! ## Who chooses the offsets
//!
//! The guest. The region is an address space the driver manages like any
//! other: this allocator hands out page-aligned ranges, first fit, and
//! takes them back when the blob is unmapped. A range is reused only once
//! nothing maps it any more -- the kernel window below, and every program
//! that mapped it (the GPU core unmaps those first).
//!
//! ## The kernel's view
//!
//! A range the kernel itself reads (the Venus reply stream, a read-back
//! buffer of the self-test) is mapped at `WINDOW + offset`: one fixed
//! virtual image of the whole region, of which only the pages in use are
//! present. The cache type is the one the host announces in the map
//! answer: write-back for a cached blob, write-combining through PAT
//! entry 4 when the host maps it write-combined, uncached otherwise.

use alloc::vec::Vec;

use crate::memory::{self, flags};

/// Virtual image of the region in the kernel: its own PML4 entry, 512
/// GiB, far above anything else the kernel maps (see ARCHITECTURE.md).
pub const WINDOW: u64 = 0xFFFF_8300_0000_0000;
/// Largest region the window can show.
pub const WINDOW_SIZE: u64 = 512 << 30;

const PAGE: u64 = 4096;

pub struct Hostmem {
    /// Physical address and length of the region.
    pub base: u64,
    pub length: u64,
    /// Free ranges, `(offset, length)`, sorted and never adjacent.
    free: Vec<(u64, u64)>,
}

impl Hostmem {
    pub fn new(base: u64, length: u64) -> Hostmem {
        let length = length.min(WINDOW_SIZE) & !(PAGE - 1);
        Hostmem { base, length, free: alloc::vec![(0, length)] }
    }

    /// Bytes not handed out.
    pub fn available(&self) -> u64 {
        self.free.iter().map(|(_, length)| length).sum()
    }

    /// A range of `size` bytes, rounded up to whole pages. `None` when no
    /// free range is large enough.
    pub fn allocate(&mut self, size: u64) -> Option<u64> {
        let size = size.checked_add(PAGE - 1)? & !(PAGE - 1);
        if size == 0 {
            return None;
        }
        let index = self.free.iter().position(|(_, length)| *length >= size)?;
        let (offset, length) = self.free[index];
        if length == size {
            self.free.remove(index);
        } else {
            self.free[index] = (offset + size, length - size);
        }
        Some(offset)
    }

    /// Takes back a range `allocate` handed out.
    pub fn release(&mut self, offset: u64, size: u64) {
        let size = (size + PAGE - 1) & !(PAGE - 1);
        if size == 0 || offset.checked_add(size).is_none_or(|end| end > self.length) {
            return;
        }
        let index = self.free.partition_point(|(start, _)| *start < offset);
        self.free.insert(index, (offset, size));
        // Merge with the next range, then with the previous one.
        if index + 1 < self.free.len() {
            let (start, length) = self.free[index];
            let (next, next_length) = self.free[index + 1];
            if start + length == next {
                self.free[index].1 += next_length;
                self.free.remove(index + 1);
            }
        }
        if index > 0 {
            let (previous, previous_length) = self.free[index - 1];
            let (start, length) = self.free[index];
            if previous + previous_length == start {
                self.free[index - 1].1 += length;
                self.free.remove(index);
            }
        }
    }
}

/// Page-table cache bits for a blob of the given `map_info` cache type.
pub fn cache_flags(cache: u32) -> u64 {
    match cache {
        super::proto::MAP_CACHE_CACHED => 0,
        super::proto::MAP_CACHE_WC if memory::pat_wc_available() => memory::PAT_4K,
        _ => flags::NO_CACHE | flags::WRITE_THROUGH,
    }
}

/// Maps `[offset, offset + size)` of the region, whose physical address
/// is `phys`, in the kernel window. Returns the virtual address of
/// `offset`.
///
/// # Safety
/// The range must be a blob the host has mapped there, and must stay so
/// until `unmap_kernel`.
pub unsafe fn map_kernel(phys: u64, offset: u64, size: u64, cache: u32) -> Option<u64> {
    let pages = size.div_ceil(PAGE);
    if offset.checked_add(pages * PAGE).is_none_or(|end| end > WINDOW_SIZE) {
        return None;
    }
    let mut page_flags = flags::PRESENT | flags::WRITABLE | cache_flags(cache);
    if memory::nx_enabled() {
        page_flags |= flags::NO_EXECUTE;
    }
    for page in 0..pages {
        let at = page * PAGE;
        if unsafe { memory::map_page(WINDOW + offset + at, phys + at, page_flags) }.is_err() {
            unsafe { unmap_kernel(offset, at) };
            return None;
        }
    }
    Some(WINDOW + offset)
}

/// Removes a kernel mapping `map_kernel` made; every core forgets it.
///
/// # Safety
/// Nothing may read or write the range any more.
pub unsafe fn unmap_kernel(offset: u64, size: u64) {
    for page in 0..size.div_ceil(PAGE) {
        unsafe { memory::unmap_page(WINDOW + offset + page * PAGE) };
    }
}

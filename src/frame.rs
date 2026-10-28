//! Physical frame allocator -- bitmap.
//!
//! A frame is a 4 KiB physical page. The bitmap reserves one bit per
//! frame: 1 = taken, 0 = free. For 512 MiB of RAM that is 16 KiB of
//! bitmap -- negligible, and the search stays fast because 64-bit words
//! are scanned in one go.
//!
//! Bitmap chosen over a free list: it allows freeing an arbitrary frame
//! in O(1) and, above all, *marking* whole regions as reserved, which is
//! what is needed to protect the kernel and the firmware.

use crate::memory::{PHYS_OFFSET, phys_to_virt};
use crate::multiboot::MemoryMap;
use crate::vga::SpinLock;

pub const FRAME_SIZE: u64 = 4096;

/// Cap on the RAM managed: 64 GiB. The matching bitmap occupies 2 MiB,
/// but only the portion really covered is consulted.
const MAX_FRAMES: usize = 16 * 1024 * 1024;
const BITMAP_WORDS: usize = MAX_FRAMES / 64;

/// Identifies a physical page by its ordinal number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Frame(pub u64);

impl Frame {
    /// Frame containing the given physical address.
    pub const fn containing(addr: u64) -> Self {
        Frame(addr / FRAME_SIZE)
    }

    pub const fn start_addr(&self) -> u64 {
        self.0 * FRAME_SIZE
    }

    /// Matching virtual address through the physical window.
    pub const fn virt_addr(&self) -> u64 {
        phys_to_virt(self.start_addr())
    }
}

pub struct BitmapAllocator {
    bitmap: &'static mut [u64],
    /// Number of frames actually covered by the detected RAM.
    total_frames: usize,
    used_frames: usize,
    /// Resume point of the scan: avoids starting from zero on every
    /// allocation.
    next_hint: usize,
}

/// 2 MiB of bitmap reserved statically in .bss. Avoiding a dynamic
/// allocation here solves the chicken-and-egg problem: it would take an
/// allocator to allocate the allocator's bitmap.
static mut BITMAP_STORAGE: [u64; BITMAP_WORDS] = [0; BITMAP_WORDS];

impl BitmapAllocator {
    const fn empty() -> Self {
        BitmapAllocator {
            bitmap: &mut [],
            total_frames: 0,
            used_frames: 0,
            next_hint: 0,
        }
    }

    fn set_used(&mut self, index: usize) {
        if index >= self.total_frames {
            return;
        }
        let word = index / 64;
        let bit = index % 64;
        if self.bitmap[word] & (1 << bit) == 0 {
            self.bitmap[word] |= 1 << bit;
            self.used_frames += 1;
        }
    }

    fn set_free(&mut self, index: usize) {
        if index >= self.total_frames {
            return;
        }
        let word = index / 64;
        let bit = index % 64;
        if self.bitmap[word] & (1 << bit) != 0 {
            self.bitmap[word] &= !(1 << bit);
            self.used_frames -= 1;
        }
    }

    fn is_used(&self, index: usize) -> bool {
        if index >= self.total_frames {
            return true;
        }
        self.bitmap[index / 64] & (1 << (index % 64)) != 0
    }

    /// Marks as taken every frame intersecting the given physical range.
    fn mark_range_used(&mut self, start: u64, end: u64) {
        let first = (start / FRAME_SIZE) as usize;
        let last = ((end + FRAME_SIZE - 1) / FRAME_SIZE) as usize;
        for index in first..last {
            self.set_used(index);
        }
    }

    fn mark_range_free(&mut self, start: u64, end: u64) {
        // Conservative rounding: a frame is freed only if it is entirely
        // contained in the available region.
        let first = ((start + FRAME_SIZE - 1) / FRAME_SIZE) as usize;
        let last = (end / FRAME_SIZE) as usize;
        for index in first..last {
            self.set_free(index);
        }
    }

    /// Removes a free frame from the pool.
    pub fn allocate(&mut self) -> Option<Frame> {
        // First pass from the remembered index, then resume at the start.
        if let Some(frame) = self.scan_from(self.next_hint) {
            return Some(frame);
        }
        self.scan_from(0)
    }

    fn scan_from(&mut self, start_word: usize) -> Option<Frame> {
        let word_count = (self.total_frames + 63) / 64;

        for word in start_word..word_count {
            // A word entirely at 1 contains no free frame.
            if self.bitmap[word] == u64::MAX {
                continue;
            }
            let bit = (!self.bitmap[word]).trailing_zeros() as usize;
            let index = word * 64 + bit;

            if index >= self.total_frames {
                break;
            }

            self.set_used(index);
            self.next_hint = word;
            return Some(Frame(index as u64));
        }
        None
    }

    /// Removes `count` physically consecutive free frames from the pool,
    /// the first of them returned. For what must be contiguous in the
    /// physical window without a page table of its own: a task's kernel
    /// stack (`process::KernelStack`). Linear in the bitmap, skipping
    /// full words: called at a task's creation, never on a hot path.
    pub fn allocate_contiguous(&mut self, count: usize) -> Option<Frame> {
        if count == 0 {
            return None;
        }
        let mut run_start = 0usize;
        let mut run = 0usize;
        let mut index = 0usize;
        while index < self.total_frames {
            let word = index / 64;
            if index % 64 == 0 && self.bitmap[word] == u64::MAX {
                run = 0;
                index += 64;
                continue;
            }
            if self.bitmap[word] & (1 << (index % 64)) == 0 {
                if run == 0 {
                    run_start = index;
                }
                run += 1;
                if run == count {
                    for taken in run_start..run_start + count {
                        self.set_used(taken);
                    }
                    return Some(Frame(run_start as u64));
                }
            } else {
                run = 0;
            }
            index += 1;
        }
        None
    }

    /// Allocates a frame and zeroes it -- indispensable for page tables,
    /// whose residual entries would be interpreted.
    pub fn allocate_zeroed(&mut self) -> Option<Frame> {
        let frame = self.allocate()?;
        unsafe {
            core::ptr::write_bytes(frame.virt_addr() as *mut u8, 0, FRAME_SIZE as usize);
        }
        Some(frame)
    }

    /// Returns a frame to the pool.
    ///
    /// # Safety
    /// The frame must no longer be referenced by any page table.
    pub unsafe fn deallocate(&mut self, frame: Frame) {
        let index = frame.0 as usize;
        if index < self.next_hint * 64 {
            self.next_hint = index / 64;
        }
        self.set_free(index);
    }

    pub fn total_frames(&self) -> usize {
        self.total_frames
    }

    pub fn used_frames(&self) -> usize {
        self.used_frames
    }

    pub fn free_frames(&self) -> usize {
        self.total_frames - self.used_frames
    }

    pub fn used_bytes(&self) -> u64 {
        self.used_frames as u64 * FRAME_SIZE
    }

    pub fn free_bytes(&self) -> u64 {
        self.free_frames() as u64 * FRAME_SIZE
    }
}

pub static FRAME_ALLOCATOR: SpinLock<BitmapAllocator> = SpinLock::new(BitmapAllocator::empty());

unsafe extern "C" {
    static __boot_start: u8;
    static __kernel_end: u8;
}

/// Builds the bitmap from the firmware memory map.
///
/// Strategy: everything is taken to start with, the regions declared
/// available are freed, then what must be reserved is reserved again. The
/// order matters -- the reverse would leave the kernel allocatable.
///
/// # Safety
/// To be called once only, with the PHYS_OFFSET window active.
pub unsafe fn init(map: &MemoryMap, multiboot_phys: u64, multiboot_size: u32) {

    // Region 6 of the memory map reaches up to 0x10000000000 (1 TiB): that
    // is PCI MMIO, not RAM. We cap on the highest region really available
    // so as not to size the bitmap on it.
    let highest_usable = map
        .iter()
        .filter(|a| a.is_usable())
        .map(|a| a.end_addr())
        .max()
        .unwrap_or(0);

    // RAM above 4 GiB is only usable once the physical window reaches it:
    // every frame handed out is zeroed and filled through `phys_to_virt`.
    // Whatever the window cannot cover is not managed at all.
    let window_end = unsafe { crate::memory::extend_window(highest_usable) };
    let frames = ((highest_usable.min(window_end) / FRAME_SIZE) as usize).min(MAX_FRAMES);

    let mut alloc = FRAME_ALLOCATOR.lock();
    alloc.bitmap = unsafe { &mut *(&raw mut BITMAP_STORAGE) };
    alloc.total_frames = frames;

    // 1. Everything taken.
    let words = (frames + 63) / 64;
    for word in 0..words {
        alloc.bitmap[word] = u64::MAX;
    }
    alloc.used_frames = frames;

    // 2. Freeing of the available regions.
    for area in map.iter() {
        if area.is_usable() {
            alloc.mark_range_free(area.base_addr, area.end_addr());
        }
    }

    // 3. Reservations.

    // The whole of low memory: IVT, BDA, EBDA, the SMP trampoline area
    // (0x8000) and any ACPI tables. We lose 1 MiB, which is of no
    // consequence and avoids every collision.
    alloc.mark_range_used(0, 0x100000);

    // The kernel itself, from .boot to the end of .bss.
    let kernel_start = &raw const __boot_start as u64;
    let kernel_end = &raw const __kernel_end as u64;
    // __kernel_end is a higher-half virtual address, __boot_start a
    // physical one: both are brought back to physical.
    let kernel_end_phys = if kernel_end >= crate::memory::KERNEL_VMA {
        kernel_end - crate::memory::KERNEL_VMA
    } else {
        kernel_end
    };
    alloc.mark_range_used(kernel_start, kernel_end_phys);

    // The Multiboot2 structure: it is read back after init, it must survive.
    if multiboot_phys != 0 {
        alloc.mark_range_used(multiboot_phys, multiboot_phys + multiboot_size as u64);
    }

    // VGA area and BIOS ROM: 0xA0000 to 0x100000.

    alloc.next_hint = 0;
}

/// Allocates a frame from the global pool.
pub fn allocate_frame() -> Option<Frame> {
    FRAME_ALLOCATOR.lock().allocate()
}

/// Allocates a zeroed frame.
pub fn allocate_zeroed_frame() -> Option<Frame> {
    FRAME_ALLOCATOR.lock().allocate_zeroed()
}

/// Allocates `count` physically consecutive frames; the first is returned.
pub fn allocate_contiguous(count: usize) -> Option<Frame> {
    FRAME_ALLOCATOR.lock().allocate_contiguous(count)
}

/// Frees a frame.
///
/// # Safety
/// The frame must no longer be mapped anywhere.
pub unsafe fn deallocate_frame(frame: Frame) {
    unsafe { FRAME_ALLOCATOR.lock().deallocate(frame) }
}

/// Instant statistics: (total, used, free).
pub fn stats() -> (usize, usize, usize) {
    let alloc = FRAME_ALLOCATOR.lock();
    (
        alloc.total_frames(),
        alloc.used_frames(),
        alloc.free_frames(),
    )
}

/// Logs the state of the allocator and checks that the kernel is protected.
pub fn dump_state() {
    let (total, used, free) = stats();
    crate::serial_println!(
        "[frame] {} frames de 4 Kio - {} utilisees ({} Kio) / {} libres ({} Mio)",
        total,
        used,
        used as u64 * FRAME_SIZE / 1024,
        free,
        free as u64 * FRAME_SIZE / (1024 * 1024)
    );

    let kernel_start = &raw const __boot_start as u64;
    let kernel_end = &raw const __kernel_end as u64;
    let kernel_end_phys = if kernel_end >= crate::memory::KERNEL_VMA {
        kernel_end - crate::memory::KERNEL_VMA
    } else {
        kernel_end
    };
    crate::serial_println!(
        "[frame] kernel physique 0x{:x}-0x{:x} ({} Kio) - reserve",
        kernel_start,
        kernel_end_phys,
        (kernel_end_phys - kernel_start) / 1024
    );

    let alloc = FRAME_ALLOCATOR.lock();
    let k_first = (kernel_start / FRAME_SIZE) as usize;
    let k_last = ((kernel_end_phys - 1) / FRAME_SIZE) as usize;
    let protected = alloc.is_used(k_first) && alloc.is_used(k_last) && alloc.is_used(0);
    crate::serial_println!(
        "[frame] verification protection kernel : {}",
        if protected { "OK" } else { "ECHEC" }
    );
    crate::serial_println!(
        "[frame] PHYS_OFFSET = 0x{:016x} - fenetre physique de {} Gio",
        PHYS_OFFSET,
        crate::memory::window_end() >> 30
    );
}

//! Kernel heap -- linked-list allocator.
//!
//! 2.3.2.1 -- Three strategies were conceivable:
//!   - bump      : trivial, but never frees anything individually;
//!   - buddy     : excellent against fragmentation, markedly more complex;
//!   - free-list : frees correctly, merges adjacent blocks, and fits in
//!                 200 lines.
//!
//! The free list is the one kept: it is amply enough for the needs of the
//! kernel, which allocates little and in large blocks. A buddy allocator
//! can replace it if the fragmentation becomes measurable.
//!
//! ## Quick lists for small blocks
//!
//! The interpreters (JavaScript, Python) broke the "little and in large
//! blocks" premise: a Python program allocates and frees millions of
//! 16 to 200 byte objects, and the free list, fragmented into thousands of
//! holes, made every one of them a walk of that list -- a sorting test
//! that takes 0.8 s on the host took 87 s in the kernel.
//!
//! So a block of at most `QUICK_MAX` bytes (alignment 16 at most) is
//! rounded up to a multiple of 16 and, once freed, kept on a list of its
//! size class instead of returning to the main list: the next request of
//! that class takes it back in O(1), without walking anything. Blocks of a
//! class are carved from the main list at alignment 16, so any of them
//! fits any request of the class.
//!
//! What the quick lists hold is free memory the main list does not see.
//! Two things keep it from being lost to the large requests: the lists
//! keep at most `QUICK_CAP` bytes (beyond, a freed block goes back to the
//! main list as before), and a request the main list cannot satisfy first
//! hands every quick block back to it -- with merging -- and tries again.

use core::alloc::{GlobalAlloc, Layout};
use core::mem::{align_of, size_of};
use core::ptr;

use crate::frame::{self, FRAME_SIZE};
use crate::memory::{self, PageTable, flags};
use crate::vga::SpinLock;

/// 2.3.2.2 -- Virtual region dedicated to the heap.
///
/// Chosen in a hole of the address space, away from the physical window
/// (PML4[256]) as from the kernel (PML4[511]): it occupies PML4[257].
pub const HEAP_START: u64 = 0xFFFF_8080_0000_0000;
/// 64 MiB.
///
/// 4 MiB was enough as long as a task stack was 32 KiB. Since
/// `process::STACK_SIZE` became 128 -- see the reason there -- the forty
/// or so tasks of the system claim close to 5 MiB on their own, and the
/// heap still has to house the filesystem, the compositor surfaces and
/// the loader images: 16 MiB.
///
/// Then came the browser. A web page is its HTML, its tree, a style per
/// element and two pixel canvases the size of the window: 20 to 30 MiB
/// for a large page, with a tab or two more in memory. Every task shares
/// this one heap, so it grew to 64 MiB -- an eighth of the 512 MiB QEMU
/// is given, mapped once at boot as before.
pub const HEAP_SIZE: u64 = 64 * 1024 * 1024;

// ============================================================
//  Linked list of free blocks
// ============================================================

struct FreeBlock {
    size: usize,
    next: Option<&'static mut FreeBlock>,
}

impl FreeBlock {
    const fn new(size: usize) -> Self {
        FreeBlock { size, next: None }
    }

    fn start_addr(&self) -> usize {
        self as *const Self as usize
    }

    fn end_addr(&self) -> usize {
        self.start_addr() + self.size
    }
}

/// Largest request served by the quick lists.
const QUICK_MAX: usize = 512;
/// Granularity (and alignment) of the quick size classes.
const QUICK_STEP: usize = 16;
/// Size classes: 16, 32, ... `QUICK_MAX`.
const QUICK_CLASSES: usize = QUICK_MAX / QUICK_STEP;
/// Most bytes the quick lists may keep away from the main list.
const QUICK_CAP: usize = 8 * 1024 * 1024;

pub struct LinkedListAllocator {
    head: FreeBlock,
    /// Heads of the quick lists, one per size class: the first word of a
    /// free block is the address of the next one, 0 ends the list.
    quick: [usize; QUICK_CLASSES],
    /// Bytes held by the quick lists.
    quick_bytes: usize,
    allocated: usize,
    allocations: usize,
    deallocations: usize,
}

impl LinkedListAllocator {
    pub const fn new() -> Self {
        LinkedListAllocator {
            head: FreeBlock::new(0),
            quick: [0; QUICK_CLASSES],
            quick_bytes: 0,
            allocated: 0,
            allocations: 0,
            deallocations: 0,
        }
    }

    /// # Safety
    /// The region must be mapped, unused, and this function called once
    /// only.
    unsafe fn init(&mut self, start: usize, size: usize) {
        unsafe { self.push_free(start, size) };
    }

    /// Inserts a free block keeping the list sorted by increasing address,
    /// and merges immediately with the contiguous neighbours.
    unsafe fn push_free(&mut self, addr: usize, size: usize) {
        assert_eq!(addr % align_of::<FreeBlock>(), 0);
        if size < size_of::<FreeBlock>() {
            // Too small to host a node: the fragment is abandoned.
            return;
        }

        // Search for the insertion point: last node of lower address.
        let mut current = &mut self.head;
        while current
            .next
            .as_ref()
            .is_some_and(|n| n.start_addr() < addr)
        {
            current = current.next.as_mut().unwrap();
        }

        // Backward merge: does the block directly extend `current`? The
        // head has zero size, it never merges.
        if current.size > 0 && current.end_addr() == addr {
            current.size += size;
            Self::merge_with_next(current);
            return;
        }

        // Insertion of a new node.
        let node_ptr = addr as *mut FreeBlock;
        let mut node = FreeBlock::new(size);
        node.next = current.next.take();
        unsafe {
            node_ptr.write(node);
            current.next = Some(&mut *node_ptr);
        }

        // Forward merge: does the new node touch its successor?
        Self::merge_with_next(current.next.as_mut().unwrap());
    }

    /// Absorbs the successor if it is immediately adjacent.
    fn merge_with_next(block: &mut FreeBlock) {
        if block
            .next
            .as_ref()
            .is_some_and(|n| block.end_addr() == n.start_addr())
        {
            let next = block.next.take().unwrap();
            block.size += next.size;
            block.next = next.next.take();
        }
    }

    /// Looks for a block able to host the request.
    fn find_block(&mut self, size: usize, align: usize) -> Option<(&'static mut FreeBlock, usize)> {
        let mut current = &mut self.head;

        while current.next.is_some() {
            let candidate = current.next.as_mut().unwrap();
            if let Ok(start) = Self::split_check(candidate, size, align) {
                let next = candidate.next.take();
                let result = current.next.take().unwrap();
                current.next = next;
                return Some((result, start));
            }
            current = current.next.as_mut().unwrap();
        }
        None
    }

    /// Checks that a block can satisfy the request once aligned, and that
    /// any remainder stays usable.
    fn split_check(block: &FreeBlock, size: usize, align: usize) -> Result<usize, ()> {
        let mut start = align_up(block.start_addr(), align);
        if start > block.start_addr() && start - block.start_addr() < size_of::<FreeBlock>() {
            // The hole in front would be too small to be reinserted: the
            // next aligned address leaves one that can be.
            start += align;
        }
        let end = start.checked_add(size).ok_or(())?;

        if end > block.end_addr() {
            return Err(());
        }

        let excess = block.end_addr() - end;
        if excess > 0 && excess < size_of::<FreeBlock>() {
            // The rest would be too small to be reinserted: we refuse
            // rather than lose it.
            return Err(());
        }

        Ok(start)
    }

    /// Takes a block of size `size` and alignment `align` from the main
    /// list, handing the remainders back to it. Null if none fits.
    fn take(&mut self, size: usize, align: usize) -> *mut u8 {
        let Some((block, start)) = self.find_block(size, align) else {
            return ptr::null_mut();
        };
        // The bounds of the block are read before any reinsertion:
        // `push_free` writes a node at the address it is given, which may
        // overwrite the node `block` still names.
        let block_start = block.start_addr();
        let block_end = block.end_addr();
        let end = start + size;

        // Tail remainder.
        if block_end > end {
            unsafe { self.push_free(end, block_end - end) };
        }
        // Head remainder: the requested alignment may leave a hole in
        // front of the area returned. Without this reinsertion it would be
        // lost for good on every strongly aligned allocation -- a slow but
        // real leak.
        if start > block_start {
            unsafe { self.push_free(block_start, start - block_start) };
        }
        start as *mut u8
    }

    /// Hands every block of the quick lists back to the main list, where
    /// they merge with their neighbours.
    fn flush_quick(&mut self) {
        for class in 0..QUICK_CLASSES {
            let size = (class + 1) * QUICK_STEP;
            while self.quick[class] != 0 {
                let addr = self.quick[class];
                // SAFETY: a quick block is free memory of the heap whose
                // first word was written by `dealloc`.
                self.quick[class] = unsafe { (addr as *const usize).read() };
                unsafe { self.push_free(addr, size) };
            }
        }
        self.quick_bytes = 0;
    }

    /// The quick class of a normalised request, if it has one.
    fn quick_class(size: usize, align: usize) -> Option<usize> {
        (size <= QUICK_MAX && align <= QUICK_STEP).then(|| size.div_ceil(QUICK_STEP) - 1)
    }

    pub fn allocated_bytes(&self) -> usize {
        self.allocated
    }

    pub fn allocation_count(&self) -> usize {
        self.allocations
    }

    pub fn deallocation_count(&self) -> usize {
        self.deallocations
    }

    /// Free bytes in all, and the largest single free block.
    pub fn free_and_largest(&self) -> (usize, usize) {
        let mut total = self.quick_bytes;
        let mut largest = 0;
        let mut current = &self.head;
        while let Some(ref next) = current.next {
            total += next.size;
            largest = largest.max(next.size);
            current = next;
        }
        (total, largest)
    }

    /// Sum of the blocks still free.
    pub fn free_bytes(&self) -> usize {
        let mut total = self.quick_bytes;
        let mut current = &self.head;
        while let Some(ref next) = current.next {
            total += next.size;
            current = next;
        }
        total
    }
}

/// Rounds up to an alignment boundary (a power of 2).
fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

/// Normalises a request: size and alignment at least equal to those of a
/// list node, failing which the block could not be recycled.
fn adjust_layout(layout: Layout) -> (usize, usize) {
    let layout = layout
        .align_to(align_of::<FreeBlock>())
        .expect("alignement impossible")
        .pad_to_align();
    (layout.size().max(size_of::<FreeBlock>()), layout.align())
}

// ============================================================
//  Global wrapper
// ============================================================

pub struct Heap(SpinLock<LinkedListAllocator>);

unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let (mut size, mut align) = adjust_layout(layout);
        let mut allocator = self.0.lock();
        let class = LinkedListAllocator::quick_class(size, align);
        if let Some(class) = class {
            size = (class + 1) * QUICK_STEP;
            align = QUICK_STEP;
            let head = allocator.quick[class];
            if head != 0 {
                // SAFETY: the first word of a quick block names the next.
                allocator.quick[class] = unsafe { (head as *const usize).read() };
                allocator.quick_bytes -= size;
                allocator.allocated += size;
                allocator.allocations += 1;
                return head as *mut u8;
            }
        }

        let mut block = allocator.take(size, align);
        if block.is_null() && allocator.quick_bytes > 0 {
            allocator.flush_quick();
            block = allocator.take(size, align);
        }
        if !block.is_null() {
            allocator.allocated += size;
            allocator.allocations += 1;
        }
        block
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let (mut size, align) = adjust_layout(layout);
        let mut allocator = self.0.lock();
        let class = LinkedListAllocator::quick_class(size, align);
        if let Some(class) = class {
            size = (class + 1) * QUICK_STEP;
        }
        allocator.allocated -= size;
        allocator.deallocations += 1;
        match class {
            Some(class) if allocator.quick_bytes + size <= QUICK_CAP => {
                // SAFETY: the block is the caller's no more; its first word
                // becomes the link.
                unsafe { (ptr as *mut usize).write(allocator.quick[class]) };
                allocator.quick[class] = ptr as usize;
                allocator.quick_bytes += size;
            }
            _ => unsafe { allocator.push_free(ptr as usize, size) },
        }
    }
}

#[global_allocator]
pub static ALLOCATOR: Heap = Heap(SpinLock::new(LinkedListAllocator::new()));

// ============================================================
//  Setup
// ============================================================

/// Inserts an entry into a table, allocating the child table if needed.
///
/// # Safety
/// The physical window must be active and the frame allocator initialised.
unsafe fn next_table_or_create(table: &mut PageTable, index: usize) -> Option<&'static mut PageTable> {
    if !table[index].is_present() {
        let frame = frame::allocate_zeroed_frame()?;
        table[index].set(
            frame.start_addr(),
            flags::PRESENT | flags::WRITABLE,
        );
    }
    let addr = memory::phys_to_virt(table[index].addr());
    Some(unsafe { &mut *(addr as *mut PageTable) })
}

/// Maps the heap region in 4 KiB pages, then initialises the allocator.
///
/// # Safety
/// Requires an initialised frame allocator and the PHYS_OFFSET window active.
pub unsafe fn init() -> Result<(), &'static str> {
    let page_count = HEAP_SIZE / FRAME_SIZE;

    for i in 0..page_count {
        let virt = HEAP_START + i * FRAME_SIZE;
        let idx = memory::table_indices(virt);

        let p4 = unsafe { memory::active_level4_table() };
        let p3 = unsafe { next_table_or_create(p4, idx[0]) }.ok_or("plus de frames (PDPT)")?;
        let p2 = unsafe { next_table_or_create(p3, idx[1]) }.ok_or("plus de frames (PD)")?;

        if p2[idx[2]].is_huge() {
            return Err("page de 2 Mio deja presente sur la region du heap");
        }
        let p1 = unsafe { next_table_or_create(p2, idx[2]) }.ok_or("plus de frames (PT)")?;

        let frame = frame::allocate_frame().ok_or("plus de frames (heap)")?;
        // NO_EXECUTE would be desirable, but requires EFER.NXE -- enabled
        // later, with the W^X protection.
        p1[idx[3]].set(frame.start_addr(), flags::PRESENT | flags::WRITABLE);
    }

    unsafe { memory::flush_tlb() };
    unsafe {
        ALLOCATOR
            .0
            .lock()
            .init(HEAP_START as usize, HEAP_SIZE as usize)
    };

    Ok(())
}

/// Statistics: (allocated, free, allocation count, free count).
pub fn stats() -> (usize, usize, usize, usize) {
    let allocator = ALLOCATOR.0.lock();
    (
        allocator.allocated_bytes(),
        allocator.free_bytes(),
        allocator.allocation_count(),
        allocator.deallocation_count(),
    )
}

/// What the heap keeps for the kernel when a script asks for memory.
const SCRIPT_RESERVE: usize = 16 * 1024 * 1024;

/// Can an allocation of `bytes` made on behalf of a script succeed and
/// leave the kernel its reserve?
///
/// The kernel allocator cannot fail gracefully (`alloc_error` halts the
/// machine), so the JavaScript engine asks before it builds a large
/// string or array, and throws a `RangeError` instead. The largest free
/// block must hold the request with half of it to spare: a vector that
/// grows moves to a new block before freeing the old.
///
/// The blocks the quick lists hold count as free but not toward the
/// largest block: when that is what falls short, they go back to the main
/// list -- which may merge them into a block large enough -- and the
/// answer is given again.
pub fn room_for(bytes: usize) -> bool {
    let mut allocator = ALLOCATOR.0.lock();
    let fits = |(free, largest): (usize, usize)| {
        free >= bytes.saturating_add(SCRIPT_RESERVE) && largest >= bytes.saturating_add(bytes / 2)
    };
    if fits(allocator.free_and_largest()) {
        return true;
    }
    if allocator.quick_bytes == 0 {
        return false;
    }
    allocator.flush_quick();
    fits(allocator.free_and_largest())
}

pub fn dump_state() {
    let (allocated, free, allocs, deallocs) = stats();
    crate::serial_println!(
        "[heap] region 0x{:016x}-0x{:016x} ({} Kio)",
        HEAP_START,
        HEAP_START + HEAP_SIZE,
        HEAP_SIZE / 1024
    );
    crate::serial_println!(
        "[heap] {} octets alloues / {} libres - {} allocations, {} liberations",
        allocated,
        free,
        allocs,
        deallocs
    );
}

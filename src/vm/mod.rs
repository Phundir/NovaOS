//! Private address spaces, demand paging, user copies (phase 11.3).
//!
//! ## What a `Space` is
//!
//! A PML4 of its own whose **upper half is the kernel's**: entries 256 to
//! 511 are copied from the boot PML4, so every kernel address -- code,
//! heap, physical window, framebuffer -- means the same thing in every
//! space. The lower half belongs to the process alone. Two processes can
//! map the same virtual address to different frames, and neither can
//! reach the other's pages: the isolation of spec 12.4 stops being a
//! convention for the tasks that get one.
//!
//! The copy of the upper half is taken once, so it must never change
//! afterwards. `init` therefore fills **every** upper PML4 entry at boot,
//! with an empty PDPT where the kernel had none. From then on the kernel
//! only ever edits tables below the PML4, which all spaces share.
//!
//! ## Regions and demand paging
//!
//! A space is described by its regions (`Vma`): a range, protections, and
//! where the bytes come from -- zero, or a `Pager` (a file). Mapping a
//! region allocates nothing. The first touch of a page faults, and
//! `resolve` allocates a frame, fills it and installs it. That is what
//! lets a program reserve gigabytes it will never use -- Firefox's
//! allocator and JavaScript engine do exactly that.
//!
//! A fault the regions do not explain ends the process (see
//! `idt::user_exception`), never the core: before this step, any ring 3
//! fault halted the processor it happened on.
//!
//! ## Touching user memory from the kernel
//!
//! Only `user_copy` (boot/fault.asm) dereferences user addresses. It runs
//! with no lock held, so a fault inside it is resolved like a user fault;
//! if it cannot be, the handler redirects it to a fixup that reports the
//! bytes left, and the system call returns an error instead of the kernel
//! faulting. A pointer check ahead of the copy could not be enough:
//! another thread may unmap the page between the check and the copy.
//!
//! ## Which tasks have a space
//!
//! Only those created through `spawn_user` -- the Linux processes of
//! phase 11. Nova images loaded by `binfmt::loader` still share the boot
//! PML4 and its arenas: moving them is a separate change, named in the
//! roadmap, and nothing in this module prevents it.

pub mod selftest;

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::frame::{self, Frame};
use crate::memory::{self, PageTable, flags, phys_to_virt};
use crate::vga::SpinLock;

pub const PAGE: u64 = 4096;

/// Lowest address a region may start at. The first 64 KiB stay unmapped
/// so that a null pointer, or a small offset from one, always faults --
/// Linux's `mmap_min_addr` by default.
pub const USER_MIN: u64 = 0x1_0000;

/// End of the lower half, exclusive: the last canonical user address + 1.
pub const USER_TOP: u64 = 0x0000_8000_0000_0000;

/// Software bit of a leaf entry: the frame is not owned by the space
/// (a page of a shared object) and must not be freed when unmapped.
const BORROWED: u64 = 1 << 9;

/// Software bit of a leaf entry (with BORROWED): the frame is a page of
/// a file's page cache, shared by every space that maps the file
/// privately. Never writable -- whatever the region says -- and copied
/// on the first write, like a COW frame whose other holders never go
/// away. Bit 52: ignored by the hardware, below the protection-key bits.
const CACHED: u64 = 1 << 52;

/// Physical address of the boot PML4, whose upper half every space
/// shares.
static KERNEL_PML4: AtomicU64 = AtomicU64::new(0);

/// Upper PML4 entries created by `init` because the kernel had none.
static UPPER_FILLED: AtomicUsize = AtomicUsize::new(0);

pub fn kernel_pml4() -> u64 {
    KERNEL_PML4.load(Ordering::Relaxed)
}

/// Freezes the upper half of the address space.
///
/// # Safety
/// To be called once, on the bootstrap core, after the frame allocator
/// and before any `Space` is created or any secondary core is started.
pub unsafe fn init() {
    let cr3 = memory::read_cr3() & !0xFFF;
    KERNEL_PML4.store(cr3, Ordering::Relaxed);
    let p4 = unsafe { &mut *(phys_to_virt(cr3) as *mut PageTable) };
    let mut created = 0;
    for index in 256..512 {
        if !p4[index].is_present() {
            let frame = frame::allocate_zeroed_frame().expect("plus de cadre pour la moitie haute");
            p4[index].set(frame.start_addr(), flags::PRESENT | flags::WRITABLE);
            created += 1;
        }
    }
    UPPER_FILLED.store(created, Ordering::Relaxed);
}

pub fn upper_entries_created() -> usize {
    UPPER_FILLED.load(Ordering::Relaxed)
}

// ============================================================
//  Protections and regions
// ============================================================

/// Region protections, as `mmap` names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prot(pub u8);

impl Prot {
    pub const NONE: Prot = Prot(0);
    pub const READ: u8 = 1;
    pub const WRITE: u8 = 2;
    pub const EXEC: u8 = 4;
    pub const RW: Prot = Prot(1 | 2);
    pub const RX: Prot = Prot(1 | 4);
    pub const R: Prot = Prot(1);

    pub fn readable(self) -> bool {
        self.0 & (Self::READ | Self::WRITE | Self::EXEC) != 0
    }
    pub fn writable(self) -> bool {
        self.0 & Self::WRITE != 0
    }
    pub fn executable(self) -> bool {
        self.0 & Self::EXEC != 0
    }

    /// Leaf entry flags. x86 cannot express write-only or execute-only:
    /// any accessible page is readable, as on Linux.
    fn entry_flags(self) -> u64 {
        let mut f = flags::PRESENT | flags::USER;
        if self.writable() {
            f |= flags::WRITABLE;
        }
        if !self.executable() && memory::nx_enabled() {
            f |= flags::NO_EXECUTE;
        }
        f
    }
}

/// Where the bytes of a file-backed region come from.
pub trait Pager: Send + Sync {
    /// Fills `page` with the 4 KiB found at byte `offset` of the object;
    /// what lies past its end reads as zero. False on an I/O error.
    fn fill(&self, offset: u64, page: &mut [u8]) -> bool;

    /// The frame every shared mapping of this page uses, if the object
    /// keeps its pages in memory (a memfd, a tmpfs file). `None` for an
    /// object that can only be copied from.
    fn shared_frame(&self, offset: u64) -> Option<u64> {
        let _ = offset;
        None
    }

    /// The page-cache frame holding the page at `offset`, if the object
    /// keeps one (`tarfs`). The frame lives as long as the kernel and
    /// is never written: a private mapping maps it read-only in place
    /// of a copy, and copies it on the first write.
    fn cached_frame(&self, offset: u64) -> Option<u64> {
        let _ = offset;
        None
    }

    /// Reads the page at `offset` from the object's backing store itself,
    /// bypassing any cache -- for `Space::verify_cached_pages`, which
    /// checks the cache against it. The default is `fill`.
    fn fill_uncached(&self, offset: u64, page: &mut [u8]) -> bool {
        self.fill(offset, page)
    }

    /// Is a read-only `MAP_SHARED` of this object shared for real? True
    /// for an object whose pages live in memory anyway (a memfd). An
    /// object that makes shared frames only when needed (a file of the
    /// Nova volume) says false while it has none: a read-only view then
    /// maps the page cache, as a private mapping does, instead of
    /// pinning a frame per page of a 40 MiB archive.
    fn shares_read_only(&self) -> bool {
        true
    }

    /// Writes what shared mappings changed back to the object (`msync`).
    /// False on an I/O error.
    fn sync(&self) -> bool {
        true
    }

    /// Cache bits for the pages of a shared mapping: 0 for memory, the
    /// write-combining PAT bit for a device window mapped so (phase 13).
    fn page_flags(&self) -> u64 {
        0
    }

    /// Does a `fork` child map this object too? False for GPU memory: its
    /// pages are the host's, lent for as long as the program holds the
    /// allocation, and a child keeping a view of them after the parent
    /// freed it would read whatever the range holds next (`MADV_DONTFORK`
    /// by construction).
    fn fork_shares(&self) -> bool {
        true
    }
}

#[derive(Clone)]
pub enum Backing {
    /// Zero-filled on first touch.
    Anon,
    /// Bytes from `pager`, starting at `offset` for the region's start.
    /// `shared`: pages come from `Pager::shared_frame`, writes are
    /// visible to every mapping (`MAP_SHARED`). Otherwise each page is a
    /// private copy (`MAP_PRIVATE`).
    File { pager: Arc<dyn Pager>, offset: u64, shared: bool },
}

#[derive(Clone)]
pub struct Vma {
    pub start: u64,
    pub end: u64,
    pub prot: Prot,
    pub backing: Backing,
    /// Short name for diagnostics: "[stack]", "libxul.so", ...
    pub name: alloc::string::String,
}

impl Vma {
    /// The same region cut down to [start, end).
    fn slice(&self, start: u64, end: u64) -> Vma {
        let mut v = self.clone();
        if let Backing::File { offset, .. } = &mut v.backing {
            *offset += start - self.start;
        }
        v.start = start;
        v.end = end;
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmError {
    /// Outside the user half, unaligned, or empty.
    Invalid,
    /// A `MAP_FIXED_NOREPLACE`-style request met an existing region.
    Exists,
    /// No free range large enough, or no frame left.
    NoMemory,
    /// Not mapped, or mapped without the requested access.
    Fault,
}

// ============================================================
//  The space
// ============================================================

struct Inner {
    /// Regions keyed by start address; never overlapping.
    vmas: BTreeMap<u64, Vma>,
    /// Frames owned by this space (leaf entries without BORROWED).
    resident: usize,
    /// Faults resolved since creation.
    faults: u64,
}

pub struct Space {
    pml4: u64,
    inner: SpinLock<Inner>,
    /// Set by `fork` from the moment its pages turn copy-on-write until
    /// every core has dropped their writable translations. A thread
    /// breaking copy-on-write meanwhile would copy a frame another core
    /// can still write through a stale entry -- and that write would be
    /// lost. Such a break waits for the flag to clear.
    forking: core::sync::atomic::AtomicBool,
    /// Invalidations of this space in flight: entries taken down, the
    /// shootdown not yet acknowledged by every core. Raised under the
    /// lock, lowered once `shootdown_and_wait` returns.
    ///
    /// While it is non-zero no fault installs a translation. Otherwise,
    /// between a `munmap` releasing its range and its shootdown reaching
    /// the other cores, a second thread could `mmap` the same addresses,
    /// fault a fresh frame in and write it, while a third core still
    /// read the *old* frame through its stale TLB entry -- zeros where
    /// the second thread had written a heap header. That was the Web
    /// Browser's random malloc crash (`meta->mem` read at 0x10) once KVM
    /// gave the processors real TLBs; TCG's software one hid it.
    invalidating: AtomicUsize,
}

/// Spaces alive, for the monitor and the self-test.
static LIVE: AtomicUsize = AtomicUsize::new(0);

pub fn live_spaces() -> usize {
    LIVE.load(Ordering::Relaxed)
}

impl Space {
    pub fn new() -> Option<Arc<Space>> {
        reap_graveyard();
        let root = frame::allocate_zeroed_frame()?;
        let kernel = unsafe { &*(phys_to_virt(kernel_pml4()) as *const PageTable) };
        let p4 = unsafe { &mut *(root.virt_addr() as *mut PageTable) };
        for index in 256..512 {
            p4[index] = kernel[index];
        }
        LIVE.fetch_add(1, Ordering::Relaxed);
        Some(Arc::new(Space {
            pml4: root.start_addr(),
            inner: SpinLock::new(Inner { vmas: BTreeMap::new(), resident: 0, faults: 0 }),
            forking: core::sync::atomic::AtomicBool::new(false),
            invalidating: AtomicUsize::new(0),
        }))
    }

    /// Value for CR3.
    pub fn cr3(&self) -> u64 {
        self.pml4
    }

    /// (regions, resident frames, faults resolved).
    pub fn stats(&self) -> (usize, usize, u64) {
        let inner = self.inner.lock();
        (inner.vmas.len(), inner.resident, inner.faults)
    }

    /// The objects behind the shared file mappings meeting [start, end),
    /// for `msync`. Returned, not synced here: writing them back reads
    /// the disk, which is not done under the region lock.
    pub fn shared_pagers(&self, start: u64, end: u64) -> Vec<Arc<dyn Pager>> {
        let inner = self.inner.lock();
        let mut pagers: Vec<Arc<dyn Pager>> = Vec::new();
        let first = inner.vmas.range(..=start).next_back().map_or(start, |(&at, _)| at);
        for (_, v) in inner.vmas.range(first..end) {
            if v.end <= start {
                continue;
            }
            if let Backing::File { pager, shared: true, .. } = &v.backing
                && !pagers.iter().any(|p| Arc::ptr_eq(p, pager))
            {
                pagers.push(pager.clone());
            }
        }
        pagers
    }

    /// The words above a program's stack pointer that point into code:
    /// likely return addresses, with the library each falls in -- what a
    /// fatal fault in a program without frame pointers can still say
    /// about how it got there. Page by page, only where the region lock
    /// finds the stack resident: printing faults nothing in.
    pub fn print_return_addresses(&self, rsp: u64) {
        let mut found = 0;
        let mut at = rsp & !7;
        let end = at.saturating_add(8192);
        while at < end && found < 16 {
            let (word, code) = {
                let inner = self.inner.lock();
                // SAFETY: the page table of this space, read under its lock.
                let leaf = unsafe { self.leaf(at & !(PAGE - 1)) };
                if leaf & flags::PRESENT == 0 {
                    break;
                }
                let phys = (leaf & ADDR) + (at & (PAGE - 1));
                // SAFETY: a present user frame, read through the physical
                // window; `at` is 8-aligned, so the word stays in the page.
                let word = unsafe { core::ptr::read_volatile(phys_to_virt(phys) as *const u64) };
                let code = inner.find(word).filter(|v| v.prot.executable()).map(|v| (v.start, v.name.clone()));
                (word, code)
            };
            if let Some((start, name)) = code {
                crate::serial_println!(
                    "[vm]   pile +0x{:x} : 0x{:x} dans \"{}\" (region 0x{:x} + 0x{:x})",
                    at - rsp,
                    word,
                    name,
                    start,
                    word - start
                );
                found += 1;
            }
            at += 8;
        }
    }

    /// Compares every page-cache frame this space maps (`CACHED`) with
    /// the file it caches, read again from the disk; prints each page
    /// that differs, and a total.
    ///
    /// A cache frame is never written: a private mapping copies it on
    /// the first write, the kernel's own writes fault on it (CR0.WP).
    /// A difference therefore means a frame was given to somebody else
    /// while the cache still named it, or a transfer landed in it late
    /// -- a corruption that shows up as a library crashing at random in
    /// whichever process maps that page next. Called at a program's
    /// fatal fault, where that is the question to answer first.
    pub fn verify_cached_pages(&self) {
        const MAX_PAGES: usize = 65536;
        let mut pages: Vec<(u64, u64, Arc<dyn Pager>, u64, alloc::string::String)> = Vec::new();
        {
            let inner = self.inner.lock();
            'regions: for v in inner.vmas.values() {
                let Backing::File { pager, offset, shared: false } = &v.backing else {
                    continue;
                };
                let mut va = v.start;
                while va < v.end {
                    // SAFETY: the page table of this space, read under its lock.
                    let leaf = unsafe { self.leaf(va) };
                    if leaf & flags::PRESENT != 0 && leaf & CACHED != 0 {
                        if pages.len() == MAX_PAGES {
                            break 'regions;
                        }
                        pages.push((va, leaf & ADDR, pager.clone(), offset + (va - v.start), v.name.clone()));
                    }
                    va += PAGE;
                }
            }
        }
        let mut fresh = alloc::vec![0u8; PAGE as usize];
        let mut differing = 0usize;
        let mut unread = 0usize;
        for (va, phys, pager, offset, name) in &pages {
            fresh.fill(0);
            if !pager.fill_uncached(*offset, &mut fresh) {
                unread += 1;
                continue;
            }
            // SAFETY: a cache frame, never freed, read through the window.
            let cached = unsafe { core::slice::from_raw_parts(phys_to_virt(*phys) as *const u8, PAGE as usize) };
            if let Some(first) = cached.iter().zip(fresh.iter()).position(|(a, b)| a != b) {
                differing += 1;
                if differing <= 16 {
                    let count = cached.iter().zip(fresh.iter()).filter(|(a, b)| a != b).count();
                    crate::serial_println!(
                        "[vm]   page du cache ALTEREE : \"{}\" +0x{:x} (0x{:x}, frame 0x{:x}) - {} octet(s) differents, premier a +0x{:x} : 0x{:02x} au lieu de 0x{:02x}",
                        name, offset, va, phys, count, first, cached[first], fresh[first]
                    );
                }
            }
        }
        crate::serial_println!(
            "[vm]   cache de pages : {} page(s) projetee(s) verifiee(s) contre le disque - {} alteree(s), {} illisible(s)",
            pages.len(), differing, unread
        );
    }

    /// The text a register points to, if it points to one in a resident
    /// page: the message of a program that crashes itself on purpose
    /// (Firefox's `RustMozCrash(file, line, reason)`, whose reason is
    /// printed nowhere else). Printable ASCII only, at most 160 bytes, not
    /// past the page.
    pub fn print_string_at(&self, register: &str, addr: u64) {
        if !(USER_MIN..USER_TOP).contains(&addr) {
            return;
        }
        let mut text = alloc::string::String::new();
        {
            let _inner = self.inner.lock();
            // SAFETY: the page table of this space, read under its lock.
            let leaf = unsafe { self.leaf(addr & !(PAGE - 1)) };
            if leaf & flags::PRESENT == 0 {
                return;
            }
            let base = phys_to_virt(leaf & ADDR) as *const u8;
            let mut at = (addr & (PAGE - 1)) as usize;
            while at < PAGE as usize && text.len() < 160 {
                // SAFETY: inside a present user frame, through the
                // physical window.
                let byte = unsafe { core::ptr::read_volatile(base.add(at)) };
                if !(0x20..0x7F).contains(&byte) {
                    break;
                }
                text.push(byte as char);
                at += 1;
            }
        }
        if text.len() >= 4 {
            crate::serial_println!("[vm]   {} -> \"{}\"", register, text);
        }
    }

    /// What covers `addr`, for the log of a fatal fault.
    pub fn describe(&self, addr: u64) -> alloc::string::String {
        let inner = self.inner.lock();
        let leaf = unsafe { self.leaf(addr & !(PAGE - 1)) };
        match inner.find(addr) {
            Some(v) => alloc::format!(
                "region 0x{:x}-0x{:x} prot {} \"{}\", entree 0x{:x}",
                v.start, v.end, v.prot.0, v.name, leaf
            ),
            None => {
                let before = inner.vmas.range(..addr).next_back().map(|(_, v)| (v.start, v.end));
                alloc::format!("aucune region (precedente {:x?}), entree 0x{:x}", before, leaf)
            }
        }
    }

    /// Copy of the regions, for `/proc/self/maps` and diagnostics.
    pub fn regions(&self) -> Vec<Vma> {
        self.inner.lock().vmas.values().cloned().collect()
    }

    // --------------------------------------------------------
    //  Region management
    // --------------------------------------------------------

    /// Creates a region. `fixed`: exactly at `addr`, replacing whatever
    /// is there. Otherwise `addr` is a hint, and the highest free range
    /// below `limit` is taken if the hint does not fit -- the top-down
    /// placement of Linux's mmap.
    pub fn map(
        &self,
        addr: u64,
        len: u64,
        prot: Prot,
        backing: Backing,
        fixed: bool,
        name: &str,
    ) -> Result<u64, VmError> {
        let len = align_up(len);
        if len == 0 || len > USER_TOP {
            return Err(VmError::Invalid);
        }
        if fixed {
            if addr % PAGE != 0 || addr < USER_MIN || addr.checked_add(len).is_none_or(|e| e > USER_TOP) {
                return Err(VmError::Invalid);
            }
            // What was there goes and the new region comes **under one
            // hold of the lock**, as `MAP_FIXED` promises. It used to be
            // `unmap` then a second lock for the insertion: between the
            // two the range was free, and another thread's `mmap(NULL)`
            // could be handed part of it. The two regions then overlapped,
            // and a library the loader was placing segment by segment read
            // another thread's memory -- the browser's parent died at
            // start one time in a few, in `libnssckbi.so` (a null pointer
            // one time, a senseless `.eh_frame` another), while every page
            // of the page cache it mapped matched the disk.
            let end = addr + len;
            let (freed, gone) = {
                let mut inner = self.lock_settled();
                let gone = inner.carve(addr, end);
                let freed = unsafe { self.clear_range(addr, end) };
                inner.resident -= freed.len();
                inner.vmas.insert(addr, Vma { start: addr, end, prot, backing, name: name.into() });
                // No page of the new region is installed before every core
                // dropped the old translations (`resolve` waits on this).
                self.invalidating.fetch_add(1, Ordering::AcqRel);
                (freed, gone)
            };
            self.finish_invalidation(freed);
            // As in `unmap`: the regions removed, and the objects they may
            // hold the last reference to, go after the invalidation.
            drop(gone);
            return Ok(addr);
        }
        let mut inner = self.lock_settled();
        let hint = addr & !(PAGE - 1);
        let start = if hint >= USER_MIN && hint.checked_add(len).is_some_and(|e| e <= USER_TOP) && inner.is_free(hint, hint + len) {
            hint
        } else {
            inner.find_free(len, MMAP_TOP).ok_or(VmError::NoMemory)?
        };
        inner.vmas.insert(start, Vma { start, end: start + len, prot, backing, name: name.into() });
        Ok(start)
    }

    /// Like `map` without `fixed`, but fails with `Exists` rather than
    /// moving if the exact range is taken.
    pub fn map_exact(&self, addr: u64, len: u64, prot: Prot, backing: Backing, name: &str) -> Result<u64, VmError> {
        let len = align_up(len);
        if addr % PAGE != 0 || addr < USER_MIN || len == 0 || addr.checked_add(len).is_none_or(|e| e > USER_TOP) {
            return Err(VmError::Invalid);
        }
        let mut inner = self.lock_settled();
        if !inner.is_free(addr, addr + len) {
            return Err(VmError::Exists);
        }
        inner.vmas.insert(addr, Vma { start: addr, end: addr + len, prot, backing, name: name.into() });
        Ok(addr)
    }

    /// Removes every region overlapping [addr, addr + len), splitting the
    /// ones that straddle an end, and frees their frames.
    pub fn unmap(&self, addr: u64, len: u64) -> Result<(), VmError> {
        let len = align_up(len);
        if addr % PAGE != 0 || len == 0 || addr.checked_add(len).is_none_or(|e| e > USER_TOP) {
            return Err(VmError::Invalid);
        }
        let end = addr + len;
        let (freed, gone) = {
            let mut inner = self.inner.lock();
            let gone = inner.carve(addr, end);
            let freed = unsafe { self.clear_range(addr, end) };
            inner.resident -= freed.len();
            self.invalidating.fetch_add(1, Ordering::AcqRel);
            (freed, gone)
        };
        self.finish_invalidation(freed);
        // The regions removed go only now. One may hold the last
        // reference to the object it mapped (a memfd whose descriptor was
        // closed after `mmap`, the browser's shared memory): the object
        // then frees its frames, which other cores could still reach
        // through their TLB until the invalidation above -- a thread
        // writing through a stale translation into a frame the allocator
        // has already handed out again, as a page table or as another
        // process's page.
        drop(gone);
        Ok(())
    }

    /// Changes the protections of [addr, addr + len). Every page of the
    /// range must be mapped, as `mprotect` demands.
    pub fn protect(&self, addr: u64, len: u64, prot: Prot) -> Result<(), VmError> {
        let len = align_up(len);
        if addr % PAGE != 0 || addr.checked_add(len).is_none_or(|e| e > USER_TOP) {
            return Err(VmError::Invalid);
        }
        if len == 0 {
            return Ok(());
        }
        let end = addr + len;
        {
            let mut inner = self.inner.lock();
            if !inner.covered(addr, end) {
                return Err(VmError::NoMemory);
            }
            let pieces = inner.carve(addr, end);
            for mut piece in pieces {
                piece.prot = prot;
                inner.vmas.insert(piece.start, piece);
            }
            inner.merge_around(addr, end);
            // Resident pages take the new rights at once: waiting for a
            // fault would let a page made read-only stay writable.
            unsafe { self.reprotect_range(addr, end, prot) };
            self.invalidating.fetch_add(1, Ordering::AcqRel);
        }
        self.finish_invalidation(Vec::new());
        Ok(())
    }

    /// Discards the contents of [addr, addr + len): the regions stay, the
    /// next touch reads zeros or the file again (`MADV_DONTNEED`).
    pub fn discard(&self, addr: u64, len: u64) -> Result<(), VmError> {
        let len = align_up(len);
        if addr % PAGE != 0 || addr.checked_add(len).is_none_or(|e| e > USER_TOP) {
            return Err(VmError::Invalid);
        }
        let freed = {
            let mut inner = self.inner.lock();
            let freed = unsafe { self.clear_range(addr, addr + len) };
            inner.resident -= freed.len();
            self.invalidating.fetch_add(1, Ordering::AcqRel);
            freed
        };
        self.finish_invalidation(freed);
        Ok(())
    }

    /// Moves or resizes the region that starts at `old` (`mremap`).
    /// Growth in place when the range above is free, otherwise the pages
    /// move to a new address if `may_move`. Returns the new address.
    pub fn remap(&self, old: u64, old_len: u64, new_len: u64, may_move: bool) -> Result<u64, VmError> {
        let old_len = align_up(old_len);
        let new_len = align_up(new_len);
        if old % PAGE != 0 || old_len == 0 || new_len == 0 {
            return Err(VmError::Invalid);
        }
        if new_len <= old_len {
            if new_len < old_len {
                self.unmap(old + new_len, old_len - new_len)?;
            }
            return Ok(old);
        }
        let mut inner = self.lock_settled();
        // The range may span several regions -- mprotect splits them --
        // provided they are contiguous and cover it all.
        if !inner.covered(old, old + old_len) {
            return Err(VmError::Fault);
        }
        let end = old + old_len;
        let last_start = inner.vmas.range(..end).next_back().map(|(k, _)| *k).ok_or(VmError::Fault)?;
        if inner.vmas[&last_start].end != end {
            // The range stops inside a region: shrink it to the range first.
            let pieces = inner.carve(old, end);
            for p in pieces {
                inner.vmas.insert(p.start, p);
            }
        }
        if old + new_len <= USER_TOP && inner.is_free(end, old + new_len) {
            let last = inner.vmas.range(..end).next_back().map(|(k, _)| *k).unwrap();
            inner.vmas.get_mut(&last).unwrap().end = old + new_len;
            return Ok(old);
        }
        if !may_move {
            return Err(VmError::NoMemory);
        }
        let target = inner.find_free(new_len, MMAP_TOP).ok_or(VmError::NoMemory)?;
        // Move the page table entries themselves: the frames stay put,
        // only their virtual address changes.
        let mut page = 0;
        while page < old_len {
            let entry = unsafe { self.take_leaf(old + page) };
            if entry != 0 {
                unsafe { self.set_leaf(target + page, entry) }.ok_or(VmError::NoMemory)?;
            }
            page += PAGE;
        }
        let moved = inner.carve(old, end);
        let delta = target.wrapping_sub(old);
        for (i, mut piece) in moved.into_iter().enumerate() {
            piece.start = piece.start.wrapping_add(delta);
            piece.end = piece.end.wrapping_add(delta);
            // `carve` returns the pieces highest first: the first one is
            // the last region, which takes the growth.
            if i == 0 {
                piece.end = target + new_len;
            }
            inner.vmas.insert(piece.start, piece);
        }
        self.invalidating.fetch_add(1, Ordering::AcqRel);
        drop(inner);
        self.finish_invalidation(Vec::new());
        Ok(target)
    }

    // --------------------------------------------------------
    //  Faults
    // --------------------------------------------------------

    /// Makes the page holding `addr` present with at least the requested
    /// access. Called by the page fault handler and by `populate`.
    ///
    /// The frame is filled with the lock **released**: filling may read a
    /// file from disk. The lock is taken again to install it, after
    /// checking that the region did not change meanwhile.
    pub fn resolve(&self, addr: u64, write: bool, exec: bool) -> Result<(), VmError> {
        // Another thread may reshape the regions while this one fills a
        // page with the lock released (an mprotect next door splits the
        // region). That is no reason to fail the access: look again.
        // Many rounds: a retry also follows every invalidation this space
        // had in flight, and a busy allocator unmaps often.
        for _ in 0..4096 {
            match self.resolve_once(addr, write, exec) {
                Err(VmError::Exists) => continue,
                other => return other,
            }
        }
        Err(VmError::Fault)
    }

    /// One attempt of `resolve`; `Err(Exists)` means "the region changed
    /// under us, retry".
    fn resolve_once(&self, addr: u64, write: bool, exec: bool) -> Result<(), VmError> {
        if !(USER_MIN..USER_TOP).contains(&addr) {
            return Err(VmError::Fault);
        }
        let page = addr & !(PAGE - 1);
        let (vma, existing) = {
            let inner = self.inner.lock();
            let vma = inner.find(addr).ok_or(VmError::Fault)?.clone();
            if !vma.prot.readable() || (write && !vma.prot.writable()) || (exec && !vma.prot.executable()) {
                return Err(VmError::Fault);
            }
            (vma, unsafe { self.leaf(page) })
        };
        if existing & flags::PRESENT != 0 {
            // Present with enough rights: a stale translation on this
            // core, or another thread resolved it first.
            let ok = (!write || existing & flags::WRITABLE != 0) && (!exec || existing & flags::NO_EXECUTE == 0);
            if ok {
                unsafe { memory::flush_page(page) };
                return Ok(());
            }
            if write && existing & COW != 0 {
                return self.break_cow(page, &vma);
            }
            if write && existing & CACHED != 0 {
                return self.copy_cached(page, &vma);
            }
            return Err(VmError::Fault);
        }

        // A read of a private file page the object caches: the cache's
        // frame itself, read-only. Every process mapping the library
        // shares it; the first write copies it (`copy_cached`).
        if !write
            && let Backing::File { pager, offset, shared: false } = &vma.backing
            && let Some(phys) = pager.cached_frame(offset + (page - vma.start))
        {
            let mut inner = self.inner.lock();
            if self.invalidating.load(Ordering::Acquire) != 0 {
                drop(inner);
                self.wait_invalidations();
                return Err(VmError::Exists);
            }
            let still = inner.find(addr).is_some_and(|v| v.prot == vma.prot && same_backing(v, &vma, page));
            if !still {
                return Err(VmError::Exists);
            }
            if unsafe { self.leaf(page) } & flags::PRESENT != 0 {
                return Ok(());
            }
            let value = phys | (vma.prot.entry_flags() & !flags::WRITABLE) | BORROWED | CACHED;
            unsafe { self.set_leaf(page, value) }.ok_or(VmError::NoMemory)?;
            inner.faults += 1;
            return Ok(());
        }

        let (phys, borrowed) = match &vma.backing {
            Backing::File { pager, offset, shared: true } => {
                let at = offset + (page - vma.start);
                (pager.shared_frame(at).ok_or(VmError::Fault)?, true)
            }
            Backing::File { pager, offset, shared: false } => {
                let frame = frame::allocate_zeroed_frame().ok_or(VmError::NoMemory)?;
                let bytes = unsafe { core::slice::from_raw_parts_mut(frame.virt_addr() as *mut u8, PAGE as usize) };
                if !pager.fill(offset + (page - vma.start), bytes) {
                    unsafe { frame::deallocate_frame(frame) };
                    return Err(VmError::Fault);
                }
                (frame.start_addr(), false)
            }
            Backing::Anon => (frame::allocate_zeroed_frame().ok_or(VmError::NoMemory)?.start_addr(), false),
        };

        let mut inner = self.inner.lock();
        if self.invalidating.load(Ordering::Acquire) != 0 {
            // A core may still translate this address to the frame that
            // was here: no new frame until every core has let it go.
            drop(inner);
            if !borrowed {
                unsafe { frame::deallocate_frame(Frame::containing(phys)) };
            }
            self.wait_invalidations();
            return Err(VmError::Exists);
        }
        let still = inner.find(addr).is_some_and(|v| v.prot == vma.prot && same_backing(v, &vma, page));
        let entry = unsafe { self.leaf(page) };
        if !still || entry & flags::PRESENT != 0 {
            drop(inner);
            if !borrowed {
                unsafe { frame::deallocate_frame(Frame::containing(phys)) };
            }
            // Someone else changed the page or the region meanwhile: let
            // the caller look again.
            return if still { Ok(()) } else { Err(VmError::Exists) };
        }
        let mut value = phys | vma.prot.entry_flags();
        if let Backing::File { pager, shared: true, .. } = &vma.backing {
            value |= pager.page_flags();
        }
        if borrowed {
            value |= BORROWED;
        } else {
            inner.resident += 1;
        }
        unsafe { self.set_leaf(page, value) }.ok_or(VmError::NoMemory)?;
        inner.faults += 1;
        Ok(())
    }

    /// First write to a copy-on-write page: the last holder takes the
    /// frame back as its own, any other gets a copy.
    fn break_cow(&self, page: u64, vma: &Vma) -> Result<(), VmError> {
        // A fork of this space is still invalidating: wait, lock released
        // and interrupts on (the fault handler turned them back on), then
        // let `resolve` look again.
        if self.forking.load(Ordering::Acquire) {
            while self.forking.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
            return Err(VmError::Exists);
        }
        let mut inner = self.inner.lock();
        if self.forking.load(Ordering::Acquire) {
            return Err(VmError::Exists);
        }
        let current = unsafe { self.leaf(page) };
        if current & flags::PRESENT == 0 || current & COW == 0 {
            // Changed meanwhile (another thread broke it): retry the access.
            return Ok(());
        }
        let phys = current & ADDR;
        let rights = vma.prot.entry_flags();
        let shared = COW_REFS.lock().get(&phys).is_some_and(|c| *c > 1);
        if !shared {
            COW_REFS.lock().remove(&phys);
            unsafe { self.set_leaf(page, phys | rights) }.ok_or(VmError::NoMemory)?;
        } else {
            let copy = frame::allocate_frame().ok_or(VmError::NoMemory)?;
            unsafe {
                core::ptr::copy_nonoverlapping(phys_to_virt(phys) as *const u8, copy.virt_addr() as *mut u8, PAGE as usize);
                self.set_leaf(page, copy.start_addr() | rights).ok_or(VmError::NoMemory)?;
            }
            inner.faults += 1;
            drop(inner);
            // The page changed frame. The other threads of this space may
            // still hold the read-only translation to the old one on
            // their cores: until they drop it they would read the old
            // bytes while this thread writes the copy -- two views of one
            // page, which is how a heap's metadata tears. Every core
            // forgets it before the faulting write goes on, and before
            // the old frame can be freed (`ptep_clear_flush` on Linux).
            // TCG's software TLB hid this; a real one under KVM did not.
            memory::shootdown_space_and_wait(self.pml4);
            if cow_release(phys) {
                // The other holders went away while we copied.
                unsafe { frame::deallocate_frame(Frame::containing(phys)) };
            }
            return Ok(());
        }
        unsafe { memory::flush_page(page) };
        inner.faults += 1;
        Ok(())
    }

    /// The region lock, taken at a moment when no invalidation of this
    /// space is in flight -- for handing out addresses.
    ///
    /// A range `munmap` has just released may still be translated, by a
    /// core the shootdown has not reached yet, to the frames it had. A
    /// thread given those addresses by `mmap` meanwhile, and running on
    /// such a core, would **write through the stale entry** without a
    /// fault -- into a frame on its way back to the allocator -- and the
    /// next reader would find the page empty. No fault means no check in
    /// `resolve` can catch it: the addresses must simply not be handed
    /// out before every core has let go of them. Linux gets the same by
    /// flushing before it releases `mmap_lock`.
    fn lock_settled(&self) -> crate::vga::SpinLockGuard<'_, Inner> {
        loop {
            let inner = self.inner.lock();
            if self.invalidating.load(Ordering::Acquire) == 0 {
                return inner;
            }
            drop(inner);
            self.wait_invalidations();
        }
    }

    /// Waits, lock released and interrupts on, until no invalidation of
    /// this space is in flight.
    fn wait_invalidations(&self) {
        while self.invalidating.load(Ordering::Acquire) != 0 {
            // The invalidator waits for this core's flush: done here too,
            // so that waiting with interrupts masked cannot deadlock on
            // an IPI that never gets in.
            memory::sync_tlb();
            core::hint::spin_loop();
        }
    }

    /// First write to a page-cache frame mapped privately: the page gets
    /// a copy of its own, and the cache frame is left untouched.
    fn copy_cached(&self, page: u64, vma: &Vma) -> Result<(), VmError> {
        if self.forking.load(Ordering::Acquire) {
            while self.forking.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
            return Err(VmError::Exists);
        }
        let copy = frame::allocate_frame().ok_or(VmError::NoMemory)?;
        let mut inner = self.inner.lock();
        let current = unsafe { self.leaf(page) };
        if self.forking.load(Ordering::Acquire) || current & flags::PRESENT == 0 || current & CACHED == 0 {
            drop(inner);
            unsafe { frame::deallocate_frame(copy) };
            // Changed meanwhile: let the access retry.
            return Err(VmError::Exists);
        }
        unsafe {
            core::ptr::copy_nonoverlapping(phys_to_virt(current & ADDR) as *const u8, copy.virt_addr() as *mut u8, PAGE as usize);
            self.set_leaf(page, copy.start_addr() | vma.prot.entry_flags()).ok_or(VmError::NoMemory)?;
        }
        inner.resident += 1;
        inner.faults += 1;
        drop(inner);
        // The page changed frame: the other threads' read-only view of
        // the cache frame goes before this thread writes (see break_cow).
        memory::shootdown_space_and_wait(self.pml4);
        Ok(())
    }

    /// Copy of this space for a `fork`: same regions, and every private
    /// page shared copy-on-write. Shared mappings (memfd, tmpfs) stay
    /// shared: both spaces map the same frame, as `MAP_SHARED` promises.
    pub fn fork(&self) -> Option<Arc<Space>> {
        let child = Space::new()?;
        {
            let inner = self.inner.lock();
            self.forking.store(true, Ordering::Release);
            let mut child_inner = child.inner.lock();
            child_inner.vmas = inner.vmas.clone();
            // Regions the child does not inherit (`Pager::fork_shares`).
            let kept_out: Vec<(u64, u64)> = inner
                .vmas
                .values()
                .filter(|v| matches!(&v.backing, Backing::File { pager, .. } if !pager.fork_shares()))
                .map(|v| (v.start, v.end))
                .collect();
            for (start, _) in &kept_out {
                child_inner.vmas.remove(start);
            }
            let mut refs = COW_REFS.lock();
            let mut va = USER_MIN;
            while va < USER_TOP {
                let idx = memory::table_indices(va);
                let p4 = self.root();
                if !p4[idx[0]].is_present() {
                    va = next_boundary(va, 39);
                    continue;
                }
                let p3 = unsafe { &mut *(phys_to_virt(p4[idx[0]].addr()) as *mut PageTable) };
                if !p3[idx[1]].is_present() {
                    va = next_boundary(va, 30);
                    continue;
                }
                let p2 = unsafe { &mut *(phys_to_virt(p3[idx[1]].addr()) as *mut PageTable) };
                if !p2[idx[2]].is_present() {
                    va = next_boundary(va, 21);
                    continue;
                }
                let p1 = unsafe { &mut *(phys_to_virt(p2[idx[2]].addr()) as *mut PageTable) };
                let old = p1[idx[3]].raw();
                if kept_out.iter().any(|(start, end)| (*start..*end).contains(&va)) {
                    va += PAGE;
                    continue;
                }
                if old & (flags::PRESENT | NONE_KEPT) != 0 {
                    let value = if old & BORROWED != 0 {
                        old
                    } else {
                        let phys = old & ADDR;
                        if old & COW != 0 {
                            *refs.entry(phys).or_insert(1) += 1;
                        } else {
                            refs.insert(phys, 2);
                        }
                        child_inner.resident += 1;
                        (old & !flags::WRITABLE) | COW
                    };
                    p1[idx[3]] = entry(value);
                    if unsafe { child.set_leaf(va, value) }.is_none() {
                        drop(refs);
                        drop(child_inner);
                        drop(inner);
                        memory::shootdown_space_and_wait(self.pml4);
                        self.forking.store(false, Ordering::Release);
                        return None;
                    }
                }
                va += PAGE;
            }
        }
        // The parent's writable translations must be gone from every
        // core before either side writes again.
        memory::shootdown_space_and_wait(self.pml4);
        self.forking.store(false, Ordering::Release);
        Some(child)
    }

    /// Removes every region whose pages come from `pager`, wherever the
    /// program has moved them (`mremap`). Returns how many went.
    ///
    /// What the GPU core calls before it gives a device window back: a
    /// region left behind would keep translating to pages the host is
    /// about to lend to someone else.
    pub fn unmap_pager(&self, pager: &Arc<dyn Pager>) -> usize {
        let ranges: Vec<(u64, u64)> = {
            let inner = self.inner.lock();
            inner
                .vmas
                .values()
                .filter(|v| matches!(&v.backing, Backing::File { pager: p, .. } if core::ptr::addr_eq(Arc::as_ptr(p), Arc::as_ptr(pager))))
                .map(|v| (v.start, v.end))
                .collect()
        };
        for (start, end) in &ranges {
            let _ = self.unmap(*start, end - start);
        }
        ranges.len()
    }

    /// Resolves every page of a range ahead of use.
    pub fn populate(&self, addr: u64, len: u64, write: bool) -> Result<(), VmError> {
        let end = addr.checked_add(len).ok_or(VmError::Fault)?;
        let mut page = addr & !(PAGE - 1);
        while page < end {
            self.resolve(page, write, false)?;
            page += PAGE;
        }
        Ok(())
    }

    /// Writes `data` at `addr` of this space from outside it -- the
    /// loader builds a process's first stack before the process runs,
    /// from a task whose CR3 is another space. Every page is resolved
    /// for writing first, then filled through the physical window.
    pub fn poke(&self, addr: u64, data: &[u8]) -> Result<(), VmError> {
        let mut done = 0usize;
        while done < data.len() {
            let at = addr + done as u64;
            self.resolve(at, true, false)?;
            let leaf = unsafe { self.leaf(at & !(PAGE - 1)) };
            if leaf & flags::PRESENT == 0 {
                return Err(VmError::Fault);
            }
            let inside = (at % PAGE) as usize;
            let count = (PAGE as usize - inside).min(data.len() - done);
            let dst = phys_to_virt(leaf & 0x000F_FFFF_FFFF_F000) + inside as u64;
            unsafe { core::ptr::copy_nonoverlapping(data[done..].as_ptr(), dst as *mut u8, count) };
            done += count;
        }
        Ok(())
    }

    // --------------------------------------------------------
    //  Page tables of this space
    // --------------------------------------------------------

    fn root(&self) -> &'static mut PageTable {
        unsafe { &mut *(phys_to_virt(self.pml4) as *mut PageTable) }
    }

    /// Leaf entry for `virt`, or 0.
    unsafe fn leaf(&self, virt: u64) -> u64 {
        let idx = memory::table_indices(virt);
        let mut table = self.root();
        for level in 0..3 {
            let entry = table[idx[level]];
            if !entry.is_present() {
                return 0;
            }
            table = unsafe { &mut *(phys_to_virt(entry.addr()) as *mut PageTable) };
        }
        table[idx[3]].raw()
    }

    /// Writes a leaf entry, creating the intermediate tables.
    unsafe fn set_leaf(&self, virt: u64, value: u64) -> Option<()> {
        let idx = memory::table_indices(virt);
        let mut table = self.root();
        for level in 0..3 {
            if !table[idx[level]].is_present() {
                let frame = frame::allocate_zeroed_frame()?;
                table[idx[level]].set(frame.start_addr(), flags::PRESENT | flags::WRITABLE | flags::USER);
            }
            table = unsafe { &mut *(phys_to_virt(table[idx[level]].addr()) as *mut PageTable) };
        }
        table[idx[3]] = entry(value);
        Some(())
    }

    /// Clears a leaf entry and returns what it held.
    unsafe fn take_leaf(&self, virt: u64) -> u64 {
        let idx = memory::table_indices(virt);
        let mut table = self.root();
        for level in 0..3 {
            let entry = table[idx[level]];
            if !entry.is_present() {
                return 0;
            }
            table = unsafe { &mut *(phys_to_virt(entry.addr()) as *mut PageTable) };
        }
        let old = table[idx[3]].raw();
        table[idx[3]].clear();
        old
    }

    /// Clears every leaf of [start, end). Returns the owned frames, to be
    /// freed only once no core can still use them.
    unsafe fn clear_range(&self, start: u64, end: u64) -> Vec<u64> {
        let mut freed = Vec::new();
        let mut va = start;
        while va < end {
            // Skip whole missing tables quickly: 2 MiB at a time.
            let idx = memory::table_indices(va);
            let p4 = self.root();
            if !p4[idx[0]].is_present() {
                va = next_boundary(va, 39);
                continue;
            }
            let p3 = unsafe { &mut *(phys_to_virt(p4[idx[0]].addr()) as *mut PageTable) };
            if !p3[idx[1]].is_present() {
                va = next_boundary(va, 30);
                continue;
            }
            let p2 = unsafe { &mut *(phys_to_virt(p3[idx[1]].addr()) as *mut PageTable) };
            if !p2[idx[2]].is_present() {
                va = next_boundary(va, 21);
                continue;
            }
            let p1 = unsafe { &mut *(phys_to_virt(p2[idx[2]].addr()) as *mut PageTable) };
            let old = p1[idx[3]].raw();
            if old & (flags::PRESENT | NONE_KEPT) != 0 {
                p1[idx[3]].clear();
                if old & BORROWED == 0 && (old & COW == 0 || cow_release(old & ADDR)) {
                    freed.push(old & ADDR);
                }
            }
            va += PAGE;
        }
        freed
    }

    unsafe fn reprotect_range(&self, start: u64, end: u64, prot: Prot) {
        let mut va = start;
        while va < end {
            let idx = memory::table_indices(va);
            let p4 = self.root();
            if !p4[idx[0]].is_present() {
                va = next_boundary(va, 39);
                continue;
            }
            let p3 = unsafe { &mut *(phys_to_virt(p4[idx[0]].addr()) as *mut PageTable) };
            if !p3[idx[1]].is_present() {
                va = next_boundary(va, 30);
                continue;
            }
            let p2 = unsafe { &mut *(phys_to_virt(p3[idx[1]].addr()) as *mut PageTable) };
            if !p2[idx[2]].is_present() {
                va = next_boundary(va, 21);
                continue;
            }
            let p1 = unsafe { &mut *(phys_to_virt(p2[idx[2]].addr()) as *mut PageTable) };
            let old = p1[idx[3]].raw();
            if old & flags::PRESENT != 0 {
                let keep = old & (ADDR | BORROWED | COW | CACHED);
                if prot.readable() {
                    // A copy-on-write or page-cache frame stays
                    // read-only: the write fault is what copies it.
                    let rights = if old & (COW | CACHED) != 0 { prot.entry_flags() & !flags::WRITABLE } else { prot.entry_flags() };
                    p1[idx[3]] = entry(keep | rights);
                } else {
                    // PROT_NONE: the entry goes, the frame stays owned --
                    // a later mprotect back to readable must find the
                    // contents again. Kept in the entry with PRESENT
                    // cleared: bit 0 clear makes the rest ignored by the
                    // hardware and meaningful to us.
                    p1[idx[3]] = entry(keep | NONE_KEPT);
                }
            } else if old & NONE_KEPT != 0 && prot.readable() {
                let rights = if old & (COW | CACHED) != 0 { prot.entry_flags() & !flags::WRITABLE } else { prot.entry_flags() };
                p1[idx[3]] = entry((old & (ADDR | BORROWED | COW | CACHED)) | rights);
            }
            va += PAGE;
        }
    }

    /// Makes every core drop the translations just removed, then frees
    /// the frames. Called with no lock held and interrupts enabled.
    fn finish_invalidation(&self, freed: Vec<u64>) {
        memory::shootdown_space_and_wait(self.pml4);
        self.invalidating.fetch_sub(1, Ordering::AcqRel);
        for phys in freed {
            unsafe { frame::deallocate_frame(Frame::containing(phys)) };
        }
    }
}

/// Marks a PROT_NONE page whose frame is kept (PRESENT clear).
const NONE_KEPT: u64 = 1 << 10;

/// Marks a frame shared copy-on-write between spaces after a `fork`: the
/// entry is read-only whatever the region says, and the first write
/// copies the frame (`resolve`) unless this space is its last holder.
const COW: u64 = 1 << 11;

const ADDR: u64 = 0x000F_FFFF_FFFF_F000;

/// Entries referencing each copy-on-write frame, across every space.
/// Only frames shared by a fork are here; a frame absent from the table
/// with the COW bit set has a single holder left.
static COW_REFS: SpinLock<BTreeMap<u64, u32>> = SpinLock::new(BTreeMap::new());

pub fn cow_frames() -> usize {
    COW_REFS.lock().len()
}

/// One reference to a copy-on-write frame goes. True if it was the last,
/// in which case the caller frees the frame.
fn cow_release(phys: u64) -> bool {
    let mut refs = COW_REFS.lock();
    match refs.get_mut(&phys) {
        Some(count) if *count > 1 => {
            *count -= 1;
            if *count == 1 {
                refs.remove(&phys);
            }
            false
        }
        _ => {
            refs.remove(&phys);
            true
        }
    }
}

fn entry(value: u64) -> memory::PageTableEntry {
    let mut e = memory::PageTableEntry::empty();
    // `set` masks the address and ORs the flags; the software bits and
    // NX live in the flag half.
    e.set(value & 0x000F_FFFF_FFFF_F000, value & !0x000F_FFFF_FFFF_F000);
    e
}

/// Does `now` still take page `page` from where `before` did? A region
/// split or merged around the page keeps its source; one replaced by a
/// new mapping does not.
fn same_backing(now: &Vma, before: &Vma, page: u64) -> bool {
    match (&now.backing, &before.backing) {
        (Backing::Anon, Backing::Anon) => true,
        (Backing::File { pager: a, offset: oa, shared: sa }, Backing::File { pager: b, offset: ob, shared: sb }) => {
            Arc::ptr_eq(a, b) && sa == sb && oa + (page - now.start) == ob + (page - before.start)
        }
        _ => false,
    }
}

fn next_boundary(va: u64, shift: u32) -> u64 {
    ((va >> shift) + 1) << shift
}

fn align_up(len: u64) -> u64 {
    len.checked_add(PAGE - 1).map_or(0, |v| v & !(PAGE - 1))
}

/// Top of the range `map` places regions in when given no usable hint.
/// The stack lives above it.
pub const MMAP_TOP: u64 = 0x0000_7F00_0000_0000;

impl Inner {
    fn find(&self, addr: u64) -> Option<&Vma> {
        self.vmas.range(..=addr).next_back().map(|(_, v)| v).filter(|v| addr < v.end)
    }

    fn is_free(&self, start: u64, end: u64) -> bool {
        if let Some((_, v)) = self.vmas.range(..end).next_back()
            && v.end > start
        {
            return false;
        }
        true
    }

    /// Is every byte of [start, end) inside some region?
    fn covered(&self, start: u64, end: u64) -> bool {
        let mut at = start;
        while at < end {
            match self.find(at) {
                Some(v) => at = v.end,
                None => return false,
            }
        }
        true
    }

    /// Highest free range of `len` bytes ending at or below `limit`.
    fn find_free(&self, len: u64, limit: u64) -> Option<u64> {
        let mut ceiling = limit;
        for (_, v) in self.vmas.range(..limit).rev() {
            if v.end <= ceiling && ceiling - v.end >= len {
                return Some(ceiling - len);
            }
            ceiling = ceiling.min(v.start);
        }
        (ceiling >= USER_MIN + len).then(|| ceiling - len)
    }

    /// Joins anonymous regions around [start, end) with their neighbours
    /// when they now have the same rights -- what a program that
    /// mprotects a page and back expects to find as one region again.
    fn merge_around(&mut self, start: u64, end: u64) {
        let keys: Vec<u64> = self
            .vmas
            .range(..=end)
            .rev()
            .take_while(|(_, v)| v.end >= start)
            .map(|(k, _)| *k)
            .collect();
        let mut keys = keys;
        keys.reverse();
        // Include the region just before, if it ends where the first begins.
        if let Some(first) = keys.first().copied()
            && let Some((k, v)) = self.vmas.range(..first).next_back()
            && v.end == first
        {
            keys.insert(0, *k);
        }
        let mut i = 0;
        while i + 1 < keys.len() {
            let (a, b) = (keys[i], keys[i + 1]);
            let mergeable = match (self.vmas.get(&a), self.vmas.get(&b)) {
                (Some(x), Some(y)) => {
                    x.end == y.start && x.prot == y.prot && x.name == y.name && matches!((&x.backing, &y.backing), (Backing::Anon, Backing::Anon))
                }
                _ => false,
            };
            if mergeable {
                let y = self.vmas.remove(&b).unwrap();
                self.vmas.get_mut(&a).unwrap().end = y.end;
                keys.remove(i + 1);
            } else {
                i += 1;
            }
        }
    }

    /// Removes [start, end) from the region list, keeping the parts of
    /// straddling regions that lie outside. Returns the removed pieces.
    fn carve(&mut self, start: u64, end: u64) -> Vec<Vma> {
        let hits: Vec<u64> = self
            .vmas
            .range(..end)
            .rev()
            .take_while(|(_, v)| v.end > start)
            .map(|(k, _)| *k)
            .collect();
        let mut removed = Vec::new();
        for key in hits {
            let v = self.vmas.remove(&key).unwrap();
            if v.start < start {
                self.vmas.insert(v.start, v.slice(v.start, start));
            }
            if v.end > end {
                self.vmas.insert(end, v.slice(end, v.end));
            }
            removed.push(v.slice(v.start.max(start), v.end.min(end)));
        }
        removed
    }
}

impl Drop for Space {
    fn drop(&mut self) {
        // Frames of every page, then the tables of the lower half.
        let freed = unsafe { self.clear_range(USER_MIN, USER_TOP) };
        for phys in freed {
            unsafe { frame::deallocate_frame(Frame::containing(phys)) };
        }
        let root = self.root();
        for i in 0..256 {
            if root[i].is_present() {
                unsafe { free_tree(root[i].addr(), 3) };
                root[i].clear();
            }
        }
        // The PML4 itself may still be in some core's CR3 for a few
        // instructions (a task dying on another core): it goes to the
        // graveyard, freed once no core names it.
        GRAVEYARD.lock().push(self.pml4);
        LIVE.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Frees a page table and every table below it (not the leaf frames).
unsafe fn free_tree(phys: u64, level: u8) {
    if level > 1 {
        let table = unsafe { &*(phys_to_virt(phys) as *const PageTable) };
        for i in 0..512 {
            if table[i].is_present() && !table[i].is_huge() {
                unsafe { free_tree(table[i].addr(), level - 1) };
            }
        }
    }
    unsafe { frame::deallocate_frame(Frame::containing(phys)) };
}

// ============================================================
//  Active space per core
// ============================================================

/// CR3 each core last loaded, so the graveyard knows what is in use.
static ACTIVE_CR3: [AtomicU64; crate::gdt::MAX_CPUS] = [const { AtomicU64::new(0) }; crate::gdt::MAX_CPUS];

/// CR3 each core is loading or has loaded, published **before** the load
/// (`activate`): with `ACTIVE_CR3`, published after it, the two cover
/// every instant at which a core may hold translations of a space.
static LOADING_CR3: [AtomicU64; crate::gdt::MAX_CPUS] = [const { AtomicU64::new(0) }; crate::gdt::MAX_CPUS];

/// May `cpu` hold translations of the page table `cr3`? What a targeted
/// shootdown asks (`memory::shootdown_space_and_wait`).
pub fn may_hold(cpu: usize, cr3: u64) -> bool {
    LOADING_CR3[cpu].load(Ordering::SeqCst) == cr3 || ACTIVE_CR3[cpu].load(Ordering::SeqCst) == cr3
}

/// Space of the task running on each core, as a raw pointer: the fault
/// handler cannot take the scheduler lock. Valid while that task runs,
/// since the task holds an `Arc` on it.
static CURRENT: [AtomicU64; crate::gdt::MAX_CPUS] = [const { AtomicU64::new(0) }; crate::gdt::MAX_CPUS];

static GRAVEYARD: SpinLock<Vec<u64>> = SpinLock::new(Vec::new());

/// Loads the address space of the task about to run on this core.
/// `None` means the boot PML4.
///
/// Called by the scheduler before `switch_context`, outside its lock.
/// The outgoing task's stack is a kernel address, the same in every
/// space: continuing on it after the switch of CR3 is safe.
pub fn activate(space: Option<&Arc<Space>>) {
    let cpu = crate::smp::current_cpu_id().min(crate::gdt::MAX_CPUS - 1);
    let (cr3, ptr) = match space {
        Some(s) => (s.pml4, Arc::as_ptr(s) as u64),
        None => (kernel_pml4(), 0),
    };
    CURRENT[cpu].store(ptr, Ordering::Release);
    if cr3 != 0 && memory::read_cr3() & !0xFFF != cr3 {
        // Announced before the load, and the store ordered before the
        // page walks the load allows: a shootdown that does not see it
        // changed its entries before this core could walk them.
        LOADING_CR3[cpu].store(cr3, Ordering::SeqCst);
        core::sync::atomic::fence(Ordering::SeqCst);
        unsafe { core::arch::asm!("mov cr3, {}", in(reg) cr3, options(nostack, preserves_flags)) };
        ACTIVE_CR3[cpu].store(cr3, Ordering::Release);
    }
}

/// Space of the task running on this core, if it has one.
///
/// # Safety
/// The reference is valid only while the current task keeps running --
/// not across a switch, not after its death.
pub unsafe fn current() -> Option<&'static Space> {
    let cpu = crate::smp::current_cpu_id().min(crate::gdt::MAX_CPUS - 1);
    let ptr = CURRENT[cpu].load(Ordering::Acquire);
    (ptr != 0).then(|| unsafe { &*(ptr as *const Space) })
}

fn reap_graveyard() {
    let mut grave = GRAVEYARD.lock();
    grave.retain(|pml4| {
        let busy = ACTIVE_CR3.iter().any(|c| c.load(Ordering::Acquire) == *pml4);
        if !busy {
            unsafe { frame::deallocate_frame(Frame::containing(*pml4)) };
        }
        busy
    });
}

// ============================================================
//  Page faults (called from boot/fault.asm)
// ============================================================

/// Registers and hardware frame saved by `page_fault_entry`.
#[repr(C)]
pub struct FaultFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub error: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

unsafe extern "C" {
    fn user_copy(dst: *mut u8, src: *const u8, len: usize) -> usize;
    static user_copy_end: u8;
    static user_copy_fixup: u8;
    pub fn enter_user(regs: *const UserRegs) -> !;
}

/// Faults resolved, faults recovered by the copy fixup, processes killed.
static STATS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

pub fn fault_stats() -> (u64, u64, u64) {
    (STATS[0].load(Ordering::Relaxed), STATS[1].load(Ordering::Relaxed), STATS[2].load(Ordering::Relaxed))
}

#[unsafe(no_mangle)]
pub extern "C" fn page_fault_handler(frame: &mut FaultFrame) {
    let cr2: u64;
    unsafe { core::arch::asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack, preserves_flags)) };
    let from_user = frame.cs & 3 == 3;
    let write = frame.error & 2 != 0;
    let exec = frame.error & 16 != 0;
    let copy_start = user_copy as *const () as u64;
    let copy_end = &raw const user_copy_end as u64;
    let in_copy = !from_user && (copy_start..copy_end).contains(&frame.rip);

    if cr2 < USER_TOP && (from_user || in_copy) {
        // SAFETY: the faulting task is the current one and holds the Arc.
        if let Some(space) = unsafe { current() } {
            // Resolving may allocate, read a file and sleep: interrupts
            // back on. Safe here, the fault came from ring 3 or from
            // `user_copy`, and neither runs with a lock held.
            unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
            let resolved = space.resolve(cr2, write, exec).is_ok();
            unsafe { core::arch::asm!("cli", options(nomem, nostack)) };
            if resolved {
                STATS[0].fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        if in_copy {
            STATS[1].fetch_add(1, Ordering::Relaxed);
            frame.rip = &raw const user_copy_fixup as u64;
            return;
        }
        if let Some(space) = unsafe { current() } {
            crate::serial_println!("[vm] faute fatale 0x{:x} (erreur 0x{:x}) : {}", cr2, frame.error, space.describe(cr2));
            // Where the code was, and what it held: enough to find the
            // instruction in the library and the pointer it followed.
            crate::serial_println!("[vm]   RIP 0x{:x} dans {}", frame.rip, space.describe(frame.rip));
            crate::serial_println!(
                "[vm]   rax {:x} rbx {:x} rcx {:x} rdx {:x} rsi {:x} rdi {:x} rbp {:x} rsp {:x}",
                frame.rax, frame.rbx, frame.rcx, frame.rdx, frame.rsi, frame.rdi, frame.rbp, frame.rsp
            );
            crate::serial_println!(
                "[vm]   r8 {:x} r9 {:x} r10 {:x} r11 {:x} r12 {:x} r13 {:x} r14 {:x} r15 {:x}",
                frame.r8, frame.r9, frame.r10, frame.r11, frame.r12, frame.r13, frame.r14, frame.r15
            );
            for (reg, value) in [("rdi", frame.rdi), ("rsi", frame.rsi), ("rdx", frame.rdx)] {
                space.print_string_at(reg, value);
            }
            space.print_return_addresses(frame.rsp);
            space.verify_cached_pages();
        }
        STATS[2].fetch_add(1, Ordering::Relaxed);
        crate::idt::user_exception_kernel_gs(14, "Faute de page", frame.rip, Some(cr2));
    }

    if from_user {
        STATS[2].fetch_add(1, Ordering::Relaxed);
        crate::idt::user_exception_kernel_gs(14, "Faute de page", frame.rip, Some(cr2));
    }

    report_kernel_fault(frame, cr2);
    crate::idt::kernel_page_fault(frame.rip, frame.rsp, frame.error, cr2)
}

/// What the serial console gets of a fault the kernel cannot recover
/// from: the task, the registers and the return addresses found on the
/// stack -- a fault seen once in a hundred boots must be diagnosable from
/// its log alone (`addr2line -e build/nova-os.bin` on each address).
fn report_kernel_fault(frame: &FaultFrame, cr2: u64) {
    crate::serial_println!(
        "[vm] faute noyau 0x{:x} (erreur 0x{:x}) - tache {} sur le coeur {} - RIP 0x{:x}",
        cr2,
        frame.error,
        crate::scheduler::current_pid(),
        crate::smp::current_cpu_id(),
        frame.rip
    );
    crate::serial_println!(
        "[vm]   rax {:x} rbx {:x} rcx {:x} rdx {:x} rsi {:x} rdi {:x} rbp {:x} rsp {:x}",
        frame.rax, frame.rbx, frame.rcx, frame.rdx, frame.rsi, frame.rdi, frame.rbp, frame.rsp
    );
    crate::serial_println!(
        "[vm]   r8 {:x} r9 {:x} r10 {:x} r11 {:x} r12 {:x} r13 {:x} r14 {:x} r15 {:x}",
        frame.r8, frame.r9, frame.r10, frame.r11, frame.r12, frame.r13, frame.r14, frame.r15
    );
    unsafe extern "C" {
        static __kernel_start: u8;
        static __text_end: u8;
    }
    let text = (&raw const __kernel_start as u64)..(&raw const __text_end as u64);
    let heap = crate::heap::HEAP_START..crate::heap::HEAP_START + crate::heap::HEAP_SIZE;
    // Kernel stacks live in the heap, which is mapped whole: the words
    // above RSP can be read as long as they stay inside it.
    if !heap.contains(&frame.rsp) || frame.rsp % 8 != 0 {
        return;
    }
    let mut found = 0;
    let mut at = frame.rsp;
    while at + 8 <= heap.end && at < frame.rsp + 4096 && found < 24 {
        // SAFETY: inside the heap, mapped from boot to halt.
        let word = unsafe { core::ptr::read_volatile(at as *const u64) };
        if text.contains(&word) {
            crate::serial_println!("[vm]   pile +0x{:x} : 0x{:x}", at - frame.rsp, word);
            found += 1;
        }
        at += 8;
    }
}

// ============================================================
//  Copies between kernel and user memory
// ============================================================

/// Copies `dst.len()` bytes from user address `src`.
pub fn copy_from_user(dst: &mut [u8], src: u64) -> Result<(), VmError> {
    check_user_range(src, dst.len())?;
    let left = unsafe { user_copy(dst.as_mut_ptr(), src as *const u8, dst.len()) };
    if left == 0 { Ok(()) } else { Err(VmError::Fault) }
}

/// Copies `src` to user address `dst`.
pub fn copy_to_user(dst: u64, src: &[u8]) -> Result<(), VmError> {
    check_user_range(dst, src.len())?;
    let left = unsafe { user_copy(dst as *mut u8, src.as_ptr(), src.len()) };
    if left == 0 { Ok(()) } else { Err(VmError::Fault) }
}

/// Reads a plain value from user memory.
pub fn read_user<T: Copy + Default>(addr: u64) -> Result<T, VmError> {
    let mut value = T::default();
    let bytes = unsafe { core::slice::from_raw_parts_mut(&mut value as *mut T as *mut u8, core::mem::size_of::<T>()) };
    copy_from_user(bytes, addr)?;
    Ok(value)
}

/// Writes a plain value to user memory.
pub fn write_user<T: Copy>(addr: u64, value: &T) -> Result<(), VmError> {
    let bytes = unsafe { core::slice::from_raw_parts(value as *const T as *const u8, core::mem::size_of::<T>()) };
    copy_to_user(addr, bytes)
}

/// Reads a NUL-terminated string of at most `max` bytes.
pub fn read_user_cstr(addr: u64, max: usize) -> Result<Vec<u8>, VmError> {
    let mut out = Vec::new();
    let mut at = addr;
    let mut chunk = [0u8; 64];
    while out.len() < max {
        // Never read across a page boundary in one go: the next page may
        // legitimately be unmapped once the terminator is found.
        let room = (PAGE - (at % PAGE)).min(chunk.len() as u64) as usize;
        copy_from_user(&mut chunk[..room], at)?;
        if let Some(nul) = chunk[..room].iter().position(|b| *b == 0) {
            out.extend_from_slice(&chunk[..nul]);
            return Ok(out);
        }
        out.extend_from_slice(&chunk[..room]);
        at += room as u64;
    }
    Err(VmError::Invalid)
}

fn check_user_range(addr: u64, len: usize) -> Result<(), VmError> {
    let end = addr.checked_add(len as u64).ok_or(VmError::Fault)?;
    if len != 0 && (addr < USER_MIN || end > USER_TOP) {
        return Err(VmError::Fault);
    }
    Ok(())
}

// ============================================================
//  Entering ring 3 with a full register set
// ============================================================

/// Registers of a ring 3 start, in the order `enter_user` reads them.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct UserRegs {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rip: u64,
    pub rsp: u64,
    pub rflags: u64,
}

/// Start parameters of user tasks not yet started, by PID.
static STARTS: SpinLock<BTreeMap<u64, UserRegs>> = SpinLock::new(BTreeMap::new());

/// Creates a task that enters ring 3 in `space` with `regs`, `fs_base`
/// as its TLS base. The task is created dormant, given its space and
/// start registers, then released: no core can run it before both are
/// in place.
pub fn spawn_user(
    name: &str,
    binary_type: crate::process::BinaryType,
    space: Arc<Space>,
    regs: UserRegs,
    fs_base: u64,
) -> u64 {
    spawn_user_with(name, binary_type, space, regs, fs_base, |_| {})
}

/// Same, with `prepare` run on the new PID before the task is released:
/// the Linux subsystem attaches the thread there, and a `clone` writes
/// the child's TID where the parent asked.
pub fn spawn_user_with(
    name: &str,
    binary_type: crate::process::BinaryType,
    space: Arc<Space>,
    regs: UserRegs,
    fs_base: u64,
    prepare: impl FnOnce(u64),
) -> u64 {
    let (pid, _) = crate::scheduler::spawn_dormant(name, binary_type, user_start);
    STARTS.lock().insert(pid as u64, regs);
    crate::scheduler::set_user_context(pid, Some(space), fs_base);
    prepare(pid as u64);
    crate::scheduler::resume(pid);
    pid as u64
}

fn user_start() -> ! {
    let pid = crate::scheduler::current_pid() as u64;
    let regs = STARTS.lock().remove(&pid);
    let Some(regs) = regs else {
        crate::serial_println!("[vm] tache {} sans registres de depart - abandon", pid);
        crate::scheduler::exit_current_with(-1)
    };
    // A translation of a recycled range may still sit in this core's
    // TLB: the same rule as `loader::user_task_entry`.
    memory::sync_tlb();
    unsafe { enter_user(&regs) }
}

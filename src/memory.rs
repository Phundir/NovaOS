//! Virtual memory management: 4-level page tables.
//!
//! In long mode with PAE, a 48-bit virtual address splits as follows:
//!
//! ```text
//!  63        48 47      39 38      30 29      21 20      12 11         0
//! +------------+----------+----------+----------+----------+------------+
//! |    sign    |   PML4   |   PDPT   |    PD    |    PT    |   offset   |
//! | extension  |  9 bits  |  9 bits  |  9 bits  |  9 bits  |  12 bits   |
//! +------------+----------+----------+----------+----------+------------+
//! ```
//!
//! Each table occupies a 4 KiB page and holds 512 entries of 8 bytes. An
//! entry may point at the table of the level below, or -- if the HUGE bit
//! is set -- describe directly a 1 GiB page (PDPT) or a 2 MiB one (PD).

use core::arch::asm;
use core::ops::{Index, IndexMut};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Base of the window giving access to all of physical memory.
/// Installed by `boot.asm`: virtual = physical + PHYS_OFFSET.
pub const PHYS_OFFSET: u64 = 0xFFFF_8000_0000_0000;

/// Virtual base the kernel is linked at.
pub const KERNEL_VMA: u64 = 0xFFFF_FFFF_8000_0000;

/// Mask isolating the physical address in a table entry.
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// Translates a physical address into a usable virtual one.
#[inline]
pub const fn phys_to_virt(phys: u64) -> u64 {
    phys + PHYS_OFFSET
}

/// Translates a kernel virtual address into a physical one.
/// Valid only for the region linked at KERNEL_VMA.
#[inline]
pub const fn kernel_virt_to_phys(virt: u64) -> u64 {
    virt - KERNEL_VMA
}

// ============================================================
//  Extent of the physical window
// ============================================================

const GIB: u64 = 1 << 30;

/// What `boot.asm` maps before any memory map is known: 4 pages of 1 GiB.
/// It holds every table and register the early boot touches, including
/// the LAPIC and I/O APIC just under 4 GiB.
const BOOT_WINDOW_END: u64 = 4 * GIB;

/// Upper bound of the window: the 64 GiB the frame allocator can manage.
/// One PDPT of 1 GiB pages could reach 512 GiB; no frame beyond 64 GiB
/// would ever be handed out, so mapping it would only map MMIO.
pub const WINDOW_MAX: u64 = 64 * GIB;

/// Current end of the window, exclusive. Only grows, and only once.
static WINDOW_END: AtomicU64 = AtomicU64::new(BOOT_WINDOW_END);

/// Physical addresses below this bound are reachable through
/// `phys_to_virt`. Never less than 4 GiB.
pub fn window_end() -> u64 {
    WINDOW_END.load(Ordering::Acquire)
}

/// Extends the physical window up to the end of the RAM.
///
/// With more than 3 GiB of RAM, the firmware moves the rest above 4 GiB,
/// past the PCI hole: that RAM exists in the memory map and the frame
/// allocator would hand it out, but `phys_to_virt` would lead nowhere and
/// the first write would fault. The window therefore follows the RAM, by
/// whole gibibytes, in the same 1 GiB pages as `boot.asm`.
///
/// The window stops at the end of the RAM rather than at `WINDOW_MAX`: a
/// 64-bit PCI BAR is placed above the RAM, and a cached mapping of MMIO
/// invites speculative reads the device never asked for. Drivers map
/// their registers explicitly, uncached, as before.
///
/// New entries fill slots that were not present, so no TLB can hold a
/// stale translation for them: no flush is needed. Secondary cores share
/// the same PDPT through PML4[256] and see the entries as soon as written.
///
/// Returns the new end of the window.
///
/// # Safety
/// To be called once, on the bootstrap core, with the window of
/// `boot.asm` active and before any address above 4 GiB is dereferenced.
pub unsafe fn extend_window(ram_end: u64) -> u64 {
    let target = ram_end.div_ceil(GIB).saturating_mul(GIB).min(WINDOW_MAX);
    if target <= BOOT_WINDOW_END {
        return window_end();
    }

    let p4 = unsafe { active_level4_table() };
    let entry = p4[table_indices(PHYS_OFFSET)[0]];
    // SAFETY: PML4[256] was installed by boot.asm and points at the PDPT
    // of the window, itself reachable through the first 4 GiB.
    let p3 = unsafe { &mut *(phys_to_virt(entry.addr()) as *mut PageTable) };

    let mut gib = BOOT_WINDOW_END / GIB;
    while gib < target / GIB {
        if !p3[gib as usize].is_present() {
            p3[gib as usize].set(gib * GIB, flags::PRESENT | flags::WRITABLE | flags::HUGE);
        }
        gib += 1;
    }

    WINDOW_END.store(target, Ordering::Release);
    target
}

// ============================================================
//  Table entry
// ============================================================

pub mod flags {
    pub const PRESENT: u64 = 1 << 0;
    pub const WRITABLE: u64 = 1 << 1;
    pub const USER: u64 = 1 << 2;
    pub const WRITE_THROUGH: u64 = 1 << 3;
    pub const NO_CACHE: u64 = 1 << 4;
    pub const ACCESSED: u64 = 1 << 5;
    pub const DIRTY: u64 = 1 << 6;
    pub const HUGE: u64 = 1 << 7;
    pub const GLOBAL: u64 = 1 << 8;
    pub const NO_EXECUTE: u64 = 1 << 63;
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct PageTableEntry(u64);

impl PageTableEntry {
    pub const fn empty() -> Self {
        PageTableEntry(0)
    }

    pub fn raw(&self) -> u64 {
        self.0
    }

    pub fn is_unused(&self) -> bool {
        self.0 == 0
    }

    pub fn is_present(&self) -> bool {
        self.0 & flags::PRESENT != 0
    }

    pub fn is_writable(&self) -> bool {
        self.0 & flags::WRITABLE != 0
    }

    pub fn is_user(&self) -> bool {
        self.0 & flags::USER != 0
    }

    pub fn is_huge(&self) -> bool {
        self.0 & flags::HUGE != 0
    }

    /// Bit 63 only means anything once EFER.NXE is armed; as long as it
    /// is not, no entry carries it and the answer is always
    /// "executable".
    pub fn is_no_execute(&self) -> bool {
        self.0 & flags::NO_EXECUTE != 0
    }

/// Physical address pointed at by this entry.
    pub fn addr(&self) -> u64 {
        self.0 & ADDR_MASK
    }

    pub fn set(&mut self, addr: u64, flags: u64) {
        self.0 = (addr & ADDR_MASK) | flags;
    }

    /// Adds flags without touching the address.
    pub fn add_flags(&mut self, extra: u64) {
        self.0 |= extra;
    }

    pub fn clear(&mut self) {
        self.0 = 0;
    }
}

// ============================================================
//  Table
// ============================================================

#[repr(C, align(4096))]
pub struct PageTable {
    entries: [PageTableEntry; 512],
}

impl PageTable {
    pub const fn new() -> Self {
        PageTable {
            entries: [PageTableEntry::empty(); 512],
        }
    }

    pub fn iter(&self) -> core::slice::Iter<'_, PageTableEntry> {
        self.entries.iter()
    }

    /// Number of entries actually in use.
    pub fn used_count(&self) -> usize {
        self.entries.iter().filter(|e| !e.is_unused()).count()
    }
}

impl Index<usize> for PageTable {
    type Output = PageTableEntry;
    fn index(&self, index: usize) -> &PageTableEntry {
        &self.entries[index]
    }
}

impl IndexMut<usize> for PageTable {
    fn index_mut(&mut self, index: usize) -> &mut PageTableEntry {
        &mut self.entries[index]
    }
}

// ============================================================
//  Access to the active hierarchy
// ============================================================

/// CR3 holds the physical address of the current PML4.
pub fn read_cr3() -> u64 {
    let value: u64;
    unsafe { asm!("mov {}, cr3", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

/// Reloads CR3, which invalidates the whole TLB.
///
/// # Safety
/// The hierarchy pointed at must stay valid, on pain of a triple fault.
pub unsafe fn flush_tlb() {
    unsafe {
        let cr3 = read_cr3();
        asm!("mov cr3, {}", in(reg) cr3, options(nostack, preserves_flags));
    }
}

// ============================================================
//  Cross-core TLB invalidation (shootdown)
// ============================================================

/// Number of times a translation has been taken down since boot.
///
/// The counter is the whole mechanism: a core whose own copy is behind
/// this value holds at least one stale translation and must reload CR3.
/// A list of the addresses concerned would be more precise and would have
/// to be bounded, drained and synchronised -- for an event that happens a
/// handful of times per boot, the counter costs nothing and cannot be
/// wrong.
static TLB_GENERATION: AtomicU64 = AtomicU64::new(0);

/// What each core has already taken into account.
static CPU_TLB_SEEN: [AtomicU64; crate::gdt::MAX_CPUS] =
    [const { AtomicU64::new(0) }; crate::gdt::MAX_CPUS];

/// Generation each core has been asked to reach: raised for every core by
/// a global shootdown, for the cores running one space only by
/// `shootdown_space_and_wait`. A core flushes only when it is behind
/// **its own** request.
static CPU_TLB_WANTED: [AtomicU64; crate::gdt::MAX_CPUS] =
    [const { AtomicU64::new(0) }; crate::gdt::MAX_CPUS];

/// Asks `cpu` to flush up to generation `generation`.
fn request_flush(cpu: usize, generation: u64) {
    CPU_TLB_WANTED[cpu].fetch_max(generation, Ordering::AcqRel);
}

/// Shootdowns raised since boot.
static SHOOTDOWNS: AtomicU64 = AtomicU64::new(0);

/// Counters: (shootdowns raised, cores still behind).
pub fn tlb_stats() -> (u64, usize) {
    (SHOOTDOWNS.load(Ordering::Relaxed), tlb_late_cores())
}

/// Brings the TLB of the current core up to date, if it is behind.
///
/// Called from three places, and the three are not redundant:
///
/// * the **shootdown IPI**, which is what makes it prompt;
/// * the **timer tick**, which is what makes it certain even for a core
///   that was masking interrupts when the IPI went out;
/// * **just before entering ring 3**, which is the point where being
///   late actually costs something -- a task starts executing at an
///   address the arena has just re-used, and a core that still held the
///   previous occupant's translation would run the previous occupant's
///   bytes.
///
/// That third call is not a precaution: it is the bug this whole
/// mechanism exists for. A foreign binary loaded into a recycled arena
/// slot and started on another core faulted at the fifth byte of its
/// entry point, reading an address belonging to the image before it.
///
/// Takes no lock and allocates nothing: it runs inside an interrupt
/// handler.
pub fn sync_tlb() {
    let cpu = crate::smp::current_cpu_id();
    if cpu >= crate::gdt::MAX_CPUS {
        return;
    }
    let wanted = CPU_TLB_WANTED[cpu].load(Ordering::Acquire);
    if CPU_TLB_SEEN[cpu].load(Ordering::Relaxed) >= wanted {
        return;
    }
    unsafe { flush_tlb() };
    // Stored **after** the flush: the reverse order would let another
    // core conclude we were up to date while our TLB still held the old
    // entry. And only ever raised: an interrupt's own `sync_tlb` may have
    // stored a later generation meanwhile.
    CPU_TLB_SEEN[cpu].fetch_max(wanted, Ordering::AcqRel);
}

/// Announces that a translation has been taken down.
///
/// ## Why nothing is waited for
///
/// The first version waited for every core to acknowledge, with a spin
/// budget as a guard against deadlock. It was both unsafe and ruinous:
/// unsafe because the caller holds the arena lock and a core spinning for
/// that same lock cannot answer, ruinous because `unmap_page` is called
/// **once per page** -- releasing an eighteen-page image meant eighteen
/// waits, each of them a million spins with interrupts masked. Under four
/// cores that is where the machine went.
///
/// And the wait buys nothing, because of what the stale entry can
/// actually reach. It names a **user** page: no kernel code addresses it,
/// so a core that has not yet flushed cannot misuse it. The only code
/// that can is a ring 3 task, and every path into ring 3 calls `sync_tlb`
/// on its own account -- see `loader::user_task_entry`.
///
/// So three things make the invalidation certain without any wait:
///
/// * the IPI, which reaches a core running ring 3 code immediately;
/// * the timer tick, which catches a core that was masking interrupts,
///   ten milliseconds later at worst;
/// * the synchronisation before entering ring 3, which is the only
///   moment where being late would cost anything at all.
fn shootdown() {
    let generation = TLB_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    SHOOTDOWNS.fetch_add(1, Ordering::Relaxed);
    for cpu in 0..crate::gdt::MAX_CPUS {
        request_flush(cpu, generation);
    }

    // The local core first: it is the one about to rely on the new state.
    sync_tlb();

    if crate::smp::online_count() <= 1 {
        return;
    }
    unsafe { crate::apic::broadcast_tlb_shootdown() };
}

/// Invalidation that **waits** until every online core has flushed.
///
/// `shootdown` alone is enough for the Nova arenas, where a stale entry
/// names a page nobody can still reach (see above). It is not enough for
/// a multithreaded process: while one thread unmaps a range, another may
/// be running on a second core with the old translation, and would keep
/// writing into a frame already handed to someone else. So the frames
/// of a private address space (`vm`) are freed only after this returns.
///
/// Must be called with no lock held and interrupts enabled: a core that
/// waits here with interrupts masked could never answer the IPI of a
/// core waiting for it.
pub fn shootdown_and_wait() {
    shootdown();
    let wanted = TLB_GENERATION.load(Ordering::Acquire);
    let cores = crate::smp::online_count().min(crate::gdt::MAX_CPUS);
    loop {
        let late = (0..cores).any(|cpu| CPU_TLB_SEEN[cpu].load(Ordering::Acquire) < wanted);
        if !late {
            return;
        }
        core::hint::spin_loop();
    }
}

/// `shootdown_and_wait` for one address space: only the cores that may
/// hold translations of the page table `cr3` are interrupted and waited
/// for.
///
/// ## Why that is enough
///
/// Translations of user pages are not global, and CR3 is reloaded at
/// every switch between two spaces (no PCID): a core that has since
/// loaded another page table holds nothing of this one. The cores that
/// may are those whose CR3 is, or is about to become, this one
/// (`vm::may_hold`); `vm::activate` announces the new value **before**
/// loading it, so a core not seen here loads its CR3 after the entries
/// changed and walks the new ones.
///
/// ## Why it matters
///
/// The broadcast interrupted and waited for every core -- eight under
/// `make run`, most of them idle or running another program -- at every
/// `munmap`, `mprotect` and `madvise` of every process: 9 to 17 us each
/// under KVM against 1 on the host (tools/ports/tests/sysbench.c), and
/// a full TLB flush on eight cores each time. A page load of the Web
/// Browser does dozens, a heavy one thousands.
///
/// Called with no lock held and interrupts enabled, like
/// `shootdown_and_wait`.
pub fn shootdown_space_and_wait(cr3: u64) {
    // The entries were changed before this call; their writes must be
    // visible before the cores' CR3 are read (see `vm::activate`, which
    // publishes before it loads).
    core::sync::atomic::fence(Ordering::SeqCst);
    let generation = TLB_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    SHOOTDOWNS.fetch_add(1, Ordering::Relaxed);
    let me = crate::smp::current_cpu_id();
    let cores = crate::smp::online_count().min(crate::gdt::MAX_CPUS);
    let mut targets = [false; crate::gdt::MAX_CPUS];
    for cpu in 0..cores {
        if cpu != me && crate::vm::may_hold(cpu, cr3) {
            targets[cpu] = true;
            request_flush(cpu, generation);
            if let Some(lapic) = crate::smp::lapic_of(cpu) {
                // SAFETY: VECTOR_TLB has its handler (`idt::tlb_shootdown_ipi`).
                unsafe { crate::apic::send_tlb_shootdown(lapic) };
            }
        }
    }
    request_flush(me, generation);
    sync_tlb();
    loop {
        let late = (0..cores).any(|cpu| targets[cpu] && CPU_TLB_SEEN[cpu].load(Ordering::Acquire) < generation);
        if !late {
            return;
        }
        core::hint::spin_loop();
    }
}

/// Cores that have not yet taken the latest invalidation into account.
///
/// Zero at rest. A value that stays non-zero means a core is not taking
/// its timer interrupts, which is a far worse problem than a stale TLB.
pub fn tlb_late_cores() -> usize {
    (0..crate::smp::online_count().min(crate::gdt::MAX_CPUS))
        .filter(|cpu| CPU_TLB_SEEN[*cpu].load(Ordering::Acquire) < CPU_TLB_WANTED[*cpu].load(Ordering::Acquire))
        .count()
}

/// Invalidates a single TLB entry -- cheaper than a reload.
///
/// # Safety
/// No dangerous effect, but assumes a canonical address.
pub unsafe fn flush_page(virt: u64) {
    unsafe {
        asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags));
    }
}

/// Reference on the active PML4, through the physical window.
///
/// # Safety
/// The caller must guarantee that no other mutable alias exists.
pub unsafe fn active_level4_table() -> &'static mut PageTable {
    let phys = read_cr3() & ADDR_MASK;
    unsafe { &mut *(phys_to_virt(phys) as *mut PageTable) }
}

/// Splits a virtual address into its four table indexes.
pub const fn table_indices(virt: u64) -> [usize; 4] {
    [
        ((virt >> 39) & 0x1FF) as usize,
        ((virt >> 30) & 0x1FF) as usize,
        ((virt >> 21) & 0x1FF) as usize,
        ((virt >> 12) & 0x1FF) as usize,
    ]
}

/// Result of a successful translation.
#[derive(Debug, Clone, Copy)]
pub struct Translation {
    pub phys: u64,
    /// Size of the page covering the address: 4 KiB, 2 MiB or 1 GiB.
    pub page_size: u64,
    /// Is the page reachable from ring 3?
    ///
    /// Conjunction of the four levels: the hardware demands the USER bit
    /// across the whole hierarchy, a page marked USER under a table that
    /// is not stays unreachable from ring 3.
    pub user: bool,
    /// Is the page writable? Same conjunction.
    pub writable: bool,
    /// Is the page executable? Disjunction, this time: the NO_EXECUTE bit
    /// of a single level is enough to forbid execution.
    pub executable: bool,
}

/// Walks the four levels to translate a virtual address.
///
/// # Safety
/// Requires a valid physical window at PHYS_OFFSET.
pub unsafe fn translate(virt: u64) -> Option<Translation> {
    let idx = table_indices(virt);

    let p4 = unsafe { active_level4_table() };
    let e4 = p4[idx[0]];
    if !e4.is_present() {
        return None;
    }
    let mut user = e4.is_user();
    let mut writable = e4.is_writable();
    let mut executable = !e4.is_no_execute();

    let p3 = unsafe { &*(phys_to_virt(e4.addr()) as *const PageTable) };
    let e3 = p3[idx[1]];
    if !e3.is_present() {
        return None;
    }
    user &= e3.is_user();
    writable &= e3.is_writable();
    executable &= !e3.is_no_execute();
    if e3.is_huge() {
        return Some(Translation {
            phys: e3.addr() + (virt & 0x3FFF_FFFF),
            page_size: 0x4000_0000,
            user,
            writable,
            executable,
        });
    }

    let p2 = unsafe { &*(phys_to_virt(e3.addr()) as *const PageTable) };
    let e2 = p2[idx[2]];
    if !e2.is_present() {
        return None;
    }
    user &= e2.is_user();
    writable &= e2.is_writable();
    executable &= !e2.is_no_execute();
    if e2.is_huge() {
        return Some(Translation {
            phys: e2.addr() + (virt & 0x1F_FFFF),
            page_size: 0x20_0000,
            user,
            writable,
            executable,
        });
    }

    let p1 = unsafe { &*(phys_to_virt(e2.addr()) as *const PageTable) };
    let e1 = p1[idx[3]];
    if !e1.is_present() {
        return None;
    }
    user &= e1.is_user();
    writable &= e1.is_writable();
    executable &= !e1.is_no_execute();

    Some(Translation {
        phys: e1.addr() + (virt & 0xFFF),
        page_size: 0x1000,
        user,
        writable,
        executable,
    })
}

// ============================================================
//  Building mappings
// ============================================================

/// Descends one level, creating the intermediate table if needed.
///
/// The flags passed are *added* to an existing entry: the hardware
/// permissions being the conjunction of every level, an intermediate
/// table without the USER bit would make the final page unreachable from
/// ring 3, even marked USER itself.
///
/// # Safety
/// Physical window active and frame allocator initialised.
unsafe fn next_table_or_create(
    table: &mut PageTable,
    index: usize,
    extra_flags: u64,
) -> Option<&'static mut PageTable> {
    if !table[index].is_present() {
        let frame = crate::frame::allocate_zeroed_frame()?;
        table[index].set(frame.start_addr(), flags::PRESENT | flags::WRITABLE | extra_flags);
    } else {
        table[index].add_flags(extra_flags);
    }

    if table[index].is_huge() {
        return None;
    }

    let addr = phys_to_virt(table[index].addr());
    Some(unsafe { &mut *(addr as *mut PageTable) })
}

/// Establishes a 4 KiB mapping, creating the missing tables.
///
/// # Safety
/// Overwriting an existing mapping can corrupt kernel memory.
pub unsafe fn map_page(virt: u64, phys: u64, page_flags: u64) -> Result<(), &'static str> {
    let idx = table_indices(virt);
    // The intermediate levels inherit the USER bit when the final page
    // carries it, failing which ring 3 would stay blocked at level one.
    let intermediate = page_flags & flags::USER;

    let p4 = unsafe { active_level4_table() };
    let p3 = unsafe { next_table_or_create(p4, idx[0], intermediate) }
        .ok_or("impossible de creer la PDPT")?;
    let p2 = unsafe { next_table_or_create(p3, idx[1], intermediate) }
        .ok_or("impossible de creer la PD")?;
    let p1 = unsafe { next_table_or_create(p2, idx[2], intermediate) }
        .ok_or("impossible de creer la PT")?;

    p1[idx[3]].set(phys, page_flags | flags::PRESENT);
    unsafe { flush_page(virt) };
    Ok(())
}

/// 2.8.3 -- Maps a page reachable from user space.
///
/// `executable` only has an effect if the NXE bit of EFER has been armed
/// by `enable_nx`: without it, bit 63 of an entry is reserved and setting
/// it triggers a page fault at the first translation.
///
/// # Safety
/// The address must belong to the lower half of the address space.
pub unsafe fn map_user_page(
    virt: u64,
    phys: u64,
    writable: bool,
    executable: bool,
) -> Result<(), &'static str> {
    let mut page_flags = flags::PRESENT | flags::USER;
    if writable {
        page_flags |= flags::WRITABLE;
    }
    if !executable && nx_enabled() {
        page_flags |= flags::NO_EXECUTE;
    }
    unsafe { map_page(virt, phys, page_flags) }
}

// ============================================================
//  NX bit -- W^X support
// ============================================================

const IA32_EFER: u32 = 0xC000_0080;
/// EFER.NXE -- makes bit 63 of the table entries interpretable.
const EFER_NXE: u64 = 1 << 11;

/// Set once the MSR has been written on the boot core. The secondary
/// cores arm their own, but the flag stays global: a single set of page
/// tables is shared, so the entries must be coherent for everyone.
static NX_ENABLED: AtomicBool = AtomicBool::new(false);

/// Can the processor interpret the NX bit? CPUID 0x80000001, EDX bit 20.
pub fn nx_supported() -> bool {
    let edx: u32;
    unsafe {
        asm!(
            "push rbx",
            "mov eax, 0x80000001",
            "cpuid",
            "pop rbx",
            out("edx") edx,
            out("eax") _,
            out("ecx") _,
            options(nostack, preserves_flags)
        );
    }
    edx & (1 << 20) != 0
}

pub fn nx_enabled() -> bool {
    NX_ENABLED.load(Ordering::Relaxed)
}

/// Arms EFER.NXE on the current core.
///
/// To be called on **every** core: EFER is a per-processor register.
/// Without it, a secondary core meeting an entry carrying bit 63 would
/// treat it as reserved and raise a page fault.
///
/// Harmless after the fact: no existing entry carries bit 63, so enabling
/// its interpretation changes no acquired permission.
///
/// # Safety
/// Modifies a control MSR of the current processor.
pub unsafe fn enable_nx() -> bool {
    if !nx_supported() {
        return false;
    }
    unsafe {
        let (high, low): (u32, u32);
        asm!("rdmsr", in("ecx") IA32_EFER, out("eax") low, out("edx") high,
             options(nomem, nostack, preserves_flags));
        let value = (((high as u64) << 32) | low as u64) | EFER_NXE;
        asm!("wrmsr", in("ecx") IA32_EFER, in("eax") value as u32,
             in("edx") (value >> 32) as u32, options(nostack, preserves_flags));
    }
    NX_ENABLED.store(true, Ordering::Relaxed);
    true
}

/// CR0.WP: ring 0 honours the read-only bit of pages.
const CR0_WP: u64 = 1 << 16;

/// Makes the kernel obey read-only pages (CR0.WP), on the calling core.
///
/// Without it a write from ring 0 goes through a read-only entry as if it
/// were writable. That was harmless while no user page was read-only for
/// a reason the kernel had to respect; copy-on-write (11.5.6) is such a
/// reason. A forked child calling `sigprocmask` got the old mask written
/// by the kernel **into the frame it still shared with its parent** --
/// the parent's stack changed under it, and its allocator later found a
/// chunk header of zeros. With WP set the kernel's write faults, and the
/// fault handler copies the page first, as for a write from ring 3.
///
/// # Safety
/// Every kernel write to a read-only page faults from now on: the kernel
/// must write user pages only through `vm::user_copy` or the physical
/// window.
pub unsafe fn enable_write_protect() {
    unsafe {
        let mut cr0: u64;
        asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack, preserves_flags));
        cr0 |= CR0_WP;
        asm!("mov cr0, {}", in(reg) cr0, options(nomem, nostack, preserves_flags));
    }
}

pub fn write_protect_enabled() -> bool {
    let cr0: u64;
    unsafe { asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack, preserves_flags)) };
    cr0 & CR0_WP != 0
}

// ============================================================
//  PAT -- per-page cache types
// ============================================================

const IA32_PAT: u32 = 0x277;

/// Default value of the PAT MSR on every x86-64:
/// `WB, WT, UC-, UC, WB, WT, UC-, UC`, one byte per entry.
const PAT_DEFAULT: u64 = 0x0007_0406_0007_0406;

/// The same, entry 4 moved from WB (0x06) to WC (0x01).
///
/// The first four entries do not move: they are the ones the PCD/PWT
/// combinations of the already mapped pages select, and changing one
/// would reinterpret the whole existing mapping. Only entry 4, which no
/// page reached for want of a set PAT bit, becomes useful.
const PAT_WITH_WC: u64 = 0x0007_0401_0007_0406;

/// PAT bit of a 4 KiB page entry. Combined with PCD=0 and PWT=0, it
/// selects entry 4 of the MSR, hence write-combining.
pub const PAT_4K: u64 = 1 << 7;

/// Set on the boot core. The other cores program their own -- the MSR is
/// per-processor -- but the flag stays global: the page tables are
/// shared, so the cache types must be shared too.
static PAT_WC_READY: AtomicBool = AtomicBool::new(false);

/// Does the processor implement the PAT? CPUID.01H:EDX bit 16.
pub fn pat_supported() -> bool {
    let edx: u32;
    unsafe {
        asm!(
            "push rbx",
            "mov eax, 1",
            "cpuid",
            "pop rbx",
            out("edx") edx,
            out("eax") _,
            out("ecx") _,
            options(nostack, preserves_flags)
        );
    }
    edx & (1 << 16) != 0
}

/// Is write-combining available through entry 4 of the PAT?
pub fn pat_wc_available() -> bool {
    PAT_WC_READY.load(Ordering::Relaxed)
}

/// Programs the PAT of the current core to make write-combining
/// reachable.
///
/// ## Why
///
/// A framebuffer written UC sends one bus access per pixel: three million
/// PCI round trips for one image, that is an unusable display. Written
/// WB, it ends up in the cache and the image only appears at the mercy of
/// the evictions. Write-combining is exactly the middle ground the
/// hardware provides for this case: the writes accumulate in a buffer and
/// leave in bursts the size of a cache line, without ever being read
/// back.
///
/// ## Why Intel's complete sequence is not needed here
///
/// The manual describes a ritual -- disable the cache through CR0.CD,
/// flush, invalidate the TLB, write the MSR, restore -- meant for the
/// case where one **redefines an entry already used** by live pages. That
/// is not the case: entries 0 to 3 keep their meaning, and entry 4 was
/// reachable by no page, the PAT bit being set nowhere. Flushing the
/// caches and the TLB is therefore enough, and it is done.
///
/// # Safety
/// Writes a control MSR of the current processor. To be called before any
/// page carries the PAT bit.
pub unsafe fn init_pat() -> bool {
    if !pat_supported() {
        return false;
    }

    unsafe {
        // Nothing must stay in flight while the cache type changes.
        asm!("wbinvd", options(nostack, preserves_flags));
        asm!("wrmsr", in("ecx") IA32_PAT, in("eax") PAT_WITH_WC as u32,
             in("edx") (PAT_WITH_WC >> 32) as u32, options(nostack, preserves_flags));
        asm!("wbinvd", options(nostack, preserves_flags));
        flush_tlb();
    }

    PAT_WC_READY.store(true, Ordering::Relaxed);
    true
}

/// Raw PAT value of the current core -- diagnostics.
pub fn read_pat() -> u64 {
    let (high, low): (u32, u32);
    unsafe {
        asm!("rdmsr", in("ecx") IA32_PAT, out("eax") low, out("edx") high,
             options(nomem, nostack, preserves_flags));
    }
    ((high as u64) << 32) | low as u64
}

/// Default value, exposed so that the diagnostics can say whether the MSR
/// was really reprogrammed.
pub const fn pat_default() -> u64 {
    PAT_DEFAULT
}

/// Changes the rights of an already mapped page, without touching its frame.
///
/// The loader uses it to apply the final permissions once the content is
/// copied and the relocations laid down: writing into a code section
/// assumes keeping it writable for the duration of the load.
///
/// # Safety
/// The page must exist and belong to user space.
pub unsafe fn protect_user_page(
    virt: u64,
    writable: bool,
    executable: bool,
) -> Result<(), &'static str> {
    let idx = table_indices(virt);

    let p4 = unsafe { active_level4_table() };
    if !p4[idx[0]].is_present() {
        return Err("PML4 absente");
    }
    let p3 = unsafe { &mut *(phys_to_virt(p4[idx[0]].addr()) as *mut PageTable) };
    if !p3[idx[1]].is_present() || p3[idx[1]].is_huge() {
        return Err("PDPT absente ou en page geante");
    }
    let p2 = unsafe { &mut *(phys_to_virt(p3[idx[1]].addr()) as *mut PageTable) };
    if !p2[idx[2]].is_present() || p2[idx[2]].is_huge() {
        return Err("PD absente ou en page geante");
    }
    let p1 = unsafe { &mut *(phys_to_virt(p2[idx[2]].addr()) as *mut PageTable) };
    if !p1[idx[3]].is_present() {
        return Err("page absente");
    }

    let mut page_flags = flags::PRESENT | flags::USER;
    if writable {
        page_flags |= flags::WRITABLE;
    }
    if !executable && nx_enabled() {
        page_flags |= flags::NO_EXECUTE;
    }

    let phys = p1[idx[3]].addr();
    p1[idx[3]].set(phys, page_flags);
    unsafe { flush_page(virt) };
    Ok(())
}

/// Removes a 4 KiB mapping. The physical frame is not freed.
///
/// The intermediate tables that have become empty are, on the other
/// hand: a PT, a PD or a PDPT describing nothing any more has no reason
/// to hold a frame. Without this pruning, every application loaded then
/// closed would leave behind the three or four frames of its hierarchy --
/// a slow leak, invisible in the user page counters since no user page is
/// concerned.
///
/// The pruning is limited to the **lower half** of the address space. The
/// tables of the upper half describe the kernel, the physical window and
/// the heap: they are shared by every core and every task, and nothing
/// would ever justify taking them down.
///
/// # Safety
/// No code must still reference this address. On a multi-core machine,
/// the other processors may keep the old translation in their TLB: there
/// is no cross invalidation yet (see the known limits of the roadmap).
pub unsafe fn unmap_page(virt: u64) {
    let idx = table_indices(virt);

    let p4 = unsafe { active_level4_table() };
    if !p4[idx[0]].is_present() {
        return;
    }
    let p3 = unsafe { &mut *(phys_to_virt(p4[idx[0]].addr()) as *mut PageTable) };
    if !p3[idx[1]].is_present() || p3[idx[1]].is_huge() {
        return;
    }
    let p2 = unsafe { &mut *(phys_to_virt(p3[idx[1]].addr()) as *mut PageTable) };
    if !p2[idx[2]].is_present() || p2[idx[2]].is_huge() {
        return;
    }
    let p1 = unsafe { &mut *(phys_to_virt(p2[idx[2]].addr()) as *mut PageTable) };

    p1[idx[3]].clear();
    unsafe { flush_page(virt) };

    // The other cores may hold the old translation. Until step 10.4
    // nothing exercised that: images were only ever unloaded at boot,
    // before the scheduler ran. A foreign binary loaded into a recycled
    // arena slot and started on another core made it visible in the most
    // direct way there is -- it executed the previous occupant's code.
    shootdown();

    // Upward pruning, in user space only.
    if idx[0] >= 256 {
        return;
    }

    if p1.used_count() == 0 {
        unsafe { free_table(&mut p2[idx[2]]) };
        if p2.used_count() == 0 {
            unsafe { free_table(&mut p3[idx[1]]) };
            if p3.used_count() == 0 {
                unsafe { free_table(&mut p4[idx[0]]) };
            }
        }
        // A higher-level entry has disappeared: invalidating a single
        // page is no longer enough, the TLB may have cached intermediate
        // translations.
        unsafe { flush_tlb() };
    }
}

/// Returns the frame of a page table that has become empty and clears the
/// entry that named it.
///
/// # Safety
/// The table pointed at must be empty and no longer referenced.
unsafe fn free_table(entry: &mut PageTableEntry) {
    if !entry.is_present() || entry.is_huge() {
        return;
    }
    let frame = crate::frame::Frame::containing(entry.addr());
    entry.clear();
    unsafe { crate::frame::deallocate_frame(frame) };
}

/// Sets the PCD and PWT bits on the 1 GiB page covering a given physical
/// address, in the physical window.
///
/// Indispensable for MMIO: without it, a write to an APIC register could
/// stay in the cache and never reach the controller.
///
/// # Safety
/// The physical window must be active and the address belong to MMIO.
pub unsafe fn set_mmio_uncached(phys: u64) {
    let virt = phys_to_virt(phys);
    let idx = table_indices(virt);

    let p4 = unsafe { active_level4_table() };
    if !p4[idx[0]].is_present() {
        return;
    }

    let p3 = unsafe { &mut *(phys_to_virt(p4[idx[0]].addr()) as *mut PageTable) };
    let entry = p3[idx[1]];
    if !entry.is_present() {
        return;
    }

    p3[idx[1]].set(
        entry.addr(),
        entry.raw() & 0xFFF | flags::NO_CACHE | flags::WRITE_THROUGH,
    );

    unsafe { flush_tlb() };
}

// ============================================================
//  Diagnostics
// ============================================================

/// Logs on the serial console the active entries of the PML4.
///
/// # Safety
/// Same prerequisites as `translate`.
pub unsafe fn dump_level4() {
    let p4 = unsafe { active_level4_table() };
    crate::serial_println!(
        "[mem] PML4 physique 0x{:012x} - {} entrees actives",
        read_cr3() & ADDR_MASK,
        p4.used_count()
    );

    for i in 0..512 {
        let entry = p4[i];
        if entry.is_unused() {
            continue;
        }
        let mut base = (i as u64) << 39;
        if i >= 256 {
            base |= 0xFFFF_0000_0000_0000;
        }
        crate::serial_println!(
            "[mem]   PML4[{:3}] -> 0x{:012x}  base 0x{:016x}  {}{}{}",
            i,
            entry.addr(),
            base,
            if entry.is_present() { "P" } else { "-" },
            if entry.is_writable() { "W" } else { "-" },
            if entry.is_user() { "U" } else { "-" }
        );
    }
}

/// Displays a translation on both consoles.
///
/// # Safety
/// Same prerequisites as `translate`.
pub unsafe fn report_translation(label: &str, virt: u64) {
    match unsafe { translate(virt) } {
        Some(t) => {
            let unit = match t.page_size {
                0x4000_0000 => "1 Gio",
                0x20_0000 => "2 Mio",
                _ => "4 Kio",
            };
            crate::kprintln!(
                "  {:<14} 0x{:016x} -> 0x{:012x} ({}, {}, {}{})",
                label,
                virt,
                t.phys,
                unit,
                if t.user { "ring 3" } else { "ring 0" },
                if t.writable { "W" } else { "-" },
                if t.executable { "X" } else { "-" }
            );
        }
        None => crate::kprintln!("  {:<14} 0x{:016x} -> NON MAPPEE", label, virt),
    }
}

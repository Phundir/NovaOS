//! Multi-core: startup, identification and scheduling (spec 7.2).
//!
//! Startup sequence standardised by Intel: INIT, then two spaced SIPIs.
//! The second is only needed on some older processors, but it has no
//! effect if the first succeeded.
//!
//! Once started, each core initialises its GDT, its TSS, its LAPIC, its
//! timer and its system call gate, then waits for the boot core to
//! release scheduling before electing its first task.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicU8, AtomicUsize, Ordering};

use crate::acpi::AcpiInfo;
use crate::gdt::MAX_CPUS;
use crate::memory::{self, PHYS_OFFSET, phys_to_virt};
use crate::vga::SpinLock;

/// Physical address the trampoline is copied to. Must be 4 KiB aligned
/// and fit under 1 MiB, a real-mode constraint.
const TRAMPOLINE_PHYS: u64 = 0x8000;
/// SIPI vector: the start address is vector x 4096.
const TRAMPOLINE_VECTOR: u8 = (TRAMPOLINE_PHYS / 0x1000) as u8;

/// 64 KiB of startup stack per secondary core.
const AP_STACK_SIZE: usize = 64 * 1024;
/// 32 KiB of kernel stack per core, for system calls and interrupts
/// occurring in ring 3.
const SYSCALL_STACK_SIZE: usize = 32 * 1024;

unsafe extern "C" {
    static __trampoline_load: u8;
    static trampoline_start: u8;
    static trampoline_end: u8;
    static tramp_cr3: u8;
    static tramp_stack: u8;
    static tramp_entry: u8;
    static tramp_flag: u8;
}

// ============================================================
//  Identification of the current core
// ============================================================

/// Mapping from LAPIC identifier to internal core index.
///
/// LAPIC identifiers are in general neither contiguous nor ordered: this
/// table allows recovering the internal index from any context, with no
/// lock and no per-core variable.
static LAPIC_TO_CPU: [AtomicU8; 256] = [const { AtomicU8::new(0xFF) }; 256];

/// How `current_cpu_id` finds the core's index.
const BY_LAPIC: u8 = 0;
const BY_RDPID: u8 = 1;
const BY_RDTSCP: u8 = 2;
static CPU_ID_BY: AtomicU8 = AtomicU8::new(BY_LAPIC);

/// Per-core MSR read by `RDPID` and `RDTSCP`: Linux keeps the core's
/// number in it (low 12 bits, the NUMA node above), and so does Nova.
const MSR_TSC_AUX: u32 = 0xC000_0103;

/// Writes this core's index into `TSC_AUX`, and on the boot core chooses
/// how every core will read it back. Called by each core as soon as it
/// knows its index, before anything that could ask for it.
///
/// ## Why not the LAPIC any more
///
/// The index was the LAPIC identity register, read by MMIO. Under a
/// hypervisor a LAPIC MMIO read is an exit and an emulated instruction --
/// several microseconds -- and `current_cpu_id` runs several times per
/// system call (the Linux thread of the core, the scheduler lock, the
/// preemption flag). Measured with the same static program on the host
/// and under Nova (tools/ports/tests/sysbench.c): 49 ns for a bare system
/// call on the host, **6 800 ns** under Nova. `RDPID` reads the MSR the
/// processor keeps per core, with no exit; `RDTSCP` does as well on older
/// processors, at the price of the TSC read that comes with it.
///
/// # Safety
/// `cpu` must be the index of the core running this call.
unsafe fn publish_cpu_id(cpu: usize) {
    // CPUID leaves 7 and 0x8000_0001 exist on every x86_64 processor this
    // kernel starts on (leaf 7 answers zeros where it is absent).
    let rdpid = core::arch::x86_64::__cpuid_count(7, 0).ecx & (1 << 22) != 0;
    let rdtscp = core::arch::x86_64::__cpuid(0x8000_0001).edx & (1 << 27) != 0;
    if !rdpid && !rdtscp {
        return;
    }
    // SAFETY: TSC_AUX exists whenever RDPID or RDTSCP does, and only
    // those two instructions read it.
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") MSR_TSC_AUX,
            in("eax") cpu as u32,
            in("edx") 0u32,
            options(nomem, nostack, preserves_flags)
        );
    }
    if cpu == 0 {
        CPU_ID_BY.store(if rdpid { BY_RDPID } else { BY_RDTSCP }, Ordering::SeqCst);
    }
}

/// Internal index of the core running this call.
///
/// Read from the core's `TSC_AUX` once `publish_cpu_id` has run (see
/// there why), from the LAPIC identity register before that. Returns 0
/// as long as the APIC is not initialised, which correctly covers the
/// early boot phase.
#[inline]
pub fn current_cpu_id() -> usize {
    match CPU_ID_BY.load(Ordering::Relaxed) {
        BY_RDPID => {
            let aux: u64;
            // SAFETY: RDPID exists (checked by `publish_cpu_id`) and only
            // reads the core's TSC_AUX.
            unsafe { core::arch::asm!("rdpid {}", out(reg) aux, options(nomem, nostack, preserves_flags)) };
            return (aux & 0xFFF) as usize;
        }
        BY_RDTSCP => {
            let aux: u32;
            // SAFETY: RDTSCP exists (checked by `publish_cpu_id`).
            unsafe {
                core::arch::asm!(
                    "rdtscp",
                    out("eax") _,
                    out("edx") _,
                    out("ecx") aux,
                    options(nomem, nostack, preserves_flags)
                )
            };
            return (aux & 0xFFF) as usize;
        }
        _ => {}
    }
    if !crate::apic::is_initialized() {
        return 0;
    }
    let lapic = (crate::apic::id() & 0xFF) as usize;
    let cpu = LAPIC_TO_CPU[lapic].load(Ordering::Relaxed);
    if cpu == 0xFF { 0 } else { cpu as usize }
}

// ============================================================
//  Per-core data
// ============================================================

#[derive(Debug, Clone, Copy)]
pub struct CpuInfo {
    pub cpu_id: usize,
    pub lapic_id: u8,
    pub online: bool,
    /// Top of the kernel stack dedicated to ring 3 -> ring 0 transitions.
    pub kernel_stack: u64,
    /// Timer ticks received by this core -- lets one check that it lives.
    pub ticks: u64,
    /// Rescheduling IPIs received.
    pub reschedule_ipis: u64,
}

impl CpuInfo {
    const fn empty() -> Self {
        CpuInfo {
            cpu_id: 0,
            lapic_id: 0,
            online: false,
            kernel_stack: 0,
            ticks: 0,
            reschedule_ipis: 0,
        }
    }
}

static CPUS: SpinLock<[CpuInfo; MAX_CPUS]> = SpinLock::new([CpuInfo::empty(); MAX_CPUS]);

/// Lock-free counters, incremented from the interrupt handlers.
static CPU_TICKS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static CPU_IPIS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// Number of cores actually online, boot core included.
static ONLINE_COUNT: AtomicUsize = AtomicUsize::new(1);
/// Assignment of the internal indexes, in startup order.
static NEXT_CPU_ID: AtomicUsize = AtomicUsize::new(1);
/// Armed by the last core started, watched by the boot core.
static AP_READY: AtomicBool = AtomicBool::new(false);
/// Releases the secondary cores: as long as it is false, they wait.
static SCHEDULER_READY: AtomicBool = AtomicBool::new(false);

/// Stacks kept so that they are never freed.
static AP_STACKS: SpinLock<Vec<Box<[u8]>>> = SpinLock::new(Vec::new());

pub fn online_count() -> usize {
    ONLINE_COUNT.load(Ordering::Relaxed)
}

pub fn cpu_ticks(cpu: usize) -> u64 {
    CPU_TICKS[cpu.min(MAX_CPUS - 1)].load(Ordering::Relaxed)
}

/// Accounts one tick on the current core -- called from the handler.
#[inline]
pub fn record_tick(cpu: usize) {
    if cpu < MAX_CPUS {
        CPU_TICKS[cpu].fetch_add(1, Ordering::Relaxed);
    }
}

/// Accounts one rescheduling IPI.
#[inline]
pub fn record_ipi(cpu: usize) {
    if cpu < MAX_CPUS {
        CPU_IPIS[cpu].fetch_add(1, Ordering::Relaxed);
    }
}

/// LAPIC identifier of each online core, `LAPIC_NONE` before it comes
/// online: read without a lock by every IPI (`lapic_of`), where `CPUS`
/// cost a lock -- interrupts masked -- per cross-core wake-up.
static LAPIC_OF: [AtomicU16; MAX_CPUS] = [const { AtomicU16::new(LAPIC_NONE) }; MAX_CPUS];
const LAPIC_NONE: u16 = 0xFFFF;

/// LAPIC identifier of a given core.
pub fn lapic_of(cpu: usize) -> Option<u8> {
    match LAPIC_OF.get(cpu).map(|id| id.load(Ordering::Acquire)) {
        Some(LAPIC_NONE) | None => None,
        Some(id) => Some(id as u8),
    }
}

/// Forces a remote core to reconsider its election.
pub fn send_reschedule(cpu: usize) {
    if let Some(lapic) = lapic_of(cpu) {
        unsafe { crate::apic::send_reschedule(lapic) };
    }
}

/// Allows the secondary cores to enter scheduling.
pub fn release_scheduler() {
    SCHEDULER_READY.store(true, Ordering::SeqCst);
}

// ============================================================
//  Trampoline
// ============================================================

/// Writes a 64-bit value into the trampoline parameter block.
///
/// The trampoline symbols carry addresses based on 0x8000, which are
/// therefore the physical addresses directly after the copy.
unsafe fn write_param(symbol: &u8, value: u64) {
    let phys = symbol as *const u8 as u64;
    unsafe { (phys_to_virt(phys) as *mut u64).write_volatile(value) };
}

/// Copies the trampoline into low memory and fills in CR3.
///
/// # Safety
/// The identity mapping must cover TRAMPOLINE_PHYS.
unsafe fn install_trampoline() -> usize {
    let start = &raw const trampoline_start as u64;
    let end = &raw const trampoline_end as u64;
    let load = &raw const __trampoline_load as u64;
    let size = (end - start) as usize;

    unsafe {
        core::ptr::copy_nonoverlapping(
            phys_to_virt(load) as *const u8,
            phys_to_virt(TRAMPOLINE_PHYS) as *mut u8,
            size,
        );

        write_param(&tramp_cr3, memory::read_cr3());
        write_param(&tramp_entry, ap_entry as *const () as u64);
    }

    size
}

/// Reinstalls the identity mapping on PML4[0], removed in 2.2.
///
/// The trampoline runs at 0x8000 in physical addressing: without this
/// entry, the first memory access of the secondary core would cause a
/// triple fault.
///
/// # Safety
/// To be removed once every core is started.
unsafe fn enable_identity_mapping() {
    let p4 = unsafe { memory::active_level4_table() };
    let physmap_index = (PHYS_OFFSET >> 39) as usize & 0x1FF;
    let entry = p4[physmap_index];
    p4[0].set(entry.addr(), memory::flags::PRESENT | memory::flags::WRITABLE);
    unsafe { memory::flush_tlb() };
}

/// Removes the identity mapping once the cores are started.
///
/// # Safety
/// No core must still be running the trampoline.
unsafe fn disable_identity_mapping() {
    let p4 = unsafe { memory::active_level4_table() };
    p4[0].clear();
    unsafe { memory::flush_tlb() };
}

// ============================================================
//  Startup sequence
// ============================================================

/// Starts every core declared by the ACPI.
///
/// Returns the number of cores online at the end of the procedure. The
/// started cores then wait until `release_scheduler`.
///
/// # Safety
/// The LAPIC must be initialised and the timer calibrated (for the delays).
pub unsafe fn start_all(acpi: &AcpiInfo) -> usize {
    let bsp_lapic = crate::apic::id() as u8;

    // The boot core registers itself: without this entry, current_cpu_id()
    // would not yet be able to identify it.
    LAPIC_TO_CPU[bsp_lapic as usize].store(0, Ordering::SeqCst);
    // SAFETY: this is core 0, and the secondary cores are not started yet.
    unsafe { publish_cpu_id(0) };

    // Kernel stack of core 0, needed as soon as a ring 3 task runs.
    let bsp_stack = allocate_kernel_stack();
    unsafe {
        crate::gdt::set_kernel_stack(0, bsp_stack);
        crate::syscall::set_kernel_stack(0, bsp_stack);
    }

    {
        let mut cpus = CPUS.lock();
        cpus[0] = CpuInfo {
            cpu_id: 0,
            lapic_id: bsp_lapic,
            online: true,
            kernel_stack: bsp_stack,
            ticks: 0,
            reschedule_ipis: 0,
        };
    }
    LAPIC_OF[0].store(bsp_lapic as u16, Ordering::Release);

    if acpi.cpus.len() <= 1 {
        crate::serial_println!("[smp] un seul coeur declare - rien a demarrer");
        return 1;
    }

    unsafe { enable_identity_mapping() };
    let size = unsafe { install_trampoline() };
    crate::serial_println!(
        "[smp] trampoline de {} octets installe a 0x{:04x} (vecteur {})",
        size,
        TRAMPOLINE_PHYS,
        TRAMPOLINE_VECTOR
    );

    for entry in acpi.cpus.iter() {
        if entry.lapic_id == bsp_lapic {
            continue; // that is us
        }
        if !entry.enabled {
            crate::serial_println!("[smp] LAPIC {} desactive - ignore", entry.lapic_id);
            continue;
        }
        if NEXT_CPU_ID.load(Ordering::Relaxed) >= MAX_CPUS {
            crate::serial_println!("[smp] limite de {} coeurs atteinte", MAX_CPUS);
            break;
        }

        unsafe { start_one(entry.lapic_id) };
    }

    unsafe { disable_identity_mapping() };

    ONLINE_COUNT.load(Ordering::Relaxed)
}

/// Allocates a kernel stack and returns its aligned top.
///
/// Deliberately leaked: it must survive the whole lifetime of the core,
/// including during system calls.
fn allocate_kernel_stack() -> u64 {
    let stack = Box::leak(alloc::vec![0u8; SYSCALL_STACK_SIZE].into_boxed_slice());
    (stack.as_ptr() as u64 + SYSCALL_STACK_SIZE as u64) & !0xF
}

/// Starts a single core and waits for its confirmation.
unsafe fn start_one(lapic_id: u8) {
    let mut stack = alloc::vec![0u8; AP_STACK_SIZE].into_boxed_slice();
    let stack_top = (stack.as_mut_ptr() as u64 + AP_STACK_SIZE as u64) & !0xF;
    AP_STACKS.lock().push(stack);

    unsafe {
        write_param(&tramp_stack, stack_top);
        let flag_phys = &raw const tramp_flag as u64;
        (phys_to_virt(flag_phys) as *mut u8).write_volatile(0);
    }

    AP_READY.store(false, Ordering::SeqCst);

    unsafe {
        crate::apic::send_init(lapic_id);
        crate::apic::busy_wait_micros(10_000); // 10 ms after INIT

        crate::apic::send_startup(lapic_id, TRAMPOLINE_VECTOR);
        crate::apic::busy_wait_micros(200);

        if !AP_READY.load(Ordering::SeqCst) {
            crate::apic::send_startup(lapic_id, TRAMPOLINE_VECTOR);
        }
    }

    let mut waited = 0;
    while !AP_READY.load(Ordering::SeqCst) && waited < 100 {
        crate::apic::busy_wait_micros(1000);
        waited += 1;
    }

    if AP_READY.load(Ordering::SeqCst) {
        crate::serial_println!("[smp] LAPIC {} en ligne", lapic_id);
    } else {
        crate::serial_println!("[smp] LAPIC {} n'a pas repondu - abandonne", lapic_id);
    }
}

// ============================================================
//  Entry point of the secondary cores
// ============================================================

/// First Rust code run by a secondary core.
///
/// Called from the trampoline, in long mode, with a clean stack and the
/// page hierarchy of the boot core.
extern "C" fn ap_entry() -> ! {
    // The PAT is a per-processor register, and the page tables are shared:
    // the boot core has already laid down write-combining pages there for
    // the framebuffer (7.1.2). A core whose PAT is not programmed would
    // read those same pages as write-back, and two cache types for one
    // physical page is a case the architecture declares undefined. So it
    // is the very first thing done here, before the least discretionary
    // memory access.
    unsafe { crate::memory::init_pat() };

    let cpu_id = NEXT_CPU_ID.fetch_add(1, Ordering::SeqCst);
    // First of all: the boot core has already switched `current_cpu_id`
    // to TSC_AUX, and this core's still reads 0 -- core 0's index.
    // SAFETY: `cpu_id` was just handed to this core.
    unsafe { publish_cpu_id(cpu_id) };

    // The LAPIC is enabled before anything else: it is what gives the
    // hardware identity of the core, hence the key of the mapping table.
    let _ = unsafe { crate::apic::init() };
    let lapic_id = crate::apic::id() as u8;
    LAPIC_TO_CPU[lapic_id as usize].store(cpu_id as u8, Ordering::SeqCst);

    // EFER is a per-processor register: the NXE bit must be armed here
    // too, otherwise this core would treat as reserved every table entry
    // carrying bit 63 set by the loader -- an immediate page fault on the
    // first user data page.
    unsafe { crate::memory::enable_nx() };

    // CR0 and CR4 are per processor as well: without SSE armed here, the
    // first switch_context of this core would fault on FXRSTOR.
    if !unsafe { crate::fpu::enable() } {
        panic!("coeur secondaire sans FXSR/SSE/SSE2");
    }
    unsafe { crate::memory::enable_write_protect() };

    // GDT and TSS private to this core.
    unsafe { crate::gdt::init_cpu(cpu_id) };

    // The IDT is shared: it is no longer modified after init, and pointing
    // the same handlers from several cores is safe. Only the IDTR
    // register belongs to each core.
    crate::idt::load();

    // Kernel stack for the ring 3 -> ring 0 transitions on this core.
    let kernel_stack = allocate_kernel_stack();
    unsafe {
        crate::gdt::set_kernel_stack(cpu_id, kernel_stack);
        crate::syscall::set_kernel_stack(cpu_id, kernel_stack);
        // EFER.SCE, STAR, LSTAR and KERNEL_GS_BASE are per-core MSRs.
        crate::syscall::init(cpu_id);
    }

    {
        let mut cpus = CPUS.lock();
        if cpu_id < MAX_CPUS {
            cpus[cpu_id] = CpuInfo {
                cpu_id,
                lapic_id,
                online: true,
                kernel_stack,
                ticks: 0,
                reschedule_ipis: 0,
            };
            LAPIC_OF[cpu_id].store(lapic_id as u16, Ordering::Release);
        }
    }

    ONLINE_COUNT.fetch_add(1, Ordering::SeqCst);
    AP_READY.store(true, Ordering::SeqCst);

    crate::serial_println!(
        "[smp] coeur {} demarre (LAPIC {}, pile noyau 0x{:x})",
        cpu_id,
        lapic_id,
        kernel_stack
    );

    // Waiting for the boot core to create the tasks.
    while !SCHEDULER_READY.load(Ordering::SeqCst) {
        core::hint::spin_loop();
    }

    // Local timer armed just before entering scheduling, so that no tick
    // arrives before a task is elected.
    unsafe { crate::apic::init_timer_ap(crate::TIMER_HZ) };
    unsafe { crate::idt::enable_interrupts() };

    crate::serial_println!("[smp] coeur {} entre dans l'ordonnancement", cpu_id);

    unsafe { crate::scheduler::start_ap(cpu_id) }
}

pub fn dump_state() {
    let (shootdowns, late) = crate::memory::tlb_stats();
    if shootdowns > 0 {
        crate::serial_println!(
            "[smp] {} invalidation(s) de TLB diffusee(s) - {} coeur(s) en retard",
            shootdowns,
            late
        );
    }

    // A copy: printing with `CPUS` held would hold up every core that
    // reads it meanwhile.
    let cpus = *CPUS.lock();
    crate::serial_println!(
        "[smp] {} coeurs en ligne",
        ONLINE_COUNT.load(Ordering::Relaxed)
    );
    crate::serial_println!("[smp]  CPU  LAPIC  ROLE  TICS      IPI RESCHED");

    for cpu in cpus.iter() {
        if !cpu.online {
            continue;
        }
        crate::serial_println!(
            "[smp]  {:3}  {:5}  {:4}  {:8}  {:12}",
            cpu.cpu_id,
            cpu.lapic_id,
            if cpu.cpu_id == 0 { "BSP" } else { "AP" },
            CPU_TICKS[cpu.cpu_id].load(Ordering::Relaxed),
            CPU_IPIS[cpu.cpu_id].load(Ordering::Relaxed)
        );
    }
}

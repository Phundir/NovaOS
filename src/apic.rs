//! Local APIC: interrupt controller built into the CPU.
//!
//! Each core owns its LAPIC, with its registers, its timer and its
//! interrupt queue. The functions of this module therefore always act on
//! the LAPIC of the core running them -- the MMIO base is the same
//! virtual address everywhere, but the hardware behind it is distinct.

use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::memory::phys_to_virt;

const IA32_APIC_BASE_MSR: u32 = 0x1B;
const APIC_BASE_ENABLE: u64 = 1 << 11;
/// x2APIC mode: the registers become MSRs `0x800 + offset / 16`.
const APIC_BASE_X2APIC: u64 = 1 << 10;
/// The 64-bit command register of x2APIC (destination in the high half).
const MSR_X2APIC_ICR: u32 = 0x830;

// Register offsets, in bytes from the base.
const REG_ID: u32 = 0x020;
const REG_VERSION: u32 = 0x030;
const REG_TPR: u32 = 0x080;
const REG_EOI: u32 = 0x0B0;
const REG_SPURIOUS: u32 = 0x0F0;
const REG_ERROR_STATUS: u32 = 0x280;
const REG_ICR_LOW: u32 = 0x300;
const REG_ICR_HIGH: u32 = 0x310;
const REG_LVT_TIMER: u32 = 0x320;
const REG_LVT_LINT0: u32 = 0x350;
const REG_LVT_LINT1: u32 = 0x360;
const REG_LVT_ERROR: u32 = 0x370;
const REG_TIMER_INIT_COUNT: u32 = 0x380;
const REG_TIMER_CURR_COUNT: u32 = 0x390;
const REG_TIMER_DIVIDE: u32 = 0x3E0;

/// Bit 16 of the LVT registers: masks the interrupt.
const LVT_MASKED: u32 = 1 << 16;
/// Bit 17 of the LVT timer: periodic mode.
const LVT_TIMER_PERIODIC: u32 = 1 << 17;

// --- Assigned vectors ---
pub const VECTOR_TIMER: u8 = 32;
pub const VECTOR_KEYBOARD: u8 = 33;
/// Rescheduling IPI: lets one core force another to reconsider its
/// election, typically after an IPC wake-up.
pub const VECTOR_RESCHEDULE: u8 = 240;
/// TLB shootdown IPI: tells the other cores that a translation has been
/// taken down and that whatever they hold for it is stale.
///
/// A separate vector from the rescheduling one, although both end in a
/// short handler: one of them must never lead to an election. A core that
/// rescheduled on receiving a shootdown would switch task in the middle
/// of somebody else's page-table edit.
pub const VECTOR_TLB: u8 = 241;
pub const VECTOR_ERROR: u8 = 254;
pub const VECTOR_SPURIOUS: u8 = 255;

static LAPIC_BASE: AtomicU64 = AtomicU64::new(0);

/// Are the LAPICs driven as x2APIC (MSRs) rather than through MMIO?
///
/// ## Why x2APIC when the processor has it
///
/// Under a hypervisor every access to the xAPIC page is an exit and an
/// **emulated instruction** -- the hypervisor fetches and decodes the
/// `mov` that touched the page. An IPI cost three of them (high command
/// word, low command word, delivery status read back) and the receiving
/// core a fourth for its EOI: some ten microseconds per cross-core
/// wake-up, paid at every hand-over between two threads of a browser
/// (tools/ports/tests/sysbench.c: a pipe round trip between two threads
/// took 24 us under Nova, 3 on the host). In x2APIC mode an IPI is one
/// `wrmsr` of the 64-bit command register, which KVM handles on its fast
/// exit path, and the delivery status no longer exists. Chosen once, by
/// the boot core, from CPUID; the machines without it keep the MMIO path.
static X2APIC: AtomicBool = AtomicBool::new(false);

/// Does the processor offer x2APIC? CPUID.1:ECX bit 21.
fn x2apic_supported() -> bool {
    core::arch::x86_64::__cpuid(1).ecx & (1 << 21) != 0
}

pub fn is_x2apic() -> bool {
    X2APIC.load(Ordering::Relaxed)
}

/// Reads an MSR.
unsafe fn read_msr(msr: u32) -> u64 {
    let (high, low): (u32, u32);
    unsafe {
        asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high,
             options(nomem, nostack, preserves_flags));
    }
    ((high as u64) << 32) | (low as u64)
}

/// Writes an MSR.
unsafe fn write_msr(msr: u32, value: u64) {
    unsafe {
        asm!("wrmsr", in("ecx") msr, in("eax") value as u32, in("edx") (value >> 32) as u32,
             options(nostack, preserves_flags));
    }
}

fn base() -> u64 {
    LAPIC_BASE.load(Ordering::Relaxed)
}

/// Is the MMIO window installed? Any read before init would dereference
/// a null address.
pub fn is_initialized() -> bool {
    base() != 0
}

fn read(reg: u32) -> u32 {
    if is_x2apic() {
        // SAFETY: in x2APIC mode every register this driver names has
        // its MSR at 0x800 + offset / 16.
        return unsafe { read_msr(0x800 + (reg >> 4)) } as u32;
    }
    unsafe { ((base() + reg as u64) as *const u32).read_volatile() }
}

fn write(reg: u32, value: u32) {
    if is_x2apic() {
        // SAFETY: as in `read`; the command register, the one register
        // whose MSR differs in width, goes through `send_ipi` instead.
        unsafe { write_msr(0x800 + (reg >> 4), value as u64) };
        return;
    }
    unsafe { ((base() + reg as u64) as *mut u32).write_volatile(value) }
}

/// Does the CPU expose an APIC? Bit 9 of EDX for CPUID.1.
pub fn is_supported() -> bool {
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
    edx & (1 << 9) != 0
}

/// Enables and configures the Local APIC of the current core.
///
/// Called once per core: each LAPIC has its own spurious register, its
/// own LVTs and its own TPR to initialise.
///
/// # Safety
/// The PIC must have been masked beforehand on the boot core.
pub unsafe fn init() -> Result<(), &'static str> {
    if !is_supported() {
        return Err("APIC absent du CPU");
    }

    unsafe {
        let msr = read_msr(IA32_APIC_BASE_MSR);
        let phys = msr & 0xF_FFFF_F000;
        write_msr(IA32_APIC_BASE_MSR, msr | APIC_BASE_ENABLE);

        // MMIO must never be cached. No effect if already done.
        crate::memory::set_mmio_uncached(phys);

        // x2APIC: decided by the first core to get here (the boot core),
        // then applied by every core to its own LAPIC -- the mode is per
        // LAPIC, and one core in each mode would read its registers
        // through the wrong door. The switch goes from xAPIC enabled to
        // x2APIC enabled, the one transition the architecture allows.
        if LAPIC_BASE.load(Ordering::Relaxed) == 0 && x2apic_supported() {
            X2APIC.store(true, Ordering::SeqCst);
        }
        if is_x2apic() {
            write_msr(IA32_APIC_BASE_MSR, msr | APIC_BASE_ENABLE | APIC_BASE_X2APIC);
        }

        LAPIC_BASE.store(phys_to_virt(phys), Ordering::Relaxed);
    }

    // Task Priority Register at 0: no interrupt is filtered.
    write(REG_TPR, 0);

    // Spurious register: bit 8 enables reception, bits 0-7 carry the
    // vector raised on a spurious interrupt.
    write(REG_SPURIOUS, 0x100 | VECTOR_SPURIOUS as u32);

    // LINT0/LINT1 masked: under APIC, those lines inherited from the PIC
    // are no longer used. The NMI will come through the I/O APIC.
    write(REG_LVT_LINT0, LVT_MASKED);
    write(REG_LVT_LINT1, LVT_MASKED);

    write(REG_LVT_ERROR, VECTOR_ERROR as u32);

    Ok(())
}

/// Acknowledges the current interrupt. **Mandatory** at the end of every
/// IRQ handler: without it, the LAPIC considers the interrupt still in
/// service and delivers no other of equal or lower priority.
#[inline]
pub fn end_of_interrupt() {
    write(REG_EOI, 0);
}

/// LAPIC identifier of the current core -- serves as hardware identity.
pub fn id() -> u32 {
    if !is_initialized() {
        return 0;
    }
    // The x2APIC identity is the whole 32-bit register; the xAPIC one is
    // its top byte.
    if is_x2apic() { read(REG_ID) } else { read(REG_ID) >> 24 }
}

pub fn version() -> u32 {
    read(REG_VERSION) & 0xFF
}

pub fn error_status() -> u32 {
    write(REG_ERROR_STATUS, 0);
    read(REG_ERROR_STATUS)
}

// ============================================================
//  Timer
// ============================================================

/// APIC bus frequency, measured once only by the boot core. It is common
/// to every core: the APs reuse it instead of fighting over the PIT.
static TIMER_FREQUENCY: AtomicU64 = AtomicU64::new(0);
/// Reload value of the periodic timer, remembered so that the timer can
/// be restored after a busy wait.
static PERIODIC_COUNT: AtomicU32 = AtomicU32::new(0);

/// Calibrates the APIC timer against the PIT, whose frequency is fixed
/// and known: 1 193 182 Hz.
///
/// # Safety
/// Interrupts masked, LAPIC initialised. The PIT being unique on the
/// machine, this function must never run on two cores at the same time
/// -- hence its use being reserved to the boot core.
unsafe fn calibrate() -> u64 {
    use crate::port::{inb, outb};

    const PIT_CHANNEL2: u16 = 0x42;
    const PIT_COMMAND: u16 = 0x43;
    const PIT_GATE: u16 = 0x61;
    const PIT_TICKS: u16 = 11932; // 10 ms

    unsafe {
        write(REG_TIMER_DIVIDE, 0b0011);

        let gate = inb(PIT_GATE);
        outb(PIT_GATE, (gate & 0xFD) | 0x01);
        outb(PIT_COMMAND, 0b1011_0010);
        outb(PIT_CHANNEL2, (PIT_TICKS & 0xFF) as u8);
        inb(0x60);
        outb(PIT_CHANNEL2, (PIT_TICKS >> 8) as u8);

        let gate = inb(PIT_GATE);
        outb(PIT_GATE, gate & 0xFE);
        outb(PIT_GATE, gate | 0x01);

        write(REG_TIMER_INIT_COUNT, u32::MAX);

        while inb(PIT_GATE) & 0x20 == 0 {
            core::hint::spin_loop();
        }

        let elapsed = u32::MAX - read(REG_TIMER_CURR_COUNT);
        write(REG_TIMER_INIT_COUNT, 0);

        (elapsed as u64) * 100
    }
}

/// Configures the timer of the boot core: calibration then arming.
///
/// # Safety
/// LAPIC initialised, IDT in place with a handler on VECTOR_TIMER.
pub unsafe fn init_timer(hz: u32) -> u64 {
    let frequency = unsafe { calibrate() };
    TIMER_FREQUENCY.store(frequency, Ordering::Relaxed);
    arm_timer(hz);
    frequency
}

/// Configures the timer of a secondary core, reusing the frequency
/// measured by the boot core.
///
/// # Safety
/// `init_timer` must have been called beforehand on the BSP.
pub unsafe fn init_timer_ap(hz: u32) {
    arm_timer(hz);
}

/// Arms the local timer in periodic mode.
fn arm_timer(hz: u32) {
    let frequency = TIMER_FREQUENCY.load(Ordering::Relaxed);
    let count = (frequency / hz.max(1) as u64).max(1) as u32;

    PERIODIC_COUNT.store(count, Ordering::Relaxed);

    write(REG_TIMER_DIVIDE, 0b0011);
    write(REG_LVT_TIMER, VECTOR_TIMER as u32 | LVT_TIMER_PERIODIC);
    write(REG_TIMER_INIT_COUNT, count);
}

pub fn timer_frequency() -> u64 {
    TIMER_FREQUENCY.load(Ordering::Relaxed)
}

/// Calibrated busy wait, required between the steps of the INIT-SIPI-SIPI
/// sequence.
///
/// The timer counter being unique per core, the wait temporarily
/// commandeers the periodic timer: its state is therefore saved and
/// restored on the way out, failing which preemption would stop dead.
pub fn busy_wait_micros(micros: u64) {
    let frequency = timer_frequency();
    if frequency == 0 {
        for _ in 0..(micros * 1000) {
            core::hint::spin_loop();
        }
        return;
    }

    let saved_lvt = read(REG_LVT_TIMER);
    // The count of *this* core's timer, read back from the register:
    // core 0 runs at `FINE_HZ` and the others at `TIMER_HZ`, so a single
    // remembered value would restore one of them at the wrong rate.
    let saved_count = read(REG_TIMER_INIT_COUNT);

    write(REG_LVT_TIMER, LVT_MASKED);

    let target = (frequency * micros) / 1_000_000;
    write(REG_TIMER_INIT_COUNT, u32::MAX);

    loop {
        let elapsed = (u32::MAX - read(REG_TIMER_CURR_COUNT)) as u64;
        if elapsed >= target {
            break;
        }
        core::hint::spin_loop();
    }

    write(REG_LVT_TIMER, saved_lvt);
    write(REG_TIMER_INIT_COUNT, saved_count);
}

// ============================================================
//  Inter-processor interrupts
// ============================================================

/// Delivery modes, bits 8-10 of the command register.
const IPI_FIXED: u32 = 0b000 << 8;
const IPI_INIT: u32 = 0b101 << 8;
const IPI_STARTUP: u32 = 0b110 << 8;
/// Bit 14: level high, demanded for INIT assert.
const IPI_LEVEL_ASSERT: u32 = 1 << 14;
/// Bit 12: armed by the LAPIC as long as the IPI is not delivered.
const IPI_DELIVERY_PENDING: u32 = 1 << 12;

fn wait_for_delivery() {
    // x2APIC has no delivery status: a `wrmsr` of the command register
    // returns once the IPI is sent.
    if is_x2apic() {
        return;
    }
    while read(REG_ICR_LOW) & IPI_DELIVERY_PENDING != 0 {
        core::hint::spin_loop();
    }
}

/// Raises a raw IPI towards a core named by its LAPIC identifier.
///
/// The write of the high register must precede that of the low one: it is
/// the latter that triggers the send. In x2APIC mode both halves are one
/// 64-bit register, the destination in the high 32 bits.
///
/// # Safety
/// A malformed IPI can reset a core or make it diverge.
unsafe fn send_ipi(lapic_id: u8, command: u32) {
    if is_x2apic() {
        unsafe { write_msr(MSR_X2APIC_ICR, ((lapic_id as u64) << 32) | command as u64) };
        return;
    }
    write(REG_ICR_HIGH, (lapic_id as u32) << 24);
    write(REG_ICR_LOW, command);
    wait_for_delivery();
}

/// Sends an INIT IPI: puts the target core into a reset state, ready to
/// receive a SIPI.
///
/// # Safety
/// Must aim only at a core not yet started.
pub unsafe fn send_init(lapic_id: u8) {
    unsafe { send_ipi(lapic_id, IPI_INIT | IPI_LEVEL_ASSERT) };
}

/// Sends a STARTUP IPI: the target core starts in real mode at address
/// `vector << 12`.
///
/// # Safety
/// The trampoline must be in place at the matching address.
pub unsafe fn send_startup(lapic_id: u8, vector: u8) {
    unsafe { send_ipi(lapic_id, IPI_STARTUP | vector as u32) };
}

/// Interrupts a core to make it reconsider its election.
///
/// Used when a task becomes ready on a core other than the one waking it:
/// without this IPI, the target core could stay in `hlt` until its next
/// tick, that is 10 ms of pointless latency.
///
/// # Safety
/// The VECTOR_RESCHEDULE vector must have a handler in the IDT.
pub unsafe fn send_reschedule(lapic_id: u8) {
    unsafe { send_ipi(lapic_id, IPI_FIXED | IPI_LEVEL_ASSERT | VECTOR_RESCHEDULE as u32) };
}

/// Sends the TLB shootdown to one core (`memory::shootdown_space_and_wait`).
///
/// # Safety
/// As `broadcast_tlb_shootdown`.
pub unsafe fn send_tlb_shootdown(lapic_id: u8) {
    unsafe { send_ipi(lapic_id, IPI_FIXED | IPI_LEVEL_ASSERT | VECTOR_TLB as u32) };
}

/// Destination shorthand 0b11: every core **except** the sender.
///
/// The shorthand rather than a loop over the LAPIC identifiers: the
/// LAPIC delivers to all of them from one write, and a loop would have to
/// wait for each delivery in turn -- which on a machine of eight cores is
/// eight round trips for something that must be quick.
const IPI_ALL_BUT_SELF: u32 = 0b11 << 18;

/// Broadcasts the TLB shootdown to every other core.
///
/// # Safety
/// The VECTOR_TLB vector must have a handler in the IDT, and it must do
/// nothing but flush and acknowledge -- see `idt::tlb_shootdown_ipi`.
pub unsafe fn broadcast_tlb_shootdown() {
    // No `unsafe` block: `read` and `write` carry their own, and the
    // safety condition of this function bears on the handler installed at
    // the far end, not on the write itself.
    let command = IPI_FIXED | IPI_LEVEL_ASSERT | IPI_ALL_BUT_SELF | VECTOR_TLB as u32;
    if is_x2apic() {
        // SAFETY: the shorthand needs no destination; see the function.
        unsafe { write_msr(MSR_X2APIC_ICR, command as u64) };
        return;
    }
    wait_for_delivery();
    write(REG_ICR_LOW, command);
    wait_for_delivery();
}

pub fn dump_state() {
    crate::serial_println!(
        "[apic] LAPIC id {} version 0x{:02x} - {}",
        id(),
        version(),
        if is_x2apic() { alloc::string::String::from("mode x2APIC (registres en MSR)") } else { alloc::format!("mode xAPIC, base virtuelle 0x{:016x}", base()) }
    );
    crate::serial_println!(
        "[apic] frequence du bus : {} Hz ({} MHz) - rechargement {} tics",
        timer_frequency(),
        timer_frequency() / 1_000_000,
        PERIODIC_COUNT.load(Ordering::Relaxed)
    );
    crate::serial_println!("[apic] registre d'erreur : 0x{:08x}", error_status());
}

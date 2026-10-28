//! Interrupt Descriptor Table and CPU exception handlers.
//!
//! The first 32 vectors are reserved by Intel for the exceptions. Beyond
//! them come the hardware interrupts routed by the I/O APIC, then the
//! local vectors of the LAPIC.
//!
//! The IDT is unique and shared by every core: it is no longer modified
//! after `init`, and pointing the same handlers from several processors
//! is safe. Only the IDTR register belongs to each core, hence the `load`
//! function called by the secondary cores.

use core::arch::asm;
use core::mem::size_of;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::gdt::{self, DescriptorTablePointer};
use crate::vga::Color;

// ============================================================
//  IDT entry
// ============================================================

#[derive(Clone, Copy)]
#[repr(C)]
pub struct Entry {
    offset_low: u16,
    selector: u16,
    /// bits 0-2: IST index - bits 8-11: type - bits 13-14: DPL - bit 15: present
    options: u16,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

impl Entry {
    const fn missing() -> Self {
        Entry {
            offset_low: 0,
            selector: 0,
            options: 0x0E00, // interrupt gate, not present
            offset_mid: 0,
            offset_high: 0,
            reserved: 0,
        }
    }

    /// Takes a pointer rather than an integer: converting a function item
    /// straight to u64 trips the `function_casts_as_integer` lint.
    fn set_handler(&mut self, handler: *const ()) {
        let addr = handler as u64;
        self.offset_low = addr as u16;
        self.offset_mid = (addr >> 16) as u16;
        self.offset_high = (addr >> 32) as u32;
        self.selector = gdt::KERNEL_CODE_SELECTOR;
        // Interrupt gate: IF is cleared automatically on entry.
        self.options = 0x8E00;
    }

    /// A gate ring 3 may raise itself with `int n` -- `int3` for the
    /// debugger's breakpoints. Any other gate answers `int n` from ring 3
    /// with #GP, which is what the others want.
    fn set_user_callable(&mut self) {
        self.options |= 3 << 13;
    }

    /// The IST index is stored one-based; 0 means "no dedicated stack".
    fn set_stack_index(&mut self, index: u16) {
        self.options = (self.options & !0b111) | (index + 1);
    }
}

static mut IDT: [Entry; 256] = [Entry::missing(); 256];

// ============================================================
//  Frame pushed by the CPU
// ============================================================

/// Context pushed automatically by the processor before calling the
/// handler. Tells where and in which state the fault happened.
#[derive(Debug)]
#[repr(C)]
pub struct InterruptStackFrame {
    pub instruction_pointer: u64,
    pub code_segment: u64,
    pub cpu_flags: u64,
    pub stack_pointer: u64,
    pub stack_segment: u64,
}

unsafe extern "C" {
    /// boot/fault.asm
    fn page_fault_entry();
    /// boot/fault.asm, 16.4: #DB and #BP with every register saved.
    fn debug_trap_entry();
    fn breakpoint_entry();
}

// ============================================================
//  Generic report
// ============================================================

fn report(vector: u8, name: &str, frame: &InterruptStackFrame, error_code: Option<u64>) {
    crate::vga::set_color(Color::White, Color::Red);
    crate::kprintln!();
    crate::kprintln!(
        " EXCEPTION CPU #{} - {} (coeur {}) ",
        vector,
        name,
        crate::smp::current_cpu_id()
    );

    crate::vga::set_color(Color::LightRed, Color::Black);
    crate::kprintln!("  RIP    : 0x{:016x}", frame.instruction_pointer);
    crate::kprintln!("  CS     : 0x{:04x}", frame.code_segment);
    crate::kprintln!("  RFLAGS : 0x{:016x}", frame.cpu_flags);
    crate::kprintln!("  RSP    : 0x{:016x}", frame.stack_pointer);
    crate::kprintln!("  SS     : 0x{:04x}", frame.stack_segment);

    if let Some(code) = error_code {
        crate::kprintln!("  Erreur : 0x{:x}", code);
    }

    // A #GP raised by `iretq` in ring 0 leaves the frame it refused at
    // RSP: its CS/SS pair is the diagnosis.
    if vector == 13 && frame.code_segment & 3 == 0 && frame.stack_pointer >= 0xFFFF_8000_0000_0000 {
        // SAFETY: a kernel stack pointer of the faulting code, read only.
        let words = unsafe { core::slice::from_raw_parts(frame.stack_pointer as *const u64, 5) };
        crate::kprintln!("  Pile   : {:x} {:x} {:x} {:x} {:x}", words[0], words[1], words[2], words[3], words[4]);
    }

    // The page fault files the faulting address in CR2.
    if vector == 14 {
        let cr2: u64;
        unsafe { asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack)) };
        crate::kprintln!("  CR2    : 0x{:016x}", cr2);

        if let Some(code) = error_code {
            crate::kprintln!(
                "  Cause  : {} - {} - {}",
                if code & 1 == 0 { "page absente" } else { "violation de protection" },
                if code & 2 == 0 { "lecture" } else { "ecriture" },
                if code & 4 == 0 { "ring 0" } else { "ring 3" }
            );
        }
    }
}

/// Generates a handler for an exception with no error code.
///
/// Raised in ring 3, an exception ends the task that raised it (11.3):
/// the core carries on with the others. Raised in ring 0, it is a kernel
/// bug, and the core stops where it is so the report stays on screen.
macro_rules! handler {
    ($name:ident, $vector:expr, $desc:expr) => {
        extern "x86-interrupt" fn $name(frame: InterruptStackFrame) -> () {
            if frame.code_segment & 3 == 3 {
                user_exception($vector, $desc, frame.instruction_pointer, None);
            }
            report($vector, $desc, &frame, None);
            crate::halt_loop();
        }
    };
}

/// Generates a handler for an exception that pushes an error code.
macro_rules! handler_with_code {
    ($name:ident, $vector:expr, $desc:expr) => {
        extern "x86-interrupt" fn $name(frame: InterruptStackFrame, error_code: u64) -> () {
            if frame.code_segment & 3 == 3 {
                user_exception($vector, $desc, frame.instruction_pointer, Some(error_code));
            }
            report($vector, $desc, &frame, Some(error_code));
            crate::halt_loop();
        }
    };
}

/// Ring 3 exceptions that ended their task, since boot.
static USER_KILLS: AtomicU64 = AtomicU64::new(0);

pub fn user_kills() -> u64 {
    USER_KILLS.load(Ordering::Relaxed)
}

/// Ends the current task after an exception it raised in ring 3.
///
/// Before phase 11.3 every exception halted the core, whichever ring it
/// came from: one faulty program took a processor with it. The task is
/// now ended with code `-(128 + vector)` -- the shell convention for a
/// death by signal, with the vector standing in for the signal -- and
/// whatever owns it (the Linux subsystem for a thread group) is told.
pub fn user_exception(vector: u8, name: &str, rip: u64, detail: Option<u64>) -> ! {
    // Entered from ring 3 through an `x86-interrupt` handler, which does
    // not swap GS: the kernel base goes back in before this task switches
    // away for good. Every task switch assumes it (see `preempt_user`).
    unsafe { asm!("swapgs", options(nomem, nostack)) };
    // The stack may be 8 bytes off here. For a vector that pushes an
    // error code the processor leaves RSP 16-aligned on the handler's
    // entry (48 bytes pushed), and LLVM's `x86-interrupt` prologue
    // counts as if it were 8 mod 16, like after a `call`: everything
    // called from the handler runs misaligned. Nothing noticed until the
    // `fxsave` of `switch_context`, which demands 16 -- a Linux program's
    // #GP ended its task, the switch away raised #GP in ring 0 and the
    // core halted (first seen under KVM). The task never comes back
    // here, so the rest runs on a stack realigned by hand.
    let fault = UserFault { vector, name_ptr: name.as_ptr(), name_len: name.len(), rip, detail };
    // SAFETY: `fault` stays on the stack above the new RSP (the stack
    // only grows down from here) and the callee never returns.
    unsafe {
        asm!(
            "and rsp, -16",
            "call {f}",
            f = sym user_exception_aligned,
            in("rdi") &raw const fault,
            options(noreturn)
        )
    }
}

/// What `user_exception` hands over its stack realignment.
#[repr(C)]
struct UserFault {
    vector: u8,
    name_ptr: *const u8,
    name_len: usize,
    rip: u64,
    detail: Option<u64>,
}

/// # Safety
/// `fault` points at a live `UserFault` whose name is valid UTF-8.
unsafe extern "C" fn user_exception_aligned(fault: *const UserFault) -> ! {
    // SAFETY: guaranteed by the caller, `user_exception`.
    let f = unsafe { &*fault };
    let name = unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(f.name_ptr, f.name_len)) };
    user_exception_kernel_gs(f.vector, name, f.rip, f.detail)
}

/// Same, for a caller that already runs with the kernel GS base (the
/// page fault entry swaps it itself).
pub fn user_exception_kernel_gs(vector: u8, name: &str, rip: u64, detail: Option<u64>) -> ! {
    USER_KILLS.fetch_add(1, Ordering::Relaxed);
    let pid = crate::scheduler::current_pid();
    match detail {
        Some(value) => crate::serial_println!(
            "[idt] tache {} : exception #{} ({}) en ring 3 a RIP 0x{:x} - 0x{:x} - tache terminee",
            pid, vector, name, rip, value
        ),
        None => crate::serial_println!(
            "[idt] tache {} : exception #{} ({}) en ring 3 a RIP 0x{:x} - tache terminee",
            pid, vector, name, rip
        ),
    }
    // Interrupts back on: ending a task switches away, and the handler
    // was entered through an interrupt gate.
    unsafe { asm!("sti", options(nomem, nostack)) };
    // A Linux thread takes its whole process down, as a fatal signal does.
    if crate::posix::current_thread().is_some() {
        crate::posix::signal::fatal_exception(vector);
    }
    crate::scheduler::exit_current_with(-(128 + vector as i64))
}

/// A page fault the kernel caused and cannot recover from.
pub fn kernel_page_fault(rip: u64, rsp: u64, error: u64, cr2: u64) -> ! {
    let frame = InterruptStackFrame {
        instruction_pointer: rip,
        code_segment: 0x08,
        cpu_flags: 0,
        stack_pointer: rsp,
        stack_segment: 0x10,
    };
    report(14, "Faute de page", &frame, Some(error));
    crate::kprintln!("  CR2    : 0x{:016x}", cr2);
    crate::halt_loop()
}

handler!(divide_error, 0, "Division par zero");
handler!(non_maskable_interrupt, 2, "Interruption non masquable");
handler!(overflow, 4, "Depassement de capacite");
handler!(bound_range_exceeded, 5, "Bornes depassees");
handler!(invalid_opcode, 6, "Instruction invalide");
handler!(device_not_available, 7, "Peripherique indisponible");
handler!(x87_floating_point, 16, "Exception x87");
handler!(simd_floating_point, 19, "Exception SIMD");
handler!(virtualization, 20, "Virtualisation");
handler!(hypervisor_injection, 28, "Injection hyperviseur");
handler!(reserved_exception, 15, "Vecteur reserve");

handler_with_code!(invalid_tss, 10, "TSS invalide");
handler_with_code!(segment_not_present, 11, "Segment absent");
handler_with_code!(stack_segment_fault, 12, "Faute de segment de pile");
handler_with_code!(general_protection_fault, 13, "Faute de protection generale");
handler_with_code!(alignment_check, 17, "Verification d'alignement");
handler_with_code!(control_protection, 21, "Protection de flot de controle");
handler_with_code!(vmm_communication, 29, "Communication VMM");
handler_with_code!(security_exception, 30, "Exception de securite");

/// Breakpoint in ring 0: the only non-fatal exception. `int3` resumes
/// execution normally -- that is what serves as our IDT test. Reached
/// through `breakpoint_entry` and `debug::debug_trap_handler`, which
/// keep ring 3 traps for the debugger (16.4).
pub fn kernel_breakpoint(rip: u64) {
    crate::vga::set_color(Color::LightCyan, Color::Black);
    crate::kprintln!("[idt] breakpoint atteint - RIP 0x{:016x}", rip);
    crate::vga::set_color(Color::LightGray, Color::Black);
}

/// A debug exception in ring 0: nothing in the kernel sets TF or a debug
/// register, so this is a bug -- reported, and the core stops.
pub fn kernel_debug_exception(rip: u64, rsp: u64, rflags: u64) -> ! {
    let frame = InterruptStackFrame {
        instruction_pointer: rip,
        code_segment: 0x08,
        cpu_flags: rflags,
        stack_pointer: rsp,
        stack_segment: 0x10,
    };
    report(1, "Debug", &frame, None);
    crate::halt_loop()
}

/// Raised when the CPU cannot invoke a handler. Runs on the IST stack
/// dedicated to the core: it is the only way to survive a stack overflow.
/// Diverging, since no return is possible.
extern "x86-interrupt" fn double_fault(frame: InterruptStackFrame, error_code: u64) -> ! {
    report(8, "DOUBLE FAUTE", &frame, Some(error_code));
    crate::vga::set_color(Color::Yellow, Color::Black);
    crate::kprintln!("  (handler execute sur la pile IST dediee a ce coeur)");
    crate::halt_loop();
}

/// Unrecoverable hardware error reported by the CPU.
extern "x86-interrupt" fn machine_check(frame: InterruptStackFrame) -> ! {
    report(18, "MACHINE CHECK", &frame, None);
    crate::halt_loop();
}

// ============================================================
//  Hardware interrupts
// ============================================================

/// Global tick counter, incremented by the boot core alone.
///
/// Every core receives timer interrupts, but only one wall clock makes
/// sense: the other cores feed their individual counters, readable
/// through `smp::cpu_ticks`.
static TICKS: AtomicU64 = AtomicU64::new(0);

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Core 0's timer interrupts: one per millisecond (`FINE_HZ`).
static FINE_TICKS: AtomicU64 = AtomicU64::new(0);

/// Milliseconds counted by core 0's timer: the clock of the Linux
/// programs' deadlines (`linux::time::deadline_after_ns`).
pub fn fine_ticks() -> u64 {
    FINE_TICKS.load(Ordering::Relaxed)
}

/// APIC timer. Each core owns its own: this handler therefore runs in
/// parallel on every processor, and must stay strictly local.
extern "x86-interrupt" fn timer_interrupt(frame: InterruptStackFrame) {
    let cpu = crate::smp::current_cpu_id();

    if cpu == 0 {
        // Every millisecond: the fine deadlines of the Linux waits.
        let fine = FINE_TICKS.fetch_add(1, Ordering::Relaxed) + 1;
        crate::posix::wait::tick(fine);
        if fine % (crate::FINE_HZ / crate::TIMER_HZ) as u64 != 0 {
            // Not a system tick: no slice consumed, no Nova sleeper due.
            // A thread just woken on this core may still take it at once.
            crate::apic::end_of_interrupt();
            if frame.code_segment & 3 == 3 {
                preempt_user();
            }
            return;
        }
        let now = TICKS.fetch_add(1, Ordering::Relaxed) + 1;
        // 5.6.3: the deadlines of `SYS_TIME_SLEEP_MS`. On core 0 alone,
        // because it is the one holding the global clock: the other cores
        // have their own counters, which do not measure the same thing.
        crate::syscall::domain_time::tick(now);
        crate::posix::audio::tick();
    }
    crate::smp::record_tick(cpu);

    // Safety net of the shootdown: a core that was masking interrupts
    // when the IPI went out catches up here, at worst one tick late.
    crate::memory::sync_tlb();

    crate::apic::end_of_interrupt();
    crate::scheduler::on_timer_tick(cpu);

    // 11.3: true preemption, for ring 3 only. A program computing without
    // system calls used to keep its core until it made one -- the flag
    // raised by the tick was only read at the next call. Interrupted ring
    // 3 code holds no kernel lock, so switching here is safe; ring 0 code
    // keeps the cooperative rule it was written for.
    if frame.code_segment & 3 == 3 {
        preempt_user();
    }
}

/// Preempts the current task from an interrupt taken in ring 3.
///
/// Interrupt handlers do not swap GS, and ring 3 runs with the user GS
/// base loaded. A switch from here would hand that base to the next task,
/// which may be in the middle of a system call and would then leave it
/// through `swapgs; sysret` with the two bases exchanged -- its next
/// system call faults on `gs:`. So the kernel base goes in around the
/// switch, and the user base comes back before the handler's `iretq`.
fn preempt_user() {
    unsafe { asm!("swapgs", options(nomem, nostack)) };
    crate::scheduler::preempt_if_needed();
    unsafe { asm!("swapgs", options(nomem, nostack)) };
}

/// Rescheduling IPI: another core signals that a task has just become
/// ready on ours.
extern "x86-interrupt" fn reschedule_ipi(frame: InterruptStackFrame) {
    let cpu = crate::smp::current_cpu_id();
    crate::smp::record_ipi(cpu);
    crate::apic::end_of_interrupt();
    crate::scheduler::on_reschedule_ipi(cpu);
    if frame.code_segment & 3 == 3 {
        preempt_user();
    }
}

/// TLB shootdown IPI: another core has taken a translation down.
///
/// The handler does one thing and takes no lock: a shootdown may arrive
/// while the sender holds a lock this core would otherwise wait for, and
/// a handler that took anything would turn an invalidation into a
/// deadlock. It does **not** reschedule either -- a core switching task
/// in the middle of somebody else's page-table edit is exactly what the
/// separate vector exists to avoid.
extern "x86-interrupt" fn tlb_shootdown_ipi(_frame: InterruptStackFrame) {
    crate::memory::sync_tlb();
    crate::apic::end_of_interrupt();
}

/// Keyboard IRQ. Routed to core 0 by the I/O APIC.
///
/// Since step 6.1.1, this handler decodes nothing: it counts the
/// occurrence and wakes the driver task, which will read port 0x60 and
/// publish the event. An interrupt handler translating a scancode and
/// printing it on screen put an interface decision -- "a key produces a
/// character" -- inside the kernel, and admitted only one consumer.
extern "x86-interrupt" fn keyboard_interrupt(_frame: InterruptStackFrame) {
    // Acknowledge **before** the wake-up: `notify` takes the scheduler
    // lock and may raise an IPI, and as long as the LAPIC considers the
    // interrupt in service it delivers no other. The line is edge
    // triggered, so acknowledging early does not make it come back --
    // which would not be true of a PCI line.
    crate::apic::end_of_interrupt();
    crate::drivers::irq::notify(crate::apic::VECTOR_KEYBOARD);
}

/// Generates a device interrupt relay.
///
/// The body is the same for the sixteen lines, and it is deliberately
/// minimal: count, wake, acknowledge. No device register is read here --
/// the kernel does not know what the line it has just received is, and
/// that is what lets a driver live entirely outside the interrupt path.
macro_rules! device_irq {
    ($name:ident, $line:expr) => {
        extern "x86-interrupt" fn $name(_frame: InterruptStackFrame) {
            crate::apic::end_of_interrupt();
            crate::drivers::irq::notify(crate::drivers::VECTOR_DEVICE_BASE + $line);
        }
    };
}

device_irq!(device_irq_0, 0);
device_irq!(device_irq_1, 1);
device_irq!(device_irq_2, 2);
device_irq!(device_irq_3, 3);
device_irq!(device_irq_4, 4);
device_irq!(device_irq_5, 5);
device_irq!(device_irq_6, 6);
device_irq!(device_irq_7, 7);
device_irq!(device_irq_8, 8);
device_irq!(device_irq_9, 9);
device_irq!(device_irq_10, 10);
device_irq!(device_irq_11, 11);
device_irq!(device_irq_12, 12);
device_irq!(device_irq_13, 13);
device_irq!(device_irq_14, 14);
device_irq!(device_irq_15, 15);

/// The sixteen relays, in line order.
const DEVICE_HANDLERS: [extern "x86-interrupt" fn(InterruptStackFrame); 16] = [
    device_irq_0,
    device_irq_1,
    device_irq_2,
    device_irq_3,
    device_irq_4,
    device_irq_5,
    device_irq_6,
    device_irq_7,
    device_irq_8,
    device_irq_9,
    device_irq_10,
    device_irq_11,
    device_irq_12,
    device_irq_13,
    device_irq_14,
    device_irq_15,
];

/// Generates an MSI relay: the same count-and-wake as a line, for a
/// vector a PCI function writes to by message (`pci::Msix`).
///
/// It also preempts ring 3 like the timer does. The task it wakes is
/// often a driver sitting on this very core -- core 0, where messages are
/// aimed -- and with a single core, ring 3 code runs there too: without
/// this, the driver would wait for the interrupted program's next tick.
macro_rules! msi_irq {
    ($name:ident, $index:expr) => {
        extern "x86-interrupt" fn $name(frame: InterruptStackFrame) {
            crate::apic::end_of_interrupt();
            crate::drivers::irq::notify(crate::drivers::VECTOR_MSI_BASE + $index);
            if frame.code_segment & 3 == 3 {
                preempt_user();
            }
        }
    };
}

msi_irq!(msi_irq_0, 0);
msi_irq!(msi_irq_1, 1);
msi_irq!(msi_irq_2, 2);
msi_irq!(msi_irq_3, 3);

const MSI_HANDLERS: [extern "x86-interrupt" fn(InterruptStackFrame); crate::drivers::MSI_VECTORS as usize] =
    [msi_irq_0, msi_irq_1, msi_irq_2, msi_irq_3];

/// Spurious interrupt: the LAPIC raises it when an IRQ disappears before
/// being delivered. No EOI must be sent for this vector.
extern "x86-interrupt" fn spurious_interrupt(_frame: InterruptStackFrame) {
    crate::serial_println!(
        "[apic] interruption parasite sur le coeur {}",
        crate::smp::current_cpu_id()
    );
}

/// Internal LAPIC error.
extern "x86-interrupt" fn apic_error(_frame: InterruptStackFrame) {
    crate::serial_println!(
        "[apic] ERREUR interne sur le coeur {} : 0x{:08x}",
        crate::smp::current_cpu_id(),
        crate::apic::error_status()
    );
    crate::apic::end_of_interrupt();
}

// ============================================================
//  Loading
// ============================================================

pub fn init() {
    unsafe {
        let idt = &raw mut IDT;

        (*idt)[0].set_handler(divide_error as *const ());
        (*idt)[1].set_handler(debug_trap_entry as *const ());
        (*idt)[2].set_handler(non_maskable_interrupt as *const ());
        (*idt)[3].set_handler(breakpoint_entry as *const ());
        (*idt)[3].set_user_callable();
        (*idt)[4].set_handler(overflow as *const ());
        (*idt)[5].set_handler(bound_range_exceeded as *const ());
        (*idt)[6].set_handler(invalid_opcode as *const ());
        (*idt)[7].set_handler(device_not_available as *const ());

        // Double fault: handler + switch to the IST stack of the core.
        (*idt)[8].set_handler(double_fault as *const ());
        (*idt)[8].set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);

        (*idt)[9].set_handler(reserved_exception as *const ());
        (*idt)[10].set_handler(invalid_tss as *const ());
        (*idt)[11].set_handler(segment_not_present as *const ());
        (*idt)[12].set_handler(stack_segment_fault as *const ());
        (*idt)[13].set_handler(general_protection_fault as *const ());
        // 11.3: an assembly entry, so the handler can edit the frame
        // (copy fixup) and resolve demand-paged faults.
        (*idt)[14].set_handler(page_fault_entry as *const ());
        (*idt)[15].set_handler(reserved_exception as *const ());
        (*idt)[16].set_handler(x87_floating_point as *const ());
        (*idt)[17].set_handler(alignment_check as *const ());
        (*idt)[18].set_handler(machine_check as *const ());
        (*idt)[19].set_handler(simd_floating_point as *const ());
        (*idt)[20].set_handler(virtualization as *const ());
        (*idt)[21].set_handler(control_protection as *const ());

        for vector in 22..=27 {
            (*idt)[vector].set_handler(reserved_exception as *const ());
        }
        (*idt)[28].set_handler(hypervisor_injection as *const ());
        (*idt)[29].set_handler(vmm_communication as *const ());
        (*idt)[30].set_handler(security_exception as *const ());
        (*idt)[31].set_handler(reserved_exception as *const ());

        // --- hardware vectors ---
        (*idt)[crate::apic::VECTOR_TIMER as usize].set_handler(timer_interrupt as *const ());
        (*idt)[crate::apic::VECTOR_KEYBOARD as usize].set_handler(keyboard_interrupt as *const ());
        (*idt)[crate::apic::VECTOR_RESCHEDULE as usize].set_handler(reschedule_ipi as *const ());
        (*idt)[crate::apic::VECTOR_TLB as usize].set_handler(tlb_shootdown_ipi as *const ());
        (*idt)[crate::apic::VECTOR_ERROR as usize].set_handler(apic_error as *const ());
        (*idt)[crate::apic::VECTOR_SPURIOUS as usize].set_handler(spurious_interrupt as *const ());

        // 6.1.6: sixteen device vectors, one per I/O APIC line. They are
        // laid down at initialisation, before any line is unmasked: a
        // line routed to a vector with no handler produces a
        // segment-not-present exception, that is to say a triple fault on
        // the first packet received.
        for line in 0..16usize {
            let vector = crate::drivers::VECTOR_DEVICE_BASE as usize + line;
            (*idt)[vector].set_handler(DEVICE_HANDLERS[line] as *const ());
        }
        for (index, handler) in MSI_HANDLERS.iter().enumerate() {
            let vector = crate::drivers::VECTOR_MSI_BASE as usize + index;
            (*idt)[vector].set_handler(*handler as *const ());
        }
    }

    load();
}

/// Loads the existing IDT on the current core.
pub fn load() {
    unsafe {
        let idt = &raw mut IDT;
        let pointer = DescriptorTablePointer {
            limit: (size_of::<[Entry; 256]>() - 1) as u16,
            base: idt as u64,
        };
        asm!(
            "lidt [{}]",
            in(reg) &pointer,
            options(readonly, nostack, preserves_flags)
        );
    }
}

/// Deliberately raises an `int3` to check that the IDT answers.
pub fn test_breakpoint() {
    unsafe { asm!("int3", options(nomem, nostack)) };
}

/// Enables the hardware interrupts.
///
/// # Safety
/// The IDT must be loaded and every active vector given a handler.
pub unsafe fn enable_interrupts() {
    unsafe { asm!("sti", options(nomem, nostack)) };
}

/// Masks the hardware interrupts.
pub fn disable_interrupts() {
    unsafe { asm!("cli", options(nomem, nostack)) };
}

/// Are interrupts enabled? Bit 9 of RFLAGS.
pub fn interrupts_enabled() -> bool {
    let flags: u64;
    unsafe {
        asm!("pushfq", "pop {}", out(reg) flags, options(nomem, preserves_flags));
    }
    flags & (1 << 9) != 0
}

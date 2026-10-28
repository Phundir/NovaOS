//! Power off and reboot.
//!
//! ## Power off: ACPI S5, and nothing else
//!
//! "Soft off" is written into the PM1 control register: the sleep type of
//! the `\_S5` package in bits 10-12, and SLP_EN in bit 13. Both the port
//! and the sleep type come from the firmware (`acpi::parse_fadt`); none
//! is hard-coded. The well-known `outw(0x604, 0x2000)` is exactly this
//! write with QEMU's values, and writing it blind on a real machine would
//! poke a port that belongs to whatever the chipset put there.
//!
//! When the tables say nothing usable, `power_off` **returns** with the
//! reason. A power-off that silently halts the processors leaves a screen
//! frozen on the desktop, indistinguishable from a hang; a refusal lets
//! the session say "extinction impossible" and stay usable.
//!
//! ## Reboot: four methods, from the cleanest to the bluntest
//!
//! 1. the FADT reset register, when the firmware declares one in I/O space;
//! 2. the reset control register at 0xCF9 (PIIX, ICH and their successors),
//!    written 0x02 then 0x06 -- the first write selects a hard reset, the
//!    rising edge of bit 2 triggers it;
//! 3. the keyboard controller pulse, command 0xFE on port 0x64, which is
//!    wired to the processor's reset line on every PC since the AT;
//! 4. a triple fault: an empty IDT, then an exception. The processor cannot
//!    deliver it, cannot deliver the double fault either, and shuts down --
//!    which the chipset turns into a reset.
//!
//! The fourth cannot fail, so `reboot` never returns.
//!
//! ## What is flushed first
//!
//! The disk caches, by `prepare`. That is the only state whose loss would
//! outlive the reboot. It is **not** a clean shutdown of the tasks: a task
//! writing a file at that moment loses what it had not handed to the
//! filesystem yet. Named here because it is a real limit.

use core::arch::asm;

use crate::port::{inb, inw, outb, outw};
use crate::vga::SpinLock;

/// Power controls read from the ACPI tables.
#[derive(Debug, Clone, Copy)]
pub struct PowerInfo {
    /// Was a FADT found at all?
    pub present: bool,
    pub pm1a_control: u16,
    pub pm1b_control: u16,
    /// SMI command port and the value that hands ACPI to the OS.
    pub smi_command: u16,
    pub acpi_enable: u8,
    /// SLP_TYPa and SLP_TYPb of `\_S5`, if the DSDT declares them.
    pub s5: Option<(u8, u8)>,
    /// FADT reset register in I/O space: port and value.
    pub reset: Option<(u16, u8)>,
}

impl PowerInfo {
    pub const fn empty() -> Self {
        PowerInfo {
            present: false,
            pm1a_control: 0,
            pm1b_control: 0,
            smi_command: 0,
            acpi_enable: 0,
            s5: None,
            reset: None,
        }
    }

    /// Can `power_off` be attempted at all?
    pub fn can_power_off(&self) -> bool {
        self.present && self.pm1a_control != 0 && self.s5.is_some()
    }
}

static INFO: SpinLock<PowerInfo> = SpinLock::new(PowerInfo::empty());

/// SLP_EN, bit 13 of PM1_CNT.
const SLP_EN: u16 = 1 << 13;
/// SCI_EN, bit 0 of PM1_CNT: set when the OS owns ACPI.
const SCI_EN: u16 = 1;
/// Reset control register of the PCI host bridge.
const RESET_CONTROL: u16 = 0xCF9;
/// Keyboard controller command and status port.
const KBC_COMMAND: u16 = 0x64;

/// Spin iterations between two methods, and after a power-off write.
///
/// Counted in `pause` instructions, not in ticks: interrupts are masked
/// from the first write on, so the tick counter of this core stops. The
/// value is a few hundred milliseconds on a current processor, and far
/// less under QEMU's TCG, where the write takes effect before the next
/// instruction anyway.
const SETTLE_SPINS: u32 = 20_000_000;

/// Records what `acpi::parse` found. Called once, at boot.
pub fn init(info: PowerInfo) {
    *INFO.lock() = info;
}

pub fn info() -> PowerInfo {
    *INFO.lock()
}

/// Writes the disk caches out. Returns `true` when every volume flushed.
///
/// Must be called with no lock held: it takes the filesystem lock and does
/// I/O under it.
pub fn prepare() -> bool {
    match crate::fs::flush_all() {
        Ok(()) => {
            crate::serial_println!("[power] caches disque ecrits");
            true
        }
        Err(error) => {
            crate::serial_println!("[power] ECHEC de l'ecriture des caches : {:?}", error);
            false
        }
    }
}

fn settle() {
    for _ in 0..SETTLE_SPINS {
        core::hint::spin_loop();
    }
}

/// Powers the machine off through ACPI S5.
///
/// Returns only on failure, with the reason. Interrupts are restored to
/// their previous state before returning.
pub fn power_off() -> &'static str {
    let info = info();
    if !info.present {
        return "pas de FADT";
    }
    let Some((type_a, type_b)) = info.s5 else {
        return "\\_S5 absent de la DSDT";
    };
    if info.pm1a_control == 0 {
        return "pas de port PM1a";
    }

    crate::serial_println!(
        "[power] extinction ACPI - PM1a 0x{:04x} <- 0x{:04x}",
        info.pm1a_control,
        ((type_a as u16) << 10) | SLP_EN
    );

    let enabled = crate::idt::interrupts_enabled();
    crate::idt::disable_interrupts();

    // SAFETY: the ports come from the FADT, which names them as the PM1
    // control blocks and the SMI command port of this very chipset.
    unsafe {
        // SLP_EN is ignored while the firmware still owns ACPI. Handing it
        // over is a write to the SMI port, answered by setting SCI_EN;
        // bounded, because a firmware that never answers must not hang the
        // power-off path.
        if inw(info.pm1a_control) & SCI_EN == 0
            && info.smi_command != 0
            && info.acpi_enable != 0
        {
            outb(info.smi_command, info.acpi_enable);
            for _ in 0..SETTLE_SPINS {
                if inw(info.pm1a_control) & SCI_EN != 0 {
                    break;
                }
                core::hint::spin_loop();
            }
        }

        outw(info.pm1a_control, ((type_a as u16) << 10) | SLP_EN);
        if info.pm1b_control != 0 {
            outw(info.pm1b_control, ((type_b as u16) << 10) | SLP_EN);
        }
    }

    settle();

    if enabled {
        // SAFETY: they were enabled on entry, so the IDT is loaded and
        // every vector has its handler.
        unsafe { crate::idt::enable_interrupts() };
    }
    "le materiel n'a pas coupe l'alimentation"
}

/// Reboots the machine. Never returns.
pub fn reboot() -> ! {
    let info = info();
    crate::idt::disable_interrupts();

    // SAFETY, for the three port writes below: each is the documented reset
    // mechanism of the device it addresses, and a write that the hardware
    // does not implement is ignored by the bus. Nothing is left in a state
    // that matters: the next step is a reset either way.
    if let Some((port, value)) = info.reset {
        crate::serial_println!(
            "[power] redemarrage : registre FADT 0x{:04x} <- 0x{:02x}",
            port,
            value
        );
        unsafe { outb(port, value) };
        settle();
    }

    crate::serial_println!("[power] redemarrage : controle de reinitialisation 0xCF9");
    unsafe {
        outb(RESET_CONTROL, 0x02);
        for _ in 0..1000 {
            core::hint::spin_loop();
        }
        outb(RESET_CONTROL, 0x06);
    }
    settle();

    crate::serial_println!("[power] redemarrage : impulsion du controleur clavier");
    unsafe {
        // Bit 1 of the status: input buffer full. The command is only taken
        // once it is empty; bounded, because a machine with no 8042 answers
        // 0xFF forever.
        for _ in 0..100_000 {
            if inb(KBC_COMMAND) & 0x02 == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        outb(KBC_COMMAND, 0xFE);
    }
    settle();

    crate::serial_println!("[power] redemarrage : triple faute");
    triple_fault()
}

/// Loads an empty IDT and raises an exception.
fn triple_fault() -> ! {
    #[repr(C, packed)]
    struct Descriptor {
        limit: u16,
        base: u64,
    }
    let empty = Descriptor { limit: 0, base: 0 };
    // SAFETY: this is meant to bring the processor down. With a zero limit
    // every vector is out of bounds: `int3` raises #GP, which cannot be
    // delivered either, which raises #DF, whose failure is the shutdown the
    // chipset answers with a reset.
    unsafe {
        asm!(
            "lidt [{}]",
            "int3",
            in(reg) &raw const empty,
            options(noreturn, nostack)
        );
    }
}

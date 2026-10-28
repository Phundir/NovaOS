//! I/O APIC: routing of the external hardware interrupts.
//!
//! Where the LAPIC belongs to each core, the I/O APIC is unique on the
//! motherboard. It receives the IRQ lines of the devices and decides, for
//! each one, the vector raised and the destination core.
//!
//! Access to it is indirect: the index of the wanted register is written
//! into a selection window, then the data is read or written in a second.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::memory::phys_to_virt;

/// Default physical address. The exact value is in the MADT table of the
/// ACPI, which will be parsed in phase 2.7.1.
const DEFAULT_PHYS_BASE: u64 = 0xFEC0_0000;

const REG_SELECT: u64 = 0x00;
const REG_DATA: u64 = 0x10;

const IOAPIC_ID: u32 = 0x00;
const IOAPIC_VERSION: u32 = 0x01;
/// First entry of the redirection table; each one occupies 2 registers.
const IOAPIC_REDTBL: u32 = 0x10;

/// Bit 16 of a redirection entry: masks the line.
const REDIR_MASKED: u64 = 1 << 16;

static IOAPIC_BASE: AtomicU64 = AtomicU64::new(0);

fn base() -> u64 {
    IOAPIC_BASE.load(Ordering::Relaxed)
}

fn read(reg: u32) -> u32 {
    unsafe {
        ((base() + REG_SELECT) as *mut u32).write_volatile(reg);
        ((base() + REG_DATA) as *const u32).read_volatile()
    }
}

fn write(reg: u32, value: u32) {
    unsafe {
        ((base() + REG_SELECT) as *mut u32).write_volatile(reg);
        ((base() + REG_DATA) as *mut u32).write_volatile(value);
    }
}

/// Initialises the I/O APIC and masks all of its lines.
///
/// # Safety
/// To be called after the LAPIC initialisation.
pub unsafe fn init() {
    unsafe { crate::memory::set_mmio_uncached(DEFAULT_PHYS_BASE) };
    IOAPIC_BASE.store(phys_to_virt(DEFAULT_PHYS_BASE), Ordering::Relaxed);

    // Every line masked to start with: only what we handle gets enabled.
    for irq in 0..max_redirection_entries() {
        mask(irq);
    }
}

pub fn id() -> u32 {
    (read(IOAPIC_ID) >> 24) & 0xF
}

pub fn version() -> u32 {
    read(IOAPIC_VERSION) & 0xFF
}

/// Number of IRQ lines handled -- typically 24.
pub fn max_redirection_entries() -> u8 {
    (((read(IOAPIC_VERSION) >> 16) & 0xFF) + 1) as u8
}

/// Routes an IRQ line to a vector, on the given core.
///
/// Format of a redirection entry:
///   bits 0-7   : vector raised
///   bits 8-10  : delivery mode (000 = fixed)
///   bit 11     : destination mode (0 = physical)
///   bit 13     : polarity (0 = active high)
///   bit 15     : trigger (0 = edge)
///   bit 16     : mask
///   bits 56-63 : destination APIC id
pub fn set_irq(irq: u8, vector: u8, lapic_id: u8) {
    let index = IOAPIC_REDTBL + (irq as u32) * 2;
    let entry: u64 = (vector as u64) | ((lapic_id as u64) << 56);

    // The high entry first: the low one carries the mask bit, and is left
    // to be unmasked last.
    write(index + 1, (entry >> 32) as u32);
    write(index, entry as u32);
}

pub fn mask(irq: u8) {
    let index = IOAPIC_REDTBL + (irq as u32) * 2;
    let low = read(index);
    write(index, low | REDIR_MASKED as u32);
}

pub fn unmask(irq: u8) {
    let index = IOAPIC_REDTBL + (irq as u32) * 2;
    let low = read(index);
    write(index, low & !(REDIR_MASKED as u32));
}

pub fn dump_state() {
    crate::serial_println!(
        "[ioapic] id {} version 0x{:02x} - {} lignes - base virtuelle 0x{:016x}",
        id(),
        version(),
        max_redirection_entries(),
        base()
    );

    for irq in 0..max_redirection_entries().min(16) {
        let index = IOAPIC_REDTBL + (irq as u32) * 2;
        let low = read(index);
        if low & REDIR_MASKED as u32 == 0 {
            crate::serial_println!(
                "[ioapic]   IRQ {:2} -> vecteur {} (active)",
                irq,
                low & 0xFF
            );
        }
    }
}

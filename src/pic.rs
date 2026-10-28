//! Shutdown of the legacy 8259 PIC.
//!
//! Two cascaded controllers inherited from the PC/AT: the master on ports
//! 0x20/0x21, the slave on 0xA0/0xA1. On leaving the BIOS they are
//! configured to raise vectors 0x08-0x0F and 0x70-0x77 -- squarely on top
//! of the CPU exceptions, which makes a keyboard IRQ indistinguishable
//! from a double fault.
//!
//! Masking them is not enough: the PIC raises spurious interrupts
//! (spurious IRQ 7) even when masked. So they are first remapped to a
//! harmless range, then everything is masked.

use crate::port::{inb, outb};

const PIC1_COMMAND: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_COMMAND: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

const ICW1_INIT: u8 = 0x11; // initialisation + ICW4 expected
const ICW4_8086: u8 = 0x01; // 8086/88 mode

/// Remap base: above the 32 exceptions, outside the APIC range.
const PIC1_OFFSET: u8 = 0x20;
const PIC2_OFFSET: u8 = 0x28;

/// A short delay: the older 8259s need a pause between two writes. One
/// write to port 0x80 (unused) is enough.
#[inline]
unsafe fn io_wait() {
    unsafe { outb(0x80, 0) };
}

/// Remaps then fully masks both controllers.
///
/// # Safety
/// To be called once only, with interrupts masked.
pub unsafe fn disable() {
    unsafe {
        // ICW1..ICW4 initialisation sequence.
        outb(PIC1_COMMAND, ICW1_INIT);
        io_wait();
        outb(PIC2_COMMAND, ICW1_INIT);
        io_wait();

        // ICW2: base vector.
        outb(PIC1_DATA, PIC1_OFFSET);
        io_wait();
        outb(PIC2_DATA, PIC2_OFFSET);
        io_wait();

        // ICW3: cascade wiring -- slave on IRQ 2 of the master.
        outb(PIC1_DATA, 4);
        io_wait();
        outb(PIC2_DATA, 2);
        io_wait();

        // ICW4: 8086 mode.
        outb(PIC1_DATA, ICW4_8086);
        io_wait();
        outb(PIC2_DATA, ICW4_8086);
        io_wait();

        // Full mask: no IRQ will leave the PIC any more.
        outb(PIC1_DATA, 0xFF);
        outb(PIC2_DATA, 0xFF);
    }
}

/// Reads the current masks -- a diagnostic check.
pub fn read_masks() -> (u8, u8) {
    unsafe { (inb(PIC1_DATA), inb(PIC2_DATA)) }
}

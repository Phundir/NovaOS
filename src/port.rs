//! Access to the x86 input/output ports.
//!
//! Legacy devices (UART, PIC, PIT, CRTC controller) are not memory
//! mapped: they are addressed through a separate 64 KiB I/O space,
//! reachable only by the `in` and `out` instructions.

use core::arch::asm;

/// Writes one byte to an I/O port.
///
/// # Safety
/// Writing to an arbitrary port can reconfigure the hardware in
/// unpredictable ways. The caller must guarantee port and value are valid.
#[inline]
pub unsafe fn outb(port: u16, value: u8) {
    unsafe {
        asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

/// Reads one byte from an I/O port.
///
/// # Safety
/// Some reads have hardware side effects (acknowledgement, register
/// flush). The caller must guarantee the port is valid.
#[inline]
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        asm!(
            "in al, dx",
            out("al") value,
            in("dx") port,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

/// Writes a 32-bit word to an I/O port.
///
/// Required by the PCI configuration space, which is addressed
/// exclusively in 32-bit words through the port pair 0xCF8 / 0xCFC.
///
/// # Safety
/// Same precautions as `outb`.
#[inline]
pub unsafe fn outl(port: u16, value: u32) {
    unsafe {
        asm!(
            "out dx, eax",
            in("dx") port,
            in("eax") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

/// Reads a 32-bit word from an I/O port.
///
/// # Safety
/// Same precautions as `inb`.
#[inline]
pub unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    unsafe {
        asm!(
            "in eax, dx",
            out("eax") value,
            in("dx") port,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

/// Reads a 16-bit word from an I/O port.
///
/// # Safety
/// Same precautions as `inb`: the port must exist and reading it must
/// have no unexpected side effect. The AC'97 mixer exposes all of its
/// registers on sixteen bits.
#[inline]
pub unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    unsafe {
        asm!(
            "in ax, dx",
            out("ax") value,
            in("dx") port,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

/// Writes a 16-bit word to an I/O port.
///
/// # Safety
/// Same precautions as `outb`.
#[inline]
pub unsafe fn outw(port: u16, value: u16) {
    unsafe {
        asm!(
            "out dx, ax",
            in("dx") port,
            in("ax") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

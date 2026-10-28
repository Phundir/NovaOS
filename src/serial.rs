//! 16550 UART driver for the serial debug console.
//!
//! COM1 is exposed on port 0x3F8. QEMU redirects that device to stdio
//! with the `-serial stdio` option, which gives a complete boot log in
//! the host terminal, with no line limit.

use core::fmt;

use crate::port::{inb, outb};
use crate::vga::SpinLock;

const COM1: u16 = 0x3F8;

// Register offsets relative to the base port.
const DATA: u16 = 0;
const INT_ENABLE: u16 = 1;
const FIFO_CTRL: u16 = 2;
const LINE_CTRL: u16 = 3;
const MODEM_CTRL: u16 = 4;
const LINE_STATUS: u16 = 5;

const LINE_STATUS_THR_EMPTY: u8 = 1 << 5;
const LINE_STATUS_DATA_READY: u8 = 1 << 0;

pub struct SerialPort {
    base: u16,
    initialized: bool,
}

impl SerialPort {
    pub const fn new(base: u16) -> Self {
        SerialPort {
            base,
            initialized: false,
        }
    }

    /// Configures the UART at 38400 baud, 8 bits, no parity, 1 stop bit.
    pub fn init(&mut self) {
        unsafe {
            outb(self.base + INT_ENABLE, 0x00); // interrupts disabled
            outb(self.base + LINE_CTRL, 0x80); // DLAB: access to the divisor
            outb(self.base + DATA, 0x03); // divisor, low byte
            outb(self.base + INT_ENABLE, 0x00); // divisor, high byte
            outb(self.base + LINE_CTRL, 0x03); // 8N1, DLAB disabled
            outb(self.base + FIFO_CTRL, 0xC7); // FIFO enabled and flushed
            outb(self.base + MODEM_CTRL, 0x0B); // DTR + RTS + OUT2
        }
        self.initialized = true;
    }

    fn wait_for_transmit(&self) {
        while unsafe { inb(self.base + LINE_STATUS) } & LINE_STATUS_THR_EMPTY == 0 {
            core::hint::spin_loop();
        }
    }

    pub fn write_byte(&mut self, byte: u8) {
        if !self.initialized {
            return;
        }
        // Terminals expect CRLF where Rust emits only LF.
        if byte == b'\n' {
            self.wait_for_transmit();
            unsafe { outb(self.base + DATA, b'\r') };
        }
        self.wait_for_transmit();
        unsafe { outb(self.base + DATA, byte) };
    }
}

impl SerialPort {
    /// A byte received, if one is waiting (polled: the boot never arms
    /// the UART's interrupt).
    pub fn read_byte(&mut self) -> Option<u8> {
        if !self.initialized {
            return None;
        }
        // SAFETY: the base port is COM1's, programmed by `init`; reading
        // the line status and the data register has no other effect than
        // consuming the received byte.
        unsafe {
            if inb(self.base + LINE_STATUS) & LINE_STATUS_DATA_READY == 0 {
                return None;
            }
            Some(inb(self.base + DATA))
        }
    }
}

impl fmt::Write for SerialPort {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            self.write_byte(byte);
        }
        Ok(())
    }
}

pub static SERIAL1: SpinLock<SerialPort> = SpinLock::new(SerialPort::new(COM1));

pub fn init() {
    SERIAL1.lock().init();
}

/// A byte typed on the serial console, if any (the shell, spec 2.2).
pub fn try_read() -> Option<u8> {
    SERIAL1.lock().read_byte()
}

/// Raw bytes, control characters included (the shell's backspace).
pub fn write_bytes(bytes: &[u8]) {
    let mut port = SERIAL1.lock();
    for byte in bytes {
        port.write_byte(*byte);
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    SERIAL1.lock().write_fmt(args).unwrap();
}

#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => ($crate::serial::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($($arg:tt)*) => ($crate::serial_print!("{}\n", format_args!($($arg)*)));
}

/// Writes to the VGA screen and the serial console at the same time.
#[macro_export]
macro_rules! kprintln {
    () => {{ $crate::println!(); $crate::serial_println!(); }};
    ($($arg:tt)*) => {{
        $crate::println!($($arg)*);
        $crate::serial_println!($($arg)*);
    }};
}

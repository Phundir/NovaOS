//! VGA text mode display driver (80x25).
//!
//! The hardware buffer sits at physical address 0xb8000, identity-mapped
//! by the page tables installed in boot.asm. Each cell occupies 16 bits:
//! the low byte holds the ASCII code (code page 437), the high byte the
//! background/foreground colour pair.

use core::cell::UnsafeCell;
use core::fmt;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

const VGA_BUFFER: *mut u16 = (crate::memory::PHYS_OFFSET + 0xb8000) as *mut u16;
const BUFFER_WIDTH: usize = 80;
const BUFFER_HEIGHT: usize = 25;

/// The 16 colours of the VGA text palette.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Color {
    Black = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Magenta = 5,
    Brown = 6,
    LightGray = 7,
    DarkGray = 8,
    LightBlue = 9,
    LightGreen = 10,
    LightCyan = 11,
    LightRed = 12,
    Pink = 13,
    Yellow = 14,
    White = 15,
}

/// (background, foreground) pair encoded in one byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct ColorCode(u8);

impl ColorCode {
    pub const fn new(foreground: Color, background: Color) -> Self {
        ColorCode((background as u8) << 4 | (foreground as u8))
    }
}

// ============================================================
//  Spin lock
// ============================================================

/// Minimal spinlock. Masks interrupts during the critical section:
/// without that, a handler taking the same lock would deadlock against
/// the code it has just interrupted.
pub struct SpinLock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

// Safe: access to the data is serialised by the atomic flag.
unsafe impl<T: Send> Sync for SpinLock<T> {}
unsafe impl<T: Send> Send for SpinLock<T> {}

impl<T> SpinLock<T> {
    pub const fn new(data: T) -> Self {
        SpinLock {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(data),
        }
    }

    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        let were_enabled = crate::idt::interrupts_enabled();
        if were_enabled {
            crate::idt::disable_interrupts();
        }

        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }

        SpinLockGuard {
            lock: self,
            restore_interrupts: were_enabled,
        }
    }

    /// Takes the lock only if it is free. For the panic path, which must
    /// not wait on a lock the stopped code may hold for ever.
    pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
        let were_enabled = crate::idt::interrupts_enabled();
        if were_enabled {
            crate::idt::disable_interrupts();
        }
        if self.locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            if were_enabled {
                unsafe { crate::idt::enable_interrupts() };
            }
            return None;
        }
        Some(SpinLockGuard {
            lock: self,
            restore_interrupts: were_enabled,
        })
    }
}

pub struct SpinLockGuard<'a, T> {
    lock: &'a SpinLock<T>,
    restore_interrupts: bool,
}

impl<T> Deref for SpinLockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for SpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for SpinLockGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        if self.restore_interrupts {
            unsafe { crate::idt::enable_interrupts() };
        }
    }
}

// ============================================================
//  Writer
// ============================================================

/// Always writes on the last visible line and scrolls the rest upwards,
/// like an ordinary terminal.
pub struct Writer {
    column: usize,
    color_code: ColorCode,
}

impl Writer {
    pub const fn new() -> Self {
        Writer {
            column: 0,
            color_code: ColorCode::new(Color::LightGray, Color::Black),
        }
    }

    pub fn set_color(&mut self, foreground: Color, background: Color) {
        self.color_code = ColorCode::new(foreground, background);
    }

    fn cell(&self, byte: u8) -> u16 {
        (self.color_code.0 as u16) << 8 | (byte as u16)
    }

    fn write_cell(&self, row: usize, col: usize, cell: u16) {
        let index = row * BUFFER_WIDTH + col;
        unsafe { VGA_BUFFER.add(index).write_volatile(cell) }
    }

    fn read_cell(&self, row: usize, col: usize) -> u16 {
        let index = row * BUFFER_WIDTH + col;
        unsafe { VGA_BUFFER.add(index).read_volatile() }
    }

    pub fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.new_line(),
            b'\r' => self.column = 0,
            byte => {
                if self.column >= BUFFER_WIDTH {
                    self.new_line();
                }
                let cell = self.cell(byte);
                self.write_cell(BUFFER_HEIGHT - 1, self.column, cell);
                self.column += 1;
            }
        }
    }

    /// Bytes outside the printable range become a solid square, which
    /// avoids displaying stray control characters.
    pub fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            match byte {
                0x20..=0x7e | b'\n' | b'\r' => self.write_byte(byte),
                _ => self.write_byte(0xfe),
            }
        }
    }

    fn new_line(&mut self) {
        for row in 1..BUFFER_HEIGHT {
            for col in 0..BUFFER_WIDTH {
                let cell = self.read_cell(row, col);
                self.write_cell(row - 1, col, cell);
            }
        }
        self.clear_row(BUFFER_HEIGHT - 1);
        self.column = 0;
    }

    fn clear_row(&self, row: usize) {
        let blank = self.cell(b' ');
        for col in 0..BUFFER_WIDTH {
            self.write_cell(row, col, blank);
        }
    }

    pub fn clear_screen(&mut self) {
        for row in 0..BUFFER_HEIGHT {
            self.clear_row(row);
        }
        self.column = 0;
    }
}

/// 1.5.2 -- lets format_args! feed the writer, hence the use of {} and
/// {:?} in print!/println!.
impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

// ============================================================
//  Global interface
// ============================================================

pub static WRITER: SpinLock<Writer> = SpinLock::new(Writer::new());

/// 7.1.1 -- Switch towards the graphics console.
///
/// As soon as GRUB has programmed a graphics mode, the buffer at 0xb8000
/// is no longer displayed: writing to it would not crash, but would not
/// be seen. The three public functions of this module therefore divert
/// towards `graphics`, which leaves unchanged the hundreds of calls to
/// `print!` and `set_color` scattered through the kernel.
///
/// The switch is an atomic flag test, not a lock acquisition: it precedes
/// the decision of which screen to touch, and the two screens each have
/// their own.
#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    if crate::graphics::is_active() {
        crate::graphics::print(args);
        return;
    }
    use core::fmt::Write;
    WRITER.lock().write_fmt(args).unwrap();
}

pub fn clear_screen() {
    if crate::graphics::is_active() {
        crate::graphics::clear_screen();
        return;
    }
    WRITER.lock().clear_screen();
}

pub fn set_color(foreground: Color, background: Color) {
    if crate::graphics::is_active() {
        crate::graphics::set_color(foreground, background);
        return;
    }
    WRITER.lock().set_color(foreground, background);
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::vga::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

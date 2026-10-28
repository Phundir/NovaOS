//! 7.1 -- Graphics output.
//!
//! ## Complete path, from the Multiboot2 header to the pixel
//!
//! 1. `boot/boot.asm` carries a tag 5 in the Multiboot2 header: that is
//!    what asks GRUB to program a graphics mode. Without it, GRUB leaves
//!    the card in 80x25 text and the kernel has no framebuffer.
//! 2. GRUB chooses a mode through VBE and describes it in a tag 8, which
//!    `multiboot::parse` decodes into a `FramebufferInfo`.
//! 3. `init` latches that framebuffer and switches the display: from
//!    then on, `print!` paints glyphs instead of writing at 0xb8000.
//! 4. `enable_double_buffer` adds, once the allocator is in place, the
//!    back buffer and the write-combining mapping.
//! 5. `virtio_gpu` may then **replace** that framebuffer with one whose
//!    size follows the host window, and replace it again whenever the
//!    window changes size. The GRUB framebuffer stays the path of every
//!    machine without a virtio-gpu.
//!
//! ## Two steps, and why not one
//!
//! The back buffer claims frames and the write-combining mapping claims
//! page tables: neither exists at the moment the kernel displays its
//! first lines. Waiting for the allocator to light the screen would make
//! everything before it disappear -- banner, GDT loading, state of the
//! allocator itself. So the first step asks nothing of anybody: it goes
//! through the physical window installed by `boot.asm`, which already
//! covers the first four gibibytes, and draws straight on the hardware.
//!
//! The price is that the first step only works if the framebuffer fits
//! under four gibibytes. Beyond that, the screen only lights at the
//! second step, which maps it explicitly -- the boot lines then stay on
//! the serial console, which the output says.
//!
//! ## One lock only
//!
//! The surface and the console live under the same lock. Two nested locks
//! would raise a question of order on every call, for no real
//! parallelism: there is only one screen. The one lock taken inside it is
//! virtio-gpu's, by the present hook -- never the other way round.

pub mod console;
pub mod font;
pub mod framebuffer;
pub mod selftest;
pub mod virtio_gpu;

use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::frame;
use crate::memory::{self, flags};
use crate::multiboot::{BootInfo, FramebufferInfo};
use crate::vga::SpinLock;

use console::Console;
use framebuffer::{Color, Framebuffer};

/// Virtual address of the hardware framebuffer once remapped
/// write-combining. PML4[258], away from the physical window
/// (PML4[256]), the heap (PML4[257]) and the kernel (PML4[511]).
pub const FRONT_VMA: u64 = 0xFFFF_8100_0000_0000;

/// Virtual address of the back buffer. PML4[259].
pub const BACK_VMA: u64 = 0xFFFF_8180_0000_0000;

/// Limit of the physical window installed by `boot.asm`: four pages of
/// 1 GiB.
const PHYSMAP_LIMIT: u64 = 4 * 1024 * 1024 * 1024;

// ============================================================
//  Palette
// ============================================================

/// The sixteen colours of VGA text mode, in RGB.
///
/// Keeping the same palette is not nostalgia: the whole kernel already
/// chooses its colours in `vga::Color`, and a change of vocabulary would
/// force rewriting every call to `set_color` for a result identical on
/// screen.
pub mod palette {
    use super::framebuffer::{Color, rgb};

    pub const BLACK: Color = rgb(0x00, 0x00, 0x00);
    pub const BLUE: Color = rgb(0x00, 0x00, 0xAA);
    pub const GREEN: Color = rgb(0x00, 0xAA, 0x00);
    pub const CYAN: Color = rgb(0x00, 0xAA, 0xAA);
    pub const RED: Color = rgb(0xAA, 0x00, 0x00);
    pub const MAGENTA: Color = rgb(0xAA, 0x00, 0xAA);
    pub const BROWN: Color = rgb(0xAA, 0x55, 0x00);
    pub const LIGHT_GRAY: Color = rgb(0xAA, 0xAA, 0xAA);
    pub const DARK_GRAY: Color = rgb(0x55, 0x55, 0x55);
    pub const LIGHT_BLUE: Color = rgb(0x55, 0x55, 0xFF);
    pub const LIGHT_GREEN: Color = rgb(0x55, 0xFF, 0x55);
    pub const LIGHT_CYAN: Color = rgb(0x55, 0xFF, 0xFF);
    pub const LIGHT_RED: Color = rgb(0xFF, 0x55, 0x55);
    pub const PINK: Color = rgb(0xFF, 0x55, 0xFF);
    pub const YELLOW: Color = rgb(0xFF, 0xFF, 0x55);
    pub const WHITE: Color = rgb(0xFF, 0xFF, 0xFF);

    pub const DEFAULT_FOREGROUND: Color = LIGHT_GRAY;
    pub const DEFAULT_BACKGROUND: Color = BLACK;

    /// Translates a colour of the VGA writer.
    pub fn from_vga(color: crate::vga::Color) -> Color {
        use crate::vga::Color as V;
        match color {
            V::Black => BLACK,
            V::Blue => BLUE,
            V::Green => GREEN,
            V::Cyan => CYAN,
            V::Red => RED,
            V::Magenta => MAGENTA,
            V::Brown => BROWN,
            V::LightGray => LIGHT_GRAY,
            V::DarkGray => DARK_GRAY,
            V::LightBlue => LIGHT_BLUE,
            V::LightGreen => LIGHT_GREEN,
            V::LightCyan => LIGHT_CYAN,
            V::LightRed => LIGHT_RED,
            V::Pink => PINK,
            V::Yellow => YELLOW,
            V::White => WHITE,
        }
    }
}

// ============================================================
//  Global state
// ============================================================

struct Display {
    fb: Framebuffer,
    console: Console,
}

impl fmt::Write for Display {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.console.write_str(&mut self.fb, text);
        Ok(())
    }
}

static DISPLAY: SpinLock<Option<Display>> = SpinLock::new(None);

/// Flag consulted by `vga::_print` on every line. An atomic rather than
/// the lock: the question "paint or write at 0xb8000?" arises before one
/// even has the right to touch the screen.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Is the framebuffer on screen still the one GRUB programmed?
static BOOT_FRAMEBUFFER: AtomicBool = AtomicBool::new(true);

/// Mode kept, retained for diagnostics even when the latch fails.
static MODE: SpinLock<Option<FramebufferInfo>> = SpinLock::new(None);

/// Reason why the graphics display is not active.
static REASON: SpinLock<&'static str> = SpinLock::new("non initialise");

/// Does the kernel console still own the screen?
///
/// 8.1 reserves writing on the framebuffer to the compositor. As soon as
/// it starts, the kernel console must fall silent: two writers on the
/// same surface do not produce a shared screen, they produce flicker and
/// half-repainted areas.
///
/// That is not a loss of diagnostics -- `kprintln!` keeps writing on the
/// serial console, which receives everything. It is the screen, and it
/// alone, that changes owner.
static CONSOLE_OWNS_SCREEN: AtomicBool = AtomicBool::new(true);

/// The compositor takes the screen.
pub fn release_console() {
    CONSOLE_OWNS_SCREEN.store(false, Ordering::Release);
}

/// The kernel takes the screen back.
///
/// Called by the panic handler: a kernel that stops must be able to say
/// so on screen, even if a compositor is running. It is the only
/// exception to 8.1, and it justifies itself -- at that stage there is no
/// compositor left to spare.
pub fn reclaim_console() {
    CONSOLE_OWNS_SCREEN.store(true, Ordering::Release);
    // The GPU core's output may be on screen (phase 13.8): the console's
    // framebuffer goes back on it, or nothing written below would show.
    virtio_gpu::scanout::reclaim();
    clear_screen();
}

pub fn console_owns_screen() -> bool {
    CONSOLE_OWNS_SCREEN.load(Ordering::Relaxed)
}

pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Mode described by the bootloader, usable or not.
///
/// Tells "GRUB programmed nothing" from "GRUB programmed a mode the
/// driver could not latch": those are not the same diagnoses, and the
/// second step still has something to try in the second case.
pub fn mode() -> Option<FramebufferInfo> {
    *MODE.lock()
}

/// Resolution in pixels, if the graphics mode is active.
pub fn resolution() -> Option<(usize, usize)> {
    let display = DISPLAY.lock();
    display.as_ref().map(|d| (d.fb.width(), d.fb.height()))
}

/// Dimensions of the text console, in cells.
pub fn console_size() -> Option<(usize, usize)> {
    let display = DISPLAY.lock();
    display.as_ref().map(|d| (d.console.columns(), d.console.rows()))
}

pub fn is_double_buffered() -> bool {
    let display = DISPLAY.lock();
    display.as_ref().map(|d| d.fb.is_double_buffered()).unwrap_or(false)
}

// ============================================================
//  7.1.1 -- Latching the graphics mode
// ============================================================

/// First step: latches the framebuffer through the physical window and
/// switches the display.
///
/// Asks for neither frame nor page table, and can therefore be called
/// before the allocator. Being able to do nothing is not an error: the
/// VGA writer stays in place and the boot carries on.
///
/// # Safety
/// To be called once only, on the boot core, with the physical window
/// active.
pub unsafe fn init(boot: &BootInfo) {
    let Some(info) = boot.framebuffer else {
        *REASON.lock() = "GRUB n'a fourni aucun tag framebuffer";
        return;
    };

    *MODE.lock() = Some(info);

    if !info.is_graphic() {
        *REASON.lock() = "mode non graphique - le writer VGA reste en place";
        return;
    }

    // The physical window of boot.asm covers only four gibibytes.
    if info.addr.saturating_add(info.bytes()) > PHYSMAP_LIMIT {
        *REASON.lock() = "framebuffer hors de la fenetre physique - attend l'etape 2";
        return;
    }

    match unsafe { Framebuffer::new(&info, memory::phys_to_virt(info.addr)) } {
        Ok(fb) => activate(fb),
        Err(reason) => *REASON.lock() = reason,
    }
}

/// Installs a surface and lights the console.
fn activate(fb: Framebuffer) {
    let console = Console::new(&fb);
    let mut display = DISPLAY.lock();
    let display = display.insert(Display { fb, console });
    display.console.clear(&mut display.fb);
    display.console.flush(&mut display.fb);
    ACTIVE.store(true, Ordering::Release);
    *REASON.lock() = "actif";
}

// ============================================================
//  Replacing the framebuffer (virtio-gpu)
// ============================================================

/// Puts `fb` on screen in place of the current framebuffer.
///
/// Returns `true` when the one replaced was GRUB's, whose memory the
/// caller then gives back with `retire_boot_framebuffer` -- once the new
/// image is on screen, not before.
///
/// The console is rebuilt at the new size and repainted only if it still
/// owns the screen: once the compositor runs, it repaints everything
/// itself on the resize, and a console clearing the screen under it would
/// flash black for a frame.
pub(crate) fn install_framebuffer(fb: Framebuffer) -> bool {
    let console = Console::new(&fb);
    let mut display = DISPLAY.lock();
    let replaced = display.is_some();
    let display = display.insert(Display { fb, console });
    if console_owns_screen() {
        display.console.clear(&mut display.fb);
        display.console.flush(&mut display.fb);
    }
    ACTIVE.store(true, Ordering::Release);
    *REASON.lock() = "actif";
    replaced && BOOT_FRAMEBUFFER.swap(false, Ordering::AcqRel)
}

/// Gives back what the GRUB framebuffer held: the frames of its back
/// buffer, and the write-combining mapping of the hardware (the VRAM
/// itself is the card's, and stays).
///
/// Nothing names those ranges any more: `install_framebuffer` swapped
/// the framebuffer out under the screen lock.
pub(crate) fn retire_boot_framebuffer() {
    let Some(info) = *MODE.lock() else {
        return;
    };
    let pages = info.bytes().div_ceil(frame::FRAME_SIZE);
    unsafe { release_back_buffer(pages) };
    for page in 0..pages {
        let virt = FRONT_VMA + page * frame::FRAME_SIZE;
        if unsafe { memory::translate(virt) }.is_some() {
            unsafe { memory::unmap_page(virt) };
        }
    }
    crate::serial_println!(
        "[gfx] framebuffer GRUB rendu - {} pages de tampon arriere liberees",
        pages
    );
}

// ============================================================
//  7.1.2 -- Write-combining and back buffer
// ============================================================

/// Second step: remaps the framebuffer write-combining and gives it a
/// back buffer.
///
/// Each of the two halves can fail without preventing the other, and
/// neither is indispensable to the display -- which is why the return is
/// a description, not an error.
///
/// # Safety
/// To be called once only, after `frame::init`, on the boot core.
pub unsafe fn enable_double_buffer() -> &'static str {
    let Some(info) = *MODE.lock() else {
        return "aucun mode graphique";
    };
    if !info.is_graphic() {
        return "mode non graphique";
    }

    let bytes = info.bytes();
    let pages = bytes.div_ceil(frame::FRAME_SIZE);

    // --- write-combining on the hardware ---
    let combining = unsafe { map_front_write_combining(&info, pages) };

    // The framebuffer may not have been latched yet: that is the case
    // when it lives beyond the physical window. The explicit mapping just
    // laid down is precisely what lifts that limit.
    if !is_active() && combining {
        match unsafe { Framebuffer::new(&info, FRONT_VMA) } {
            Ok(fb) => activate(fb),
            Err(reason) => {
                *REASON.lock() = reason;
                return reason;
            }
        }
    } else if combining {
        let mut display = DISPLAY.lock();
        if let Some(display) = display.as_mut() {
            // The old view stays valid: both addresses name the same
            // physical memory. Switching therefore loses no pixel.
            unsafe { display.fb.rebind_front(FRONT_VMA) };
        }
    }

    if !is_active() {
        return "framebuffer inaccessible";
    }

    // --- back buffer ---
    let doubled = unsafe { attach_back_buffer(pages) };

    match (combining, doubled) {
        (true, true) => "write-combining et double tampon",
        (true, false) => "write-combining, sans double tampon",
        (false, true) => "double tampon, cache write-back",
        (false, false) => "acces direct, cache write-back",
    }
}

/// Maps the hardware framebuffer at `FRONT_VMA` with the write-combining
/// cache type.
///
/// The mapping is done in 4 KiB pages and not 2 MiB ones: nothing
/// guarantees the physical address the firmware returns is aligned on two
/// mebibytes, and a misaligned huge page does not map at all. Seven
/// hundred and sixty-eight entries for a 1024x768x32 mode, laid down once
/// at boot, weigh nothing.
///
/// # Safety
/// The virtual range `FRONT_VMA` must be free.
unsafe fn map_front_write_combining(info: &FramebufferInfo, pages: u64) -> bool {
    if !unsafe { memory::init_pat() } {
        return false;
    }

    // NO_EXECUTE: nothing executes from a framebuffer. Bit 63 is only
    // interpretable once EFER.NXE is armed, hence the test.
    let mut page_flags = flags::PRESENT | flags::WRITABLE | memory::PAT_4K;
    if memory::nx_enabled() {
        page_flags |= flags::NO_EXECUTE;
    }

    let base = info.addr & !(frame::FRAME_SIZE - 1);
    for page in 0..pages {
        let offset = page * frame::FRAME_SIZE;
        if unsafe { memory::map_page(FRONT_VMA + offset, base + offset, page_flags) }.is_err() {
            return false;
        }
    }
    true
}

/// Allocates and maps the back buffer, then installs it.
///
/// The frames need not be physically contiguous: that is the whole point
/// of going through paging rather than reserving a range in the heap,
/// which caps at four mebibytes anyway -- less than one 1280x1024 image.
///
/// # Safety
/// The virtual range `BACK_VMA` must be free.
unsafe fn attach_back_buffer(pages: u64) -> bool {
    let mut page_flags = flags::PRESENT | flags::WRITABLE;
    if memory::nx_enabled() {
        page_flags |= flags::NO_EXECUTE;
    }

    for page in 0..pages {
        let Some(physical) = frame::allocate_frame() else {
            // Give back the frames already taken: a half-mapped buffer is
            // worse than no buffer, and letting the memory of a boot
            // failure leak is paid for on every restart.
            unsafe { release_back_buffer(page) };
            return false;
        };
        let virt = BACK_VMA + page * frame::FRAME_SIZE;
        if unsafe { memory::map_page(virt, physical.start_addr(), page_flags) }.is_err() {
            unsafe { frame::deallocate_frame(physical) };
            unsafe { release_back_buffer(page) };
            return false;
        }
    }

    let mut display = DISPLAY.lock();
    match display.as_mut() {
        Some(display) => {
            unsafe { display.fb.attach_back_buffer(BACK_VMA) };
            true
        }
        None => false,
    }
}

/// Undoes the first `pages` pages of the back buffer.
///
/// # Safety
/// None of those pages must be in use.
unsafe fn release_back_buffer(pages: u64) {
    for page in 0..pages {
        let virt = BACK_VMA + page * frame::FRAME_SIZE;
        if let Some(translation) = unsafe { memory::translate(virt) } {
            unsafe { memory::unmap_page(virt) };
            unsafe { frame::deallocate_frame(frame::Frame(translation.phys & !0xFFF)) };
        }
    }
}

// ============================================================
//  Text output
// ============================================================

/// Entry point of `print!` when the graphics mode is active.
pub fn print(args: fmt::Arguments) {
    if !console_owns_screen() {
        return;
    }
    let mut display = DISPLAY.lock();
    if let Some(display) = display.as_mut() {
        let _ = fmt::Write::write_fmt(display, args);
        display.console.flush(&mut display.fb);
    }
}

pub fn clear_screen() {
    let mut display = DISPLAY.lock();
    if let Some(display) = display.as_mut() {
        display.console.clear(&mut display.fb);
        display.console.flush(&mut display.fb);
    }
}

/// The colour is remembered even when the console no longer paints: it
/// will apply as-is if the kernel takes the screen back.
pub fn set_color(foreground: crate::vga::Color, background: crate::vga::Color) {
    let mut display = DISPLAY.lock();
    if let Some(display) = display.as_mut() {
        display
            .console
            .set_color(palette::from_vga(foreground), palette::from_vga(background));
    }
}

// ============================================================
//  Exposed drawing primitives
// ============================================================

/// Gives exclusive access to the surface, for the duration of a closure.
///
/// The only way to draw from outside the module: the lock is taken and
/// released by the function, which makes it impossible to keep the
/// surface between two calls -- and therefore to switch task while
/// holding it, which `docs/DECISIONS.md` forbids.
pub fn with_surface<R>(action: impl FnOnce(&mut Framebuffer) -> R) -> Option<R> {
    let mut display = DISPLAY.lock();
    display.as_mut().map(|display| action(&mut display.fb))
}

/// Publishes the complete image.
pub fn present() {
    let mut display = DISPLAY.lock();
    if let Some(display) = display.as_mut() {
        display.fb.present();
    }
}

/// Paints a filled rectangle then publishes it.
pub fn fill_rect(x: usize, y: usize, width: usize, height: usize, color: Color) {
    let mut display = DISPLAY.lock();
    if let Some(display) = display.as_mut() {
        display.fb.fill_rect(x, y, width, height, color);
        display.fb.present_rows(y, height);
    }
}

// ============================================================
//  Diagnostics
// ============================================================

pub fn dump_state() {
    let mode = *MODE.lock();
    let reason = *REASON.lock();

    match mode {
        Some(info) => {
            crate::serial_println!(
                "[gfx] mode {}x{} en {} bits ({}) - pas {} o, {} Kio a 0x{:x}",
                info.width,
                info.height,
                info.bpp,
                info.kind_name(),
                info.pitch,
                info.bytes() / 1024,
                info.addr
            );
            if info.is_graphic() {
                crate::serial_println!(
                    "[gfx] composantes : R {}+{}  V {}+{}  B {}+{}",
                    info.red_pos,
                    info.red_size,
                    info.green_pos,
                    info.green_size,
                    info.blue_pos,
                    info.blue_size
                );
            }
        }
        None => crate::serial_println!("[gfx] aucun tag framebuffer - mode texte VGA"),
    }

    if is_active() {
        let (columns, rows) = console_size().unwrap_or((0, 0));
        crate::serial_println!(
            "[gfx] console {}x{} cellules de {}x{} - double tampon : {}",
            columns,
            rows,
            font::GLYPH_WIDTH,
            font::GLYPH_HEIGHT,
            if is_double_buffered() { "oui" } else { "non" }
        );
        crate::serial_println!(
            "[gfx] PAT 0x{:016x} (defaut 0x{:016x}) - write-combining : {}",
            memory::read_pat(),
            memory::pat_default(),
            if memory::pat_wc_available() { "oui" } else { "non" }
        );
    } else {
        crate::serial_println!("[gfx] affichage graphique inactif : {}", reason);
    }
    virtio_gpu::dump_state();
}

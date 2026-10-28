//! The screen and the cursor, owned by the GPU core (spec 8.5, 13.4.4).
//!
//! ## Who may show what
//!
//! One client puts images on the screen: the compositor, a kernel task
//! (spec 8.1, "seul maitre de l'ecran"). It hands the core **a memory of
//! one of its contexts**, mapped, at least as large as the image; the
//! core checks the handle is the caller's and turns it into what the
//! device shows. A program -- an `Owner::Process` -- is refused: its way
//! to the screen is a window, and `SYS_GPU_PRESENT`.
//!
//! ## Why a memory and not an image
//!
//! On this device the screen is fed through a 2D resource whose backing
//! is the memory (`virtio_gpu::scanout`): the compositor renders into an
//! image, copies the damaged rectangle into this memory on the GPU, and
//! calls `flush`. A backend that one day scans GPU images out directly
//! would take the image instead; the interface of `show` would change, not
//! its callers' logic.
//!
//! The cursor plane is here for the same reason: the core owns it, the
//! compositor asks.

use super::*;
use crate::graphics::virtio_gpu::scanout;

/// Puts memory `handle` of `context` on the screen, as a `width` x
/// `height` image, 4 bytes a pixel, rows packed.
pub fn show(owner: &Owner, context: u32, handle: u32, width: u32, height: u32) -> Result<()> {
    if !matches!(owner, Owner::Task(_)) {
        return Err(Error::Invalid("l'ecran est au compositeur"));
    }
    let window = contexts::mem_window(owner, context, handle)?;
    let bytes = width as u64 * height as u64 * 4;
    if bytes == 0 || bytes > window.size {
        return Err(Error::Invalid("memoire plus petite que l'image"));
    }
    scanout::show(window.phys, width, height).map(|_| ()).map_err(Error::Invalid)
}

/// Sends a rectangle of the shown memory to the screen.
pub fn flush(x: u32, y: u32, width: u32, height: u32) {
    scanout::flush(x, y, width, height);
}

/// Gives the screen back to the console's framebuffer. The memory shown
/// may be freed afterwards, not before.
pub fn release() {
    scanout::release();
}

/// Is an image of the core on the screen?
pub fn shown() -> bool {
    scanout::gpu_output().is_some()
}

/// Does the device have a cursor plane?
pub fn cursor_plane() -> bool {
    scanout::cursor_available()
}

/// Shows a 64 x 64 image (`0xAARRGGBB`) as the cursor, its hot spot at
/// `(x, y)`.
pub fn cursor_image(pixels: &[u32], hot: (u32, u32), x: i32, y: i32) -> bool {
    scanout::cursor_image(pixels, hot, x, y)
}

/// Does the plane show the pointer now -- the host having acknowledged
/// its pixels, not only the compositor having asked?
pub fn cursor_shown() -> bool {
    scanout::cursor_shown()
}

/// Shows the image `cursor_image` staged, once the host has its pixels.
/// Called every frame; never waits.
pub fn cursor_poll() {
    scanout::cursor_poll();
}

pub fn cursor_move(x: i32, y: i32) {
    scanout::cursor_move(x, y);
}

pub fn cursor_hide() {
    scanout::cursor_hide();
}

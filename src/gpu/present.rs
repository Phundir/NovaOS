//! `SYS_GPU_PRESENT`: an image of a program, shown in one of its windows.
//!
//! ## The path, and why this one
//!
//! A window of a program of the Nova C library is a `/dev/nova-gui`
//! descriptor: pixels in shared memory that the window's bridge copies to
//! the compositor when told they changed. `PRESENT(ctx, image, fd, fence)`
//! feeds that same path from the GPU: once `fence` (if any) has signalled,
//! the image is copied by the GPU into a read-back area of the program's
//! context, then row by row into the window's pixels, and the window is
//! presented.
//!
//! The screen itself stays where it is: on this host the direct output of
//! GPU memory to the screen (a dma-buf blob) shows black (DECISIONS, "La
//! sortie d'affichage directe par dma-buf est retirée"), so the compositor
//! keeps composing in software, and a GPU image reaches it as a window's
//! pixels do. One GPU copy and one processor copy per present: the price
//! of the one path that shows the right pixels.
//!
//! ## The read-back area
//!
//! Made at the first present of a context and kept -- memory, a buffer, a
//! fence --, remade when the window grows. Its handles are ordinary ones
//! of the program's context, charged to its quota; a program that destroys
//! them (it would have to guess them) only makes the next present rebuild
//! them.

use alloc::vec::Vec;

use super::backend::{Format, MemKind};
use super::{Create, Error, Owner, Result};

/// The read-back area of a context.
pub struct Staging {
    memory: u32,
    buffer: u32,
    fence: u32,
    bytes: u64,
    address: u64,
}

/// Longest wait for the copy.
const COPY_TIMEOUT_MS: u64 = 2000;

fn build(owner: &Owner, context: u32, bytes: u64) -> Result<Staging> {
    let memory = super::mem_alloc(owner, context, bytes, MemKind::Readback)?;
    let made = (|| -> Result<Staging> {
        let address = super::mem_map(owner, context, memory)?;
        let buffer = super::object_create(owner, context, Create::Buffer { size: bytes, usage: 0, memory, offset: 0 })?;
        let fence = super::fence_create(owner, context)?;
        Ok(Staging { memory, buffer, fence, bytes, address })
    })();
    if made.is_err() {
        let _ = super::mem_free(owner, context, memory);
    }
    made
}

fn release(owner: &Owner, context: u32, staging: Staging) {
    let _ = super::object_destroy(owner, context, staging.buffer);
    let _ = super::object_destroy(owner, context, staging.fence);
    let _ = super::mem_free(owner, context, staging.memory);
}

pub fn present(owner: &Owner, context: u32, image: u32, surface: u64, fence: u64) -> Result<()> {
    let window = crate::posix::gui::window_of_descriptor(surface as i64).ok_or(Error::BadHandle)?;
    let (window_w, window_h) = window.size();
    let shared = super::context_of(owner, context)?;

    // --- the image ---
    let (format, image_w, image_h, staging) = {
        let mut ctx = shared.state.lock();
        let object = ctx.objects.get(&image).ok_or(Error::BadHandle)?;
        let super::cmd::Info::Image { format, width, height, .. } = object.info else {
            return Err(Error::BadHandle);
        };
        (format, width, height, ctx.present.take())
    };
    let swap = match format {
        Format::Bgra8 | Format::Bgra8Srgb => false,
        Format::Rgba8 | Format::Rgba8Srgb => true,
        _ => return Err(Error::Unsupported),
    };
    let width = image_w.min(window_w);
    let height = image_h.min(window_h);
    let bytes = width as u64 * height as u64 * 4;

    // --- the read-back area, kept or remade ---
    let staging = match staging {
        Some(s) if s.bytes >= bytes => s,
        Some(s) => {
            release(owner, context, s);
            build(owner, context, bytes.max(window_w as u64 * window_h as u64 * 4))?
        }
        None => build(owner, context, bytes.max(window_w as u64 * window_h as u64 * 4))?,
    };

    // --- the GPU copy, after the program's fence ---
    let words: [u32; 10] = [
        super::cmd::op::COPY_IMAGE_TO_BUFFER | (10 << 16),
        image,
        0,
        0,
        width,
        height,
        staging.buffer,
        0,
        0,
        width,
    ];
    let waits: Vec<u32> = if fence != 0 && fence <= u32::MAX as u64 { alloc::vec![fence as u32] } else { Vec::new() };
    let copied = super::submit(owner, context, &words, staging.fence, &waits)
        .and_then(|()| super::fence_view(owner, staging.fence))
        .and_then(|view| super::fence_wait(core::slice::from_ref(&view), true, Some(COPY_TIMEOUT_MS)));
    if let Err(error) = copied {
        shared.state.lock().present = Some(staging);
        return Err(error);
    }

    // --- into the window, row by row ---
    let row_bytes = width as usize * 4;
    let mut row = alloc::vec![0u8; row_bytes];
    let mut result = Ok(());
    for y in 0..height {
        // Through `copy_from_user`: the area is a region of the caller's
        // space, whose pages come on first touch -- a plain read from the
        // kernel would fault on one the program never touched.
        if crate::vm::copy_from_user(&mut row, staging.address + y as u64 * row_bytes as u64).is_err() {
            result = Err(Error::Invalid("zone de relecture inaccessible"));
            break;
        }
        if swap {
            for pixel in row.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
        }
        if window.write_row(y, &row).is_err() {
            result = Err(Error::Invalid("fenetre redimensionnee pendant la presentation"));
            break;
        }
    }
    shared.state.lock().present = Some(staging);
    if result.is_ok() {
        window.present_all();
    }
    result
}

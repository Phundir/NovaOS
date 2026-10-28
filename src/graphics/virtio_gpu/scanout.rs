//! What scanout 0 shows once the GPU core owns the screen, and the cursor
//! plane (phase 13.4.4).
//!
//! ## An output in GPU memory, shown through a 2D resource
//!
//! The direct way to show GPU memory -- `SET_SCANOUT_BLOB` on a Venus
//! blob -- is accepted by QEMU and shows **black** on an NVIDIA host (the
//! dma-buf is imported without its format modifier; DECISIONS, "La sortie
//! d'affichage directe par dma-buf est retirée"). What works everywhere is
//! the 2D path, fed differently: a 2D resource whose **backing** is not
//! guest RAM but a range of the shared region -- a blob the GPU core
//! mapped there. The GPU writes the image into that memory; a
//! `TRANSFER_TO_HOST_2D` has the host copy it into the resource; a
//! `RESOURCE_FLUSH` shows it. No guest processor touches a pixel.
//!
//! The console's framebuffer (`current`) lives on underneath: it is what
//! the panic handler shows again (`reclaim`), and what the screen falls
//! back to when the GPU core gives the screen up (`release`).
//!
//! ## Never waiting while the host's GPU is taken
//!
//! QEMU draws its 2D resources with OpenGL, on the host's GPU. While a
//! program holds that GPU with a long draw -- until the watchdog cuts it,
//! two seconds -- QEMU answers nothing that touches a 2D resource. A
//! command waited for then would spin past its bound and declare the
//! device dead (13.11.3 found it). So once the screen runs, nothing here
//! waits: `flush`, `release` and the cursor's commands are posted, and
//! the cursor shows a new image only once its transfer has been answered
//! (`cursor_poll`), on a second resource meanwhile. Only `show`, at start
//! and on a resize, waits.
//!
//! ## The cursor plane
//!
//! The host composes a 64 x 64 image over the scanout wherever it is
//! told to, on the cursor queue. Moving the pointer is then one 56-byte
//! command, with nothing repainted: the pointer's old and new places no
//! longer dirty the screen. The image is a small 2D resource in guest
//! frames, with an alpha byte around the shape -- two of them, one shown
//! while the other receives the next shape.

use alloc::vec::Vec;

use super::proto::{self, Area};
use super::queue::{Exchange, Ticket};
use super::{Gpu, Request, GPU};

/// The GPU core's image on scanout 0.
#[derive(Debug, Clone, Copy)]
pub struct Output {
    pub resource: u32,
    pub width: u32,
    pub height: u32,
}

/// One cursor image: a 2D resource and its four frames.
struct CursorImage {
    resource: u32,
    /// 64 x 64 x 4 bytes.
    frames: Vec<u64>,
}

/// The cursor plane.
pub struct Cursor {
    images: [CursorImage; 2],
    /// Which image the plane shows, if any.
    shown: Option<usize>,
    /// A new image on its way: its transfer, its hot spot and position.
    staged: Option<(usize, Ticket, (u32, u32), (i32, i32))>,
}

const CURSOR_BYTES: u64 = (proto::CURSOR_SIDE * proto::CURSOR_SIDE * 4) as u64;

/// Posts commands whose answers nobody reads.
fn post(gpu: &mut Gpu, requests: &[&[u8]]) -> bool {
    let exchanges: Vec<Exchange> =
        requests.iter().map(|request| Exchange { request: core::slice::from_ref(request), response: proto::HEADER_SIZE }).collect();
    gpu.queue.post(&exchanges)
}

/// Detaches and destroys a resource nothing shows any more. Posted.
fn retire(gpu: &mut Gpu, resource: u32) {
    let detach = proto::detach_backing(resource);
    let unref = proto::resource_unref(resource);
    post(gpu, &[&detach, &unref]);
}

/// Puts on scanout 0 the image in `[phys, phys + width * height * 4)`,
/// a range of the shared region the caller keeps mapped until `release`
/// or the next `show`. Replaces the previous output, if any. Waits for
/// the host with the lock released (`super::transact`).
pub fn show(phys: u64, width: u32, height: u32) -> Result<Output, &'static str> {
    let bytes = width as u64 * height as u64 * 4;
    if bytes == 0 || bytes > u32::MAX as u64 {
        return Err("taille d'image invalide");
    }
    let whole = Area::new(0, 0, width, height);
    let (answers, resource) = super::transact(|gpu| {
        let resource = gpu.next_resource;
        gpu.next_resource = gpu.next_resource.wrapping_add(1).max(1);
        let requests = alloc::vec![
            Request::nodata(alloc::vec![proto::resource_create_2d(resource, width, height)]),
            Request::nodata(alloc::vec![proto::attach_backing(resource, 1), proto::backing_entries(&[(phys, bytes as u32)])]),
            Request::nodata(alloc::vec![proto::set_scanout(0, resource, whole)]),
            Request::nodata(alloc::vec![proto::transfer_to_host_2d(resource, whole, 0)]),
            Request::nodata(alloc::vec![proto::resource_flush(resource, whole)]),
        ];
        Ok((requests, resource))
    })?;
    let mut guard = GPU.lock();
    let gpu = guard.as_mut().ok_or("pas de virtio-gpu")?;
    if !super::all_ok(&answers) {
        retire(gpu, resource);
        return Err("ressource d'affichage refusee");
    }
    let output = Output { resource, width, height };
    if let Some(old) = gpu.gpu_output.replace(output) {
        retire(gpu, old.resource);
    }
    Ok(output)
}

/// Sends a rectangle of the output to the screen. Posted.
pub fn flush(x: u32, y: u32, width: u32, height: u32) {
    let mut guard = GPU.lock();
    let Some(gpu) = guard.as_mut() else {
        return;
    };
    let Some(output) = gpu.gpu_output else {
        return;
    };
    let x = x.min(output.width);
    let y = y.min(output.height);
    let width = width.min(output.width - x);
    let height = height.min(output.height - y);
    if width == 0 || height == 0 || gpu.broken {
        return;
    }
    let area = Area::new(x, y, width, height);
    let offset = (y as u64 * output.width as u64 + x as u64) * 4;
    let transfer = proto::transfer_to_host_2d(output.resource, area, offset);
    let show = proto::resource_flush(output.resource, area);
    if post(gpu, &[&transfer, &show]) {
        super::FLUSHES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    }
}

/// Gives the screen back to the console's framebuffer, whole, and
/// destroys the output's resource. Posted: the host runs the commands in
/// order, before anything sent later -- the caller may free the output's
/// memory once `settled` says the queue went past them.
pub fn release() {
    let mut guard = GPU.lock();
    let Some(gpu) = guard.as_mut() else {
        return;
    };
    let Some(output) = gpu.gpu_output.take() else {
        return;
    };
    if let Some(current) = gpu.current.as_ref() {
        let (resource, width, height) = (current.resource, current.width, current.height);
        let whole = Area::new(0, 0, width, height);
        let point = proto::set_scanout(0, resource, whole);
        let transfer = proto::transfer_to_host_2d(resource, whole, 0);
        let flush = proto::resource_flush(resource, whole);
        post(gpu, &[&point, &transfer, &flush]);
    }
    retire(gpu, output.resource);
}

/// The panic path: the console's framebuffer back on screen, if the lock
/// is free. The output's resource is left as it is -- nothing will free
/// its memory any more.
pub fn reclaim() {
    let Some(mut guard) = GPU.try_lock() else {
        return;
    };
    let Some(gpu) = guard.as_mut() else {
        return;
    };
    if gpu.gpu_output.take().is_none() {
        return;
    }
    if let Some(current) = gpu.current.as_ref() {
        let whole = Area::new(0, 0, current.width, current.height);
        let point = proto::set_scanout(0, current.resource, whole);
        post(gpu, &[&point]);
    }
}

/// Is the GPU core's output on screen?
pub fn gpu_output() -> Option<Output> {
    GPU.lock().as_ref().and_then(|gpu| gpu.gpu_output)
}

// ============================================================
//  Cursor plane
// ============================================================

/// Does the device have a cursor plane?
pub fn cursor_available() -> bool {
    GPU.lock().as_ref().is_some_and(|gpu| gpu.cursor_queue.is_some() && !gpu.broken)
}

/// One cursor image's resource: four guest frames as its backing. Waits
/// for the host with the lock released.
fn cursor_resource() -> Option<CursorImage> {
    let pages = CURSOR_BYTES.div_ceil(4096);
    let mut frames = Vec::with_capacity(pages as usize);
    let give_back = |frames: Vec<u64>| {
        for frame in frames {
            // SAFETY: allocated here, never handed to the device.
            unsafe { crate::frame::deallocate_frame(crate::frame::Frame(frame)) };
        }
    };
    for _ in 0..pages {
        match crate::frame::allocate_zeroed_frame() {
            Some(frame) => frames.push(frame.start_addr()),
            None => {
                give_back(frames);
                return None;
            }
        }
    }
    let side = proto::CURSOR_SIDE;
    let runs = proto::coalesce(&frames, 4096);
    let made = super::transact(|gpu| {
        let resource = gpu.next_resource;
        gpu.next_resource = gpu.next_resource.wrapping_add(1).max(1);
        let requests = alloc::vec![
            Request::nodata(alloc::vec![proto::resource_create_2d_format(resource, proto::FORMAT_B8G8R8A8, side, side)]),
            Request::nodata(alloc::vec![proto::attach_backing(resource, runs.len() as u32), proto::backing_entries(&runs)]),
        ];
        Ok((requests, resource))
    });
    match made {
        Ok((answers, resource)) if super::all_ok(&answers) => Some(CursorImage { resource, frames }),
        _ => {
            give_back(frames);
            None
        }
    }
}

fn post_cursor(gpu: &mut Gpu, request: &[u8]) -> bool {
    let Some(queue) = gpu.cursor_queue.as_mut() else {
        return false;
    };
    queue.post(&[Exchange { request: &[request], response: proto::HEADER_SIZE }])
}

/// Stages `pixels` -- 64 x 64, `0xAARRGGBB`, top row first -- as the next
/// cursor, its hot spot at `(x, y)` on the screen. Shown by `cursor_poll`
/// once the host has the pixels. `false` if it cannot be staged now (a
/// previous image still on its way): the caller tries again.
pub fn cursor_image(pixels: &[u32], hot: (u32, u32), x: i32, y: i32) -> bool {
    let side = proto::CURSOR_SIDE as usize;
    if pixels.len() != side * side {
        return false;
    }
    let made = GPU.lock().as_ref().is_some_and(|gpu| gpu.cursor.is_some());
    if !made {
        // Made outside the lock, which the waits must not hold.
        let (Some(first), Some(second)) = (cursor_resource(), cursor_resource()) else {
            return false;
        };
        if let Some(gpu) = GPU.lock().as_mut()
            && gpu.cursor.is_none()
        {
            gpu.cursor = Some(Cursor { images: [first, second], shown: None, staged: None });
        }
    }
    let mut guard = GPU.lock();
    let Some(gpu) = guard.as_mut() else {
        return false;
    };
    if gpu.broken || gpu.cursor_queue.is_none() {
        return false;
    }
    let Some(cursor) = gpu.cursor.as_ref() else {
        return false;
    };
    if cursor.staged.is_some() {
        return false;
    }
    // The image the plane does not show receives the pixels.
    let target = match cursor.shown {
        Some(0) => 1,
        _ => 0,
    };
    let image = &cursor.images[target];
    for (index, pixel) in pixels.iter().enumerate() {
        let byte = index as u64 * 4;
        let frame = image.frames[(byte / 4096) as usize];
        // SAFETY: the frame is this image's, owned by the driver; the
        // host reads it only during the transfer below, and the plane
        // shows the other image meanwhile.
        unsafe { ((crate::memory::phys_to_virt(frame) + byte % 4096) as *mut u32).write_volatile(*pixel) };
    }
    let resource = image.resource;
    let whole = Area::new(0, 0, proto::CURSOR_SIDE, proto::CURSOR_SIDE);
    let transfer = proto::transfer_to_host_2d(resource, whole, 0);
    let tickets = gpu.queue.submit(&[Exchange { request: &[&transfer], response: proto::HEADER_SIZE }]);
    let Some(ticket) = tickets.into_iter().next().flatten() else {
        return false;
    };
    if let Some(cursor) = gpu.cursor.as_mut() {
        cursor.staged = Some((target, ticket, hot, (x, y)));
    }
    true
}

/// Shows the staged image once its transfer is answered. Called every
/// frame; never waits.
pub fn cursor_poll() {
    let mut guard = GPU.lock();
    let Some(gpu) = guard.as_mut() else {
        return;
    };
    let Some((target, ticket, hot, (x, y))) = gpu.cursor.as_ref().and_then(|c| c.staged) else {
        return;
    };
    gpu.queue.reap();
    let Some(answer) = gpu.queue.take(ticket) else {
        return;
    };
    let accepted = proto::response_type(&answer) == Some(proto::RESP_OK_NODATA);
    let resource = gpu.cursor.as_ref().map(|c| c.images[target].resource).unwrap_or(0);
    let update = proto::update_cursor(resource, x.max(0) as u32, y.max(0) as u32, hot.0, hot.1);
    let shown = accepted && post_cursor(gpu, &update);
    if let Some(cursor) = gpu.cursor.as_mut() {
        cursor.staged = None;
        if shown {
            cursor.shown = Some(target);
        }
    }
}

/// Does the plane show an image?
pub fn cursor_shown() -> bool {
    GPU.lock().as_ref().and_then(|gpu| gpu.cursor.as_ref()).is_some_and(|c| c.shown.is_some())
}

/// Moves the cursor's hot spot to `(x, y)`.
pub fn cursor_move(x: i32, y: i32) {
    let mut guard = GPU.lock();
    let Some(gpu) = guard.as_mut() else {
        return;
    };
    if !gpu.cursor.as_ref().is_some_and(|cursor| cursor.shown.is_some()) {
        return;
    }
    let request = proto::move_cursor(x.max(0) as u32, y.max(0) as u32);
    post_cursor(gpu, &request);
}

/// Hides the cursor plane: the compositor draws the pointer again.
pub fn cursor_hide() {
    let mut guard = GPU.lock();
    let Some(gpu) = guard.as_mut() else {
        return;
    };
    if !gpu.cursor.as_ref().is_some_and(|cursor| cursor.shown.is_some()) {
        return;
    }
    let request = proto::update_cursor(0, 0, 0, 0, 0);
    if post_cursor(gpu, &request)
        && let Some(cursor) = gpu.cursor.as_mut()
    {
        cursor.shown = None;
    }
}

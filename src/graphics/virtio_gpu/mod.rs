//! virtio-gpu -- a screen whose size follows the host.
//!
//! ## Why a second display path
//!
//! The framebuffer of 7.1 is the one GRUB programs through VBE: a mode
//! chosen once, before the kernel runs, from a list of fixed sizes. Nothing
//! in VBE says how large the host window is, and nothing tells the guest
//! when it changes. virtio-gpu does both: `GET_DISPLAY_INFO` answers with
//! the size of the host window -- the whole monitor in full screen --, and
//! the device raises `EVENT_DISPLAY` in its configuration whenever that
//! size changes.
//!
//! The GRUB framebuffer stays the path of every machine without this
//! device: `-vga std`, real hardware, a firmware with no virtio. The driver
//! only takes over when it finds the device and has a first mode running.
//!
//! ## How an image reaches the host
//!
//! The pixels live in guest memory -- the "backing" of a 2D resource. The
//! host sees them only after `TRANSFER_TO_HOST_2D` copies a rectangle into
//! its own copy, and shows them only after `RESOURCE_FLUSH`. Both are sent
//! by `flush`, which the framebuffer calls at the end of each present, with
//! the rectangle just published. The back buffer of 7.1.2 is kept as it
//! is: the magnifier and the colour transforms of 16 need a source distinct
//! from the destination, and that is the back buffer.
//!
//! ## Changing the size
//!
//! A new size is a new resource, with new backing and a new back buffer,
//! in the **other** of two virtual slots. The switch happens in three
//! steps, and the order is what keeps the locks apart:
//!
//! 1. with no lock, allocate and map the frames; then, under the GPU lock,
//!    create the resource and attach them as its backing;
//! 2. under the screen lock, swap the framebuffers -- without the GPU lock,
//!    since a present inside the swap calls `flush`, which takes it;
//! 3. under the GPU lock again, point the scanout at the new resource,
//!    push its first image, and detach the old one.
//!
//! The order of the locks is therefore always screen, then GPU. Never the
//! reverse. And the frames are allocated and freed outside both: the GPU
//! lock masks interrupts, and mapping or unmapping a few thousand pages
//! is milliseconds of work.
//!
//! Then the old slot is unmapped and its frames freed. `unmap_page`
//! invalidates the TLB of every core, which matters here more than
//! anywhere: the slots alternate, so the next resize maps new frames at
//! the very addresses another core may have cached -- the defect entry
//! "La TLB d'un autre coeur" of DECISIONS.md records.
//!
//! ## The same device, in 3D (phase 13)
//!
//! When the host offers them, the driver also negotiates the 3D features
//! -- contexts, blob resources, context protocols -- and reads the Venus
//! capability set. The screen path above is unchanged by it; `context`
//! adds what the GPU core (`crate::gpu`) builds on: contexts speaking
//! Venus, host memory mapped through the shared region (`hostmem`), and
//! fenced submissions answered once the host's GPU is done. Both paths
//! share one control queue and one lock: the queue holds requests of
//! either kind in flight at once.

pub mod context;
mod exchange;
pub mod hostmem;
pub mod proto;
mod queue;
pub mod scanout;
pub mod selftest;
mod transport;

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::memory::{self, flags};
use crate::multiboot::{FB_RGB, FramebufferInfo};
use crate::vga::SpinLock;

use super::framebuffer::Framebuffer;
use proto::Area;
use exchange::{all_ok, transact, Request};
use queue::{Exchange, Queue};
use transport::Transport;

/// Distance between two virtual slots of a framebuffer: 64 GiB, far more
/// than any image, and small enough that four slots stay inside one PML4
/// entry.
pub const SLOT: u64 = 1 << 36;

const VENDOR_VIRTIO: u16 = 0x1AF4;
/// virtio 1.0 device 16, the GPU. `virtio-vga` and `virtio-gpu-pci` both
/// carry it; only their class differs.
const DEVICE_GPU: u16 = 0x1040 + 16;

/// Offsets in the device configuration (5.7.4).
const CONFIG_EVENTS_READ: u64 = 0;
const CONFIG_EVENTS_CLEAR: u64 = 4;

/// Sizes accepted. Below the minimum, the task bar and a window no longer
/// fit side by side; above the maximum, the protocol of 7.4 -- which
/// carries coordinates on 16 bits -- would still hold, but the two slots
/// would take more memory than the rest of the system.
pub const MIN_WIDTH: u32 = 640;
pub const MIN_HEIGHT: u32 = 480;
pub const MAX_WIDTH: u32 = 3840;
pub const MAX_HEIGHT: u32 = 2400;

/// Frames always left to the rest of the system by a resize.
const RESERVE_FRAMES: usize = 8192;

const PAGE: u64 = 4096;

struct Buffer {
    base: u64,
    frames: Vec<u64>,
}

impl Buffer {
    /// Allocates `pages` frames, maps them at `base` and zeroes them.
    ///
    /// # Safety
    /// The range `base..base + pages * PAGE` must be unmapped.
    unsafe fn allocate(base: u64, pages: u64) -> Option<Buffer> {
        let mut page_flags = flags::PRESENT | flags::WRITABLE;
        if memory::nx_enabled() {
            page_flags |= flags::NO_EXECUTE;
        }
        let mut buffer = Buffer {
            base,
            frames: Vec::with_capacity(pages as usize),
        };
        for page in 0..pages {
            let Some(frame) = crate::frame::allocate_frame() else {
                unsafe { buffer.release() };
                return None;
            };
            let physical = frame.start_addr();
            if unsafe { memory::map_page(base + page * PAGE, physical, page_flags) }.is_err() {
                unsafe { crate::frame::deallocate_frame(frame) };
                unsafe { buffer.release() };
                return None;
            }
            buffer.frames.push(physical);
        }
        unsafe { core::ptr::write_bytes(base as *mut u8, 0, (pages * PAGE) as usize) };
        Some(buffer)
    }

    /// Unmaps and frees every frame.
    ///
    /// # Safety
    /// Nothing may still read or write the range, the device included.
    unsafe fn release(&mut self) {
        for (page, frame) in self.frames.drain(..).enumerate() {
            unsafe { memory::unmap_page(self.base + page as u64 * PAGE) };
            unsafe { crate::frame::deallocate_frame(crate::frame::Frame(frame)) };
        }
    }
}

struct Scanout {
    resource: u32,
    width: u32,
    height: u32,
    /// 1 or 2: which virtual slot holds the buffers.
    slot: u64,
    front: Buffer,
    back: Buffer,
}

struct Gpu {
    transport: Transport,
    queue: Queue,
    /// The cursor queue, when the device has one (`scanout`).
    cursor_queue: Option<Queue>,
    /// What the GPU core put on scanout 0 in place of `current`
    /// (`scanout::show`): then `current` keeps being the console's
    /// framebuffer, but nothing presents it.
    gpu_output: Option<scanout::Output>,
    /// The cursor plane's image, once made.
    cursor: Option<scanout::Cursor>,
    current: Option<Scanout>,
    next_resource: u32,
    /// The 3D side (phase 13): present when the host granted the 3D
    /// features and offers the Venus protocol.
    three_d: Option<context::ThreeD>,
    /// Does the control queue interrupt (MSI-X), or is it polled?
    interrupts: bool,
    /// Size last announced by the host, and how many polls in a row it has
    /// been the same.
    wanted: (u32, u32),
    steady: u32,
    /// Last size handed to the compositor. A size that failed -- not
    /// enough memory -- is not handed again until the host asks for
    /// another: retrying it every tenth of a second would fill the log and
    /// change nothing.
    tried: (u32, u32),
    broken: bool,
}

static GPU: SpinLock<Option<Gpu>> = SpinLock::new(None);
/// The device-specific configuration's mapping (uncached), 0 before the
/// device is up: read without the lock by `poll_resize`.
static DEVICE_CONFIG: AtomicU64 = AtomicU64::new(0);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static FLUSHES: AtomicU64 = AtomicU64::new(0);
static RESIZES: AtomicU64 = AtomicU64::new(0);

/// Is the screen driven by virtio-gpu?
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Clamps a size the host asks for to what the driver accepts.
pub fn clamp_size(width: u32, height: u32) -> (u32, u32) {
    (
        width.clamp(MIN_WIDTH, MAX_WIDTH),
        height.clamp(MIN_HEIGHT, MAX_HEIGHT),
    )
}

impl Gpu {
    /// Runs exchanges; a device that stops answering is declared broken
    /// once, loudly, and never waited for again.
    fn run(&mut self, exchanges: &[Exchange]) -> Option<Vec<Vec<u8>>> {
        if self.broken {
            return None;
        }
        let answers = self.queue.run(exchanges);
        if answers.is_none() {
            self.broken = true;
            crate::serial_println!("[virtio-gpu] le peripherique ne repond plus - affichage fige");
        }
        answers
    }

    /// Runs commands whose only answer is `OK_NODATA`.
    fn command(&mut self, requests: &[&[&[u8]]]) -> bool {
        let exchanges: Vec<Exchange> = requests
            .iter()
            .map(|request| Exchange {
                request,
                response: proto::HEADER_SIZE,
            })
            .collect();
        self.run(&exchanges).is_some_and(|answers| {
            answers
                .iter()
                .all(|answer| proto::response_type(answer) == Some(proto::RESP_OK_NODATA))
        })
    }

    fn display_size(&mut self) -> Option<(u32, u32)> {
        let request = proto::get_display_info();
        let answers = self.run(&[Exchange {
            request: &[&request],
            response: proto::DISPLAY_INFO_SIZE,
        }])?;
        proto::scanout_size(&answers[0])
    }

    /// Creates a resource backed by `front`. Returns its identifier.
    fn create(&mut self, width: u32, height: u32, front: &Buffer) -> Result<u32, &'static str> {
        let resource = self.next_resource;
        self.next_resource += 1;
        let runs = proto::coalesce(&front.frames, PAGE);
        let create = proto::resource_create_2d(resource, width, height);
        let attach = proto::attach_backing(resource, runs.len() as u32);
        let entries = proto::backing_entries(&runs);
        if self.command(&[&[&create], &[&attach, &entries]]) {
            Ok(resource)
        } else {
            Err("ressource refusee par le peripherique")
        }
    }

    /// Detaches and destroys a resource the scanout no longer shows.
    ///
    /// The buffers are **returned**, not freed: freeing unmaps thousands of
    /// pages, each followed by a TLB shootdown, and that must not happen
    /// under this lock -- it masks interrupts.
    fn retire(&mut self, old: Scanout) -> (Buffer, Buffer) {
        let detach = proto::detach_backing(old.resource);
        let unref = proto::resource_unref(old.resource);
        // Freed even if the device refuses: once detached from the
        // scanout, the host has no business reading the frames, and a
        // leak per resize would add up.
        let _ = self.command(&[&[&detach], &[&unref]]);
        (old.front, old.back)
    }
}

/// Frees buffers `retire` or a failed `set_mode` handed back.
fn release(mut front: Buffer, mut back: Buffer) {
    // SAFETY: the resource they backed is detached or was never created,
    // and the screen lock no longer names them.
    unsafe {
        front.release();
        back.release();
    }
}

/// The hook the framebuffer calls after publishing a rectangle.
///
/// Called under the screen lock. Takes the GPU lock, which is the order the
/// module header fixes.
fn flush(x: usize, y: usize, width: usize, height: usize) {
    let mut guard = GPU.lock();
    let Some(gpu) = guard.as_mut() else {
        return;
    };
    // The GPU core's output is on screen: the console's framebuffer is
    // not shown, and sending it would cost a host copy per frame for
    // nothing.
    if gpu.gpu_output.is_some() {
        return;
    }
    let Some(current) = gpu.current.as_ref() else {
        return;
    };
    let (resource, screen_w, screen_h) = (current.resource, current.width, current.height);
    let x = (x as u32).min(screen_w);
    let y = (y as u32).min(screen_h);
    let width = (width as u32).min(screen_w - x);
    let height = (height as u32).min(screen_h - y);
    if width == 0 || height == 0 {
        return;
    }
    let area = Area::new(x, y, width, height);
    let offset = (y as u64 * screen_w as u64 + x as u64) * 4;
    let transfer = proto::transfer_to_host_2d(resource, area, offset);
    let show = proto::resource_flush(resource, area);
    // Posted, not waited for: see `Queue::post`. The answers are not read
    // -- a transfer the host refused shows as a stale rectangle, which a
    // later full repaint corrects, and waiting to learn it is what cost a
    // round trip per frame.
    let header = proto::HEADER_SIZE;
    let posted = gpu.queue.post(&[
        Exchange {
            request: &[&transfer],
            response: header,
        },
        Exchange {
            request: &[&show],
            response: header,
        },
    ]);
    if posted {
        FLUSHES.fetch_add(1, Ordering::Relaxed);
    } else if !gpu.broken {
        gpu.broken = true;
        crate::serial_println!("[virtio-gpu] le peripherique ne repond plus - affichage fige");
    }
}

/// Switches the screen to `width` x `height`.
///
/// Called with no lock held: see the three steps of the module header.
/// The frames are allocated and freed outside every lock; only the
/// commands to the device run under the GPU lock.
pub fn set_mode(width: u32, height: u32) -> Result<(), &'static str> {
    let (width, height) = clamp_size(width, height);

    let slot = {
        let guard = GPU.lock();
        let gpu = guard.as_ref().ok_or("pas de virtio-gpu")?;
        if gpu.broken {
            return Err("peripherique hors service");
        }
        match gpu.current.as_ref() {
            Some(current) if (current.width, current.height) == (width, height) => return Ok(()),
            Some(current) if current.slot == 1 => 2,
            _ => 1,
        }
    };

    // --- 1. buffers, then the resource ---
    let pages = (width as u64 * 4 * height as u64).div_ceil(PAGE);
    let (_, _, free) = crate::frame::stats();
    if free < 2 * pages as usize + RESERVE_FRAMES {
        return Err("memoire insuffisante pour cette taille");
    }
    let front_base = super::FRONT_VMA + slot * SLOT;
    let back_base = super::BACK_VMA + slot * SLOT;
    let Some(front) = (unsafe { Buffer::allocate(front_base, pages) }) else {
        return Err("plus de cadre pour l'image");
    };
    let Some(back) = (unsafe { Buffer::allocate(back_base, pages) }) else {
        release(front, Buffer { base: back_base, frames: Vec::new() });
        return Err("plus de cadre pour le tampon arriere");
    };

    let created = match GPU.lock().as_mut() {
        Some(gpu) => gpu.create(width, height, &front),
        None => Err("pas de virtio-gpu"),
    };
    let resource = match created {
        Ok(resource) => resource,
        Err(reason) => {
            release(front, back);
            return Err(reason);
        }
    };

    // --- 2. the framebuffers, under the screen lock only ---
    let info = FramebufferInfo {
        addr: front.frames.first().copied().unwrap_or(0),
        pitch: width * 4,
        width,
        height,
        bpp: 32,
        kind: FB_RGB,
        red_pos: 16,
        red_size: 8,
        green_pos: 8,
        green_size: 8,
        blue_pos: 0,
        blue_size: 8,
    };
    let mut fb = match unsafe { Framebuffer::new(&info, front_base) } {
        Ok(fb) => fb,
        Err(reason) => {
            let orphan = Scanout { resource, width, height, slot, front, back };
            let buffers = GPU.lock().as_mut().map(|gpu| gpu.retire(orphan));
            if let Some((front, back)) = buffers {
                release(front, back);
            }
            return Err(reason);
        }
    };
    unsafe { fb.attach_back_buffer(back_base) };
    fb.set_flush(flush);
    let was_boot = super::install_framebuffer(fb);

    // --- 3. the scanout, under the GPU lock ---
    let retired = {
        let mut guard = GPU.lock();
        let gpu = guard.as_mut().ok_or("pas de virtio-gpu")?;
        // While the GPU core's output is shown, the new framebuffer waits
        // behind it: the compositor makes an output of the new size and
        // shows that (`scanout::show`).
        if gpu.gpu_output.is_none() {
            let whole = Area::new(0, 0, width, height);
            let point = proto::set_scanout(0, resource, whole);
            let transfer = proto::transfer_to_host_2d(resource, whole, 0);
            let show = proto::resource_flush(resource, whole);
            if !gpu.command(&[&[&point], &[&transfer], &[&show]]) {
                crate::serial_println!("[virtio-gpu] SET_SCANOUT refuse pour {}x{}", width, height);
            }
        }
        let old = gpu.current.replace(Scanout { resource, width, height, slot, front, back });
        old.map(|old| gpu.retire(old))
    };
    if let Some((front, back)) = retired {
        release(front, back);
    }
    if was_boot {
        super::retire_boot_framebuffer();
    }

    ACTIVE.store(true, Ordering::Release);
    RESIZES.fetch_add(1, Ordering::Relaxed);
    crate::serial_println!(
        "[virtio-gpu] mode {}x{} - ressource {}, emplacement {}, {} pages par tampon",
        width,
        height,
        resource,
        slot,
        pages
    );
    Ok(())
}

/// Finds the device and brings it up, once. Returns the size of the host
/// window.
///
/// Called by `init` for the screen, and by the GPU core for 3D -- on a
/// machine whose screen is not the virtio-gpu (a GRUB framebuffer kept,
/// `virtio-gpu-pci` next to another card), only the second. The device is
/// reset by the first call only: a second bring-up would drop the
/// scanout and every resource the first one created.
pub fn probe() -> Result<(u32, u32), &'static str> {
    if let Some(gpu) = GPU.lock().as_mut() {
        return Ok(gpu.wanted);
    }
    let device = crate::pci::enumerate()
        .into_iter()
        .find(|device| device.vendor_id == VENDOR_VIRTIO && device.device_id == DEVICE_GPU)
        .ok_or("aucun virtio-gpu sur le bus PCI")?;

    let msix = crate::smp::lapic_of(0).map(|apic| (crate::drivers::VECTOR_GPU, apic));
    let up = unsafe { transport::bring_up(&device, transport::FEATURES_3D, msix)? };
    let negotiated = up.negotiated;
    let size = up.control.size();
    let mut gpu = Gpu {
        transport: up.transport,
        queue: up.control,
        cursor_queue: up.cursor,
        gpu_output: None,
        cursor: None,
        current: None,
        next_resource: 1,
        three_d: None,
        interrupts: negotiated.interrupts,
        wanted: (0, 0),
        steady: 0,
        tried: (0, 0),
        broken: false,
    };
    // An event raised before the driver existed means nothing now.
    gpu.transport.device_write32(CONFIG_EVENTS_CLEAR, proto::EVENT_DISPLAY);
    DEVICE_CONFIG.store(gpu.transport.device_base(), Ordering::Release);
    let host = gpu.display_size().unwrap_or((1024, 768));
    // Stored clamped, as `poll_resize` stores it: an unclamped size would
    // differ from the mode actually set, and read as a resize request on
    // the very first poll.
    gpu.wanted = clamp_size(host.0, host.1);
    gpu.tried = gpu.wanted;
    let three_d = context::discover(&mut gpu, &negotiated);
    let summary = match &three_d {
        Ok(state) => state.summary(),
        Err(reason) => alloc::format!("3D absente - {}", reason),
    };
    gpu.three_d = three_d.ok();
    let interrupts = gpu.interrupts;
    *GPU.lock() = Some(gpu);

    crate::serial_println!(
        "[virtio-gpu] {:02x}:{:02x}.{} - file de {} descripteurs{} - hote {}x{}",
        device.address.bus,
        device.address.device,
        device.address.function,
        size,
        if interrupts { ", interruption MSI-X" } else { ", scrutee" },
        host.0,
        host.1
    );
    crate::serial_println!("[virtio-gpu] {}", summary);
    Ok(host)
}

/// Finds the device, brings it up and switches the screen to the size of
/// the host window. Returns that size.
///
/// Called once, by `gui::start`, before the compositor reads the
/// resolution: everything drawn from then on is drawn at the right size.
pub fn init() -> Result<(u32, u32), &'static str> {
    let host = probe()?;
    set_mode(host.0, host.1)?;
    Ok(clamp_size(host.0, host.1))
}

/// Has the host asked for another size, steadily?
///
/// Called by the compositor a few times a second. Returns the new size only
/// once two polls in a row have seen it: dragging the edge of a window
/// announces a size per movement, and switching mode for each would
/// allocate and free megabytes for images nobody sees.
pub fn poll_resize() -> Option<(u32, u32)> {
    // The event register is read **without** the lock. Reading the
    // device's configuration traps into QEMU, which serves it under its
    // own big lock -- and while a program holds the host's GPU, that lock
    // is held by a GL call for seconds. A reader holding the device's
    // spin lock meanwhile froze every core waiting for it, timer and all
    // (13.11.3). Without the lock, only this core waits.
    let config = DEVICE_CONFIG.load(Ordering::Acquire);
    let event = config != 0
        // SAFETY: the configuration block is mapped uncached for good at
        // bring-up; `events_read` is a 32-bit register at offset 0.
        && unsafe { ((config + CONFIG_EVENTS_READ) as *const u32).read_volatile() } & proto::EVENT_DISPLAY != 0;
    let mut guard = GPU.lock();
    let gpu = guard.as_mut()?;
    if gpu.broken {
        return None;
    }
    if event {
        gpu.transport.device_write32(CONFIG_EVENTS_CLEAR, proto::EVENT_DISPLAY);
        if let Some(size) = gpu.display_size() {
            let size = clamp_size(size.0, size.1);
            if size != gpu.wanted {
                gpu.wanted = size;
                gpu.steady = 0;
                return None;
            }
        }
    }
    let current = gpu.current.as_ref().map(|scanout| (scanout.width, scanout.height))?;
    if gpu.wanted == current {
        gpu.steady = 0;
        return None;
    }
    gpu.steady += 1;
    if gpu.steady < 2 || gpu.wanted == gpu.tried {
        return None;
    }
    gpu.tried = gpu.wanted;
    Some(gpu.wanted)
}

pub fn dump_state() {
    let guard = GPU.lock();
    match guard.as_ref() {
        None => crate::serial_println!("[virtio-gpu] absent - framebuffer GRUB en taille fixe"),
        Some(gpu) => {
            let (width, height) = gpu
                .current
                .as_ref()
                .map(|scanout| (scanout.width, scanout.height))
                .unwrap_or((0, 0));
            crate::serial_println!(
                "[virtio-gpu] {}x{} - {} changement(s) de mode, {} envoi(s) a l'hote, {} requete(s) en vol{}",
                width,
                height,
                RESIZES.load(Ordering::Relaxed),
                FLUSHES.load(Ordering::Relaxed),
                gpu.queue.in_flight(),
                if gpu.broken { " - HORS SERVICE" } else { "" }
            );
            if let Some(three_d) = gpu.three_d.as_ref() {
                crate::serial_println!("[virtio-gpu] {}", three_d.summary());
            }
        }
    }
}


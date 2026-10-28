//! Pixel canvases: a surface whose content the application paints itself.
//!
//! ## Why the display list is not enough
//!
//! A surface is a list of drawing commands, capped at `MAX_COMMANDS` so
//! that no client can make the render of the whole screen slow. That suits
//! a dialog, a menu, a list -- a few dozen rectangles and lines of text.
//! A web page is thousands of runs of text, and the cap would cut it off
//! a third of the way down.
//!
//! So a client may instead paint pixels into a buffer and hand the
//! compositor **one** command, `OP_DRAW_CANVAS`, that copies the buffer
//! into its surface. The cost of the render is then that of a copy, bounded
//! by the size of the surface, whatever the page holds.
//!
//! ## How the buffer is shared
//!
//! Spec 6.4 provides `SYS_MEM_SHARE`, "share a region through IPC", and
//! this is that sharing -- but with a reference count instead of an
//! address. Every task today runs in one address space (the limit opened
//! by phase 3), so an address would work; it would also let the client
//! free the region while the compositor copies from it, on another core,
//! with its lock held and interrupts masked. An `Arc` makes that
//! impossible: the pixels live until the last of the two lets go. The day
//! a CR3 per process exists, this becomes a mapped region and its
//! contract -- create, paint, publish, attach by identifier -- stays.
//!
//! ## Double buffering
//!
//! The client paints into its own `pixels` with no lock, then `publish`
//! swaps them with the shared ones under a lock held for the time of a
//! pointer swap. The compositor never sees a half-painted frame, and never
//! waits for a client that paints slowly. After `publish`, `pixels` holds
//! the **previous** frame: a client repaints fully each time, which a
//! browser does anyway.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::graphics::framebuffer::Color;
use crate::process::Pid;
use crate::vga::SpinLock;

/// Largest canvas side, in pixels -- the largest screen `virtio_gpu`
/// accepts.
pub const MAX_SIDE: usize = 4096;

/// The shared pixels.
pub struct Pixels {
    pub width: usize,
    pub height: usize,
    pub data: Vec<Color>,
    /// When the current frame was published, in nanoseconds (the Linux
    /// monotonic clock, calibrated on the TSC).
    pub published_ns: u64,
    /// What the screen made of the published frames.
    pub pacing: Pacing,
}

/// How the frames of a canvas reached the screen: the delay from
/// `publish` to the compositor's copy, and the interval between two new
/// frames on screen. A video can lose no frame and still stutter -- a
/// frame held 50 ms, the next 17 -- and only the intervals show it.
#[derive(Clone, Copy, Default)]
pub struct Pacing {
    /// Stamp of the last frame copied to the screen.
    shown_stamp: u64,
    /// When it was copied.
    shown_at: u64,
    /// New frames copied to the screen.
    pub frames: u64,
    pub latency_sum_ns: u64,
    pub latency_max_ns: u64,
    /// Intervals between two new frames on screen, in buckets of
    /// `INTERVAL_BUCKETS_MS` (the last one open-ended).
    pub intervals: [u64; 6],
    pub interval_max_ns: u64,
}

/// Upper bounds of the interval buckets, in milliseconds: below a 60 Hz
/// period, one period, 30 i/s, 2.5 periods, 4 periods, beyond.
pub const INTERVAL_BUCKETS_MS: [u64; 5] = [12, 22, 38, 55, 75];

impl Pacing {
    /// A frame is being copied to the screen at `now`.
    pub fn shown(&mut self, stamp: u64, now: u64) {
        if stamp == 0 || stamp == self.shown_stamp {
            return;
        }
        self.frames += 1;
        let latency = now.saturating_sub(stamp);
        self.latency_sum_ns += latency;
        self.latency_max_ns = self.latency_max_ns.max(latency);
        // An interval longer than a second is a pause, not a stutter.
        if self.shown_at != 0 && now - self.shown_at < 1_000_000_000 {
            let interval = now - self.shown_at;
            let ms = interval / 1_000_000;
            let bucket = INTERVAL_BUCKETS_MS.iter().position(|&b| ms < b).unwrap_or(5);
            self.intervals[bucket] += 1;
            self.interval_max_ns = self.interval_max_ns.max(interval);
        }
        self.shown_stamp = stamp;
        self.shown_at = now;
    }
}

/// What a display list holds: the shared pixels, reference-counted.
#[derive(Clone)]
pub struct CanvasRef(pub Arc<SpinLock<Pixels>>);

impl core::fmt::Debug for CanvasRef {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "CanvasRef")
    }
}

/// Canvases by identifier, with their owner: `OP_DRAW_CANVAS` attaches
/// a canvas only to the surface of the task that created it.
static REGISTRY: SpinLock<Vec<(u32, Pid, CanvasRef)>> = SpinLock::new(Vec::new());
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

/// The client's side.
pub struct Canvas {
    pub id: u32,
    pub width: usize,
    pub height: usize,
    /// The frame being painted.
    pub pixels: Vec<Color>,
    shared: CanvasRef,
}

impl Canvas {
    /// A canvas of `width` x `height`, owned by the calling task.
    pub fn new(width: usize, height: usize) -> Canvas {
        let width = width.clamp(1, MAX_SIDE);
        let height = height.clamp(1, MAX_SIDE);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let shared = CanvasRef(Arc::new(SpinLock::new(Pixels {
            width,
            height,
            data: alloc::vec![0; width * height],
            published_ns: 0,
            pacing: Pacing::default(),
        })));
        REGISTRY
            .lock()
            .push((id, crate::scheduler::current_pid(), shared.clone()));
        Canvas {
            id,
            width,
            height,
            pixels: alloc::vec![0; width * height],
            shared,
        }
    }

    /// Makes the painted frame the one the compositor copies.
    pub fn publish(&mut self) {
        let now = crate::posix::time::monotonic_ns();
        let mut shared = self.shared.0.lock();
        shared.published_ns = now;
        if shared.width == self.width && shared.height == self.height {
            core::mem::swap(&mut shared.data, &mut self.pixels);
        } else {
            // A resize: the shared buffer takes the new size.
            shared.width = self.width;
            shared.height = self.height;
            shared.data = core::mem::take(&mut self.pixels);
            self.pixels = alloc::vec![0; self.width * self.height];
        }
    }

    /// The pacing statistics since the previous call, reset.
    pub fn take_pacing(&mut self) -> Pacing {
        let mut shared = self.shared.0.lock();
        let taken = shared.pacing;
        shared.pacing = Pacing { shown_stamp: taken.shown_stamp, shown_at: taken.shown_at, ..Pacing::default() };
        taken
    }

    /// Changes the size. The next `publish` carries it to the compositor.
    pub fn resize(&mut self, width: usize, height: usize) {
        self.width = width.clamp(1, MAX_SIDE);
        self.height = height.clamp(1, MAX_SIDE);
        self.pixels = alloc::vec![0; self.width * self.height];
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        REGISTRY.lock().retain(|(id, _, _)| *id != self.id);
    }
}

/// The canvas `id`, if `owner` created it.
pub fn lookup(id: u32, owner: Pid) -> Option<CanvasRef> {
    REGISTRY
        .lock()
        .iter()
        .find(|(candidate, created_by, _)| *candidate == id && *created_by == owner)
        .map(|(_, _, canvas)| canvas.clone())
}

/// Forgets the canvases of a task that died without dropping them.
pub fn forget_owner(owner: Pid) {
    REGISTRY.lock().retain(|(_, created_by, _)| *created_by != owner);
}

/// Canvases alive, for the state dump.
pub fn count() -> usize {
    REGISTRY.lock().len()
}

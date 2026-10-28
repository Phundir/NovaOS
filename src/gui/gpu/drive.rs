//! The compositor task's hold on the GPU: when to switch to it, a frame,
//! a resize, the cursor plane, and the way back.
//!
//! ## When
//!
//! The GPU core starts after the boot self-tests (DECISIONS, "Le GPU se
//! met en route après les autotests du démarrage"): until then the
//! compositor paints on the processor, as it always did, and the phase 7
//! self-tests read the framebuffer they expect. On the first lap after
//! the core is up, the compositor opens its context, shows its output and
//! repaints the whole screen on the GPU. One attempt: a refusal is logged
//! and the processor keeps the screen for good.
//!
//! ## Slow, then lost
//!
//! A frame the GPU does not finish in time (`Renderer::draw` waits
//! 100 ms) hands the painting to the processor -- `suspended` -- until the
//! late frame is done. The processor paints straight into the GPU's
//! output, which stays on the screen: switching the screen back to the
//! console's framebuffer would be a command to wait for, and QEMU answers
//! none while the host's GPU is held (13.11.3). The compositor thus goes
//! on taking its mail and its input, and the host shows the frames as
//! soon as its GPU is free again. Once the late frame is done, the GPU
//! paints again, the whole screen first. A context lost is closed and
//! another one opened a second later; after three losses the processor
//! keeps the screen, and says so.
//!
//! ## The cursor plane
//!
//! While the GPU composes and no accessibility transform is on (the
//! magnifier must magnify the pointer too), the overlay surface -- the
//! pointer `cursor.mod` draws -- is not composed: it is painted once per
//! change of shape into a 64 x 64 image with an alpha byte, and the device
//! shows it over the screen (`gpu::screen::cursor_image`). A movement is
//! then one `MOVE_CURSOR`, and repaints nothing.

use core::sync::atomic::{AtomicBool, Ordering};

use super::{DrawError, Renderer, Settled};
use crate::gui::compositor::{self, Compositor};
use crate::gui::painter::{Image, Painter};
use crate::gui::surface::{Layer, Rect};

/// What the plane shows: the overlay surface at a given commit, and
/// where.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Shown {
    surface: u32,
    commits: u64,
    size: (i32, i32),
    hot: (u32, u32),
    position: (i32, i32),
}

/// Keep the processor painting even with a GPU: the 13.8.3 measure runs
/// the same frames both ways.
static PREFER_PROCESSOR: AtomicBool = AtomicBool::new(false);

/// Asks the compositor to paint on the processor (`true`) or on the GPU.
/// Taken into account on its next lap.
pub fn prefer_processor(on: bool) {
    PREFER_PROCESSOR.store(on, Ordering::Release);
}

/// Contexts lost before the processor keeps the screen for good.
const MAX_LOSSES: u32 = 3;

/// Wait after a loss before a new context is opened, in milliseconds.
const RETRY_MS: u64 = 1000;

pub struct Driver {
    renderer: Option<Renderer>,
    /// The switch was refused, or the GPU lost too often: the processor
    /// keeps the screen.
    given_up: bool,
    /// The processor holds the screen while a late frame finishes.
    suspended: bool,
    losses: u32,
    /// No new context before this instant (fine ticks).
    retry_at: u64,
    cursor: Option<Shown>,
}

impl Default for Driver {
    fn default() -> Self {
        Self::new()
    }
}

impl Driver {
    pub const fn new() -> Driver {
        Driver { renderer: None, given_up: false, suspended: false, losses: 0, retry_at: 0, cursor: None }
    }

    /// Is the GPU busy with a late frame? The compositor then leaves the
    /// device alone as much as it can (see `suspend`).
    pub fn suspended(&self) -> bool {
        self.suspended
    }

    /// Does the GPU compose the screen right now?
    pub fn active(&self) -> bool {
        self.renderer.is_some() && !self.suspended
    }

    pub fn renderer(&self) -> Option<&Renderer> {
        self.renderer.as_ref()
    }

    /// Switches to the GPU once its core is up. Called every lap.
    pub fn poll(&mut self) {
        if PREFER_PROCESSOR.load(Ordering::Acquire) {
            if let Some(renderer) = self.renderer.take() {
                renderer.teardown();
                self.suspended = false;
                self.hide_cursor();
                crate::serial_println!("[gui] compositeur rendu au processeur (demande)");
            }
            return;
        }
        if self.renderer.is_some()
            || self.given_up
            || !crate::gpu::started()
            || !crate::gpu::present()
            || crate::idt::fine_ticks() < self.retry_at
        {
            return;
        }
        let Some((width, height)) = crate::graphics::resolution() else {
            return;
        };
        let started = crate::posix::time::monotonic_ns();
        match Renderer::new(width as u32, height as u32) {
            Ok(renderer) => {
                self.renderer = Some(renderer);
                super::super::with(|comp| comp.damage_all());
                crate::serial_println!(
                    "[gui] compositeur sur le GPU - {}x{}, sortie en memoire hote, pret en {} ms",
                    width,
                    height,
                    (crate::posix::time::monotonic_ns() - started) / 1_000_000
                );
            }
            Err(reason) => {
                self.given_up = true;
                crate::serial_println!("[gui] compositeur GPU refuse ({}) - rendu processeur", reason);
            }
        }
    }

    /// Draws a frame on the GPU if it holds the screen. Returns `false`
    /// when the processor must paint instead.
    pub fn frame(&mut self) -> bool {
        let Some(renderer) = self.renderer.as_mut() else {
            return false;
        };
        if self.suspended {
            match renderer.settle() {
                Settled::Pending => {
                    super::super::with(|comp| {
                        let damage = compositor::take_damage(comp)?;
                        let comp = &*comp;
                        renderer.paint_processor(damage, |raw| compositor::paint(comp, raw, damage));
                        Some(())
                    });
                    return true;
                }
                Settled::Lost => {
                    self.lose("contexte perdu pendant l'attente");
                    return false;
                }
                Settled::Done => {
                    renderer.invalidate();
                    self.suspended = false;
                    super::super::with(|comp| comp.damage_all());
                    crate::serial_println!("[gui] GPU de nouveau disponible - le compositeur y revient");
                }
            }
        }
        let Some(renderer) = self.renderer.as_mut() else {
            return false;
        };
        // A resize reached the compositor: the output follows first.
        let size = super::super::with(|comp| (comp.width as u32, comp.height as u32));
        if let Some(size) = size
            && size != renderer.size()
        {
            if let Err(reason) = renderer.resize(size.0, size.1) {
                self.lose(reason);
                return false;
            }
            super::super::with(|comp| comp.damage_all());
        }
        let frame = super::super::with(|comp| {
            let damage = compositor::take_damage(comp)?;
            let comp = &*comp;
            Some(renderer.record(damage, |recorder| compositor::paint(comp, recorder, damage)))
        })
        .flatten();
        if let Some(frame) = frame {
            match renderer.draw(frame) {
                Ok(()) => {}
                Err(DrawError::Slow) => {
                    self.suspend();
                    return false;
                }
                Err(DrawError::Lost(reason)) => {
                    self.lose(reason);
                    return false;
                }
            }
        }
        self.update_cursor();
        if let Some(renderer) = self.renderer.as_ref() {
            *super::STATS.lock() = Some((renderer.stats, renderer.held()));
        }
        true
    }

    /// The GPU did not finish a frame in time: the processor takes the
    /// screen, the frame stays pending (`frame` looks at it every lap).
    fn suspend(&mut self) {
        self.suspended = true;
        // The cursor plane is left as it is: hiding it has QEMU redraw
        // the pointer with OpenGL, which waits for the held GPU.
        super::super::with(|comp| comp.damage_all());
        crate::serial_println!("[gui] GPU occupe (une image en retard) - le processeur peint en attendant");
    }

    /// The compositor's context is lost: closed, the screen back to the
    /// processor, another context a second later -- or never, after
    /// `MAX_LOSSES`.
    fn lose(&mut self, reason: &str) {
        if let Some(renderer) = self.renderer.take() {
            renderer.teardown();
        }
        self.suspended = false;
        self.losses += 1;
        self.retry_at = crate::idt::fine_ticks() + RETRY_MS;
        self.hide_cursor();
        if self.losses >= MAX_LOSSES {
            self.given_up = true;
            crate::serial_println!("[gui] compositeur GPU abandonne ({}, {} pertes) - rendu processeur", reason, self.losses);
        } else {
            crate::serial_println!("[gui] contexte du compositeur perdu ({}) - rendu processeur, nouvel essai dans 1 s", reason);
        }
    }

    /// The pointer back in the frame, the whole screen repainted.
    fn hide_cursor(&mut self) {
        if self.cursor.take().is_some() {
            crate::gpu::screen::cursor_hide();
        }
        super::super::with(|comp| {
            comp.cursor_plane = false;
            comp.damage_all();
        });
    }

    /// Keeps the cursor plane in step with the overlay surface.
    fn update_cursor(&mut self) {
        // An image staged at an earlier frame goes on the plane once the
        // host has its pixels.
        crate::gpu::screen::cursor_poll();
        let wanted = crate::gpu::screen::cursor_plane() && !crate::access::transforms_pixels();
        let previous = self.cursor;
        let state = super::super::with(|comp| overlay_state(comp, wanted, previous)).flatten();
        match state {
            None => {
                if self.cursor.take().is_some() {
                    crate::gpu::screen::cursor_hide();
                }
            }
            Some((shown, image)) => {
                let same_image = previous.is_some_and(|p| (p.surface, p.commits, p.size, p.hot) == (shown.surface, shown.commits, shown.size, shown.hot));
                if !same_image {
                    if let Some(image) = image
                        && crate::gpu::screen::cursor_image(&image.pixels, shown.hot, shown.position.0, shown.position.1)
                    {
                        self.cursor = Some(shown);
                    }
                } else if previous.is_some_and(|p| p.position != shown.position) {
                    crate::gpu::screen::cursor_move(shown.position.0, shown.position.1);
                    self.cursor = Some(shown);
                }
            }
        }
    }
}

/// Under the compositor's lock: turns the plane on or off, and describes
/// the overlay surface -- with its image painted when the plane may need
/// it.
fn overlay_state(comp: &mut Compositor, wanted: bool, previous: Option<Shown>) -> Option<(Shown, Option<Image>)> {
    let overlay = comp
        .stack
        .iter()
        .filter(|surface| surface.layer == Layer::Overlay && surface.visible)
        .max_by_key(|surface| (surface.z, surface.sequence))
        .map(|surface| (surface.id, surface.commits, surface.rect));
    let usable = wanted && overlay.is_some_and(|(_, _, rect)| rect.width <= 64 && rect.height <= 64);
    if comp.cursor_plane != usable {
        comp.cursor_plane = usable;
        // Composed until now, or composed from now on: its place is
        // repainted either way.
        if let Some((_, _, rect)) = overlay {
            comp.damage_rect(Rect::new(rect.x - 1, rect.y - 1, rect.width + 2, rect.height + 2));
        }
    }
    if !usable {
        return None;
    }
    let (id, commits, rect) = overlay?;
    // The hot spot is the part of the image left of or above the screen:
    // the device places the image at `position - hot`, and takes no
    // negative position.
    let hot = ((-rect.x).max(0) as u32, (-rect.y).max(0) as u32);
    let shown = Shown {
        surface: id,
        commits,
        size: (rect.width, rect.height),
        hot,
        position: (rect.x + hot.0 as i32, rect.y + hot.1 as i32),
    };
    if previous.is_some_and(|p| (p.surface, p.commits, p.size, p.hot) == (shown.surface, shown.commits, shown.size, shown.hot)) {
        return Some((shown, None));
    }
    let mut image = Image::new(64, 64, true);
    let surface = comp.stack.get(id)?;
    {
        let mut painter = Painter::screen(&mut image);
        let mut region = painter.region(Rect::new(0, 0, rect.width, rect.height));
        region.replay(surface.display_list());
    }
    Some((shown, Some(image)))
}

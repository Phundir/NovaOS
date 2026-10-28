//! 7.3.5 -- The native modules of 8.2.
//!
//! ## What they have in common
//!
//! Each is a task that connects to the compositor, creates a surface in
//! its layer, draws, then waits for events. None touches the framebuffer:
//! they emit instructions, the compositor paints. 8.1 is thus respected
//! by construction rather than by discipline -- a module simply has no
//! other way of displaying anything at all.
//!
//! ## The common canvas
//!
//! `run` carries the loop: connection, surface creation, first render,
//! then waiting. Each module supplies only its geometry and its drawing
//! function. It is not mutualisation for its own sake: the six loops
//! written separately would diverge on the handling of `TAG_MODULE_STOP`,
//! and a module that ignores the stop leaves its surface on screen --
//! exactly the defect 8.3 asks to avoid.

pub mod cursor;
pub mod launcher;
pub mod notifs;
pub mod taskbar;
pub mod winmanager;
pub mod wallpaper;

use core::sync::atomic::{AtomicI32, Ordering};

/// How long a module with a refresh period sleeps between two polls.
///
/// Not the module's own period: `taskbar.mod` refreshes its clock once a
/// second, but it must answer a **click** far sooner than that. The sleep
/// is therefore the shorter of the two, and it sets the worst case latency
/// of a click on a module.
///
/// Ten milliseconds is one compositor frame, the finest the 100 Hz timer
/// can time. A hundred -- the value this was first -- is past what a hand
/// reads as immediate, and it was only half the problem: the other half
/// was taking a single event per wake.
const POLL_PERIOD_MS: u64 = 10;

use super::client::{Client, Event};
use super::protocol::{CursorShape, PointerEvent};
use super::surface::{Layer, SurfaceId};
use super::theme::Theme;

/// Context handed to the drawing function of a module.
pub struct Canvas<'a> {
    pub client: &'a mut Client,
    pub surface: SurfaceId,
    pub theme: Theme,
    /// Where `geometry` placed the surface, in screen coordinates.
    ///
    /// Needed because a pointer event carries **screen** coordinates while
    /// everything a module draws is local to its surface. A module that
    /// hit-tested without subtracting this would answer clicks that
    /// happened elsewhere on screen -- and for the task bar, pinned to the
    /// bottom edge, "elsewhere" means the whole desktop above it.
    ///
    /// This is where `geometry` put it, not necessarily where it is now: a
    /// module that moves its own surface, as `cursor.mod` does, must not
    /// read this.
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// Dimensions of the screen, which most modules need.
    pub screen_width: i32,
    pub screen_height: i32,
}

/// What a module must supply.
pub struct Spec {
    pub name: &'static str,
    pub layer: Layer,
    /// Geometry, computed from the screen and the theme.
    pub geometry: fn(i32, i32, &Theme) -> (i32, i32, i32, i32),
    /// Complete drawing of the surface.
    pub draw: fn(&mut Canvas),
    /// Handling of an event. Returns `true` if a repaint is needed.
    pub on_event: fn(&mut Canvas, &Event) -> bool,
    /// Forced refresh period, in milliseconds. Zero for "on events only".
    pub tick_ms: u64,
    /// Is the surface shown as soon as it is created?
    ///
    /// A menu opens on demand, not at boot. Declaring it here rather than
    /// hiding it in the drawing function stops the first repaint
    /// reopening it: visibility is a state of the surface, not a
    /// consequence of the drawing.
    pub visible_at_start: bool,
}

/// Loop common to every native module.
///
/// Never returns: it ends with `exit_current_with`, including on a stop
/// request. A module returning from its loop would leave a task with no
/// stack.
pub fn run(spec: &Spec) -> ! {
    let Some(mut client) = Client::connect(5_000) else {
        crate::serial_println!("[mod-{}] compositeur injoignable", spec.name);
        crate::scheduler::exit_current_with(-1);
    };

    let Some((screen_width, screen_height, _, _)) = client.screen_info() else {
        crate::serial_println!("[mod-{}] ecran non decrit", spec.name);
        crate::scheduler::exit_current_with(-1);
    };
    let screen_width = screen_width as i32;
    let screen_height = screen_height as i32;
    let theme = client.theme().unwrap_or_default();

    let (x, y, width, height) = (spec.geometry)(screen_width, screen_height, &theme);
    let Some(surface) = client.create_surface(spec.layer, x, y, width, height, spec.name) else {
        crate::serial_println!("[mod-{}] creation de surface refusee", spec.name);
        crate::scheduler::exit_current_with(-1);
    };

    let mut canvas = Canvas {
        client: &mut client,
        surface,
        theme,
        x,
        y,
        width,
        height,
        screen_width,
        screen_height,
    };

    repaint(spec, &mut canvas);
    if !spec.visible_at_start {
        canvas.client.show_surface(surface, false);
    }
    crate::serial_println!(
        "[mod-{}] surface {} - {}x{} en ({}, {})",
        spec.name,
        surface,
        width,
        height,
        x,
        y
    );

    let mut next_tick = crate::idt::ticks();

    loop {
        let mut dirty = false;

        // A module with no refresh period sleeps on its box; one with a
        // period has just woken from its sleep and takes what is waiting.
        // The choice belongs to the module because only it knows whether
        // its content depends on time.
        if spec.tick_ms == 0 {
            let event = canvas.client.next_event();
            dirty |= dispatch(spec, &mut canvas, surface, &event);
        }

        // Then **empty the box**, and that is not an optimisation.
        //
        // This loop used to take one event per turn. For a module with a
        // period that meant one event per sleep, hence one per hundred
        // milliseconds -- and a pointer publishes an event per movement.
        // Bringing the mouse to a button queued a dozen of them, and the
        // click that followed waited behind the whole queue. Measured on
        // the task bar: **1336 ms** between the press and the menu opening.
        //
        // The bound is the size of a mailbox, so that one wake can always
        // empty a full box and the backlog can never build up again.
        for _ in 0..crate::ipc::MAX_QUEUE_LEN {
            let Some(event) = canvas.client.poll_event() else {
                break;
            };
            dirty |= dispatch(spec, &mut canvas, surface, &event);
        }

        if spec.tick_ms != 0 {
            let per_tick = 1000 / crate::TIMER_HZ as u64;
            let now = crate::idt::ticks();
            if now >= next_tick {
                next_tick = now + spec.tick_ms.div_ceil(per_tick).max(1);
                dirty = true;
            }
        }

        if dirty {
            repaint(spec, &mut canvas);
        }

        if spec.tick_ms != 0 {
            let _ = crate::syscall::invoke(
                crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
                [spec.tick_ms.min(POLL_PERIOD_MS), 0, 0, 0, 0, 0],
            );
        }
    }
}

/// Handles one event, and ends the task on a stop request.
///
/// Extracted so that the blocking path and the draining path cannot
/// diverge on it. They did not, when there was one event per turn; with
/// two call sites they would, and the one that forgot `TAG_MODULE_STOP`
/// would leave a surface on screen after its module died -- the defect 8.3
/// exists to avoid.
fn dispatch(spec: &Spec, canvas: &mut Canvas, surface: SurfaceId, event: &Event) -> bool {
    if let Event::Other(message) = event
        && super::stop_requested(message)
    {
        // 7.3.4: clean deregistration. The surface goes before the task,
        // otherwise the compositor would keep it until the next
        // collection.
        canvas.client.destroy_surface(surface);
        crate::serial_println!("[mod-{}] arret demande - surface rendue", spec.name);
        crate::scheduler::exit_current_with(0);
    }
    if let Event::ScreenResized { width, height } = event {
        relayout(spec, canvas, surface, *width, *height);
        // The module still sees the event -- and whatever it answers, the
        // surface is repainted: its size may have changed.
        let _ = (spec.on_event)(canvas, event);
        return true;
    }
    (spec.on_event)(canvas, event)
}

/// Places the surface again for a new screen size.
///
/// Through the module's own `geometry`, the same function that placed it
/// at start: a second arithmetic here would be the "14 by 20" defect of
/// DECISIONS.md waiting to happen -- the task bar placed one way at boot
/// and another after a resize.
///
/// Not for the overlay: the cursor moves its own surface with the pointer,
/// and `Canvas::x` is documented as meaningless for it.
fn relayout(spec: &Spec, canvas: &mut Canvas, surface: SurfaceId, width: i32, height: i32) {
    canvas.screen_width = width;
    canvas.screen_height = height;
    if spec.layer == Layer::Overlay {
        return;
    }
    let (x, y, w, h) = (spec.geometry)(width, height, &canvas.theme);
    if (w, h) != (canvas.width, canvas.height) {
        canvas.client.resize_surface(surface, w, h);
    }
    if (x, y) != (canvas.x, canvas.y) {
        canvas.client.move_surface(surface, x, y);
    }
    canvas.x = x;
    canvas.y = y;
    canvas.width = w;
    canvas.height = h;
    crate::serial_println!(
        "[mod-{}] ecran {}x{} - surface {}x{} en ({}, {})",
        spec.name,
        width,
        height,
        w,
        h,
        x,
        y
    );
}

/// Redraws the surface entirely.
///
/// `reset` then the commands then `commit`: the published list is
/// replaced in one block, never modified in place. That is what stops the
/// compositor painting a half-redrawn surface.
fn repaint(spec: &Spec, canvas: &mut Canvas) {
    canvas.client.reset(canvas.surface);
    (spec.draw)(canvas);
    canvas.client.commit(canvas.surface);
}

// ============================================================
//  Hover, shared by the modules that have elements
// ============================================================

/// Element under the pointer, by rank.
///
/// ## Why a static rather than a field of `Canvas`
///
/// A module is a set of function pointers with no state of its own --
/// that is what makes the loop in `run` common to all of them. State that
/// `on_event` writes and `draw` reads therefore lives in a static, as
/// `launcher::OPEN` and `cursor::SHAPE` already do.
///
/// ## Why it is one integer and not a rectangle
///
/// The rectangles belong to the drawing, which recomputes them on every
/// paint. Storing the hovered **rank** rather than its geometry means a
/// window button that shifts left because another window closed does not
/// leave a stale rectangle behind.
pub struct Hover(AtomicI32);

impl Hover {
    /// Nothing under the pointer.
    pub const NONE: i32 = -1;

    pub const fn new() -> Self {
        Hover(AtomicI32::new(Self::NONE))
    }

    /// Records what is under the pointer. Returns `true` if it changed,
    /// hence if a repaint is due.
    ///
    /// The return value is the whole point: repainting on the pointer
    /// **position** would cost a score of IPC messages per millimetre of
    /// mouse, which is the measured reason the task bar refused a hover
    /// effect for two phases. Repainting on the **boundary** costs one
    /// repaint per element entered.
    pub fn set(&self, index: i32) -> bool {
        self.0.swap(index, Ordering::Relaxed) != index
    }

    pub fn get(&self) -> i32 {
        self.0.load(Ordering::Relaxed)
    }

    pub fn is(&self, index: i32) -> bool {
        index != Self::NONE && self.get() == index
    }

    /// Forgets what was under the pointer, for when it leaves the surface.
    pub fn clear(&self) -> bool {
        self.set(Self::NONE)
    }
}

/// Pointer position in the surface's own coordinates.
pub fn local(canvas: &Canvas, pointer: &PointerEvent) -> (i32, i32) {
    (pointer.x as i32 - canvas.x, pointer.y as i32 - canvas.y)
}

/// Asks for a cursor shape, and only when it is not already in force.
///
/// ## Why the comparison is against the event and not against a memory
///
/// The event **carries** the shape the compositor currently holds. So
/// there is nothing to remember: a module that compares against what it
/// last sent would get it wrong the moment the compositor resets the
/// shape on its own -- which it does whenever the pointer leaves a
/// surface, so that no client can strand its cursor on the desktop. The
/// module would then believe the hand is still in force and stay silent,
/// and the pointer would keep the arrow over a button.
///
/// Comparing against the event makes the whole thing self-correcting and
/// stateless: whatever happened in between, the shape converges within one
/// movement.
pub fn want_cursor(canvas: &mut Canvas, pointer: &PointerEvent, wanted: CursorShape) {
    if pointer.cursor_shape() != wanted {
        canvas.client.set_cursor(wanted);
    }
}

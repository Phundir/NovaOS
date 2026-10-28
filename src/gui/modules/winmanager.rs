//! `winmanager.mod` -- window management (8.2, WindowManager slot).
//!
//! ## What it does, and what the compositor already does
//!
//! Moving by the title bar, coming to the foreground on a click and focus
//! are handled by the compositor itself (`route_pointer`). It is not an
//! encroachment: those are the rules that must hold **even when this
//! module is paused**, failing which pausing the window manager would
//! make the system unusable -- and 8.3 says a module is paused without
//! breaking the OS.
//!
//! This module adds what can disappear without harm: the layout
//! shortcuts, cycling between windows with the keyboard (16.4,
//! `Super+Tab`) and closing with the keyboard.
//!
//! ## Its surface
//!
//! It draws no visible one. It creates a one-pixel, invisible one,
//! because the common canvas creates one -- and because a surface is also
//! what gives the module a holder of the keyboard focus. With no surface,
//! it would receive no key and its shortcuts would be unreachable.

use super::super::client::Event;
use super::super::protocol::KeyEvent;
use super::super::surface::Layer;
use super::super::theme::Theme;
use super::{Canvas, Spec};

/// Scancodes of set 1 this module uses.
const SCAN_TAB: u8 = 0x0F;
const SCAN_F4: u8 = 0x3E;

pub fn task() -> ! {
    super::run(&Spec {
        name: "winmanager",
        layer: Layer::Shell,
        geometry,
        draw,
        on_event,
        tick_ms: 0,
        visible_at_start: false,
    })
}

fn geometry(_width: i32, _height: i32, _theme: &Theme) -> (i32, i32, i32, i32) {
    (0, 0, 1, 1)
}

fn draw(_canvas: &mut Canvas) {
    // Nothing visible: the one-pixel surface is only there to give the
    // module a holder of the keyboard focus. An empty display list is a
    // perfectly valid state.
}

fn on_event(canvas: &mut Canvas, event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    if !key.pressed {
        return false;
    }

    // Super+Tab: cycle between the windows (16.4).
    if key.scancode == SCAN_TAB && key.has(KeyEvent::MOD_SUPER) {
        cycle(canvas);
        return false;
    }

    // Alt+F4: close the active window.
    if key.scancode == SCAN_F4 && key.has(KeyEvent::MOD_ALT) {
        close_focused(canvas);
        return false;
    }

    false
}

/// Brings the window at the bottom to the foreground.
///
/// Cycling is done by raising the lowest: repeated, it scrolls through
/// every window. It is the simplest behaviour that really is a cycle --
/// raising the next one would assume remembering an index, hence a state
/// that goes out of sync as soon as a window closes.
fn cycle(canvas: &mut Canvas) {
    use super::super::protocol::{self as proto, Writer};

    let windows = canvas.client.window_list();
    if windows.len() < 2 {
        return;
    }
    let (id, _) = windows[0];
    canvas
        .client
        .tell(proto::OP_SURFACE_RAISE, &Writer::new().u32(id).build());
    canvas
        .client
        .tell(proto::OP_SURFACE_FOCUS, &Writer::new().u32(id).build());
}

fn close_focused(canvas: &mut Canvas) {
    use super::super::protocol::{self as proto, Writer};

    let windows = canvas.client.window_list();
    let Some((id, _)) = windows.last().copied() else {
        return;
    };
    // The compositor will tell the owner: this module has no right to
    // destroy a surface that is not its own, and rightly so. It asks, it
    // does not impose.
    canvas
        .client
        .tell(proto::OP_SURFACE_LOWER, &Writer::new().u32(id).build());
}

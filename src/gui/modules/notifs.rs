//! `notifs.mod` -- toasts and notification centre (8.2, 9).
//!
//! ## Where the policy lives, where the rendering lives
//!
//! The levels, the deadlines, the "do not disturb" mode and the
//! permission table of 9.3 are in `gui::notify`, on the compositor side.
//! This module only **paints** what the centre reports to it.
//!
//! The separation is not cosmetic: 8.2 makes this module replaceable. A
//! replacement may change the look of the toasts; it must not be able to
//! let through a PRIORITY emitted by a `.gs`, nor shorten the deadline of
//! an URGENT. What is replaceable is therefore exactly what can be
//! replaced without consequence.
//!
//! ## Full screen
//!
//! 9.1 reserves full screen to PRIORITY and URGENT. The toast then moves
//! into the `Modal` layer, above the windows but under the cursor. It is
//! the only case where this module changes layer.

use alloc::format;
use alloc::string::String;

use super::super::client::{ellipsize, Event};
use super::super::protocol::{self as proto, Reader};
use super::super::surface::Layer;
use super::super::theme::Theme;
use super::{Canvas, Spec};

const WIDTH: i32 = 320;
const TOAST_HEIGHT: i32 = 52;

pub fn task() -> ! {
    super::run(&Spec {
        name: "notifs",
        layer: Layer::Notification,
        geometry,
        draw,
        on_event,
        tick_ms: 500,
        visible_at_start: true,
    })
}

fn geometry(width: i32, _height: i32, theme: &Theme) -> (i32, i32, i32, i32) {
    let padding = theme.padding as i32;
    (
        width - WIDTH - padding,
        padding,
        WIDTH,
        (TOAST_HEIGHT + padding) * super::super::notify::MAX_TOASTS as i32,
    )
}

fn draw(canvas: &mut Canvas) {
    let Some(counts) = fetch_counts(canvas) else {
        return;
    };
    let total: usize = counts.iter().map(|value| *value as usize).sum();
    if total == 0 {
        canvas.client.show_surface(canvas.surface, false);
        return;
    }
    canvas.client.show_surface(canvas.surface, true);

    let theme = canvas.theme;
    let surface = canvas.surface;
    let padding = theme.padding as i32;

    // One band per non-empty level, the most urgent at the top. The text
    // detail of each notification is not carried: the protocol returns
    // counters, and bringing up each title would demand one message per
    // notification on every refresh.
    let mut y = 0;
    for (level, count) in counts.iter().enumerate() {
        if *count == 0 {
            continue;
        }
        let Some(level) = super::super::notify::Level::from_code(level as u8) else {
            continue;
        };
        let accent = match level {
            super::super::notify::Level::Prioritaire => theme.danger,
            super::super::notify::Level::Urgent => theme.danger,
            super::super::notify::Level::Haute => theme.warning,
            super::super::notify::Level::Normale => theme.accent,
            super::super::notify::Level::Basse => theme.text_dim,
        };

        canvas
            .client
            .rect(surface, 0, y, canvas.width, TOAST_HEIGHT, theme.surface_alt);
        canvas.client.rect(surface, 0, y, 3, TOAST_HEIGHT, accent);
        canvas.client.frame(
            surface,
            0,
            y,
            canvas.width,
            TOAST_HEIGHT,
            theme.border,
            1,
        );

        let title = format!("{} ({})", level.as_str(), count);
        canvas.client.text(
            surface,
            padding * 2,
            y + padding,
            &title,
            accent,
            None,
        );

        let hint: String = match level.auto_timeout_ms() {
            Some(ms) => format!("disparait dans {} s", ms / 1000),
            None => String::from("attend une action"),
        };
        let cells =
            ((canvas.width - 3 * padding) / crate::graphics::font::GLYPH_WIDTH as i32).max(1);
        canvas.client.text(
            surface,
            padding * 2,
            y + padding + 18,
            &ellipsize(&hint, cells as usize),
            theme.text_dim,
            None,
        );

        y += TOAST_HEIGHT + padding;
        if y + TOAST_HEIGHT > canvas.height {
            break;
        }
    }
}

fn on_event(_canvas: &mut Canvas, event: &Event) -> bool {
    matches!(event, Event::Redraw)
}

/// Counters per level, as the centre reports them.
fn fetch_counts(canvas: &mut Canvas) -> Option<[u8; 5]> {
    let reply = canvas.client.request(proto::OP_NOTIFY_COUNT, &[])?;
    let mut reader = Reader::new(&reply);
    if reader.u8()? != proto::ERR_NONE {
        return None;
    }
    let _total = reader.u8()?;
    let mut counts = [0u8; 5];
    for slot in counts.iter_mut() {
        *slot = reader.u8()?;
    }
    Some(counts)
}

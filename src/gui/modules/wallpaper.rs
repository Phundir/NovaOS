//! `wallpaper.mod` -- wallpaper and desktop (8.2, Wallpaper slot).
//!
//! The background covers the whole screen in the lowest layer. There is
//! no background image: a 1024x768 file in 32 bits weighs three
//! mebibytes, and the protocol caps a message at 256 bytes. So the
//! gradient is computed -- which has the advantage of adapting to any
//! resolution with no resizing.
//!
//! The gradient is drawn in horizontal bands and not pixel by pixel: a
//! band is a rectangle, hence an optimised fill, and the eye does not
//! tell apart two shades a step of one in thirty-two apart.

use super::super::client::Event;
use super::super::surface::Layer;
use super::super::theme::Theme;
use super::{Canvas, Spec};

/// Number of bands of the gradient.
const BANDS: i32 = 32;

pub fn task() -> ! {
    super::run(&Spec {
        name: "wallpaper",
        layer: Layer::Wallpaper,
        geometry,
        draw,
        on_event,
        tick_ms: 0,
        visible_at_start: true,
    })
}

fn geometry(width: i32, height: i32, _theme: &Theme) -> (i32, i32, i32, i32) {
    (0, 0, width, height)
}

fn draw(canvas: &mut Canvas) {
    let theme = canvas.theme;
    let surface = canvas.surface;

    let band_height = (canvas.height / BANDS).max(1);
    let top = theme.background;
    let bottom = theme.surface;

    for band in 0..BANDS {
        let y = band * band_height;
        let height = if band == BANDS - 1 {
            canvas.height - y
        } else {
            band_height
        };
        let color = blend(top, bottom, band, BANDS - 1);
        canvas.client.rect(surface, 0, y, canvas.width, height, color);
    }

    // A discreet edge at the bottom of the screen: it gives a visual
    // landmark when the task bar is paused, and proves the gradient does
    // reach the last pixel.
    canvas.client.rect(
        surface,
        0,
        canvas.height - 1,
        canvas.width,
        1,
        theme.border,
    );

    let label = "Nova OS";
    let x = (canvas.width - crate::graphics::font::GLYPH_WIDTH as i32 * label.len() as i32) / 2;
    let y = canvas.height / 2 - crate::graphics::font::GLYPH_HEIGHT as i32;
    canvas
        .client
        .text(surface, x, y, label, theme.text_dim, None);
}

fn on_event(_canvas: &mut Canvas, event: &Event) -> bool {
    // The background reacts only to an explicit repaint request: a click
    // on the desktop belongs to the launcher, not to it.
    matches!(event, Event::Redraw)
}

/// Linear interpolation between two colours, in integer arithmetic.
///
/// Component by component: mixing the three bytes at once would overflow
/// from one channel into the next.
fn blend(from: u32, to: u32, step: i32, total: i32) -> u32 {
    let total = total.max(1) as u32;
    let step = step.clamp(0, total as i32) as u32;
    let mut result = 0u32;
    for shift in [16, 8, 0] {
        let a = (from >> shift) & 0xFF;
        let b = (to >> shift) & 0xFF;
        let value = (a * (total - step) + b * step) / total;
        result |= (value & 0xFF) << shift;
    }
    result
}

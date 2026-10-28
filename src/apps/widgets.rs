//! Controls shared by the audio applications: buttons, sliders, check
//! boxes, level meters.
//!
//! Each control is a rectangle and a drawing function; the application
//! keeps the rectangles in one layout, read by its drawing **and** its hit
//! test -- the rule entry 38 of DECISIONS.md learnt the hard way. Every
//! function states how many drawing commands it emits, because a window
//! holds `surface::MAX_COMMANDS` of them.
//!
//! Hover and state are drawn apart (entry 43): the hover lightens towards
//! white, the state uses the accent. A disabled control gets no hover --
//! a highlight on something that will not answer is the lie of entry 49.

use alloc::format;
use alloc::string::String;

use crate::graphics::font::{GLYPH_HEIGHT, GLYPH_WIDTH};
use crate::graphics::framebuffer::mix;
use crate::gui::surface::Rect;

use super::window::Ui;

const WHITE: u32 = 0xFF_FF_FF;

/// Vertical position of a line of text centred in `rect`.
pub fn text_y(rect: Rect) -> i32 {
    rect.y + (rect.height - GLYPH_HEIGHT as i32) / 2
}

/// Width of a text, in pixels.
pub fn text_width(text: &str) -> i32 {
    GLYPH_WIDTH as i32 * text.len() as i32
}

/// A button. 3 commands.
///
/// `on` draws it in the accent colour -- a toggle that is set, the record
/// button while recording.
pub fn button(ui: &mut Ui, rect: Rect, label: &str, enabled: bool, hovered: bool, on: bool) {
    let theme = ui.theme;
    let base = if on { theme.accent } else { theme.surface_alt };
    let fill = if enabled && hovered { mix(base, WHITE, 45) } else { base };
    ui.rect(rect.x, rect.y, rect.width, rect.height, fill);
    ui.frame(
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        if enabled && hovered { theme.text_dim } else { theme.border },
    );
    let color = match (enabled, on) {
        (false, _) => theme.text_dim,
        (true, true) => theme.background,
        (true, false) => theme.text,
    };
    ui.text(rect.x + (rect.width - text_width(label)) / 2, text_y(rect), label, color);
}

/// A horizontal slider: track, filled part, knob. 3 commands.
pub fn slider(ui: &mut Ui, rect: Rect, value: u32, max: u32, hovered: bool) {
    let theme = ui.theme;
    let track_y = rect.y + rect.height / 2 - 2;
    ui.rect(rect.x, track_y, rect.width, 4, theme.surface_alt);
    let filled = (rect.width as i64 * value.min(max) as i64 / max.max(1) as i64) as i32;
    ui.rect(rect.x, track_y, filled, 4, theme.accent);
    let knob = if hovered { mix(theme.accent, WHITE, 45) } else { theme.text };
    ui.rect(rect.x + filled - 3, rect.y + 2, 7, rect.height - 4, knob);
}

/// The value a slider takes for a pointer at `x`, clamped to its ends.
pub fn slider_value(rect: Rect, x: i32, max: u32) -> u32 {
    let offset = (x - rect.x).clamp(0, rect.width);
    (offset as i64 * max as i64 / rect.width.max(1) as i64) as u32
}

/// A check box and its label, the label to the right. 3 commands.
///
/// `rect` is the whole clickable area; the box is its left square.
pub fn checkbox(ui: &mut Ui, rect: Rect, label: &str, checked: bool, hovered: bool) {
    let theme = ui.theme;
    let size = 14;
    let box_y = rect.y + (rect.height - size) / 2;
    ui.frame(
        rect.x,
        box_y,
        size,
        size,
        if hovered { theme.text } else { theme.border },
    );
    ui.rect(
        rect.x + 3,
        box_y + 3,
        size - 6,
        size - 6,
        if checked { theme.accent } else { theme.background },
    );
    ui.text(rect.x + size + 8, text_y(rect), label, theme.text);
}

/// A level meter: background, level, frame. 3 commands.
///
/// `level` is a peak in 0..=32767. The scale is **logarithmic** -- 60 dB
/// over the width -- because a linear bar shows speech as a flicker in
/// the first tenth and music as a bar stuck at the right. The colour
/// warns: green, then the warning colour above -12 dB, then red above
/// -3 dB, where the next louder word clips.
pub fn meter(ui: &mut Ui, rect: Rect, level: u16) {
    let theme = ui.theme;
    ui.rect(rect.x, rect.y, rect.width, rect.height, theme.surface);
    let decibels = level_to_db(level);
    // -60 dB at the left edge, 0 dB at the right.
    let filled = ((decibels + 60).max(0) as i64 * rect.width as i64 / 60) as i32;
    let color = if decibels > -3 {
        theme.danger
    } else if decibels > -12 {
        theme.warning
    } else {
        theme.success
    };
    ui.rect(rect.x, rect.y, filled, rect.height, color);
    ui.frame(rect.x, rect.y, rect.width, rect.height, theme.border);
}

/// Peak level in decibels relative to full scale, rounded down, with no
/// logarithm: the number of halvings below 32767, times six.
///
/// Six decibels per halving is 6.02 in truth; the error, a third of a
/// decibel at the bottom of the scale, is below what a meter shows.
/// Within a halving the position is interpolated linearly, which is the
/// difference between a meter that moves and one that jumps by 6 dB.
pub fn level_to_db(level: u16) -> i32 {
    if level == 0 {
        return -96;
    }
    let mut value = level as i32;
    let mut halvings = 0;
    while value < 16384 && halvings < 16 {
        value *= 2;
        halvings += 1;
    }
    // `value` is now in 16384..=32767: the fraction of the last halving.
    let within = (value - 16384) * 6 / 16384;
    -6 * halvings - 6 + within
}

/// Milliseconds as `m:ss`.
pub fn clock(milliseconds: u64) -> String {
    let seconds = milliseconds / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// Pixel rectangles are built in so many places that the constructor is
/// re-exported.
pub const fn rect(x: i32, y: i32, width: i32, height: i32) -> Rect {
    Rect::new(x, y, width, height)
}

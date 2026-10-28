//! Painting into a pixel buffer: rectangles and text of the system font.
//!
//! The browser draws its pages here rather than as compositor commands
//! (see `gui::canvas` for why), so this is a small rasteriser of its own:
//! solid rectangles, and glyphs of the 8x16 font at scale 1 or 2, with
//! bold as a second pass shifted one pixel.

use super::layout::{Laid, Paint};

/// A clipped view of a pixel buffer.
pub struct Target<'a> {
    pub pixels: &'a mut [u32],
    pub width: i32,
    pub height: i32,
    /// Clip rectangle: (x0, y0, x1, y1), exclusive ends.
    pub clip: (i32, i32, i32, i32),
}

impl<'a> Target<'a> {
    pub fn new(pixels: &'a mut [u32], width: usize, height: usize) -> Target<'a> {
        Target {
            pixels,
            width: width as i32,
            height: height as i32,
            clip: (0, 0, width as i32, height as i32),
        }
    }

    pub fn set_clip(&mut self, x: i32, y: i32, w: i32, h: i32) {
        self.clip = (
            x.max(0),
            y.max(0),
            (x + w).min(self.width),
            (y + h).min(self.height),
        );
    }

    pub fn reset_clip(&mut self) {
        self.clip = (0, 0, self.width, self.height);
    }

    pub fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        let x0 = x.max(self.clip.0);
        let y0 = y.max(self.clip.1);
        let x1 = (x + w).min(self.clip.2);
        let y1 = (y + h).min(self.clip.3);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        for row in y0..y1 {
            let start = (row * self.width + x0) as usize;
            let end = (row * self.width + x1) as usize;
            self.pixels[start..end].fill(color);
        }
    }

    /// A rectangle with its corners rounded to `radius`: each row of a
    /// corner is inset by where a circle of that radius crosses it,
    /// computed with an integer square root (no floating point here).
    pub fn round_fill(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32, radius: i32) {
        let radius = radius.min(w / 2).min(h / 2).max(0);
        if radius == 0 {
            self.fill(x, y, w, h, color);
            return;
        }
        for row in 0..radius {
            let dy = radius - row;
            let inside = isqrt((radius * radius - dy * dy + dy).max(0));
            let inset = radius - inside;
            self.fill(x + inset, y + row, w - 2 * inset, 1, color);
            self.fill(x + inset, y + h - 1 - row, w - 2 * inset, 1, color);
        }
        self.fill(x, y + radius, w, h - 2 * radius, color);
    }

    pub fn frame(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        self.fill(x, y, w, 1, color);
        self.fill(x, y + h - 1, w, 1, color);
        self.fill(x, y, 1, h, color);
        self.fill(x + w - 1, y, 1, h, color);
    }

    #[inline]
    fn put(&mut self, x: i32, y: i32, color: u32) {
        if x >= self.clip.0 && x < self.clip.2 && y >= self.clip.1 && y < self.clip.3 {
            self.pixels[(y * self.width + x) as usize] = color;
        }
    }

    /// One glyph at `scale`.
    fn glyph(&mut self, x: i32, y: i32, byte: u8, color: u32, scale: i32) {
        if x + 8 * scale <= self.clip.0 || x >= self.clip.2 || y + 16 * scale <= self.clip.1 || y >= self.clip.3 {
            return;
        }
        let rows = crate::graphics::font::glyph(byte);
        for (line, bits) in rows.iter().enumerate() {
            if *bits == 0 {
                continue;
            }
            for column in 0..8 {
                if bits & (0x80 >> column) == 0 {
                    continue;
                }
                if scale == 1 {
                    self.put(x + column, y + line as i32, color);
                } else {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            self.put(x + column * scale + dx, y + line as i32 * scale + dy, color);
                        }
                    }
                }
            }
        }
    }

    /// Text of printable ASCII; anything else is drawn as `?`.
    pub fn text(&mut self, x: i32, y: i32, text: &str, color: u32, scale: i32, bold: bool) -> i32 {
        let scale = scale.clamp(1, 4);
        let mut cursor = x;
        for byte in text.bytes() {
            if cursor >= self.clip.2 {
                break;
            }
            let byte = if (0x20..0x7f).contains(&byte) { byte } else { b'?' };
            if byte != b' ' {
                self.glyph(cursor, y, byte, color, scale);
                if bold {
                    self.glyph(cursor + 1, y, byte, color, scale);
                }
            }
            cursor += 8 * scale;
        }
        cursor - x
    }
}

/// Integer square root, by Newton's method.
fn isqrt(value: i32) -> i32 {
    if value < 2 {
        return value.max(0);
    }
    let mut guess = value;
    let mut next = (guess + 1) / 2;
    while next < guess {
        guess = next;
        next = (guess + value / guess) / 2;
    }
    guess
}

/// Draws the part of a laid-out page that falls in the viewport:
/// `(left, top)` is where the page's origin lands in the target, `scroll`
/// how far down the page the viewport starts. Controls are left to the
/// caller, which knows their state; it gets their indices back.
pub fn page(target: &mut Target, laid: &Laid, left: i32, top: i32, scroll: i32, visible_height: i32) -> alloc::vec::Vec<usize> {
    let mut controls = alloc::vec::Vec::new();
    let bottom = scroll + visible_height;
    // `overflow: hidden` boxes nest: each clip is the intersection of its
    // rectangle with the one in force, restored at its `Unclip`.
    let outer = target.clip;
    let mut clips: alloc::vec::Vec<(i32, i32, i32, i32)> = alloc::vec::Vec::new();
    for paint in &laid.page.paints {
        match paint {
            Paint::Rect { x, y, w, h, color } => {
                if *w <= 0 || *h <= 0 || y + h < scroll || *y > bottom {
                    continue;
                }
                target.fill(left + x, top + y - scroll, *w, *h, *color);
            }
            Paint::Text {
                x,
                y,
                text,
                color,
                bold,
                scale,
                underline,
                strike,
            } => {
                let height = 16 * scale;
                if y + height < scroll || *y > bottom {
                    continue;
                }
                let screen_y = top + y - scroll;
                let width = target.text(left + x, screen_y, text, *color, *scale, *bold);
                if *underline {
                    target.fill(left + x, screen_y + 15 * scale, width, *scale, *color);
                }
                if *strike {
                    target.fill(left + x, screen_y + 8 * scale, width, *scale, *color);
                }
            }
            Paint::Image { x, y, w, h, alt } => {
                if y + h < scroll || *y > bottom {
                    continue;
                }
                let screen_y = top + y - scroll;
                target.fill(left + x, screen_y, *w, *h, 0xe8e8e8);
                target.frame(left + x, screen_y, *w, *h, 0xb0b0b0);
                if !alt.is_empty() && *h >= 16 && *w >= 24 {
                    let fits = ((w - 8) / 8).max(0) as usize;
                    let shown: alloc::string::String = alt.chars().take(fits).collect();
                    target.text(left + x + 4, screen_y + (h - 16) / 2, &shown, 0x606060, 1, false);
                }
            }
            Paint::RoundRect { x, y, w, h, color, radius } => {
                if *w <= 0 || *h <= 0 || y + h < scroll || *y > bottom {
                    continue;
                }
                target.round_fill(left + x, top + y - scroll, *w, *h, *color, *radius);
            }
            Paint::Clip { x, y, w, h } => {
                clips.push(target.clip);
                let x0 = (left + x).max(target.clip.0);
                let y0 = (top + y - scroll).max(target.clip.1);
                let x1 = (left + x + w).min(target.clip.2);
                let y1 = (top + y - scroll + h).min(target.clip.3);
                target.clip = (x0, y0, x1.max(x0), y1.max(y0));
            }
            Paint::Unclip => target.clip = clips.pop().unwrap_or(outer),
            Paint::Control(index) => controls.push(*index),
        }
    }
    target.clip = outer;
    controls
}

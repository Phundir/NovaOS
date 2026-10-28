//! Text console rendered on the framebuffer.
//!
//! ## Why it exists from phase 7.1 onwards
//!
//! Leaving VGA text mode cuts the only screen output of the kernel: the
//! buffer at 0xb8000 is no longer displayed, and everything `kprintln!`
//! produces disappears from the screen. Rendering the same lines on the
//! framebuffer is therefore not an advance on the compositor, it is what
//! stops step 7.1 from breaking everything before it.
//!
//! What this console is not: the text rendering of the system. 8.1
//! reserves writing on the framebuffer to the compositor, which will have
//! its fonts, its antialiasing and its windows. This one is the
//! diagnostic console of the kernel -- the graphical equivalent of the
//! VGA writer, with the same sixteen-colour palette and the same limits.
//!
//! ## Scrolling
//!
//! The VGA writer always writes on the last line and moves the other
//! twenty-four up on every newline. This console moves a cursor down and
//! only shifts the image when it passes the bottom. The visible result is
//! the same once the screen is full, for a forty-eighth of the work.
//!
//! ## Publishing
//!
//! Publishing after every character would copy the whole image per
//! character. So the console records the band of lines it has dirtied and
//! publishes it only at the end of a `print!`, once the line is complete.

use super::framebuffer::{Color, Framebuffer};
use super::font;

/// Margin left on the four edges, in pixels.
///
/// Text stuck to the edge of a screen is unpleasant to read, and on a
/// real monitor the first columns sometimes fall outside the visible
/// area.
const MARGIN: usize = 4;

pub struct Console {
    columns: usize,
    rows: usize,
    column: usize,
    row: usize,
    foreground: Color,
    background: Color,
    /// Band dirtied since the last publication, in pixel rows.
    /// `dirty_end == 0` means "nothing to publish".
    dirty_start: usize,
    dirty_end: usize,
}

impl Console {
    pub fn new(fb: &Framebuffer) -> Self {
        let usable_width = fb.width().saturating_sub(2 * MARGIN);
        let usable_height = fb.height().saturating_sub(2 * MARGIN);

        Console {
            columns: (usable_width / font::GLYPH_WIDTH).max(1),
            rows: (usable_height / font::GLYPH_HEIGHT).max(1),
            column: 0,
            row: 0,
            foreground: super::palette::DEFAULT_FOREGROUND,
            background: super::palette::DEFAULT_BACKGROUND,
            dirty_start: 0,
            dirty_end: 0,
        }
    }

    pub fn columns(&self) -> usize {
        self.columns
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn set_color(&mut self, foreground: Color, background: Color) {
        self.foreground = foreground;
        self.background = background;
    }

    pub fn background(&self) -> Color {
        self.background
    }

    fn mark_dirty(&mut self, first: usize, count: usize) {
        let end = first + count;
        if self.dirty_end == 0 {
            self.dirty_start = first;
            self.dirty_end = end;
        } else {
            self.dirty_start = self.dirty_start.min(first);
            self.dirty_end = self.dirty_end.max(end);
        }
    }

    /// Upper left corner, in pixels, of the current cell.
    fn origin(&self) -> (usize, usize) {
        (
            MARGIN + self.column * font::GLYPH_WIDTH,
            MARGIN + self.row * font::GLYPH_HEIGHT,
        )
    }

    pub fn clear(&mut self, fb: &mut Framebuffer) {
        fb.clear(self.background);
        self.column = 0;
        self.row = 0;
        self.mark_dirty(0, fb.height());
    }

    pub fn write_byte(&mut self, fb: &mut Framebuffer, byte: u8) {
        match byte {
            b'\n' => self.new_line(fb),
            b'\r' => self.column = 0,
            b'\t' => {
                // Tab at eight columns, without passing the line.
                let stop = (self.column / 8 + 1) * 8;
                while self.column < stop.min(self.columns) {
                    self.write_byte(fb, b' ');
                }
            }
            byte => {
                if self.column >= self.columns {
                    self.new_line(fb);
                }
                let (x, y) = self.origin();
                fb.draw_glyph(x, y, font::glyph(byte), self.foreground, self.background);
                self.mark_dirty(y, font::GLYPH_HEIGHT);
                self.column += 1;
            }
        }
    }

    /// Same convention as the VGA writer: any byte outside the printable
    /// range becomes a solid square, which makes a stray byte visible
    /// instead of letting it disturb the rendering.
    pub fn write_str(&mut self, fb: &mut Framebuffer, text: &str) {
        for byte in text.bytes() {
            match byte {
                0x20..=0x7e | b'\n' | b'\r' | b'\t' => self.write_byte(fb, byte),
                _ => self.write_byte(fb, 0xfe),
            }
        }
    }

    fn new_line(&mut self, fb: &mut Framebuffer) {
        self.column = 0;
        if self.row + 1 < self.rows {
            self.row += 1;
            return;
        }

        // The cursor is at the bottom: the image moves up one text line
        // and the cursor stays where it is. The whole screen has changed.
        fb.scroll_up(font::GLYPH_HEIGHT, self.background);
        self.mark_dirty(0, fb.height());
    }

    /// Publishes the dirtied band and starts again from a clean page.
    pub fn flush(&mut self, fb: &mut Framebuffer) {
        if self.dirty_end == 0 {
            return;
        }
        fb.present_rows(self.dirty_start, self.dirty_end - self.dirty_start);
        self.dirty_start = 0;
        self.dirty_end = 0;
    }
}

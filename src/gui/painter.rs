//! Execution of a display list on the framebuffer.
//!
//! ## Where the boundary runs
//!
//! `graphics::framebuffer` can lay down a pixel and fill a rectangle, in
//! screen coordinates and clipping nothing but the edge of the screen.
//! This module adds the one thing the compositor lacks: a **clip
//! rectangle**, and the translation that goes with it.
//!
//! That is what makes a surface watertight. An application draws in its
//! own frame, starts from (0, 0), and has no way of reaching a pixel that
//! does not belong to it -- not by convention, but because every
//! coordinate goes through `Painter`, which brings it back into the area
//! before calling the framebuffer.
//!
//! 8.1 says the compositor is the only one to write on the framebuffer.
//! `Painter` is the exact place where that rule becomes checkable: all
//! the rendering of the graphics system goes through it.
//!
//! ## Three targets, one painter (phase 13.8)
//!
//! What a painter paints on is a `Target`: the framebuffer, a plain image
//! in memory (`Image`: the cursor plane, the self-test's reference), or the
//! GPU compositor's list of primitives (`gui::gpu`). The clipping and the
//! translation stay here, once, for all three -- which is what makes a
//! frame composed on the GPU the same, pixel for pixel, as the one the
//! processor paints. A target only has to lay down what it is given, where
//! it is given.

use alloc::vec::Vec;

use crate::graphics::font;
use crate::graphics::framebuffer::{Color, Framebuffer};

use super::canvas::{CanvasRef, Pixels};
use super::surface::{Command, Rect};

// ============================================================
//  Targets
// ============================================================

/// What a painter lays pixels on. Coordinates are the target's own, and
/// always inside it: the painter clipped them.
pub trait Target {
    fn width(&self) -> usize;
    fn height(&self) -> usize;
    fn fill_rect(&mut self, x: usize, y: usize, width: usize, height: usize, color: Color);
    fn put_pixel(&mut self, x: usize, y: usize, color: Color);

    /// The glyph of `code` in the system font, its cell at `(x, y)` --
    /// which may run past `clip` -- drawn only inside `clip`.
    fn glyph(&mut self, x: i32, y: i32, code: u8, color: Color, background: Option<Color>, clip: Rect) {
        let glyph = font::glyph(code);
        for (line, bits) in glyph.iter().enumerate() {
            for column in 0..font::GLYPH_WIDTH {
                let lit = bits & (0x80 >> column) != 0;
                let (px, py) = (x + column as i32, y + line as i32);
                if !clip.contains(px, py) {
                    continue;
                }
                match (lit, background) {
                    (true, _) => self.put_pixel(px as usize, py as usize, color),
                    (false, Some(background)) => self.put_pixel(px as usize, py as usize, background),
                    (false, None) => {}
                }
            }
        }
    }

    /// A monochrome 16 x 16 icon at `(x, y)`, drawn only inside `clip`.
    fn icon(&mut self, x: i32, y: i32, bits: &[u8; 32], color: Color, background: Option<Color>, clip: Rect) {
        for row in 0..16 {
            let pattern = u16::from_be_bytes([bits[row * 2], bits[row * 2 + 1]]);
            for column in 0..16 {
                let lit = pattern & (0x8000 >> column) != 0;
                let (px, py) = (x + column, y + row as i32);
                if !clip.contains(px, py) {
                    continue;
                }
                match (lit, background) {
                    (true, _) => self.put_pixel(px as usize, py as usize, color),
                    (false, Some(background)) => self.put_pixel(px as usize, py as usize, background),
                    (false, None) => {}
                }
            }
        }
    }

    /// The part `visible` of a canvas whose top-left corner is at `area`'s.
    /// `pixels` is the canvas, locked by the caller.
    fn canvas(&mut self, area: Rect, visible: Rect, canvas: &CanvasRef, pixels: &Pixels);
}

impl Target for Framebuffer {
    fn width(&self) -> usize {
        Framebuffer::width(self)
    }

    fn height(&self) -> usize {
        Framebuffer::height(self)
    }

    fn fill_rect(&mut self, x: usize, y: usize, width: usize, height: usize, color: Color) {
        Framebuffer::fill_rect(self, x, y, width, height, color);
    }

    fn put_pixel(&mut self, x: usize, y: usize, color: Color) {
        Framebuffer::put_pixel(self, x, y, color);
    }

    fn glyph(&mut self, x: i32, y: i32, code: u8, color: Color, background: Option<Color>, clip: Rect) {
        // Fast case: entirely inside the area, opaque background. The
        // framebuffer paints a complete glyph in one go.
        let cell = Rect::new(x, y, font::GLYPH_WIDTH as i32, font::GLYPH_HEIGHT as i32);
        if let Some(background) = background
            && clip.intersect(&cell) == cell
        {
            self.draw_glyph(x as usize, y as usize, font::glyph(code), color, background);
            return;
        }
        let glyph = font::glyph(code);
        for (line, bits) in glyph.iter().enumerate() {
            for column in 0..font::GLYPH_WIDTH {
                let lit = bits & (0x80 >> column) != 0;
                let (px, py) = (x + column as i32, y + line as i32);
                if !clip.contains(px, py) {
                    continue;
                }
                match (lit, background) {
                    (true, _) => self.put_pixel(px as usize, py as usize, color),
                    (false, Some(background)) => self.put_pixel(px as usize, py as usize, background),
                    (false, None) => {}
                }
            }
        }
    }

    fn canvas(&mut self, area: Rect, visible: Rect, _canvas: &CanvasRef, pixels: &Pixels) {
        let left = (visible.x - area.x) as usize;
        let columns = visible.width as usize;
        for row in 0..visible.height {
            let source_row = (visible.y - area.y + row) as usize;
            let start = source_row * pixels.width + left;
            if let Some(line) = pixels.data.get(start..start + columns) {
                self.write_row(visible.x as usize, (visible.y + row) as usize, line);
            }
        }
    }
}

/// A plain image in memory, `0x00RRGGBB` per pixel -- or, with `alpha`,
/// `0xFFRRGGBB` for every pixel painted and 0 elsewhere (the cursor
/// plane's image).
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
    alpha: bool,
}

impl Image {
    pub fn new(width: usize, height: usize, alpha: bool) -> Image {
        Image { width, height, pixels: alloc::vec![0; width * height], alpha }
    }

    fn stamp(&self, color: Color) -> u32 {
        if self.alpha { color | 0xFF00_0000 } else { color & 0x00FF_FFFF }
    }
}

impl Target for Image {
    fn width(&self) -> usize {
        self.width
    }

    fn height(&self) -> usize {
        self.height
    }

    fn fill_rect(&mut self, x: usize, y: usize, width: usize, height: usize, color: Color) {
        let value = self.stamp(color);
        for row in y..(y + height).min(self.height) {
            let start = row * self.width + x;
            let end = row * self.width + (x + width).min(self.width);
            if start < end {
                self.pixels[start..end].fill(value);
            }
        }
    }

    fn put_pixel(&mut self, x: usize, y: usize, color: Color) {
        if x < self.width && y < self.height {
            self.pixels[y * self.width + x] = self.stamp(color);
        }
    }

    fn canvas(&mut self, area: Rect, visible: Rect, _canvas: &CanvasRef, pixels: &Pixels) {
        let left = (visible.x - area.x) as usize;
        for row in 0..visible.height {
            let source_row = (visible.y - area.y + row) as usize;
            for column in 0..visible.width as usize {
                if let Some(color) = pixels.data.get(source_row * pixels.width + left + column) {
                    self.put_pixel(visible.x as usize + column, (visible.y + row) as usize, *color);
                }
            }
        }
    }
}

/// Pixels in memory the caller lends, `0x00RRGGBB`, rows packed: the GPU
/// compositor's output, painted by the processor while the GPU is busy
/// (`gui::gpu::drive`).
pub struct Raw {
    base: *mut u32,
    width: usize,
    height: usize,
}

impl Raw {
    /// # Safety
    /// `address` must be writable for `width * height` pixels for as long
    /// as the `Raw` lives, and nothing else may write there meanwhile.
    pub unsafe fn new(address: u64, width: usize, height: usize) -> Raw {
        Raw { base: address as *mut u32, width, height }
    }
}

impl Target for Raw {
    fn width(&self) -> usize {
        self.width
    }

    fn height(&self) -> usize {
        self.height
    }

    fn fill_rect(&mut self, x: usize, y: usize, width: usize, height: usize, color: Color) {
        let x_end = (x + width).min(self.width);
        for row in y..(y + height).min(self.height) {
            for column in x..x_end {
                // SAFETY: inside the image `new` was promised.
                unsafe { self.base.add(row * self.width + column).write_volatile(color & 0x00FF_FFFF) };
            }
        }
    }

    fn put_pixel(&mut self, x: usize, y: usize, color: Color) {
        if x < self.width && y < self.height {
            // SAFETY: inside the image.
            unsafe { self.base.add(y * self.width + x).write_volatile(color & 0x00FF_FFFF) };
        }
    }

    fn canvas(&mut self, area: Rect, visible: Rect, _canvas: &CanvasRef, pixels: &Pixels) {
        let left = (visible.x - area.x) as usize;
        let columns = visible.width as usize;
        for row in 0..visible.height {
            let source_row = (visible.y - area.y + row) as usize;
            let start = source_row * pixels.width + left;
            let (y, x) = ((visible.y + row) as usize, visible.x as usize);
            if let Some(line) = pixels.data.get(start..start + columns)
                && y < self.height
                && x + columns <= self.width
            {
                // SAFETY: the row lies inside the image, checked above.
                unsafe { core::ptr::copy_nonoverlapping(line.as_ptr(), self.base.add(y * self.width + x), columns) };
            }
        }
    }
}

// ============================================================
//  The painter
// ============================================================

/// A drawing area: an origin and a clip rectangle, both in screen
/// coordinates.
pub struct Painter<'a, T: Target + ?Sized = Framebuffer> {
    fb: &'a mut T,
    origin_x: i32,
    origin_y: i32,
    clip: Rect,
    /// **Logical** size of the area, before clipping.
    ///
    /// Distinct from the clip rectangle: a window half off the screen
    /// keeps its size, and a `Fill` command must cover the window, not
    /// only its visible part. Confusing the two fills wrongly as soon as
    /// the window touches an edge.
    width: i32,
    height: i32,
}

impl<'a, T: Target + ?Sized> Painter<'a, T> {
    /// Area covering the whole screen, with no translation.
    pub fn screen(fb: &'a mut T) -> Self {
        let clip = Rect::new(0, 0, fb.width() as i32, fb.height() as i32);
        Painter {
            fb,
            origin_x: 0,
            origin_y: 0,
            clip,
            width: clip.width,
            height: clip.height,
        }
    }

    /// Restricts the area to a sub-rectangle and places the origin in it.
    ///
    /// The clipping **composes**: restricting twice gives the
    /// intersection, never the union. A surface therefore cannot grow by
    /// creating a sub-area.
    pub fn region(&mut self, rect: Rect) -> Painter<'_, T> {
        let absolute = rect.offset(self.origin_x, self.origin_y);
        Painter {
            fb: self.fb,
            origin_x: absolute.x,
            origin_y: absolute.y,
            clip: self.clip.intersect(&absolute),
            width: rect.width,
            height: rect.height,
        }
    }

    /// Logical size of the area, clipping not included.
    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    pub fn clip(&self) -> Rect {
        self.clip
    }

    pub fn is_empty(&self) -> bool {
        self.clip.is_empty()
    }

    /// Filled rectangle, in local coordinates.
    pub fn fill(&mut self, rect: Rect, color: Color) {
        let visible = rect.offset(self.origin_x, self.origin_y).intersect(&self.clip);
        if visible.is_empty() {
            return;
        }
        self.fb.fill_rect(
            visible.x as usize,
            visible.y as usize,
            visible.width as usize,
            visible.height as usize,
            color,
        );
    }

    /// Hollow frame. Four rectangles rather than a pixel loop: the fill
    /// is already optimised, so it may as well be used.
    pub fn frame(&mut self, rect: Rect, color: Color, thickness: i32) {
        let thickness = thickness.clamp(1, rect.width.min(rect.height).max(1));
        self.fill(Rect::new(rect.x, rect.y, rect.width, thickness), color);
        self.fill(
            Rect::new(rect.x, rect.bottom() - thickness, rect.width, thickness),
            color,
        );
        self.fill(Rect::new(rect.x, rect.y, thickness, rect.height), color);
        self.fill(
            Rect::new(rect.right() - thickness, rect.y, thickness, rect.height),
            color,
        );
    }

    /// 7.5.2 -- Rectangle with rounded corners.
    ///
    /// The rounding is obtained by removing a staircase at each corner,
    /// computed from the circle equation alone in integer arithmetic:
    /// `dx*dx + dy*dy <= r*r`. No trigonometry, hence no floats -- which
    /// the soft-float target forbids anyway.
    ///
    /// The result is aliased. Antialiasing assumes blending with what is
    /// underneath, hence reading the framebuffer back, which a
    /// write-combining mapping makes slow. It will come with a
    /// composition layer that keeps its pixels in RAM.
    pub fn rounded(&mut self, rect: Rect, color: Color, radius: i32) {
        let radius = radius.min(rect.width / 2).min(rect.height / 2).max(0);
        if radius == 0 {
            self.fill(rect, color);
            return;
        }

        // Central band: full height, without the corners.
        self.fill(
            Rect::new(rect.x, rect.y + radius, rect.width, rect.height - 2 * radius),
            color,
        );

        for row in 0..radius {
            let dy = radius - 1 - row;
            let mut inset = radius;
            while inset > 0 {
                let dx = radius - inset;
                if dx * dx + dy * dy <= radius * radius {
                    break;
                }
                inset -= 1;
            }
            let width = rect.width - 2 * (radius - inset);
            if width <= 0 {
                continue;
            }
            let x = rect.x + (radius - inset);
            self.fill(Rect::new(x, rect.y + row, width, 1), color);
            self.fill(Rect::new(x, rect.bottom() - 1 - row, width, 1), color);
        }
    }

    /// Segment, by Bresenham's algorithm.
    ///
    /// Entirely integer: no slope is computed, only an accumulated error.
    /// That is what allows drawing with no floats on a target that has no
    /// SSE.
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: Color) {
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut error = dx + dy;
        let (mut x, mut y) = (x0, y0);

        loop {
            self.pixel(x, y, color);
            if x == x1 && y == y1 {
                break;
            }
            let doubled = 2 * error;
            if doubled >= dy {
                error += dy;
                x += sx;
            }
            if doubled <= dx {
                error += dx;
                y += sy;
            }
        }
    }

    pub fn pixel(&mut self, x: i32, y: i32, color: Color) {
        let (sx, sy) = (x + self.origin_x, y + self.origin_y);
        if !self.clip.contains(sx, sy) {
            return;
        }
        self.fb.put_pixel(sx as usize, sy as usize, color);
    }

    /// Text in the system font. Returns the width consumed.
    ///
    /// A `None` background lets what is underneath show, which a title
    /// bar needs; a `Some` background is much faster, the glyph and its
    /// background being painted in one pass.
    pub fn text(&mut self, x: i32, y: i32, text: &str, color: Color, background: Option<Color>) -> i32 {
        let mut cursor = x;
        for byte in text.bytes() {
            let byte = if (0x20..=0x7e).contains(&byte) { byte } else { 0xfe };
            let cell = Rect::new(cursor + self.origin_x, y + self.origin_y, font::GLYPH_WIDTH as i32, font::GLYPH_HEIGHT as i32);
            if !self.clip.intersect(&cell).is_empty() {
                self.fb.glyph(cell.x, cell.y, byte, color, background, self.clip);
            }
            cursor += font::GLYPH_WIDTH as i32;
        }
        cursor - x
    }

    /// Width a text would occupy, without drawing it.
    pub fn text_width(text: &str) -> i32 {
        (text.len() * font::GLYPH_WIDTH) as i32
    }

    /// Monochrome 16x16 icon: one bit per pixel, two bytes per row.
    pub fn icon(&mut self, x: i32, y: i32, bits: &[u8; 32], color: Color, background: Option<Color>) {
        let cell = Rect::new(x + self.origin_x, y + self.origin_y, 16, 16);
        if !self.clip.intersect(&cell).is_empty() {
            self.fb.icon(cell.x, cell.y, bits, color, background, self.clip);
        }
    }

    /// Copies a client canvas, clipped. The canvas lock is taken for the
    /// time of the copy; the client only ever holds it for a pointer swap.
    pub fn canvas(&mut self, x: i32, y: i32, canvas: &CanvasRef) {
        let mut pixels = canvas.0.lock();
        let stamp = pixels.published_ns;
        pixels.pacing.shown(stamp, crate::posix::time::monotonic_ns());
        let area = Rect::new(x, y, pixels.width as i32, pixels.height as i32)
            .offset(self.origin_x, self.origin_y);
        let visible = area.intersect(&self.clip);
        if visible.is_empty() {
            return;
        }
        self.fb.canvas(area, visible, canvas, &pixels);
    }

    /// Replays a display list. The coordinates of the commands are local
    /// to the surface, hence to this area.
    pub fn replay(&mut self, commands: &[Command]) {
        if self.is_empty() {
            return;
        }
        let full = self.bounds();
        for command in commands {
            match command {
                Command::Fill { color } => self.fill(full, *color),
                Command::Rect { rect, color } => self.fill(*rect, *color),
                Command::Frame {
                    rect,
                    color,
                    thickness,
                } => self.frame(*rect, *color, *thickness),
                Command::Line {
                    x0,
                    y0,
                    x1,
                    y1,
                    color,
                } => self.line(*x0, *y0, *x1, *y1, *color),
                Command::Text {
                    x,
                    y,
                    color,
                    background,
                    text,
                } => {
                    self.text(*x, *y, text, *color, *background);
                }
                Command::Icon {
                    x,
                    y,
                    bits,
                    color,
                    background,
                } => self.icon(*x, *y, bits, *color, *background),
                Command::Canvas { x, y, canvas } => self.canvas(*x, *y, canvas),
            }
        }
    }
}

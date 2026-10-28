//! The painter's target on the GPU: what the processor would paint,
//! recorded as a list of instances for `compose.vert` / `compose.frag`.
//!
//! ## One instance, eight words
//!
//! ```text
//!   0  x | y << 16          rectangle, signed 16-bit halves
//!   1  width | height << 16
//!   2  clip x | y << 16     the painter's clip, signed
//!   3  clip width | height << 16
//!   4  colour               0x00RRGGBB
//!   5  background           0x00RRGGBB, bit 24 set when opaque
//!   6  kind | index << 8    0 fill, 1 glyph (index: the code), 2 icon
//!                           (index: its atlas slot), 3 canvas
//!   7  source x | y << 16   where the rectangle starts in the canvas
//! ```
//!
//! Every coordinate is a whole pixel on screen, already translated by the
//! painter. The clip is the painter's own: a glyph cut by a window edge is
//! one instance whose fragments outside the clip are discarded, exactly
//! where the processor would have skipped the pixels.
//!
//! ## Batches
//!
//! The instances are drawn in the order they were recorded -- the
//! painter's order, which is the z-order. A canvas is a texture: the draw
//! binds one at a time, so a change of canvas starts a new batch. Nothing
//! else does.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::graphics::framebuffer::Color;
use crate::gui::canvas::{CanvasRef, Pixels};
use crate::gui::painter::Target;
use crate::gui::surface::Rect;

pub const KIND_FILL: u32 = 0;
pub const KIND_GLYPH: u32 = 1;
pub const KIND_ICON: u32 = 2;
pub const KIND_CANVAS: u32 = 3;

/// Bit of the background word: the background is painted.
const OPAQUE: u32 = 1 << 24;

/// Instances one draw may carry (`cmd::MAX_INSTANCES`).
pub const BATCH_LIMIT: u32 = 1 << 16;

/// Icon slots of the atlas: 64 x 64 cells of 16 x 16.
pub const ICON_SLOTS: u32 = 64 * 64;

pub type Instance = [u32; 8];

/// A run of instances drawn with one canvas bound (or none).
#[derive(Debug, Clone, Copy)]
pub struct Batch {
    pub first: u32,
    pub count: u32,
    /// Index into `Recorder::canvases`.
    pub canvas: Option<usize>,
}

/// The icons the atlas holds, by their bits.
#[derive(Default)]
pub struct IconCache {
    slots: BTreeMap<[u8; 32], u32>,
    /// Icons given a slot and not yet copied into the atlas.
    pub pending: Vec<(u32, [u8; 32])>,
}

impl IconCache {
    fn slot(&mut self, bits: &[u8; 32]) -> u32 {
        if let Some(slot) = self.slots.get(bits) {
            return *slot;
        }
        let slot = self.slots.len() as u32 % ICON_SLOTS;
        self.slots.insert(*bits, slot);
        self.pending.push((slot, *bits));
        slot
    }

    /// Starts the atlas afresh when it is nearly full. Called between two
    /// frames only: a slot reused inside a frame would change an icon
    /// already recorded.
    pub fn trim(&mut self) {
        if self.slots.len() as u32 >= ICON_SLOTS - 256 {
            self.slots.clear();
            self.pending.clear();
        }
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }
}

pub struct Recorder<'a> {
    width: usize,
    height: usize,
    pub instances: Vec<Instance>,
    pub batches: Vec<Batch>,
    /// Canvases the frame shows, each once.
    pub canvases: Vec<CanvasRef>,
    icons: &'a mut IconCache,
}

fn pair(x: i32, y: i32) -> u32 {
    (x as i16 as u16 as u32) | ((y as i16 as u16 as u32) << 16)
}

fn size(width: i32, height: i32) -> u32 {
    (width.clamp(0, 0xFFFF) as u32) | ((height.clamp(0, 0xFFFF) as u32) << 16)
}

impl<'a> Recorder<'a> {
    pub fn new(width: usize, height: usize, icons: &'a mut IconCache) -> Recorder<'a> {
        Recorder { width, height, instances: Vec::new(), batches: Vec::new(), canvases: Vec::new(), icons }
    }

    fn push(&mut self, instance: Instance, canvas: Option<usize>) {
        let index = self.instances.len() as u32;
        self.instances.push(instance);
        if let Some(last) = self.batches.last_mut()
            && last.count < BATCH_LIMIT
        {
            match (last.canvas, canvas) {
                (_, None) => {
                    last.count += 1;
                    return;
                }
                (None, Some(new)) => {
                    last.canvas = Some(new);
                    last.count += 1;
                    return;
                }
                (Some(bound), Some(new)) if bound == new => {
                    last.count += 1;
                    return;
                }
                _ => {}
            }
        }
        self.batches.push(Batch { first: index, count: 1, canvas });
    }

    fn shape(&mut self, kind: u32, index: u32, cell: Rect, color: Color, background: Option<Color>, clip: Rect) {
        let clip = clip.intersect(&cell);
        if clip.is_empty() {
            return;
        }
        let background = background.map_or(0, |b| (b & 0x00FF_FFFF) | OPAQUE);
        self.push(
            [
                pair(cell.x, cell.y),
                size(cell.width, cell.height),
                pair(clip.x, clip.y),
                size(clip.width, clip.height),
                color & 0x00FF_FFFF,
                background,
                kind | index << 8,
                0,
            ],
            None,
        );
    }
}

impl Target for Recorder<'_> {
    fn width(&self) -> usize {
        self.width
    }

    fn height(&self) -> usize {
        self.height
    }

    fn fill_rect(&mut self, x: usize, y: usize, width: usize, height: usize, color: Color) {
        let rect = Rect::new(x as i32, y as i32, width as i32, height as i32);
        let screen = Rect::new(0, 0, self.width as i32, self.height as i32);
        let rect = rect.intersect(&screen);
        if rect.is_empty() {
            return;
        }
        let at = pair(rect.x, rect.y);
        let extent = size(rect.width, rect.height);
        self.push([at, extent, at, extent, color & 0x00FF_FFFF, 0, KIND_FILL, 0], None);
    }

    fn put_pixel(&mut self, x: usize, y: usize, color: Color) {
        self.fill_rect(x, y, 1, 1, color);
    }

    fn glyph(&mut self, x: i32, y: i32, code: u8, color: Color, background: Option<Color>, clip: Rect) {
        let cell = Rect::new(x, y, crate::graphics::font::GLYPH_WIDTH as i32, crate::graphics::font::GLYPH_HEIGHT as i32);
        self.shape(KIND_GLYPH, code as u32, cell, color, background, clip);
    }

    fn icon(&mut self, x: i32, y: i32, bits: &[u8; 32], color: Color, background: Option<Color>, clip: Rect) {
        let slot = self.icons.slot(bits);
        self.shape(KIND_ICON, slot, Rect::new(x, y, 16, 16), color, background, clip);
    }

    fn canvas(&mut self, area: Rect, visible: Rect, canvas: &CanvasRef, _pixels: &Pixels) {
        let index = match self.canvases.iter().position(|known| alloc::sync::Arc::ptr_eq(&known.0, &canvas.0)) {
            Some(index) => index,
            None => {
                self.canvases.push(canvas.clone());
                self.canvases.len() - 1
            }
        };
        let at = pair(visible.x, visible.y);
        let extent = size(visible.width, visible.height);
        let source = pair(visible.x - area.x, visible.y - area.y);
        self.push([at, extent, at, extent, 0, 0, KIND_CANVAS, source], Some(index));
    }
}

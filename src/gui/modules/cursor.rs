//! `cursor.mod` -- pointer rendering (8.2, Overlay slot).
//!
//! ## Why a surface that moves rather than a direct drawing
//!
//! The cursor could be painted by the compositor at the end of every
//! frame. It is not: 8.2 makes it a replaceable module, and a component
//! the compositor drew itself would not be replaceable at all. So it is a
//! surface of the `Overlay` layer, which this module moves on every
//! pointer event.
//!
//! The `Overlay` layer is at the top by construction and never receives a
//! click: `Stack::hit` ignores it. Without that the cursor would
//! intercept every one of its own clicks, always being under itself.
//!
//! ## Why the shapes are bitmaps and not drawing commands
//!
//! The arrow used to be built from fifteen `rect` and `line` commands.
//! Three shapes that way would have been forty-five, and a display list
//! rebuilt on every change of shape. `DRAW_ICON` carries a **monochrome
//! 16x16** in a single message -- thirty-two bytes of bits plus a colour
//! -- which is exactly the size of a cursor. A shape therefore costs two
//! messages: one for the body, one for the outline.
//!
//! The outline is a second mask rather than a colour trick. A cursor of a
//! single shade disappears the moment it passes over a background of the
//! same colour, and that is the kind of defect one only notices the day
//! one changes theme.
//!
//! ## The hot spot
//!
//! The point a shape designates is not always its top-left corner: the
//! beam of a text cursor is at its middle. Each shape therefore carries
//! its hot spot, and the surface is placed at `pointer - hot_spot`.
//!
//! That is what obliged `compositor::route_pointer` to read the **real**
//! position of the overlay surface rather than deducing it from the
//! pointer. A damage rectangle computed from the pointer would miss the
//! rows above by the offset, and the symptom would be a cursor leaving a
//! trail.

use core::sync::atomic::{AtomicU8, Ordering};

use super::super::client::Event;
use super::super::protocol::CursorShape;
use super::super::surface::Layer;
use super::super::theme::Theme;
use super::{Canvas, Spec};

/// Size of the cursor box.
const SIZE: i32 = 16;

/// A shape: two masks and a hot spot.
///
/// `body` and `outline` are sixteen rows of sixteen bits, the leftmost
/// column being the high bit -- the order `Painter::icon` reads, which
/// takes `0x8000 >> column`.
struct Shape {
    body: [u16; 16],
    outline: [u16; 16],
    /// Point of the drawing that designates, relative to its corner.
    hot: (i32, i32),
}

/// The classic arrow. Hot spot at the very tip, hence at the corner.
///
/// It is authored here as a mask rather than transcribed from the
/// commands it replaces: those painted their outline **over** the body
/// with three `line` calls whose ends ran past it, leaving a few stray
/// pixels of background colour outside the arrow. Invisible on a flat
/// background, faintly visible on the gradient of `wallpaper.mod`. The
/// mask has no such thing.
const ARROW: Shape = Shape {
    body: [
        0x0000, 0x0000, 0x4000, 0x6000, 0x7000, 0x7800, 0x7C00, 0x7E00, 0x7F00, 0x7C00, 0x6C00,
        0x4600, 0x0600, 0x0300, 0x0100, 0x0000,
    ],
    outline: [
        0x8000, 0xC000, 0xA000, 0x9000, 0x8800, 0x8400, 0x8200, 0x8100, 0x8080, 0x8380, 0x9200,
        0xA900, 0xC900, 0x8480, 0x0280, 0x0100,
    ],
    hot: (0, 0),
};

/// Pointing hand: over anything that answers a click.
///
/// Drawn with the fingertip at the top-left so that the hot spot stays
/// close to the corner. A hand centred in its box would have put the
/// designated point seven pixels below the finger, which reads as a
/// pointer that misses what it is over.
const HAND: Shape = Shape {
    body: [
        0x0000, 0x6000, 0x6000, 0x6000, 0x6000, 0x6000, 0x7C00, 0x7F00, 0x7F80, 0x7F80, 0x7FC0,
        0x7FC0, 0x7FC0, 0x3F80, 0x3F80, 0x0000,
    ],
    outline: [
        0x6000, 0x9000, 0x9000, 0x9000, 0x9000, 0x9C00, 0x8300, 0x8080, 0x8040, 0x8040, 0x8020,
        0x8020, 0x8020, 0x4040, 0x4040, 0x7FC0,
    ],
    hot: (1, 0),
};

/// Text beam: over an entry.
///
/// The serifs are what tell it apart from a plain vertical bar at a
/// glance, and the halo is what keeps it visible over a dark field as
/// well as a light one. Hot spot at the middle of the stem, which is the
/// only point a beam can sensibly designate.
const TEXT: Shape = Shape {
    body: [
        0x0000, 0x7000, 0x2000, 0x2000, 0x2000, 0x2000, 0x2000, 0x2000, 0x2000, 0x2000, 0x2000,
        0x2000, 0x2000, 0x7000, 0x0000, 0x0000,
    ],
    outline: [
        0xF800, 0x8800, 0xD800, 0x5000, 0x5000, 0x5000, 0x5000, 0x5000, 0x5000, 0x5000, 0x5000,
        0x5000, 0xD800, 0x8800, 0xF800, 0x0000,
    ],
    hot: (2, 7),
};

fn shape_of(shape: CursorShape) -> &'static Shape {
    match shape {
        CursorShape::Arrow => &ARROW,
        CursorShape::Hand => &HAND,
        CursorShape::Text => &TEXT,
    }
}

/// Shape in force, as a `CursorShape` code.
///
/// A static and not a field of `Canvas`: a module is a set of function
/// pointers with no state of its own, which is what makes the loop in
/// `modules::run` common to all of them. The same choice as the `SESSION`
/// of `login.rs`.
static SHAPE: AtomicU8 = AtomicU8::new(0);

pub fn task() -> ! {
    super::run(&Spec {
        name: "cursor",
        layer: Layer::Overlay,
        geometry,
        draw,
        on_event,
        tick_ms: 0,
        visible_at_start: true,
    })
}

fn geometry(width: i32, height: i32, _theme: &Theme) -> (i32, i32, i32, i32) {
    (width / 2, height / 2, SIZE, SIZE)
}

/// Turns a row mask into the byte pair `DRAW_ICON` expects.
///
/// Big-endian, because `Painter::icon` reads each row as
/// `u16::from_be_bytes` then tests `0x8000 >> column`: the leftmost
/// column is the high bit of the **first** byte.
fn to_bits(rows: &[u16; 16]) -> [u8; 32] {
    let mut bits = [0u8; 32];
    for (index, row) in rows.iter().enumerate() {
        let [high, low] = row.to_be_bytes();
        bits[index * 2] = high;
        bits[index * 2 + 1] = low;
    }
    bits
}

/// Outline first, then body.
///
/// The order is not indifferent: the two masks touch, and drawing the
/// body last means a pixel claimed by both belongs to the body. The
/// reverse would eat into the shape from its edges, which on a figure
/// twelve pixels wide is a third of it.
fn draw(canvas: &mut Canvas) {
    let theme = canvas.theme;
    let surface = canvas.surface;
    let shape = shape_of(CursorShape::from_code(SHAPE.load(Ordering::Relaxed)));

    // `None` as background: an unlit bit paints nothing. A background
    // colour here would turn the cursor into an opaque sixteen by sixteen
    // square, which is the whole surface.
    canvas
        .client
        .icon(surface, 0, 0, &to_bits(&shape.outline), theme.background, None);
    canvas
        .client
        .icon(surface, 0, 0, &to_bits(&shape.body), theme.text, None);
}

fn on_event(canvas: &mut Canvas, event: &Event) -> bool {
    match event {
        Event::Pointer(pointer) => {
            let wanted = pointer.shape;
            let changed = SHAPE.swap(wanted, Ordering::Relaxed) != wanted;
            let shape = shape_of(CursorShape::from_code(wanted));

            // The cursor follows the pointer but does not redraw itself
            // unless its shape changed: its drawing does not depend on its
            // position. Moving the surface is enough, and avoids sending
            // two commands per millimetre of mouse.
            canvas.client.move_surface(
                canvas.surface,
                pointer.x as i32 - shape.hot.0,
                pointer.y as i32 - shape.hot.1,
            );
            if changed {
                crate::serial_println!(
                    "[mod-cursor] forme {}",
                    CursorShape::from_code(wanted).as_str()
                );
            }
            changed
        }
        Event::Redraw => true,
        _ => false,
    }
}

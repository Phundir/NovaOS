//! 10.1.3 -- Global zoom, magnifier and area (spec 16.2, `zoom.mod`).
//!
//! ## The three shapes 16.2 asks for
//!
//! "Zoom global / loupe / zone". They are not three settings of one
//! feature, they are three different answers to three different needs:
//!
//! | Shape | What it magnifies | For |
//! |---|---|---|
//! | `Global` | the whole screen, centred on the pointer | reading everything, permanently |
//! | `Lens` | a disc that follows the pointer | inspecting one detail without losing the layout |
//! | `Area` | a fixed rectangle, wherever the pointer goes | watching one panel while working elsewhere |
//!
//! ## Nearest-neighbour, and it is not laziness
//!
//! A magnifier for someone who needs it must keep **edges sharp**.
//! Bilinear interpolation would blur a one-pixel-wide glyph stem into
//! two grey pixels, which is exactly what the person is zooming in to
//! avoid. Nearest-neighbour keeps the stem black. It also costs one
//! integer division per pixel instead of four multiplies, which is the
//! secondary reason.
//!
//! ## Why the whole screen is republished while zoom is on
//!
//! The compositor accumulates a damaged rectangle and publishes only
//! that. Under magnification a rectangle of the back buffer lands on a
//! *different* rectangle of the screen, and the mapping depends on the
//! zoom centre -- which moves with the pointer. Publishing a partial area
//! would leave the rest of the screen showing the previous
//! magnification, torn along the boundary.
//!
//! So `wants_full_frame` is true while zoom is on, and the compositor
//! damages the whole screen. The cost is real and it is the correct
//! trade: a magnifier that tears is not a magnifier.
//!
//! ## Where it is applied
//!
//! On the way from the back buffer to the screen, like the contrast
//! transform, and for the same reason -- the applications draw at their
//! own size, and nothing above the publication stage knows the screen is
//! magnified. 8.1 is untouched: the compositor is still the only writer.

/// What is magnified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// The whole screen, centred on the pointer.
    Global,
    /// A disc that follows the pointer.
    Lens,
    /// A fixed rectangle of the screen.
    Area,
}

impl Shape {
    pub fn as_str(&self) -> &'static str {
        match self {
            Shape::Global => "global",
            Shape::Lens => "loupe",
            Shape::Area => "zone",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "global" => Some(Shape::Global),
            "loupe" | "lens" => Some(Shape::Lens),
            "zone" | "area" => Some(Shape::Area),
            _ => None,
        }
    }

    pub fn code(&self) -> u8 {
        match self {
            Shape::Global => 0,
            Shape::Lens => 1,
            Shape::Area => 2,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Shape::Global),
            1 => Some(Shape::Lens),
            2 => Some(Shape::Area),
            _ => None,
        }
    }
}

/// Magnification, in percent. 100 is no magnification at all.
pub const MIN_PERCENT: u32 = 100;
pub const MAX_PERCENT: u32 = 800;
/// Step of one keystroke.
pub const STEP_PERCENT: u32 = 50;
/// Radius of the lens, in screen pixels.
pub const LENS_RADIUS: i32 = 140;

/// Complete state of the magnifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub shape: Shape,
    /// Magnification in percent, always at least `MIN_PERCENT`.
    pub percent: u32,
    /// Rectangle magnified in `Area` mode.
    pub area: (i32, i32, i32, i32),
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            shape: Shape::Global,
            percent: 200,
            area: (0, 0, 640, 360),
        }
    }
}

impl Settings {
    /// Clamps the magnification into the supported range.
    ///
    /// A factor below one would shrink the screen, which is not a
    /// magnifier; a factor of zero would divide by zero in the mapping.
    /// Both come from a settings file, hence from outside.
    pub fn sanitised(&self) -> Self {
        Settings {
            shape: self.shape,
            percent: self.percent.clamp(MIN_PERCENT, MAX_PERCENT),
            area: self.area,
        }
    }

    pub fn zoomed_in(&self) -> Self {
        Settings {
            percent: (self.percent + STEP_PERCENT).min(MAX_PERCENT),
            ..*self
        }
    }

    pub fn zoomed_out(&self) -> Self {
        Settings {
            percent: self.percent.saturating_sub(STEP_PERCENT).max(MIN_PERCENT),
            ..*self
        }
    }
}

/// Maps a screen pixel back to the back-buffer pixel it shows.
///
/// This is the only piece of arithmetic in the module, and everything
/// rests on it being an **inverse** map: iterating over the destination
/// and asking where each pixel comes from is what guarantees no
/// destination pixel is left unwritten. Iterating over the source and
/// asking where each pixel goes leaves holes as soon as the factor is not
/// an integer -- the classic way a magnifier ends up with a grid of
/// unpainted pixels through it.
///
/// Returns `None` when the pixel is outside the magnified region, which
/// means "copy it unchanged".
pub fn source_of(
    settings: &Settings,
    screen_width: i32,
    screen_height: i32,
    centre_x: i32,
    centre_y: i32,
    x: i32,
    y: i32,
) -> Option<(i32, i32)> {
    let settings = settings.sanitised();
    let percent = settings.percent as i64;

    // The point the magnification turns around, and the region it
    // applies to.
    let (pivot_x, pivot_y) = match settings.shape {
        Shape::Global | Shape::Lens => (centre_x, centre_y),
        Shape::Area => {
            let (ax, ay, aw, ah) = settings.area;
            (ax + aw / 2, ay + ah / 2)
        }
    };

    match settings.shape {
        Shape::Global => {}
        Shape::Lens => {
            // A disc, so the test is on the squared distance: a square
            // root here would be a float, and comparing squares is exact.
            let dx = (x - centre_x) as i64;
            let dy = (y - centre_y) as i64;
            if dx * dx + dy * dy > (LENS_RADIUS as i64) * (LENS_RADIUS as i64) {
                return None;
            }
        }
        Shape::Area => {
            let (ax, ay, aw, ah) = settings.area;
            if x < ax || y < ay || x >= ax + aw || y >= ay + ah {
                return None;
            }
        }
    }

    // Inverse map: source = pivot + (destination - pivot) * 100 / percent.
    let source_x = pivot_x as i64 + ((x - pivot_x) as i64 * 100) / percent;
    let source_y = pivot_y as i64 + ((y - pivot_y) as i64 * 100) / percent;

    // Clamped rather than dropped: a destination pixel whose source falls
    // off the edge must show the edge, not a hole. Dropping it would
    // leave whatever the previous frame had there.
    Some((
        source_x.clamp(0, screen_width as i64 - 1) as i32,
        source_y.clamp(0, screen_height as i64 - 1) as i32,
    ))
}

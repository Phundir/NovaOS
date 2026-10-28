//! 10.1.4 -- High contrast and colour-blind modes (spec 16.2, 16.3).
//!
//! ## Correcting, not simulating
//!
//! There are two things one can do with a colour-vision deficiency, and
//! they are opposites. **Simulating** it shows a normally-sighted person
//! what a colour-blind person sees; it is a design tool. **Correcting**
//! it -- daltonisation -- moves the information out of the channel the
//! viewer cannot separate and into one they can; it is an accessibility
//! feature. 16.3 lists the modes under "Accessibilite", so this module
//! corrects.
//!
//! The algorithm is the usual one, and it has three steps:
//!
//! 1. simulate the deficiency on the pixel;
//! 2. take the **error** -- what the deficiency destroyed;
//! 3. put that error back into the channels the viewer still separates.
//!
//! A protanope cannot separate red from green, but sees blue perfectly:
//! so the red-green error is pushed into blue. That is why the
//! redistribution matrices below are not symmetric, and why using the
//! same one for all three types would be worse than doing nothing.
//!
//! ## Achromatopsia is not a matrix
//!
//! Total absence of colour vision leaves only luminance. Redistributing
//! an error into a channel that does not exist would achieve nothing, so
//! that mode converts to grey and then **stretches the contrast**: what a
//! person with achromatopsia loses is not hue, it is the ability to tell
//! two colours of similar luminance apart. Two greys ten units apart
//! become two greys forty apart.
//!
//! ## Everything is an integer, again
//!
//! The matrices are in Q8 -- coefficients out of 256. The luminance
//! weights are the ITU BT.601 ones, 0.299 / 0.587 / 0.114, written as
//! 77 / 150 / 29 out of 256. That is the same fixed-point discipline as
//! everywhere else in this kernel, and for the same reason: SSE is not
//! enabled, so a float here is a float in the kernel.
//!
//! ## Where it is applied
//!
//! On the **back buffer, on its way to the screen**. Not on the theme,
//! not on the drawing commands: an application draws what it draws, and a
//! transform that changed the commands would be visible to the
//! application, which has no business knowing. The compositor stays the
//! only writer of the framebuffer (8.1) -- this is a stage of its
//! publication, not a second painter.

use crate::graphics::framebuffer::{blue_of, green_of, red_of, rgb, Color};

/// The modes of 16.3, plus the high-contrast mode of 16.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// No transform. The pixels go out as they were painted.
    None,
    /// High contrast: luminance stretched around the mid grey.
    High,
    /// Red difficult.
    Protanopia,
    /// Green difficult.
    Deuteranopia,
    /// Blue difficult.
    Tritanopia,
    /// No colour vision at all.
    Achromatopsia,
    /// User matrix, nine coefficients out of 256.
    Custom,
}

/// Number of modes, to size the tables and bound a cycle.
pub const MODE_COUNT: usize = 7;

impl Mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::None => "aucun",
            Mode::High => "contraste eleve",
            Mode::Protanopia => "protanopie",
            Mode::Deuteranopia => "deuteranopie",
            Mode::Tritanopia => "tritanopie",
            Mode::Achromatopsia => "achromatopsie",
            Mode::Custom => "personnalise",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "aucun" | "none" => Some(Mode::None),
            "contraste_eleve" | "high" => Some(Mode::High),
            "protanopie" | "protanopia" => Some(Mode::Protanopia),
            "deuteranopie" | "deuteranopia" => Some(Mode::Deuteranopia),
            "tritanopie" | "tritanopia" => Some(Mode::Tritanopia),
            "achromatopsie" | "achromatopsia" => Some(Mode::Achromatopsia),
            "personnalise" | "custom" => Some(Mode::Custom),
            _ => None,
        }
    }

    pub fn code(&self) -> u8 {
        match self {
            Mode::None => 0,
            Mode::High => 1,
            Mode::Protanopia => 2,
            Mode::Deuteranopia => 3,
            Mode::Tritanopia => 4,
            Mode::Achromatopsia => 5,
            Mode::Custom => 6,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Mode::None),
            1 => Some(Mode::High),
            2 => Some(Mode::Protanopia),
            3 => Some(Mode::Deuteranopia),
            4 => Some(Mode::Tritanopia),
            5 => Some(Mode::Achromatopsia),
            6 => Some(Mode::Custom),
            _ => None,
        }
    }

    /// The five modes 16.3 lists, in the order it lists them.
    pub fn colour_blind() -> [Mode; 5] {
        [
            Mode::Protanopia,
            Mode::Deuteranopia,
            Mode::Tritanopia,
            Mode::Achromatopsia,
            Mode::Custom,
        ]
    }

    /// Does this mode change any pixel?
    pub fn is_active(&self) -> bool {
        *self != Mode::None
    }
}

/// Luminance weights of ITU BT.601, out of 256.
const LUMA_R: i32 = 77;
const LUMA_G: i32 = 150;
const LUMA_B: i32 = 29;

/// Simulation matrices, in Q8.
///
/// Each row says how one output channel is built from the three input
/// ones. These are the Brettel-Vienot-Mollon coefficients rounded to
/// eighths of a percent; the rounding moves a channel by at most one
/// level out of 255, which is under the quantisation of every mode this
/// driver supports.
fn simulate(mode: Mode, red: i32, green: i32, blue: i32) -> (i32, i32, i32) {
    match mode {
        // A protanope's red channel is rebuilt almost entirely from
        // green; the blue channel is untouched.
        Mode::Protanopia => (
            (147 * green + 109 * blue) / 256,
            (183 * green + 73 * blue) / 256,
            256 * blue / 256,
        ),
        // A deuteranope loses green: it is rebuilt from red.
        Mode::Deuteranopia => (
            (161 * red + 95 * blue) / 256,
            (179 * red + 77 * blue) / 256,
            256 * blue / 256,
        ),
        // A tritanope loses blue: red and green stay, blue is rebuilt
        // from the two of them.
        Mode::Tritanopia => (
            (243 * red + 13 * green) / 256,
            (187 * green + 69 * blue) / 256,
            (120 * green + 136 * blue) / 256,
        ),
        _ => (red, green, blue),
    }
}

/// Redistribution matrices, in Q8.
///
/// Where the error goes. Asymmetric on purpose: a protanope sees blue,
/// so the red-green error goes there; a tritanope sees red and green, so
/// the blue error goes into those. Using one matrix for all three is the
/// mistake that makes daltonisation look like a tint.
fn redistribute(mode: Mode, error: (i32, i32, i32)) -> (i32, i32, i32) {
    let (er, eg, eb) = error;
    match mode {
        Mode::Protanopia | Mode::Deuteranopia => (
            0,
            (179 * er + 256 * eg) / 256,
            (179 * er + 256 * eb) / 256,
        ),
        Mode::Tritanopia => (
            (256 * er + 179 * eb) / 256,
            (256 * eg + 179 * eb) / 256,
            0,
        ),
        _ => (0, 0, 0),
    }
}

/// A user-supplied correction matrix (16.3, "Custom").
///
/// Nine coefficients out of 256, in row order. The identity is the
/// default: a custom mode nobody configured must not change the screen.
#[derive(Debug, Clone, Copy)]
pub struct Matrix(pub [i32; 9]);

impl Default for Matrix {
    fn default() -> Self {
        Matrix([256, 0, 0, 0, 256, 0, 0, 0, 256])
    }
}

impl Matrix {
    /// Parses nine comma-separated integers.
    ///
    /// Returns `None` on anything else. A matrix half read is worse than
    /// no matrix: it would apply three correct rows and one of zeros,
    /// which blanks a channel.
    pub fn parse(text: &str) -> Option<Self> {
        let mut values = [0i32; 9];
        let mut count = 0;
        for field in text.split(',') {
            if count == 9 {
                return None;
            }
            values[count] = field.trim().parse::<i32>().ok()?;
            count += 1;
        }
        (count == 9).then_some(Matrix(values))
    }

    fn apply(&self, red: i32, green: i32, blue: i32) -> (i32, i32, i32) {
        let m = &self.0;
        (
            (m[0] * red + m[1] * green + m[2] * blue) / 256,
            (m[3] * red + m[4] * green + m[5] * blue) / 256,
            (m[6] * red + m[7] * green + m[8] * blue) / 256,
        )
    }
}

/// Transforms one pixel.
///
/// Written to be called once per pixel of a damaged rectangle, so it does
/// no allocation and takes no lock: the mode and the matrix are handed
/// in by the caller, which read them once for the whole frame.
pub fn transform(mode: Mode, matrix: &Matrix, colour: Color) -> Color {
    if !mode.is_active() {
        return colour;
    }

    let red = red_of(colour) as i32;
    let green = green_of(colour) as i32;
    let blue = blue_of(colour) as i32;

    let (r, g, b) = match mode {
        Mode::None => (red, green, blue),

        // Each channel stretched around the mid grey, by a factor of two.
        //
        // The other candidate was to stretch the **luminance** and scale
        // the three channels by the ratio, which keeps the hue exactly.
        // It collapses: a dark saturated colour has a low luminance, the
        // stretch drives it below zero, and the ratio then takes every
        // channel to zero. A dark blue comes out black -- and "it was
        // blue" is precisely the information the person is trying to
        // see.
        //
        // Stretching per channel is a monotone map, so it preserves the
        // **order** of the three channels and with it the recognisable
        // hue; what it changes is the saturation, which for a high
        // contrast mode is the intended direction anyway.
        Mode::High => (
            128 + (red - 128) * 2,
            128 + (green - 128) * 2,
            128 + (blue - 128) * 2,
        ),

        // Grey, then contrast stretched: what is lost is the ability to
        // tell two colours of similar luminance apart, so the useful
        // thing to do is to push them apart.
        Mode::Achromatopsia => {
            let luma = (LUMA_R * red + LUMA_G * green + LUMA_B * blue) / 256;
            let stretched = (128 + (luma - 128) * 3 / 2).clamp(0, 255);
            (stretched, stretched, stretched)
        }

        Mode::Custom => matrix.apply(red, green, blue),

        // The three-step daltonisation.
        deficiency => {
            let (sr, sg, sb) = simulate(deficiency, red, green, blue);
            let error = (red - sr, green - sg, blue - sb);
            let (dr, dg, db) = redistribute(deficiency, error);
            (red + dr, green + dg, blue + db)
        }
    };

    rgb(
        r.clamp(0, 255) as u8,
        g.clamp(0, 255) as u8,
        b.clamp(0, 255) as u8,
    )
}

/// Relative luminance of a colour, out of 255.
///
/// Exposed because the self-test needs it: "the contrast increased" is
/// only checkable against a measure, and the measure has to be the same
/// one the transform uses.
pub fn luminance(colour: Color) -> i32 {
    (LUMA_R * red_of(colour) as i32
        + LUMA_G * green_of(colour) as i32
        + LUMA_B * blue_of(colour) as i32)
        / 256
}

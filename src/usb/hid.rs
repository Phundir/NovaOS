//! HID -- interpretation of the reports of a pointing device.
//!
//! ## Two forms, and why both matter
//!
//! HID defines a **boot protocol**: a mouse accepting it emits three or
//! four bytes of a fixed layout, which the BIOS can read without
//! understanding a report descriptor. That is the ordinary case of a USB
//! mouse.
//!
//! ```text
//!   0  buttons: bit 0 left, bit 1 right, bit 2 middle
//!   1  X displacement, signed 8-bit integer
//!   2  Y displacement, signed 8-bit integer
//!   3  wheel, signed integer (optional)
//! ```
//!
//! A **tablet**, on the other hand, returns **absolute** coordinates on
//! sixteen bits, and has no boot protocol: its layout is described by its
//! report descriptor.
//!
//! ```text
//!   0  buttons
//!   1  absolute X, 16 bits, scale 0..32767
//!   3  absolute Y, 16 bits, same scale
//!   5  wheel
//! ```
//!
//! ## Why the distinction is not cosmetic
//!
//! A **relative** pointer forces the host to capture the physical pointer
//! before transmitting anything at all: there is no position to
//! communicate, only displacements, so the host must commandeer the
//! mouse. An **absolute** pointer transmits a position, which means
//! something with no capture -- the guest cursor follows the host's directly.
//!
//! That is exactly the difference between "you have to click in the
//! window for the mouse to work" and "the mouse works".
//!
//! ## Recognising the form without reading the report descriptor
//!
//! Parsing a HID report descriptor demands an item interpreter (usage
//! pages, collections, variable field sizes): a state machine of several
//! hundred lines for a result two pieces of information already give.
//!
//! The report length is enough here: three or four bytes is the boot
//! layout; six or seven is the tablet. The `protocol` field of the
//! interface confirms that this really is a pointing device. That reading
//! covers the two devices QEMU emulates and the commercial USB mice,
//! which all respect the boot protocol.
//!
//! **Reservation on record**: an exotic device whose report were five
//! bytes, or that laid its fields out differently, would be misread. The
//! real remedy is the report descriptor interpreter, and it is not written.

/// Form of a pointing report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// Three or four bytes, relative displacements.
    BootMouse,
    /// Six bytes or more, absolute position on sixteen bits.
    AbsoluteTablet,
    /// Length not recognised.
    Unknown,
}

impl Shape {
    /// Derives the form from the report length.
    pub fn of(length: usize) -> Shape {
        match length {
            3 | 4 => Shape::BootMouse,
            6..=8 => Shape::AbsoluteTablet,
            _ => Shape::Unknown,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Shape::BootMouse => "souris relative (protocole d'amorcage)",
            Shape::AbsoluteTablet => "tablette absolue",
            Shape::Unknown => "disposition inconnue",
        }
    }

    /// Does the device return a position rather than a displacement?
    pub fn is_absolute(&self) -> bool {
        *self == Shape::AbsoluteTablet
    }
}

/// What a report carries, once decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    /// bit 0 left, bit 1 right, bit 2 middle -- same convention as PS/2,
    /// so that both sources feed the same state.
    pub buttons: u8,
    /// Displacement, for a relative device.
    pub dx: i32,
    pub dy: i32,
    /// Position on the device scale, for an absolute one.
    pub absolute: Option<(u32, u32)>,
    pub wheel: i8,
}

/// Logical scale of a tablet: `0..=ABSOLUTE_RANGE`.
///
/// Value imposed by the report descriptor of the usual tablets. The
/// screen position follows by rule of three, and not by shifting: a
/// screen whose width is not a power of two would otherwise lose its last
/// columns.
pub const ABSOLUTE_RANGE: u32 = 32767;

/// Decodes a report according to its form.
///
/// Returns `None` on a length inconsistent with the announced form: a
/// truncated report is better ignored than misread, where the mouse would
/// shoot off into a corner.
pub fn decode(shape: Shape, report: &[u8]) -> Option<Report> {
    match shape {
        Shape::BootMouse if report.len() >= 3 => Some(Report {
            buttons: report[0] & 0b111,
            dx: report[1] as i8 as i32,
            dy: report[2] as i8 as i32,
            absolute: None,
            wheel: report.get(3).map(|value| *value as i8).unwrap_or(0),
        }),

        Shape::AbsoluteTablet if report.len() >= 5 => {
            let x = u16::from_le_bytes([report[1], report[2]]) as u32;
            let y = u16::from_le_bytes([report[3], report[4]]) as u32;
            Some(Report {
                buttons: report[0] & 0b111,
                dx: 0,
                dy: 0,
                absolute: Some((x, y)),
                wheel: report.get(5).map(|value| *value as i8).unwrap_or(0),
            })
        }

        _ => None,
    }
}

/// Converts an absolute tablet coordinate into a screen pixel.
///
/// The rounding is done in 64 bits: `position * width` overflows a `u32`
/// as soon as the width exceeds 131 072, which does not happen today but
/// would cost a cursor jumping to the opposite side of the screen the day
/// it did.
pub fn to_screen(position: u32, extent: i32) -> i32 {
    if extent <= 1 {
        return 0;
    }
    let scaled = position as u64 * (extent as u64 - 1) / ABSOLUTE_RANGE as u64;
    (scaled as i32).clamp(0, extent - 1)
}

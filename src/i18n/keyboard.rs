//! 10.2.4 -- Keyboard layouts (spec 14.2, "Clavier" row).
//!
//! ## What was wrong before
//!
//! `drivers::keyboard::decode` carried a single AZERTY table, written
//! into the function. Its own module header called that out as defect
//! number three of phase 6.1.1: "14.2 wants a per-user arrangement; a
//! table inside an interrupt handler cannot be reloaded". This module is
//! that reload.
//!
//! ## Layouts as data
//!
//! A layout is four rows of scancodes with, for each key, the character
//! it produces alone and the one it produces with shift. Everything is
//! described by the **physical** key -- scancode set 1, the same numbers
//! on every layout -- because that is what the hardware sends. Only the
//! glyph changes.
//!
//! ## Primary, secondary, switch key
//!
//! 14.2 asks for the three. The switch key is a scancode: pressing it
//! flips between the primary and the secondary layout, and the flip is
//! global because it is the physical keyboard that changed meaning, not
//! one window. The default is the **right Alt** key, which produces no
//! character on any of the four layouts here -- picking a key that has a
//! glyph would cost that glyph.
//!
//! ## Only printable ASCII
//!
//! The rule of the whole repository: nothing outside 0x20-0x7E may reach
//! a displayed string. So an accented key hands back its bare letter, and
//! a dead key that would compose one is not described. A real
//! multilingual layout needs a wider character pipeline than the console
//! and the bitmap font offer today, and saying so is worth more than
//! printing solid squares.

/// One of the layouts the system carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// French, the layout the phase 6 driver hard-coded.
    Azerty,
    /// US English.
    Qwerty,
    /// German and Swiss.
    Qwertz,
    /// Dvorak simplified, US variant.
    Dvorak,
}

/// Number of layouts, to size the tables.
pub const LAYOUT_COUNT: usize = 4;

impl Layout {
    pub fn as_str(&self) -> &'static str {
        match self {
            Layout::Azerty => "azerty",
            Layout::Qwerty => "qwerty",
            Layout::Qwertz => "qwertz",
            Layout::Dvorak => "dvorak",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Layout::Azerty => "AZERTY (francais)",
            Layout::Qwerty => "QWERTY (anglais)",
            Layout::Qwertz => "QWERTZ (allemand)",
            Layout::Dvorak => "Dvorak simplifie",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "azerty" => Some(Layout::Azerty),
            "qwerty" => Some(Layout::Qwerty),
            "qwertz" => Some(Layout::Qwertz),
            "dvorak" => Some(Layout::Dvorak),
            _ => None,
        }
    }

    pub fn all() -> [Layout; LAYOUT_COUNT] {
        [
            Layout::Azerty,
            Layout::Qwerty,
            Layout::Qwertz,
            Layout::Dvorak,
        ]
    }

    /// Character produced by a scancode, or `None` if the key is not a
    /// character key on this layout.
    pub fn character(&self, scancode: u8, shift: bool) -> Option<char> {
        let table = self.table();
        table
            .iter()
            .find(|entry| entry.0 == scancode)
            .map(|entry| if shift { entry.2 } else { entry.1 })
            .filter(|glyph| *glyph != '\0')
    }

    /// The layout table: scancode, bare glyph, shifted glyph.
    ///
    /// `\0` means "this key produces nothing in this state", which is not
    /// the same as "this key is unknown": the difference matters for a
    /// key that has a glyph only when shifted.
    fn table(&self) -> &'static [(u8, char, char)] {
        match self {
            Layout::Azerty => AZERTY,
            Layout::Qwerty => QWERTY,
            Layout::Qwertz => QWERTZ,
            Layout::Dvorak => DVORAK,
        }
    }

    /// Number of character keys described.
    pub fn key_count(&self) -> usize {
        self.table().len()
    }
}

// ============================================================
//  The tables
// ============================================================

/// The space bar and the number row are common to every layout below --
/// only the glyphs differ, and they are written out in each table rather
/// than shared. A shared prefix would save thirty lines and make a
/// mistake in one layout invisible in the diff of another.
const SPACE: (u8, char, char) = (0x39, ' ', ' ');

/// French AZERTY. The accented keys hand back their bare letter, per the
/// ASCII rule of the repository.
static AZERTY: &[(u8, char, char)] = &[
    (0x02, '&', '1'), (0x03, 'e', '2'), (0x04, '"', '3'), (0x05, '\'', '4'),
    (0x06, '(', '5'), (0x07, '-', '6'), (0x08, 'e', '7'), (0x09, '_', '8'),
    (0x0A, 'c', '9'), (0x0B, 'a', '0'), (0x0C, ')', '.'), (0x0D, '=', '+'),
    (0x10, 'a', 'A'), (0x11, 'z', 'Z'), (0x12, 'e', 'E'), (0x13, 'r', 'R'),
    (0x14, 't', 'T'), (0x15, 'y', 'Y'), (0x16, 'u', 'U'), (0x17, 'i', 'I'),
    (0x18, 'o', 'O'), (0x19, 'p', 'P'),
    (0x1E, 'q', 'Q'), (0x1F, 's', 'S'), (0x20, 'd', 'D'), (0x21, 'f', 'F'),
    (0x22, 'g', 'G'), (0x23, 'h', 'H'), (0x24, 'j', 'J'), (0x25, 'k', 'K'),
    (0x26, 'l', 'L'), (0x27, 'm', 'M'), (0x28, 'u', 'U'),
    (0x2C, 'w', 'W'), (0x2D, 'x', 'X'), (0x2E, 'c', 'C'), (0x2F, 'v', 'V'),
    (0x30, 'b', 'B'), (0x31, 'n', 'N'), (0x32, ',', '?'), (0x33, ';', '.'),
    (0x34, ':', '/'), (0x35, '!', '!'),
    SPACE,
];

/// US QWERTY.
static QWERTY: &[(u8, char, char)] = &[
    (0x02, '1', '!'), (0x03, '2', '@'), (0x04, '3', '#'), (0x05, '4', '$'),
    (0x06, '5', '%'), (0x07, '6', '^'), (0x08, '7', '&'), (0x09, '8', '*'),
    (0x0A, '9', '('), (0x0B, '0', ')'), (0x0C, '-', '_'), (0x0D, '=', '+'),
    (0x10, 'q', 'Q'), (0x11, 'w', 'W'), (0x12, 'e', 'E'), (0x13, 'r', 'R'),
    (0x14, 't', 'T'), (0x15, 'y', 'Y'), (0x16, 'u', 'U'), (0x17, 'i', 'I'),
    (0x18, 'o', 'O'), (0x19, 'p', 'P'), (0x1A, '[', '{'), (0x1B, ']', '}'),
    (0x1E, 'a', 'A'), (0x1F, 's', 'S'), (0x20, 'd', 'D'), (0x21, 'f', 'F'),
    (0x22, 'g', 'G'), (0x23, 'h', 'H'), (0x24, 'j', 'J'), (0x25, 'k', 'K'),
    (0x26, 'l', 'L'), (0x27, ';', ':'), (0x28, '\'', '"'),
    (0x2B, '\\', '|'),
    (0x2C, 'z', 'Z'), (0x2D, 'x', 'X'), (0x2E, 'c', 'C'), (0x2F, 'v', 'V'),
    (0x30, 'b', 'B'), (0x31, 'n', 'N'), (0x32, 'm', 'M'), (0x33, ',', '<'),
    (0x34, '.', '>'), (0x35, '/', '?'),
    SPACE,
];

/// German QWERTZ. `y` and `z` are swapped against QWERTY, and the three
/// umlaut keys hand back their bare letter.
static QWERTZ: &[(u8, char, char)] = &[
    (0x02, '1', '!'), (0x03, '2', '"'), (0x04, '3', '#'), (0x05, '4', '$'),
    (0x06, '5', '%'), (0x07, '6', '&'), (0x08, '7', '/'), (0x09, '8', '('),
    (0x0A, '9', ')'), (0x0B, '0', '='), (0x0C, 's', '?'), (0x0D, '\'', '`'),
    (0x10, 'q', 'Q'), (0x11, 'w', 'W'), (0x12, 'e', 'E'), (0x13, 'r', 'R'),
    (0x14, 't', 'T'), (0x15, 'z', 'Z'), (0x16, 'u', 'U'), (0x17, 'i', 'I'),
    (0x18, 'o', 'O'), (0x19, 'p', 'P'), (0x1A, 'u', 'U'), (0x1B, '+', '*'),
    (0x1E, 'a', 'A'), (0x1F, 's', 'S'), (0x20, 'd', 'D'), (0x21, 'f', 'F'),
    (0x22, 'g', 'G'), (0x23, 'h', 'H'), (0x24, 'j', 'J'), (0x25, 'k', 'K'),
    (0x26, 'l', 'L'), (0x27, 'o', 'O'), (0x28, 'a', 'A'),
    (0x2B, '#', '\''),
    (0x2C, 'y', 'Y'), (0x2D, 'x', 'X'), (0x2E, 'c', 'C'), (0x2F, 'v', 'V'),
    (0x30, 'b', 'B'), (0x31, 'n', 'N'), (0x32, 'm', 'M'), (0x33, ',', ';'),
    (0x34, '.', ':'), (0x35, '-', '_'),
    SPACE,
];

/// Dvorak simplified, US variant. Same physical keys, letters moved.
static DVORAK: &[(u8, char, char)] = &[
    (0x02, '1', '!'), (0x03, '2', '@'), (0x04, '3', '#'), (0x05, '4', '$'),
    (0x06, '5', '%'), (0x07, '6', '^'), (0x08, '7', '&'), (0x09, '8', '*'),
    (0x0A, '9', '('), (0x0B, '0', ')'), (0x0C, '[', '{'), (0x0D, ']', '}'),
    (0x10, '\'', '"'), (0x11, ',', '<'), (0x12, '.', '>'), (0x13, 'p', 'P'),
    (0x14, 'y', 'Y'), (0x15, 'f', 'F'), (0x16, 'g', 'G'), (0x17, 'c', 'C'),
    (0x18, 'r', 'R'), (0x19, 'l', 'L'), (0x1A, '/', '?'), (0x1B, '=', '+'),
    (0x1E, 'a', 'A'), (0x1F, 'o', 'O'), (0x20, 'e', 'E'), (0x21, 'u', 'U'),
    (0x22, 'i', 'I'), (0x23, 'd', 'D'), (0x24, 'h', 'H'), (0x25, 't', 'T'),
    (0x26, 'n', 'N'), (0x27, 's', 'S'), (0x28, '-', '_'),
    (0x2B, '\\', '|'),
    (0x2C, ';', ':'), (0x2D, 'q', 'Q'), (0x2E, 'j', 'J'), (0x2F, 'k', 'K'),
    (0x30, 'x', 'X'), (0x31, 'b', 'B'), (0x32, 'm', 'M'), (0x33, 'w', 'W'),
    (0x34, 'v', 'V'), (0x35, 'z', 'Z'),
    SPACE,
];

// ============================================================
//  Switch key (14.2)
// ============================================================

/// Scancode of the key that flips primary and secondary.
///
/// Set 1, extended prefix stripped: the right Alt key arrives as
/// `E0 38`, and the driver hands the second byte on.
pub const SWITCH_RIGHT_ALT: u8 = 0x38;
/// Right Ctrl, `E0 1D`.
pub const SWITCH_RIGHT_CTRL: u8 = 0x1D;
/// The two Shift keys pressed together is described by its left member.
pub const SWITCH_BOTH_SHIFT: u8 = 0x2A;

/// The key that switches between the two layouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchKey {
    RightAlt,
    RightCtrl,
    BothShift,
    /// No switching: the secondary layout stays unreachable.
    None,
}

impl SwitchKey {
    pub fn as_str(&self) -> &'static str {
        match self {
            SwitchKey::RightAlt => "alt_droit",
            SwitchKey::RightCtrl => "ctrl_droit",
            SwitchKey::BothShift => "deux_maj",
            SwitchKey::None => "aucune",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "alt_droit" | "right_alt" => Some(SwitchKey::RightAlt),
            "ctrl_droit" | "right_ctrl" => Some(SwitchKey::RightCtrl),
            "deux_maj" | "both_shift" => Some(SwitchKey::BothShift),
            "aucune" | "none" => Some(SwitchKey::None),
            _ => None,
        }
    }

    /// Scancode this key sends, when it has one.
    pub fn scancode(&self) -> Option<u8> {
        match self {
            SwitchKey::RightAlt => Some(SWITCH_RIGHT_ALT),
            SwitchKey::RightCtrl => Some(SWITCH_RIGHT_CTRL),
            SwitchKey::BothShift => Some(SWITCH_BOTH_SHIFT),
            SwitchKey::None => None,
        }
    }

    /// Does this scancode, seen extended or not, trigger the switch?
    ///
    /// The extended flag is part of the answer: `E0 38` is the right Alt,
    /// a bare `38` is the left one. Ignoring it would make the left Alt
    /// switch the layout, which is exactly the key an application menu
    /// uses.
    pub fn triggered_by(&self, scancode: u8, extended: bool) -> bool {
        match self {
            SwitchKey::RightAlt => extended && scancode == SWITCH_RIGHT_ALT,
            SwitchKey::RightCtrl => extended && scancode == SWITCH_RIGHT_CTRL,
            SwitchKey::BothShift => !extended && scancode == SWITCH_BOTH_SHIFT,
            SwitchKey::None => false,
        }
    }
}

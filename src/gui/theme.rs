//! 7.5 -- Theme and customisation (spec 8.4).
//!
//! ## What 8.4 imposes
//!
//! "A `theme.cfg` file per user controls the colours, fonts, rounded
//! corners and dimensions." Four families of settings, and one priority
//! rule: what comes from the user wins over what comes from the system.
//!
//! ## Format
//!
//! One key per line, `key = value`, `#` for a comment. No sectioned INI,
//! no TOML: the kernel has no parser and wants none. An unknown key is
//! **silently ignored** rather than refused -- a theme written for a
//! later version must stay usable, even if some settings do not take.
//!
//! Colours are written `#RRGGBB`. The numbers are decimal.
//!
//! ## Priority (7.5.1)
//!
//! 1. `/users/{user}/settings/gui/theme.cfg`
//! 2. `/sys/gui/themes/theme.cfg`
//! 3. the default values compiled here
//!
//! A **partial** user theme is the normal case: one only overrides what
//! one wants to change. The three levels therefore stack instead of
//! replacing one another, which is also the reason a missing key is not
//! an error.

use alloc::string::String;

use crate::fs::perm::Credentials;
use crate::graphics::framebuffer::{Color, rgb};

/// Path of the system theme.
pub const SYSTEM_THEME: &str = "/sys/gui/themes/theme.cfg";

/// Path of the theme of an account, relative to its directory.
pub const USER_THEME: &str = "settings/gui/theme.cfg";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    // --- colours ---
    pub background: Color,
    pub surface: Color,
    pub surface_alt: Color,
    pub accent: Color,
    pub text: Color,
    pub text_dim: Color,
    pub border: Color,
    pub title_active: Color,
    pub title_inactive: Color,
    pub danger: Color,
    pub warning: Color,
    pub success: Color,

    // --- dimensions ---
    pub taskbar_height: u16,
    pub title_height: u16,
    pub border_width: u16,
    pub corner_radius: u16,
    pub padding: u16,

    // --- font ---
    /// Name of the font asked for. Only one is embedded; the field exists
    /// so that the file is complete and that the day a second font
    /// arrives, no theme has to be rewritten.
    pub font: [u8; 24],
    pub font_scale: u8,
}

impl Theme {
    /// Default theme: dark, high contrast.
    ///
    /// Dark because a full-screen light background is the worst case for
    /// a framebuffer that republishes a lot, and because the boot log is
    /// displayed on it since phase 7.1.
    pub const fn dark() -> Self {
        Theme {
            background: rgb(0x10, 0x12, 0x18),
            surface: rgb(0x1C, 0x20, 0x28),
            surface_alt: rgb(0x26, 0x2B, 0x35),
            accent: rgb(0x4A, 0x9E, 0xFF),
            text: rgb(0xE6, 0xE9, 0xEF),
            text_dim: rgb(0x8A, 0x92, 0xA3),
            border: rgb(0x3A, 0x41, 0x4E),
            title_active: rgb(0x2E, 0x3A, 0x50),
            title_inactive: rgb(0x1C, 0x20, 0x28),
            danger: rgb(0xE0, 0x50, 0x50),
            warning: rgb(0xE0, 0xA0, 0x30),
            success: rgb(0x50, 0xC0, 0x70),

            taskbar_height: 28,
            title_height: 20,
            border_width: 1,
            corner_radius: 4,
            padding: 6,

            font: *b"systeme\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
            font_scale: 1,
        }
    }

    pub fn font_name(&self) -> &str {
        let end = self.font.iter().position(|b| *b == 0).unwrap_or(self.font.len());
        core::str::from_utf8(&self.font[..end]).unwrap_or("systeme")
    }

    /// Applies a `key = value` line. Returns `true` if the key was known
    /// - which the self-test checks, and which the log reports.
    pub fn apply(&mut self, line: &str) -> bool {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return false;
        }
        let Some((key, value)) = line.split_once('=') else {
            return false;
        };
        let key = key.trim();
        let value = value.trim();

        if let Some(color) = parse_color(value) {
            let slot: Option<&mut Color> = match key {
                "background" => Some(&mut self.background),
                "surface" => Some(&mut self.surface),
                "surface_alt" => Some(&mut self.surface_alt),
                "accent" => Some(&mut self.accent),
                "text" => Some(&mut self.text),
                "text_dim" => Some(&mut self.text_dim),
                "border" => Some(&mut self.border),
                "title_active" => Some(&mut self.title_active),
                "title_inactive" => Some(&mut self.title_inactive),
                "danger" => Some(&mut self.danger),
                "warning" => Some(&mut self.warning),
                "success" => Some(&mut self.success),
                _ => None,
            };
            if let Some(slot) = slot {
                *slot = color;
                return true;
            }
        }

        if let Some(number) = parse_number(value) {
            let slot: Option<&mut u16> = match key {
                "taskbar_height" => Some(&mut self.taskbar_height),
                "title_height" => Some(&mut self.title_height),
                "border_width" => Some(&mut self.border_width),
                "corner_radius" => Some(&mut self.corner_radius),
                "padding" => Some(&mut self.padding),
                _ => None,
            };
            if let Some(slot) = slot {
                // Upper bound: a theme asking for a ten-thousand-pixel
                // task bar would mask the whole screen, and nothing in
                // the rendering would catch it.
                *slot = number.min(512) as u16;
                return true;
            }
            if key == "font_scale" {
                self.font_scale = number.clamp(1, 4) as u8;
                return true;
            }
        }

        if key == "font" {
            self.font = [0; 24];
            for (index, byte) in value.bytes().take(23).enumerate() {
                self.font[index] = if (0x20..=0x7e).contains(&byte) { byte } else { b'?' };
            }
            return true;
        }

        false
    }

    /// Loads a complete theme from a text. Returns the number of keys
    /// recognised.
    pub fn merge(&mut self, text: &str) -> usize {
        text.lines().filter(|line| self.apply(line)).count()
    }

    /// Serialises the theme in the form `theme.cfg` accepts as input.
    ///
    /// Exact round trip: it is what allows the writing of the reference
    /// file to be generated by the code rather than copied by hand, and
    /// therefore never to diverge from the structure.
    pub fn to_cfg(&self) -> String {
        use core::fmt::Write;
        let mut out = String::new();
        let _ = writeln!(out, "# Theme Nova OS - spec 8.4");
        let _ = writeln!(out, "# Couleurs en #RRGGBB, dimensions en pixels.");
        let _ = writeln!(out);
        for (key, color) in self.colors() {
            let _ = writeln!(
                out,
                "{} = #{:02X}{:02X}{:02X}",
                key,
                (color >> 16) as u8,
                (color >> 8) as u8,
                color as u8
            );
        }
        let _ = writeln!(out);
        let _ = writeln!(out, "taskbar_height = {}", self.taskbar_height);
        let _ = writeln!(out, "title_height = {}", self.title_height);
        let _ = writeln!(out, "border_width = {}", self.border_width);
        let _ = writeln!(out, "corner_radius = {}", self.corner_radius);
        let _ = writeln!(out, "padding = {}", self.padding);
        let _ = writeln!(out, "font = {}", self.font_name());
        let _ = writeln!(out, "font_scale = {}", self.font_scale);
        out
    }

    pub fn colors(&self) -> [(&'static str, Color); 12] {
        [
            ("background", self.background),
            ("surface", self.surface),
            ("surface_alt", self.surface_alt),
            ("accent", self.accent),
            ("text", self.text),
            ("text_dim", self.text_dim),
            ("border", self.border),
            ("title_active", self.title_active),
            ("title_inactive", self.title_inactive),
            ("danger", self.danger),
            ("warning", self.warning),
            ("success", self.success),
        ]
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::dark()
    }
}

/// `#RRGGBB`, with or without the hash.
fn parse_color(value: &str) -> Option<Color> {
    let digits = value.strip_prefix('#').unwrap_or(value);
    if digits.len() != 6 {
        return None;
    }
    let mut channels = [0u8; 3];
    for (index, channel) in channels.iter_mut().enumerate() {
        let high = hex(digits.as_bytes()[index * 2])?;
        let low = hex(digits.as_bytes()[index * 2 + 1])?;
        *channel = high << 4 | low;
    }
    Some(rgb(channels[0], channels[1], channels[2]))
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_number(value: &str) -> Option<u32> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

// ============================================================
//  Loading from the filesystem
// ============================================================

/// Result of a load, so that the log says where the theme comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Origin {
    pub system_keys: usize,
    pub user_keys: usize,
    pub user_present: bool,
}

/// 7.5.1 -- Composes the effective theme of an account.
///
/// The order of application **is** the priority rule: the user theme goes
/// last, so it wins. No code compares the two sources; that is what makes
/// the rule impossible to break by inattention.
pub fn load_for(user: Option<&str>) -> (Theme, Origin) {
    let mut theme = Theme::dark();
    let mut origin = Origin {
        system_keys: 0,
        user_keys: 0,
        user_present: false,
    };

    if !crate::fs::is_ready() {
        return (theme, origin);
    }
    let who = Credentials::kernel();

    if let Ok(bytes) = crate::fs::read_file(SYSTEM_THEME, who)
        && let Ok(text) = core::str::from_utf8(&bytes)
    {
        origin.system_keys = theme.merge(text);
    }

    if let Some(user) = user {
        let path = alloc::format!("/users/{}/{}", user, USER_THEME);
        if let Ok(bytes) = crate::fs::read_file(&path, who)
            && let Ok(text) = core::str::from_utf8(&bytes)
        {
            origin.user_present = true;
            origin.user_keys = theme.merge(text);
        }
    }

    (theme, origin)
}

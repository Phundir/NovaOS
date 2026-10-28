//! Audio settings of an account: output volume and mute, input source,
//! gain and mute.
//!
//! ## Per account, like the theme and the language
//!
//! Stored in `/users/{user}/settings/audio.cfg` and applied when the
//! session opens -- the same arrangement as `theme.cfg` (8.4), the locale
//! (14.1) and the accessibility settings (16). Logging out gives the
//! defaults back, so that the next person does not inherit a muted
//! microphone they cannot find the reason for.
//!
//! ## The file
//!
//! ```text
//! sortie_volume = 80        percent, 0..100
//! sortie_coupee = non
//! entree_source = micro     micro | ligne
//! entree_gain = 100         percent, 0..400
//! entree_coupee = non
//! ```
//!
//! An unknown key is ignored and a malformed value keeps the default: a
//! settings file edited by hand must not be able to silence the machine
//! by a typo.

use alloc::format;
use alloc::string::String;

use crate::fs::perm::{Credentials, Mode};
use crate::vga::SpinLock;

use super::VOLUME_UNITY;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Microphone,
    Line,
}

impl Source {
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Microphone => "micro",
            Source::Line => "ligne",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Source::Microphone => "Microphone",
            Source::Line => "Entree ligne",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// Percent, 0..=100.
    pub output_volume: u32,
    pub output_muted: bool,
    pub input_source: Source,
    /// Percent, 0..=400.
    pub input_gain: u32,
    pub input_muted: bool,
}

impl Settings {
    pub const DEFAULT: Settings = Settings {
        output_volume: 80,
        output_muted: false,
        input_source: Source::Microphone,
        input_gain: 100,
        input_muted: false,
    };

    pub fn parse(text: &str) -> Settings {
        let mut settings = Settings::DEFAULT;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            let flag = match value {
                "oui" => Some(true),
                "non" => Some(false),
                _ => None,
            };
            let number = value.parse::<u32>().ok();
            match key {
                "sortie_volume" => {
                    if let Some(n) = number.filter(|n| *n <= 100) {
                        settings.output_volume = n;
                    }
                }
                "sortie_coupee" => {
                    if let Some(f) = flag {
                        settings.output_muted = f;
                    }
                }
                "entree_source" => match value {
                    "micro" => settings.input_source = Source::Microphone,
                    "ligne" => settings.input_source = Source::Line,
                    _ => {}
                },
                "entree_gain" => {
                    if let Some(n) = number.filter(|n| *n <= 400) {
                        settings.input_gain = n;
                    }
                }
                "entree_coupee" => {
                    if let Some(f) = flag {
                        settings.input_muted = f;
                    }
                }
                _ => {}
            }
        }
        settings
    }

    pub fn serialize(&self) -> String {
        let flag = |value: bool| if value { "oui" } else { "non" };
        format!(
            "# Reglages audio - ecrit par reglages-audio.ss\n\
             sortie_volume = {}\n\
             sortie_coupee = {}\n\
             entree_source = {}\n\
             entree_gain = {}\n\
             entree_coupee = {}\n",
            self.output_volume,
            flag(self.output_muted),
            self.input_source.as_str(),
            self.input_gain,
            flag(self.input_muted)
        )
    }
}

/// Settings in force.
static CURRENT: SpinLock<Settings> = SpinLock::new(Settings::DEFAULT);

pub fn current() -> Settings {
    *CURRENT.lock()
}

fn path_of(user: &str) -> String {
    format!("/users/{}/settings/audio.cfg", user)
}

/// Percent of full scale, as the fraction over 256 the mixer uses.
pub fn percent_to_volume(percent: u32) -> u32 {
    percent * VOLUME_UNITY / 100
}

/// Makes `settings` the settings in force: mixer, capture, codec.
pub fn apply(settings: Settings) {
    super::set_master(percent_to_volume(settings.output_volume));
    super::set_muted(settings.output_muted);
    super::capture::set_gain(percent_to_volume(settings.input_gain));
    super::capture::set_muted(settings.input_muted);
    crate::drivers::audio::set_input_source(settings.input_source);
    *CURRENT.lock() = settings;
}

/// Reads the settings of an account, or the defaults.
pub fn load(user: Option<&str>) -> Settings {
    let Some(user) = user else {
        return Settings::DEFAULT;
    };
    match crate::fs::read_file(&path_of(user), Credentials::kernel()) {
        Ok(bytes) => Settings::parse(core::str::from_utf8(&bytes).unwrap_or("")),
        Err(_) => Settings::DEFAULT,
    }
}

/// Writes the settings of an account.
///
/// With the kernel's credentials, like `i18n` and `access` write theirs:
/// the file sits in the account's own settings directory, which the
/// settings panel is the only writer of.
pub fn save(user: &str, settings: &Settings) -> Result<(), crate::fs::FsError> {
    crate::fs::write_file(
        &path_of(user),
        settings.serialize().as_bytes(),
        Credentials::kernel(),
        Mode::private_file(),
        crate::idt::ticks(),
    )
    .map(|_| ())
}

/// Applies an account's settings when its session opens.
pub fn adopt_user(user: &str) {
    let settings = load(Some(user));
    apply(settings);
    crate::serial_println!(
        "[audio] session {} - sortie {}%{}, entree {} {}%{}",
        user,
        settings.output_volume,
        if settings.output_muted { " (coupee)" } else { "" },
        settings.input_source.as_str(),
        settings.input_gain,
        if settings.input_muted { " (coupee)" } else { "" }
    );
}

/// Gives the defaults back, at log out.
pub fn reset() {
    apply(Settings::DEFAULT);
}

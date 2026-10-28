//! 10.2.5 -- Time zones (spec 14.2, "Fuseau horaire" row).
//!
//! ## Store in UTC, convert at display
//!
//! 14.2 is explicit: "all dates stored in UTC, converted at display".
//! That rule is what keeps a file written in Paris and read in Tokyo
//! from moving in time. So nothing here touches the stored instant: a
//! zone is an **offset in minutes** applied on the way out, and taken off
//! again on the way in.
//!
//! ## Why minutes and not hours
//!
//! Not every zone is a whole number of hours from UTC. India is at
//! +05:30, Nepal at +05:45, the Chatham Islands at +12:45. A table in
//! hours would be wrong for a good part of the planet, and wrong in a way
//! nobody notices until a timestamp lands forty-five minutes off.
//!
//! ## What this does not do
//!
//! **No daylight saving.** A summer-time rule is a piece of legislation
//! that changes, carried by a database that is updated several times a
//! year (the IANA `tzdata`). Embedding a frozen copy in a kernel would
//! give an answer that is confidently wrong half the year. The zones
//! below carry their **standard** offset, the summer variants are listed
//! as separate zones, and the choice belongs to the user. It is the same
//! honesty as `sdk::runtime` refusing a compiler it does not have.

use alloc::string::String;

/// A zone: identifier, offset from UTC in minutes, printable label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zone {
    pub id: &'static str,
    pub offset_minutes: i16,
    pub label: &'static str,
}

/// The zones the system knows.
///
/// Deliberately short: it covers the offsets rather than the cities. A
/// complete list is a database, not a table, and it belongs on disk with
/// the rest of `/sys/i18n/`.
pub static ZONES: &[Zone] = &[
    Zone { id: "UTC", offset_minutes: 0, label: "Temps universel coordonne" },
    Zone { id: "Europe/London", offset_minutes: 0, label: "Royaume-Uni (hiver)" },
    Zone { id: "Europe/Paris", offset_minutes: 60, label: "Europe centrale (hiver)" },
    Zone { id: "Europe/Paris-ete", offset_minutes: 120, label: "Europe centrale (ete)" },
    Zone { id: "Europe/Athens", offset_minutes: 120, label: "Europe de l'Est (hiver)" },
    Zone { id: "Europe/Moscow", offset_minutes: 180, label: "Moscou" },
    Zone { id: "Asia/Dubai", offset_minutes: 240, label: "Golfe" },
    Zone { id: "Asia/Kolkata", offset_minutes: 330, label: "Inde" },
    Zone { id: "Asia/Kathmandu", offset_minutes: 345, label: "Nepal" },
    Zone { id: "Asia/Shanghai", offset_minutes: 480, label: "Chine" },
    Zone { id: "Asia/Tokyo", offset_minutes: 540, label: "Japon" },
    Zone { id: "Australia/Sydney", offset_minutes: 600, label: "Australie orientale (hiver)" },
    Zone { id: "Pacific/Chatham", offset_minutes: 765, label: "Iles Chatham" },
    Zone { id: "Atlantic/Azores", offset_minutes: -60, label: "Acores" },
    Zone { id: "America/Sao_Paulo", offset_minutes: -180, label: "Bresil (est)" },
    Zone { id: "America/New_York", offset_minutes: -300, label: "Etats-Unis (est, hiver)" },
    Zone { id: "America/Chicago", offset_minutes: -360, label: "Etats-Unis (centre, hiver)" },
    Zone { id: "America/Denver", offset_minutes: -420, label: "Etats-Unis (montagnes, hiver)" },
    Zone { id: "America/Los_Angeles", offset_minutes: -480, label: "Etats-Unis (ouest, hiver)" },
    Zone { id: "Pacific/Honolulu", offset_minutes: -600, label: "Hawai" },
];

/// Looks a zone up by identifier.
pub fn find(id: &str) -> Option<&'static Zone> {
    ZONES.iter().find(|zone| zone.id == id)
}

/// The zone used when the configuration names none, or names one that
/// does not exist. UTC and not a local zone: an unknown setting must not
/// silently shift every timestamp of the machine.
pub fn default_zone() -> &'static Zone {
    &ZONES[0]
}

impl Zone {
    /// Converts a UTC instant into local wall time.
    ///
    /// Saturating rather than wrapping: an instant close to zero shifted
    /// by a negative offset would otherwise wrap to the far end of the
    /// epoch, and print a date in the year 584 billion.
    pub fn to_local(&self, epoch_utc: u64) -> u64 {
        let shift = self.offset_minutes as i64 * 60;
        if shift >= 0 {
            epoch_utc.saturating_add(shift as u64)
        } else {
            epoch_utc.saturating_sub(shift.unsigned_abs())
        }
    }

    /// Converts a local wall time back into a UTC instant.
    pub fn to_utc(&self, epoch_local: u64) -> u64 {
        let shift = self.offset_minutes as i64 * 60;
        if shift >= 0 {
            epoch_local.saturating_sub(shift as u64)
        } else {
            epoch_local.saturating_add(shift.unsigned_abs())
        }
    }

    /// The IANA identifier Linux programs are given for this zone
    /// (`/etc/localtime`, read by musl and by the Web Browser's ICU).
    ///
    /// Nova's zones are fixed offsets, the summer ones listed apart; an
    /// IANA zone like `Europe/Paris` carries its own summer rule. Handing
    /// the latter to the browser would give two clocks on one screen an
    /// hour apart half the year whenever the user's choice and the
    /// legislation disagree. So a zone that has a summer time somewhere
    /// becomes the fixed-offset `Etc/GMT-N` of the offset chosen (POSIX
    /// sign, inverted: `Etc/GMT-2` is UTC+02:00), and the browser shows
    /// exactly what the taskbar shows. Zones without summer time keep
    /// their name. Chatham, at a quarter-hour offset `Etc/` cannot
    /// express, keeps its own.
    pub fn linux_id(&self) -> String {
        const NO_SUMMER_TIME: [&str; 9] = [
            "UTC",
            "Europe/Moscow",
            "Asia/Dubai",
            "Asia/Kolkata",
            "Asia/Kathmandu",
            "Asia/Shanghai",
            "Asia/Tokyo",
            "America/Sao_Paulo",
            "Pacific/Honolulu",
        ];
        if NO_SUMMER_TIME.contains(&self.id) || self.offset_minutes % 60 != 0 {
            return String::from(self.id.trim_end_matches("-ete"));
        }
        let hours = self.offset_minutes / 60;
        if hours == 0 {
            String::from("Etc/GMT")
        } else {
            // Inverted sign: POSIX counts westward.
            alloc::format!("Etc/GMT{}{}", if hours > 0 { '-' } else { '+' }, hours.unsigned_abs())
        }
    }

    /// `UTC+02:00` form. The sign is always written, including for UTC
    /// itself: a bare `UTC` beside `UTC+01:00` reads as a missing value.
    pub fn offset_text(&self) -> String {
        let sign = if self.offset_minutes < 0 { '-' } else { '+' };
        let magnitude = self.offset_minutes.unsigned_abs();
        alloc::format!("UTC{}{:02}:{:02}", sign, magnitude / 60, magnitude % 60)
    }
}

//! CMOS real-time clock (dependency of step 5.6.3).
//!
//! Until now Nova OS could only count ticks since boot. That is enough to
//! schedule, and not enough for anything else: node dates were worth the
//! tick counter, the `valid_from` / `valid_until` certificate fields were
//! never compared, and the exports carried a conventional date. This
//! module closes that point.
//!
//! ## The hardware
//!
//! The CMOS is addressed through two ports: a register number is written
//! to 0x70, its value read or written at 0x71. Bit 7 of the index port
//! controls the masking of non-maskable interrupts -- it must be
//! **restored as it was found**, on pain of leaving the machine deaf to
//! hardware errors.
//!
//! ## The two traps
//!
//! **The update in progress.** The circuit spends a fraction of a second
//! per second copying its counters. Reading during that window gives a
//! mix of old and new: 01:59:60, or worse, an hour change with no minute
//! change. Bit 7 of register A signals the update; the counter-measure
//! kept here is to read twice in a row and accept only two identical
//! readings, which also covers the case where the update starts just
//! after the test.
//!
//! **The encoding.** Depending on the BIOS, the values are binary or
//! binary-coded decimal, and the hour on 12 or 24 hours. Register B says
//! so; nothing else allows guessing it, and a BCD read as binary gives
//! plausible but wrong dates -- the kind of error one only notices on the
//! twelfth day of the month.
//!
//! ## What this module does not do
//!
//! It corrects no time zone: the CMOS is read as-is, and the
//! specification says nothing of the expected zone. The convention kept
//! is **UTC**, that of machines that have never booted Windows. Nor does
//! it handle the periodic RTC interrupt (IRQ 8): the current time is
//! derived from the initial reading and the tick counter, which avoids a
//! second timer without losing any useful precision.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::port::{inb, outb};

const CMOS_INDEX: u16 = 0x70;
const CMOS_DATA: u16 = 0x71;

const REG_SECONDS: u8 = 0x00;
const REG_MINUTES: u8 = 0x02;
const REG_HOURS: u8 = 0x04;
const REG_DAY: u8 = 0x07;
const REG_MONTH: u8 = 0x08;
const REG_YEAR: u8 = 0x09;
/// Century register, when the firmware offers one.
const REG_CENTURY: u8 = 0x32;
const REG_STATUS_A: u8 = 0x0A;
const REG_STATUS_B: u8 = 0x0B;

/// Bit 7 of register A: an update is in progress.
const STATUS_A_UPDATING: u8 = 0x80;
/// Bit 1 of register B: the hour is on 24 hours.
const STATUS_B_24H: u8 = 0x02;
/// Bit 2 of register B: the values are binary, not BCD.
const STATUS_B_BINARY: u8 = 0x04;

/// Wall clock at tick zero, in milliseconds since the Unix epoch: the
/// current time is this plus the ticks elapsed. One word rather than an
/// (epoch, tick) pair, so that a correction (`set_now_millis`, network
/// time) can never be seen half-written by a concurrent reader.
static EPOCH_AT_TICK_ZERO_MS: AtomicU64 = AtomicU64::new(0);
/// Was the clock read successfully (or set from the network)?
static AVAILABLE: AtomicU64 = AtomicU64::new(0);

// ============================================================
//  CMOS access
// ============================================================

/// Reads a CMOS register while preserving the NMI masking state.
///
/// # Safety
/// Direct access to I/O ports. The caller must guarantee that no other
/// code handles the CMOS at the same time.
unsafe fn read_register(register: u8) -> u8 {
    unsafe {
        // The current bit 7 is read back and rewritten as-is: clearing it
        // would unmask the NMIs, setting it would mask them, and neither
        // is our business.
        let nmi = inb(CMOS_INDEX) & 0x80;
        outb(CMOS_INDEX, nmi | (register & 0x7F));
        inb(CMOS_DATA)
    }
}

/// Converts a binary-coded decimal value.
fn from_bcd(value: u8) -> u8 {
    (value & 0x0F) + ((value >> 4) * 10)
}

// ============================================================
//  Date and time
// ============================================================

/// Wall date and time, as the CMOS carries them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl DateTime {
    /// Seconds elapsed since 1 January 1970 at midnight UTC.
    ///
    /// The day-number computation follows the civil-days algorithm: the
    /// year is shifted to start in March, which puts 29 February at the
    /// end of the cycle and removes any special handling of leap years.
    pub fn to_epoch(&self) -> u64 {
        let year = self.year as i64;
        let month = self.month as i64;
        let day = self.day as i64;

        let shifted = year - i64::from(month <= 2);
        let era = if shifted >= 0 { shifted } else { shifted - 399 } / 400;
        let year_of_era = shifted - era * 400;
        let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
        let day_of_era =
            year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        let days = era * 146_097 + day_of_era - 719_468;

        let seconds = days * 86_400
            + self.hour as i64 * 3600
            + self.minute as i64 * 60
            + self.second as i64;
        seconds.max(0) as u64
    }

    /// Rebuilds a date from a Unix instant.
    pub fn from_epoch(epoch: u64) -> Self {
        let days = (epoch / 86_400) as i64;
        let rest = epoch % 86_400;

        let shifted = days + 719_468;
        let era = if shifted >= 0 { shifted } else { shifted - 146_096 } / 146_097;
        let day_of_era = shifted - era * 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let year = year_of_era + era * 400;
        let day_of_year =
            day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let shifted_month = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
        let month = shifted_month + if shifted_month < 10 { 3 } else { -9 };

        DateTime {
            year: (year + i64::from(month <= 2)) as u16,
            month: month as u8,
            day: day as u8,
            hour: (rest / 3600) as u8,
            minute: ((rest % 3600) / 60) as u8,
            second: (rest % 60) as u8,
        }
    }

    /// `YYYY-MM-DD HH:MM:SS` format, printable ASCII only.
    pub fn format(&self) -> [u8; 19] {
        let mut out = *b"0000-00-00 00:00:00";
        let two = |value: u8| [b'0' + value / 10, b'0' + value % 10];

        out[0] = b'0' + (self.year / 1000 % 10) as u8;
        out[1] = b'0' + (self.year / 100 % 10) as u8;
        out[2] = b'0' + (self.year / 10 % 10) as u8;
        out[3] = b'0' + (self.year % 10) as u8;
        out[5..7].copy_from_slice(&two(self.month));
        out[8..10].copy_from_slice(&two(self.day));
        out[11..13].copy_from_slice(&two(self.hour));
        out[14..16].copy_from_slice(&two(self.minute));
        out[17..19].copy_from_slice(&two(self.second));
        out
    }
}

// ============================================================
//  Reading
// ============================================================

/// Reads every counter once, with no stability check.
///
/// # Safety
/// See `read_register`.
unsafe fn sample() -> DateTime {
    unsafe {
        DateTime {
            year: read_register(REG_YEAR) as u16,
            month: read_register(REG_MONTH),
            day: read_register(REG_DAY),
            hour: read_register(REG_HOURS),
            minute: read_register(REG_MINUTES),
            second: read_register(REG_SECONDS),
        }
    }
}

/// Reads the current date and time from the CMOS.
///
/// # Safety
/// Accesses ports 0x70 and 0x71. To be called at boot only, before
/// several cores are running, or under a global lock.
pub unsafe fn read() -> Option<DateTime> {
    unsafe {
        // We let an update in progress pass rather than read through it.
        // The iteration bound avoids spinning forever if the circuit is
        // absent -- under some emulators, reading everything gives 0xFF,
        // including the update bit.
        let mut guard = 0;
        while read_register(REG_STATUS_A) & STATUS_A_UPDATING != 0 {
            guard += 1;
            if guard > 1_000_000 {
                return None;
            }
        }

        // Two identical readings: the only proof that no update slipped
        // in between the test above and the reading.
        let mut previous = sample();
        let mut raw;
        let mut attempts = 0;
        loop {
            raw = sample();
            if raw == previous {
                break;
            }
            previous = raw;
            attempts += 1;
            if attempts > 16 {
                return None;
            }
        }

        let status = read_register(REG_STATUS_B);
        let binary = status & STATUS_B_BINARY != 0;
        let hour_24 = status & STATUS_B_24H != 0;

        let decode = |value: u8| if binary { value } else { from_bcd(value) };

        // Bit 7 of the hour marks the afternoon in 12-hour mode, and it
        // survives BCD decoding: it must be removed before, and reapplied
        // after.
        let raw_hour = raw.hour;
        let mut hour = decode(raw_hour & 0x7F);
        if !hour_24 && raw_hour & 0x80 != 0 {
            hour = (hour % 12) + 12;
        } else if !hour_24 && hour == 12 {
            hour = 0;
        }

        // The century is only reliable if the firmware exposes it. An
        // absurd value means that register serves something else on this
        // machine; we then fall back on the usual convention, which puts
        // a two-digit year between 1970 and 2069.
        let century = decode(read_register(REG_CENTURY)) as u16;
        let short_year = decode(raw.year as u8) as u16;
        let year = if (19..=21).contains(&century) {
            century * 100 + short_year
        } else if short_year >= 70 {
            1900 + short_year
        } else {
            2000 + short_year
        };

        let value = DateTime {
            year,
            month: decode(raw.month),
            day: decode(raw.day),
            hour,
            minute: decode(raw.minute),
            second: decode(raw.second),
        };

        // Plausibility check: an absent CMOS returns 0xFF everywhere,
        // which gives an impossible date rather than an outright error.
        if value.month == 0
            || value.month > 12
            || value.day == 0
            || value.day > 31
            || value.hour > 23
            || value.minute > 59
            || value.second > 59
            || value.year < 1970
            || value.year > 2200
        {
            return None;
        }

        Some(value)
    }
}

// ============================================================
//  Initialisation and current time
// ============================================================

/// Reads the clock once and pins the system time to it.
///
/// # Safety
/// To be called once only at boot, before the application cores are
/// launched.
pub unsafe fn init() {
    match unsafe { read() } {
        Some(now) => {
            let elapsed_ms = crate::idt::ticks() * (1000 / crate::TIMER_HZ as u64);
            EPOCH_AT_TICK_ZERO_MS.store((now.to_epoch() * 1000).saturating_sub(elapsed_ms), Ordering::Relaxed);
            AVAILABLE.store(1, Ordering::Relaxed);

            let text = now.format();
            crate::serial_println!(
                "[rtc] horloge CMOS : {} UTC ({} secondes depuis 1970)",
                core::str::from_utf8(&text).unwrap_or("?"),
                now.to_epoch()
            );
        }
        None => {
            // With no clock, everything depending on a date must know it
            // rather than receive an invented value.
            crate::serial_println!(
                "[rtc] aucune horloge CMOS exploitable - le temps mural reste indisponible"
            );
        }
    }
}

/// Is the wall clock available?
pub fn is_available() -> bool {
    AVAILABLE.load(Ordering::Relaxed) != 0
}

/// Current instant in seconds since the Unix epoch.
///
/// Derived from the initial reading and the tick counter, at 100 Hz. The
/// drift is that of the APIC timer, calibrated at boot: a few seconds a
/// day, corrected by the network synchronisation (`timesync`) when the
/// machine is online.
pub fn now() -> Option<u64> {
    now_millis().map(|ms| ms / 1000)
}

/// Current instant in milliseconds since the Unix epoch.
pub fn now_millis() -> Option<u64> {
    if !is_available() {
        return None;
    }
    let elapsed_ms = crate::idt::ticks() * (1000 / crate::TIMER_HZ as u64);
    Some(EPOCH_AT_TICK_ZERO_MS.load(Ordering::Relaxed) + elapsed_ms)
}

/// Sets the wall clock to `epoch_ms` (network time, `timesync`). The CMOS
/// is not rewritten: the correction lives until the next boot, which
/// reads the CMOS again and synchronises again.
pub fn set_now_millis(epoch_ms: u64) {
    let elapsed_ms = crate::idt::ticks() * (1000 / crate::TIMER_HZ as u64);
    EPOCH_AT_TICK_ZERO_MS.store(epoch_ms.saturating_sub(elapsed_ms), Ordering::Relaxed);
    AVAILABLE.store(1, Ordering::Relaxed);
}

/// Current date and time.
pub fn now_datetime() -> Option<DateTime> {
    now().map(DateTime::from_epoch)
}

pub fn dump_state() {
    match now_datetime() {
        Some(value) => {
            let text = value.format();
            crate::serial_println!(
                "[rtc] {} UTC - {} secondes depuis 1970, cale sur le compteur de tics",
                core::str::from_utf8(&text).unwrap_or("?"),
                now().unwrap_or(0)
            );
        }
        None => crate::serial_println!("[rtc] horloge murale indisponible"),
    }
}

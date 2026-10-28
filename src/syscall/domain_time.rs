//! 5.6.3 -- Domain 0x07, time (spec 6.6).
//!
//! "Clock, all types": this domain is the only one the specification
//! opens without restriction, and that is justified -- knowing the time
//! is not a privilege.
//!
//! ## Two scales, never confused
//!
//! The tick counter measures the time **elapsed since boot**: monotonic,
//! insensitive to the clock, it is what is needed to measure a duration.
//! The wall clock gives the **absolute** instant: it is what is needed to
//! date a file. Both are exposed separately and never mix; a program
//! measuring a duration by subtracting two wall dates would be caught out
//! the day a resynchronisation exists.
//!
//! When the CMOS could not be read, the wall calls fail with
//! `UNSUPPORTED` instead of returning an invented date. A wrong date is
//! worse than an absent one: it propagates into the files.
//!
//! ## Sleeping
//!
//! `SLEEP_MS` is the third blocking call of the system. Its
//! implementation comes down to a list of wake-ups the timer tick of core
//! 0 walks. A list, and not a sorted queue: the sleepers can be counted
//! on one hand, and a linear walk of three elements every ten
//! milliseconds costs less than an ordered structure to maintain under a
//! lock in an interrupt handler.

use alloc::vec::Vec;

use super::error::{self, ErrorCode, SysError, SysResult};
use super::table::{Entry, entry};
use super::Call;
use crate::process::Pid;
use crate::vga::SpinLock;

pub const SYS_TIME_TICKS: u16 = 0x0701;
pub const SYS_TIME_UPTIME_MS: u16 = 0x0702;
pub const SYS_TIME_NOW: u16 = 0x0703;
pub const SYS_TIME_NOW_MS: u16 = 0x0704;
pub const SYS_TIME_DATE: u16 = 0x0705;
pub const SYS_TIME_SLEEP_MS: u16 = 0x0706;
pub const SYS_TIME_RESOLUTION: u16 = 0x0707;

pub static TABLE: &[Entry] = &[
    entry!(SYS_TIME_TICKS, "TICKS", Sync, ticks),
    entry!(SYS_TIME_UPTIME_MS, "UPTIME_MS", Sync, uptime_ms),
    entry!(SYS_TIME_NOW, "NOW", Sync, now),
    entry!(SYS_TIME_NOW_MS, "NOW_MS", Sync, now_ms),
    entry!(SYS_TIME_DATE, "DATE", Sync, date),
    entry!(SYS_TIME_SLEEP_MS, "SLEEP_MS", Async, sleep_ms),
    entry!(SYS_TIME_RESOLUTION, "RESOLUTION", Sync, resolution),
];

/// Maximum duration of a sleep: one hour.
///
/// Beyond that, it is almost always a confused unit -- microseconds taken
/// for milliseconds. Refusing outright is worth more than putting a task
/// to sleep for three weeks.
const SLEEP_MAX_MS: u64 = 3_600_000;

fn ticks(_call: &mut Call) -> SysResult<u64> {
    Ok(crate::idt::ticks())
}

fn uptime_ms(_call: &mut Call) -> SysResult<u64> {
    Ok(crate::idt::ticks() * (1000 / crate::TIMER_HZ as u64))
}

fn wall_clock() -> SysResult<u64> {
    crate::rtc::now().ok_or_else(|| {
        SysError::with(
            ErrorCode::Unsupported,
            "aucune horloge CMOS exploitable sur cette machine",
        )
    })
}

fn now(_call: &mut Call) -> SysResult<u64> {
    wall_clock()
}

fn now_ms(_call: &mut Call) -> SysResult<u64> {
    crate::rtc::now_millis().ok_or_else(|| {
        SysError::with(ErrorCode::Unsupported, "aucune horloge CMOS exploitable")
    })
}

/// Layout of the decomposed date.
///
/// ```text
///   0x00  8  seconds since the Unix epoch
///   0x08  2  year
///   0x0A  1  month, from 1 to 12
///   0x0B  1  day, from 1 to 31
///   0x0C  1  hour
///   0x0D  1  minute
///   0x0E  1  second
///   0x0F  1  reserved
///   0x10 19  YYYY-MM-DD HH:MM:SS representation
/// ```
pub const DATE_SIZE: usize = 0x28;

fn date(call: &mut Call) -> SysResult<u64> {
    let epoch = wall_clock()?;
    let value = crate::rtc::DateTime::from_epoch(epoch);

    let mut record = [0u8; DATE_SIZE];
    record[0x00..0x08].copy_from_slice(&epoch.to_le_bytes());
    record[0x08..0x0A].copy_from_slice(&value.year.to_le_bytes());
    record[0x0A] = value.month;
    record[0x0B] = value.day;
    record[0x0C] = value.hour;
    record[0x0D] = value.minute;
    record[0x0E] = value.second;
    record[0x10..0x10 + 19].copy_from_slice(&value.format());

    call.write_bytes(call.arg(0), call.arg(1), &record)
}

fn resolution(_call: &mut Call) -> SysResult<u64> {
    Ok(crate::TIMER_HZ as u64)
}

// ============================================================
//  Sleeping
// ============================================================

/// Sleeping tasks, with the tick at which to wake them.
static SLEEPERS: SpinLock<Vec<(Pid, u64)>> = SpinLock::new(Vec::new());

/// `SLEEP_MS(duration)` -- puts the caller to sleep.
fn sleep_ms(call: &mut Call) -> SysResult<u64> {
    call.require_async("SLEEP_MS")?;

    let duration = call.arg(0);
    if duration == 0 {
        // Sleeping zero milliseconds is yielding the CPU: saying so this
        // way avoids a round trip through the sleeper list.
        crate::scheduler::yield_now();
        return Ok(0);
    }
    if duration > SLEEP_MAX_MS {
        return Err(SysError::with(ErrorCode::InvalidArgument, "duree au-dela d'une heure"));
    }

    // The granularity is the tick: ten milliseconds. A shorter sleep is
    // rounded to one tick rather than to zero, failing which a loop
    // sleeping one millisecond would run flat out.
    let per_tick = 1000 / crate::TIMER_HZ as u64;
    let wake_at = crate::idt::ticks() + duration.div_ceil(per_tick).max(1);

    // Same order as everywhere else: the sleep is armed before the
    // deadline is published, then we check it has not already passed.
    // Without that, a tick occurring between the publication and the
    // sleep would remove the entry and wake a task that is still RUNNING
    // -- which would then go to sleep with nobody left to wake it.
    crate::scheduler::arm_sleep();
    SLEEPERS.lock().push((call.pid, wake_at));

    if crate::idt::ticks() >= wake_at {
        SLEEPERS.lock().retain(|(pid, _)| *pid != call.pid);
        crate::scheduler::cancel_sleep();
        return Ok(crate::idt::ticks());
    }

    super::count_block();
    crate::scheduler::yield_now();

    // Defensive removal: if the task was woken by something other than a
    // deadline -- an explicit `wake` -- its entry would linger.
    SLEEPERS.lock().retain(|(pid, _)| *pid != call.pid);

    Ok(crate::idt::ticks())
}

/// Wakes the sleepers whose deadline has been reached.
///
/// Called from the timer handler of core 0. The PIDs to wake are
/// extracted **under the lock**, then woken once the lock is released:
/// `scheduler::wake` takes the scheduler's and may raise an IPI, two
/// operations one does not want to nest under a lock taken in an
/// interrupt handler.
pub fn tick(now: u64) {
    let due: Vec<Pid> = {
        let mut sleepers = SLEEPERS.lock();
        if sleepers.is_empty() {
            return;
        }
        let mut due = Vec::new();
        sleepers.retain(|(pid, wake_at)| {
            if *wake_at <= now {
                due.push(*pid);
                false
            } else {
                true
            }
        });
        due
    };

    for pid in due {
        crate::scheduler::wake(pid);
    }
}

/// Number of tasks currently asleep -- diagnostics.
pub fn sleeping_count() -> usize {
    SLEEPERS.lock().len()
}

/// Removes a dead task from the sleeper list.
pub fn forget(pid: Pid) {
    SLEEPERS.lock().retain(|(owner, _)| *owner != pid);
}

pub fn dump_state() {
    crate::serial_println!(
        "[time] {} tics ecoules, resolution {} Hz - {} tache(s) endormie(s)",
        crate::idt::ticks(),
        crate::TIMER_HZ,
        sleeping_count()
    );
    crate::rtc::dump_state();
}

/// Error returned when the wall clock is missing -- exposed so that the
/// self-test can recognise it without rebuilding it.
pub fn no_clock() -> SysError {
    error::unsupported("aucune horloge CMOS exploitable")
}

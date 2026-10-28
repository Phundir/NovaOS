//! Clocks of the POSIX programs, whichever interface they call through.
//!
//! These programs ask for time in nanoseconds and expect `CLOCK_MONOTONIC`
//! to advance between two calls a microsecond apart. The Nova clock is the
//! 100 Hz tick: a program that measures a duration with it sees zero, then
//! ten milliseconds -- Firefox's frame scheduler and JavaScript's
//! `performance.now()` both notice.
//!
//! So the subsystem reads the TSC, calibrated once against the APIC timer
//! (itself calibrated at boot). QEMU and every processor of the last
//! fifteen years keep the TSC constant-rate and synchronised across
//! cores, which is what makes one calibration valid for all of them.
//! `CLOCK_REALTIME` is the RTC reading of boot plus the monotonic clock,
//! its origin moved when the network time corrects it (`timesync`).

use core::sync::atomic::{AtomicU64, Ordering};

use super::errno::*;

static TSC_ORIGIN: AtomicU64 = AtomicU64::new(0);
static TSC_PER_MS: AtomicU64 = AtomicU64::new(0);
/// Wall clock at `TSC_ORIGIN`, in nanoseconds since the epoch.
static REALTIME_ORIGIN_NS: AtomicU64 = AtomicU64::new(0);

pub const NS_PER_SEC: u64 = 1_000_000_000;

/// Calibrates the TSC over 20 ms of APIC timer.
pub fn init() {
    let start = crate::random::rdtsc();
    crate::apic::busy_wait_micros(20_000);
    let end = crate::random::rdtsc();
    let per_ms = (end.wrapping_sub(start) / 20).max(1);
    TSC_PER_MS.store(per_ms, Ordering::Relaxed);
    TSC_ORIGIN.store(end, Ordering::Relaxed);
    let wall_ms = crate::rtc::now_millis().unwrap_or(0);
    REALTIME_ORIGIN_NS.store(wall_ms * 1_000_000, Ordering::Relaxed);
}

pub fn tsc_per_ms() -> u64 {
    TSC_PER_MS.load(Ordering::Relaxed)
}

/// Nanoseconds since the calibration.
pub fn monotonic_ns() -> u64 {
    let per_ms = TSC_PER_MS.load(Ordering::Relaxed);
    if per_ms == 0 {
        return crate::idt::ticks() * (NS_PER_SEC / crate::TIMER_HZ as u64);
    }
    let delta = crate::random::rdtsc().wrapping_sub(TSC_ORIGIN.load(Ordering::Relaxed));
    ((delta as u128 * 1_000_000) / per_ms as u128) as u64
}

pub fn realtime_ns() -> u64 {
    REALTIME_ORIGIN_NS.load(Ordering::Relaxed) + monotonic_ns()
}

/// Sets `CLOCK_REALTIME` to `epoch_ns` now (network time). The monotonic
/// clock does not move: only the origin the wall clock is counted from.
pub fn set_realtime_ns(epoch_ns: u64) {
    REALTIME_ORIGIN_NS.store(epoch_ns.saturating_sub(monotonic_ns()), Ordering::Relaxed);
}

/// Tick at which `ns` nanoseconds from now will have passed, rounded up:
/// a timeout never expires early. One tick more than the division gives,
/// because the current tick has already partly gone by: 5 ticks from
/// now may be only 41 ms away -- which failed the 50 ms `poll` of the
/// events test on a single core.
///
/// The unit is core 0's millisecond (`idt::fine_ticks`), not the 10 ms
/// system tick: the one extra tick then costs a millisecond, not ten.
pub fn deadline_after_ns(ns: u64) -> u64 {
    if ns == 0 {
        return crate::idt::fine_ticks();
    }
    let per_tick = NS_PER_SEC / crate::FINE_HZ as u64;
    crate::idt::fine_ticks().saturating_add(ns.div_ceil(per_tick)).saturating_add(1)
}

/// Deadline tick for an absolute instant of the monotonic clock.
pub fn deadline_at_monotonic(ns: u64) -> u64 {
    deadline_after_ns(ns.saturating_sub(monotonic_ns()))
}

/// Deadline tick for an absolute instant of the realtime clock.
pub fn deadline_at_realtime(ns: u64) -> u64 {
    deadline_after_ns(ns.saturating_sub(realtime_ns()))
}

/// `struct timespec` as the x86-64 ABI lays it out.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Timespec {
    pub sec: i64,
    pub nsec: i64,
}

impl Timespec {
    pub fn from_ns(ns: u64) -> Self {
        Timespec { sec: (ns / NS_PER_SEC) as i64, nsec: (ns % NS_PER_SEC) as i64 }
    }

    /// None for a negative or malformed value (EINVAL).
    pub fn to_ns(self) -> Option<u64> {
        if self.sec < 0 || !(0..1_000_000_000).contains(&self.nsec) {
            return None;
        }
        (self.sec as u64).checked_mul(NS_PER_SEC)?.checked_add(self.nsec as u64)
    }
}

/// Clock identifiers.
pub const CLOCK_REALTIME: u64 = 0;
pub const CLOCK_MONOTONIC: u64 = 1;
pub const CLOCK_PROCESS_CPUTIME_ID: u64 = 2;
pub const CLOCK_THREAD_CPUTIME_ID: u64 = 3;
pub const CLOCK_MONOTONIC_RAW: u64 = 4;
pub const CLOCK_REALTIME_COARSE: u64 = 5;
pub const CLOCK_MONOTONIC_COARSE: u64 = 6;
pub const CLOCK_BOOTTIME: u64 = 7;

/// Reads a clock; None for an unknown one.
pub fn read_clock(id: u64) -> Option<u64> {
    match id {
        CLOCK_REALTIME | CLOCK_REALTIME_COARSE => Some(realtime_ns()),
        CLOCK_MONOTONIC | CLOCK_MONOTONIC_RAW | CLOCK_MONOTONIC_COARSE | CLOCK_BOOTTIME => Some(monotonic_ns()),
        // CPU time: the tick count of the caller, the only CPU accounting
        // the scheduler keeps. Coarse, but monotonic and never ahead of
        // the wall clock, which is what callers rely on.
        CLOCK_PROCESS_CPUTIME_ID | CLOCK_THREAD_CPUTIME_ID => {
            let ticks = crate::scheduler::current_info().map(|t| t.ticks_used).unwrap_or(0);
            Some(ticks * (NS_PER_SEC / crate::TIMER_HZ as u64))
        }
        _ => None,
    }
}

// ============================================================
//  Calls on the clocks, for both interfaces
// ============================================================

pub fn clock_gettime(clock: u64, out: u64) -> LxResult<u64> {
    let ns = read_clock(clock).ok_or(EINVAL)?;
    crate::vm::write_user(out, &Timespec::from_ns(ns)).map_err(|_| EFAULT)?;
    Ok(0)
}

pub fn gettimeofday(out: u64) -> LxResult<u64> {
    if out != 0 {
        let ns = realtime_ns();
        let tv: [i64; 2] = [(ns / 1_000_000_000) as i64, ((ns % 1_000_000_000) / 1000) as i64];
        crate::vm::write_user(out, &tv).map_err(|_| EFAULT)?;
    }
    Ok(0)
}

pub fn nanosleep(req: u64, _rem: u64) -> LxResult<u64> {
    let ts: Timespec = crate::vm::read_user(req).map_err(|_| EFAULT)?;
    let ns = ts.to_ns().ok_or(EINVAL)?;
    if ns == 0 {
        crate::scheduler::yield_now();
        return Ok(0);
    }
    match super::wait::sleep_until(deadline_after_ns(ns)) {
        Ok(()) => Ok(0),
        Err(_) => Err(EINTR),
    }
}

pub fn clock_nanosleep(clock: u64, flags: u64, req: u64, rem: u64) -> LxResult<u64> {
    const TIMER_ABSTIME: u64 = 1;
    if flags & TIMER_ABSTIME == 0 {
        return nanosleep(req, rem);
    }
    let ts: Timespec = crate::vm::read_user(req).map_err(|_| EFAULT)?;
    let ns = ts.to_ns().ok_or(EINVAL)?;
    let deadline = if clock == CLOCK_REALTIME {
        deadline_at_realtime(ns)
    } else {
        deadline_at_monotonic(ns)
    };
    match super::wait::sleep_until(deadline) {
        Ok(()) => Ok(0),
        Err(_) => Err(EINTR),
    }
}

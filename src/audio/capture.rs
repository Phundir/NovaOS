//! Audio input: what the driver captures, on its way to one reader.
//!
//! ## One reader, several watchers
//!
//! A captured sample can be **consumed** only once: two readers draining
//! the same queue would each get half of the words spoken. So one task at
//! a time may `open` the input -- the voice recorder -- and a second one
//! is refused by name.
//!
//! The settings panel only needs the level, to show that the microphone
//! hears something. That does not consume anything, so any number of
//! tasks may `watch`: the driver captures as long as there is a reader or
//! a watcher, and stops the DMA when there is neither -- the same rule the
//! output follows, a device transferring silence for nobody costs bus and
//! interrupts for nothing.
//!
//! ## Where the gain is applied
//!
//! In software, here, before the level is measured and before the reader
//! sees anything. The AC'97 has a record gain register too, and the
//! driver leaves it at 0 dB: the emulated codec of QEMU does not apply
//! it on every backend, and a setting that works on one host and not on
//! another is a setting nobody can trust. Integer arithmetic, a fraction
//! over 256, saturated -- the same rules as the mixer.

use alloc::collections::VecDeque;

use crate::vga::SpinLock;

use super::VOLUME_UNITY;

/// Samples kept for the reader: two seconds of 48 kHz stereo. A reader
/// late by more than that loses the oldest, and the counter says so.
const QUEUE_CAPACITY: usize = super::SAMPLE_RATE as usize * super::CHANNELS * 2;

/// Highest gain accepted: four times, +12 dB.
pub const MAX_GAIN: u32 = 4 * VOLUME_UNITY;

struct Capture {
    reader: bool,
    /// Task holding the reader, for `forget_owner`.
    reader_owner: crate::process::Pid,
    /// One entry per `watch`, by task.
    watchers: alloc::vec::Vec<crate::process::Pid>,
    queue: VecDeque<i16>,
    gain: u32,
    muted: bool,
    /// Peak of the last block pushed, 0..=32767, after the gain.
    level: u16,
    pushed: u64,
    dropped: u64,
    clipped: u64,
}

static CAPTURE: SpinLock<Capture> = SpinLock::new(Capture {
    reader: false,
    reader_owner: 0,
    watchers: alloc::vec::Vec::new(),
    queue: VecDeque::new(),
    gain: VOLUME_UNITY,
    muted: false,
    level: 0,
    pushed: 0,
    dropped: 0,
    clipped: 0,
});

/// Proof of being the reader. Not `Copy`: closing it twice is impossible.
pub struct Reader(());

/// Becomes the reader of the input.
pub fn open() -> Result<Reader, &'static str> {
    let mut capture = CAPTURE.lock();
    if capture.reader {
        return Err("entree audio deja utilisee par une autre application");
    }
    capture.reader = true;
    capture.reader_owner = crate::scheduler::current_pid();
    capture.queue.clear();
    capture.level = 0;
    Ok(Reader(()))
}

/// Gives the input back.
pub fn close(reader: Reader) {
    let _ = reader;
    let mut capture = CAPTURE.lock();
    capture.reader = false;
    capture.queue.clear();
}

/// Takes up to `out.len()` captured samples, interleaved stereo 48 kHz.
pub fn read(_reader: &Reader, out: &mut [i16]) -> usize {
    let mut capture = CAPTURE.lock();
    let count = out.len().min(capture.queue.len());
    for (slot, sample) in out.iter_mut().zip(capture.queue.drain(..count)) {
        *slot = sample;
    }
    count
}

/// Starts watching the level. Pair with `unwatch`.
pub fn watch() {
    CAPTURE.lock().watchers.push(crate::scheduler::current_pid());
}

pub fn unwatch() {
    let me = crate::scheduler::current_pid();
    let mut capture = CAPTURE.lock();
    if let Some(index) = capture.watchers.iter().position(|pid| *pid == me) {
        capture.watchers.swap_remove(index);
    }
    if capture.watchers.is_empty() && !capture.reader {
        capture.level = 0;
    }
}

/// Releases whatever a task that is gone held: see `audio::forget_owner`.
pub fn forget_owner(owner: crate::process::Pid) {
    let mut capture = CAPTURE.lock();
    capture.watchers.retain(|pid| *pid != owner);
    if capture.reader && capture.reader_owner == owner {
        capture.reader = false;
        capture.queue.clear();
    }
}

/// Does anybody want the input? The driver runs the capture DMA only then.
pub fn wanted() -> bool {
    let capture = CAPTURE.lock();
    capture.reader || !capture.watchers.is_empty()
}

/// Applies gain and mute to a block, in place, and returns its peak and
/// the number of samples clipped.
///
/// Extracted from `push` so that the self-test can check the arithmetic
/// without a driver.
pub fn apply_gain(samples: &mut [i16], gain: u32, muted: bool) -> (u16, u64) {
    let mut peak = 0u16;
    let mut clipped = 0u64;
    for sample in samples.iter_mut() {
        let scaled = if muted {
            0
        } else {
            *sample as i32 * gain as i32 / VOLUME_UNITY as i32
        };
        let value = if scaled > i16::MAX as i32 {
            clipped += 1;
            i16::MAX
        } else if scaled < i16::MIN as i32 {
            clipped += 1;
            i16::MIN
        } else {
            scaled as i16
        };
        *sample = value;
        peak = peak.max(value.unsigned_abs().min(32767));
    }
    (peak, clipped)
}

/// Hands captured samples over. Called by the driver.
pub fn push(samples: &[i16]) {
    let mut capture = CAPTURE.lock();
    let mut block = alloc::vec::Vec::from(samples);
    let (peak, clipped) = apply_gain(&mut block, capture.gain, capture.muted);
    capture.level = peak;
    capture.clipped += clipped;
    capture.pushed += block.len() as u64;
    if !capture.reader {
        return;
    }
    let overflow = (capture.queue.len() + block.len()).saturating_sub(QUEUE_CAPACITY);
    if overflow != 0 {
        let oldest = overflow.min(capture.queue.len());
        capture.queue.drain(..oldest);
        capture.dropped += overflow as u64;
    }
    capture.queue.extend(block);
}

/// Peak of the last captured block, 0..=32767.
pub fn level() -> u16 {
    CAPTURE.lock().level
}

pub fn set_gain(gain: u32) {
    CAPTURE.lock().gain = gain.min(MAX_GAIN);
}

pub fn gain() -> u32 {
    CAPTURE.lock().gain
}

pub fn set_muted(muted: bool) {
    CAPTURE.lock().muted = muted;
}

pub fn muted() -> bool {
    CAPTURE.lock().muted
}

/// Counters: (samples captured, samples dropped for a late reader,
/// samples clipped by the gain).
pub fn counters() -> (u64, u64, u64) {
    let capture = CAPTURE.lock();
    (capture.pushed, capture.dropped, capture.clipped)
}

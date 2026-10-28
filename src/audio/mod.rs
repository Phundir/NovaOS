//! Phase 6.3 -- Audio abstraction and mixing.
//!
//! ## Why a mixer in the system
//!
//! An audio device plays one stream at a time: it reads a buffer and
//! converts it. If every application wrote into it directly, the last to
//! write would erase the others -- that is the behaviour of a system with
//! no mixer, where starting a game cuts the music. 6.3.2 wants the
//! opposite; so somebody must add the streams together before handing
//! them to the hardware, and that somebody must be unique.
//!
//! ## Integer arithmetic, and why that is a hard constraint
//!
//! The kernel runs with floating point **disabled**: the target is
//! soft-float and SSE is armed neither in CR0 nor in CR4. A single `f32`
//! in this module would fail the link or, worse, produce an exception at
//! the first note. So everything is in integers:
//!
//! * the samples are signed sixteen-bit integers, the native format of
//!   the AC'97 as of most codecs;
//! * the volume is a fraction over 256 -- 256 means "unchanged", 128
//!   means half;
//! * the accumulation is done on thirty-two bits, then **saturates**
//!   instead of overflowing. A silent overflow turns a positive maximum
//!   into a negative maximum: to the ear, a click on every peak.
//!
//! ## What is around it
//!
//! | Module | Role |
//! |---|---|
//! | `capture` | the input: one reader, level watchers, software gain |
//! | `wav` | reading and writing WAV files |
//! | `resample` | any WAV to 48 kHz stereo, 48 kHz stereo to 16 kHz mono |
//! | `settings` | the per-account `audio.cfg`, applied at session open |
//! | `demo` | the sample file installed for the player |
//!
//! The mixer itself still does no resampling: a stream must be produced
//! at the frequency of the device, and `resample` is what a producer
//! uses to get there. Keeping the conversion out of the mixer keeps the
//! mixer a sum -- one loop, one saturation.

pub mod capture;
pub mod resample;
pub mod selftest;
pub mod settings;
pub mod synth;
pub mod wav;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::vga::SpinLock;

/// System sampling frequency, in hertz.
///
/// 48 kHz is the native frequency of the AC'97 and of HDA: choosing it
/// spares all resampling on the output path.
pub const SAMPLE_RATE: u32 = 48_000;

/// Number of channels. Interleaved stereo: left, right, left...
pub const CHANNELS: usize = 2;

/// Number of simultaneous streams.
const MAX_STREAMS: usize = 8;

/// Samples kept per stream before a write is refused.
///
/// 48 000 stereo samples, that is half a second: enough to absorb a task
/// delayed by the scheduler, few enough that the latency stays inaudible.
const STREAM_CAPACITY: usize = SAMPLE_RATE as usize / 2 * CHANNELS;

/// Neutral volume, in 256ths.
pub const VOLUME_UNITY: u32 = 256;

struct Stream {
    used: bool,
    name: String,
    /// Task that opened it, so that a task killed without closing its
    /// stream does not keep one of the eight slots forever.
    owner: crate::process::Pid,
    /// Volume applied to this stream, in 256ths.
    volume: u32,
    samples: alloc::collections::VecDeque<i16>,
    /// Samples refused for want of room.
    overruns: u64,
    /// Samples missing at mixing time.
    underruns: u64,
    /// Buffers mixed short of this stream's samples: each one is a hole of
    /// silence in the middle of the sound, heard as a click.
    underrun_events: u64,
}

impl Stream {
    const fn new() -> Self {
        Stream {
            used: false,
            name: String::new(),
            owner: 0,
            volume: VOLUME_UNITY,
            samples: alloc::collections::VecDeque::new(),
            overruns: 0,
            underruns: 0,
            underrun_events: 0,
        }
    }
}

struct Mixer {
    streams: [Stream; MAX_STREAMS],
    /// Master volume, in 256ths.
    master: u32,
    muted: bool,
    /// Is the mixer output reserved by a consumer other than the driver?
    reserved: bool,
    /// Samples handed to the device.
    rendered: u64,
    /// Peaks clipped by the saturation -- a counter that climbs signals a
    /// mix that is too loud, not a failure.
    clipped: u64,
    /// Peak of the last buffer mixed, 0..=32767, after the master volume:
    /// what the loudspeaker is given, for a level meter.
    level: u16,
    /// Accumulator of the mixing, kept from one call to the next.
    ///
    /// A mix needs more range than a sample: two streams at full scale
    /// overflow an `i16`, so the sum is built in `i32` and saturated
    /// afterwards. Allocating that buffer inside `mix_into` cost an
    /// allocation and a free per audio buffer -- a hundred times a second
    /// while anything plays. It is the same allocation every time, so it
    /// is kept.
    accumulator: alloc::vec::Vec<i32>,
}

static MIXER: SpinLock<Mixer> = SpinLock::new(Mixer {
    streams: [const { Stream::new() }; MAX_STREAMS],
    master: VOLUME_UNITY,
    muted: false,
    reserved: false,
    rendered: 0,
    clipped: 0,
    level: 0,
    accumulator: alloc::vec::Vec::new(),
});

/// Reference to an open stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handle(usize);

impl Handle {
    pub fn index(&self) -> usize {
        self.0
    }
}

/// Opens an output stream.
pub fn open(name: &str) -> Result<Handle, &'static str> {
    let mut mixer = MIXER.lock();
    for (index, stream) in mixer.streams.iter_mut().enumerate() {
        if !stream.used {
            stream.used = true;
            stream.name = name.to_string();
            stream.owner = crate::scheduler::current_pid();
            stream.volume = VOLUME_UNITY;
            stream.samples.clear();
            stream.overruns = 0;
            stream.underruns = 0;
            stream.underrun_events = 0;
            return Ok(Handle(index));
        }
    }
    Err("plus de flux audio disponible")
}

/// Closes a stream and throws away what it has not played.
pub fn close(handle: Handle) {
    let mut mixer = MIXER.lock();
    let stream = &mut mixer.streams[handle.0];
    stream.used = false;
    stream.samples.clear();
}

/// Throws away what a stream has not played yet: a seek or a pause.
pub fn flush(handle: Handle) {
    MIXER.lock().streams[handle.0].samples.clear();
}

/// Closes every stream and gives the input back for a task that is gone.
///
/// Called where a dead application's surfaces are collected: an
/// application killed at log out never runs its own `close`, and its
/// stream would otherwise stay open -- one of eight -- and its capture
/// reader would lock the microphone for everybody after it.
pub fn forget_owner(owner: crate::process::Pid) {
    {
        let mut mixer = MIXER.lock();
        for stream in mixer.streams.iter_mut() {
            if stream.used && stream.owner == owner {
                stream.used = false;
                stream.samples.clear();
            }
        }
    }
    capture::forget_owner(owner);
}

/// Drops samples into a stream.
///
/// Returns the number of samples accepted: an application producing
/// faster than the device consumes must slow down, not see its data
/// vanish in silence.
pub fn write(handle: Handle, samples: &[i16]) -> Result<usize, &'static str> {
    let mut mixer = MIXER.lock();
    let stream = &mut mixer.streams[handle.0];
    if !stream.used {
        return Err("flux ferme");
    }

    let room = STREAM_CAPACITY.saturating_sub(stream.samples.len());
    let accepted = room.min(samples.len());
    stream.samples.extend(samples[..accepted].iter().copied());
    if accepted < samples.len() {
        stream.overruns += (samples.len() - accepted) as u64;
    }
    Ok(accepted)
}

/// Sets the volume of a stream, in 256ths.
pub fn set_volume(handle: Handle, volume: u32) {
    let mut mixer = MIXER.lock();
    mixer.streams[handle.0].volume = volume.min(4 * VOLUME_UNITY);
}

/// Sets the master volume, in 256ths.
pub fn set_master(volume: u32) {
    MIXER.lock().master = volume.min(VOLUME_UNITY);
}

/// Mutes or unmutes the output.
pub fn set_muted(muted: bool) {
    MIXER.lock().muted = muted;
}

/// Master volume and mute state.
pub fn master() -> (u32, bool) {
    let mixer = MIXER.lock();
    (mixer.master, mixer.muted)
}

/// 6.3.2 -- Mixes every stream into an output buffer.
///
/// The buffer is filled **entirely**: a stream short of samples
/// contributes silence, it does not shorten the output. A device
/// receiving a partial buffer would replay the end of the previous one.
///
/// Returns the number of streams that really contributed.
pub fn mix_into(output: &mut [i16]) -> usize {
    let mut mixer = MIXER.lock();
    let master = if mixer.muted { 0 } else { mixer.master };

    let mut contributors = 0;
    // Resized rather than reallocated, and cleared rather than dropped:
    // the buffer is the same size on every call in practice.
    mixer.accumulator.clear();
    mixer.accumulator.resize(output.len(), 0);
    let mut accumulator = core::mem::take(&mut mixer.accumulator);

    for stream in mixer.streams.iter_mut() {
        if !stream.used || stream.samples.is_empty() {
            continue;
        }
        contributors += 1;

        let available = stream.samples.len().min(output.len());
        for (slot, sample) in accumulator
            .iter_mut()
            .zip(stream.samples.drain(..available))
        {
            // The volume applies per stream **before** the accumulation:
            // applying it after would amount to one volume for all.
            *slot += (sample as i32 * stream.volume as i32) / VOLUME_UNITY as i32;
        }
        if available < output.len() {
            stream.underruns += (output.len() - available) as u64;
            stream.underrun_events += 1;
        }
    }

    let mut clipped = 0u64;
    let mut peak = 0u16;
    for (slot, value) in output.iter_mut().zip(accumulator.iter()) {
        let scaled = (*value * master as i32) / VOLUME_UNITY as i32;
        if scaled > i16::MAX as i32 {
            *slot = i16::MAX;
            clipped += 1;
        } else if scaled < i16::MIN as i32 {
            *slot = i16::MIN;
            clipped += 1;
        } else {
            *slot = scaled as i16;
        }
        peak = peak.max(slot.unsigned_abs().min(32767));
    }

    mixer.level = peak;
    mixer.clipped += clipped;
    mixer.rendered += output.len() as u64;
    // Given back for the next call. Forgetting this line would turn the
    // saving above into an allocation of a growing vector every time.
    mixer.accumulator = accumulator;
    contributors
}

/// Does every stream that has anything to play hold at least `samples`?
///
/// What the driver asks before mixing a buffer it could still postpone:
/// a stream with less would be padded with silence in the middle of its
/// sound. A stream with nothing at all is not playing, and does not count.
pub fn streams_ready(samples: usize) -> bool {
    let mixer = MIXER.lock();
    mixer
        .streams
        .iter()
        .all(|s| !s.used || s.samples.is_empty() || s.samples.len() >= samples)
}

/// Is there anything to play?
///
/// False as long as the output is reserved: mixing has **one consumer at
/// a time**, and two callers mixing in turn would steal samples from each
/// other -- the first would empty the queues the second waits for. The
/// driver is the normal consumer; a diagnostic may take its place for the
/// duration of its measurements.
pub fn has_audio() -> bool {
    let mixer = MIXER.lock();
    !mixer.reserved && mixer.streams.iter().any(|s| s.used && !s.samples.is_empty())
}

/// Reserves the mixer output. The driver then stops drawing from it.
///
/// Returns `false` if it already is: two simultaneous reservations would
/// pose exactly the problem the reservation exists to avoid.
pub fn reserve() -> bool {
    let mut mixer = MIXER.lock();
    if mixer.reserved {
        return false;
    }
    mixer.reserved = true;
    true
}

/// Gives the output back to the driver.
pub fn release() {
    MIXER.lock().reserved = false;
}

/// State of the streams: (name, samples pending, volume).
pub fn streams() -> Vec<(String, usize, u32)> {
    let mixer = MIXER.lock();
    mixer
        .streams
        .iter()
        .filter(|s| s.used)
        .map(|s| (s.name.clone(), s.samples.len(), s.volume))
        .collect()
}

/// Peak of the last buffer handed to the device, 0..=32767.
pub fn output_level() -> u16 {
    MIXER.lock().level
}

/// Samples waiting in one stream: what a producer uses to know how far
/// ahead of the loudspeaker it is.
pub fn pending(handle: Handle) -> usize {
    MIXER.lock().streams[handle.0].samples.len()
}

/// Holes of one stream: (samples missing at mixing time, buffers mixed
/// short, samples refused for want of room).
pub fn stream_counters(handle: Handle) -> (u64, u64, u64) {
    let mixer = MIXER.lock();
    let stream = &mixer.streams[handle.0];
    (stream.underruns, stream.underrun_events, stream.overruns)
}

/// Counters: (samples handed over, peaks clipped).
pub fn counters() -> (u64, u64) {
    let mixer = MIXER.lock();
    (mixer.rendered, mixer.clipped)
}

/// Produces a square wave, useful to exercise the output path.
///
/// A square wave rather than a sine: it is built with no floating-point
/// operation at all, which is an absolute constraint here, and its rich
/// spectrum makes any defect of the conversion path audible.
pub fn square_wave(frequency: u32, milliseconds: u32, amplitude: i16) -> Vec<i16> {
    let frames = (SAMPLE_RATE as u64 * milliseconds as u64 / 1000) as usize;
    // A period must count at least two samples: below that, a square wave
    // has no half period left and reduces to a constant.
    let period = SAMPLE_RATE
        .checked_div(frequency)
        .map(|value| value.max(2) as usize)
        .unwrap_or(1);

    let mut samples = Vec::with_capacity(frames * CHANNELS);
    for frame in 0..frames {
        let value = if (frame % period) < period / 2 {
            amplitude
        } else {
            -amplitude
        };
        for _ in 0..CHANNELS {
            samples.push(value);
        }
    }
    samples
}

pub fn dump_state() {
    let (rendered, clipped) = counters();
    let (master, muted) = master();
    crate::serial_println!(
        "[audio] {} Hz, {} canaux - volume general {}/256{} - {} echantillons rendus, {} ecretes",
        SAMPLE_RATE,
        CHANNELS,
        master,
        if muted { " (coupe)" } else { "" },
        rendered,
        clipped
    );
    for (name, pending, volume) in streams() {
        crate::serial_println!(
            "[audio]   flux \"{}\" - {} echantillons en attente, volume {}/256",
            name,
            pending,
            volume
        );
    }
}

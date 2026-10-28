//! Rate and channel conversion, in fixed point.
//!
//! ## Why here and not in the mixer
//!
//! The mixer's header says it takes 48 kHz stereo and nothing else, and
//! that stays true: the conversion is done by whoever produces the stream
//! -- the player -- before `audio::write`. The mixer adds streams; it does
//! not have to know where one came from.
//!
//! ## Linear interpolation, and what it costs
//!
//! Each output frame is the weighted mean of the two source frames around
//! it, the weight kept on 16 bits. The position advances by
//! `source_rate / 48000` per output frame, held in 32.32 fixed point so
//! that a 44.1 kHz file does not drift by a frame per second.
//!
//! It is not a proper resampler: it has no low-pass filter, so a 96 kHz
//! file folds what lies above 24 kHz back into the audible band, and a
//! 8 kHz file sounds duller than it should. For speech and ordinary music
//! at 22.05, 44.1 or 48 kHz the difference is not audible on a laptop
//! speaker. A windowed-sinc filter is the next step, and it can only be
//! checked by ear -- which is why it is not done blind.

use alloc::vec::Vec;

use super::wav::Info;

/// Fixed-point one, for positions.
const ONE: u64 = 1 << 32;

/// Streams a WAV file as 48 kHz interleaved stereo.
pub struct Converter {
    info: Info,
    /// Position in source frames, 32.32.
    position: u64,
    /// Source frames per output frame, 32.32.
    step: u64,
}

impl Converter {
    pub fn new(info: Info) -> Self {
        Converter {
            info,
            position: 0,
            step: ((info.rate as u64) << 32) / super::SAMPLE_RATE as u64,
        }
    }

    pub fn info(&self) -> &Info {
        &self.info
    }

    /// Current position, in source frames.
    pub fn frame(&self) -> usize {
        (self.position >> 32) as usize
    }

    pub fn finished(&self) -> bool {
        self.frame() >= self.info.frames()
    }

    /// Moves to a source frame.
    pub fn seek(&mut self, frame: usize) {
        self.position = (frame.min(self.info.frames()) as u64) << 32;
    }

    /// Sample of the left or right output channel at a source frame.
    ///
    /// Mono feeds both sides; a file of more than two channels plays its
    /// first two -- down-mixing a 5.1 file properly needs coefficients per
    /// layout, and nobody has asked for it.
    fn side(&self, bytes: &[u8], frame: usize, right: bool) -> i32 {
        let channel = if right && self.info.channels > 1 { 1 } else { 0 };
        self.info.sample(bytes, frame, channel) as i32
    }

    /// Produces up to `frames` output frames. Fewer at the end of the file.
    pub fn next(&mut self, bytes: &[u8], frames: usize) -> Vec<i16> {
        let total = self.info.frames();
        let mut out = Vec::with_capacity(frames * 2);
        for _ in 0..frames {
            let index = (self.position >> 32) as usize;
            if index >= total {
                break;
            }
            let weight = ((self.position & (ONE - 1)) >> 16) as i64;
            let following = (index + 1).min(total - 1);
            for right in [false, true] {
                let a = self.side(bytes, index, right) as i64;
                let b = self.side(bytes, following, right) as i64;
                // In 64 bits: a difference of 65535 times a weight of
                // 65535 does not fit in an `i32`, and the wrap would be a
                // click at every large swing.
                out.push((a + (((b - a) * weight) >> 16)) as i16);
            }
            self.position += self.step;
        }
        out
    }
}

/// 48 kHz stereo to 16 kHz mono, for the voice recorder.
///
/// Both channels are averaged, then every three frames become one --
/// averaged too, which is a crude low-pass filter: a three-tap box that
/// removes the worst of what would fold back from above 8 kHz. Enough for
/// a voice; a recorder of music would keep 48 kHz, which a file of 2 MiB
/// holds for ten seconds.
pub struct Decimator {
    sum: i32,
    count: u32,
}

/// Rate the recorder writes.
pub const VOICE_RATE: u32 = 16_000;

impl Decimator {
    pub const fn new() -> Self {
        Decimator { sum: 0, count: 0 }
    }

    /// Feeds interleaved stereo; returns the mono samples completed.
    pub fn push(&mut self, stereo: &[i16], out: &mut Vec<i16>) {
        let ratio = super::SAMPLE_RATE / VOICE_RATE;
        for frame in stereo.chunks_exact(2) {
            self.sum += (frame[0] as i32 + frame[1] as i32) / 2;
            self.count += 1;
            if self.count == ratio {
                out.push((self.sum / ratio as i32) as i16);
                self.sum = 0;
                self.count = 0;
            }
        }
    }
}

impl Default for Decimator {
    fn default() -> Self {
        Self::new()
    }
}

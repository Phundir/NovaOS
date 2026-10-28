//! WAV files: reading the formats found in practice, writing one.
//!
//! ## What is read
//!
//! RIFF/WAVE with a `fmt ` chunk and a `data` chunk, in any order, other
//! chunks skipped. The encodings:
//!
//! | Tag | Bits | Read as |
//! |---|---|---|
//! | 1 (PCM) | 8 | unsigned, centred on 128 |
//! | 1 (PCM) | 16, 24, 32 | signed little-endian, top 16 bits kept |
//! | 3 (IEEE float) | 32 | converted **without a float**, see below |
//! | 0xFFFE (extensible) | as above | the sub-format GUID decides PCM or float |
//!
//! Anything else -- ADPCM, mu-law, 64-bit float -- is refused by name, not
//! played as noise.
//!
//! ## A float sample with no float
//!
//! The kernel runs soft-float with SSE off: a single `f32` operation is a
//! link error or a fault. A 32-bit float sample is therefore taken apart
//! by hand -- sign, exponent, mantissa -- and scaled with shifts. That is
//! exact for the range that matters (|x| <= 1), and saturates beyond.
//!
//! ## Decoded field by field
//!
//! The header is read with `from_le_bytes` at computed offsets, never by
//! casting a pointer to a struct: nothing guarantees a chunk starts on an
//! aligned address -- a chunk of odd length is followed by a pad byte, and
//! files that forget it exist.

use alloc::vec::Vec;

const FORMAT_PCM: u16 = 1;
const FORMAT_FLOAT: u16 = 3;
const FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// Encoding of the samples, once the format tag is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// Integer PCM of 8, 16, 24 or 32 bits.
    Integer(u16),
    /// IEEE 754 single precision.
    Float32,
}

/// Description of a WAV file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    pub rate: u32,
    pub channels: u16,
    pub encoding: Encoding,
    /// Bytes of one frame: one sample per channel.
    pub frame_bytes: usize,
    /// Where the samples start in the file, and how many bytes they take.
    pub data_offset: usize,
    pub data_len: usize,
}

impl Info {
    pub fn frames(&self) -> usize {
        self.data_len / self.frame_bytes.max(1)
    }

    /// Duration in milliseconds.
    pub fn duration_ms(&self) -> u64 {
        self.frames() as u64 * 1000 / self.rate.max(1) as u64
    }

    pub fn bits(&self) -> u16 {
        match self.encoding {
            Encoding::Integer(bits) => bits,
            Encoding::Float32 => 32,
        }
    }

    /// Sample of `channel` in `frame`, as a signed 16-bit value.
    ///
    /// `bytes` is the whole file. Out of range reads return silence rather
    /// than panicking: a truncated file plays until it stops.
    pub fn sample(&self, bytes: &[u8], frame: usize, channel: usize) -> i16 {
        let width = self.frame_bytes / self.channels.max(1) as usize;
        let at = self.data_offset + frame * self.frame_bytes + channel * width;
        let Some(raw) = bytes.get(at..at + width) else {
            return 0;
        };
        match self.encoding {
            Encoding::Integer(8) => ((raw[0] as i16) - 128) << 8,
            Encoding::Integer(16) => i16::from_le_bytes([raw[0], raw[1]]),
            // The top two bytes are the most significant ones: dropping
            // the low byte(s) is the truncation to 16 bits.
            Encoding::Integer(24) => i16::from_le_bytes([raw[1], raw[2]]),
            Encoding::Integer(32) => i16::from_le_bytes([raw[2], raw[3]]),
            Encoding::Integer(_) => 0,
            Encoding::Float32 => {
                float_to_i16(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
            }
        }
    }
}

fn u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
    let b = bytes.get(offset..offset + 2)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    let b = bytes.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Reads the header of a WAV file.
pub fn parse(bytes: &[u8]) -> Result<Info, &'static str> {
    if bytes.get(0..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return Err("pas un fichier WAV (RIFF/WAVE absent)");
    }

    let mut format: Option<(u16, u16, u32, u16, u16)> = None;
    let mut data: Option<(usize, usize)> = None;
    let mut cursor = 12usize;
    // Bounded by the file: every lap advances by at least 8 bytes.
    while cursor + 8 <= bytes.len() {
        let id = &bytes[cursor..cursor + 4];
        let size = u32_at(bytes, cursor + 4).ok_or("en-tete tronque")? as usize;
        let body = cursor + 8;
        match id {
            b"fmt " => {
                let mut tag = u16_at(bytes, body).ok_or("bloc fmt tronque")?;
                let channels = u16_at(bytes, body + 2).ok_or("bloc fmt tronque")?;
                let rate = u32_at(bytes, body + 4).ok_or("bloc fmt tronque")?;
                let align = u16_at(bytes, body + 12).ok_or("bloc fmt tronque")?;
                let bits = u16_at(bytes, body + 14).ok_or("bloc fmt tronque")?;
                if tag == FORMAT_EXTENSIBLE {
                    // The first two bytes of the sub-format GUID carry the
                    // real tag; the other fourteen are the same fixed
                    // suffix for both PCM and float.
                    tag = u16_at(bytes, body + 24).ok_or("bloc fmt etendu tronque")?;
                }
                format = Some((tag, channels, rate, align, bits));
            }
            b"data" => {
                // A data chunk claiming more than the file holds is cut to
                // what is there: recorders that crash leave exactly that.
                let available = bytes.len().saturating_sub(body);
                data = Some((body, size.min(available)));
            }
            _ => {}
        }
        // Chunks are padded to an even length.
        cursor = body.checked_add(size + (size & 1)).ok_or("taille de bloc aberrante")?;
    }

    let (tag, channels, rate, align, bits) = format.ok_or("bloc fmt absent")?;
    let (data_offset, data_len) = data.ok_or("bloc data absent")?;

    let encoding = match (tag, bits) {
        (FORMAT_PCM, 8 | 16 | 24 | 32) => Encoding::Integer(bits),
        (FORMAT_FLOAT, 32) => Encoding::Float32,
        (FORMAT_PCM, _) => return Err("profondeur PCM non geree"),
        (FORMAT_FLOAT, _) => return Err("flottant autre que 32 bits non gere"),
        _ => return Err("encodage non gere (ni PCM ni flottant)"),
    };
    if channels == 0 || rate == 0 {
        return Err("format vide");
    }
    if !(1000..=384_000).contains(&rate) {
        return Err("frequence d'echantillonnage aberrante");
    }
    let frame_bytes = channels as usize * (bits as usize / 8);
    // The block alignment is what the file says a frame is; a file where
    // the two disagree is read with the computed value, which is the one
    // the samples actually follow in every file seen.
    let _ = align;

    Ok(Info {
        rate,
        channels,
        encoding,
        frame_bytes,
        data_offset,
        data_len,
    })
}

/// Converts an IEEE 754 single to a 16-bit sample, with integers only.
///
/// `value * 32767`, saturated. The mantissa with its implicit bit is a
/// 24-bit integer `m`, and the value is `m * 2^(e - 150)`; multiplying by
/// 32767 and shifting gives the sample.
pub fn float_to_i16(bits: u32) -> i16 {
    let negative = bits >> 31 != 0;
    let exponent = ((bits >> 23) & 0xFF) as i32;
    let mantissa = (bits & 0x7F_FFFF) as i64;

    if exponent == 0xFF {
        // Infinity saturates; NaN is silence rather than a click.
        if mantissa != 0 {
            return 0;
        }
        return if negative { i16::MIN } else { i16::MAX };
    }
    if exponent == 0 {
        // Subnormals are far below one sixteen-bit step.
        return 0;
    }

    let significand = mantissa | (1 << 23);
    let product = significand * 32767;
    let shift = exponent - 150;
    let magnitude = if shift >= 0 {
        if shift > 16 {
            i64::MAX
        } else {
            product << shift
        }
    } else if shift < -62 {
        0
    } else {
        product >> (-shift)
    };
    let magnitude = magnitude.min(32767);
    if negative {
        -(magnitude as i16)
    } else {
        magnitude as i16
    }
}

/// Writes a 16-bit PCM WAV file.
///
/// `samples` is interleaved when `channels` is more than one.
pub fn encode(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
    let data_len = samples.len() * 2;
    let block = channels as u32 * 2;
    let mut bytes = Vec::with_capacity(44 + data_len);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&FORMAT_PCM.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * block).to_le_bytes());
    bytes.extend_from_slice(&(block as u16).to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data_len as u32).to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

/// Samples of 16-bit mono a WAV file of `max_bytes` can hold, header
/// included.
pub fn capacity_frames(max_bytes: u64) -> u64 {
    max_bytes.saturating_sub(44) / 2
}

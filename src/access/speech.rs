//! 10.1.2 -- Speech synthesis (spec 16.2, `screenreader.mod`).
//!
//! ## What "speech synthesis" means here
//!
//! Not a recording, and not a beep standing in for a voice. This is a
//! **formant synthesiser**: a source excites a set of resonators, and the
//! resonators shape the spectrum into something the ear reads as a vowel
//! or a consonant. It is the family Klatt described in 1980, reduced to
//! what a kernel can carry -- three resonators, two source types, one
//! pitch contour.
//!
//! The chain has three stages, and each is a separate concern:
//!
//! ```text
//!   texte  --> graphemes()  --> phonemes  --> render() --> PCM 48 kHz
//! ```
//!
//! ## Why French letter-to-sound rules
//!
//! Everything this system says is written in French: the roles of the
//! accessibility tree, the labels of the login screen, the notifications.
//! A reader applying English rules to `bouton` would say something no
//! French speaker recognises. The rules below are the ones that matter
//! for the vocabulary a system interface actually uses -- digraphs (`ou`,
//! `on`, `ch`), silent final consonants, `e` at the end of a word -- and
//! not a complete French phonology, which is a research project.
//!
//! ## Everything is an integer
//!
//! The kernel is soft-float: an `f32` here would be an `f32` in the
//! kernel. A two-pole resonator is normally written with cosines and an
//! exponential; here the coefficients are computed once, in Q12 fixed
//! point, from a small cosine table. The filter state is `i32`, the
//! output `i16` -- the format `audio::write` takes.
//!
//! That constraint is the reason the resonator count is three and not
//! five: each extra pole is another Q12 multiply per sample per formant,
//! and at 48 kHz that budget is real. Three formants are enough to tell
//! the French vowels apart, which is what a reader needs.
//!
//! ## What this does not do
//!
//! * **No prosody beyond a falling pitch.** No question intonation, no
//!   stress. A sentence is spoken at a steady rate with a slow fall.
//! * **No liaison, no elision.** `les amis` is spoken as two words.
//! * **Nothing outside printable ASCII.** The accented letters never
//!   reach it: the whole repository forbids them in displayed strings, so
//!   the reader receives `fenetre`, not the accented spelling, and reads
//!   the bare vowel. It is a real loss of quality and it is the same
//!   trade the console makes.
//!
//! Saying so is worth more than a synthesiser that appears complete.

use alloc::string::String;
use alloc::vec::Vec;

/// Sample rate: the one the mixer runs at, so nothing is resampled.
pub const SAMPLE_RATE: u32 = crate::audio::SAMPLE_RATE;

/// Longest utterance accepted, in phonemes.
///
/// A reader announces a label, not a document. The bound is what stops a
/// window title built by a loop from occupying the audio device for
/// minutes -- and it bounds the allocation, which is the part that
/// matters in a kernel.
pub const MAX_PHONEMES: usize = 256;

// ============================================================
//  Phonemes
// ============================================================

/// The phoneme inventory.
///
/// Reduced to what French system vocabulary needs. Each carries its three
/// formants in hertz, its duration in milliseconds, and how it is
/// excited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phoneme {
    // --- oral vowels ---
    /// `a` as in "table".
    A,
    /// `e` as in "fenetre" -- the mid front vowel.
    E,
    /// `e` as in "ete" -- the close-mid front vowel.
    Ee,
    /// `i` as in "liste".
    I,
    /// `o` as in "bouton".
    O,
    /// `ou` as in "bouton".
    Ou,
    /// `u` as in "menu".
    U,
    /// `eu` as in "deux".
    Eu,
    // --- nasal vowels ---
    /// `an`, `en`.
    An,
    /// `on`.
    On,
    /// `in`, `ain`.
    In,
    // --- voiced consonants ---
    B,
    D,
    G,
    V,
    Z,
    /// `j`, `ge`.
    Zh,
    M,
    N,
    L,
    R,
    // --- unvoiced consonants ---
    P,
    T,
    K,
    F,
    S,
    /// `ch`.
    Sh,
    // --- glides ---
    /// `y` as in "yeux".
    J,
    /// `w` as in "oui".
    W,
    /// Silence: word boundary, punctuation.
    Silence,
}

/// How a phoneme is excited (16.2 has nothing to say about this; it is
/// what makes a consonant sound like one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// Glottal pulse train: every vowel, every voiced consonant.
    Voiced,
    /// Noise: `s`, `f`, `ch`.
    Noise,
    /// Both at once: `z`, `v`, `j`.
    Mixed,
    /// A short silence then a burst: the plosives.
    Plosive,
    /// Nothing at all.
    Silent,
}

/// Acoustic description of one phoneme.
struct Voice {
    /// The three formants, in hertz.
    formants: [u32; 3],
    /// Bandwidth of each, in hertz. A wide band is a damped resonance,
    /// which is what makes a consonant sound duller than a vowel.
    bandwidths: [u32; 3],
    duration_ms: u32,
    source: Source,
    /// Amplitude, out of 256.
    gain: u32,
}

impl Phoneme {
    /// The acoustic table.
    ///
    /// The formant values are the standard reference ones for French
    /// vowels -- they are what makes `i` sound like `i` rather than like
    /// `e`, and getting one wrong is audible immediately, which is the
    /// one saving grace of a table like this.
    fn voice(&self) -> Voice {
        use Source::*;
        match self {
            // Vowels: narrow bands, full gain, the long durations.
            Phoneme::A => Voice { formants: [ 800, 1200, 2500], bandwidths: [ 80,  90, 120], duration_ms: 110, source: Voiced, gain: 220 },
            Phoneme::E => Voice { formants: [ 550, 1650, 2500], bandwidths: [ 70,  90, 120], duration_ms:  95, source: Voiced, gain: 210 },
            Phoneme::Ee => Voice { formants: [ 400, 2100, 2600], bandwidths: [ 60,  90, 120], duration_ms:  95, source: Voiced, gain: 210 },
            Phoneme::I => Voice { formants: [ 280, 2300, 3000], bandwidths: [ 55,  90, 130], duration_ms:  95, source: Voiced, gain: 200 },
            Phoneme::O => Voice { formants: [ 450,  800, 2600], bandwidths: [ 70,  90, 120], duration_ms: 100, source: Voiced, gain: 215 },
            Phoneme::Ou => Voice { formants: [ 300,  700, 2400], bandwidths: [ 60,  90, 120], duration_ms: 100, source: Voiced, gain: 205 },
            Phoneme::U => Voice { formants: [ 300, 1800, 2400], bandwidths: [ 60,  90, 120], duration_ms:  95, source: Voiced, gain: 200 },
            Phoneme::Eu => Voice { formants: [ 450, 1500, 2400], bandwidths: [ 70,  90, 120], duration_ms:  95, source: Voiced, gain: 205 },
            // Nasal vowels: a wider first formant is what nasalisation
            // sounds like once the nasal branch is left out.
            Phoneme::An => Voice { formants: [ 700, 1100, 2500], bandwidths: [160, 130, 140], duration_ms: 115, source: Voiced, gain: 200 },
            Phoneme::On => Voice { formants: [ 450,  900, 2400], bandwidths: [160, 130, 140], duration_ms: 115, source: Voiced, gain: 200 },
            Phoneme::In => Voice { formants: [ 500, 1800, 2500], bandwidths: [160, 130, 140], duration_ms: 115, source: Voiced, gain: 200 },
            // Voiced consonants.
            Phoneme::B => Voice { formants: [ 250,  800, 2200], bandwidths: [110, 120, 150], duration_ms:  55, source: Plosive, gain: 150 },
            Phoneme::D => Voice { formants: [ 250, 1700, 2600], bandwidths: [110, 120, 150], duration_ms:  55, source: Plosive, gain: 150 },
            Phoneme::G => Voice { formants: [ 250, 1600, 2200], bandwidths: [110, 120, 150], duration_ms:  55, source: Plosive, gain: 150 },
            Phoneme::V => Voice { formants: [ 300, 1100, 2200], bandwidths: [120, 150, 200], duration_ms:  65, source: Mixed, gain: 140 },
            Phoneme::Z => Voice { formants: [ 300, 1400, 4000], bandwidths: [120, 200, 300], duration_ms:  70, source: Mixed, gain: 140 },
            Phoneme::Zh => Voice { formants: [ 300, 1800, 2600], bandwidths: [120, 200, 300], duration_ms:  70, source: Mixed, gain: 140 },
            Phoneme::M => Voice { formants: [ 250,  900, 2200], bandwidths: [100, 150, 200], duration_ms:  70, source: Voiced, gain: 160 },
            Phoneme::N => Voice { formants: [ 250, 1600, 2600], bandwidths: [100, 150, 200], duration_ms:  70, source: Voiced, gain: 160 },
            Phoneme::L => Voice { formants: [ 350, 1200, 2700], bandwidths: [ 90, 120, 160], duration_ms:  65, source: Voiced, gain: 170 },
            Phoneme::R => Voice { formants: [ 400, 1300, 2000], bandwidths: [130, 180, 220], duration_ms:  70, source: Mixed, gain: 150 },
            // Unvoiced consonants: noise, no glottal source at all.
            Phoneme::P => Voice { formants: [ 300,  900, 2200], bandwidths: [200, 250, 300], duration_ms:  50, source: Plosive, gain: 110 },
            Phoneme::T => Voice { formants: [ 300, 1800, 3000], bandwidths: [200, 250, 300], duration_ms:  50, source: Plosive, gain: 110 },
            Phoneme::K => Voice { formants: [ 300, 1700, 2300], bandwidths: [200, 250, 300], duration_ms:  50, source: Plosive, gain: 110 },
            Phoneme::F => Voice { formants: [ 400, 1200, 4500], bandwidths: [250, 300, 400], duration_ms:  75, source: Noise, gain: 100 },
            Phoneme::S => Voice { formants: [ 400, 1400, 5500], bandwidths: [250, 300, 400], duration_ms:  80, source: Noise, gain: 110 },
            Phoneme::Sh => Voice { formants: [ 400, 1800, 2800], bandwidths: [250, 350, 400], duration_ms:  80, source: Noise, gain: 110 },
            // Glides: short vowels, really.
            Phoneme::J => Voice { formants: [ 280, 2200, 3000], bandwidths: [ 80, 110, 140], duration_ms:  45, source: Voiced, gain: 180 },
            Phoneme::W => Voice { formants: [ 300,  700, 2300], bandwidths: [ 80, 110, 140], duration_ms:  45, source: Voiced, gain: 180 },
            Phoneme::Silence => Voice { formants: [0, 0, 0], bandwidths: [0, 0, 0], duration_ms: 90, source: Silent, gain: 0 },
        }
    }

    /// Duration in milliseconds, before the rate is applied.
    pub fn duration_ms(&self) -> u32 {
        self.voice().duration_ms
    }

    /// Is this phoneme a vowel? Used by the letter-to-sound rules.
    pub fn is_vowel(&self) -> bool {
        matches!(
            self,
            Phoneme::A
                | Phoneme::E
                | Phoneme::Ee
                | Phoneme::I
                | Phoneme::O
                | Phoneme::Ou
                | Phoneme::U
                | Phoneme::Eu
                | Phoneme::An
                | Phoneme::On
                | Phoneme::In
        )
    }

    /// Short mnemonic, for a trace or a self-test.
    pub fn as_str(&self) -> &'static str {
        match self {
            Phoneme::A => "a", Phoneme::E => "e", Phoneme::Ee => "ee",
            Phoneme::I => "i", Phoneme::O => "o", Phoneme::Ou => "ou",
            Phoneme::U => "u", Phoneme::Eu => "eu",
            Phoneme::An => "an", Phoneme::On => "on", Phoneme::In => "in",
            Phoneme::B => "b", Phoneme::D => "d", Phoneme::G => "g",
            Phoneme::V => "v", Phoneme::Z => "z", Phoneme::Zh => "zh",
            Phoneme::M => "m", Phoneme::N => "n", Phoneme::L => "l",
            Phoneme::R => "r", Phoneme::P => "p", Phoneme::T => "t",
            Phoneme::K => "k", Phoneme::F => "f", Phoneme::S => "s",
            Phoneme::Sh => "sh", Phoneme::J => "j", Phoneme::W => "w",
            Phoneme::Silence => "_",
        }
    }
}

// ============================================================
//  Letter to sound (French)
// ============================================================

/// Converts text into phonemes.
///
/// The rules are applied longest-match-first on the remaining text. That
/// order is the whole algorithm: `ch` must be tried before `c`, `ou`
/// before `o`, `ain` before `an`. Trying the single letters first would
/// produce a reader that says `c-h` for `ch`, and no amount of later
/// patching recovers from it.
pub fn graphemes(text: &str) -> Vec<Phoneme> {
    let lower: Vec<u8> = text
        .bytes()
        .map(|byte| byte.to_ascii_lowercase())
        .collect();
    let mut out: Vec<Phoneme> = Vec::new();
    let mut index = 0;

    while index < lower.len() && out.len() < MAX_PHONEMES {
        let rest = &lower[index..];
        // Is the letter at this position the last of its word? A closure
        // capturing `index` would borrow it for the whole loop body, and
        // the body advances `index`.
        let final_letter =
            rest.len() == 1 || !rest.get(1).copied().is_some_and(|b| b.is_ascii_alphabetic());

        // Anything that is not a letter is a boundary. Several in a row
        // collapse into one silence: "a, b" must not pause twice.
        if !rest[0].is_ascii_alphabetic() {
            if !matches!(out.last(), Some(Phoneme::Silence) | None) {
                out.push(Phoneme::Silence);
            }
            index += 1;
            continue;
        }

        // A doubled consonant is pronounced once: `bonne` is /bon/, not
        // /bonn/. Consumed as a pair and emitted directly, bypassing the
        // context rules below -- `ss` between two vowels must stay /s/
        // (`poisson`), where a single `s` in the same place is /z/
        // (`usine`). Letting the pair fall through to the single-letter
        // rule would turn every `ss` into a `z`.
        if rest.len() >= 2 && rest[0] == rest[1] && !is_vowel_letter(rest[0]) {
            if let Some(phoneme) = plain_consonant(rest[0]) {
                out.push(phoneme);
            }
            index += 2;
            continue;
        }

        // --- three-letter groups ---
        if starts(rest, b"ain") || starts(rest, b"ein") {
            out.push(Phoneme::In);
            index += 3;
            continue;
        }
        if starts(rest, b"eau") {
            out.push(Phoneme::O);
            index += 3;
            continue;
        }
        if starts(rest, b"ill") {
            out.push(Phoneme::J);
            index += 3;
            continue;
        }

        // --- two-letter groups ---
        if starts(rest, b"ch") {
            out.push(Phoneme::Sh);
            index += 2;
            continue;
        }
        if starts(rest, b"ph") {
            out.push(Phoneme::F);
            index += 2;
            continue;
        }
        if starts(rest, b"gn") {
            // Approximated as n + a palatal glide: the palatal nasal has
            // no resonator of its own here, and saying so is better than
            // pretending the table has one.
            out.push(Phoneme::N);
            out.push(Phoneme::J);
            index += 2;
            continue;
        }
        if starts(rest, b"qu") {
            out.push(Phoneme::K);
            index += 2;
            continue;
        }
        if starts(rest, b"ou") {
            out.push(Phoneme::Ou);
            index += 2;
            continue;
        }
        if starts(rest, b"au") {
            out.push(Phoneme::O);
            index += 2;
            continue;
        }
        if starts(rest, b"eu") || starts(rest, b"oe") {
            out.push(Phoneme::Eu);
            index += 2;
            continue;
        }
        if starts(rest, b"oi") {
            out.push(Phoneme::W);
            out.push(Phoneme::A);
            index += 2;
            continue;
        }
        if starts(rest, b"ai") || starts(rest, b"ei") {
            out.push(Phoneme::E);
            index += 2;
            continue;
        }
        // A nasal is a vowel plus `n` or `m` **not** followed by a vowel:
        // `bon` is nasal, `bonne` is not. Skipping that test would make
        // the reader nasalise half the vocabulary.
        if (starts(rest, b"an") || starts(rest, b"en") || starts(rest, b"am") || starts(rest, b"em"))
            && !nasal_broken(&lower, index + 2)
        {
            out.push(Phoneme::An);
            index += 2;
            continue;
        }
        if (starts(rest, b"on") || starts(rest, b"om")) && !nasal_broken(&lower, index + 2) {
            out.push(Phoneme::On);
            index += 2;
            continue;
        }
        if (starts(rest, b"in") || starts(rest, b"im") || starts(rest, b"un"))
            && !nasal_broken(&lower, index + 2)
        {
            out.push(Phoneme::In);
            index += 2;
            continue;
        }

        // --- single letters ---
        let letter = rest[0];
        index += 1;
        match letter {
            b'a' => out.push(Phoneme::A),
            // A final `e` is silent in French, and pronouncing it is the
            // single most recognisable mistake a foreign reader makes.
            b'e' => {
                if !final_letter {
                    out.push(Phoneme::E);
                }
            }
            b'i' | b'y' => out.push(Phoneme::I),
            b'o' => out.push(Phoneme::O),
            b'u' => out.push(Phoneme::U),
            b'b' => out.push(Phoneme::B),
            // `c` before `e`, `i`, `y` is /s/; elsewhere /k/.
            b'c' => out.push(if soft_follows(&lower, index) {
                Phoneme::S
            } else {
                Phoneme::K
            }),
            b'd' => out.push(Phoneme::D),
            b'f' => out.push(Phoneme::F),
            // Same rule for `g`.
            b'g' => out.push(if soft_follows(&lower, index) {
                Phoneme::Zh
            } else {
                Phoneme::G
            }),
            b'h' => {} // Always silent.
            b'j' => out.push(Phoneme::Zh),
            b'k' => out.push(Phoneme::K),
            b'l' => out.push(Phoneme::L),
            b'm' => out.push(Phoneme::M),
            b'n' => out.push(Phoneme::N),
            b'p' => out.push(Phoneme::P),
            b'q' => out.push(Phoneme::K),
            b'r' => out.push(Phoneme::R),
            // `s` between two vowels is /z/: "usine", "raison".
            b's' => out.push(if between_vowels(&lower, index - 1) {
                Phoneme::Z
            } else {
                Phoneme::S
            }),
            // A final `t` or `d` is usually silent.
            b't' => {
                if !final_letter {
                    out.push(Phoneme::T);
                }
            }
            b'v' | b'w' => out.push(Phoneme::V),
            b'x' => {
                out.push(Phoneme::K);
                out.push(Phoneme::S);
            }
            b'z' => out.push(Phoneme::Z),
            _ => {}
        }
    }

    // A trailing silence gives the utterance an end: without it the last
    // phoneme is cut at the buffer boundary, which is heard as a click.
    if !matches!(out.last(), Some(Phoneme::Silence)) {
        out.push(Phoneme::Silence);
    }
    out
}

fn starts(rest: &[u8], pattern: &[u8]) -> bool {
    rest.len() >= pattern.len() && &rest[..pattern.len()] == pattern
}

fn is_vowel_letter(byte: u8) -> bool {
    matches!(byte, b'a' | b'e' | b'i' | b'o' | b'u' | b'y')
}

/// Does what follows a vowel-plus-nasal stop it being nasal?
///
/// Two cases, and leaving either out mis-reads a large slice of the
/// vocabulary:
///
/// * a **vowel** follows -- `bonne`, `panier`: the nasal consonant
///   attaches to the next syllable and the vowel stays oral;
/// * a **second n or m** follows -- `bonne`, `homme`: same thing, and it
///   is the case a test on vowels alone misses, because the letter it
///   looks at is a consonant.
fn nasal_broken(text: &[u8], index: usize) -> bool {
    match text.get(index).copied() {
        Some(byte) => is_vowel_letter(byte) || byte == b'n' || byte == b'm',
        None => false,
    }
}

/// Phoneme of a consonant read out of context.
///
/// Used for a doubled consonant, where the context rules must not apply.
fn plain_consonant(letter: u8) -> Option<Phoneme> {
    Some(match letter {
        b'b' => Phoneme::B,
        b'c' => Phoneme::K,
        b'd' => Phoneme::D,
        b'f' => Phoneme::F,
        b'g' => Phoneme::G,
        b'j' => Phoneme::Zh,
        b'k' => Phoneme::K,
        b'l' => Phoneme::L,
        b'm' => Phoneme::M,
        b'n' => Phoneme::N,
        b'p' => Phoneme::P,
        b'q' => Phoneme::K,
        b'r' => Phoneme::R,
        b's' => Phoneme::S,
        b't' => Phoneme::T,
        b'v' | b'w' => Phoneme::V,
        b'z' => Phoneme::Z,
        // `hh` and `xx` do not occur; returning nothing is right rather
        // than inventing a sound for them.
        _ => return None,
    })
}

/// Is the letter at `index` one of `e`, `i`, `y`?
fn soft_follows(text: &[u8], index: usize) -> bool {
    matches!(text.get(index), Some(b'e') | Some(b'i') | Some(b'y'))
}

/// Is the letter at `index` surrounded by two vowels?
fn between_vowels(text: &[u8], index: usize) -> bool {
    let before = index
        .checked_sub(1)
        .and_then(|previous| text.get(previous))
        .copied()
        .is_some_and(is_vowel_letter);
    let after = text.get(index + 1).copied().is_some_and(is_vowel_letter);
    before && after
}

// ============================================================
//  Fixed-point resonator
// ============================================================

/// Fractional bits of the filter arithmetic.
///
/// Twelve: enough precision that a resonator at 280 Hz -- the first
/// formant of `i`, the lowest in the table -- keeps its pitch, and few
/// enough that `i32` never overflows on the intermediate product with a
/// 16-bit sample.
const Q: u32 = 12;
const ONE: i32 = 1 << Q;

/// Cosine, in Q12, of an angle given in thousandths of a radian.
///
/// A five-term Taylor series folded into the first quadrant. The kernel
/// has no `libm` and cannot have floats; a table of 256 entries would
/// have been the other choice, and this is smaller and exact enough --
/// the residual error at the fold points is under one part in a thousand,
/// which moves a formant by less than a hertz.
fn cos_q12(milli_radians: i64) -> i32 {
    const PI: i64 = 3_142; // pi, in thousandths.
    const TWO_PI: i64 = 6_283;

    // Fold into [0, 2pi).
    let mut angle = milli_radians % TWO_PI;
    if angle < 0 {
        angle += TWO_PI;
    }
    // Fold into [0, pi] using cos(2pi - x) = cos(x).
    if angle > PI {
        angle = TWO_PI - angle;
    }
    // Fold into [0, pi/2] using cos(pi - x) = -cos(x).
    let (angle, sign) = if angle > PI / 2 {
        (PI - angle, -1i64)
    } else {
        (angle, 1i64)
    };

    // Taylor: 1 - x^2/2 + x^4/24 - x^6/720, with x in thousandths.
    // Each term is divided down as it goes, so nothing overflows.
    let x2 = angle * angle / 1_000; // x^2, in thousandths
    let term2 = x2 / 2;
    let term4 = x2 * x2 / 1_000 / 24;
    let term6 = x2 * x2 / 1_000 * x2 / 1_000 / 720;
    let value = 1_000 - term2 + term4 - term6;

    (sign * value * ONE as i64 / 1_000) as i32
}

/// A two-pole resonator, in Q12.
///
/// The transfer function is the classic one: a pole pair at radius `r`
/// and angle `theta`, giving `y[n] = a*x[n] + b*y[n-1] - c*y[n-2]`. The
/// gain `a` is chosen so the peak sits at unity, failing which a narrow
/// resonance would be forty times louder than a wide one and the vowels
/// would have no relative loudness at all.
#[derive(Debug, Clone, Copy, Default)]
struct Resonator {
    a: i32,
    b: i32,
    c: i32,
    y1: i32,
    y2: i32,
}

impl Resonator {
    /// Builds the coefficients for a centre frequency and a bandwidth.
    fn new(frequency: u32, bandwidth: u32) -> Self {
        if frequency == 0 || frequency * 2 >= SAMPLE_RATE {
            return Resonator::default();
        }
        // r = exp(-pi * bw / fs), approximated by its first-order form
        // 1 - pi*bw/fs. Over the bandwidths of the table -- 55 to 400 Hz
        // at 48 kHz -- the exponent stays under 0.027, where the linear
        // term is within one part in a thousand of the exponential.
        let decay = (3_142i64 * bandwidth as i64) / SAMPLE_RATE as i64; // in thousandths
        let radius = ONE as i64 - (decay * ONE as i64 / 1_000);
        let radius = radius.clamp(0, ONE as i64 - 1);

        // theta = 2*pi*f/fs, in thousandths of a radian.
        let theta = 6_283i64 * frequency as i64 / SAMPLE_RATE as i64;
        let cosine = cos_q12(theta) as i64;

        let b = 2 * radius * cosine / ONE as i64;
        let c = radius * radius / ONE as i64;
        // a = 1 - b + c, the value that puts the DC response at unity.
        let a = ONE as i64 - b + c;

        Resonator {
            a: a as i32,
            b: b as i32,
            c: c as i32,
            y1: 0,
            y2: 0,
        }
    }

    /// One sample through the filter.
    #[inline]
    fn step(&mut self, input: i32) -> i32 {
        let accumulator = (self.a as i64 * input as i64
            + self.b as i64 * self.y1 as i64
            - self.c as i64 * self.y2 as i64)
            >> Q;
        // Clamped, not wrapped: an unstable filter must go quiet, not
        // explode into a full-scale square wave in somebody's ears.
        let output = accumulator.clamp(-(1 << 24), 1 << 24) as i32;
        self.y2 = self.y1;
        self.y1 = output;
        output
    }
}

// ============================================================
//  Synthesis
// ============================================================

/// Parameters of a voice.
#[derive(Debug, Clone, Copy)]
pub struct VoiceSettings {
    /// Fundamental frequency at the start of an utterance, in hertz.
    pub pitch_hz: u32,
    /// Speaking rate, as a percentage: 100 is the table's own timing,
    /// 200 twice as fast.
    pub rate_percent: u32,
    /// Loudness, out of 256.
    pub volume: u32,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        VoiceSettings {
            // 120 Hz: a low voice, which stays intelligible on the small
            // speakers a machine like this actually has.
            pitch_hz: 120,
            rate_percent: 100,
            volume: 180,
        }
    }
}

impl VoiceSettings {
    /// Clamps every field into a range where the synthesiser stays sane.
    ///
    /// A rate of zero would divide by zero; a pitch of zero would give a
    /// pulse train with no period. Both come from a settings file, hence
    /// from outside.
    pub fn sanitised(&self) -> Self {
        VoiceSettings {
            pitch_hz: self.pitch_hz.clamp(60, 400),
            rate_percent: self.rate_percent.clamp(25, 400),
            volume: self.volume.min(256),
        }
    }
}

/// Pseudo-random source for the fricatives.
///
/// A 32-bit xorshift: three shifts and three exclusive ors per sample,
/// with a period long enough that no repetition is audible in an
/// utterance. A linear congruential generator would have been shorter and
/// its low bits are periodic, which for noise is exactly the wrong end.
struct Noise(u32);

impl Noise {
    #[inline]
    fn next(&mut self) -> i32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        // Centred: an uncentred noise carries a constant offset, which
        // the resonators turn into a click at every phoneme boundary.
        (self.0 >> 16) as i32 - 32_768
    }
}

/// Renders a phoneme sequence into PCM.
///
/// Mono at `SAMPLE_RATE`. The caller interleaves for stereo -- doing it
/// here would double the buffer for nothing, since both channels carry
/// the same signal.
pub fn render(phonemes: &[Phoneme], settings: VoiceSettings) -> Vec<i16> {
    let settings = settings.sanitised();
    let mut out = Vec::new();
    let mut noise = Noise(0x1234_5678);
    let mut phase = 0u32;

    // The pitch falls over the utterance: a flat pitch is what makes a
    // synthetic voice sound like a machine reading a list. A quarter of
    // the starting frequency, spread over the whole utterance.
    let total_ms: u32 = phonemes
        .iter()
        .map(|phoneme| phoneme.duration_ms() * 100 / settings.rate_percent)
        .sum::<u32>()
        .max(1);
    let mut elapsed_ms = 0u32;

    for phoneme in phonemes.iter().take(MAX_PHONEMES) {
        let voice = phoneme.voice();
        let duration_ms = (voice.duration_ms * 100 / settings.rate_percent).max(1);
        let samples = (SAMPLE_RATE * duration_ms / 1000).max(1) as usize;

        let mut filters = [
            Resonator::new(voice.formants[0], voice.bandwidths[0]),
            Resonator::new(voice.formants[1], voice.bandwidths[1]),
            Resonator::new(voice.formants[2], voice.bandwidths[2]),
        ];

        // Pitch at this point of the fall.
        let pitch = settings.pitch_hz
            - (settings.pitch_hz / 4) * elapsed_ms.min(total_ms) / total_ms;
        let pitch = pitch.max(40);
        let period = (SAMPLE_RATE / pitch).max(1);

        for index in 0..samples {
            let excitation = match voice.source {
                Source::Silent => 0,
                Source::Noise => noise.next() / 2,
                Source::Voiced => glottal(&mut phase, period),
                Source::Mixed => glottal(&mut phase, period) + noise.next() / 6,
                Source::Plosive => {
                    // Silence for the first third, then a burst. That
                    // shape is what a stop consonant *is*: the closure is
                    // as much a part of it as the release, and a burst
                    // with no closure is heard as a click.
                    if index * 3 < samples {
                        0
                    } else {
                        noise.next() / 2 + glottal(&mut phase, period) / 3
                    }
                }
            };

            // The three resonators in parallel, not in series: a cascade
            // multiplies the peaks and buries the third formant. Parallel
            // keeps each formant at its own level, which is what the
            // gains in the table assume.
            let mut sum = 0i32;
            for filter in filters.iter_mut() {
                sum += filter.step(excitation);
            }
            sum /= 3;

            // Envelope: a short fade in and out on each phoneme. Without
            // it every boundary is a discontinuity, and a sequence of
            // discontinuities is heard as a crackle rather than as
            // speech.
            let fade = (SAMPLE_RATE / 400).max(1) as usize;
            let envelope = if index < fade {
                (index * 256 / fade) as i32
            } else if index + fade >= samples {
                ((samples - index) * 256 / fade).min(256) as i32
            } else {
                256
            };

            let value = sum * voice.gain as i32 / 256 * settings.volume as i32 / 256 * envelope
                / 256;
            out.push(value.clamp(i16::MIN as i32, i16::MAX as i32) as i16);
        }

        elapsed_ms += duration_ms;
    }

    out
}

/// One sample of the glottal source.
///
/// A sawtooth rather than an impulse train: an impulse has energy at
/// every harmonic in equal measure, which sounds like a buzzer. A
/// sawtooth falls off at 6 dB per octave, which is roughly what a glottis
/// does and is the standard cheap approximation.
#[inline]
fn glottal(phase: &mut u32, period: u32) -> i32 {
    *phase += 1;
    if *phase >= period {
        *phase = 0;
    }
    // From +16384 down to -16384 across the period.
    16_384 - (*phase as i32 * 32_768 / period as i32)
}

/// The whole chain: text in, samples out.
pub fn synthesise(text: &str, settings: VoiceSettings) -> Vec<i16> {
    render(&graphemes(text), settings)
}

/// Phoneme sequence written out, for a trace or a self-test.
pub fn transcribe(phonemes: &[Phoneme]) -> String {
    let mut out = String::new();
    for (index, phoneme) in phonemes.iter().enumerate() {
        if index > 0 {
            out.push('-');
        }
        out.push_str(phoneme.as_str());
    }
    out
}

/// Duration an utterance will take, in milliseconds.
///
/// Computed from the phonemes rather than from the sample count so a
/// caller can size a deadline before rendering anything.
pub fn duration_ms(phonemes: &[Phoneme], settings: VoiceSettings) -> u32 {
    let settings = settings.sanitised();
    phonemes
        .iter()
        .take(MAX_PHONEMES)
        .map(|phoneme| (phoneme.duration_ms() * 100 / settings.rate_percent).max(1))
        .sum()
}

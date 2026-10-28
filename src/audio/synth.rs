//! Integer sine, for the test tone of the sound settings.
//!
//! Bhaskara I's approximation, `sin x ~ 16x(pi - x) / (5pi^2 - 4x(pi - x))`,
//! rewritten over an integer half period `H`: it stays within 0.2 % of the
//! true sine, far below what 16 bits of audio can show, and it is two
//! multiplications and a division -- no floating point, which the kernel
//! does not have.
//!
//! It was part of `audio::demo`, the tune the system used to install for
//! the player; the tune is gone, the sine the settings need stays.

/// Phase units in half a period.
const HALF: i64 = 1 << 15;

/// Sine of a phase in `0..2 * HALF`, scaled to `amplitude`.
pub fn sine(phase: i64, amplitude: i64) -> i64 {
    let phase = phase.rem_euclid(2 * HALF);
    let (p, sign) = if phase < HALF { (phase, 1) } else { (phase - HALF, -1) };
    let product = p * (HALF - p);
    let value = 4 * product * amplitude / (5 * HALF * HALF / 4 - product);
    sign * value
}

//! JavaScript numbers: IEEE 754 doubles, computed with integers.
//!
//! JavaScript has one number type, the 64-bit double, and a page's
//! scripts count on its exact behaviour -- `0.1 + 0.2`, `1 / 0`, `NaN`,
//! `-0`, integers exact up to 2^53. The kernel runs soft-float with SSE
//! off and allows no `f64` (see `DECISIONS.md`), so `Num` keeps the 64
//! bits of the double in a `u64` and does the arithmetic itself.
//!
//! The four operations, the remainder and the square root are **correctly
//! rounded** -- round to nearest, ties to even, subnormals included --
//! so they give bit for bit what a hardware FPU gives: every operation
//! computes the exact result widened to 128 bits, with a sticky bit for
//! whatever falls below, and `pack` rounds once. Conversions to and from
//! decimal are exact too (`big` carries the long integers); the printed
//! form is the shortest one that reads back to the same number, as
//! ECMAScript's `Number::toString` requires.
//!
//! The transcendental functions (`exp`, `log`, `sin`...) are series
//! after argument reduction: close to the last bit, not always on it,
//! which ECMAScript allows ("implementation-approximated").

use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;

use super::big::Big;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Num(pub u64);

const SIGN: u64 = 1 << 63;
const FRACTION: u64 = (1 << 52) - 1;
const IMPLICIT: u64 = 1 << 52;

/// A finite non-zero number taken apart: value = m * 2^e, m < 2^53.
#[derive(Clone, Copy)]
struct Parts {
    negative: bool,
    m: u64,
    e: i32,
}

impl core::fmt::Debug for Num {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.to_js_string())
    }
}

impl Num {
    pub const NAN: Num = Num(0x7ff8_0000_0000_0000);
    pub const INFINITY: Num = Num(0x7ff0_0000_0000_0000);
    pub const NEG_INFINITY: Num = Num(0xfff0_0000_0000_0000);
    pub const ZERO: Num = Num(0);
    pub const NEG_ZERO: Num = Num(SIGN);
    pub const ONE: Num = Num(0x3ff0_0000_0000_0000);
    pub const HALF: Num = Num(0x3fe0_0000_0000_0000);
    pub const TWO: Num = Num(0x4000_0000_0000_0000);
    pub const MAX: Num = Num(0x7fef_ffff_ffff_ffff);
    pub const MIN_POSITIVE: Num = Num(1);
    pub const EPSILON: Num = Num(0x3cb0_0000_0000_0000);
    pub const PI: Num = Num(0x4009_21fb_5444_2d18);
    pub const E: Num = Num(0x4005_bf0a_8b14_5769);
    pub const LN2: Num = Num(0x3fe6_2e42_fefa_39ef);
    pub const LN10: Num = Num(0x4002_6bb1_bbb5_5516);
    pub const LOG2E: Num = Num(0x3ff7_1547_652b_82fe);
    pub const LOG10E: Num = Num(0x3fdb_cb7b_1526_e50e);
    pub const SQRT2: Num = Num(0x3ff6_a09e_667f_3bcd);
    pub const SQRT1_2: Num = Num(0x3fe6_a09e_667f_3bcd);
    /// 2^53 - 1.
    pub const MAX_SAFE: Num = Num(0x433f_ffff_ffff_ffff);

    // --------------------------------------------------------
    //  Classification
    // --------------------------------------------------------

    fn exponent_field(self) -> u64 {
        (self.0 >> 52) & 0x7ff
    }

    pub fn is_nan(self) -> bool {
        self.exponent_field() == 0x7ff && self.0 & FRACTION != 0
    }

    pub fn is_infinite(self) -> bool {
        self.0 & !SIGN == Self::INFINITY.0
    }

    pub fn is_finite(self) -> bool {
        self.exponent_field() != 0x7ff
    }

    pub fn is_zero(self) -> bool {
        self.0 & !SIGN == 0
    }

    pub fn is_sign_negative(self) -> bool {
        self.0 & SIGN != 0
    }

    /// Negative and not NaN, zero included when it is -0.
    pub fn is_negative(self) -> bool {
        self.is_sign_negative() && !self.is_nan()
    }

    fn parts(self) -> Parts {
        let negative = self.is_sign_negative();
        let field = self.exponent_field() as i32;
        let fraction = self.0 & FRACTION;
        if field == 0 {
            Parts { negative, m: fraction, e: -1074 }
        } else {
            Parts { negative, m: fraction | IMPLICIT, e: field - 1075 }
        }
    }

    /// The value as sign, integer significand and binary exponent:
    /// `m * 2^e` (finite numbers only).
    pub fn decompose(self) -> (bool, u64, i32) {
        let p = self.parts();
        (p.negative, p.m, p.e)
    }

    /// The double nearest `m * 2^e` (sign apart), ties to even; `m`'s
    /// lowest bit may stand for lost bits below it (see `pack`).
    pub fn from_parts(negative: bool, e: i32, m: u128) -> Num {
        Self::pack(negative, e, m)
    }

    /// Rounds `m * 2^e` (sign apart) to the nearest double, ties to even.
    /// `m`'s lowest bit may be a sticky bit standing for lost bits below
    /// it; there must be at least two bits of margin under the 53 kept.
    fn pack(negative: bool, e: i32, m: u128) -> Num {
        let sign = if negative { SIGN } else { 0 };
        if m == 0 {
            return Num(sign);
        }
        let lead = m.leading_zeros() as i32;
        let m = m << lead;
        let e = e - lead;
        // Now value = m * 2^e with m in [2^127, 2^128): the leading bit
        // is worth 2^(e + 127).
        let biased = e + 127 + 1023;
        if biased >= 2047 {
            return Num(sign | Self::INFINITY.0);
        }
        let shift: i32 = if biased > 0 { 75 } else { 75 + 1 - biased };
        let (kept, rest, half) = if shift >= 129 {
            (0u128, 1u128, 2u128) // below half of the smallest subnormal
        } else if shift == 128 {
            (0u128, m, 1u128 << 127)
        } else {
            let mask = (1u128 << shift) - 1;
            (m >> shift, m & mask, 1u128 << (shift - 1))
        };
        let mut kept = kept as u64;
        if rest > half || (rest == half && kept & 1 == 1) {
            kept += 1;
        }
        let bits = if biased > 0 {
            // kept is in [2^52, 2^53]: the carry of a round up spills into
            // the exponent, which is exactly the right behaviour.
            ((biased as u64) << 52) + (kept - IMPLICIT)
        } else {
            kept
        };
        Num(sign | bits)
    }

    // --------------------------------------------------------
    //  Conversions with integers
    // --------------------------------------------------------

    pub fn from_i64(value: i64) -> Num {
        Self::pack(value < 0, 0, value.unsigned_abs() as u128)
    }

    pub fn from_u64(value: u64) -> Num {
        Self::pack(false, 0, value as u128)
    }

    pub fn from_i32(value: i32) -> Num {
        Self::from_i64(value as i64)
    }

    /// The value, when it is an integer that fits an `i64`.
    pub fn to_i64_exact(self) -> Option<i64> {
        if !self.is_finite() {
            return None;
        }
        if self.is_zero() {
            return Some(0);
        }
        let p = self.parts();
        let magnitude: u64 = if p.e >= 0 {
            if p.e > 10 {
                return None;
            }
            let wide = (p.m as u128) << p.e;
            if wide > i64::MAX as u128 {
                return None;
            }
            wide as u64
        } else {
            if -p.e >= 64 || p.m & ((1u64 << -p.e) - 1) != 0 {
                return None;
            }
            p.m >> -p.e
        };
        Some(if p.negative { -(magnitude as i64) } else { magnitude as i64 })
    }

    /// Truncation towards zero, saturated to the `i64` range; NaN is 0.
    pub fn to_i64_saturating(self) -> i64 {
        if self.is_nan() || self.is_zero() {
            return 0;
        }
        if self.is_infinite() {
            return if self.is_sign_negative() { i64::MIN } else { i64::MAX };
        }
        let p = self.parts();
        let magnitude: u128 = if p.e >= 0 {
            if p.e > 70 {
                u128::MAX
            } else {
                (p.m as u128) << p.e
            }
        } else if -p.e >= 64 {
            0
        } else {
            (p.m >> -p.e) as u128
        };
        if p.negative {
            if magnitude > i64::MAX as u128 + 1 {
                i64::MIN
            } else {
                (magnitude as i128).wrapping_neg() as i64
            }
        } else if magnitude > i64::MAX as u128 {
            i64::MAX
        } else {
            magnitude as i64
        }
    }

    /// ECMAScript `ToUint32`: truncation, then modulo 2^32.
    pub fn to_u32(self) -> u32 {
        if !self.is_finite() || self.is_zero() {
            return 0;
        }
        let p = self.parts();
        let low: u32 = if p.e >= 0 {
            if p.e >= 64 {
                0
            } else {
                ((p.m as u128) << p.e) as u32
            }
        } else if -p.e >= 64 {
            0
        } else {
            (p.m >> -p.e) as u32
        };
        if p.negative {
            low.wrapping_neg()
        } else {
            low
        }
    }

    /// ECMAScript `ToInt32`.
    pub fn to_i32(self) -> i32 {
        self.to_u32() as i32
    }

    /// A small non-negative integer index (array index, count...).
    pub fn to_index(self) -> Option<usize> {
        match self.to_i64_exact() {
            Some(value) if value >= 0 => Some(value as usize),
            _ => None,
        }
    }

    // --------------------------------------------------------
    //  Arithmetic
    // --------------------------------------------------------

    pub fn neg(self) -> Num {
        Num(self.0 ^ SIGN)
    }

    pub fn abs(self) -> Num {
        Num(self.0 & !SIGN)
    }

    pub fn add(self, other: Num) -> Num {
        if self.is_nan() || other.is_nan() {
            return Self::NAN;
        }
        if self.is_infinite() || other.is_infinite() {
            if self.is_infinite() && other.is_infinite() && self.0 != other.0 {
                return Self::NAN;
            }
            return if self.is_infinite() { self } else { other };
        }
        if self.is_zero() && other.is_zero() {
            // -0 + -0 is -0; any other mix of zeros is +0.
            return Num(self.0 & other.0);
        }
        if self.is_zero() {
            return other;
        }
        if other.is_zero() {
            return self;
        }
        let mut a = self.parts();
        let mut b = other.parts();
        if a.e < b.e {
            core::mem::swap(&mut a, &mut b);
        }
        // a has the larger exponent: both go 64 bits up, b is shifted
        // right to a's scale, what falls off jammed into a sticky bit.
        let big_a = (a.m as u128) << 64;
        let distance = (a.e - b.e) as u32;
        let wide_b = (b.m as u128) << 64;
        let big_b = if distance >= 128 {
            (wide_b != 0) as u128
        } else {
            let shifted = wide_b >> distance;
            let lost = wide_b & ((1u128 << distance) - 1);
            shifted | (lost != 0) as u128
        };
        let e = a.e - 64;
        if a.negative == b.negative {
            Self::pack(a.negative, e, big_a + big_b)
        } else {
            match big_a.cmp(&big_b) {
                Ordering::Equal => Self::ZERO,
                Ordering::Greater => Self::pack(a.negative, e, big_a - big_b),
                Ordering::Less => Self::pack(b.negative, e, big_b - big_a),
            }
        }
    }

    pub fn sub(self, other: Num) -> Num {
        self.add(other.neg())
    }

    pub fn mul(self, other: Num) -> Num {
        let negative = self.is_sign_negative() != other.is_sign_negative();
        if self.is_nan() || other.is_nan() {
            return Self::NAN;
        }
        if self.is_infinite() || other.is_infinite() {
            if self.is_zero() || other.is_zero() {
                return Self::NAN;
            }
            return Num(Self::INFINITY.0 | if negative { SIGN } else { 0 });
        }
        if self.is_zero() || other.is_zero() {
            return Num(if negative { SIGN } else { 0 });
        }
        let a = self.parts();
        let b = other.parts();
        Self::pack(negative, a.e + b.e, a.m as u128 * b.m as u128)
    }

    /// `m * 2^e` with m's top bit at bit 52.
    fn normalized(p: Parts) -> Parts {
        let shift = p.m.leading_zeros() as i32 - 11;
        Parts { negative: p.negative, m: p.m << shift, e: p.e - shift }
    }

    pub fn div(self, other: Num) -> Num {
        let negative = self.is_sign_negative() != other.is_sign_negative();
        let signed = |bits: u64| Num(bits | if negative { SIGN } else { 0 });
        if self.is_nan() || other.is_nan() {
            return Self::NAN;
        }
        if self.is_infinite() {
            return if other.is_infinite() { Self::NAN } else { signed(Self::INFINITY.0) };
        }
        if other.is_infinite() {
            return signed(0);
        }
        if other.is_zero() {
            return if self.is_zero() { Self::NAN } else { signed(Self::INFINITY.0) };
        }
        if self.is_zero() {
            return signed(0);
        }
        let a = Self::normalized(self.parts());
        let b = Self::normalized(other.parts());
        let numerator = (a.m as u128) << 75;
        let quotient = numerator / b.m as u128;
        let sticky = (numerator % b.m as u128 != 0) as u128;
        Self::pack(negative, a.e - 75 - b.e, quotient | sticky)
    }

    /// ECMAScript `%`: the remainder of the truncated division, exact,
    /// with the sign of the dividend.
    pub fn rem(self, other: Num) -> Num {
        if self.is_nan() || other.is_nan() || self.is_infinite() || other.is_zero() {
            return Self::NAN;
        }
        if other.is_infinite() || self.is_zero() {
            return self;
        }
        let a = Self::normalized(self.parts());
        let b = Self::normalized(other.parts());
        if a.e < b.e || (a.e == b.e && a.m < b.m) {
            return self;
        }
        let mut remainder = a.m as u128 % b.m as u128;
        let mut distance = a.e - b.e;
        while distance > 0 {
            let step = distance.min(64);
            remainder = (remainder << step) % b.m as u128;
            distance -= step;
        }
        Self::pack(a.negative, b.e, remainder)
    }

    pub fn sqrt(self) -> Num {
        if self.is_nan() || (self.is_sign_negative() && !self.is_zero()) {
            return Self::NAN;
        }
        if self.is_zero() || self.is_infinite() {
            return self;
        }
        let mut p = Self::normalized(self.parts());
        if p.e & 1 != 0 {
            p.m <<= 1;
            p.e -= 1;
        }
        // m * 2^e = (m * 2^70) * 2^(e - 70), and 70 is even: the root of
        // the widened m has 61 or more bits, plenty for 53 plus rounding.
        let wide = (p.m as u128) << 70;
        let root = isqrt_u128(wide);
        let sticky = (root * root != wide) as u128;
        Self::pack(false, (p.e - 70) / 2, root | sticky)
    }

    // --------------------------------------------------------
    //  Comparisons
    // --------------------------------------------------------

    /// IEEE comparison: `None` when either is NaN; -0 equals +0.
    pub fn partial_cmp(self, other: Num) -> Option<Ordering> {
        if self.is_nan() || other.is_nan() {
            return None;
        }
        if self.is_zero() && other.is_zero() {
            return Some(Ordering::Equal);
        }
        let key = |bits: u64| -> i64 {
            if bits & SIGN != 0 {
                -((bits & !SIGN) as i64)
            } else {
                bits as i64
            }
        };
        Some(key(self.0).cmp(&key(other.0)))
    }

    /// `==` on numbers: NaN is unequal to everything, -0 equals +0.
    pub fn equals(self, other: Num) -> bool {
        self.partial_cmp(other) == Some(Ordering::Equal)
    }

    pub fn lt(self, other: Num) -> bool {
        self.partial_cmp(other) == Some(Ordering::Less)
    }

    /// `Object.is`: NaN is itself, -0 is not +0.
    pub fn same_value(self, other: Num) -> bool {
        if self.is_nan() && other.is_nan() {
            return true;
        }
        self.0 == other.0
    }

    pub fn max(self, other: Num) -> Num {
        if self.is_nan() || other.is_nan() {
            return Self::NAN;
        }
        match self.partial_cmp(other) {
            Some(Ordering::Greater) => self,
            Some(Ordering::Less) => other,
            _ => Num(self.0 & other.0), // +0 wins over -0
        }
    }

    pub fn min(self, other: Num) -> Num {
        if self.is_nan() || other.is_nan() {
            return Self::NAN;
        }
        match self.partial_cmp(other) {
            Some(Ordering::Less) => self,
            Some(Ordering::Greater) => other,
            _ => Num(self.0 | other.0), // -0 wins over +0
        }
    }

    // --------------------------------------------------------
    //  Rounding to integers
    // --------------------------------------------------------

    pub fn trunc(self) -> Num {
        if !self.is_finite() || self.is_zero() {
            return self;
        }
        let exponent = self.exponent_field() as i32 - 1023;
        if exponent >= 52 {
            return self;
        }
        if exponent < 0 {
            return Num(self.0 & SIGN);
        }
        let mask = FRACTION >> exponent;
        Num(self.0 & !mask)
    }

    pub fn is_integer(self) -> bool {
        self.is_finite() && self.trunc().0 == self.0
    }

    pub fn floor(self) -> Num {
        let truncated = self.trunc();
        if self.is_sign_negative() && truncated.0 != self.0 {
            truncated.sub(Self::ONE)
        } else {
            truncated
        }
    }

    pub fn ceil(self) -> Num {
        let truncated = self.trunc();
        if !self.is_sign_negative() && truncated.0 != self.0 {
            truncated.add(Self::ONE)
        } else if self.is_sign_negative() && truncated.is_zero() {
            Self::NEG_ZERO
        } else {
            truncated
        }
    }

    /// `Math.round`: halves go up, -0.4 gives -0.
    pub fn round(self) -> Num {
        if !self.is_finite() || self.is_integer() {
            return self;
        }
        let floor = self.floor();
        let result = if self.sub(floor).partial_cmp(Self::HALF) != Some(Ordering::Less) {
            floor.add(Self::ONE)
        } else {
            floor
        };
        if result.is_zero() && self.is_sign_negative() {
            Self::NEG_ZERO
        } else {
            result
        }
    }

    // --------------------------------------------------------
    //  Decimal text
    // --------------------------------------------------------

    /// The exact decimal expansion of a finite positive number: digits
    /// (no leading or trailing zero) and `point` such that the value is
    /// 0.DIGITS * 10^point.
    pub fn exact_decimal(self) -> (Vec<u8>, i32) {
        let p = self.parts();
        let (digits, point) = if p.e >= 0 {
            let digits = Big::from_u128(p.m as u128).shl(p.e as u32).decimal();
            let point = digits.len() as i32;
            (digits, point)
        } else {
            // m / 2^k = m * 5^k / 10^k.
            let mut big = Big::from_u128(p.m as u128);
            big.mul_pow5((-p.e) as u32);
            let digits = big.decimal();
            let point = digits.len() as i32 + p.e;
            (digits, point)
        };
        let end = digits.iter().rposition(|d| *d != b'0').map_or(0, |i| i + 1);
        (digits[..end].to_vec(), point)
    }

    /// Rounds an exact expansion to `count` significant digits, halves
    /// up.
    fn round_digits(digits: &[u8], point: i32, count: usize) -> (Vec<u8>, i32) {
        Self::round_digits_with(digits, point, count, false)
    }

    /// The same, an exact half going to the even digit when `even`.
    fn round_digits_with(digits: &[u8], point: i32, count: usize, even: bool) -> (Vec<u8>, i32) {
        if digits.len() <= count {
            return (digits.to_vec(), point);
        }
        let mut kept = digits[..count].to_vec();
        let exact_half = digits[count] == b'5' && digits[count + 1..].iter().all(|d| *d == b'0');
        let up = if even && exact_half { kept.last().is_some_and(|d| (d - b'0') % 2 == 1) } else { digits[count] >= b'5' };
        if up {
            let mut index = count;
            loop {
                if index == 0 {
                    kept.insert(0, b'1');
                    kept.truncate(count);
                    return (kept, point + 1);
                }
                index -= 1;
                if kept[index] == b'9' {
                    kept[index] = b'0';
                } else {
                    kept[index] += 1;
                    break;
                }
            }
        }
        let end = kept.iter().rposition(|d| *d != b'0').map_or(0, |i| i + 1);
        kept.truncate(end.max(1));
        (kept, point)
    }

    /// The shortest digits that read back as this number (positive and
    /// finite): digits and `point`, the value being 0.DIGITS * 10^point.
    pub fn shortest(self) -> (Vec<u8>, i32) {
        let (digits, point) = self.exact_decimal();
        // Of two shortest candidates at the same distance, the even one
        // (ECMAScript's Number::toString, Python's repr).
        for count in 1..=17 {
            let (candidate, candidate_point) = Self::round_digits_with(&digits, point, count, true);
            let exponent = candidate_point - candidate.len() as i32;
            if Self::from_decimal(&candidate, exponent).0 == self.abs().0 {
                return (candidate, candidate_point);
            }
        }
        Self::round_digits(&digits, point, 17)
    }

    /// ECMAScript `Number::toString(10)`.
    pub fn to_js_string(self) -> String {
        if self.is_nan() {
            return String::from("NaN");
        }
        if self.is_zero() {
            return String::from("0");
        }
        if self.is_infinite() {
            return String::from(if self.is_sign_negative() { "-Infinity" } else { "Infinity" });
        }
        let mut out = String::new();
        if self.is_sign_negative() {
            out.push('-');
        }
        if let Some(integer) = self.abs().to_i64_exact() {
            if integer.unsigned_abs() <= 1 << 53 {
                out.push_str(&alloc::format!("{}", integer));
                return out;
            }
        }
        let (digits, n) = self.abs().shortest();
        let k = digits.len() as i32;
        let text = |bytes: &[u8]| String::from(core::str::from_utf8(bytes).unwrap_or("0"));
        if k <= n && n <= 21 {
            out.push_str(&text(&digits));
            for _ in 0..(n - k) {
                out.push('0');
            }
        } else if 0 < n && n <= 21 {
            out.push_str(&text(&digits[..n as usize]));
            out.push('.');
            out.push_str(&text(&digits[n as usize..]));
        } else if -6 < n && n <= 0 {
            out.push_str("0.");
            for _ in 0..(-n) {
                out.push('0');
            }
            out.push_str(&text(&digits));
        } else {
            out.push_str(&text(&digits[..1]));
            if k > 1 {
                out.push('.');
                out.push_str(&text(&digits[1..]));
            }
            out.push('e');
            let exponent = n - 1;
            out.push(if exponent >= 0 { '+' } else { '-' });
            out.push_str(&alloc::format!("{}", exponent.abs()));
        }
        out
    }

    /// `Number.prototype.toFixed(fraction)`.
    pub fn to_fixed(self, fraction: usize) -> String {
        if !self.is_finite() || self.abs().partial_cmp(Self::from_decimal(b"1", 21)) != Some(Ordering::Less) {
            return self.to_js_string();
        }
        let mut out = String::new();
        if self.is_sign_negative() && !self.is_zero() {
            out.push('-');
        }
        let (digits, point) = if self.is_zero() { (Vec::new(), 0) } else { self.abs().exact_decimal() };
        // Keep the digits down to 10^-fraction, rounding halves up.
        let wanted = point + fraction as i32;
        let (kept, point) = if wanted < 0 {
            (Vec::new(), point)
        } else if wanted == 0 {
            if digits.first().is_some_and(|d| *d >= b'5') {
                (alloc::vec![b'1'], point + 1)
            } else {
                (Vec::new(), point)
            }
        } else {
            let (mut kept, new_point) = Self::round_digits(&digits, point, wanted as usize);
            // round_digits trims zeros: pad back to the fraction.
            let wanted = new_point + fraction as i32;
            while (kept.len() as i32) < wanted {
                kept.push(b'0');
            }
            (kept, new_point)
        };
        let integer_len = point.max(0) as usize;
        let mut all: Vec<u8> = Vec::new();
        // Leading zeros for numbers under one.
        if point < 0 {
            for _ in 0..(-point) {
                all.push(b'0');
            }
        }
        all.extend_from_slice(&kept);
        while all.len() < integer_len + fraction {
            all.push(b'0');
        }
        let integer_part = &all[..integer_len.min(all.len())];
        if integer_part.is_empty() {
            out.push('0');
        } else {
            out.push_str(core::str::from_utf8(integer_part).unwrap_or("0"));
        }
        if fraction > 0 {
            out.push('.');
            let rest = &all[integer_len.min(all.len())..];
            out.push_str(core::str::from_utf8(&rest[..fraction.min(rest.len())]).unwrap_or(""));
        }
        out
    }

    /// `Number.prototype.toPrecision(precision)`.
    pub fn to_precision(self, precision: usize) -> String {
        if !self.is_finite() {
            return self.to_js_string();
        }
        let mut out = String::new();
        if self.is_sign_negative() && !self.is_zero() {
            out.push('-');
        }
        let (mut digits, point) = if self.is_zero() {
            (alloc::vec![b'0'], 1)
        } else {
            let (digits, point) = self.abs().exact_decimal();
            Self::round_digits(&digits, point, precision)
        };
        while digits.len() < precision {
            digits.push(b'0');
        }
        let exponent = point - 1;
        let text = |bytes: &[u8]| String::from(core::str::from_utf8(bytes).unwrap_or(""));
        if exponent < -6 || exponent >= precision as i32 {
            out.push_str(&text(&digits[..1]));
            if precision > 1 {
                out.push('.');
                out.push_str(&text(&digits[1..]));
            }
            out.push('e');
            out.push(if exponent >= 0 { '+' } else { '-' });
            out.push_str(&alloc::format!("{}", exponent.abs()));
        } else if exponent >= 0 {
            let split = (exponent + 1) as usize;
            out.push_str(&text(&digits[..split]));
            if precision > split {
                out.push('.');
                out.push_str(&text(&digits[split..]));
            }
        } else {
            out.push_str("0.");
            for _ in 0..(-exponent - 1) {
                out.push('0');
            }
            out.push_str(&text(&digits));
        }
        out
    }

    /// `Number.prototype.toExponential(fraction)`: `None` gives as many
    /// digits as it takes to read the number back.
    pub fn to_exponential(self, fraction: Option<usize>) -> String {
        if !self.is_finite() {
            return self.to_js_string();
        }
        let mut out = String::new();
        if self.is_sign_negative() && !self.is_zero() {
            out.push('-');
        }
        let (mut digits, point) = if self.is_zero() {
            (alloc::vec![b'0'], 1)
        } else {
            match fraction {
                None => self.abs().shortest(),
                Some(fraction) => {
                    let (digits, point) = self.abs().exact_decimal();
                    Self::round_digits(&digits, point, fraction + 1)
                }
            }
        };
        if let Some(fraction) = fraction {
            while digits.len() < fraction + 1 {
                digits.push(b'0');
            }
        }
        let text = |bytes: &[u8]| String::from(core::str::from_utf8(bytes).unwrap_or(""));
        out.push_str(&text(&digits[..1]));
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&text(&digits[1..]));
        }
        let exponent = point - 1;
        out.push('e');
        out.push(if exponent >= 0 { '+' } else { '-' });
        out.push_str(&alloc::format!("{}", exponent.abs()));
        out
    }

    /// `Number.prototype.toString(radix)` for radix other than 10.
    pub fn to_radix(self, radix: u32) -> String {
        if !self.is_finite() || radix == 10 {
            return self.to_js_string();
        }
        let mut out = String::new();
        let negative = self.is_sign_negative() && !self.is_zero();
        let value = self.abs();
        let integer = value.trunc();
        let mut fraction = value.sub(integer);
        let radix_num = Num::from_i64(radix as i64);
        // Integer part: exact digit by digit through the big integer.
        let p = integer.parts();
        let mut big = if integer.is_zero() {
            Big::from_u128(0)
        } else if p.e >= 0 {
            Big::from_u128(p.m as u128).shl(p.e as u32)
        } else {
            Big::from_u128((p.m >> -p.e) as u128)
        };
        let mut integer_digits: Vec<u8> = Vec::new();
        if big.is_zero() {
            integer_digits.push(b'0');
        }
        while !big.is_zero() {
            let (quotient, remainder) = big_divmod_small(&big, radix);
            integer_digits.push(DIGITS[remainder as usize]);
            big = quotient;
        }
        integer_digits.reverse();
        if negative {
            out.push('-');
        }
        out.push_str(core::str::from_utf8(&integer_digits).unwrap_or("0"));
        if !fraction.is_zero() {
            out.push('.');
            for _ in 0..52 {
                fraction = fraction.mul(radix_num);
                let digit = fraction.trunc();
                out.push(DIGITS[digit.to_i64_saturating() as usize % 36] as char);
                fraction = fraction.sub(digit);
                if fraction.is_zero() {
                    break;
                }
            }
        }
        out
    }

    /// The double nearest to DIGITS * 10^exponent (ASCII digits, maybe
    /// with leading zeros).
    pub fn from_decimal(digits: &[u8], exponent: i32) -> Num {
        let start = digits.iter().position(|d| *d != b'0').unwrap_or(digits.len());
        let mut digits = &digits[start..];
        let mut exponent = exponent;
        if digits.is_empty() {
            return Self::ZERO;
        }
        // Beyond 800 digits nothing changes the nearest double but the
        // fact that something is there: keep a nonzero digit as a
        // stand-in.
        let mut long: Vec<u8>;
        if digits.len() > 800 {
            let dropped = digits.len() - 800;
            long = digits[..800].to_vec();
            if digits[800..].iter().any(|d| *d != b'0') {
                long.push(b'1');
                exponent += dropped as i32 - 1;
            } else {
                exponent += dropped as i32;
            }
            digits = &long;
        }
        let magnitude = digits.len() as i32 + exponent;
        if magnitude > 310 {
            return Self::INFINITY;
        }
        if magnitude < -330 {
            return Self::ZERO;
        }
        // Fast path: an exact integer times or over an exact power of ten
        // is one correctly rounded operation.
        if digits.len() <= 15 && (-22..=22).contains(&exponent) {
            let mut value = 0u64;
            for digit in digits {
                value = value * 10 + (digit - b'0') as u64;
            }
            let mantissa = Self::from_u64(value);
            let power = Self::from_u64(10u64.pow(exponent.unsigned_abs().min(19)));
            let power = if exponent.unsigned_abs() > 19 {
                power.mul(Self::from_u64(10u64.pow(exponent.unsigned_abs() - 19)))
            } else {
                power
            };
            return if exponent >= 0 { mantissa.mul(power) } else { mantissa.div(power) };
        }
        let mut big = Big::from_decimal(digits);
        if exponent >= 0 {
            big.mul_pow10(exponent as u32);
            let (top, sticky) = big.top_bits(100);
            let shift = big.bits().saturating_sub(100) as i32;
            return Self::pack(false, shift - 1, (top << 1) | sticky as u128);
        }
        // value = big / 10^k: the numerator or the denominator is shifted
        // so that the quotient has 70-odd bits; it is found one bit at a
        // time, and the remainder becomes the sticky bit.
        let mut denominator = Big::from_u128(1);
        denominator.mul_pow10((-exponent) as u32);
        let shift = denominator.bits() as i32 - big.bits() as i32 + 70;
        let mut remainder = if shift > 0 { big.shl(shift as u32) } else { big };
        if shift < 0 {
            denominator = denominator.shl((-shift) as u32);
        }
        let mut quotient: u128 = 0;
        let top = remainder.bits().saturating_sub(denominator.bits()) + 1;
        for bit in (0..=top.min(120)).rev() {
            let shifted = denominator.shl(bit);
            if remainder.compare(&shifted) != Ordering::Less {
                remainder.sub_assign(&shifted);
                quotient |= 1u128 << bit;
            }
        }
        let sticky = !remainder.is_zero();
        Self::pack(false, -shift - 1, (quotient << 1) | sticky as u128)
    }

    /// ECMAScript `StringToNumber`: the whole text, trimmed, must be a
    /// number; the empty text is 0; anything else is NaN.
    pub fn parse(text: &str) -> Num {
        let text = trim_js(text);
        if text.is_empty() {
            return Self::ZERO;
        }
        // Numeric separators are source text: `Number("1_0")` is NaN.
        if text.contains('_') {
            return Self::NAN;
        }
        let lower = text.as_bytes();
        if lower.len() > 2 && lower[0] == b'0' {
            let radix = match lower[1] {
                b'x' | b'X' => 16,
                b'o' | b'O' => 8,
                b'b' | b'B' => 2,
                _ => 0,
            };
            if radix != 0 {
                return Self::parse_radix_digits(&text[2..], radix).unwrap_or(Self::NAN);
            }
        }
        match Self::parse_prefix(text) {
            Some((value, used)) if used == text.len() => value,
            _ => Self::NAN,
        }
    }

    /// Digits in a radix, all of them valid, none missing.
    fn parse_radix_digits(text: &str, radix: u32) -> Option<Num> {
        if text.is_empty() {
            return None;
        }
        let mut value = Self::ZERO;
        let radix_num = Self::from_i64(radix as i64);
        for character in text.chars() {
            let digit = character.to_digit(radix)?;
            value = value.mul(radix_num).add(Self::from_i64(digit as i64));
        }
        Some(value)
    }

    /// The longest prefix that is a decimal number (with sign, point,
    /// exponent, or `Infinity`), and its length. `parseFloat` and
    /// `StringToNumber` share it.
    pub fn parse_prefix(text: &str) -> Option<(Num, usize)> {
        let bytes = text.as_bytes();
        let mut index = 0;
        let mut negative = false;
        if index < bytes.len() && (bytes[index] == b'+' || bytes[index] == b'-') {
            negative = bytes[index] == b'-';
            index += 1;
        }
        if text[index..].starts_with("Infinity") {
            let value = if negative { Self::NEG_INFINITY } else { Self::INFINITY };
            return Some((value, index + 8));
        }
        let mut digits: Vec<u8> = Vec::new();
        let mut exponent: i32 = 0;
        let mut seen = false;
        while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'_') {
            if bytes[index] != b'_' {
                digits.push(bytes[index]);
                seen = true;
            }
            index += 1;
        }
        if index < bytes.len() && bytes[index] == b'.' {
            let mark = index;
            index += 1;
            let mut fraction_seen = false;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                digits.push(bytes[index]);
                exponent -= 1;
                fraction_seen = true;
                index += 1;
            }
            if !seen && !fraction_seen {
                index = mark;
            }
            seen |= fraction_seen;
        }
        if !seen {
            return None;
        }
        if index < bytes.len() && (bytes[index] == b'e' || bytes[index] == b'E') {
            let mark = index;
            index += 1;
            let mut exponent_negative = false;
            if index < bytes.len() && (bytes[index] == b'+' || bytes[index] == b'-') {
                exponent_negative = bytes[index] == b'-';
                index += 1;
            }
            let start = index;
            let mut value: i32 = 0;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                value = value.saturating_mul(10).saturating_add((bytes[index] - b'0') as i32).min(100_000);
                index += 1;
            }
            if index == start {
                index = mark;
            } else {
                exponent = exponent.saturating_add(if exponent_negative { -value } else { value });
            }
        }
        let value = Self::from_decimal(&digits, exponent);
        Some((if negative { value.neg() } else { value }, index))
    }

    /// `parseInt(text, radix)`.
    pub fn parse_int(text: &str, radix: u32) -> Num {
        let text = trim_js(text);
        let (negative, text) = match text.as_bytes().first() {
            Some(b'-') => (true, &text[1..]),
            Some(b'+') => (false, &text[1..]),
            _ => (false, text),
        };
        let (radix, text) = if (radix == 0 || radix == 16) && (text.starts_with("0x") || text.starts_with("0X")) {
            (16, &text[2..])
        } else if radix == 0 {
            (10, text)
        } else {
            (radix, text)
        };
        if !(2..=36).contains(&radix) {
            return Self::NAN;
        }
        let end = text.char_indices().find(|(_, c)| c.to_digit(radix).is_none()).map_or(text.len(), |(i, _)| i);
        if end == 0 {
            return Self::NAN;
        }
        let value = if radix == 10 {
            Self::from_decimal(&text.as_bytes()[..end], 0)
        } else {
            Self::parse_radix_digits(&text[..end], radix).unwrap_or(Self::NAN)
        };
        if negative {
            value.neg()
        } else {
            value
        }
    }

    // --------------------------------------------------------
    //  Elementary functions
    // --------------------------------------------------------

    fn lit(value: i64) -> Num {
        Self::from_i64(value)
    }

    /// e^x, correctly rounded but in vanishingly rare cases: x = k ln2 + r
    /// with r in [0, ln2), both computed in 116-bit fixed point (`wide`),
    /// then the series of e^r, then one rounding to a double. A series in
    /// doubles would round at every term: `Math.exp(1)` must be `Math.E`.
    pub fn exp(self) -> Num {
        if self.is_nan() {
            return self;
        }
        if self.is_infinite() {
            return if self.is_sign_negative() { Self::ZERO } else { self };
        }
        if self.partial_cmp(Self::lit(710)) == Some(Ordering::Greater) {
            return Self::INFINITY;
        }
        if self.lt(Self::lit(-746)) {
            return Self::ZERO;
        }
        if self.is_zero() || self.abs().lt(Self::ONE.scale2(-60)) {
            // e^x = 1 + x + x^2/2...: past 2^-60 the square cannot move
            // the rounding of 1 + x.
            return Self::ONE.add(self);
        }
        Self::exp_wide(wide::from_num(self))
    }

    /// e^x of a fixed-point x (FRAC fractional bits), rounded once.
    fn exp_wide(x: i128) -> Num {
        let k = x.div_euclid(wide::LN2 as i128);
        let r = (x - k * wide::LN2 as i128) as u128;
        let sum = wide::exp_reduced(r);
        Self::pack(false, (k as i32).saturating_sub(wide::FRAC as i32), sum)
    }

    /// x * 2^n.
    pub fn scale2(self, n: i32) -> Num {
        if !self.is_finite() || self.is_zero() {
            return self;
        }
        let p = self.parts();
        Self::pack(p.negative, p.e.saturating_add(n.clamp(-3000, 3000)), p.m as u128)
    }

    /// ln x: x = m * 2^k with m in [sqrt(1/2), sqrt(2)), then the series
    /// of atanh on (m - 1) / (m + 1) -- in 116-bit fixed point, rounded
    /// once at the end, as `exp`.
    pub fn ln(self) -> Num {
        if self.is_nan() || (self.is_sign_negative() && !self.is_zero()) {
            return Self::NAN;
        }
        if self.is_zero() {
            return Self::NEG_INFINITY;
        }
        if self.is_infinite() {
            return self;
        }
        if self.equals(Self::ONE) {
            return Self::ZERO;
        }
        let total = self.ln_wide();
        Self::pack(total < 0, -(wide::FRAC as i32), total.unsigned_abs())
    }

    /// ln of a positive finite number in fixed point (FRAC bits).
    fn ln_wide(self) -> i128 {
        let p = Self::normalized(self.parts());
        // value = (m / 2^52) * 2^(e + 52), m / 2^52 in [1, 2).
        let mut k = p.e as i128 + 52;
        let mut m = (p.m as u128) << (wide::FRAC - 52);
        // sqrt(2) in the same fixed point: past it, halve.
        if m > wide::SQRT2 {
            m >>= 1;
            k += 1;
        }
        k * wide::LN2 as i128 + wide::ln_reduced(m)
    }

    /// log2 x, with ln x divided by ln 2 in fixed point before the one
    /// rounding: an exact power of two gives its exponent exactly.
    pub fn log2(self) -> Num {
        self.log_with(wide::LN2)
    }

    /// log10 by fdlibm's formula (glibc's and V8's): x = 2^k m, then
    /// k log10(2) in two parts plus log(m) / ln(10). Not always correctly
    /// rounded, but what CPython and Node print; exact for powers of ten.
    pub fn log10(self) -> Num {
        const IVLN10: Num = Num(0x3fdb_cb7b_1526_e50e);
        const LOG10_2HI: Num = Num(0x3fd3_4413_509f_6000);
        const LOG10_2LO: Num = Num(0x3d59_fef3_11f1_2b36);
        if self.is_nan() || (self.is_sign_negative() && !self.is_zero()) {
            return Self::NAN;
        }
        if self.is_zero() {
            return Self::NEG_INFINITY;
        }
        if self.is_infinite() {
            return self;
        }
        let mut x = self;
        let mut k: i32 = 0;
        let mut high = (x.0 >> 32) as u32;
        if high < 0x0010_0000 {
            // Subnormal: scaled up first.
            k -= 54;
            x = x.mul(Self::ONE.scale2(54));
            high = (x.0 >> 32) as u32;
        }
        k += (high >> 20) as i32 - 1023;
        let i = ((k as u32) & 0x8000_0000) >> 31;
        let high = (high & 0x000f_ffff) | ((0x3ff - i) << 20);
        let y = Self::from_i64((k + i as i32) as i64);
        let m = Num((x.0 & 0xffff_ffff) | ((high as u64) << 32));
        let z = y.mul(LOG10_2LO).add(IVLN10.mul(m.ln()));
        z.add(y.mul(LOG10_2HI))
    }

    fn log_with(self, divisor: u128) -> Num {
        if self.is_nan() || (self.is_sign_negative() && !self.is_zero()) {
            return Self::NAN;
        }
        if self.is_zero() {
            return Self::NEG_INFINITY;
        }
        if self.is_infinite() {
            return self;
        }
        if self.equals(Self::ONE) {
            return Self::ZERO;
        }
        let total = self.ln_wide();
        let magnitude = total.unsigned_abs();
        let value = ((magnitude / divisor) << wide::FRAC) | wide::div(magnitude % divisor, divisor);
        Self::pack(total < 0, -(wide::FRAC as i32), value)
    }

    /// x^y, correctly rounded but in vanishingly rare cases: y ln x in
    /// 116-bit fixed point (ln as `ln`, the product exact), then e^ as
    /// `exp`, one rounding at the end. Repeated multiplication would
    /// round at each step: `2 ** 0.5` must be `Math.SQRT2`.
    pub fn pow(self, exponent: Num) -> Num {
        if exponent.is_nan() {
            return Self::NAN;
        }
        if exponent.is_zero() {
            return Self::ONE;
        }
        if self.is_nan() {
            return Self::NAN;
        }
        let integer = exponent.is_integer() && exponent.is_finite();
        let odd = integer && exponent.abs().lt(Self::lit(1 << 53)) && exponent.to_i64_saturating() & 1 == 1;
        // The exactly rounded cases.
        if exponent.equals(Self::ONE) {
            return self;
        }
        if exponent.equals(Self::TWO) {
            return self.mul(self);
        }
        if exponent.equals(Self::ONE.neg()) {
            return Self::ONE.div(self);
        }
        if self.abs().equals(Self::ONE) && exponent.is_infinite() {
            return Self::NAN;
        }
        if exponent.is_infinite() {
            let grows = Self::ONE.lt(self.abs()) == !exponent.is_sign_negative();
            return if grows { Self::INFINITY } else { Self::ZERO };
        }
        if self.is_sign_negative() && !self.is_zero() && !integer {
            return Self::NAN;
        }
        let negative = self.is_sign_negative() && odd;
        let sign = |v: Num| if negative { v.neg() } else { v };
        if self.is_zero() || self.is_infinite() {
            // 0^y and inf^y: 0 or infinity, the sign of an odd power kept.
            let large = self.is_infinite() != exponent.is_sign_negative();
            return sign(if large { Self::INFINITY } else { Self::ZERO });
        }
        let base = self.abs();
        if base.equals(Self::ONE) {
            return sign(Self::ONE);
        }
        // Far beyond the range: the double estimate decides.
        let estimate = exponent.mul(base.ln());
        if Self::lit(1100).lt(estimate.abs()) {
            return sign(if estimate.is_sign_negative() { Self::ZERO } else { Self::INFINITY });
        }
        let l = base.ln_wide();
        let (y_negative, m, e) = exponent.decompose();
        // |l| * m exactly (192 bits), then scaled by 2^e.
        let magnitude = l.unsigned_abs();
        let low = (magnitude & u64::MAX as u128) * m as u128;
        let high = (magnitude >> 64) * m as u128 + (low >> 64);
        let low = low as u64;
        // The estimate bounds the result under 2^127: the shifts below
        // lose no bit.
        let product: u128 = if e >= 0 {
            let full = (high << 64) | low as u128;
            if high >> 63 != 0 || e as u32 >= full.leading_zeros() {
                return sign(if (l < 0) != y_negative { Self::ZERO } else { Self::INFINITY });
            }
            full << e as u32
        } else {
            let shift = (-e) as u32;
            if shift >= 192 {
                0
            } else if shift >= 64 {
                high >> (shift - 64)
            } else {
                (high << (64 - shift)) | ((low as u128) >> shift)
            }
        };
        if product >> 126 != 0 {
            return sign(if (l < 0) != y_negative { Self::ZERO } else { Self::INFINITY });
        }
        let total = if (l < 0) != y_negative { -(product as i128) } else { product as i128 };
        sign(Self::exp_wide(total))
    }

    // --------------------------------------------------------
    //  Trigonometry, in fixed point
    // --------------------------------------------------------

    /// |x| modulo pi/2 for |x| >= pi/4: the quadrant (mod 4) and the
    /// remainder in fixed point, in [-pi/4, pi/4]. Payne-Hanek: the
    /// window of 2/pi's bits that matters for x's exponent, times x's
    /// significand, exactly; 1e300 reduces as well as 3.
    fn reduce_pio2(self) -> (u32, i128) {
        let (_, m, e) = self.abs().decompose();
        // x * 2/pi = m * 2^e * T; bits i <= e - 2 of T only add multiples
        // of 4, bits past e + 185 less than 2^-128.
        let last = e + 185;
        let first = (e - 1).max(1);
        let mut window = [0u64; 4];
        for i in first..=last {
            let index = (i - 1) as usize;
            let bit = (wide::TWO_OVER_PI[index / 64] >> (63 - index % 64)) & 1;
            let mut carry = bit;
            for limb in window.iter_mut() {
                let next = *limb >> 63;
                *limb = (*limb << 1) | carry;
                carry = next;
            }
        }
        // m * window: five limbs.
        let mut product = [0u64; 5];
        let mut carry = 0u128;
        for (i, limb) in window.iter().enumerate() {
            let t = *limb as u128 * m as u128 + carry;
            product[i] = t as u64;
            carry = t >> 64;
        }
        product[4] = carry as u64;
        // Bits 61..187: x * 2/pi mod 4 with 124 fractional bits.
        let mut v: u128 = 0;
        for bit in (61..187).rev() {
            v = (v << 1) | ((product[bit / 64] >> (bit % 64)) & 1) as u128;
        }
        let mut quadrant = (v >> 124) as u32 & 3;
        let mut f = (v & ((1u128 << 124) - 1)) as i128;
        if f >= 1i128 << 123 {
            quadrant = (quadrant + 1) & 3;
            f -= 1i128 << 124;
        }
        // To 116 bits, times pi/2.
        let magnitude = f.unsigned_abs() >> 8;
        let r = wide::mul(magnitude, wide::PIO2) as i128;
        (quadrant, if f < 0 { -r } else { r })
    }

    /// sin r and cos r for r in [0, pi/4], fixed point: their series.
    fn sin_cos_wide(r: u128) -> (u128, u128) {
        let r2 = wide::mul(r, r);
        let mut sin = r as i128;
        let mut term = r;
        let mut k = 1u128;
        loop {
            term = wide::mul(term, r2) / ((2 * k) * (2 * k + 1));
            if term == 0 {
                break;
            }
            sin += if k % 2 == 1 { -(term as i128) } else { term as i128 };
            k += 1;
        }
        let mut cos = wide::ONE as i128;
        let mut term = wide::ONE;
        let mut k = 1u128;
        loop {
            term = wide::mul(term, r2) / ((2 * k - 1) * (2 * k));
            if term == 0 {
                break;
            }
            cos += if k % 2 == 1 { -(term as i128) } else { term as i128 };
            k += 1;
        }
        (sin.max(0) as u128, cos.max(0) as u128)
    }

    /// sin x and cos x as signed fixed-point values.
    fn sin_cos_fixed(self) -> (i128, i128) {
        let a = self.abs();
        let (quadrant, r) = if a.lt(Self::lit(785).div(Self::lit(1000))) {
            let (_, m, e) = a.decompose();
            (0, ((m as u128) << (e + wide::FRAC as i32) as u32) as i128)
        } else {
            a.reduce_pio2()
        };
        let (s, c) = Self::sin_cos_wide(r.unsigned_abs());
        let s = if r < 0 { -(s as i128) } else { s as i128 };
        let c = c as i128;
        let (sin, cos) = match quadrant {
            0 => (s, c),
            1 => (c, -s),
            2 => (-s, -c),
            _ => (-c, s),
        };
        (if self.is_sign_negative() { -sin } else { sin }, cos)
    }

    fn from_wide(value: i128) -> Num {
        Self::pack(value < 0, -(wide::FRAC as i32), value.unsigned_abs())
    }

    pub fn sin(self) -> Num {
        if !self.is_finite() {
            return Self::NAN;
        }
        // sin x = x - x^3/6...: below 2^-27 the cube cannot move x.
        if self.abs().lt(Self::ONE.scale2(-27)) {
            return self;
        }
        Self::from_wide(self.sin_cos_fixed().0)
    }

    pub fn cos(self) -> Num {
        if !self.is_finite() {
            return Self::NAN;
        }
        if self.abs().lt(Self::ONE.scale2(-27)) {
            return Self::ONE;
        }
        Self::from_wide(self.sin_cos_fixed().1)
    }

    pub fn tan(self) -> Num {
        if !self.is_finite() {
            return Self::NAN;
        }
        if self.abs().lt(Self::ONE.scale2(-27)) {
            return self;
        }
        let (s, c) = self.sin_cos_fixed();
        if c == 0 {
            return if s < 0 { Self::NEG_INFINITY } else { Self::INFINITY };
        }
        let (q, exponent) = wide::ratio(s.unsigned_abs(), c.unsigned_abs());
        Self::pack((s < 0) != (c < 0), exponent, q)
    }

    /// A positive finite double in fixed point (truncated below 2^-116).
    fn to_wide(self) -> u128 {
        let (_, m, e) = self.decompose();
        let shift = e + wide::FRAC as i32;
        if shift >= 0 { (m as u128) << shift as u32 } else if shift > -64 { (m >> (-shift) as u32) as u128 } else { 0 }
    }

    /// atan of t in [0, 1], fixed point: t = c + d with c the nearest
    /// eighth, atan t = atan c + atan((t - c) / (1 + t c)), the second by
    /// its series (|argument| <= 1/16).
    fn atan_wide(t: u128) -> i128 {
        let k = ((t + (wide::ONE >> 4)) >> (wide::FRAC - 3)).min(8);
        let c = k << (wide::FRAC - 3);
        let den = wide::ONE + wide::mul(t, c);
        let (num, negative) = if t >= c { (t - c, false) } else { (c - t, true) };
        let u = wide::div(num, den);
        let u2 = wide::mul(u, u);
        let mut power = u;
        let mut sum = u as i128;
        let mut n = 1u128;
        loop {
            power = wide::mul(power, u2);
            if power == 0 {
                break;
            }
            let term = (power / (2 * n + 1)) as i128;
            sum += if n % 2 == 1 { -term } else { term };
            n += 1;
        }
        wide::ATAN_EIGHTHS[k as usize] as i128 + if negative { -sum } else { sum }
    }

    /// The angle of (x, y) for x, y >= 0 in fixed point, in [0, pi/2].
    fn angle_wide(x: u128, y: u128) -> i128 {
        if y == x {
            return wide::ATAN_EIGHTHS[8] as i128;
        }
        if y < x {
            Self::atan_wide(wide::div(y, x))
        } else {
            wide::PIO2 as i128 - Self::atan_wide(wide::div(x, y))
        }
    }

    pub fn atan(self) -> Num {
        if self.is_nan() || self.is_zero() {
            return self;
        }
        let a = self.abs();
        if a.lt(Self::ONE.scale2(-27)) {
            return self;
        }
        let angle = if a.lt(Self::ONE.scale2(60)) {
            if !Self::ONE.lt(a) {
                Self::atan_wide(a.to_wide())
            } else {
                // 1/a in fixed point, then pi/2 - atan(1/a).
                let (_, m, e) = a.decompose();
                let (q, exponent) = wide::ratio(1, m as u128);
                let shift = exponent - e + wide::FRAC as i32;
                let inverse = if shift >= 0 { q << shift as u32 } else { q >> (-shift) as u32 };
                wide::PIO2 as i128 - Self::atan_wide(inverse)
            }
        } else {
            wide::PIO2 as i128
        };
        let r = Self::from_wide(angle);
        if self.is_sign_negative() { r.neg() } else { r }
    }

    pub fn atan2(self, x: Num) -> Num {
        let y = self;
        if y.is_nan() || x.is_nan() {
            return Self::NAN;
        }
        let half_pi = Self::from_wide(wide::PIO2 as i128);
        let pi = Self::from_wide(wide::PI as i128);
        let signed = |v: Num| if y.is_sign_negative() { v.neg() } else { v };
        if y.is_zero() {
            return if x.is_sign_negative() { signed(pi) } else { y };
        }
        if x.is_zero() {
            return signed(half_pi);
        }
        if x.is_infinite() || y.is_infinite() {
            let angle = match (x.is_infinite(), y.is_infinite(), x.is_sign_negative()) {
                (true, true, false) => Self::from_wide(wide::PIO2 as i128 / 2),
                (true, true, true) => Self::from_wide(3 * (wide::PIO2 as i128) / 2),
                (true, false, false) => Self::ZERO,
                (true, false, true) => pi,
                _ => half_pi,
            };
            return signed(angle);
        }
        let (ay, ax) = (y.abs(), x.abs());
        // The ratio of the two, from their significands.
        let (_, my, ey) = ay.decompose();
        let (_, mx, ex) = ax.decompose();
        let (q, exponent) = wide::ratio(my as u128, mx as u128);
        let scale = exponent + ey - ex;
        // y/x tiny: the angle is y/x (or pi minus it).
        let bits = 128 - q.leading_zeros() as i32 + scale;
        if bits < -27 {
            return if x.is_sign_negative() { signed(pi) } else { y.div(x) };
        }
        let angle = if bits <= 0 {
            let shift = scale + wide::FRAC as i32;
            let t = if shift >= 0 { q << shift as u32 } else { q >> (-shift) as u32 };
            Self::atan_wide(t.min(wide::ONE))
        } else {
            let (q2, exponent2) = wide::ratio(mx as u128, my as u128);
            let shift = exponent2 + ex - ey + wide::FRAC as i32;
            let t = if shift >= 0 { q2 << shift.min(127) as u32 } else { q2 >> (-shift).min(127) as u32 };
            wide::PIO2 as i128 - Self::atan_wide(t.min(wide::ONE))
        };
        let angle = if x.is_sign_negative() { wide::PI as i128 - angle } else { angle };
        signed(Self::from_wide(angle))
    }

    /// sqrt of a fixed-point value in [0, 1]: Newton from the double.
    fn sqrt_wide(s: u128) -> u128 {
        if s == 0 {
            return 0;
        }
        let estimate = Self::pack(false, -(wide::FRAC as i32), s).sqrt();
        let mut y = estimate.to_wide().max(1);
        for _ in 0..3 {
            if s >= y {
                break;
            }
            y = (y + wide::div(s, y)) / 2;
        }
        y
    }

    pub fn asin(self) -> Num {
        if self.abs().partial_cmp(Self::ONE) == Some(Ordering::Greater) || self.is_nan() {
            return Self::NAN;
        }
        if self.abs().lt(Self::ONE.scale2(-27)) {
            return self;
        }
        let a = self.abs().to_wide();
        let c = Self::sqrt_wide(wide::ONE - wide::mul(a, a).min(wide::ONE));
        let r = Self::from_wide(Self::angle_wide(c, a));
        if self.is_sign_negative() { r.neg() } else { r }
    }

    pub fn acos(self) -> Num {
        if self.abs().partial_cmp(Self::ONE) == Some(Ordering::Greater) || self.is_nan() {
            return Self::NAN;
        }
        let a = self.abs().to_wide();
        let s = Self::sqrt_wide(wide::ONE - wide::mul(a, a).min(wide::ONE));
        // acos |x| = angle of (|x|, sqrt(1 - x^2)); acos(-x) = pi - acos x.
        let angle = Self::angle_wide(a, s);
        Self::from_wide(if self.is_sign_negative() { wide::PI as i128 - angle } else { angle })
    }

    pub fn cbrt(self) -> Num {
        if !self.is_finite() || self.is_zero() {
            return self;
        }
        let root = self.abs().ln().div(Self::lit(3)).exp();
        // One Newton step polishes the last bits.
        let root = root.sub(root.mul(root).mul(root).sub(self.abs()).div(Self::lit(3).mul(root).mul(root)));
        if self.is_sign_negative() {
            root.neg()
        } else {
            root
        }
    }
}

const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

fn big_divmod_small(big: &Big, divisor: u32) -> (Big, u32) {
    big.divmod_small(divisor)
}

/// Integer square root (floor).
fn isqrt_u128(value: u128) -> u128 {
    if value < 2 {
        return value;
    }
    let mut x = 1u128 << (128 - value.leading_zeros()).div_ceil(2);
    loop {
        let y = (x + value / x) / 2;
        if y >= x {
            return x;
        }
        x = y;
    }
}

/// ECMAScript white space and line terminators, trimmed.
pub fn trim_js(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}


/// Fixed-point arithmetic for `exp` and `ln`: unsigned values with
/// `FRAC` fraction bits in a `u128`, products through a 256-bit
/// intermediate. 116 bits leave 63 below the 53 of a double: the series'
/// truncation errors stay far under the last bit.
mod wide {
    use super::Num;

    pub const FRAC: u32 = 116;
    pub const ONE: u128 = 1 << FRAC;
    /// floor(ln 2 * 2^116).
    pub const LN2: u128 = 0xb17217f7d1cf79abc9e3b39803f2f;
    /// sqrt(2) * 2^116, rounded up (only compared against).
    pub const SQRT2: u128 = 0x16a09e667f3bcc908b2fb1366ea958;
    /// floor(pi/2 * 2^116) and floor(pi * 2^116).
    pub const PIO2: u128 = 0x1921fb54442d18469898cc51701b83;
    pub const PI: u128 = 0x3243f6a8885a308d313198a2e03707;
    /// atan(k/8) for k = 0..8, floor(* 2^116).
    pub const ATAN_EIGHTHS: [u128; 9] = [
        0x0,
        0x1fd5ba9aac2f6dc65912f313e7d11,
        0x3eb6ebf25901bac55b71e7bd7de88,
        0x5bd86507937bc239c55190916e7f2,
        0x76b19c1586ed3da2b7f222f65e1d4,
        0x8f005d5ef7f59f9b5c835e1665c43,
        0xa4bc7d1934f7092419a87f2a457da,
        0xb8053e2bc2319e73cb2da55210a44,
        0xc90fdaa22168c234c4c6628b80dc1,
    ];
    /// The first 1280 bits of 2/pi (all fractional), for `reduce_pio2`.
    pub const TWO_OVER_PI: [u64; 20] = [
        0xa2f9836e4e441529, 0xfc2757d1f534ddc0, 0xdb6295993c439041, 0xfe5163abdebbc561, 0xb7246e3a424dd2e0,
        0x06492eea09d1921c, 0xfe1deb1cb129a73e, 0xe88235f52ebb4484, 0xe99c7026b45f7e41, 0x3991d639835339f4,
        0x9c845f8bbdf9283b, 0x1ff897ffde05980f, 0xef2f118b5a0a6d1f, 0x6d367ecf27cb09b7, 0x4f463f669e5fea2d,
        0x7527bac7ebe5f17b, 0x3d0739f78a5292ea, 0x6bfb5fb11f8d5d08, 0x56033046fc7b6bab, 0xf0cfbc209af4361d,
    ];

    /// num / den for any non-zero num and den: q * 2^exponent with q of
    /// 125 bits, its lowest bit sticky.
    pub fn ratio(num: u128, den: u128) -> (u128, i32) {
        let nz = num.leading_zeros() as i32 - 1;
        let dz = den.leading_zeros() as i32 - 1;
        let n = if nz >= 0 { num << nz as u32 } else { num >> 1 };
        let d = if dz >= 0 { den << dz as u32 } else { den >> 1 };
        let mut remainder = n;
        let mut q = 0u128;
        for _ in 0..125 {
            q <<= 1;
            if remainder >= d {
                remainder -= d;
                q |= 1;
            }
            remainder <<= 1;
        }
        if remainder != 0 {
            q |= 1;
        }
        (q, dz - nz - 124)
    }

    /// (a * b) >> FRAC, for a, b < 2^118.
    pub fn mul(a: u128, b: u128) -> u128 {
        let (a1, a0) = (a >> 64, a & u64::MAX as u128);
        let (b1, b0) = (b >> 64, b & u64::MAX as u128);
        let p00 = a0 * b0;
        let mid = a0 * b1 + a1 * b0;
        let p11 = a1 * b1;
        let (low, carry) = p00.overflowing_add(mid << 64);
        let high = p11 + (mid >> 64) + carry as u128;
        (high << (128 - FRAC)) | (low >> FRAC)
    }

    /// floor(a / b * 2^FRAC), for a < b < 2^118: long division.
    pub fn div(a: u128, b: u128) -> u128 {
        let mut remainder = a;
        let mut quotient = 0u128;
        for _ in 0..FRAC {
            remainder <<= 1;
            quotient <<= 1;
            if remainder >= b {
                remainder -= b;
                quotient |= 1;
            }
        }
        quotient
    }

    /// A finite double, |x| < 2^10, as signed fixed point.
    pub fn from_num(x: Num) -> i128 {
        let p = Num::normalized(x.parts());
        let shift = p.e + FRAC as i32;
        let magnitude = if shift >= 0 { (p.m as i128) << shift } else { (p.m as i128) >> (-shift) };
        if p.negative { -magnitude } else { magnitude }
    }

    /// e^r for r in [0, ln 2): in [1, 2) * 2^FRAC.
    pub fn exp_reduced(r: u128) -> u128 {
        let mut sum = ONE;
        let mut term = ONE;
        for n in 1..64u128 {
            term = mul(term, r) / n;
            if term == 0 {
                break;
            }
            sum += term;
        }
        sum
    }

    /// ln m for m in [sqrt(1/2), sqrt(2)] * 2^FRAC, signed.
    pub fn ln_reduced(m: u128) -> i128 {
        let negative = m < ONE;
        let numerator = if negative { ONE - m } else { m - ONE };
        let s = div(numerator, m + ONE);
        let s2 = mul(s, s);
        let mut power = s;
        let mut sum = 0u128;
        for n in 0..80u128 {
            let term = power / (2 * n + 1);
            if term == 0 {
                break;
            }
            sum += term;
            power = mul(power, s2);
        }
        let value = (sum << 1) as i128;
        if negative { -value } else { value }
    }
}

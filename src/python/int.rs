//! Python's `int`: unbounded, exact, floor-dividing.
//!
//! Two representations behind one type: an `i64` while the value fits,
//! a sign and 32-bit limbs beyond. Arithmetic on small values is the
//! machine's, checked; an overflow moves to the big form, and a big
//! result that fits again comes back to the small one, so `==` between
//! two equal ints never has to compare different representations.
//!
//! The semantics are Python's, not the machine's: `//` and `%` round
//! towards minus infinity (`-7 // 2 == -4`, `-7 % 2 == 1`), shifts and
//! bitwise operators act on an infinite two's complement (`~5 == -6`,
//! `-1 >> 10 == -1`), and `hash` is the value modulo the Mersenne prime
//! 2^61 - 1, which is what makes `hash(1) == hash(1.0)`.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;

use crate::web::js::num::Num;

/// A magnitude: little-endian limbs, no trailing zero limb.
type Mag = Vec<u32>;

#[derive(Clone, Debug)]
pub enum Int {
    Small(i64),
    Big(Rc<BigInt>),
}

/// Sign and magnitude; never a value that fits an `i64` (see `Int`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BigInt {
    pub negative: bool,
    pub mag: Mag,
}

/// Largest number of decimal digits an int may print or parse (CPython
/// 3.11+ refuses more than 4300 by default: the conversion is quadratic).
pub const MAX_STR_DIGITS: usize = 4300;
/// Largest number of limbs a result may have: 2^20 bits.
pub const MAX_LIMBS: usize = 1 << 15;

fn trim(mag: &mut Mag) {
    while mag.last() == Some(&0) {
        mag.pop();
    }
}

fn mag_from_u128(mut value: u128) -> Mag {
    let mut mag = Vec::new();
    while value != 0 {
        mag.push(value as u32);
        value >>= 32;
    }
    mag
}

fn mag_cmp(a: &[u32], b: &[u32]) -> Ordering {
    if a.len() != b.len() {
        return a.len().cmp(&b.len());
    }
    for (x, y) in a.iter().rev().zip(b.iter().rev()) {
        if x != y {
            return x.cmp(y);
        }
    }
    Ordering::Equal
}

fn mag_add(a: &[u32], b: &[u32]) -> Mag {
    let (long, short) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let mut out = Vec::with_capacity(long.len() + 1);
    let mut carry = 0u64;
    for (i, limb) in long.iter().enumerate() {
        let sum = *limb as u64 + *short.get(i).unwrap_or(&0) as u64 + carry;
        out.push(sum as u32);
        carry = sum >> 32;
    }
    if carry != 0 {
        out.push(carry as u32);
    }
    out
}

/// `a - b`, with `a >= b`.
fn mag_sub(a: &[u32], b: &[u32]) -> Mag {
    let mut out = Vec::with_capacity(a.len());
    let mut borrow = 0i64;
    for (i, limb) in a.iter().enumerate() {
        let mut diff = *limb as i64 - *b.get(i).unwrap_or(&0) as i64 - borrow;
        if diff < 0 {
            diff += 1 << 32;
            borrow = 1;
        } else {
            borrow = 0;
        }
        out.push(diff as u32);
    }
    trim(&mut out);
    out
}

fn mag_mul(a: &[u32], b: &[u32]) -> Mag {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut out = alloc::vec![0u32; a.len() + b.len()];
    for (i, x) in a.iter().enumerate() {
        let mut carry = 0u64;
        for (j, y) in b.iter().enumerate() {
            let t = *x as u64 * *y as u64 + out[i + j] as u64 + carry;
            out[i + j] = t as u32;
            carry = t >> 32;
        }
        let mut k = i + b.len();
        while carry != 0 {
            let t = out[k] as u64 + carry;
            out[k] = t as u32;
            carry = t >> 32;
            k += 1;
        }
    }
    trim(&mut out);
    out
}

/// Divides by a single limb: quotient and remainder.
fn mag_divmod_small(a: &[u32], divisor: u32) -> (Mag, u32) {
    let mut out = alloc::vec![0u32; a.len()];
    let mut rest = 0u64;
    for i in (0..a.len()).rev() {
        let cur = (rest << 32) | a[i] as u64;
        out[i] = (cur / divisor as u64) as u32;
        rest = cur % divisor as u64;
    }
    trim(&mut out);
    (out, rest as u32)
}

fn mag_shl(a: &[u32], shift: usize) -> Mag {
    if a.is_empty() {
        return Vec::new();
    }
    let limbs = shift / 32;
    let bits = shift % 32;
    let mut out = alloc::vec![0u32; limbs];
    if bits == 0 {
        out.extend_from_slice(a);
    } else {
        let mut carry = 0u32;
        for limb in a {
            out.push((limb << bits) | carry);
            carry = limb >> (32 - bits);
        }
        if carry != 0 {
            out.push(carry);
        }
    }
    out
}

fn mag_shr(a: &[u32], shift: usize) -> Mag {
    let limbs = shift / 32;
    if limbs >= a.len() {
        return Vec::new();
    }
    let bits = shift % 32;
    let mut out: Mag = a[limbs..].to_vec();
    if bits != 0 {
        for i in 0..out.len() {
            let high = if i + 1 < out.len() { out[i + 1] << (32 - bits) } else { 0 };
            out[i] = (out[i] >> bits) | high;
        }
    }
    trim(&mut out);
    out
}

/// Knuth's algorithm D: quotient and remainder of magnitudes, `b` not
/// zero.
fn mag_divmod(a: &[u32], b: &[u32]) -> (Mag, Mag) {
    if mag_cmp(a, b) == Ordering::Less {
        return (Vec::new(), a.to_vec());
    }
    if b.len() == 1 {
        let (q, r) = mag_divmod_small(a, b[0]);
        return (q, if r == 0 { Vec::new() } else { alloc::vec![r] });
    }
    // Normalise so that the divisor's top limb has its high bit set.
    let shift = b[b.len() - 1].leading_zeros() as usize;
    let v = mag_shl(b, shift);
    let mut u = mag_shl(a, shift);
    if u.len() == a.len() {
        u.push(0);
    }
    let n = v.len();
    let m = u.len() - n;
    let mut q = alloc::vec![0u32; m];
    let top = v[n - 1] as u64;
    let second = v[n - 2] as u64;
    for j in (0..m).rev() {
        let numerator = ((u[j + n] as u64) << 32) | u[j + n - 1] as u64;
        let mut qhat = numerator / top;
        let mut rhat = numerator % top;
        while qhat >= 1 << 32 || qhat * second > ((rhat << 32) | u[j + n - 2] as u64) {
            qhat -= 1;
            rhat += top;
            if rhat >= 1 << 32 {
                break;
            }
        }
        // u[j..j+n+1] -= qhat * v
        let mut borrow = 0i64;
        let mut carry = 0u64;
        for i in 0..n {
            let product = qhat * v[i] as u64 + carry;
            carry = product >> 32;
            let t = u[i + j] as i64 - borrow - (product & 0xffff_ffff) as i64;
            u[i + j] = t as u32;
            borrow = if t < 0 { 1 } else { 0 };
        }
        let t = u[j + n] as i64 - borrow - carry as i64;
        u[j + n] = t as u32;
        if t < 0 {
            // Added back: qhat was one too many.
            qhat -= 1;
            let mut carry = 0u64;
            for i in 0..n {
                let s = u[i + j] as u64 + v[i] as u64 + carry;
                u[i + j] = s as u32;
                carry = s >> 32;
            }
            u[j + n] = u[j + n].wrapping_add(carry as u32);
        }
        q[j] = qhat as u32;
    }
    trim(&mut q);
    u.truncate(n);
    trim(&mut u);
    (q, mag_shr(&u, shift))
}

impl BigInt {
    fn new(negative: bool, mut mag: Mag) -> BigInt {
        trim(&mut mag);
        let negative = negative && !mag.is_empty();
        BigInt { negative, mag }
    }
}

/// Builds the canonical `Int` of a sign and a magnitude.
fn make(negative: bool, mut mag: Mag) -> Int {
    trim(&mut mag);
    if mag.len() <= 2 {
        let mut value = 0u64;
        for (i, limb) in mag.iter().enumerate() {
            value |= (*limb as u64) << (32 * i);
        }
        if !negative && value <= i64::MAX as u64 {
            return Int::Small(value as i64);
        }
        if negative && value <= 1u64 << 63 {
            return Int::Small((value as i64).wrapping_neg());
        }
    }
    Int::Big(Rc::new(BigInt::new(negative, mag)))
}

impl Int {
    pub fn zero() -> Int {
        Int::Small(0)
    }

    pub fn from_i64(value: i64) -> Int {
        Int::Small(value)
    }

    pub fn from_u128(value: u128) -> Int {
        make(false, mag_from_u128(value))
    }

    pub fn from_i128(value: i128) -> Int {
        make(value < 0, mag_from_u128(value.unsigned_abs()))
    }

    /// Sign and magnitude, whatever the form.
    fn parts(&self) -> (bool, Mag) {
        match self {
            Int::Small(v) => (*v < 0, mag_from_u128(v.unsigned_abs() as u128)),
            Int::Big(b) => (b.negative, b.mag.clone()),
        }
    }

    pub fn to_i64(&self) -> Option<i64> {
        match self {
            Int::Small(v) => Some(*v),
            Int::Big(_) => None,
        }
    }

    pub fn is_zero(&self) -> bool {
        matches!(self, Int::Small(0))
    }

    pub fn is_negative(&self) -> bool {
        match self {
            Int::Small(v) => *v < 0,
            Int::Big(b) => b.negative,
        }
    }

    /// Limbs a result of these operands may need: the guard against
    /// `2 ** 10 ** 9` eating the heap.
    fn too_big(limbs: usize) -> bool {
        limbs > MAX_LIMBS
    }

    pub fn neg(&self) -> Int {
        match self {
            Int::Small(v) => match v.checked_neg() {
                Some(n) => Int::Small(n),
                None => make(false, mag_from_u128(1u128 << 63)),
            },
            Int::Big(b) => make(!b.negative, b.mag.clone()),
        }
    }

    pub fn abs(&self) -> Int {
        if self.is_negative() { self.neg() } else { self.clone() }
    }

    pub fn add(&self, other: &Int) -> Int {
        if let (Int::Small(a), Int::Small(b)) = (self, other) {
            if let Some(sum) = a.checked_add(*b) {
                return Int::Small(sum);
            }
        }
        let (an, am) = self.parts();
        let (bn, bm) = other.parts();
        if an == bn {
            return make(an, mag_add(&am, &bm));
        }
        match mag_cmp(&am, &bm) {
            Ordering::Equal => Int::zero(),
            Ordering::Greater => make(an, mag_sub(&am, &bm)),
            Ordering::Less => make(bn, mag_sub(&bm, &am)),
        }
    }

    pub fn sub(&self, other: &Int) -> Int {
        if let (Int::Small(a), Int::Small(b)) = (self, other) {
            if let Some(diff) = a.checked_sub(*b) {
                return Int::Small(diff);
            }
        }
        self.add(&other.neg())
    }

    pub fn mul(&self, other: &Int) -> Result<Int, &'static str> {
        if let (Int::Small(a), Int::Small(b)) = (self, other) {
            if let Some(product) = a.checked_mul(*b) {
                return Ok(Int::Small(product));
            }
        }
        let (an, am) = self.parts();
        let (bn, bm) = other.parts();
        if Self::too_big(am.len() + bm.len()) {
            return Err("entier trop grand");
        }
        Ok(make(an != bn, mag_mul(&am, &bm)))
    }

    /// Floor division and the matching remainder (sign of the divisor).
    /// None when dividing by zero.
    pub fn divmod(&self, other: &Int) -> Option<(Int, Int)> {
        if other.is_zero() {
            return None;
        }
        if let (Int::Small(a), Int::Small(b)) = (self, other) {
            if !(*a == i64::MIN && *b == -1) {
                let mut q = a / b;
                let mut r = a % b;
                if r != 0 && ((r < 0) != (*b < 0)) {
                    q -= 1;
                    r += b;
                }
                return Some((Int::Small(q), Int::Small(r)));
            }
        }
        let (an, am) = self.parts();
        let (bn, bm) = other.parts();
        let (q, r) = mag_divmod(&am, &bm);
        let mut quotient = make(an != bn, q);
        let mut remainder = make(an, r);
        // Truncated to floored: when the signs differ and something is
        // left, one more step down.
        if !remainder.is_zero() && remainder.is_negative() != other.is_negative() {
            quotient = quotient.sub(&Int::Small(1));
            remainder = remainder.add(other);
        }
        Some((quotient, remainder))
    }

    pub fn pow(&self, exponent: u64) -> Result<Int, &'static str> {
        // An estimate of the result's size before computing it.
        let bits = self.bit_length() as u64;
        if bits > 1 && bits.saturating_mul(exponent) > (MAX_LIMBS as u64) * 32 {
            return Err("entier trop grand");
        }
        let mut result = Int::Small(1);
        let mut base = self.clone();
        let mut e = exponent;
        while e > 0 {
            if e & 1 == 1 {
                result = result.mul(&base)?;
            }
            e >>= 1;
            if e > 0 {
                base = base.mul(&base)?;
            }
        }
        Ok(result)
    }

    /// `pow(self, exponent, modulus)`, exponent not negative.
    pub fn pow_mod(&self, exponent: &Int, modulus: &Int) -> Option<Int> {
        if modulus.is_zero() {
            return None;
        }
        let mut result = Int::Small(1).divmod(modulus)?.1;
        let mut base = self.divmod(modulus)?.1;
        let (_, mag) = exponent.parts();
        for limb in mag.iter() {
            let mut bits = *limb;
            for _ in 0..32 {
                if bits & 1 == 1 {
                    result = result.mul(&base).ok()?.divmod(modulus)?.1;
                }
                base = base.mul(&base).ok()?.divmod(modulus)?.1;
                bits >>= 1;
            }
        }
        Some(result)
    }

    pub fn cmp(&self, other: &Int) -> Ordering {
        if let (Int::Small(a), Int::Small(b)) = (self, other) {
            return a.cmp(b);
        }
        let (an, am) = self.parts();
        let (bn, bm) = other.parts();
        match (an, bn) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => mag_cmp(&am, &bm),
            (true, true) => mag_cmp(&bm, &am),
        }
    }

    pub fn bit_length(&self) -> u32 {
        match self {
            Int::Small(v) => 64 - v.unsigned_abs().leading_zeros(),
            Int::Big(b) => (b.mag.len() as u32 - 1) * 32 + (32 - b.mag[b.mag.len() - 1].leading_zeros()),
        }
    }

    // --------------------------------------------------------
    //  Bits, on an infinite two's complement
    // --------------------------------------------------------

    pub fn shl(&self, shift: u64) -> Result<Int, &'static str> {
        if self.is_zero() {
            return Ok(Int::zero());
        }
        if (self.bit_length() as u64 + shift) / 32 > MAX_LIMBS as u64 {
            return Err("entier trop grand");
        }
        if let Int::Small(v) = self {
            if shift < 63 && v.unsigned_abs() < (1u64 << (62 - shift)) {
                return Ok(Int::Small(v << shift));
            }
        }
        let (negative, mag) = self.parts();
        Ok(make(negative, mag_shl(&mag, shift as usize)))
    }

    /// Arithmetic shift: floor(self / 2^shift).
    pub fn shr(&self, shift: u64) -> Int {
        if let Int::Small(v) = self {
            return Int::Small(if shift >= 64 { if *v < 0 { -1 } else { 0 } } else { v >> shift });
        }
        let (negative, mag) = self.parts();
        if !negative {
            return make(false, mag_shr(&mag, shift as usize));
        }
        // -((|x| - 1) >> n) - 1, which floors.
        let less = mag_sub(&mag, &[1]);
        let shifted = mag_shr(&less, shift as usize);
        make(true, mag_add(&shifted, &[1]))
    }

    /// Two's complement limbs of `self` on `limbs` limbs.
    fn twos(&self, limbs: usize) -> Vec<u32> {
        let (negative, mag) = self.parts();
        let mut out = mag.clone();
        out.resize(limbs, 0);
        if negative {
            let mut carry = 1u64;
            for limb in out.iter_mut() {
                let t = (!*limb) as u64 + carry;
                *limb = t as u32;
                carry = t >> 32;
            }
        }
        out
    }

    fn from_twos(mut limbs: Vec<u32>) -> Int {
        let negative = limbs.last().is_some_and(|top| top & 0x8000_0000 != 0);
        if negative {
            let mut carry = 1u64;
            for limb in limbs.iter_mut() {
                let t = (!*limb) as u64 + carry;
                *limb = t as u32;
                carry = t >> 32;
            }
        }
        make(negative, limbs)
    }

    pub fn bitop(&self, other: &Int, op: u8) -> Int {
        if let (Int::Small(a), Int::Small(b)) = (self, other) {
            return Int::Small(match op {
                b'&' => a & b,
                b'|' => a | b,
                _ => a ^ b,
            });
        }
        let limbs = (self.bit_length().max(other.bit_length()) as usize) / 32 + 2;
        let a = self.twos(limbs);
        let b = other.twos(limbs);
        let out = a
            .iter()
            .zip(b.iter())
            .map(|(x, y)| match op {
                b'&' => x & y,
                b'|' => x | y,
                _ => x ^ y,
            })
            .collect();
        Int::from_twos(out)
    }

    pub fn invert(&self) -> Int {
        // ~x == -x - 1
        self.neg().sub(&Int::Small(1))
    }

    // --------------------------------------------------------
    //  Text
    // --------------------------------------------------------

    /// Digits in `radix` (2 to 36), lowercase, with a `-` if negative.
    pub fn to_radix(&self, radix: u32) -> String {
        if let Int::Small(v) = self {
            if radix == 10 {
                return alloc::format!("{}", v);
            }
        }
        let (negative, mut mag) = self.parts();
        if mag.is_empty() {
            return String::from("0");
        }
        // Chunks of digits per division: 10^9 for decimal.
        let (chunk, width) = match radix {
            10 => (1_000_000_000u32, 9usize),
            16 => (1 << 28, 7),
            8 => (1 << 30, 10),
            2 => (1 << 31, 31),
            _ => (radix, 1),
        };
        let mut groups = Vec::new();
        while !mag.is_empty() {
            let (q, r) = mag_divmod_small(&mag, chunk);
            groups.push(r);
            mag = q;
        }
        let mut out = String::new();
        if negative {
            out.push('-');
        }
        for (i, group) in groups.iter().rev().enumerate() {
            let mut digits = Vec::new();
            let mut g = *group;
            for _ in 0..width {
                digits.push(core::char::from_digit(g % radix, radix).unwrap_or('?'));
                g /= radix;
            }
            let text: String = digits.iter().rev().collect();
            if i == 0 {
                let trimmed = text.trim_start_matches('0');
                out.push_str(if trimmed.is_empty() { "0" } else { trimmed });
            } else {
                out.push_str(&text);
            }
        }
        out
    }

    /// Parses digits in `radix` (no sign, no prefix, no underscore).
    pub fn parse_radix(digits: &str, radix: u32) -> Option<Int> {
        if digits.is_empty() {
            return None;
        }
        if digits.len() <= 18 && radix == 10 {
            return digits.parse::<i64>().ok().map(Int::Small);
        }
        let mut mag: Mag = Vec::new();
        for c in digits.chars() {
            let digit = c.to_digit(radix)?;
            // mag = mag * radix + digit
            let mut carry = digit as u64;
            for limb in mag.iter_mut() {
                let t = *limb as u64 * radix as u64 + carry;
                *limb = t as u32;
                carry = t >> 32;
            }
            if carry != 0 {
                mag.push(carry as u32);
            }
            if mag.len() > MAX_LIMBS {
                return None;
            }
        }
        Some(make(false, mag))
    }

    // --------------------------------------------------------
    //  Floats
    // --------------------------------------------------------

    /// The nearest float, ties to even; infinite beyond the range.
    pub fn to_float(&self) -> Num {
        if let Int::Small(v) = self {
            return Num::from_i64(*v);
        }
        let (negative, mag) = self.parts();
        let bits = self.bit_length();
        // The top 120 bits, the rest folded into a sticky bit.
        let drop = bits.saturating_sub(120);
        let top = mag_shr(&mag, drop as usize);
        let mut m = 0u128;
        for (i, limb) in top.iter().enumerate() {
            m |= (*limb as u128) << (32 * i);
        }
        let lost = drop > 0 && mag_shl(&top, drop as usize) != mag;
        if lost {
            m |= 1;
        }
        Num::from_parts(negative, drop as i32, m)
    }

    /// `int(x)` of a float: towards zero. None for an infinity or a NaN.
    pub fn from_float(x: Num) -> Option<Int> {
        if !x.is_finite() {
            return None;
        }
        let (negative, m, e) = x.trunc().decompose();
        if e >= 0 {
            let mag = mag_shl(&mag_from_u128(m as u128), e as usize);
            Some(make(negative, mag))
        } else {
            let shifted = if -e >= 64 { 0 } else { m >> (-e) };
            Some(make(negative, mag_from_u128(shifted as u128)))
        }
    }

    // --------------------------------------------------------
    //  Hash
    // --------------------------------------------------------

    /// CPython's numeric hash: the value modulo 2^61 - 1, sign kept, -1
    /// replaced by -2.
    pub fn hash(&self) -> i64 {
        const P: u64 = (1 << 61) - 1;
        let (negative, mag) = self.parts();
        let mut h: u64 = 0;
        for limb in mag.iter().rev() {
            // h = (h * 2^32 + limb) mod P
            let t = ((h as u128) << 32) | *limb as u128;
            h = (t % P as u128) as u64;
        }
        let mut value = h as i64;
        if negative {
            value = -value;
        }
        if value == -1 { -2 } else { value }
    }
}

impl PartialEq for Int {
    fn eq(&self, other: &Int) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

//! Unsigned big integers, just enough for RSA and the NIST curves.
//!
//! Numbers are little-endian vectors of 64-bit limbs, all of the length of
//! the modulus they are used with. The one real operation is Montgomery
//! multiplication (CIOS form): RSA verification is a modular power, and
//! elliptic-curve arithmetic is modular products, both of which it serves
//! without a single division.
//!
//! Nothing here runs in constant time. RSA and ECDSA **verification**
//! handle public values only; the one secret -- the ephemeral P-256 scalar
//! of a HelloRetryRequest -- lives for one handshake.

use alloc::vec;
use alloc::vec::Vec;
use core::cmp::Ordering;

/// Big-endian bytes to `limbs` limbs. Excess high bytes are dropped by
/// the caller's choice of `limbs`; leading zero bytes are fine.
pub fn from_be(bytes: &[u8], limbs: usize) -> Vec<u64> {
    let mut out = vec![0u64; limbs];
    for (index, byte) in bytes.iter().rev().enumerate() {
        let limb = index / 8;
        if limb >= limbs {
            break;
        }
        out[limb] |= (*byte as u64) << ((index % 8) * 8);
    }
    out
}

/// Limbs to exactly `length` big-endian bytes.
pub fn to_be(value: &[u64], length: usize) -> Vec<u8> {
    let mut out = vec![0u8; length];
    for index in 0..length {
        let limb = index / 8;
        if limb >= value.len() {
            break;
        }
        out[length - 1 - index] = (value[limb] >> ((index % 8) * 8)) as u8;
    }
    out
}

pub fn is_zero(value: &[u64]) -> bool {
    value.iter().all(|limb| *limb == 0)
}

pub fn compare(a: &[u64], b: &[u64]) -> Ordering {
    let length = a.len().max(b.len());
    for index in (0..length).rev() {
        let x = a.get(index).copied().unwrap_or(0);
        let y = b.get(index).copied().unwrap_or(0);
        match x.cmp(&y) {
            Ordering::Equal => continue,
            other => return other,
        }
    }
    Ordering::Equal
}

/// a -= b, returning the borrow. `b` may be shorter.
pub fn sub_assign(a: &mut [u64], b: &[u64]) -> bool {
    let mut borrow = 0u64;
    for index in 0..a.len() {
        let y = b.get(index).copied().unwrap_or(0);
        let (d1, o1) = a[index].overflowing_sub(y);
        let (d2, o2) = d1.overflowing_sub(borrow);
        a[index] = d2;
        borrow = (o1 | o2) as u64;
    }
    borrow != 0
}

/// a += b, returning the carry.
pub fn add_assign(a: &mut [u64], b: &[u64]) -> bool {
    let mut carry = 0u64;
    for index in 0..a.len() {
        let y = b.get(index).copied().unwrap_or(0);
        let (s1, o1) = a[index].overflowing_add(y);
        let (s2, o2) = s1.overflowing_add(carry);
        a[index] = s2;
        carry = (o1 | o2) as u64;
    }
    carry != 0
}

/// Number of significant bits.
pub fn bits(value: &[u64]) -> usize {
    for index in (0..value.len()).rev() {
        if value[index] != 0 {
            return index * 64 + 64 - value[index].leading_zeros() as usize;
        }
    }
    0
}

fn bit(value: &[u64], index: usize) -> bool {
    value
        .get(index / 64)
        .is_some_and(|limb| (limb >> (index % 64)) & 1 == 1)
}

/// `value mod modulus`, by shift and subtract. Slow and simple; used only
/// to set up a Montgomery context and to reduce the odd hash.
pub fn reduce(value: &[u64], modulus: &[u64]) -> Vec<u64> {
    let limbs = modulus.len();
    let mut remainder = vec![0u64; limbs + 1];
    for index in (0..bits(value)).rev() {
        // remainder = remainder * 2 + bit
        let mut carry = bit(value, index) as u64;
        for limb in remainder.iter_mut() {
            let next = *limb >> 63;
            *limb = (*limb << 1) | carry;
            carry = next;
        }
        if compare(&remainder, modulus) != Ordering::Less {
            sub_assign(&mut remainder, modulus);
        }
    }
    remainder.truncate(limbs);
    remainder
}

/// A Montgomery context for an odd modulus.
#[derive(Clone)]
pub struct Mont {
    pub modulus: Vec<u64>,
    /// -modulus^-1 mod 2^64.
    inverse: u64,
    /// R^2 mod modulus, R = 2^(64 * limbs).
    r2: Vec<u64>,
    /// R mod modulus: one, in Montgomery form.
    pub one: Vec<u64>,
}

impl Mont {
    /// `None` for an even or null modulus.
    pub fn new(modulus: Vec<u64>) -> Option<Self> {
        if modulus.is_empty() || modulus[0] & 1 == 0 {
            return None;
        }
        let limbs = modulus.len();
        // Newton iteration for the inverse modulo 2^64.
        let mut inverse = 1u64;
        for _ in 0..6 {
            inverse = inverse.wrapping_mul(2u64.wrapping_sub(modulus[0].wrapping_mul(inverse)));
        }
        let inverse = inverse.wrapping_neg();
        let mut r2_source = vec![0u64; 2 * limbs + 1];
        r2_source[2 * limbs] = 1;
        let r2 = reduce(&r2_source, &modulus);
        let mut r_source = vec![0u64; limbs + 1];
        r_source[limbs] = 1;
        let one = reduce(&r_source, &modulus);
        Some(Mont {
            modulus,
            inverse,
            r2,
            one,
        })
    }

    pub fn limbs(&self) -> usize {
        self.modulus.len()
    }

    /// a * b * R^-1 mod m, for a, b < m.
    pub fn mul(&self, a: &[u64], b: &[u64]) -> Vec<u64> {
        let n = self.modulus.len();
        let mut t = vec![0u64; n + 2];
        for i in 0..n {
            let mut carry = 0u128;
            for j in 0..n {
                let product = t[j] as u128 + a[i] as u128 * b[j] as u128 + carry;
                t[j] = product as u64;
                carry = product >> 64;
            }
            let sum = t[n] as u128 + carry;
            t[n] = sum as u64;
            t[n + 1] = (sum >> 64) as u64;

            let m = t[0].wrapping_mul(self.inverse);
            let mut carry = (t[0] as u128 + m as u128 * self.modulus[0] as u128) >> 64;
            for j in 1..n {
                let product = t[j] as u128 + m as u128 * self.modulus[j] as u128 + carry;
                t[j - 1] = product as u64;
                carry = product >> 64;
            }
            let sum = t[n] as u128 + carry;
            t[n - 1] = sum as u64;
            t[n] = t[n + 1] + (sum >> 64) as u64;
            t[n + 1] = 0;
        }
        let overflow = t[n] != 0;
        t.truncate(n);
        if overflow || compare(&t, &self.modulus) != Ordering::Less {
            sub_assign(&mut t, &self.modulus);
        }
        t
    }

    /// Into Montgomery form. `a` must be below the modulus.
    pub fn to(&self, a: &[u64]) -> Vec<u64> {
        let mut padded = a.to_vec();
        padded.resize(self.limbs(), 0);
        self.mul(&padded, &self.r2)
    }

    /// Out of Montgomery form.
    pub fn from(&self, a: &[u64]) -> Vec<u64> {
        let mut one = vec![0u64; self.limbs()];
        one[0] = 1;
        self.mul(a, &one)
    }

    pub fn add(&self, a: &[u64], b: &[u64]) -> Vec<u64> {
        let mut sum = a.to_vec();
        let carry = add_assign(&mut sum, b);
        if carry || compare(&sum, &self.modulus) != Ordering::Less {
            sub_assign(&mut sum, &self.modulus);
        }
        sum
    }

    pub fn sub(&self, a: &[u64], b: &[u64]) -> Vec<u64> {
        let mut difference = a.to_vec();
        if sub_assign(&mut difference, b) {
            add_assign(&mut difference, &self.modulus);
        }
        difference
    }

    /// base^exponent, base in Montgomery form, result in Montgomery form.
    /// The exponent is big-endian bytes.
    pub fn pow(&self, base: &[u64], exponent: &[u8]) -> Vec<u64> {
        let mut result = self.one.clone();
        for byte in exponent {
            for shift in (0..8).rev() {
                result = self.mul(&result, &result);
                if (byte >> shift) & 1 == 1 {
                    result = self.mul(&result, base);
                }
            }
        }
        result
    }

    /// Inverse of a Montgomery-form value, for a **prime** modulus:
    /// a^(m-2).
    pub fn invert(&self, a: &[u64]) -> Vec<u64> {
        let mut exponent = self.modulus.clone();
        let two = [2u64];
        sub_assign(&mut exponent, &two);
        let bytes = to_be(&exponent, self.limbs() * 8);
        self.pow(a, &bytes)
    }
}

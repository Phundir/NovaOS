//! Unsigned big integers, for exact decimal conversions of numbers.
//!
//! Printing a double exactly, or reading a decimal literal into the
//! nearest double, needs more than 128 bits: `1e-300` is a 700-digit
//! integer once the decimal point is moved out of the way. These
//! integers exist only for that, so they carry only the operations the
//! conversions use -- small multiplications, shifts, comparisons,
//! subtraction and a conversion to decimal digits.

use alloc::vec::Vec;
use core::cmp::Ordering;

/// Little-endian 32-bit limbs, no trailing zero limb.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Big {
    limbs: Vec<u32>,
}

impl Big {
    pub fn from_u128(mut value: u128) -> Big {
        let mut limbs = Vec::new();
        while value != 0 {
            limbs.push(value as u32);
            value >>= 32;
        }
        Big { limbs }
    }

    pub fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    fn trim(&mut self) {
        while self.limbs.last() == Some(&0) {
            self.limbs.pop();
        }
    }

    /// Number of significant bits.
    pub fn bits(&self) -> u32 {
        match self.limbs.last() {
            None => 0,
            Some(top) => (self.limbs.len() as u32 - 1) * 32 + (32 - top.leading_zeros()),
        }
    }

    pub fn mul_small(&mut self, factor: u32) {
        let mut carry: u64 = 0;
        for limb in self.limbs.iter_mut() {
            let product = *limb as u64 * factor as u64 + carry;
            *limb = product as u32;
            carry = product >> 32;
        }
        if carry != 0 {
            self.limbs.push(carry as u32);
        }
        self.trim();
    }

    pub fn add_small(&mut self, value: u32) {
        let mut carry = value as u64;
        for limb in self.limbs.iter_mut() {
            if carry == 0 {
                return;
            }
            let sum = *limb as u64 + carry;
            *limb = sum as u32;
            carry = sum >> 32;
        }
        if carry != 0 {
            self.limbs.push(carry as u32);
        }
    }

    /// Multiplies by `10^power`.
    pub fn mul_pow10(&mut self, mut power: u32) {
        while power >= 9 {
            self.mul_small(1_000_000_000);
            power -= 9;
        }
        if power > 0 {
            self.mul_small(10u32.pow(power));
        }
    }

    /// Multiplies by `5^power`.
    pub fn mul_pow5(&mut self, mut power: u32) {
        while power >= 13 {
            self.mul_small(1_220_703_125); // 5^13
            power -= 13;
        }
        if power > 0 {
            self.mul_small(5u32.pow(power));
        }
    }

    pub fn shl(&self, shift: u32) -> Big {
        if self.is_zero() {
            return self.clone();
        }
        let words = (shift / 32) as usize;
        let bits = shift % 32;
        let mut limbs = alloc::vec![0u32; words];
        if bits == 0 {
            limbs.extend_from_slice(&self.limbs);
        } else {
            let mut carry = 0u32;
            for limb in &self.limbs {
                limbs.push((limb << bits) | carry);
                carry = limb >> (32 - bits);
            }
            if carry != 0 {
                limbs.push(carry);
            }
        }
        Big { limbs }
    }

    pub fn compare(&self, other: &Big) -> Ordering {
        if self.limbs.len() != other.limbs.len() {
            return self.limbs.len().cmp(&other.limbs.len());
        }
        for (a, b) in self.limbs.iter().rev().zip(other.limbs.iter().rev()) {
            if a != b {
                return a.cmp(b);
            }
        }
        Ordering::Equal
    }

    /// `self -= other`; `other` must not be larger.
    pub fn sub_assign(&mut self, other: &Big) {
        let mut borrow = 0i64;
        for index in 0..self.limbs.len() {
            let rhs = other.limbs.get(index).copied().unwrap_or(0) as i64;
            let mut value = self.limbs[index] as i64 - rhs - borrow;
            if value < 0 {
                value += 1 << 32;
                borrow = 1;
            } else {
                borrow = 0;
            }
            self.limbs[index] = value as u32;
        }
        self.trim();
    }

    /// The top `count` bits (at most 128) as an integer, and whether any
    /// bit below them is set.
    pub fn top_bits(&self, count: u32) -> (u128, bool) {
        let total = self.bits();
        if total <= count {
            let mut value = 0u128;
            for limb in self.limbs.iter().rev() {
                value = (value << 32) | *limb as u128;
            }
            return (value, false);
        }
        let drop = total - count;
        let mut value = 0u128;
        let mut sticky = false;
        for bit in (0..total).rev() {
            let set = self.limbs[(bit / 32) as usize] >> (bit % 32) & 1 == 1;
            if bit >= drop {
                value = (value << 1) | set as u128;
            } else if set {
                sticky = true;
                break;
            }
        }
        (value, sticky)
    }

    /// Quotient and remainder of a division by a small divisor.
    pub fn divmod_small(&self, divisor: u32) -> (Big, u32) {
        let mut limbs = self.limbs.clone();
        let mut remainder = 0u64;
        for limb in limbs.iter_mut().rev() {
            let current = (remainder << 32) | *limb as u64;
            *limb = (current / divisor as u64) as u32;
            remainder = current % divisor as u64;
        }
        let mut quotient = Big { limbs };
        quotient.trim();
        (quotient, remainder as u32)
    }

    /// Decimal digits, most significant first, as ASCII.
    pub fn decimal(&self) -> Vec<u8> {
        let mut work = self.limbs.clone();
        let mut chunks: Vec<u32> = Vec::new();
        while !work.is_empty() {
            let mut remainder = 0u64;
            for limb in work.iter_mut().rev() {
                let current = (remainder << 32) | *limb as u64;
                *limb = (current / 1_000_000_000) as u32;
                remainder = current % 1_000_000_000;
            }
            while work.last() == Some(&0) {
                work.pop();
            }
            chunks.push(remainder as u32);
        }
        let mut out = Vec::new();
        for (index, chunk) in chunks.iter().rev().enumerate() {
            let mut digits = [0u8; 9];
            let mut value = *chunk;
            for slot in digits.iter_mut().rev() {
                *slot = b'0' + (value % 10) as u8;
                value /= 10;
            }
            if index == 0 {
                let first = digits.iter().position(|d| *d != b'0').unwrap_or(8);
                out.extend_from_slice(&digits[first..]);
            } else {
                out.extend_from_slice(&digits);
            }
        }
        if out.is_empty() {
            out.push(b'0');
        }
        out
    }

    /// From ASCII decimal digits.
    pub fn from_decimal(digits: &[u8]) -> Big {
        let mut big = Big { limbs: Vec::new() };
        for chunk in digits.chunks(9) {
            let mut value = 0u32;
            for digit in chunk {
                value = value * 10 + (digit - b'0') as u32;
            }
            big.mul_small(10u32.pow(chunk.len() as u32));
            big.add_small(value);
        }
        big.trim();
        big
    }
}

//! Arithmetic in GF(2^255 - 19), shared by X25519 and Ed25519.
//!
//! Field elements are five 51-bit limbs; products go through `u128`.
//! Written once for the two curves that live over this field: the
//! Montgomery form of X25519 (key agreement, RFC 7748) and the twisted
//! Edwards form of Ed25519 (signatures, RFC 8032).
//!
//! `fe_add` does not carry: its result may only feed `fe_mul`,
//! `fe_square` or `fe_sub`, which carry their inputs.

pub type Fe = [u64; 5];

const MASK: u64 = (1 << 51) - 1;

pub fn fe_from_bytes(bytes: &[u8; 32]) -> Fe {
    let load = |offset: usize| {
        let mut word = [0u8; 8];
        word.copy_from_slice(&bytes[offset..offset + 8]);
        u64::from_le_bytes(word)
    };
    [
        load(0) & MASK,
        (load(6) >> 3) & MASK,
        (load(12) >> 6) & MASK,
        (load(19) >> 1) & MASK,
        (load(24) >> 12) & MASK,
    ]
}

pub fn fe_carry(mut f: Fe) -> Fe {
    for _ in 0..2 {
        let c0 = f[0] >> 51;
        f[0] &= MASK;
        f[1] += c0;
        let c1 = f[1] >> 51;
        f[1] &= MASK;
        f[2] += c1;
        let c2 = f[2] >> 51;
        f[2] &= MASK;
        f[3] += c2;
        let c3 = f[3] >> 51;
        f[3] &= MASK;
        f[4] += c3;
        let c4 = f[4] >> 51;
        f[4] &= MASK;
        f[0] += c4 * 19;
    }
    f
}

pub fn fe_to_bytes(f: &Fe) -> [u8; 32] {
    let mut f = fe_carry(*f);
    // Full reduction: subtract p once if f >= p.
    let mut q = (f[0] + 19) >> 51;
    q = (f[1] + q) >> 51;
    q = (f[2] + q) >> 51;
    q = (f[3] + q) >> 51;
    q = (f[4] + q) >> 51;
    f[0] += 19 * q;
    let c = f[0] >> 51;
    f[0] &= MASK;
    f[1] += c;
    let c = f[1] >> 51;
    f[1] &= MASK;
    f[2] += c;
    let c = f[2] >> 51;
    f[2] &= MASK;
    f[3] += c;
    let c = f[3] >> 51;
    f[3] &= MASK;
    f[4] += c;
    f[4] &= MASK;

    let mut out = [0u8; 32];
    let words = [
        f[0] | (f[1] << 51),
        (f[1] >> 13) | (f[2] << 38),
        (f[2] >> 26) | (f[3] << 25),
        (f[3] >> 39) | (f[4] << 12),
    ];
    for (index, word) in words.iter().enumerate() {
        out[index * 8..index * 8 + 8].copy_from_slice(&word.to_le_bytes());
    }
    out
}

pub fn fe_add(a: &Fe, b: &Fe) -> Fe {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2], a[3] + b[3], a[4] + b[4]]
}

/// a - b, with 2p added so that no limb goes negative.
///
/// `b` is carried first: the sum of two elements (`fe_add` does not
/// carry) has limbs above 2^52, larger than the 2p added here, and the
/// subtraction would wrap. X25519 never subtracts a sum; Ed25519 does.
pub fn fe_sub(a: &Fe, b: &Fe) -> Fe {
    let b = fe_carry(*b);
    fe_carry([
        (a[0] + 0xf_ffff_ffff_ffda) - b[0],
        (a[1] + 0xf_ffff_ffff_fffe) - b[1],
        (a[2] + 0xf_ffff_ffff_fffe) - b[2],
        (a[3] + 0xf_ffff_ffff_fffe) - b[3],
        (a[4] + 0xf_ffff_ffff_fffe) - b[4],
    ])
}

pub fn fe_mul(a: &Fe, b: &Fe) -> Fe {
    let a = fe_carry(*a);
    let b = fe_carry(*b);
    let m = |x: u64, y: u64| x as u128 * y as u128;
    let b1_19 = b[1] * 19;
    let b2_19 = b[2] * 19;
    let b3_19 = b[3] * 19;
    let b4_19 = b[4] * 19;
    let t0 = m(a[0], b[0]) + m(a[1], b4_19) + m(a[2], b3_19) + m(a[3], b2_19) + m(a[4], b1_19);
    let mut t1 = m(a[0], b[1]) + m(a[1], b[0]) + m(a[2], b4_19) + m(a[3], b3_19) + m(a[4], b2_19);
    let mut t2 = m(a[0], b[2]) + m(a[1], b[1]) + m(a[2], b[0]) + m(a[3], b4_19) + m(a[4], b3_19);
    let mut t3 = m(a[0], b[3]) + m(a[1], b[2]) + m(a[2], b[1]) + m(a[3], b[0]) + m(a[4], b4_19);
    let mut t4 = m(a[0], b[4]) + m(a[1], b[3]) + m(a[2], b[2]) + m(a[3], b[1]) + m(a[4], b[0]);

    let mut r = [0u64; 5];
    r[0] = (t0 as u64) & MASK;
    t1 += t0 >> 51;
    r[1] = (t1 as u64) & MASK;
    t2 += t1 >> 51;
    r[2] = (t2 as u64) & MASK;
    t3 += t2 >> 51;
    r[3] = (t3 as u64) & MASK;
    t4 += t3 >> 51;
    r[4] = (t4 as u64) & MASK;
    let carry = (t4 >> 51) as u64;
    r[0] += carry * 19;
    let c = r[0] >> 51;
    r[0] &= MASK;
    r[1] += c;
    r
}

pub fn fe_square(a: &Fe) -> Fe {
    fe_mul(a, a)
}

pub fn fe_mul_small(a: &Fe, k: u64) -> Fe {
    let a = fe_carry(*a);
    let mut r = [0u64; 5];
    let mut carry = 0u128;
    for index in 0..5 {
        let t = a[index] as u128 * k as u128 + carry;
        r[index] = (t as u64) & MASK;
        carry = t >> 51;
    }
    r[0] += (carry as u64) * 19;
    fe_carry(r)
}

pub fn fe_invert(z: &Fe) -> Fe {
    // z^(p-2) by square-and-multiply over the bits of p - 2.
    let mut result: Fe = [1, 0, 0, 0, 0];
    // p - 2 = 2^255 - 21: bits 254..5 set, then 01011.
    for bit in (0..255).rev() {
        result = fe_square(&result);
        let set = bit >= 5 || matches!(bit, 0 | 1 | 3);
        if set {
            result = fe_mul(&result, z);
        }
    }
    result
}

pub fn cswap(swap: u64, a: &mut Fe, b: &mut Fe) {
    let mask = 0u64.wrapping_sub(swap);
    for index in 0..5 {
        let t = mask & (a[index] ^ b[index]);
        a[index] ^= t;
        b[index] ^= t;
    }
}

/// The neutral element of addition and of multiplication.
pub const FE_ZERO: Fe = [0; 5];
pub const FE_ONE: Fe = [1, 0, 0, 0, 0];

/// -a.
pub fn fe_neg(a: &Fe) -> Fe {
    fe_sub(&FE_ZERO, a)
}

/// z^((p - 5) / 8) = z^(2^252 - 3), the exponent of the square root
/// in Ed25519's point decompression.
pub fn fe_pow22523(z: &Fe) -> Fe {
    // 2^252 - 3: bits 251..2 set, bit 1 clear, bit 0 set.
    let mut result = FE_ONE;
    for bit in (0..252).rev() {
        result = fe_square(&result);
        if bit != 1 {
            result = fe_mul(&result, z);
        }
    }
    result
}

/// The "sign" of RFC 8032: the lowest bit of the canonical encoding.
pub fn fe_is_negative(a: &Fe) -> bool {
    fe_to_bytes(a)[0] & 1 == 1
}

/// Equality of the canonical encodings (two representations of the same
/// element may differ limb by limb).
pub fn fe_equal(a: &Fe, b: &Fe) -> bool {
    super::equal(&fe_to_bytes(a), &fe_to_bytes(b))
}

pub fn fe_is_zero(a: &Fe) -> bool {
    fe_to_bytes(a) == [0u8; 32]
}

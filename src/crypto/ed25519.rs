//! Ed25519 signatures (RFC 8032).
//!
//! ## Why it exists
//!
//! Until phase 15 Nova could hash but not sign. SYSTEM binaries are
//! anchored in the kernel image (a digest table), SELF ones are what
//! they claim, but ENTERPRISE -- "a third-party certificate" (spec 3.2) --
//! was a SHA-256 of public data: anyone reading the format could forge
//! one, and DECISIONS forbade presenting it as verified. The registry of
//! phase 15 has the same need: a manifest published next to its package
//! by the same account proves nothing unless it is signed by a key the
//! system knows. One primitive serves both.
//!
//! Ed25519 rather than ECDSA (already here, for TLS): deterministic
//! signatures (no random nonce whose reuse leaks the key), 32-byte keys,
//! 64-byte signatures, and a field shared with X25519 (`field25519`).
//!
//! ## Shape
//!
//! Points in extended twisted Edwards coordinates (X:Y:Z:T), a = -1.
//! Scalars modulo L are reduced bit by bit (512 shift-subtract steps):
//! slower than the Barrett reduction of the reference code, and still
//! microseconds per signature -- the code that verifies binaries at load
//! is not where time goes, and this form is checkable by eye.
//!
//! Verification is the cofactorless equation of RFC 8032, 5.1.7:
//! `[S]B = R + [k]A`, with S < L and canonical encodings required.

use super::field25519::*;
use super::sha2::{Hash, Sha512};

/// Size of a public key, a secret seed, and a signature.
pub const PUBLIC_KEY_SIZE: usize = 32;
pub const SECRET_KEY_SIZE: usize = 32;
pub const SIGNATURE_SIZE: usize = 64;

/// d = -121665/121666, its double, and sqrt(-1), as RFC 8032 gives them
/// (little-endian), checked against the published vectors at boot.
const D_BYTES: [u8; 32] = hex32("a3785913ca4deb75abd841414d0a700098e879777940c78c73fe6f2bee6c0352");
const D2_BYTES: [u8; 32] = hex32("59f1b226949bd6eb56b183829a14e00030d1f3eef2808e19e7fcdf56dcd90624");
const SQRT_M1_BYTES: [u8; 32] = hex32("b0a00e4a271beec478e42fad0618432fa7d7fb3d99004d2b0bdfc14f8024832b");
/// Encoding of the base point B (y = 4/5, x positive).
const BASE_BYTES: [u8; 32] = hex32("5866666666666666666666666666666666666666666666666666666666666666");

/// L = 2^252 + 27742317777372353535851937790883648493, little-endian words.
const L: [u64; 4] = [
    0x5812631a5cf5d3ed,
    0x14def9dea2f79cd6,
    0x0000000000000000,
    0x1000000000000000,
];

const fn hex32(text: &str) -> [u8; 32] {
    let bytes = text.as_bytes();
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = (nibble(bytes[2 * i]) << 4) | nibble(bytes[2 * i + 1]);
        i += 1;
    }
    out
}

const fn nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => 0,
    }
}

// ============================================================
//  Points
// ============================================================

#[derive(Clone, Copy)]
struct Point {
    x: Fe,
    y: Fe,
    z: Fe,
    t: Fe,
}

const IDENTITY: Point = Point {
    x: FE_ZERO,
    y: FE_ONE,
    z: FE_ONE,
    t: FE_ZERO,
};

fn add(p: &Point, q: &Point) -> Point {
    let d2 = fe_from_bytes(&D2_BYTES);
    let a = fe_mul(&fe_sub(&p.y, &p.x), &fe_sub(&q.y, &q.x));
    let b = fe_mul(&fe_add(&p.y, &p.x), &fe_add(&q.y, &q.x));
    let c = fe_mul(&fe_mul(&p.t, &d2), &q.t);
    let d = fe_mul_small(&fe_mul(&p.z, &q.z), 2);
    let e = fe_sub(&b, &a);
    let f = fe_sub(&d, &c);
    let g = fe_add(&d, &c);
    let h = fe_add(&b, &a);
    Point {
        x: fe_mul(&e, &f),
        y: fe_mul(&g, &h),
        t: fe_mul(&e, &h),
        z: fe_mul(&f, &g),
    }
}

fn double(p: &Point) -> Point {
    let a = fe_square(&p.x);
    let b = fe_square(&p.y);
    let c = fe_mul_small(&fe_square(&p.z), 2);
    let h = fe_add(&a, &b);
    let e = fe_sub(&h, &fe_square(&fe_add(&p.x, &p.y)));
    let g = fe_sub(&a, &b);
    let f = fe_add(&c, &g);
    Point {
        x: fe_mul(&e, &f),
        y: fe_mul(&g, &h),
        t: fe_mul(&e, &h),
        z: fe_mul(&f, &g),
    }
}

fn negate(p: &Point) -> Point {
    Point {
        x: fe_neg(&p.x),
        y: p.y,
        z: p.z,
        t: fe_neg(&p.t),
    }
}

/// Selects `b` when `choice` is 1, `a` when 0, without a branch.
fn select(a: &Point, b: &Point, choice: u64) -> Point {
    let mask = 0u64.wrapping_sub(choice);
    let pick = |x: &Fe, y: &Fe| {
        let mut out = [0u64; 5];
        for i in 0..5 {
            out[i] = x[i] ^ (mask & (x[i] ^ y[i]));
        }
        out
    };
    Point {
        x: pick(&a.x, &b.x),
        y: pick(&a.y, &b.y),
        z: pick(&a.z, &b.z),
        t: pick(&a.t, &b.t),
    }
}

/// `[scalar] p`, 256 doublings and 256 additions whatever the scalar:
/// signing multiplies by secrets, and its flow must not depend on them.
fn multiply(scalar: &[u8; 32], p: &Point) -> Point {
    let mut result = IDENTITY;
    for bit in (0..256).rev() {
        result = double(&result);
        let set = ((scalar[bit / 8] >> (bit % 8)) & 1) as u64;
        let sum = add(&result, p);
        result = select(&result, &sum, set);
    }
    result
}

fn encode(p: &Point) -> [u8; 32] {
    let inverse = fe_invert(&p.z);
    let x = fe_mul(&p.x, &inverse);
    let y = fe_mul(&p.y, &inverse);
    let mut out = fe_to_bytes(&y);
    if fe_is_negative(&x) {
        out[31] |= 0x80;
    }
    out
}

/// RFC 8032, 5.1.3. `None` for an encoding that is not a point, or not
/// the canonical encoding of one.
fn decode(bytes: &[u8; 32]) -> Option<Point> {
    let sign = bytes[31] >> 7;
    let mut y_bytes = *bytes;
    y_bytes[31] &= 0x7f;
    let y = fe_from_bytes(&y_bytes);
    // y must be below p: re-encoding it must give the same bytes.
    if fe_to_bytes(&y) != y_bytes {
        return None;
    }

    let d = fe_from_bytes(&D_BYTES);
    let y2 = fe_square(&y);
    let u = fe_sub(&y2, &FE_ONE);
    let v = fe_add(&fe_mul(&d, &y2), &FE_ONE);

    // x = u v^3 (u v^7)^((p-5)/8)
    let v3 = fe_mul(&fe_square(&v), &v);
    let v7 = fe_mul(&fe_square(&v3), &v);
    let mut x = fe_mul(&fe_mul(&u, &v3), &fe_pow22523(&fe_mul(&u, &v7)));

    let vx2 = fe_mul(&v, &fe_square(&x));
    if fe_equal(&vx2, &u) {
        // x is a root.
    } else if fe_equal(&vx2, &fe_neg(&u)) {
        x = fe_mul(&x, &fe_from_bytes(&SQRT_M1_BYTES));
    } else {
        return None;
    }

    if fe_is_zero(&x) && sign == 1 {
        return None;
    }
    if fe_is_negative(&x) != (sign == 1) {
        x = fe_neg(&x);
    }
    Some(Point {
        x,
        y,
        z: FE_ONE,
        t: fe_mul(&x, &y),
    })
}

fn base() -> Point {
    // The constant always decodes; a failure here is a broken field
    // implementation, which the boot vectors report.
    decode(&BASE_BYTES).unwrap_or(IDENTITY)
}

// ============================================================
//  Scalars modulo L
// ============================================================

fn geq_l(r: &[u64; 4]) -> bool {
    for i in (0..4).rev() {
        if r[i] != L[i] {
            return r[i] > L[i];
        }
    }
    true
}

fn sub_l(r: &mut [u64; 4]) {
    let mut borrow = 0u64;
    for i in 0..4 {
        let (d1, b1) = r[i].overflowing_sub(L[i]);
        let (d2, b2) = d1.overflowing_sub(borrow);
        r[i] = d2;
        borrow = (b1 | b2) as u64;
    }
}

/// A little-endian integer of any length, reduced modulo L.
fn reduce(bytes: &[u8]) -> [u8; 32] {
    let mut r = [0u64; 4];
    for bit in (0..bytes.len() * 8).rev() {
        // r = 2r + bit; r < 2L < 2^254 so the shift does not overflow.
        let incoming = ((bytes[bit / 8] >> (bit % 8)) & 1) as u64;
        let mut carry = incoming;
        for word in r.iter_mut() {
            let next = *word >> 63;
            *word = (*word << 1) | carry;
            carry = next;
        }
        if geq_l(&r) {
            sub_l(&mut r);
        }
    }
    let mut out = [0u8; 32];
    for i in 0..4 {
        out[i * 8..i * 8 + 8].copy_from_slice(&r[i].to_le_bytes());
    }
    out
}

/// (a * b + c) mod L, inputs little-endian scalars below 2^256.
fn mul_add(a: &[u8; 32], b: &[u8; 32], c: &[u8; 32]) -> [u8; 32] {
    let word = |s: &[u8; 32], i: usize| {
        let mut w = [0u8; 8];
        w.copy_from_slice(&s[i * 8..i * 8 + 8]);
        u64::from_le_bytes(w)
    };
    let mut product = [0u64; 9];
    for i in 0..4 {
        let mut carry = 0u128;
        for j in 0..4 {
            let t = word(a, i) as u128 * word(b, j) as u128 + product[i + j] as u128 + carry;
            product[i + j] = t as u64;
            carry = t >> 64;
        }
        product[i + 4] = carry as u64;
    }
    let mut carry = 0u128;
    for i in 0..9 {
        let addend = if i < 4 { word(c, i) as u128 } else { 0 };
        let t = product[i] as u128 + addend + carry;
        product[i] = t as u64;
        carry = t >> 64;
    }
    let mut bytes = [0u8; 72];
    for i in 0..9 {
        bytes[i * 8..i * 8 + 8].copy_from_slice(&product[i].to_le_bytes());
    }
    reduce(&bytes)
}

fn below_l(s: &[u8; 32]) -> bool {
    let mut words = [0u64; 4];
    for i in 0..4 {
        let mut w = [0u8; 8];
        w.copy_from_slice(&s[i * 8..i * 8 + 8]);
        words[i] = u64::from_le_bytes(w);
    }
    !geq_l(&words)
}

// ============================================================
//  Keys and signatures
// ============================================================

/// The expanded secret: the clamped scalar `a` and the nonce prefix.
fn expand(seed: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let digest = Sha512::digest(seed);
    let mut scalar = [0u8; 32];
    scalar.copy_from_slice(&digest[..32]);
    scalar[0] &= 248;
    scalar[31] &= 127;
    scalar[31] |= 64;
    let mut prefix = [0u8; 32];
    prefix.copy_from_slice(&digest[32..64]);
    (scalar, prefix)
}

/// The public key of a 32-byte secret seed.
pub fn public_key(seed: &[u8; 32]) -> [u8; 32] {
    let (scalar, _) = expand(seed);
    encode(&multiply(&scalar, &base()))
}

/// Signs `message` with the secret seed (RFC 8032, 5.1.6).
pub fn sign(seed: &[u8; 32], message: &[u8]) -> [u8; 64] {
    let (scalar, prefix) = expand(seed);
    let public = encode(&multiply(&scalar, &base()));

    let mut hasher = Sha512::new();
    hasher.update(&prefix);
    hasher.update(message);
    let r = reduce(&hasher.finish()[..64]);
    let big_r = encode(&multiply(&r, &base()));

    let mut hasher = Sha512::new();
    hasher.update(&big_r);
    hasher.update(&public);
    hasher.update(message);
    let k = reduce(&hasher.finish()[..64]);
    let s = mul_add(&k, &scalar, &r);

    let mut signature = [0u8; 64];
    signature[..32].copy_from_slice(&big_r);
    signature[32..].copy_from_slice(&s);
    signature
}

/// Verifies `signature` on `message` under `public` (RFC 8032, 5.1.7).
pub fn verify(public: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    let Some(a) = decode(public) else {
        return false;
    };
    let mut r_bytes = [0u8; 32];
    r_bytes.copy_from_slice(&signature[..32]);
    let mut s = [0u8; 32];
    s.copy_from_slice(&signature[32..]);
    if !below_l(&s) || decode(&r_bytes).is_none() {
        return false;
    }

    let mut hasher = Sha512::new();
    hasher.update(&r_bytes);
    hasher.update(public);
    hasher.update(message);
    let k = reduce(&hasher.finish()[..64]);

    // [S]B - [k]A must encode to R.
    let sb = multiply(&s, &base());
    let ka = multiply(&k, &a);
    let check = add(&sb, &negate(&ka));
    super::equal(&encode(&check), &r_bytes)
}

/// Convenience over slices: false for wrong lengths.
pub fn verify_slices(public: &[u8], message: &[u8], signature: &[u8]) -> bool {
    let (Ok(public), Ok(signature)) = (
        <&[u8; 32]>::try_from(public),
        <&[u8; 64]>::try_from(signature),
    ) else {
        return false;
    };
    verify(public, message, signature)
}

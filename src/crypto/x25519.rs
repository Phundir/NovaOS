//! X25519 key agreement (RFC 7748).
//!
//! The key share every TLS 1.3 server accepts. The field arithmetic is
//! `field25519`, shared with Ed25519. The ladder is the RFC's, with a
//! conditional swap by mask so that its flow does not depend on the
//! secret scalar.

use super::field25519::*;

/// The scalar multiplication: `scalar * u`, both little-endian.
pub fn scalarmult(scalar: &[u8; 32], u: &[u8; 32]) -> [u8; 32] {
    let mut k = *scalar;
    k[0] &= 248;
    k[31] &= 127;
    k[31] |= 64;
    let mut u_masked = *u;
    u_masked[31] &= 127;

    let x1 = fe_from_bytes(&u_masked);
    let mut x2: Fe = [1, 0, 0, 0, 0];
    let mut z2: Fe = [0; 5];
    let mut x3 = x1;
    let mut z3: Fe = [1, 0, 0, 0, 0];
    let mut swap = 0u64;

    for t in (0..255).rev() {
        let bit = ((k[t / 8] >> (t % 8)) & 1) as u64;
        swap ^= bit;
        cswap(swap, &mut x2, &mut x3);
        cswap(swap, &mut z2, &mut z3);
        swap = bit;

        let a = fe_add(&x2, &z2);
        let aa = fe_square(&a);
        let b = fe_sub(&x2, &z2);
        let bb = fe_square(&b);
        let e = fe_sub(&aa, &bb);
        let c = fe_add(&x3, &z3);
        let d = fe_sub(&x3, &z3);
        let da = fe_mul(&d, &a);
        let cb = fe_mul(&c, &b);
        x3 = fe_square(&fe_add(&da, &cb));
        z3 = fe_mul(&x1, &fe_square(&fe_sub(&da, &cb)));
        x2 = fe_mul(&aa, &bb);
        z2 = fe_mul(&e, &fe_add(&aa, &fe_mul_small(&e, 121665)));
    }
    cswap(swap, &mut x2, &mut x3);
    cswap(swap, &mut z2, &mut z3);

    fe_to_bytes(&fe_mul(&x2, &fe_invert(&z2)))
}

/// The public key of a private one: `scalar * 9`.
pub fn public_key(private: &[u8; 32]) -> [u8; 32] {
    let mut base = [0u8; 32];
    base[0] = 9;
    scalarmult(private, &base)
}

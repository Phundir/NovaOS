//! Cryptography for the web client: hashes, ciphers, key agreement and
//! signature verification, written from the standards.
//!
//! ## Why it exists
//!
//! Almost every web site answers only over HTTPS, DuckDuckGo included. So
//! the browser needs TLS 1.3, and TLS 1.3 needs every piece here. The
//! project takes no external dependency; these are the RFC and FIPS
//! algorithms, checked against their published test vectors
//! (`selftest`).
//!
//! ## Only `core` and `alloc`
//!
//! Nothing in this directory touches the kernel: no lock, no scheduler,
//! no hardware. That keeps it checkable on the development machine, fed
//! the same vectors, and it keeps the one input that must come from the
//! machine -- randomness -- outside, in `crate::random`.
//!
//! | Module | Standard | Used for |
//! |---|---|---|
//! | `sha2` | FIPS 180-4 | transcript, signatures |
//! | `hmac` | RFC 2104, RFC 5869 | TLS 1.3 key schedule |
//! | `aes` | FIPS 197, SP 800-38D | `TLS_AES_*_GCM` records |
//! | `chacha` | RFC 8439 | `TLS_CHACHA20_POLY1305`, the generator |
//! | `x25519` | RFC 7748 | key share |
//! | `bignum` | -- | Montgomery arithmetic |
//! | `ecc` | FIPS 186-4 | ECDSA P-256/P-384, P-256 key share |
//! | `rsa` | RFC 8017 | PKCS #1 v1.5 and PSS verification |

pub mod aes;
pub mod bignum;
pub mod chacha;
pub mod ecc;
pub mod hmac;
pub mod rsa;
pub mod selftest;
pub mod sha2;
pub mod x25519;

/// Compares two byte strings in time independent of where they differ:
/// an authentication tag compared with an early exit tells an attacker
/// how many of its first bytes were right.
pub fn equal(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut difference = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        difference |= x ^ y;
    }
    difference == 0
}

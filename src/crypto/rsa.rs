//! RSA signature verification: PKCS #1 v1.5 and PSS (RFC 8017).
//!
//! Most certificate chains on the web are RSA-signed with PKCS #1 v1.5,
//! and a TLS 1.3 server with an RSA key proves it with PSS
//! (`rsa_pss_rsae_*`) -- the two are needed. Only verification: this
//! client holds no RSA key.

use alloc::vec::Vec;

use super::bignum::{self, Mont};
use super::sha2::{Hash, Sha256, Sha384, Sha512};

/// The digests a signature may be computed over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Digest {
    Sha256,
    Sha384,
    Sha512,
}

impl Digest {
    pub fn len(&self) -> usize {
        match self {
            Digest::Sha256 => 32,
            Digest::Sha384 => 48,
            Digest::Sha512 => 64,
        }
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn hash(&self, data: &[u8]) -> Vec<u8> {
        match self {
            Digest::Sha256 => Sha256::digest(data)[..32].to_vec(),
            Digest::Sha384 => Sha384::digest(data)[..48].to_vec(),
            Digest::Sha512 => Sha512::digest(data)[..64].to_vec(),
        }
    }

    /// DER prefix of the `DigestInfo` of PKCS #1 v1.5.
    fn digest_info(&self) -> &'static [u8] {
        match self {
            Digest::Sha256 => &[
                0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x01, 0x05, 0x00, 0x04, 0x20,
            ],
            Digest::Sha384 => &[
                0x30, 0x41, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x02, 0x05, 0x00, 0x04, 0x30,
            ],
            Digest::Sha512 => &[
                0x30, 0x51, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x03, 0x05, 0x00, 0x04, 0x40,
            ],
        }
    }
}

/// A public key: modulus and exponent, big-endian.
pub struct PublicKey {
    pub modulus: Vec<u8>,
    pub exponent: Vec<u8>,
}

impl PublicKey {
    fn size(&self) -> usize {
        let start = self.modulus.iter().position(|byte| *byte != 0).unwrap_or(self.modulus.len());
        self.modulus.len() - start
    }

    /// s^e mod n, as `size()` bytes. `None` if the key or the signature is
    /// malformed.
    fn apply(&self, signature: &[u8]) -> Option<Vec<u8>> {
        let size = self.size();
        // Below 1024 bits nothing is signed any more; above 8192 the
        // arithmetic would be slow for nothing.
        if !(128..=1024).contains(&size) || signature.len() != size {
            return None;
        }
        let limbs = size.div_ceil(8);
        let modulus = bignum::from_be(&self.modulus, limbs);
        let value = bignum::from_be(signature, limbs);
        if bignum::compare(&value, &modulus) != core::cmp::Ordering::Less {
            return None;
        }
        let mont = Mont::new(modulus)?;
        let result = mont.from(&mont.pow(&mont.to(&value), &self.exponent));
        Some(bignum::to_be(&result, size))
    }

    /// PKCS #1 v1.5: `00 01 FF.. 00 DigestInfo`.
    pub fn verify_pkcs1(&self, digest: Digest, message: &[u8], signature: &[u8]) -> bool {
        let Some(encoded) = self.apply(signature) else {
            return false;
        };
        let hash = digest.hash(message);
        let info = digest.digest_info();
        let tail = info.len() + hash.len();
        if encoded.len() < tail + 11 {
            return false;
        }
        let padding = encoded.len() - tail - 3;
        let mut expected = Vec::with_capacity(encoded.len());
        expected.extend_from_slice(&[0, 1]);
        expected.resize(2 + padding, 0xff);
        expected.push(0);
        expected.extend_from_slice(info);
        expected.extend_from_slice(&hash);
        super::equal(&expected, &encoded)
    }

    /// PSS with MGF1 over the same digest. The salt length is read from
    /// the encoding rather than imposed: TLS uses the digest length,
    /// certificates say theirs in parameters, and the encoding carries it.
    pub fn verify_pss(&self, digest: Digest, message: &[u8], signature: &[u8]) -> bool {
        let Some(encoded) = self.apply(signature) else {
            return false;
        };
        let modulus_bits = {
            let limbs = self.size().div_ceil(8);
            bignum::bits(&bignum::from_be(&self.modulus, limbs))
        };
        let em_bits = modulus_bits - 1;
        let em_len = em_bits.div_ceil(8);
        // `encoded` is `size` bytes; EM is its last `em_len`.
        let skip = encoded.len() - em_len;
        if encoded[..skip].iter().any(|byte| *byte != 0) {
            return false;
        }
        let em = &encoded[skip..];
        let h_len = digest.len();
        if em_len < h_len + 2 || em[em_len - 1] != 0xbc {
            return false;
        }
        let masked = &em[..em_len - h_len - 1];
        let h = &em[em_len - h_len - 1..em_len - 1];
        let top_bits = 8 * em_len - em_bits;
        if top_bits > 0 && masked[0] >> (8 - top_bits) != 0 {
            return false;
        }
        let mask = mgf1(digest, h, masked.len());
        let mut db: Vec<u8> = masked.iter().zip(mask.iter()).map(|(a, b)| a ^ b).collect();
        if top_bits > 0 {
            db[0] &= 0xff >> top_bits;
        }
        let Some(separator) = db.iter().position(|byte| *byte != 0) else {
            return false;
        };
        if db[separator] != 1 {
            return false;
        }
        let salt = &db[separator + 1..];
        let m_hash = digest.hash(message);
        let mut m_prime = Vec::with_capacity(8 + h_len + salt.len());
        m_prime.extend_from_slice(&[0u8; 8]);
        m_prime.extend_from_slice(&m_hash);
        m_prime.extend_from_slice(salt);
        super::equal(&digest.hash(&m_prime), h)
    }
}

fn mgf1(digest: Digest, seed: &[u8], length: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(length + 64);
    let mut counter = 0u32;
    while out.len() < length {
        let mut input = seed.to_vec();
        input.extend_from_slice(&counter.to_be_bytes());
        out.extend_from_slice(&digest.hash(&input));
        counter += 1;
    }
    out.truncate(length);
    out
}

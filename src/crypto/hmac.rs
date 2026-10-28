//! HMAC (RFC 2104) and HKDF (RFC 5869), over any hash of `sha2`.
//!
//! The whole key schedule of TLS 1.3 is HKDF: `Extract` turns a shared
//! secret into a key, `Expand-Label` derives each traffic key and IV from
//! it. HMAC also computes the `Finished` messages.

use alloc::vec::Vec;

use super::sha2::Hash;

/// A running HMAC.
#[derive(Clone)]
pub struct Hmac<H: Hash> {
    inner: H,
    outer: H,
}

impl<H: Hash> Hmac<H> {
    pub fn new(key: &[u8]) -> Self {
        let mut block = [0u8; 128];
        if key.len() > H::BLOCK {
            let digest = H::digest(key);
            block[..H::OUTPUT].copy_from_slice(&digest[..H::OUTPUT]);
        } else {
            block[..key.len()].copy_from_slice(key);
        }
        let mut inner = H::new();
        let mut outer = H::new();
        let mut pad = [0u8; 128];
        for (slot, byte) in pad.iter_mut().zip(block.iter()).take(H::BLOCK) {
            *slot = byte ^ 0x36;
        }
        inner.update(&pad[..H::BLOCK]);
        for (slot, byte) in pad.iter_mut().zip(block.iter()).take(H::BLOCK) {
            *slot = byte ^ 0x5c;
        }
        outer.update(&pad[..H::BLOCK]);
        Hmac { inner, outer }
    }

    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// The tag, `H::OUTPUT` bytes.
    pub fn finish(self) -> Vec<u8> {
        let inner = self.inner.finish();
        let mut outer = self.outer;
        outer.update(&inner[..H::OUTPUT]);
        outer.finish()[..H::OUTPUT].to_vec()
    }

    pub fn mac(key: &[u8], data: &[u8]) -> Vec<u8> {
        let mut hmac = Self::new(key);
        hmac.update(data);
        hmac.finish()
    }
}

/// HKDF-Extract: a pseudo-random key from input keying material.
pub fn extract<H: Hash>(salt: &[u8], ikm: &[u8]) -> Vec<u8> {
    let zeros = [0u8; 64];
    let salt = if salt.is_empty() { &zeros[..H::OUTPUT] } else { salt };
    Hmac::<H>::mac(salt, ikm)
}

/// HKDF-Expand: `length` bytes from a pseudo-random key.
pub fn expand<H: Hash>(prk: &[u8], info: &[u8], length: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(length);
    let mut previous: Vec<u8> = Vec::new();
    let mut counter = 1u8;
    while out.len() < length {
        let mut hmac = Hmac::<H>::new(prk);
        hmac.update(&previous);
        hmac.update(info);
        hmac.update(&[counter]);
        previous = hmac.finish();
        let take = (length - out.len()).min(previous.len());
        out.extend_from_slice(&previous[..take]);
        counter = counter.wrapping_add(1);
    }
    out
}

/// HKDF-Expand-Label of TLS 1.3 (RFC 8446, 7.1).
pub fn expand_label<H: Hash>(secret: &[u8], label: &str, context: &[u8], length: usize) -> Vec<u8> {
    let mut info = Vec::with_capacity(4 + 6 + label.len() + context.len());
    info.extend_from_slice(&(length as u16).to_be_bytes());
    info.push((6 + label.len()) as u8);
    info.extend_from_slice(b"tls13 ");
    info.extend_from_slice(label.as_bytes());
    info.push(context.len() as u8);
    info.extend_from_slice(context);
    expand::<H>(secret, &info, length)
}

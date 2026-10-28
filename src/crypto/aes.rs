//! AES-128 and AES-256 (FIPS 197), encryption only, and AES-GCM
//! (NIST SP 800-38D).
//!
//! GCM runs the block cipher in counter mode both ways, so decryption of
//! a TLS record never needs the inverse cipher -- only the forward one is
//! written.
//!
//! ## What this is not
//!
//! Table lookups indexed by secret bytes leak through the cache timing.
//! That matters to a server holding a long-term key under attack from a
//! co-tenant; a client using a fresh key per connection, on a machine that
//! runs nothing hostile beside it, is not that case. Named so that nobody
//! reuses this for the other one.

use alloc::vec::Vec;

const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

const RCON: [u8; 10] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36];

const fn xtime(byte: u8) -> u8 {
    (byte << 1) ^ if byte & 0x80 != 0 { 0x1b } else { 0 }
}

/// The "T-table" of the round function: SubBytes, ShiftRows and
/// MixColumns of one byte folded into one 32-bit word, built at compile
/// time. One lookup per byte per round instead of a dozen operations: the
/// difference between a TLS download limited by the cipher and one that
/// is not.
const fn te0() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < 256 {
        let s = SBOX[index];
        let s2 = xtime(s);
        let s3 = s2 ^ s;
        table[index] = ((s2 as u32) << 24) | ((s as u32) << 16) | ((s as u32) << 8) | s3 as u32;
        index += 1;
    }
    table
}

static TE0: [u32; 256] = te0();

#[inline(always)]
fn te(which: u32, byte: u32) -> u32 {
    TE0[(byte & 0xff) as usize].rotate_right(8 * which)
}

/// An expanded key.
#[derive(Clone)]
pub struct Aes {
    /// Round keys as big-endian words.
    words: [u32; 60],
    rounds: usize,
}

impl Aes {
    /// A 16- or 32-byte key. `None` for any other length.
    pub fn new(key: &[u8]) -> Option<Self> {
        let words_in_key = match key.len() {
            16 => 4,
            32 => 8,
            _ => return None,
        };
        let rounds = words_in_key + 6;
        let total = 4 * (rounds + 1);
        let mut words = [0u32; 60];
        for (index, word) in words.iter_mut().take(words_in_key).enumerate() {
            *word = u32::from_be_bytes([key[index * 4], key[index * 4 + 1], key[index * 4 + 2], key[index * 4 + 3]]);
        }
        let sub = |word: u32| -> u32 {
            let bytes = word.to_be_bytes();
            u32::from_be_bytes([
                SBOX[bytes[0] as usize],
                SBOX[bytes[1] as usize],
                SBOX[bytes[2] as usize],
                SBOX[bytes[3] as usize],
            ])
        };
        for index in words_in_key..total {
            let mut temp = words[index - 1];
            if index % words_in_key == 0 {
                temp = sub(temp.rotate_left(8)) ^ ((RCON[index / words_in_key - 1] as u32) << 24);
            } else if words_in_key > 6 && index % words_in_key == 4 {
                temp = sub(temp);
            }
            words[index] = words[index - words_in_key] ^ temp;
        }
        Some(Aes { words, rounds })
    }

    pub fn encrypt_block(&self, block: &mut [u8; 16]) {
        let k = &self.words;
        let load = |i: usize| u32::from_be_bytes([block[i], block[i + 1], block[i + 2], block[i + 3]]);
        let mut s0 = load(0) ^ k[0];
        let mut s1 = load(4) ^ k[1];
        let mut s2 = load(8) ^ k[2];
        let mut s3 = load(12) ^ k[3];
        for round in 1..self.rounds {
            let r = 4 * round;
            let t0 = te(0, s0 >> 24) ^ te(1, s1 >> 16) ^ te(2, s2 >> 8) ^ te(3, s3) ^ k[r];
            let t1 = te(0, s1 >> 24) ^ te(1, s2 >> 16) ^ te(2, s3 >> 8) ^ te(3, s0) ^ k[r + 1];
            let t2 = te(0, s2 >> 24) ^ te(1, s3 >> 16) ^ te(2, s0 >> 8) ^ te(3, s1) ^ k[r + 2];
            let t3 = te(0, s3 >> 24) ^ te(1, s0 >> 16) ^ te(2, s1 >> 8) ^ te(3, s2) ^ k[r + 3];
            s0 = t0;
            s1 = t1;
            s2 = t2;
            s3 = t3;
        }
        // Last round: no MixColumns.
        let r = 4 * self.rounds;
        let last = |a: u32, b: u32, c: u32, d: u32, key: u32| -> u32 {
            u32::from_be_bytes([
                SBOX[(a >> 24) as usize],
                SBOX[((b >> 16) & 0xff) as usize],
                SBOX[((c >> 8) & 0xff) as usize],
                SBOX[(d & 0xff) as usize],
            ]) ^ key
        };
        let o0 = last(s0, s1, s2, s3, k[r]);
        let o1 = last(s1, s2, s3, s0, k[r + 1]);
        let o2 = last(s2, s3, s0, s1, k[r + 2]);
        let o3 = last(s3, s0, s1, s2, k[r + 3]);
        block[0..4].copy_from_slice(&o0.to_be_bytes());
        block[4..8].copy_from_slice(&o1.to_be_bytes());
        block[8..12].copy_from_slice(&o2.to_be_bytes());
        block[12..16].copy_from_slice(&o3.to_be_bytes());
    }
}

// ============================================================
//  GCM
// ============================================================

/// Reduction constants of the 4-bit method.
const LAST4: [u64; 16] = [
    0x0000, 0x1c20, 0x3840, 0x2460, 0x7080, 0x6ca0, 0x48c0, 0x54e0, 0xe100, 0xfd20, 0xd940, 0xc560, 0x9180, 0x8da0,
    0xa9c0, 0xb5e0,
];

/// GHASH by Shoup's 4-bit tables (as in mbed TLS): sixteen multiples of H
/// precomputed per key, two table steps per byte. The bit-by-bit product
/// it replaces took 128 rounds per block and bounded a download to a
/// few megabytes a second.
#[derive(Clone)]
struct Ghash {
    high: [u64; 16],
    low: [u64; 16],
}

impl Ghash {
    fn new(h: u128) -> Ghash {
        let mut high = [0u64; 16];
        let mut low = [0u64; 16];
        let mut vh = (h >> 64) as u64;
        let mut vl = h as u64;
        high[8] = vh;
        low[8] = vl;
        let mut index = 4;
        while index > 0 {
            let t = (vl & 1) * 0xe100_0000_0000_0000;
            vl = (vh << 63) | (vl >> 1);
            vh = (vh >> 1) ^ t;
            high[index] = vh;
            low[index] = vl;
            index >>= 1;
        }
        let mut index = 2;
        while index <= 8 {
            for j in 1..index {
                high[index + j] = high[index] ^ high[j];
                low[index + j] = low[index] ^ low[j];
            }
            index *= 2;
        }
        Ghash { high, low }
    }

    /// x * H.
    fn multiply(&self, x: u128) -> u128 {
        let bytes = x.to_be_bytes();
        let nibble = (bytes[15] & 0x0f) as usize;
        let mut zh = self.high[nibble];
        let mut zl = self.low[nibble];
        for index in (0..16).rev() {
            let lo = (bytes[index] & 0x0f) as usize;
            let hi = (bytes[index] >> 4) as usize;
            if index != 15 {
                let rem = (zl & 0x0f) as usize;
                zl = (zh << 60) | (zl >> 4);
                zh = (zh >> 4) ^ (LAST4[rem] << 48);
                zh ^= self.high[lo];
                zl ^= self.low[lo];
            }
            let rem = (zl & 0x0f) as usize;
            zl = (zh << 60) | (zl >> 4);
            zh = (zh >> 4) ^ (LAST4[rem] << 48);
            zh ^= self.high[hi];
            zl ^= self.low[hi];
        }
        ((zh as u128) << 64) | zl as u128
    }

    fn hash(&self, aad: &[u8], text: &[u8]) -> u128 {
        let mut y = 0u128;
        for data in [aad, text] {
            for chunk in data.chunks(16) {
                let mut block = [0u8; 16];
                block[..chunk.len()].copy_from_slice(chunk);
                y = self.multiply(y ^ u128::from_be_bytes(block));
            }
        }
        let lengths = ((aad.len() as u128 * 8) << 64) | (text.len() as u128 * 8);
        self.multiply(y ^ lengths)
    }
}

/// AES-GCM with a 96-bit nonce and a 128-bit tag -- the only shape TLS
/// uses.
#[derive(Clone)]
pub struct AesGcm {
    cipher: Aes,
    ghash: Ghash,
}

impl AesGcm {
    pub fn new(key: &[u8]) -> Option<Self> {
        let cipher = Aes::new(key)?;
        let mut zero = [0u8; 16];
        cipher.encrypt_block(&mut zero);
        Some(AesGcm {
            cipher,
            ghash: Ghash::new(u128::from_be_bytes(zero)),
        })
    }

    fn counter_block(nonce: &[u8; 12], counter: u32) -> [u8; 16] {
        let mut block = [0u8; 16];
        block[..12].copy_from_slice(nonce);
        block[12..].copy_from_slice(&counter.to_be_bytes());
        block
    }

    fn apply_keystream(&self, nonce: &[u8; 12], data: &mut [u8]) {
        for (index, chunk) in data.chunks_mut(16).enumerate() {
            let mut stream = Self::counter_block(nonce, 2 + index as u32);
            self.cipher.encrypt_block(&mut stream);
            for (byte, key) in chunk.iter_mut().zip(stream.iter()) {
                *byte ^= key;
            }
        }
    }

    fn tag(&self, nonce: &[u8; 12], aad: &[u8], ciphertext: &[u8]) -> [u8; 16] {
        let mut mask = Self::counter_block(nonce, 1);
        self.cipher.encrypt_block(&mut mask);
        (self.ghash.hash(aad, ciphertext) ^ u128::from_be_bytes(mask)).to_be_bytes()
    }

    /// Encrypts; returns ciphertext followed by the tag.
    pub fn seal(&self, nonce: &[u8; 12], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
        let mut out = plaintext.to_vec();
        self.apply_keystream(nonce, &mut out);
        let tag = self.tag(nonce, aad, &out);
        out.extend_from_slice(&tag);
        out
    }

    /// Checks the tag and decrypts. `None` if the tag does not match.
    pub fn open(&self, nonce: &[u8; 12], aad: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
        if sealed.len() < 16 {
            return None;
        }
        let (ciphertext, tag) = sealed.split_at(sealed.len() - 16);
        let expected = self.tag(nonce, aad, ciphertext);
        if !super::equal(&expected, tag) {
            return None;
        }
        let mut out = ciphertext.to_vec();
        self.apply_keystream(nonce, &mut out);
        Some(out)
    }
}

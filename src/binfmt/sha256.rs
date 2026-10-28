//! 3.3.1 -- SHA-256, complete implementation (FIPS 180-4).
//!
//! Written by hand like the rest of the kernel: no external dependency.
//! The algorithm fits in a hundred lines or so and handles only 32-bit
//! integers, which makes it compatible with the soft-float target.
//!
//! The hash serves three distinct purposes, not to be confused:
//!
//!   * **integrity** -- `payload_hash` of the header, recomputed at
//!     load time. Detects a corruption or a later modification;
//!   * **SYSTEM identity** -- digest compared against a table frozen in
//!     the kernel (spec 3.2, "hard-coded hash verified by the kernel");
//!   * **ENTERPRISE binding** -- digest taken back by the certificate
//!     block to attach a binary to an authority.
//!
//! SHA-256 provides no **authentication** on its own: see the
//! reservations recorded in `signature.rs`.

/// Size of a digest, in bytes.
pub const DIGEST_SIZE: usize = 32;

/// Size of a compression block, in bytes.
const BLOCK_SIZE: usize = 64;

/// Cube roots of the first 64 primes, fractional part.
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Square roots of the first 8 primes, fractional part.
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// State of a computation in progress.
///
/// The API is incremental: the loader hashes an amended header then the
/// payload without ever building an intermediate buffer, which avoids
/// allocating a copy of a potentially large image.
pub struct Sha256 {
    state: [u32; 8],
    /// Partial block awaiting completion.
    buffer: [u8; BLOCK_SIZE],
    buffered: usize,
    /// Total length absorbed, in bytes.
    length: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub const fn new() -> Self {
        Sha256 {
            state: H0,
            buffer: [0u8; BLOCK_SIZE],
            buffered: 0,
            length: 0,
        }
    }

    /// Absorbs data. May be called as many times as wanted.
    pub fn update(&mut self, data: &[u8]) {
        self.length = self.length.wrapping_add(data.len() as u64);

        let mut offset = 0;

        // Completion of any partial block.
        if self.buffered > 0 {
            let take = (BLOCK_SIZE - self.buffered).min(data.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&data[..take]);
            self.buffered += take;
            offset = take;

            if self.buffered == BLOCK_SIZE {
                let block = self.buffer;
                self.compress(&block);
                self.buffered = 0;
            }
        }

        // Complete blocks, handled with no copy.
        while offset + BLOCK_SIZE <= data.len() {
            let mut block = [0u8; BLOCK_SIZE];
            block.copy_from_slice(&data[offset..offset + BLOCK_SIZE]);
            self.compress(&block);
            offset += BLOCK_SIZE;
        }

        // Remainder kept for the next call.
        let rest = data.len() - offset;
        if rest > 0 {
            self.buffer[..rest].copy_from_slice(&data[offset..]);
            self.buffered = rest;
        }
    }

    /// Adds `count` null bytes -- a shortcut to neutralise a field
    /// without building a copy of it.
    pub fn update_zeros(&mut self, count: usize) {
        const ZEROS: [u8; BLOCK_SIZE] = [0u8; BLOCK_SIZE];
        let mut remaining = count;
        while remaining > 0 {
            let take = remaining.min(BLOCK_SIZE);
            self.update(&ZEROS[..take]);
            remaining -= take;
        }
    }

    /// Ends the computation and returns the digest.
    ///
    /// FIPS 180-4 padding: one 0x80 byte, zeros, then the length of the
    /// message in **bits** on 64 big-endian bits.
    pub fn finish(mut self) -> [u8; DIGEST_SIZE] {
        let bit_length = self.length.wrapping_mul(8);

        self.update(&[0x80]);
        while self.buffered != BLOCK_SIZE - 8 {
            self.update(&[0x00]);
        }

        // The length is written straight into the block: going through
        // `update` would falsify the counter.
        self.buffer[BLOCK_SIZE - 8..].copy_from_slice(&bit_length.to_be_bytes());
        let block = self.buffer;
        self.compress(&block);

        let mut digest = [0u8; DIGEST_SIZE];
        for (i, word) in self.state.iter().enumerate() {
            digest[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        digest
    }

    /// Compression function on a 64-byte block.
    fn compress(&mut self, block: &[u8; BLOCK_SIZE]) {
        let mut w = [0u32; 64];

        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ (!e & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(choose)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(majority);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        for (slot, value) in self
            .state
            .iter_mut()
            .zip([a, b, c, d, e, f, g, h].into_iter())
        {
            *slot = slot.wrapping_add(value);
        }
    }
}

/// Digest of a single buffer.
pub fn digest(data: &[u8]) -> [u8; DIGEST_SIZE] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finish()
}

/// Constant-time comparison.
///
/// The response time must not depend on the number of bytes in common: a
/// comparison stopping at the first difference lets enough leak, through
/// its duration, to rebuild the expected digest byte by byte.
pub fn equal(a: &[u8; DIGEST_SIZE], b: &[u8; DIGEST_SIZE]) -> bool {
    let mut diff = 0u8;
    for i in 0..DIGEST_SIZE {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// Abbreviated hexadecimal representation -- the first 8 bytes are enough
/// to identify a digest in a log.
pub fn short_hex(digest: &[u8; DIGEST_SIZE]) -> [u8; 16] {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = [0u8; 16];
    for i in 0..8 {
        out[i * 2] = HEX[(digest[i] >> 4) as usize];
        out[i * 2 + 1] = HEX[(digest[i] & 0x0F) as usize];
    }
    out
}

// ============================================================
//  Self-test
// ============================================================

/// Reference vectors from FIPS 180-4 and from NESSIE.
///
/// A wrong hash implementation does not show: it produces stable,
/// normal-looking digests, simply incompatible with the rest of the
/// world. Hence these vectors, checked on every boot.
pub fn selftest() -> (usize, usize) {
    let cases: [(&[u8], [u8; 32]); 4] = [
        (
            b"",
            [
                0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
                0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
                0x78, 0x52, 0xb8, 0x55,
            ],
        ),
        (
            b"abc",
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ],
        ),
        (
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
            [
                0x24, 0x8d, 0x6a, 0x61, 0xd2, 0x06, 0x38, 0xb8, 0xe5, 0xc0, 0x26, 0x93, 0x0c, 0x3e,
                0x60, 0x39, 0xa3, 0x3c, 0xe4, 0x59, 0x64, 0xff, 0x21, 0x67, 0xf6, 0xec, 0xed, 0xd4,
                0x19, 0xdb, 0x06, 0xc1,
            ],
        ),
        (
            // 112 bytes: two full blocks plus a complete padding, the
            // case where sloppy buffer handling gives itself away.
            b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu",
            [
                0xcf, 0x5b, 0x16, 0xa7, 0x78, 0xaf, 0x83, 0x80, 0x03, 0x6c, 0xe5, 0x9e, 0x7b, 0x04,
                0x92, 0x37, 0x0b, 0x24, 0x9b, 0x11, 0xe8, 0xf0, 0x7a, 0x51, 0xaf, 0xac, 0x45, 0x03,
                0x7a, 0xfe, 0xe9, 0xd1,
            ],
        ),
    ];

    let mut passed = 0;
    let mut failed = 0;

    for (input, expected) in cases.iter() {
        let got = digest(input);
        if equal(&got, expected) {
            passed += 1;
        } else {
            failed += 1;
            crate::serial_println!(
                "[sha256] ECHEC sur un message de {} octets - obtenu {:02x?}",
                input.len(),
                &got[..8]
            );
        }
    }

    // Check of the incremental API: the splitting must change nothing in
    // the result.
    let mut hasher = Sha256::new();
    hasher.update(b"ab");
    hasher.update(b"");
    hasher.update(b"c");
    if equal(&hasher.finish(), &digest(b"abc")) {
        passed += 1;
    } else {
        failed += 1;
        crate::serial_println!("[sha256] ECHEC : l'API incrementale diverge du calcul direct");
    }

    // Check of `update_zeros`, on which the neutralisation of the digest
    // field at signature verification depends.
    let mut hasher = Sha256::new();
    hasher.update(b"x");
    hasher.update_zeros(70);
    let mut reference = Sha256::new();
    reference.update(b"x");
    reference.update(&[0u8; 70]);
    if equal(&hasher.finish(), &reference.finish()) {
        passed += 1;
    } else {
        failed += 1;
        crate::serial_println!("[sha256] ECHEC : update_zeros diverge");
    }

    crate::serial_println!(
        "[sha256] vecteurs FIPS 180-4 : {} reussites, {} echecs",
        passed,
        failed
    );

    (passed, failed)
}

//! ChaCha20, Poly1305 and their AEAD (RFC 8439).
//!
//! The AEAD is the second cipher suite of TLS 1.3 this client offers, and
//! ChaCha20 alone is the generator behind `random`: a stream cipher keyed
//! with gathered entropy is the classic construction of a deterministic
//! random generator, and it makes the generator as strong as the cipher.

use alloc::vec::Vec;

fn quarter(state: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    state[a] = state[a].wrapping_add(state[b]);
    state[d] = (state[d] ^ state[a]).rotate_left(16);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_left(12);
    state[a] = state[a].wrapping_add(state[b]);
    state[d] = (state[d] ^ state[a]).rotate_left(8);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_left(7);
}

/// One 64-byte block of key stream.
pub fn block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let mut state = [0u32; 16];
    state[0] = 0x6170_7865;
    state[1] = 0x3320_646e;
    state[2] = 0x7962_2d32;
    state[3] = 0x6b20_6574;
    for index in 0..8 {
        state[4 + index] = u32::from_le_bytes([
            key[index * 4],
            key[index * 4 + 1],
            key[index * 4 + 2],
            key[index * 4 + 3],
        ]);
    }
    state[12] = counter;
    for index in 0..3 {
        state[13 + index] = u32::from_le_bytes([
            nonce[index * 4],
            nonce[index * 4 + 1],
            nonce[index * 4 + 2],
            nonce[index * 4 + 3],
        ]);
    }
    let initial = state;
    for _ in 0..10 {
        quarter(&mut state, 0, 4, 8, 12);
        quarter(&mut state, 1, 5, 9, 13);
        quarter(&mut state, 2, 6, 10, 14);
        quarter(&mut state, 3, 7, 11, 15);
        quarter(&mut state, 0, 5, 10, 15);
        quarter(&mut state, 1, 6, 11, 12);
        quarter(&mut state, 2, 7, 8, 13);
        quarter(&mut state, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for index in 0..16 {
        out[index * 4..index * 4 + 4]
            .copy_from_slice(&state[index].wrapping_add(initial[index]).to_le_bytes());
    }
    out
}

/// XORs the key stream starting at block `counter` into `data`.
pub fn apply(key: &[u8; 32], counter: u32, nonce: &[u8; 12], data: &mut [u8]) {
    for (index, chunk) in data.chunks_mut(64).enumerate() {
        let stream = block(key, counter.wrapping_add(index as u32), nonce);
        for (byte, key) in chunk.iter_mut().zip(stream.iter()) {
            *byte ^= key;
        }
    }
}

/// Poly1305 one-time authenticator, with 26-bit limbs.
pub fn poly1305(key: &[u8; 32], message: &[u8]) -> [u8; 16] {
    let load = |bytes: &[u8]| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let r0 = load(&key[0..]) & 0x3ff_ffff;
    let r1 = (load(&key[3..]) >> 2) & 0x3ff_ff03;
    let r2 = (load(&key[6..]) >> 4) & 0x3ff_c0ff;
    let r3 = (load(&key[9..]) >> 6) & 0x3f0_3fff;
    let r4 = (load(&key[12..]) >> 8) & 0x00f_ffff;
    let (s1, s2, s3, s4) = (r1 * 5, r2 * 5, r3 * 5, r4 * 5);
    let (mut h0, mut h1, mut h2, mut h3, mut h4) = (0u32, 0u32, 0u32, 0u32, 0u32);

    for chunk in message.chunks(16) {
        let mut block = [0u8; 17];
        block[..chunk.len()].copy_from_slice(chunk);
        block[chunk.len()] = 1;
        h0 += load(&block[0..]) & 0x3ff_ffff;
        h1 += (load(&block[3..]) >> 2) & 0x3ff_ffff;
        h2 += (load(&block[6..]) >> 4) & 0x3ff_ffff;
        h3 += (load(&block[9..]) >> 6) & 0x3ff_ffff;
        h4 += (load(&block[12..]) >> 8) | ((block[16] as u32) << 24);

        let d0 = h0 as u64 * r0 as u64
            + h1 as u64 * s4 as u64
            + h2 as u64 * s3 as u64
            + h3 as u64 * s2 as u64
            + h4 as u64 * s1 as u64;
        let mut d1 = h0 as u64 * r1 as u64
            + h1 as u64 * r0 as u64
            + h2 as u64 * s4 as u64
            + h3 as u64 * s3 as u64
            + h4 as u64 * s2 as u64;
        let mut d2 = h0 as u64 * r2 as u64
            + h1 as u64 * r1 as u64
            + h2 as u64 * r0 as u64
            + h3 as u64 * s4 as u64
            + h4 as u64 * s3 as u64;
        let mut d3 = h0 as u64 * r3 as u64
            + h1 as u64 * r2 as u64
            + h2 as u64 * r1 as u64
            + h3 as u64 * r0 as u64
            + h4 as u64 * s4 as u64;
        let mut d4 = h0 as u64 * r4 as u64
            + h1 as u64 * r3 as u64
            + h2 as u64 * r2 as u64
            + h3 as u64 * r1 as u64
            + h4 as u64 * r0 as u64;

        let mut carry = d0 >> 26;
        h0 = (d0 & 0x3ff_ffff) as u32;
        d1 += carry;
        carry = d1 >> 26;
        h1 = (d1 & 0x3ff_ffff) as u32;
        d2 += carry;
        carry = d2 >> 26;
        h2 = (d2 & 0x3ff_ffff) as u32;
        d3 += carry;
        carry = d3 >> 26;
        h3 = (d3 & 0x3ff_ffff) as u32;
        d4 += carry;
        carry = d4 >> 26;
        h4 = (d4 & 0x3ff_ffff) as u32;
        // In 64 bits: the carry out of d4 can exceed 2^30, and five times
        // that does not fit a u32.
        let folded = h0 as u64 + carry * 5;
        h0 = (folded & 0x3ff_ffff) as u32;
        h1 += (folded >> 26) as u32;
    }

    // Full carry, then h - p, kept if it did not go negative.
    let mut carry = h1 >> 26;
    h1 &= 0x3ff_ffff;
    h2 += carry;
    carry = h2 >> 26;
    h2 &= 0x3ff_ffff;
    h3 += carry;
    carry = h3 >> 26;
    h3 &= 0x3ff_ffff;
    h4 += carry;
    carry = h4 >> 26;
    h4 &= 0x3ff_ffff;
    h0 += carry * 5;
    carry = h0 >> 26;
    h0 &= 0x3ff_ffff;
    h1 += carry;

    let mut g0 = h0.wrapping_add(5);
    carry = g0 >> 26;
    g0 &= 0x3ff_ffff;
    let mut g1 = h1.wrapping_add(carry);
    carry = g1 >> 26;
    g1 &= 0x3ff_ffff;
    let mut g2 = h2.wrapping_add(carry);
    carry = g2 >> 26;
    g2 &= 0x3ff_ffff;
    let mut g3 = h3.wrapping_add(carry);
    carry = g3 >> 26;
    g3 &= 0x3ff_ffff;
    let g4 = h4.wrapping_add(carry).wrapping_sub(1 << 26);

    let mask = (g4 >> 31).wrapping_sub(1);
    let (g0, g1, g2, g3, g4) = (g0 & mask, g1 & mask, g2 & mask, g3 & mask, g4 & mask);
    let keep = !mask;
    h0 = (h0 & keep) | g0;
    h1 = (h1 & keep) | g1;
    h2 = (h2 & keep) | g2;
    h3 = (h3 & keep) | g3;
    h4 = (h4 & keep) | g4;

    let h0 = h0 | (h1 << 26);
    let h1 = (h1 >> 6) | (h2 << 20);
    let h2 = (h2 >> 12) | (h3 << 14);
    let h3 = (h3 >> 18) | (h4 << 8);

    let mut f = h0 as u64 + load(&key[16..]) as u64;
    let o0 = f as u32;
    f = h1 as u64 + load(&key[20..]) as u64 + (f >> 32);
    let o1 = f as u32;
    f = h2 as u64 + load(&key[24..]) as u64 + (f >> 32);
    let o2 = f as u32;
    f = h3 as u64 + load(&key[28..]) as u64 + (f >> 32);
    let o3 = f as u32;

    let mut tag = [0u8; 16];
    tag[0..4].copy_from_slice(&o0.to_le_bytes());
    tag[4..8].copy_from_slice(&o1.to_le_bytes());
    tag[8..12].copy_from_slice(&o2.to_le_bytes());
    tag[12..16].copy_from_slice(&o3.to_le_bytes());
    tag
}

fn aead_tag(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], ciphertext: &[u8]) -> [u8; 16] {
    let first = block(key, 0, nonce);
    let mut one_time = [0u8; 32];
    one_time.copy_from_slice(&first[..32]);
    let mut data = Vec::with_capacity(aad.len() + ciphertext.len() + 32);
    data.extend_from_slice(aad);
    data.resize(aad.len().div_ceil(16) * 16, 0);
    data.extend_from_slice(ciphertext);
    data.resize(data.len().div_ceil(16) * 16, 0);
    data.extend_from_slice(&(aad.len() as u64).to_le_bytes());
    data.extend_from_slice(&(ciphertext.len() as u64).to_le_bytes());
    poly1305(&one_time, &data)
}

/// ChaCha20-Poly1305 seal: ciphertext followed by the tag.
pub fn seal(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let mut out = plaintext.to_vec();
    apply(key, 1, nonce, &mut out);
    let tag = aead_tag(key, nonce, aad, &out);
    out.extend_from_slice(&tag);
    out
}

/// ChaCha20-Poly1305 open. `None` if the tag does not match.
pub fn open(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < 16 {
        return None;
    }
    let (ciphertext, tag) = sealed.split_at(sealed.len() - 16);
    if !super::equal(&aead_tag(key, nonce, aad, ciphertext), tag) {
        return None;
    }
    let mut out = ciphertext.to_vec();
    apply(key, 1, nonce, &mut out);
    Some(out)
}

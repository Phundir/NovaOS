//! `_nova`: the byte-level primitives the Python library modules are
//! written over (hashlib, zlib, binascii, struct): message digests,
//! checksums, DEFLATE, and the bit patterns of IEEE 754 formats.
//!
//! Kept in Rust because they are loops over bytes that the interpreter
//! would run a hundred times slower, and kept self-contained (no use of
//! the kernel's crypto module) so the host build compiles the same file.
//! The DEFLATE compressor is a plain LZ77 with fixed Huffman codes: its
//! output is valid zlib, but not byte for byte the zlib library's.

use alloc::rc::Rc;
use alloc::vec::Vec;

use super::super::int::Int;
use super::super::types::bytes::bytes_of;
use super::super::value::*;
use super::super::vm::{PyResult, Vm};
use crate::web::js::num::Num;

fn data(vm: &mut Vm, a: &[Value], i: usize) -> PyResult<Vec<u8>> {
    match a.get(i).and_then(bytes_of) {
        Some(b) => Ok(b),
        None => {
            let name = a.get(i).map(|v| vm.type_name(v)).unwrap_or_else(|| Rc::from("NoneType"));
            let message = alloc::format!("a bytes-like object is required, not '{}'", name);
            vm.type_error(&message)
        }
    }
}

fn bytes_value(b: Vec<u8>) -> Value {
    Value::Bytes(Rc::from(b.as_slice()))
}

fn int_arg(vm: &mut Vm, a: &[Value], i: usize, default: i64) -> PyResult<Int> {
    match a.get(i) {
        None | Some(Value::None) => Ok(Int::Small(default)),
        Some(Value::Int(n)) => Ok(n.clone()),
        Some(Value::Bool(b)) => Ok(Int::Small(*b as i64)),
        Some(other) => {
            let name = vm.type_name(other);
            let message = alloc::format!("'{}' object cannot be interpreted as an integer", name);
            vm.type_error(&message)
        }
    }
}

fn u64_of(n: &Int) -> u64 {
    // The low 64 bits, two's complement for negatives.
    let mask = Int::from_u128(u64::MAX as u128);
    let low = n.bitop(&mask, b'&');
    match low.to_i64() {
        Some(v) => v as u64,
        None => {
            let high = low.shr(32).to_i64().unwrap_or(0) as u64;
            let lowest = low.bitop(&Int::Small(0xffff_ffff), b'&').to_i64().unwrap_or(0) as u64;
            (high << 32) | lowest
        }
    }
}

// ------------------------------------------------------------
//  Digests
// ------------------------------------------------------------

fn md5(input: &[u8]) -> Vec<u8> {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
        4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let mut k = [0u32; 64];
    // K[i] = floor(|sin(i + 1)| * 2^32), tabulated.
    const KT: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501, 0x698098d8, 0x8b44f7af,
        0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa,
        0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8,
        0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
        0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244, 0x432aff97,
        0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1, 0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1,
        0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
    ];
    k.copy_from_slice(&KT);
    let mut h: [u32; 4] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476];
    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());
    for chunk in msg.chunks(64) {
        let mut m = [0u32; 16];
        for (i, w) in m.iter_mut().enumerate() {
            *w = u32::from_le_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        let (mut a, mut b, mut c, mut d) = (h[0], h[1], h[2], h[3]);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
    }
    h.iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn pad_be(input: &[u8], block: usize) -> Vec<u8> {
    let mut msg = input.to_vec();
    msg.push(0x80);
    let length_bytes = if block == 64 { 8 } else { 16 };
    while msg.len() % block != block - length_bytes {
        msg.push(0);
    }
    let bits = (input.len() as u128).wrapping_mul(8);
    if block == 64 {
        msg.extend_from_slice(&(bits as u64).to_be_bytes());
    } else {
        msg.extend_from_slice(&bits.to_be_bytes());
    }
    msg
}

fn sha1(input: &[u8]) -> Vec<u8> {
    let mut h: [u32; 5] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];
    for chunk in pad_be(input, 64).chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i / 20 {
                0 => ((b & c) | (!b & d), 0x5a827999),
                1 => (b ^ c ^ d, 0x6ed9eba1),
                2 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
                _ => (b ^ c ^ d, 0xca62c1d6u32),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    h.iter().flat_map(|w| w.to_be_bytes()).collect()
}

const K256: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01,
    0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
    0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
    0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08,
    0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256(input: &[u8], small: bool) -> Vec<u8> {
    let mut h: [u32; 8] = if small {
        [0xc1059ed8, 0x367cd507, 0x3070dd17, 0xf70e5939, 0xffc00b31, 0x68581511, 0x64f98fa7, 0xbefa4fa4]
    } else {
        [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19]
    };
    for chunk in pad_be(input, 64).chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K256[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    let out: Vec<u8> = h.iter().flat_map(|w| w.to_be_bytes()).collect();
    if small { out[..28].to_vec() } else { out }
}

const K512: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc, 0x3956c25bf348b538, 0x59f111f1b605d019,
    0x923f82a4af194f9b, 0xab1c5ed5da6d8118, 0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694, 0xe49b69c19ef14ad2, 0xefbe4786384f25e3,
    0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65, 0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
    0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4, 0xc6e00bf33da88fc2, 0xd5a79147930aa725,
    0x06ca6351e003826f, 0x142929670a0e6e70, 0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
    0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b, 0xa2bfe8a14cf10364, 0xa81a664bbc423001,
    0xc24b8b70d0f89791, 0xc76c51a30654be30, 0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8, 0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb,
    0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3, 0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b, 0xca273eceea26619c, 0xd186b8c721c0c207,
    0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178, 0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
    0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c, 0x4cc5d4becb3e42b6, 0x597f299cfc657e2a,
    0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

fn sha512(input: &[u8], small: bool) -> Vec<u8> {
    let mut h: [u64; 8] = if small {
        [
            0xcbbb9d5dc1059ed8, 0x629a292a367cd507, 0x9159015a3070dd17, 0x152fecd8f70e5939, 0x67332667ffc00b31,
            0x8eb44a8768581511, 0xdb0c2e0d64f98fa7, 0x47b5481dbefa4fa4,
        ]
    } else {
        [
            0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1, 0x510e527fade682d1,
            0x9b05688c2b3e6c1f, 0x1f83d9abfb41bd6b, 0x5be0cd19137e2179,
        ]
    };
    for chunk in pad_be(input, 128).chunks(128) {
        let mut w = [0u64; 80];
        for i in 0..16 {
            let mut b = [0u8; 8];
            b.copy_from_slice(&chunk[8 * i..8 * i + 8]);
            w[i] = u64::from_be_bytes(b);
        }
        for i in 16..80 {
            let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
            let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..80 {
            let s1 = v[4].rotate_right(14) ^ v[4].rotate_right(18) ^ v[4].rotate_right(41);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K512[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(28) ^ v[0].rotate_right(34) ^ v[0].rotate_right(39);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    let out: Vec<u8> = h.iter().flat_map(|w| w.to_be_bytes()).collect();
    if small { out[..48].to_vec() } else { out }
}

/// The digest of `data` by the algorithm `name`, or None.
pub fn hash(name: &str, data: &[u8]) -> Option<Vec<u8>> {
    Some(match name {
        "md5" => md5(data),
        "sha1" => sha1(data),
        "sha224" => sha256(data, true),
        "sha256" => sha256(data, false),
        "sha384" => sha512(data, true),
        "sha512" => sha512(data, false),
        _ => return None,
    })
}

fn digest(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let name = match a.first() {
        Some(Value::Str(s)) => s.clone(),
        _ => return vm.type_error("digest name must be a string"),
    };
    let input = data(vm, a, 1)?;
    match hash(&name, &input) {
        Some(d) => Ok(bytes_value(d)),
        None => {
            let message = alloc::format!("unsupported hash type {}", name);
            vm.value_error(&message)
        }
    }
}

// ------------------------------------------------------------
//  Checksums
// ------------------------------------------------------------

pub fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    crc = !crc;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn adler32_update(value: u32, data: &[u8]) -> u32 {
    let (mut a, mut b) = (value & 0xffff, value >> 16);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

fn crc32(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let input = data(vm, a, 0)?;
    let start = int_arg(vm, a, 1, 0)?;
    Ok(Value::int(crc32_update(u64_of(&start) as u32, &input) as i64))
}

fn adler32(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let input = data(vm, a, 0)?;
    let start = int_arg(vm, a, 1, 1)?;
    Ok(Value::int(adler32_update(u64_of(&start) as u32, &input) as i64))
}

// ------------------------------------------------------------
//  DEFLATE
// ------------------------------------------------------------

struct BitWriter {
    out: Vec<u8>,
    buffer: u64,
    count: u32,
}

impl BitWriter {
    fn put(&mut self, value: u32, bits: u32) {
        self.buffer |= (value as u64) << self.count;
        self.count += bits;
        while self.count >= 8 {
            self.out.push(self.buffer as u8);
            self.buffer >>= 8;
            self.count -= 8;
        }
    }

    /// A Huffman code, most significant bit first.
    fn code(&mut self, code: u32, bits: u32) {
        let mut reversed = 0;
        for i in 0..bits {
            reversed |= ((code >> i) & 1) << (bits - 1 - i);
        }
        self.put(reversed, bits);
    }

    fn flush(&mut self) {
        if self.count > 0 {
            self.out.push(self.buffer as u8);
        }
        self.buffer = 0;
        self.count = 0;
    }
}

fn fixed_literal(w: &mut BitWriter, symbol: u32) {
    match symbol {
        0..=143 => w.code(0x30 + symbol, 8),
        144..=255 => w.code(0x190 + symbol - 144, 9),
        256..=279 => w.code(symbol - 256, 7),
        _ => w.code(0xc0 + symbol - 280, 8),
    }
}

const LENGTH_BASE: [u32; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LENGTH_EXTRA: [u32; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u32; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u32; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

/// Raw DEFLATE: stored blocks at level 0, one fixed-Huffman block with
/// LZ77 matches (hash chains over a 32 KiB window) otherwise.
pub fn deflate(input: &[u8], level: i64) -> Vec<u8> {
    let mut w = BitWriter { out: Vec::new(), buffer: 0, count: 0 };
    if level == 0 {
        let mut chunks = input.chunks(65535).peekable();
        if input.is_empty() {
            w.out.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
            return w.out;
        }
        while let Some(chunk) = chunks.next() {
            let last = chunks.peek().is_none();
            w.out.push(last as u8);
            let n = chunk.len() as u16;
            w.out.extend_from_slice(&n.to_le_bytes());
            w.out.extend_from_slice(&(!n).to_le_bytes());
            w.out.extend_from_slice(chunk);
        }
        return w.out;
    }
    let chain_limit = match level {
        1..=3 => 8,
        4..=6 => 64,
        _ => 512,
    };
    w.put(1, 1);
    w.put(1, 2);
    const HASH: usize = 1 << 15;
    let mut head = alloc::vec![usize::MAX; HASH];
    let mut prev = alloc::vec![usize::MAX; input.len()];
    let key = |i: usize| -> usize { ((input[i] as usize) << 10 ^ (input[i + 1] as usize) << 5 ^ input[i + 2] as usize) & (HASH - 1) };
    let mut i = 0;
    let n = input.len();
    while i < n {
        let mut best_len = 0;
        let mut best_dist = 0;
        if i + 2 < n {
            let h = key(i);
            let mut candidate = head[h];
            let mut chain = 0;
            while candidate != usize::MAX && i - candidate <= 32768 && chain < chain_limit {
                let mut l = 0;
                while l < 258 && i + l < n && input[candidate + l] == input[i + l] {
                    l += 1;
                }
                if l > best_len {
                    best_len = l;
                    best_dist = i - candidate;
                    if l == 258 {
                        break;
                    }
                }
                candidate = prev[candidate];
                chain += 1;
            }
            prev[i] = head[h];
            head[h] = i;
        }
        if best_len >= 3 {
            let li = LENGTH_BASE.iter().rposition(|&b| b <= best_len as u32).unwrap_or(0);
            fixed_literal(&mut w, 257 + li as u32);
            w.put(best_len as u32 - LENGTH_BASE[li], LENGTH_EXTRA[li]);
            let di = DIST_BASE.iter().rposition(|&b| b <= best_dist as u32).unwrap_or(0);
            w.code(di as u32, 5);
            w.put(best_dist as u32 - DIST_BASE[di], DIST_EXTRA[di]);
            for j in i + 1..i + best_len {
                if j + 2 < n {
                    let h = key(j);
                    prev[j] = head[h];
                    head[h] = j;
                }
            }
            i += best_len;
        } else {
            fixed_literal(&mut w, input[i] as u32);
            i += 1;
        }
    }
    fixed_literal(&mut w, 256);
    w.flush();
    w.out
}

fn deflate_native(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let input = data(vm, a, 0)?;
    let level = int_arg(vm, a, 1, 6)?.to_i64().unwrap_or(6);
    Ok(bytes_value(deflate(&input, level)))
}

/// `inflate(data, mode)`: mode 0 raw, 1 zlib, 2 gzip; None if corrupt.
fn inflate_native(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let input = data(vm, a, 0)?;
    let mode = int_arg(vm, a, 1, 1)?.to_i64().unwrap_or(1);
    const LIMIT: usize = 1 << 30;
    let out = match mode {
        0 => crate::web::inflate::inflate(&input, LIMIT),
        2 => crate::web::inflate::gunzip(&input, LIMIT),
        _ => crate::web::inflate::unzlib(&input, LIMIT),
    };
    Ok(match out {
        Some(b) => bytes_value(b),
        None => Value::None,
    })
}

// ------------------------------------------------------------
//  IEEE 754 bit patterns (struct's 'e', 'f', 'd')
// ------------------------------------------------------------

/// The bits of `x` in a binary format of `ebits` exponent bits and
/// `mbits` fraction bits, rounded to nearest even; None on overflow.
fn to_format(x: Num, ebits: u32, mbits: u32) -> Option<u64> {
    let sign = (x.0 >> 63) << (ebits + mbits);
    let all_ones = (1u64 << ebits) - 1;
    if x.is_nan() {
        return Some(sign | all_ones << mbits | 1 << (mbits - 1));
    }
    if x.is_infinite() {
        return Some(sign | all_ones << mbits);
    }
    if x.is_zero() {
        return Some(sign);
    }
    let (_, m, e) = x.decompose();
    let bias = (1i32 << (ebits - 1)) - 1;
    let emin = 1 - bias;
    let len = 64 - m.leading_zeros() as i32;
    let exponent = e + len - 1;
    let shift = if exponent >= emin { len - (mbits as i32 + 1) } else { (emin - mbits as i32) - e };
    let (mut q, mut qe) = if shift > 0 {
        if shift >= 64 {
            (0u64, e + shift)
        } else {
            let q = m >> shift;
            let rest = m & ((1u64 << shift) - 1);
            let half = 1u64 << (shift - 1);
            let up = rest > half || (rest == half && q & 1 == 1);
            (q + up as u64, e + shift)
        }
    } else {
        (m << (-shift), e + shift)
    };
    if q == 1u64 << (mbits + 1) {
        q >>= 1;
        qe += 1;
    }
    if q >= 1u64 << mbits {
        let biased = qe + mbits as i32 + bias;
        if biased >= all_ones as i32 {
            return None;
        }
        Some(sign | (biased as u64) << mbits | (q - (1u64 << mbits)))
    } else {
        Some(sign | q)
    }
}

fn from_format(bits: u64, ebits: u32, mbits: u32) -> Num {
    let negative = bits >> (ebits + mbits) & 1 == 1;
    let field = (bits >> mbits) & ((1 << ebits) - 1);
    let fraction = bits & ((1 << mbits) - 1);
    let bias = (1i32 << (ebits - 1)) - 1;
    let r = if field == (1 << ebits) - 1 {
        if fraction != 0 { Num::NAN } else { Num::INFINITY }
    } else if field == 0 {
        Num::from_u64(fraction).scale2(1 - bias - mbits as i32)
    } else {
        Num::from_u64(fraction | 1 << mbits).scale2(field as i32 - bias - mbits as i32)
    };
    if negative { r.neg() } else { r }
}

fn float_arg(vm: &mut Vm, a: &[Value]) -> PyResult<Num> {
    match a.first() {
        Some(Value::Float(f)) => Ok(*f),
        Some(v @ (Value::Int(_) | Value::Bool(_) | Value::Instance(_))) => super::super::types::num::float_of(vm, v),
        _ => vm.type_error("required argument is not a float"),
    }
}

fn pack_float(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = float_arg(vm, a)?;
    let size = int_arg(vm, &a[1..], 0, 8)?.to_i64().unwrap_or(8);
    let (ebits, mbits, letter) = match size {
        2 => (5, 10, 'e'),
        4 => (8, 23, 'f'),
        _ => return Ok(Value::Int(Int::from_u128(x.0 as u128))),
    };
    match to_format(x, ebits, mbits) {
        Some(bits) => Ok(Value::int(bits as i64)),
        None => {
            let message = alloc::format!("float too large to pack with {} format", letter);
            vm.overflow(&message)
        }
    }
}

fn unpack_float(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let bits = u64_of(&int_arg(vm, a, 0, 0)?);
    let size = int_arg(vm, a, 1, 8)?.to_i64().unwrap_or(8);
    Ok(Value::Float(match size {
        2 => from_format(bits, 5, 10),
        4 => from_format(bits, 8, 23),
        _ => Num(bits),
    }))
}

pub fn module(vm: &mut Vm) -> Rc<Module> {
    let functions: &[(&'static str, NativeFn)] = &[
        ("digest", digest), ("crc32", crc32), ("adler32", adler32), ("deflate", deflate_native), ("inflate", inflate_native),
        ("pack_float", pack_float), ("unpack_float", unpack_float),
    ];
    super::native_module(vm, "_nova", functions)
}

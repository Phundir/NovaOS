//! DEFLATE decompression (RFC 1951), with the gzip (RFC 1952) and zlib
//! (RFC 1950) wrappers.
//!
//! Asked for in every request: an HTML page shrinks five to ten times
//! compressed, and the TCP of this kernel is slow -- one small window, no
//! fast retransmission. Decompressing costs less than waiting.
//!
//! The output is bounded (`limit`): a small compressed body can expand
//! a thousandfold, and a page must not be able to exhaust the heap.

use alloc::vec::Vec;

struct Bits<'a> {
    data: &'a [u8],
    position: usize,
    buffer: u64,
    count: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits {
            data,
            position: 0,
            buffer: 0,
            count: 0,
        }
    }

    fn need(&mut self, bits: u32) -> Option<()> {
        while self.count < bits {
            let byte = *self.data.get(self.position)?;
            self.position += 1;
            self.buffer |= (byte as u64) << self.count;
            self.count += 8;
        }
        Some(())
    }

    fn take(&mut self, bits: u32) -> Option<u32> {
        if bits == 0 {
            return Some(0);
        }
        self.need(bits)?;
        let value = (self.buffer & ((1u64 << bits) - 1)) as u32;
        self.buffer >>= bits;
        self.count -= bits;
        Some(value)
    }

    fn align(&mut self) {
        let drop = self.count % 8;
        self.buffer >>= drop;
        self.count -= drop;
    }
}

/// A canonical Huffman code, decoded by counting (as in zlib's `puff`).
struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Option<Huffman> {
        let mut counts = [0u16; 16];
        for length in lengths {
            counts[*length as usize] += 1;
        }
        counts[0] = 0;
        let mut offsets = [0u16; 16];
        for index in 1..15 {
            offsets[index + 1] = offsets[index] + counts[index];
        }
        let mut symbols = alloc::vec![0u16; lengths.len()];
        for (symbol, length) in lengths.iter().enumerate() {
            if *length != 0 {
                symbols[offsets[*length as usize] as usize] = symbol as u16;
                offsets[*length as usize] += 1;
            }
        }
        Some(Huffman { counts, symbols })
    }

    fn decode(&self, bits: &mut Bits) -> Option<u16> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for length in 1..16 {
            code |= bits.take(1)? as i32;
            let count = self.counts[length] as i32;
            if code - count < first {
                return self.symbols.get((index + (code - first)) as usize).copied();
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        None
    }
}

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195,
    227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DISTANCE_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073,
    4097, 6145, 8193, 12289, 16385, 24577,
];
const DISTANCE_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13,
];

fn codes(bits: &mut Bits, out: &mut Vec<u8>, literal: &Huffman, distance: &Huffman, limit: usize) -> Option<()> {
    loop {
        let symbol = literal.decode(bits)?;
        match symbol {
            0..=255 => {
                if out.len() >= limit {
                    return None;
                }
                out.push(symbol as u8);
            }
            256 => return Some(()),
            257..=285 => {
                let index = (symbol - 257) as usize;
                let length = LENGTH_BASE[index] as usize + bits.take(LENGTH_EXTRA[index] as u32)? as usize;
                let code = distance.decode(bits)? as usize;
                if code >= 30 {
                    return None;
                }
                let back = DISTANCE_BASE[code] as usize + bits.take(DISTANCE_EXTRA[code] as u32)? as usize;
                if back > out.len() || out.len() + length > limit {
                    return None;
                }
                let start = out.len() - back;
                for offset in 0..length {
                    let byte = out[start + offset];
                    out.push(byte);
                }
            }
            _ => return None,
        }
    }
}

/// Raw DEFLATE.
pub fn inflate(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    let mut bits = Bits::new(data);
    let mut out = Vec::new();
    loop {
        let last = bits.take(1)?;
        match bits.take(2)? {
            0 => {
                bits.align();
                let length = bits.take(16)? as usize;
                let complement = bits.take(16)? as usize;
                if length != !complement & 0xffff || out.len() + length > limit {
                    return None;
                }
                for _ in 0..length {
                    out.push(bits.take(8)? as u8);
                }
            }
            1 => {
                let mut lengths = [0u8; 288];
                lengths[..144].fill(8);
                lengths[144..256].fill(9);
                lengths[256..280].fill(7);
                lengths[280..].fill(8);
                let literal = Huffman::new(&lengths)?;
                let distance = Huffman::new(&[5u8; 30])?;
                codes(&mut bits, &mut out, &literal, &distance, limit)?;
            }
            2 => {
                let literals = bits.take(5)? as usize + 257;
                let distances = bits.take(5)? as usize + 1;
                let code_lengths = bits.take(4)? as usize + 4;
                const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
                let mut lengths = [0u8; 19];
                for slot in ORDER.iter().take(code_lengths) {
                    lengths[*slot] = bits.take(3)? as u8;
                }
                let lencode = Huffman::new(&lengths)?;
                let mut all = alloc::vec![0u8; literals + distances];
                let mut index = 0;
                while index < literals + distances {
                    let symbol = lencode.decode(&mut bits)?;
                    let (value, repeat) = match symbol {
                        0..=15 => (symbol as u8, 1),
                        16 => {
                            if index == 0 {
                                return None;
                            }
                            (all[index - 1], 3 + bits.take(2)? as usize)
                        }
                        17 => (0, 3 + bits.take(3)? as usize),
                        18 => (0, 11 + bits.take(7)? as usize),
                        _ => return None,
                    };
                    if index + repeat > all.len() {
                        return None;
                    }
                    all[index..index + repeat].fill(value);
                    index += repeat;
                }
                let literal = Huffman::new(&all[..literals])?;
                let distance = Huffman::new(&all[literals..])?;
                codes(&mut bits, &mut out, &literal, &distance, limit)?;
            }
            _ => return None,
        }
        if last == 1 {
            return Some(out);
        }
    }
}

/// A gzip member.
pub fn gunzip(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    if data.len() < 18 || data[0] != 0x1f || data[1] != 0x8b || data[2] != 8 {
        return None;
    }
    let flags = data[3];
    let mut offset = 10;
    if flags & 4 != 0 {
        let extra = *data.get(offset)? as usize | (*data.get(offset + 1)? as usize) << 8;
        offset += 2 + extra;
    }
    for flag in [8u8, 16] {
        if flags & flag != 0 {
            while *data.get(offset)? != 0 {
                offset += 1;
            }
            offset += 1;
        }
    }
    if flags & 2 != 0 {
        offset += 2;
    }
    inflate(data.get(offset..)?, limit)
}

/// A zlib stream -- what HTTP calls `deflate`, though some servers send
/// raw DEFLATE under that name; both are accepted.
pub fn unzlib(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    if data.len() >= 2 && (data[0] & 0x0f) == 8 && ((data[0] as u16) << 8 | data[1] as u16) % 31 == 0 {
        return inflate(&data[2..], limit);
    }
    inflate(data, limit)
}

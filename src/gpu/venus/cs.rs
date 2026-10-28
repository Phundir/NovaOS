//! The two primitives of a Venus stream: writing it, and reading a reply.
//!
//! Everything in a stream is little-endian and aligned on four bytes
//! (VK_EXT_command_serialization): a 32-bit value takes one word, a 64-bit
//! value two, a byte array its length rounded up to a word. `protocol.rs`,
//! generated from the Vulkan registry, calls nothing but these.
//!
//! ## Errors
//!
//! Neither side panics. The encoder notes an array handed shorter than the
//! count it claims -- the host would read past it into the next command
//! -- and the stream is then refused whole before it leaves (`error`). The
//! decoder reads zeros past the end of a reply and notes it: a reply is
//! host memory, and a truncated one must be refused, not trusted.

use alloc::vec::Vec;

/// A stream being written.
pub struct Enc {
    buf: Vec<u8>,
    error: bool,
}

impl Default for Enc {
    fn default() -> Self {
        Self::new()
    }
}

impl Enc {
    pub fn new() -> Enc {
        Enc { buf: Vec::with_capacity(1024), error: false }
    }

    pub fn u32(&mut self, value: u32) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    pub fn i32(&mut self, value: i32) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    pub fn u64(&mut self, value: u64) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    pub fn i64(&mut self, value: i64) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    /// The count in front of every array, string and blob.
    pub fn array_size(&mut self, count: u64) {
        self.u64(count);
    }

    /// The count of an array whose elements come from a slice of
    /// `available` elements: a slice shorter than the count is an error.
    pub fn array_items(&mut self, count: u64, available: usize) {
        if count > available as u64 {
            self.error = true;
        }
        self.array_size(count);
    }

    /// `count` bytes of `data`, padded to a word. The count was written by
    /// the caller.
    pub fn blob(&mut self, data: &[u8], count: u64) {
        let count = count as usize;
        if count > data.len() {
            self.error = true;
        }
        let taken = count.min(data.len());
        self.buf.extend_from_slice(&data[..taken]);
        let padded = (count + 3) & !3;
        self.buf.resize(self.buf.len() + (padded - taken), 0);
    }

    /// A C string: its length with the terminator, then the bytes and the
    /// terminator, padded. A null pointer is an empty array.
    pub fn string(&mut self, value: Option<&str>) {
        match value {
            None => self.array_size(0),
            Some(text) => {
                let bytes = text.as_bytes();
                // A string may not carry its own terminator: the host would
                // cut it there and read the rest as the next field.
                if bytes.contains(&0) {
                    self.error = true;
                }
                let size = bytes.len() as u64 + 1;
                self.array_size(size);
                let start = self.buf.len();
                self.buf.extend_from_slice(bytes);
                self.buf.push(0);
                let padded = (size as usize + 3) & !3;
                self.buf.resize(start + padded, 0);
            }
        }
    }

    /// A fixed character array of an input structure.
    pub fn chars(&mut self, value: &[u8]) {
        self.array_size(value.len() as u64);
        self.blob(value, value.len() as u64);
    }

    /// `VkClearValue`, sent with the reference encoder's default tag (0,
    /// the colour), itself a `VkClearColorValue`.
    pub fn clear_value(&mut self, words: &[u32; 4]) {
        self.u32(0);
        self.clear_color(words);
    }

    /// `VkClearColorValue`, default tag 2: four 32-bit words.
    pub fn clear_color(&mut self, words: &[u32; 4]) {
        self.u32(2);
        self.array_size(4);
        for word in words {
            self.u32(*word);
        }
    }

    /// Bytes of another stream, copied as they are: a stream cut in
    /// pieces is sent piece by piece.
    pub fn raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Was an inconsistent value encoded? The stream must then not be sent.
    pub fn error(&self) -> bool {
        self.error
    }

    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }

    pub fn clear(&mut self) {
        self.buf.clear();
        self.error = false;
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
}

/// A reply being read.
pub struct Dec<'a> {
    data: &'a [u8],
    position: usize,
    error: bool,
}

impl<'a> Dec<'a> {
    pub fn new(data: &'a [u8]) -> Dec<'a> {
        Dec { data, position: 0, error: false }
    }

    fn take<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0u8; N];
        match self.data.get(self.position..self.position + N) {
            Some(bytes) => {
                out.copy_from_slice(bytes);
                self.position += N;
            }
            None => self.error = true,
        }
        out
    }

    pub fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take::<4>())
    }

    pub fn i32(&mut self) -> i32 {
        i32::from_le_bytes(self.take::<4>())
    }

    pub fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.take::<8>())
    }

    pub fn i64(&mut self) -> i64 {
        i64::from_le_bytes(self.take::<8>())
    }

    /// The count of an array, at most `max`: a larger one is an error, and
    /// reads as zero.
    pub fn array_size(&mut self, max: usize) -> usize {
        let count = self.u64();
        if count > max as u64 {
            self.error = true;
            return 0;
        }
        count as usize
    }

    /// A fixed character (or byte) array: its count, then the bytes,
    /// padded. The last byte of a character array is forced to zero.
    pub fn chars(&mut self, out: &mut [u8]) {
        let count = self.array_size(out.len());
        let padded = (count + 3) & !3;
        match self.data.get(self.position..self.position + padded) {
            Some(bytes) => {
                out[..count].copy_from_slice(&bytes[..count]);
                self.position += padded;
            }
            None => self.error = true,
        }
    }

    pub fn error(&self) -> bool {
        self.error
    }

    pub fn position(&self) -> usize {
        self.position
    }

    /// Skips to `position`, a word boundary inside the reply.
    pub fn seek(&mut self, position: usize) {
        if position > self.data.len() {
            self.error = true;
        } else {
            self.position = position;
        }
    }
}

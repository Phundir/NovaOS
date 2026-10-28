//! A file's content: extents, reading, writing with copy-on-write,
//! truncating, cloning.
//!
//! ## Extents
//!
//! A file is a list of extents -- (first block of the file, first block of
//! the disk, length) -- sorted, never overlapping; what no extent covers is
//! a hole and reads as zeros. Seven fit in the node record; more go to a
//! chain of extent blocks (`"LE"`, 170 extents each), rewritten whole when
//! the list changes. A file written in one go on a free disk is one extent,
//! whatever its size -- which is what makes a clone of it a few writes.
//!
//! ## Writing into a shared block
//!
//! A block the refcount tree says is shared (a clone designates it too) is
//! never written in place: the write goes to a new block, filled with the
//! old content where the write does not cover it, and the file's extent is
//! remapped; the old block's count drops by one. That holds whichever of
//! the two files is written -- the original is protected like the copy
//! (point 2 of 14.1).

use alloc::vec::Vec;

use super::btree::Space;
use super::format::{get_u32, get_u64, put_u32, put_u64, Extent, Record, BLOCK, CHAINED, INLINE_EXTENTS};
use super::Volume;
use crate::fs::FsError;

const CHAIN_MAGIC: &[u8; 2] = b"LE";
const PER_CHAIN_BLOCK: usize = (BLOCK - 16) / 24;
const BLOCK_U64: u64 = BLOCK as u64;

/// Largest file LIFS addresses, in bytes: blocks are numbered on 64 bits,
/// so the limit is the partition in practice.
pub const MAX_SIZE: u64 = 1 << 62;

impl Volume {
    /// The extents of a node, sorted.
    pub fn extents(&mut self, record: &Record) -> Result<Vec<Extent>, FsError> {
        if record.extent_count != CHAINED {
            return Ok(record.inline[..(record.extent_count as usize).min(INLINE_EXTENTS)].to_vec());
        }
        let mut out = Vec::new();
        let mut block = record.tree;
        let mut guard = 0;
        while block != 0 {
            guard += 1;
            if guard > 1 << 20 {
                return Err(FsError::Corrupt);
            }
            let image = self.store_mut().get(block)?;
            if &image[0..2] != CHAIN_MAGIC {
                return Err(FsError::Corrupt);
            }
            let count = (get_u32(&image[..], 4) as usize).min(PER_CHAIN_BLOCK);
            for i in 0..count {
                let at = 16 + i * 24;
                out.push(Extent { logical: get_u64(&image[..], at), phys: get_u64(&image[..], at + 8), len: get_u64(&image[..], at + 16) });
            }
            block = get_u64(&image[..], 8);
        }
        Ok(out)
    }

    fn free_chain(&mut self, record: &Record) -> Result<(), FsError> {
        if record.extent_count != CHAINED {
            return Ok(());
        }
        let mut block = record.tree;
        while block != 0 {
            let image = self.store_mut().get(block)?;
            let next = get_u64(&image[..], 8);
            self.free_meta(block);
            block = next;
        }
        Ok(())
    }

    /// Stores a node's extents: inline if they fit, else a new chain (the
    /// old one freed). The caller saves the record.
    pub fn set_extents(&mut self, record: &mut Record, extents: &[Extent]) -> Result<(), FsError> {
        self.free_chain(record)?;
        if extents.len() <= INLINE_EXTENTS {
            record.extent_count = extents.len() as u8;
            record.tree = 0;
            for (i, slot) in record.inline.iter_mut().enumerate() {
                *slot = extents.get(i).copied().unwrap_or(Extent { logical: 0, phys: 0, len: 0 });
            }
            return Ok(());
        }
        let chunks: Vec<&[Extent]> = extents.chunks(PER_CHAIN_BLOCK).collect();
        let mut blocks = Vec::with_capacity(chunks.len());
        for _ in 0..chunks.len() {
            blocks.push(self.alloc_meta()?);
        }
        for (index, chunk) in chunks.iter().enumerate() {
            let mut image = [0u8; BLOCK];
            image[0..2].copy_from_slice(CHAIN_MAGIC);
            put_u32(&mut image, 4, chunk.len() as u32);
            put_u64(&mut image, 8, blocks.get(index + 1).copied().unwrap_or(0));
            for (i, e) in chunk.iter().enumerate() {
                let at = 16 + i * 24;
                put_u64(&mut image, at, e.logical);
                put_u64(&mut image, at + 8, e.phys);
                put_u64(&mut image, at + 16, e.len);
            }
            self.store_mut().put(blocks[index], &image);
        }
        record.extent_count = CHAINED;
        record.tree = blocks[0];
        record.inline = [Extent { logical: 0, phys: 0, len: 0 }; INLINE_EXTENTS];
        Ok(())
    }

    // ============================================================
    //  Reading
    // ============================================================

    /// Reads from `offset`; returns the bytes read (short at the end).
    pub fn read_at(&mut self, record: &Record, offset: u64, buffer: &mut [u8]) -> Result<usize, FsError> {
        if offset >= record.size || buffer.is_empty() {
            return Ok(0);
        }
        let total = (buffer.len() as u64).min(record.size - offset) as usize;
        let extents = self.extents(record)?;
        let mut block = [0u8; BLOCK];
        let mut done = 0;
        while done < total {
            let at = offset + done as u64;
            let logical = at / BLOCK_U64;
            let inside = (at % BLOCK_U64) as usize;
            let take = (BLOCK - inside).min(total - done);
            match map(&extents, logical) {
                Some(phys) => {
                    self.store_mut().read_data(phys, &mut block)?;
                    buffer[done..done + take].copy_from_slice(&block[inside..inside + take]);
                }
                None => buffer[done..done + take].fill(0),
            }
            done += take;
        }
        Ok(total)
    }

    /// The disk block of each of `count` blocks from `first` (0: a hole).
    pub fn physical(&mut self, record: &Record, first: u64, count: usize) -> Result<Vec<u64>, FsError> {
        let extents = self.extents(record)?;
        Ok((0..count as u64).map(|i| map(&extents, first + i).unwrap_or(0)).collect())
    }

    // ============================================================
    //  Writing
    // ============================================================

    /// Writes `data` at `offset`, growing the file if needed. Blocks
    /// shared with a clone are copied first (module header).
    pub fn write_at(&mut self, record: &mut Record, offset: u64, data: &[u8], now: u64) -> Result<usize, FsError> {
        if data.is_empty() {
            return Ok(0);
        }
        let end = offset.checked_add(data.len() as u64).ok_or(FsError::TooLarge)?;
        if end > MAX_SIZE {
            return Err(FsError::TooLarge);
        }
        let mut extents = self.extents(record)?;
        let mut changed = false;
        let first = offset / BLOCK_U64;
        let last = (end - 1) / BLOCK_U64;
        let mut logical = first;
        let mut block = [0u8; BLOCK];
        while logical <= last {
            let mapped = map(&extents, logical);
            let shared = match mapped {
                Some(phys) => self.is_shared(phys)?,
                None => false,
            };
            if let (Some(phys), false) = (mapped, shared) {
                // In place.
                let (from, to) = span(logical, offset, end);
                if from != 0 || to != BLOCK {
                    self.store_mut().read_data(phys, &mut block)?;
                }
                let source = (logical * BLOCK_U64 + from as u64 - offset) as usize;
                block[from..to].copy_from_slice(&data[source..source + (to - from)]);
                self.store_mut().write_data(phys, &block)?;
                logical += 1;
                continue;
            }
            // New blocks: the run of blocks from here that are holes or
            // shared, allocated together.
            let mut run = 1;
            while logical + run <= last {
                let needs_new = match map(&extents, logical + run) {
                    Some(phys) => self.is_shared(phys)?,
                    None => true,
                };
                if !needs_new {
                    break;
                }
                run += 1;
            }
            let (start, got) = self.alloc_run(run)?;
            for i in 0..got {
                let at = logical + i;
                let (from, to) = span(at, offset, end);
                let old = map(&extents, at);
                if from != 0 || to != BLOCK {
                    match old {
                        Some(phys) => self.store_mut().read_data(phys, &mut block)?,
                        None => block.fill(0),
                    }
                }
                let source = (at * BLOCK_U64 + from as u64 - offset) as usize;
                block[from..to].copy_from_slice(&data[source..source + (to - from)]);
                self.store_mut().write_data(start + i, &block)?;
                if let Some(phys) = old {
                    // The shared block loses one designation: ours.
                    self.adjust_refs(phys, 1, -1)?;
                }
            }
            remap(&mut extents, logical, start, got);
            changed = true;
            logical += got;
        }
        if changed {
            self.set_extents(record, &extents)?;
        }
        record.size = record.size.max(end);
        record.modified = now;
        self.save_record(record)?;
        Ok(data.len())
    }

    /// Cuts the file to `size`, or grows it with a hole.
    pub fn truncate(&mut self, record: &mut Record, size: u64, now: u64) -> Result<(), FsError> {
        if size > MAX_SIZE {
            return Err(FsError::TooLarge);
        }
        if size < record.size {
            // The tail of the last block kept must read zero if the file
            // grows again: written as zeros (copied first if shared).
            let tail = size % BLOCK_U64;
            if tail != 0 {
                let zeros = alloc::vec![0u8; (BLOCK_U64 - tail).min(record.size - size) as usize];
                let extents = self.extents(record)?;
                if map(&extents, size / BLOCK_U64).is_some() {
                    let keep = record.size;
                    self.write_at(record, size, &zeros, now)?;
                    record.size = keep;
                }
            }
            let keep_blocks = size.div_ceil(BLOCK_U64);
            let mut extents = self.extents(record)?;
            let mut dropped = Vec::new();
            cut(&mut extents, keep_blocks, u64::MAX, &mut dropped);
            for (phys, len) in dropped {
                self.adjust_refs(phys, len, -1)?;
            }
            self.set_extents(record, &extents)?;
        }
        record.size = size;
        record.modified = now;
        self.save_record(record)
    }

    /// Gives back every block of the node (refcounts lowered).
    ///
    /// No commit in the middle: the record on the disk still names these
    /// extents until the caller saves or drops it, and a commit between the
    /// two would make blocks free that a durable record designates.
    pub fn release_content(&mut self, record: &Record) -> Result<(), FsError> {
        let extents = self.extents(record)?;
        for e in extents {
            self.adjust_refs(e.phys, e.len, -1)?;
        }
        self.free_chain(record)
    }

    /// The extents of `source` given to `target` too: each extent's count
    /// raised by one. A few tree writes, whatever the size (14.6.1).
    pub fn share_content(&mut self, source: &Record, target: &mut Record) -> Result<(), FsError> {
        let extents = self.extents(source)?;
        for e in extents.iter() {
            self.adjust_refs(e.phys, e.len, 1)?;
        }
        target.extent_count = 0;
        target.tree = 0;
        self.set_extents(target, &extents)?;
        target.size = source.size;
        Ok(())
    }

    /// Blocks a node designates (data and chain), for the check.
    pub fn content_blocks(&mut self, record: &Record, out: &mut Vec<(u64, u64)>) -> Result<(), FsError> {
        for e in self.extents(record)? {
            out.push((e.phys, e.len));
        }
        if record.extent_count == CHAINED {
            let mut block = record.tree;
            while block != 0 {
                out.push((block, 1));
                let image = self.store_mut().get(block)?;
                block = get_u64(&image[..], 8);
            }
        }
        Ok(())
    }
}

/// The part of block `logical` a write of `offset .. end` covers, as byte
/// offsets in the block.
fn span(logical: u64, offset: u64, end: u64) -> (usize, usize) {
    let start = logical * BLOCK_U64;
    let from = offset.max(start) - start;
    let to = end.min(start + BLOCK_U64) - start;
    (from as usize, to as usize)
}

/// The disk block of file block `logical`.
pub fn map(extents: &[Extent], logical: u64) -> Option<u64> {
    let index = extents.partition_point(|e| e.logical + e.len <= logical);
    let e = extents.get(index)?;
    (e.logical <= logical).then(|| e.phys + (logical - e.logical))
}

/// Removes file blocks `from .. to` from the list; what they mapped is
/// pushed to `dropped` as (disk block, length).
fn cut(extents: &mut Vec<Extent>, from: u64, to: u64, dropped: &mut Vec<(u64, u64)>) {
    let mut out = Vec::with_capacity(extents.len() + 1);
    for e in extents.iter() {
        let end = e.logical + e.len;
        if end <= from || e.logical >= to {
            out.push(*e);
            continue;
        }
        if e.logical < from {
            out.push(Extent { logical: e.logical, phys: e.phys, len: from - e.logical });
        }
        let lo = e.logical.max(from);
        let hi = end.min(to);
        dropped.push((e.phys + (lo - e.logical), hi - lo));
        if end > to {
            out.push(Extent { logical: to, phys: e.phys + (to - e.logical), len: end - to });
        }
    }
    *extents = out;
}

/// Maps file blocks `logical .. logical + len` to disk blocks from `phys`,
/// replacing what mapped them (the caller has dealt with the old blocks),
/// and merges neighbours that continue each other.
fn remap(extents: &mut Vec<Extent>, logical: u64, phys: u64, len: u64) {
    let mut ignored = Vec::new();
    cut(extents, logical, logical + len, &mut ignored);
    let index = extents.partition_point(|e| e.logical < logical);
    extents.insert(index, Extent { logical, phys, len });
    let mut merged: Vec<Extent> = Vec::with_capacity(extents.len());
    for e in extents.iter() {
        if let Some(last) = merged.last_mut()
            && last.logical + last.len == e.logical
            && last.phys + last.len == e.phys
        {
            last.len += e.len;
            continue;
        }
        merged.push(*e);
    }
    *extents = merged;
}

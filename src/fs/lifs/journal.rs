//! The journal of LIFS (spec 4.5, decision of 24 September 2026): every
//! change of the metadata goes to the journal first.
//!
//! ## The store
//!
//! `Store` holds the block cache and the **open transaction**: the images
//! of the metadata blocks changed since the last commit (tree blocks, the
//! bitmap, the superblock). Metadata is read through the transaction first
//! -- the latest image wins -- and written only into it; file data goes
//! straight to the cache. Nothing of the metadata reaches the disk before
//! the commit, whatever the cache evicts.
//!
//! ## A commit, in four steps
//!
//! 1. the cache is written out: the file data the new metadata will name
//!    is on the disk first (ordered mode);
//! 2. the transaction goes to the journal area -- descriptor blocks (the
//!    home block of each image), the images, then a **commit block** with
//!    the sequence number and the CRC32 of the images -- and is flushed;
//! 3. the images are written to their homes, and flushed;
//! 4. the transaction is emptied; the blocks it freed may be allocated
//!    again (`Volume` holds them back until then).
//!
//! The superblock is one of the images: it carries the sequence of the
//! transaction as its `journal_seq`. At mount, a journal whose commit block
//! is whole and whose sequence is the superblock's plus one is **replayed**
//! (step 3 again -- it is idempotent); anything else in the journal is
//! stale or unfinished, and ignored. A power cut therefore loses at most the
//! transaction being written, never the consistency.
//!
//! ## Group commit
//!
//! A transaction spans many operations: it is committed every two seconds
//! (`Filesystem::tick`), on `fsync` and `sync`, at shutdown, and when it
//! grows past half the journal. What that costs: the last seconds after a
//! power cut; what it saves: three disk flushes per file operation.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use super::format::{crc32, get_u32, get_u64, put_u32, put_u64, BLOCK};
use crate::fs::block::BlockError;
use crate::fs::cache::BlockCache;
use crate::fs::FsError;

const DESC_MAGIC: [u8; 8] = *b"LJDESC\0\0";
const COMMIT_MAGIC: [u8; 8] = *b"LJCOMMIT";
/// Home numbers a descriptor block lists.
const PER_DESCRIPTOR: usize = (BLOCK - 24) / 8;

pub type Image = Box<[u8; BLOCK]>;

pub struct Store {
    pub cache: BlockCache,
    txn: BTreeMap<u64, Image>,
    journal_start: u64,
    journal_blocks: u64,
    pub commits: u64,
    pub journaled_blocks: u64,
}

fn blank() -> Image {
    Box::new([0u8; BLOCK])
}

impl Store {
    pub fn new(cache: BlockCache, journal_start: u64, journal_blocks: u64) -> Store {
        Store { cache, txn: BTreeMap::new(), journal_start, journal_blocks, commits: 0, journaled_blocks: 0 }
    }

    /// Blocks in the open transaction.
    pub fn pending(&self) -> usize {
        self.txn.len()
    }

    /// Images the journal can take in one transaction.
    pub fn capacity(&self) -> usize {
        let blocks = self.journal_blocks as usize;
        // Every PER_DESCRIPTOR images take one descriptor more, and the
        // commit block one.
        blocks.saturating_sub(1) * PER_DESCRIPTOR / (PER_DESCRIPTOR + 1)
    }

    /// Should the transaction be committed before it grows further?
    pub fn full(&self) -> bool {
        self.txn.len() >= self.capacity() / 2
    }

    /// Reads a metadata block: its image in the transaction, or the disk's.
    pub fn read(&mut self, block: u64, out: &mut [u8]) -> Result<(), FsError> {
        if let Some(image) = self.txn.get(&block) {
            out.copy_from_slice(&image[..]);
            return Ok(());
        }
        self.cache.read(block, out).map_err(FsError::Block)
    }

    /// A metadata block, as an image to change.
    pub fn get(&mut self, block: u64) -> Result<Image, FsError> {
        let mut image = blank();
        self.read(block, &mut image[..])?;
        Ok(image)
    }

    /// Writes a metadata block into the transaction.
    pub fn put(&mut self, block: u64, data: &[u8]) {
        let mut image = blank();
        image.copy_from_slice(data);
        self.txn.insert(block, image);
    }

    /// Changes a metadata block in place in the transaction.
    pub fn modify<R>(&mut self, block: u64, action: impl FnOnce(&mut [u8; BLOCK]) -> R) -> Result<R, FsError> {
        if !self.txn.contains_key(&block) {
            let image = self.get(block)?;
            self.txn.insert(block, image);
        }
        let image = self.txn.get_mut(&block).ok_or(FsError::Corrupt)?;
        Ok(action(image))
    }

    /// A block about to become metadata (a fresh tree block): whatever the
    /// disk held there is irrelevant.
    pub fn fresh(&mut self, block: u64) {
        self.txn.insert(block, blank());
    }

    /// Drops a metadata block from the transaction: it was freed in the
    /// same transaction that made it.
    pub fn forget(&mut self, block: u64) {
        self.txn.remove(&block);
    }

    // --- file data: straight to the cache ---

    pub fn read_data(&mut self, block: u64, out: &mut [u8]) -> Result<(), FsError> {
        self.cache.read(block, out).map_err(FsError::Block)
    }

    pub fn write_data(&mut self, block: u64, data: &[u8]) -> Result<(), FsError> {
        self.cache.write(block, data).map_err(FsError::Block)
    }

    /// Commits the open transaction as number `seq` (steps 1 to 4 of the
    /// module header). The caller has put the superblock, carrying `seq`,
    /// into it.
    pub fn commit(&mut self, seq: u64) -> Result<(), FsError> {
        if self.txn.is_empty() {
            return Ok(());
        }
        let homes: Vec<u64> = self.txn.keys().copied().collect();
        let descriptors = homes.len().div_ceil(PER_DESCRIPTOR);
        if (descriptors + homes.len() + 1) as u64 > self.journal_blocks {
            return Err(FsError::NoSpace);
        }
        // 1. the data first.
        self.cache.flush().map_err(FsError::Block)?;
        // 2. the journal.
        let mut at = self.journal_start;
        let mut crc = 0u32;
        for chunk in homes.chunks(PER_DESCRIPTOR) {
            let mut descriptor = [0u8; BLOCK];
            descriptor[0..8].copy_from_slice(&DESC_MAGIC);
            put_u64(&mut descriptor, 8, seq);
            put_u32(&mut descriptor, 16, chunk.len() as u32);
            for (i, home) in chunk.iter().enumerate() {
                put_u64(&mut descriptor, 24 + i * 8, *home);
            }
            self.cache.write(at, &descriptor).map_err(FsError::Block)?;
            at += 1;
            for home in chunk {
                let image = &self.txn[home];
                crc = crate::fs::partition::crc32_update(crc, &image[..]);
                self.cache.write(at, &image[..]).map_err(FsError::Block)?;
                at += 1;
            }
        }
        let mut commit = [0u8; BLOCK];
        commit[0..8].copy_from_slice(&COMMIT_MAGIC);
        put_u64(&mut commit, 8, seq);
        put_u32(&mut commit, 16, homes.len() as u32);
        put_u32(&mut commit, 20, crc);
        let sum = crc32(&commit[..BLOCK - 4]);
        put_u32(&mut commit, BLOCK - 4, sum);
        self.cache.write(at, &commit).map_err(FsError::Block)?;
        self.cache.flush().map_err(FsError::Block)?;
        // 3. the homes.
        let images: Vec<(u64, &Image)> = self.txn.iter().map(|(home, image)| (*home, image)).collect();
        write_homes(&mut self.cache, &images)?;
        // 4. done.
        self.commits += 1;
        self.journaled_blocks += homes.len() as u64;
        self.txn.clear();
        Ok(())
    }

    /// Replays a committed transaction the homes may not all have: the one
    /// of sequence `seq` if the journal holds it whole. Returns whether it
    /// did.
    pub fn replay(&mut self, seq: u64) -> Result<bool, FsError> {
        let mut at = self.journal_start;
        let end = self.journal_start + self.journal_blocks;
        let mut block = [0u8; BLOCK];
        let mut images: Vec<(u64, Image)> = Vec::new();
        let mut crc = 0u32;
        loop {
            if at >= end {
                return Ok(false);
            }
            self.cache.read(at, &mut block).map_err(FsError::Block)?;
            at += 1;
            if block[0..8] == COMMIT_MAGIC {
                let whole = get_u64(&block, 8) == seq
                    && get_u32(&block, 16) as usize == images.len()
                    && get_u32(&block, 20) == crc
                    && crc32(&block[..BLOCK - 4]) == get_u32(&block, BLOCK - 4);
                if !whole {
                    return Ok(false);
                }
                break;
            }
            if block[0..8] != DESC_MAGIC || get_u64(&block, 8) != seq {
                return Ok(false);
            }
            let count = (get_u32(&block, 16) as usize).min(PER_DESCRIPTOR);
            let homes: Vec<u64> = (0..count).map(|i| get_u64(&block, 24 + i * 8)).collect();
            for home in homes {
                if at >= end {
                    return Ok(false);
                }
                let mut image = blank();
                self.cache.read(at, &mut image[..]).map_err(FsError::Block)?;
                at += 1;
                crc = crate::fs::partition::crc32_update(crc, &image[..]);
                images.push((home, image));
            }
        }
        let homes: Vec<(u64, &Image)> = images.iter().map(|(home, image)| (*home, image)).collect();
        write_homes(&mut self.cache, &homes)?;
        Ok(true)
    }

    /// Wipes the journal's first block: a fresh volume has no transaction.
    pub fn reset(&mut self) -> Result<(), BlockError> {
        self.cache.write(self.journal_start, &[0u8; BLOCK])
    }
}

/// Writes images to their homes, the superblock (block 0) **last**, after
/// a flush of the others: a superblock carrying sequence `seq` then proves
/// every other home of `seq` is on the disk. Were it written among them, a
/// power cut right after it would leave a volume claiming `seq` with half
/// its blocks, and the replay -- which looks for `seq + 1` -- would not
/// run.
fn write_homes(cache: &mut BlockCache, images: &[(u64, &Image)]) -> Result<(), FsError> {
    for (home, image) in images.iter().filter(|(home, _)| *home != 0) {
        cache.write(*home, &image[..]).map_err(FsError::Block)?;
    }
    cache.flush().map_err(FsError::Block)?;
    if let Some((_, image)) = images.iter().find(|(home, _)| *home == 0) {
        cache.write(0, &image[..]).map_err(FsError::Block)?;
        cache.flush().map_err(FsError::Block)?;
    }
    Ok(())
}

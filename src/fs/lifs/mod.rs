//! LIFS, the Linked Index File System: Nova's native format (spec 4.5,
//! phase 14), the system volume included.
//!
//! ## The pieces
//!
//! | Module | Role |
//! |---|---|
//! | `format` | Superblock, node record, entry, keys -- the bytes on disk |
//! | `journal` | The open transaction, the commit, the replay |
//! | `btree` | The micro-database: B+ trees of 4 KiB blocks |
//! | this file | The volume: mount, format, blocks, refcounts, nodes, commit |
//! | `files` | A file's extents: read, write with copy-on-write, truncate |
//! | `dirs` | Entries and the reverse index |
//! | `ops` | The operations by path and by identifier, the rights |
//!
//! ## Shared extents
//!
//! A block belongs to one node unless the refcount tree says otherwise: it
//! holds, for every run of blocks two nodes or more designate (a clone),
//! the number of designations. Allocated and absent from the tree means
//! one. Freeing a run lowers its count, and gives back to the bitmap only
//! what drops to zero; cloning a file raises the count of each of its
//! extents -- a few tree entries, whatever the size of the file.
//!
//! ## Blocks freed wait for the commit
//!
//! A block freed in the open transaction is still designated by the
//! metadata on the disk, which the transaction has not replaced yet: were
//! it written with new data now, a power cut would leave the old metadata
//! pointing at foreign bytes. So freed runs wait in `pending` and join the
//! bitmap when the transaction commits.

pub mod btree;
pub mod check;
pub mod dirs;
pub mod files;
pub mod format;
pub mod journal;
pub mod ops;
pub mod selftest;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::block::MediumKind;
use crate::fs::cache::BlockCache;
use crate::fs::FsError;
use btree::{Key, Space};
use format::{get_u32, get_u64, put_u32, put_u64, Record, Superblock, BLOCK, NODE_SIZE, REFCOUNT_SIZE};
use journal::Store;

/// Seconds between two group commits (`Filesystem::tick`).
pub const COMMIT_EVERY_MS: u64 = 2000;

pub struct Volume {
    store: Store,
    pub sb: Superblock,
    sb_dirty: bool,
    pub letter: char,
    name: String,
    medium: MediumKind,
    /// Runs freed in the open transaction: start -> length.
    pending: BTreeMap<u64, u64>,
    /// Where the next allocation starts looking.
    hint: u64,
    last_commit: u64,
    /// The partition's GUID, when mounted from a GPT.
    pub partition: Option<[u8; 16]>,
    /// Did the mount replay a transaction from the journal?
    pub replayed: bool,
}

impl Space for Volume {
    fn store(&mut self) -> &mut Store {
        &mut self.store
    }

    fn alloc_meta(&mut self) -> Result<u64, FsError> {
        let (block, _) = self.alloc_run(1)?;
        self.store.fresh(block);
        Ok(block)
    }

    fn free_meta(&mut self, block: u64) {
        self.store.forget(block);
        self.free_run(block, 1);
    }
}

fn now_ms() -> u64 {
    crate::idt::fine_ticks()
}

impl Volume {
    // ============================================================
    //  Mount and format
    // ============================================================

    /// Mounts the LIFS volume of `cache`: superblock, replay of a whole
    /// transaction the homes may lack, then the superblock again.
    pub fn mount(mut cache: BlockCache) -> Result<Volume, FsError> {
        let blocks = cache.block_count();
        let mut raw = [0u8; BLOCK];
        cache.read(0, &mut raw).map_err(FsError::Block)?;
        let sb = Superblock::decode(&raw, blocks).map_err(|_| FsError::Format(crate::fs::format::FormatError::CorruptSuperblock))?;
        let name = cache.device_name();
        let medium = cache.medium();
        let mut store = Store::new(cache, sb.journal_start, sb.journal_blocks);
        let replayed = store.replay(sb.journal_seq + 1)?;
        let sb = if replayed {
            let mut raw = [0u8; BLOCK];
            store.read_data(0, &mut raw)?;
            Superblock::decode(&raw, blocks).map_err(|_| FsError::Corrupt)?
        } else {
            sb
        };
        let hint = sb.data_start();
        let mut volume = Volume {
            store,
            sb,
            sb_dirty: false,
            letter: '/',
            name,
            medium,
            pending: BTreeMap::new(),
            hint,
            last_commit: now_ms(),
            partition: None,
            replayed,
        };
        volume.sb.mount_count += 1;
        volume.sb_dirty = true;
        volume.commit()?;
        Ok(volume)
    }

    /// Formats `cache` as an empty LIFS volume -- an explicit act (spec
    /// 4.5): only `tools/lifs.py` and the in-memory volume call it.
    pub fn format(cache: BlockCache, label: &str, flags: u32, now: u64) -> Result<Volume, FsError> {
        let total = cache.block_count();
        let bitmap_blocks = total.div_ceil(BLOCK as u64 * 8);
        let journal_blocks = (total / 64).clamp(64, 16384);
        if 1 + bitmap_blocks + journal_blocks + 16 > total {
            return Err(FsError::NoSpace);
        }
        let mut sb = Superblock {
            total_blocks: total,
            uuid: [0; 16],
            label: String::from(label),
            flags,
            bitmap_start: 1,
            bitmap_blocks,
            journal_start: 1 + bitmap_blocks,
            journal_blocks,
            node_root: 0,
            refcount_root: 0,
            reverse_root: 0,
            next_node: format::ROOT,
            free_blocks: 0,
            node_count: 0,
            journal_seq: 0,
            created: now,
            modified: now,
            mount_count: 0,
        };
        let data_start = sb.data_start();
        sb.free_blocks = total - data_start;
        let name = cache.device_name();
        let medium = cache.medium();
        let mut store = Store::new(cache, sb.journal_start, sb.journal_blocks);
        // The bitmap: the superblock, the bitmap and the journal taken.
        for index in 0..bitmap_blocks {
            let mut image = [0u8; BLOCK];
            let first = index * BLOCK as u64 * 8;
            for bit in 0..(BLOCK as u64 * 8) {
                let block = first + bit;
                if block < data_start || block >= total {
                    image[(bit / 8) as usize] |= 1 << (bit % 8);
                }
            }
            store.put(sb.bitmap_start + index, &image);
        }
        store.reset().map_err(FsError::Block)?;
        let mut volume = Volume {
            store,
            sb,
            sb_dirty: true,
            letter: '/',
            name,
            medium,
            pending: BTreeMap::new(),
            hint: data_start,
            last_commit: now_ms(),
            partition: None,
            replayed: false,
        };
        // The root directory.
        let mut root = volume.new_record(format::KIND_DIR, 0, 0, now)?;
        let mode = crate::fs::perm::Mode::system_dir();
        root.perm_owner = mode.owner.0 as u16;
        root.perm_group = mode.group.0 as u16;
        root.perm_other = mode.other.0 as u16;
        root.links = 1;
        volume.save_record(&root)?;
        volume.commit()?;
        Ok(volume)
    }

    pub fn name(&self) -> String {
        self.name.clone()
    }

    pub fn medium(&self) -> MediumKind {
        self.medium
    }

    pub fn block_count(&self) -> u64 {
        self.store.cache.block_count()
    }

    pub fn raw_read(&mut self, block: u64, out: &mut [u8]) -> Result<(), FsError> {
        self.store.read(block, out)
    }

    // ============================================================
    //  Commit
    // ============================================================

    /// Commits the open transaction (see `journal`): the frees waiting join
    /// the bitmap, the superblock carries the sequence, the store writes.
    pub fn commit(&mut self) -> Result<(), FsError> {
        if self.store.pending() == 0 && !self.sb_dirty && self.pending.is_empty() {
            return Ok(());
        }
        let freed: Vec<(u64, u64)> = core::mem::take(&mut self.pending).into_iter().collect();
        for (start, len) in freed {
            self.set_bits(start, len, false)?;
            self.sb.free_blocks += len;
        }
        let seq = self.sb.journal_seq + 1;
        self.sb.journal_seq = seq;
        self.sb.modified = crate::idt::ticks();
        let image = self.sb.encode();
        self.store.put(0, &image);
        if let Err(error) = self.store.commit(seq) {
            // The transaction stays open: the next commit retries it.
            self.sb.journal_seq = seq - 1;
            return Err(error);
        }
        self.sb_dirty = false;
        self.last_commit = now_ms();
        Ok(())
    }

    /// Commits if the transaction grew past half the journal: called at the
    /// safe points of long operations, and at the start of every one.
    pub fn commit_if_full(&mut self) -> Result<(), FsError> {
        if self.store.full() { self.commit() } else { Ok(()) }
    }

    /// The group commit, every `COMMIT_EVERY_MS`.
    pub fn tick(&mut self) -> Result<(), FsError> {
        if now_ms().saturating_sub(self.last_commit) >= COMMIT_EVERY_MS {
            self.commit()?;
        }
        Ok(())
    }

    // ============================================================
    //  Blocks
    // ============================================================

    fn bit_place(&self, block: u64) -> (u64, usize, u8) {
        let per = BLOCK as u64 * 8;
        (self.sb.bitmap_start + block / per, ((block % per) / 8) as usize, 1 << (block % 8))
    }

    fn used(&mut self, block: u64) -> Result<bool, FsError> {
        let (b, byte, bit) = self.bit_place(block);
        let mut image = [0u8; BLOCK];
        self.store.read(b, &mut image)?;
        Ok(image[byte] & bit != 0)
    }

    fn set_bits(&mut self, start: u64, len: u64, value: bool) -> Result<(), FsError> {
        let mut block = start;
        while block < start + len {
            let (b, _, _) = self.bit_place(block);
            let per = BLOCK as u64 * 8;
            let last = ((block / per + 1) * per).min(start + len);
            let first = block;
            let base = (block / per) * per;
            self.store.modify(b, |image| {
                for x in first..last {
                    let bit = x - base;
                    let (byte, mask) = ((bit / 8) as usize, 1u8 << (bit % 8));
                    if value {
                        image[byte] |= mask;
                    } else {
                        image[byte] &= !mask;
                    }
                }
            })?;
            block = last;
        }
        Ok(())
    }

    fn is_pending(&self, block: u64) -> bool {
        self.pending.range(..=block).next_back().is_some_and(|(start, len)| block < start + len)
    }

    /// Allocates a run of up to `want` consecutive blocks, the first free
    /// run from the hint on. Returns (start, length), length at least 1.
    pub fn alloc_run(&mut self, want: u64) -> Result<(u64, u64), FsError> {
        if self.sb.free_blocks == 0 {
            return Err(FsError::NoSpace);
        }
        let data_start = self.sb.data_start();
        let total = self.sb.total_blocks;
        let per = BLOCK as u64 * 8;
        let mut image = [0u8; BLOCK];
        let mut loaded = u64::MAX;
        let mut start = None;
        let mut len = 0;
        // Two passes: from the hint to the end, then from the start.
        for (from, to) in [(self.hint.max(data_start), total), (data_start, self.hint.max(data_start))] {
            let mut block = from;
            while block < to {
                let map = self.sb.bitmap_start + block / per;
                if map != loaded {
                    self.store.read(map, &mut image)?;
                    loaded = map;
                }
                let bit = block % per;
                // A whole byte in use is skipped at once.
                if bit % 8 == 0 && start.is_none() && image[(bit / 8) as usize] == 0xFF && block + 8 <= to {
                    block += 8;
                    continue;
                }
                let free = image[(bit / 8) as usize] & (1 << (bit % 8)) == 0 && !self.is_pending(block);
                if free {
                    if start.is_none() {
                        start = Some(block);
                    }
                    len += 1;
                    if len == want {
                        break;
                    }
                } else if start.is_some() {
                    break;
                }
                block += 1;
            }
            if start.is_some() {
                break;
            }
        }
        let Some(first) = start else {
            return Err(FsError::NoSpace);
        };
        self.set_bits(first, len, true)?;
        self.sb.free_blocks = self.sb.free_blocks.saturating_sub(len);
        self.sb_dirty = true;
        self.hint = first + len;
        Ok((first, len))
    }

    /// Frees a run: its blocks are allocatable again after the commit.
    pub fn free_run(&mut self, start: u64, len: u64) {
        if len == 0 {
            return;
        }
        self.pending.insert(start, len);
        self.sb_dirty = true;
    }

    // ============================================================
    //  Refcounts of shared runs
    // ============================================================

    /// The shared runs overlapping `start .. end`: (start, length, refs).
    fn shared_runs(&mut self, start: u64, end: u64) -> Result<Vec<(u64, u64, u32)>, FsError> {
        let root = self.sb.refcount_root;
        let mut out = Vec::new();
        if let Some((key, value)) = btree::last_at_most(self, root, Key::new(start, 0))? {
            let len = get_u64(&value, 0);
            if key.hi + len > start {
                out.push((key.hi, len, get_u32(&value, 8)));
            }
        }
        if end > start + 1 {
            for (key, value) in btree::range(self, root, Key::new(start + 1, 0), Key::new(end - 1, u64::MAX))? {
                out.push((key.hi, get_u64(&value, 0), get_u32(&value, 8)));
            }
        }
        Ok(out)
    }

    fn put_shared(&mut self, start: u64, len: u64, refs: u32) -> Result<(), FsError> {
        let mut value = [0u8; REFCOUNT_SIZE];
        put_u64(&mut value, 0, len);
        put_u32(&mut value, 8, refs);
        let mut root = self.sb.refcount_root;
        btree::insert(self, &mut root, REFCOUNT_SIZE, Key::new(start, 0), &value)?;
        self.sb.refcount_root = root;
        self.sb_dirty = true;
        Ok(())
    }

    fn drop_shared(&mut self, start: u64) -> Result<(), FsError> {
        let mut root = self.sb.refcount_root;
        btree::remove(self, &mut root, Key::new(start, 0))?;
        self.sb.refcount_root = root;
        self.sb_dirty = true;
        Ok(())
    }

    /// Changes the count of every block of `start .. start + len` by
    /// `delta` (+1 or -1). With -1, what drops to zero is freed.
    pub fn adjust_refs(&mut self, start: u64, len: u64, delta: i32) -> Result<(), FsError> {
        let end = start + len;
        let runs = self.shared_runs(start, end)?;
        // The runs are cut at `start` and `end`; what lies outside keeps
        // its count.
        for (s, l, refs) in runs.iter() {
            self.drop_shared(*s)?;
            if *s < start {
                self.put_shared(*s, start - s, *refs)?;
            }
            if s + l > end {
                self.put_shared(end, s + l - end, *refs)?;
            }
        }
        // Then the covered part, run by run: shared ones, and the gaps
        // between them, which count one.
        let mut at = start;
        let mut pieces: Vec<(u64, u64, u32)> = Vec::new();
        for (s, l, refs) in runs.iter() {
            let from = (*s).max(start);
            let to = (s + l).min(end);
            if from > at {
                pieces.push((at, from - at, 1));
            }
            pieces.push((from, to - from, *refs));
            at = to;
        }
        if at < end {
            pieces.push((at, end - at, 1));
        }
        for (s, l, refs) in pieces {
            let new = refs as i64 + delta as i64;
            if new <= 0 {
                self.free_run(s, l);
            } else if new >= 2 {
                self.put_shared(s, l, new as u32)?;
            }
            // new == 1: the implicit count, no entry.
        }
        Ok(())
    }

    /// Is `block` designated by more than one node?
    pub fn is_shared(&mut self, block: u64) -> Result<bool, FsError> {
        Ok(!self.shared_runs(block, block + 1)?.is_empty())
    }

    // ============================================================
    //  Nodes
    // ============================================================

    pub fn load_record(&mut self, id: u64) -> Result<Record, FsError> {
        let root = self.sb.node_root;
        match btree::get(self, root, Key::new(id, 0))? {
            Some(value) => Ok(Record::decode(&value)),
            None => Err(FsError::NotFound),
        }
    }

    pub fn save_record(&mut self, record: &Record) -> Result<(), FsError> {
        let mut root = self.sb.node_root;
        btree::insert(self, &mut root, NODE_SIZE, Key::new(record.id, 0), &record.encode())?;
        self.sb.node_root = root;
        self.sb_dirty = true;
        Ok(())
    }

    /// A new node, its identifier never used before.
    pub fn new_record(&mut self, kind: u8, owner: u32, group: u32, now: u64) -> Result<Record, FsError> {
        let id = self.sb.next_node;
        self.sb.next_node += 1;
        self.sb.node_count += 1;
        self.sb_dirty = true;
        Ok(Record::new(id, kind, owner, group, now))
    }

    pub fn drop_record(&mut self, id: u64) -> Result<(), FsError> {
        let mut root = self.sb.node_root;
        btree::remove(self, &mut root, Key::new(id, 0))?;
        self.sb.node_root = root;
        self.sb.node_count = self.sb.node_count.saturating_sub(1);
        self.sb_dirty = true;
        Ok(())
    }

    /// Every node record, for the check and the dump.
    pub fn all_records(&mut self) -> Result<Vec<Record>, FsError> {
        let root = self.sb.node_root;
        Ok(btree::range(self, root, Key::MIN, Key::MAX)?.iter().map(|(_, v)| Record::decode(v)).collect())
    }

    pub fn store_mut(&mut self) -> &mut Store {
        &mut self.store
    }
}

/// Does block 0 of `cache` hold a LIFS superblock?
pub fn probe(cache: &mut BlockCache) -> bool {
    let mut raw = [0u8; BLOCK];
    cache.read(0, &mut raw).is_ok() && Superblock::decode(&raw, cache.block_count()).is_ok()
}

/// The superblock flags of a LIFS volume on `cache`, if it holds one.
pub fn flags_of(cache: &mut BlockCache) -> Option<u32> {
    let mut raw = [0u8; BLOCK];
    cache.read(0, &mut raw).ok()?;
    Superblock::decode(&raw, cache.block_count()).ok().map(|sb| sb.flags)
}

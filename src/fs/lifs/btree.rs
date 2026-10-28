//! A B+ tree of 4 KiB blocks: the micro-database of LIFS (spec 4.5).
//!
//! Four uses, one code: the node table (key: node id), every directory
//! (key: hash of the name, then a disambiguator), the reverse index (key:
//! node, then directory), the refcounts of shared extents (key: first
//! block). Keys are 128 bits (`Key`), values a fixed size per tree.
//!
//! ## Block layout
//!
//! ```text
//!   0  "LB"          magic
//!   2  level         0 for a leaf
//!   4  count  u16
//!   6  value size u16  (8 in an inner block: the child's block)
//!   16 entries       key (hi u64, lo u64), then the value
//! ```
//!
//! In an inner block, an entry's key is a **lower bound** of its child's
//! keys: a search takes the last entry whose key is at most the target.
//! Removal does not borrow from siblings; it merges a child with its
//! neighbour when both fit in one block, frees a block that empties (its
//! entry removed from its parent), and replaces a root with one child by
//! that child -- so a lower bound may become smaller than its child's
//! smallest key, which routing tolerates. Every block goes through the store: in the journal's
//! transaction until the commit.

use alloc::vec::Vec;

use super::format::{get_u16, get_u64, put_u16, put_u64, BLOCK};
use super::journal::Store;
use crate::fs::FsError;

/// A 128-bit key, compared by `hi` then `lo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
    pub hi: u64,
    pub lo: u64,
}

impl Key {
    pub const MIN: Key = Key { hi: 0, lo: 0 };
    pub const MAX: Key = Key { hi: u64::MAX, lo: u64::MAX };

    pub const fn new(hi: u64, lo: u64) -> Key {
        Key { hi, lo }
    }
}

const HEADER: usize = 16;
const CHILD: usize = 8;

/// What a tree needs of its volume: the store, and blocks.
pub trait Space {
    fn store(&mut self) -> &mut Store;
    /// A block for the tree, blank in the transaction.
    fn alloc_meta(&mut self) -> Result<u64, FsError>;
    /// Gives a tree block back (after the commit).
    fn free_meta(&mut self, block: u64);
}

type Entries = Vec<(Key, Vec<u8>)>;

fn capacity(value_size: usize) -> usize {
    (BLOCK - HEADER) / (16 + value_size)
}

fn decode(image: &[u8]) -> Result<(u8, usize, Entries), FsError> {
    if &image[0..2] != b"LB" {
        return Err(FsError::Corrupt);
    }
    let level = image[2];
    let count = get_u16(image, 4) as usize;
    let size = get_u16(image, 6) as usize;
    if size == 0 || count > capacity(size) {
        return Err(FsError::Corrupt);
    }
    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let at = HEADER + i * (16 + size);
        entries.push((Key::new(get_u64(image, at), get_u64(image, at + 8)), image[at + 16..at + 16 + size].to_vec()));
    }
    Ok((level, size, entries))
}

fn encode(level: u8, size: usize, entries: &Entries) -> [u8; BLOCK] {
    let mut image = [0u8; BLOCK];
    image[0..2].copy_from_slice(b"LB");
    image[2] = level;
    put_u16(&mut image, 4, entries.len() as u16);
    put_u16(&mut image, 6, size as u16);
    for (i, (key, value)) in entries.iter().enumerate() {
        let at = HEADER + i * (16 + size);
        put_u64(&mut image, at, key.hi);
        put_u64(&mut image, at + 8, key.lo);
        image[at + 16..at + 16 + size].copy_from_slice(value);
    }
    image
}

fn load<S: Space + ?Sized>(space: &mut S, block: u64) -> Result<(u8, usize, Entries), FsError> {
    let image = space.store().get(block)?;
    decode(&image[..])
}

fn save<S: Space + ?Sized>(space: &mut S, block: u64, level: u8, size: usize, entries: &Entries) {
    space.store().put(block, &encode(level, size, entries));
}

/// Index of the child a key belongs to.
fn route(entries: &Entries, key: Key) -> usize {
    match entries.iter().rposition(|(k, _)| *k <= key) {
        Some(index) => index,
        None => 0,
    }
}

fn child(entries: &Entries, index: usize) -> u64 {
    get_u64(&entries[index].1, 0)
}

fn child_value(block: u64) -> Vec<u8> {
    block.to_le_bytes().to_vec()
}

/// The value of `key`.
pub fn get<S: Space + ?Sized>(space: &mut S, root: u64, key: Key) -> Result<Option<Vec<u8>>, FsError> {
    if root == 0 {
        return Ok(None);
    }
    let mut block = root;
    for _ in 0..64 {
        let (level, _, entries) = load(space, block)?;
        if level == 0 {
            return Ok(entries.into_iter().find(|(k, _)| *k == key).map(|(_, v)| v));
        }
        if entries.is_empty() {
            return Err(FsError::Corrupt);
        }
        block = child(&entries, route(&entries, key));
    }
    Err(FsError::Corrupt)
}

/// Inserts or replaces `key`. `root` may change.
pub fn insert<S: Space + ?Sized>(space: &mut S, root: &mut u64, size: usize, key: Key, value: &[u8]) -> Result<(), FsError> {
    if value.len() != size {
        return Err(FsError::Corrupt);
    }
    if *root == 0 {
        let block = space.alloc_meta()?;
        save(space, block, 0, size, &alloc::vec![(key, value.to_vec())]);
        *root = block;
        return Ok(());
    }
    if let Some((split_key, split_block)) = insert_in(space, *root, size, key, value, 0)? {
        let (_, _, old) = load(space, *root)?;
        let first = old.first().map(|(k, _)| *k).unwrap_or(Key::MIN).min(key);
        let (level, _, _) = load(space, *root)?;
        let block = space.alloc_meta()?;
        let entries = alloc::vec![(first, child_value(*root)), (split_key, child_value(split_block))];
        save(space, block, level + 1, CHILD, &entries);
        *root = block;
    }
    Ok(())
}

fn insert_in<S: Space + ?Sized>(
    space: &mut S,
    block: u64,
    size: usize,
    key: Key,
    value: &[u8],
    depth: usize,
) -> Result<Option<(Key, u64)>, FsError> {
    if depth > 64 {
        return Err(FsError::Corrupt);
    }
    let (level, stored, mut entries) = load(space, block)?;
    if level == 0 {
        if stored != size {
            return Err(FsError::Corrupt);
        }
        match entries.binary_search_by(|(k, _)| k.cmp(&key)) {
            Ok(index) => entries[index].1 = value.to_vec(),
            Err(index) => entries.insert(index, (key, value.to_vec())),
        }
    } else {
        if entries.is_empty() {
            return Err(FsError::Corrupt);
        }
        let index = route(&entries, key);
        if key < entries[index].0 {
            entries[index].0 = key;
        }
        let target = child(&entries, index);
        if let Some((split_key, split_block)) = insert_in(space, target, size, key, value, depth + 1)? {
            entries.insert(index + 1, (split_key, child_value(split_block)));
        }
    }
    let value_size = if level == 0 { size } else { CHILD };
    if entries.len() <= capacity(value_size) {
        save(space, block, level, value_size, &entries);
        return Ok(None);
    }
    let right = entries.split_off(entries.len() / 2);
    let right_block = space.alloc_meta()?;
    save(space, block, level, value_size, &entries);
    save(space, right_block, level, value_size, &right);
    Ok(Some((right[0].0, right_block)))
}

/// Removes `key`. Returns its value, if it was there. `root` may change.
pub fn remove<S: Space + ?Sized>(space: &mut S, root: &mut u64, key: Key) -> Result<Option<Vec<u8>>, FsError> {
    if *root == 0 {
        return Ok(None);
    }
    let (removed, empty) = remove_in(space, *root, key, true, 0)?;
    if empty {
        space.free_meta(*root);
        *root = 0;
        return Ok(removed);
    }
    // A root with one child is replaced by it.
    loop {
        let (level, _, entries) = load(space, *root)?;
        if level == 0 || entries.len() != 1 {
            break;
        }
        let only = child(&entries, 0);
        space.free_meta(*root);
        *root = only;
    }
    Ok(removed)
}

fn remove_in<S: Space + ?Sized>(
    space: &mut S,
    block: u64,
    key: Key,
    is_root: bool,
    depth: usize,
) -> Result<(Option<Vec<u8>>, bool), FsError> {
    if depth > 64 {
        return Err(FsError::Corrupt);
    }
    let (level, size, mut entries) = load(space, block)?;
    let removed;
    if level == 0 {
        match entries.binary_search_by(|(k, _)| k.cmp(&key)) {
            Ok(index) => removed = Some(entries.remove(index).1),
            Err(_) => return Ok((None, false)),
        }
    } else {
        if entries.is_empty() {
            return Err(FsError::Corrupt);
        }
        let index = route(&entries, key);
        let target = child(&entries, index);
        let (value, empty) = remove_in(space, target, key, false, depth + 1)?;
        if value.is_none() {
            return Ok((None, false));
        }
        removed = value;
        if empty {
            space.free_meta(target);
            entries.remove(index);
        } else {
            merge_neighbours(space, &mut entries, index)?;
        }
    }
    // Empty: its parent drops it, or `remove` frees the root.
    let _ = is_root;
    if entries.is_empty() {
        return Ok((removed, true));
    }
    save(space, block, level, size, &entries);
    Ok((removed, false))
}

/// Child `index` of an inner block just lost an entry: if it and a
/// neighbour now fit in one block, they become one. Without it a tree
/// that grew and shrank back keeps the blocks of its splits -- the node
/// tree after a thousand files created and removed would stay a few
/// blocks larger, and a volume's free count would not come back to where
/// it was.
fn merge_neighbours<S: Space + ?Sized>(space: &mut S, entries: &mut Entries, index: usize) -> Result<(), FsError> {
    if entries.len() < 2 {
        return Ok(());
    }
    let left_index = if index + 1 < entries.len() { index } else { index - 1 };
    let (left_block, right_block) = (child(entries, left_index), child(entries, left_index + 1));
    let (level, size, mut left) = load(space, left_block)?;
    let (right_level, right_size, right) = load(space, right_block)?;
    if level != right_level || size != right_size || left.len() + right.len() > capacity(size) {
        return Ok(());
    }
    left.extend(right);
    save(space, left_block, level, size, &left);
    space.free_meta(right_block);
    entries.remove(left_index + 1);
    Ok(())
}

/// Every entry of key in `from ..= to`, in key order.
pub fn range<S: Space + ?Sized>(space: &mut S, root: u64, from: Key, to: Key) -> Result<Entries, FsError> {
    let mut out = Vec::new();
    if root != 0 {
        range_in(space, root, from, to, &mut out, 0)?;
    }
    Ok(out)
}

fn range_in<S: Space + ?Sized>(space: &mut S, block: u64, from: Key, to: Key, out: &mut Entries, depth: usize) -> Result<(), FsError> {
    if depth > 64 {
        return Err(FsError::Corrupt);
    }
    let (level, _, entries) = load(space, block)?;
    if level == 0 {
        out.extend(entries.into_iter().filter(|(k, _)| *k >= from && *k <= to));
        return Ok(());
    }
    for index in 0..entries.len() {
        // Child `index` holds keys from its bound (child 0: from anything)
        // up to the next bound, excluded.
        let low_ok = index == 0 || entries[index].0 <= to;
        let high_ok = index + 1 >= entries.len() || entries[index + 1].0 > from;
        if low_ok && high_ok {
            range_in(space, child(&entries, index), from, to, out, depth + 1)?;
        }
    }
    Ok(())
}

/// The entry of greatest key at most `key`.
pub fn last_at_most<S: Space + ?Sized>(space: &mut S, root: u64, key: Key) -> Result<Option<(Key, Vec<u8>)>, FsError> {
    if root == 0 {
        return Ok(None);
    }
    last_in(space, root, key, 0)
}

fn last_in<S: Space + ?Sized>(space: &mut S, block: u64, key: Key, depth: usize) -> Result<Option<(Key, Vec<u8>)>, FsError> {
    if depth > 64 {
        return Err(FsError::Corrupt);
    }
    let (level, _, entries) = load(space, block)?;
    if level == 0 {
        return Ok(entries.into_iter().filter(|(k, _)| *k <= key).next_back());
    }
    // Lower bounds may be stale (smaller than their child's keys): the
    // answer can sit in any child up to the routed one.
    let routed = route(&entries, key);
    for index in (0..=routed).rev() {
        if let Some(found) = last_in(space, child(&entries, index), key, depth + 1)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

/// Frees every block of a tree.
pub fn free_all<S: Space + ?Sized>(space: &mut S, root: u64) -> Result<(), FsError> {
    if root == 0 {
        return Ok(());
    }
    free_in(space, root, 0)
}

fn free_in<S: Space + ?Sized>(space: &mut S, block: u64, depth: usize) -> Result<(), FsError> {
    if depth > 64 {
        return Err(FsError::Corrupt);
    }
    let (level, _, entries) = load(space, block)?;
    if level > 0 {
        for index in 0..entries.len() {
            free_in(space, child(&entries, index), depth + 1)?;
        }
    }
    space.free_meta(block);
    Ok(())
}

/// Every block of a tree, for the check.
pub fn blocks<S: Space + ?Sized>(space: &mut S, root: u64, out: &mut Vec<u64>) -> Result<(), FsError> {
    if root == 0 {
        return Ok(());
    }
    blocks_in(space, root, out, 0)
}

fn blocks_in<S: Space + ?Sized>(space: &mut S, block: u64, out: &mut Vec<u64>, depth: usize) -> Result<(), FsError> {
    if depth > 64 {
        return Err(FsError::Corrupt);
    }
    out.push(block);
    let (level, _, entries) = load(space, block)?;
    if level > 0 {
        for index in 0..entries.len() {
            blocks_in(space, child(&entries, index), out, depth + 1)?;
        }
    }
    Ok(())
}

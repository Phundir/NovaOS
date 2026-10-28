//! Directories and the reverse index.
//!
//! A directory is a tree of entries (`btree`): the key is the name's hash
//! and a disambiguator, the value the target node and the name. A lookup
//! reads the entries of that hash -- almost always one -- and compares the
//! names (point 13 of 14.1: equal hashes are told apart by the name).
//!
//! Every entry added or removed also changes the **reverse index**: key
//! (node, directory), value the number of entries of that directory naming
//! the node. It is what answers "which directories tag this file"
//! (`SYS_FS_NODE_TAGS`) without walking the volume.

use alloc::vec::Vec;

use super::btree::{self, Key};
use super::format::{get_u64, name_hash, put_u64, valid_name, Entry, Record, ENTRY_SIZE, REVERSE_SIZE};
use super::Volume;
use crate::fs::FsError;

impl Volume {
    /// The entry `name` of a directory: its key and its target.
    pub fn lookup(&mut self, dir: &Record, name: &str) -> Result<Option<(Key, u64)>, FsError> {
        let hash = name_hash(name.as_bytes());
        let found = btree::range(self, dir.tree, Key::new(hash, 0), Key::new(hash, u64::MAX))?;
        Ok(found.into_iter().find(|(_, v)| Entry::named(v, name.as_bytes())).map(|(k, v)| (k, get_u64(&v, 0))))
    }

    /// Adds entry `name` -> `target` to `dir` (reloaded and saved here).
    pub fn add_entry(&mut self, dir_id: u64, name: &str, target: u64, now: u64) -> Result<(), FsError> {
        if !valid_name(name) {
            return Err(FsError::InvalidName);
        }
        let mut dir = self.load_record(dir_id)?;
        if !dir.is_dir() {
            return Err(FsError::NotADirectory);
        }
        let hash = name_hash(name.as_bytes());
        let same = btree::range(self, dir.tree, Key::new(hash, 0), Key::new(hash, u64::MAX))?;
        if same.iter().any(|(_, v)| Entry::named(v, name.as_bytes())) {
            return Err(FsError::AlreadyExists);
        }
        let lo = same.iter().map(|(k, _)| k.lo + 1).max().unwrap_or(0);
        let value = Entry { target, name: alloc::string::String::from(name) }.encode();
        let mut root = dir.tree;
        btree::insert(self, &mut root, ENTRY_SIZE, Key::new(hash, lo), &value)?;
        dir.tree = root;
        dir.entries += 1;
        dir.modified = now;
        self.save_record(&dir)?;
        self.reverse_adjust(target, dir_id, 1)
    }

    /// Removes entry `name` of `dir`. Returns its target.
    pub fn remove_entry(&mut self, dir_id: u64, name: &str, now: u64) -> Result<u64, FsError> {
        let mut dir = self.load_record(dir_id)?;
        let (key, target) = self.lookup(&dir, name)?.ok_or(FsError::NotFound)?;
        let mut root = dir.tree;
        btree::remove(self, &mut root, key)?;
        dir.tree = root;
        dir.entries = dir.entries.saturating_sub(1);
        dir.modified = now;
        self.save_record(&dir)?;
        self.reverse_adjust(target, dir_id, -1)?;
        Ok(target)
    }

    /// Every entry of a directory, in key order.
    pub fn entries_of(&mut self, dir: &Record) -> Result<Vec<Entry>, FsError> {
        Ok(btree::range(self, dir.tree, Key::MIN, Key::MAX)?.iter().map(|(_, v)| Entry::decode(v)).collect())
    }

    fn reverse_adjust(&mut self, node: u64, dir: u64, delta: i64) -> Result<(), FsError> {
        let key = Key::new(node, dir);
        let root = self.sb.reverse_root;
        let count = btree::get(self, root, key)?.map(|v| get_u64(&v, 0)).unwrap_or(0) as i64 + delta;
        let mut root = self.sb.reverse_root;
        if count <= 0 {
            btree::remove(self, &mut root, key)?;
        } else {
            let mut value = [0u8; REVERSE_SIZE];
            put_u64(&mut value, 0, count as u64);
            btree::insert(self, &mut root, REVERSE_SIZE, key, &value)?;
        }
        self.sb.reverse_root = root;
        self.sb_dirty = true;
        Ok(())
    }

    /// The directories naming `node`, with how many entries each.
    pub fn reverse_of(&mut self, node: u64) -> Result<Vec<(u64, u64)>, FsError> {
        let root = self.sb.reverse_root;
        Ok(btree::range(self, root, Key::new(node, 0), Key::new(node, u64::MAX))?
            .iter()
            .map(|(k, v)| (k.lo, get_u64(v, 0)))
            .collect())
    }
}

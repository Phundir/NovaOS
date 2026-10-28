//! The operations of a LIFS volume, by path and by identifier, with the
//! rights of spec 5.1 -- the same rule as NovaFS (`volume::node_allows`),
//! the same errors, the same audit.
//!
//! Every operation reloads the records it changes rather than trusting a
//! copy read earlier: two entries of one directory changed in a row from a
//! stale copy would lose one of them. Every public operation starts by
//! committing if the transaction is half the journal (`commit_if_full`);
//! the commit of the rest is the group commit's.

use alloc::string::String;
use alloc::vec::Vec;

use super::format::{Record, BLOCK, KIND_DIR, KIND_FILE, ROOT};
use super::Volume;
use crate::fs::audit;
use crate::fs::block::MediumKind;
use crate::fs::filesystem::{Filesystem, TagEntry, VolumeInfo};
use crate::fs::format::{Link, Node, NodeId};
use crate::fs::perm::{Credentials, Gid, Mode, Permissions, Uid};
use crate::fs::volume::node_allows;
use crate::fs::FsError;

impl Volume {
    fn allows(&self, record: &Record, who: Credentials, permission: u64) -> bool {
        node_allows(&record.view(), who, permission)
    }

    fn require(&self, record: &Record, who: Credentials, permission: u64, path: &str) -> Result<(), FsError> {
        let granted = self.allows(record, who, permission);
        audit::record(who, path, permission, granted);
        if granted { Ok(()) } else { Err(FsError::PermissionDenied(permission)) }
    }

    /// The record a path names; every directory crossed demands `access`.
    fn walk(&mut self, path: &str, who: Credentials) -> Result<Record, FsError> {
        let mut record = self.load_record(ROOT)?;
        for component in path.split('/').filter(|c| !c.is_empty()) {
            if !record.is_dir() {
                return Err(FsError::NotADirectory);
            }
            self.require(&record, who, Permissions::ACCESS, path)?;
            let (_, target) = self.lookup(&record, component)?.ok_or(FsError::NotFound)?;
            record = self.load_record(target)?;
        }
        Ok(record)
    }

    /// The parent directory of a path, and the last component.
    fn walk_parent(&mut self, path: &str, who: Credentials) -> Result<(Record, String), FsError> {
        let trimmed = path.trim_end_matches('/');
        let (parent, name) = match trimmed.rfind('/') {
            Some(index) => (&trimmed[..index], &trimmed[index + 1..]),
            None => ("", trimmed),
        };
        if name.is_empty() {
            return Err(FsError::InvalidName);
        }
        let parent = self.walk(parent, who)?;
        if !parent.is_dir() {
            return Err(FsError::NotADirectory);
        }
        Ok((parent, String::from(name)))
    }

    fn set_mode(record: &mut Record, mode: Mode) {
        let applicable = record.view().kind.applicable_permissions();
        record.perm_owner = (mode.owner.0 & applicable) as u16;
        record.perm_group = (mode.group.0 & applicable) as u16;
        record.perm_other = (mode.other.0 & applicable) as u16;
    }

    fn make_directory(&mut self, path: &str, who: Credentials, mode: Mode, now: u64) -> Result<NodeId, FsError> {
        let (parent, name) = self.walk_parent(path, who)?;
        self.require(&parent, who, Permissions::WRITE, path)?;
        if self.lookup(&parent, &name)?.is_some() {
            return Err(FsError::AlreadyExists);
        }
        let mut record = self.new_record(KIND_DIR, who.uid, who.gid, now)?;
        Self::set_mode(&mut record, mode);
        record.links = 1;
        self.save_record(&record)?;
        self.add_entry(parent.id, &name, record.id, now)?;
        Ok(record.id)
    }

    /// A node loses one Tag: gone with its last (content, tree, record).
    fn unlink_node(&mut self, id: u64) -> Result<(), FsError> {
        let mut record = self.load_record(id)?;
        record.links = record.links.saturating_sub(1);
        if record.links > 0 {
            return self.save_record(&record);
        }
        if record.is_dir() {
            super::btree::free_all(self, record.tree)?;
        } else {
            self.release_content(&record)?;
        }
        self.drop_record(id)
    }

    fn remove_path(&mut self, dir: &Record, name: &str, who: Credentials, path: &str, now: u64) -> Result<(), FsError> {
        let (_, target) = self.lookup(dir, name)?.ok_or(FsError::NotFound)?;
        let record = self.load_record(target)?;
        self.require(&record, who, Permissions::DELETE, path)?;
        if record.is_dir() && record.entries > 0 {
            return Err(FsError::DirectoryNotEmpty);
        }
        self.remove_entry(dir.id, name, now)?;
        self.unlink_node(target)
    }

    fn relink(&mut self, from: &str, to: &str, who: Credentials, now: u64) -> Result<NodeId, FsError> {
        let (source, source_name) = self.walk_parent(from, who)?;
        let (_, target) = self.lookup(&source, &source_name)?.ok_or(FsError::NotFound)?;
        let record = self.load_record(target)?;
        self.require(&record, who, Permissions::MOVE, from)?;
        let (destination, destination_name) = self.walk_parent(to, who)?;
        self.require(&destination, who, Permissions::WRITE, to)?;
        if self.lookup(&destination, &destination_name)?.is_some() {
            return Err(FsError::AlreadyExists);
        }
        self.add_entry(destination.id, &destination_name, target, now)?;
        self.remove_entry(source.id, &source_name, now)?;
        Ok(target)
    }

    /// A clone of file `source` as entry `name` of `dir`, replacing a file
    /// there (with `overwrite`). The new node shares the extents (CoW).
    fn clone_into(&mut self, source: &Record, dir: &Record, name: &str, who: Credentials, path: &str, now: u64) -> Result<NodeId, FsError> {
        if let Some((_, existing)) = self.lookup(dir, name)? {
            let mut record = self.load_record(existing)?;
            if record.is_dir() {
                return Err(FsError::IsADirectory);
            }
            if existing == source.id {
                return Ok(existing);
            }
            self.require(&record, who, Permissions::OVERWRITE, path)?;
            self.release_content(&record)?;
            self.share_content(source, &mut record)?;
            record.modified = now;
            self.save_record(&record)?;
            return Ok(existing);
        }
        self.require(dir, who, Permissions::WRITE, path)?;
        let mut record = self.new_record(KIND_FILE, who.uid, who.gid, now)?;
        record.perm_owner = source.perm_owner;
        record.perm_group = source.perm_group;
        record.perm_other = source.perm_other;
        record.links = 1;
        self.share_content(source, &mut record)?;
        self.save_record(&record)?;
        self.add_entry(dir.id, name, record.id, now)?;
        Ok(record.id)
    }
}

impl Filesystem for Volume {
    fn letter(&self) -> char {
        self.letter
    }

    fn set_letter(&mut self, letter: char) {
        self.letter = letter;
    }

    fn info(&self) -> VolumeInfo {
        VolumeInfo {
            label: self.sb.label.clone(),
            format: "LIFS",
            total_blocks: self.sb.total_blocks,
            // The runs freed in the open transaction count as free: they
            // are, at the next commit -- two seconds at most.
            free_blocks: self.sb.free_blocks + self.pending.values().sum::<u64>(),
            node_count: self.sb.node_count,
            // No node table: as many nodes as the free blocks hold records.
            free_nodes: self.sb.free_blocks * (BLOCK / 272) as u64,
        }
    }

    fn name(&self) -> String {
        Volume::name(self)
    }

    fn medium(&self) -> MediumKind {
        Volume::medium(self)
    }

    fn block_count(&mut self) -> u64 {
        Volume::block_count(self)
    }

    fn raw_read(&mut self, block: u64, buffer: &mut [u8]) -> Result<(), FsError> {
        Volume::raw_read(self, block, buffer)
    }

    fn flush(&mut self) -> Result<(), FsError> {
        self.commit()?;
        self.store_mut().cache.flush().map_err(FsError::Block)
    }

    fn tick(&mut self) -> Result<(), FsError> {
        Volume::tick(self)
    }

    fn read_node(&mut self, id: NodeId) -> Result<Node, FsError> {
        Ok(self.load_record(id)?.view())
    }

    fn read_range(&mut self, node: &Node, offset: u64, buffer: &mut [u8]) -> Result<usize, FsError> {
        let record = self.load_record(node.id)?;
        if record.is_dir() {
            return Err(FsError::IsADirectory);
        }
        self.read_at(&record, offset, buffer)
    }

    fn write_range(&mut self, node: &mut Node, offset: u64, data: &[u8], now: u64) -> Result<usize, FsError> {
        self.commit_if_full()?;
        let mut record = self.load_record(node.id)?;
        if record.is_dir() {
            return Err(FsError::IsADirectory);
        }
        let written = self.write_at(&mut record, offset, data, now)?;
        *node = record.view();
        Ok(written)
    }

    fn truncate_node(&mut self, node: &mut Node, size: u64, now: u64) -> Result<(), FsError> {
        self.commit_if_full()?;
        let mut record = self.load_record(node.id)?;
        if record.is_dir() {
            return Err(FsError::IsADirectory);
        }
        self.truncate(&mut record, size, now)?;
        *node = record.view();
        Ok(())
    }

    unsafe fn read_blocks_into_frames(&mut self, node: &Node, first: u64, frames: &[u64]) -> Result<(), FsError> {
        let record = self.load_record(node.id)?;
        let blocks = self.physical(&record, first, frames.len())?;
        let mut index = 0;
        while index < frames.len() {
            if blocks[index] == 0 {
                // A hole: zeros, no command.
                let at = crate::memory::phys_to_virt(frames[index]) as *mut u8;
                // SAFETY: a frame of the caller's, 4 KiB.
                unsafe { core::ptr::write_bytes(at, 0, BLOCK) };
                index += 1;
                continue;
            }
            let mut run = 1;
            while index + run < frames.len() && blocks[index + run] == blocks[index] + run as u64 {
                run += 1;
            }
            // SAFETY: passed on from this function's contract.
            unsafe { self.store_mut().cache.read_into_frames(blocks[index], &frames[index..index + run]).map_err(FsError::Block)? };
            index += run;
        }
        Ok(())
    }

    fn resolve(&mut self, path: &str, who: Credentials) -> Result<Node, FsError> {
        Ok(self.walk(path, who)?.view())
    }

    fn require_permission(&mut self, node: &Node, who: Credentials, permission: u64, path: &str) -> Result<(), FsError> {
        let granted = node_allows(node, who, permission);
        audit::record(who, path, permission, granted);
        if granted { Ok(()) } else { Err(FsError::PermissionDenied(permission)) }
    }

    fn create_path(&mut self, path: &str, who: Credentials, mode: Mode, now: u64) -> Result<NodeId, FsError> {
        self.commit_if_full()?;
        let mut current = String::new();
        let mut last = ROOT;
        for component in path.split('/').filter(|c| !c.is_empty()) {
            current.push('/');
            current.push_str(component);
            match self.walk(&current, who) {
                Ok(record) if record.is_dir() => last = record.id,
                Ok(_) => return Err(FsError::NotADirectory),
                Err(FsError::NotFound) => last = self.make_directory(&current, who, mode, now)?,
                Err(other) => return Err(other),
            }
        }
        Ok(last)
    }

    fn write_file(&mut self, path: &str, content: &[u8], who: Credentials, mode: Mode, now: u64) -> Result<NodeId, FsError> {
        self.commit_if_full()?;
        let (parent, name) = self.walk_parent(path, who)?;
        if let Some((_, existing)) = self.lookup(&parent, &name)? {
            let mut record = self.load_record(existing)?;
            if record.is_dir() {
                return Err(FsError::IsADirectory);
            }
            self.require(&record, who, Permissions::OVERWRITE, path)?;
            self.truncate(&mut record, 0, now)?;
            self.write_at(&mut record, 0, content, now)?;
            return Ok(existing);
        }
        self.require(&parent, who, Permissions::WRITE, path)?;
        let mut record = self.new_record(KIND_FILE, who.uid, who.gid, now)?;
        Self::set_mode(&mut record, mode);
        record.links = 1;
        self.save_record(&record)?;
        if !content.is_empty() {
            self.write_at(&mut record, 0, content, now)?;
        }
        self.add_entry(parent.id, &name, record.id, now)?;
        Ok(record.id)
    }

    fn read_file(&mut self, path: &str, who: Credentials) -> Result<Vec<u8>, FsError> {
        let record = self.walk(path, who)?;
        if record.is_dir() {
            return Err(FsError::IsADirectory);
        }
        self.require(&record, who, Permissions::READ, path)?;
        let mut content = alloc::vec![0u8; record.size as usize];
        let read = self.read_at(&record, 0, &mut content)?;
        content.truncate(read);
        Ok(content)
    }

    fn delete(&mut self, path: &str, who: Credentials, now: u64) -> Result<(), FsError> {
        self.commit_if_full()?;
        let (parent, name) = self.walk_parent(path, who)?;
        self.remove_path(&parent, &name, who, path, now)
    }

    fn rename(&mut self, path: &str, new_name: &str, who: Credentials, now: u64) -> Result<(), FsError> {
        self.commit_if_full()?;
        let (parent, name) = self.walk_parent(path, who)?;
        let (_, target) = self.lookup(&parent, &name)?.ok_or(FsError::NotFound)?;
        let record = self.load_record(target)?;
        self.require(&record, who, Permissions::RENAME, path)?;
        if name == new_name {
            return Ok(());
        }
        if self.lookup(&parent, new_name)?.is_some() {
            return Err(FsError::AlreadyExists);
        }
        self.add_entry(parent.id, new_name, target, now)?;
        self.remove_entry(parent.id, &name, now)?;
        Ok(())
    }

    fn move_entry(&mut self, from: &str, to: &str, who: Credentials, now: u64) -> Result<NodeId, FsError> {
        self.commit_if_full()?;
        self.relink(from, to, who, now)
    }

    fn move_entries(&mut self, moves: &[(String, String)], who: Credentials, now: u64) -> Result<usize, (usize, FsError)> {
        let mut moved = 0;
        for (from, to) in moves {
            if let Err(error) = self.commit_if_full() {
                return Err((moved, error));
            }
            match self.relink(from, to, who, now) {
                Ok(_) => moved += 1,
                Err(error) => return Err((moved, error)),
            }
        }
        Ok(moved)
    }

    fn link(&mut self, from: &str, to: &str, who: Credentials, now: u64) -> Result<NodeId, FsError> {
        self.commit_if_full()?;
        let record = self.walk(from, who)?;
        self.require(&record, who, Permissions::LINK, from)?;
        let (destination, name) = self.walk_parent(to, who)?;
        self.require(&destination, who, Permissions::WRITE, to)?;
        self.add_entry(destination.id, &name, record.id, now)?;
        let mut record = self.load_record(record.id)?;
        record.links += 1;
        self.save_record(&record)?;
        Ok(record.id)
    }

    /// The entry is replaced inside one transaction of the journal: the
    /// removal of the old entry, the addition of the new one, the link
    /// counts and the reverse index commit together or not at all (no
    /// commit happens in the middle of an operation, DECISIONS, phase 14).
    ///
    /// Files only, on both sides: a LIFS directory is named exactly once
    /// (`check_volume`), which is what keeps its tree free of cycles.
    fn retarget(&mut self, path: &str, target: &str, who: Credentials, now: u64) -> Result<NodeId, FsError> {
        self.commit_if_full()?;
        let node = self.walk(target, who)?;
        if node.is_dir() {
            return Err(FsError::IsADirectory);
        }
        self.require(&node, who, Permissions::LINK, target)?;
        let (parent, name) = self.walk_parent(path, who)?;
        self.require(&parent, who, Permissions::WRITE, path)?;
        let previous = self.lookup(&parent, &name)?.map(|(_, id)| id);
        if previous == Some(node.id) {
            return Ok(node.id);
        }
        if let Some(previous) = previous {
            let old = self.load_record(previous)?;
            if old.is_dir() {
                return Err(FsError::IsADirectory);
            }
            self.require(&old, who, Permissions::DELETE, path)?;
            self.remove_entry(parent.id, &name, now)?;
        }
        self.add_entry(parent.id, &name, node.id, now)?;
        let mut record = self.load_record(node.id)?;
        record.links += 1;
        self.save_record(&record)?;
        if let Some(previous) = previous {
            self.unlink_node(previous)?;
        }
        Ok(node.id)
    }

    /// Inside a LIFS volume, a copy is a **clone** (spec 4.5): a new node
    /// sharing the extents, in time independent of the size.
    fn copy(&mut self, from: &str, to: &str, who: Credentials, now: u64) -> Result<NodeId, FsError> {
        self.commit_if_full()?;
        let source = self.walk(from, who)?;
        if source.is_dir() {
            return Err(FsError::IsADirectory);
        }
        self.require(&source, who, Permissions::COPY, from)?;
        self.require(&source, who, Permissions::READ, from)?;
        let (dir, name) = self.walk_parent(to, who)?;
        self.clone_into(&source, &dir, &name, who, to, now)
    }

    fn stat(&mut self, path: &str, who: Credentials) -> Result<Node, FsError> {
        Ok(self.walk(path, who)?.view())
    }

    fn chmod(&mut self, path: &str, mode: Mode, who: Credentials, now: u64) -> Result<(), FsError> {
        self.commit_if_full()?;
        let mut record = self.walk(path, who)?;
        self.require(&record, who, Permissions::CHMOD, path)?;
        if mode.owner.unknown_bits() | mode.group.unknown_bits() | mode.other.unknown_bits() != 0 {
            return Err(FsError::InvalidPermission);
        }
        Self::set_mode(&mut record, mode);
        record.modified = now;
        self.save_record(&record)
    }

    fn chown(&mut self, path: &str, owner: Uid, group: Gid, who: Credentials, now: u64) -> Result<(), FsError> {
        self.commit_if_full()?;
        let mut record = self.walk(path, who)?;
        self.require(&record, who, Permissions::OWN, path)?;
        record.owner = owner;
        record.group = group;
        record.modified = now;
        self.save_record(&record)
    }

    fn list(&mut self, path: &str, who: Credentials) -> Result<Vec<Link>, FsError> {
        let record = self.walk(path, who)?;
        if !record.is_dir() {
            return Err(FsError::NotADirectory);
        }
        self.require(&record, who, Permissions::ACCESS, path)?;
        Ok(self.entries_of(&record)?.into_iter().map(|e| Link { name: e.name, node: e.target }).collect())
    }

    fn is_executable(&mut self, path: &str, who: Credentials) -> Result<bool, FsError> {
        let (parent, name) = self.walk_parent(path, who)?;
        let (_, target) = self.lookup(&parent, &name)?.ok_or(FsError::NotFound)?;
        let record = self.load_record(target)?;
        let granted = self.allows(&record, who, Permissions::EXECUTE) && self.allows(&parent, who, Permissions::EXECUTE);
        audit::record(who, path, Permissions::EXECUTE, granted);
        Ok(granted)
    }

    // --- by identifier (spec 6.2) ---

    fn node_stat(&mut self, id: NodeId, who: Credentials) -> Result<Node, FsError> {
        let record = self.load_record(id)?;
        if id != ROOT && Filesystem::node_tags(self, id, who)?.is_empty() {
            return Err(FsError::PermissionDenied(Permissions::ACCESS));
        }
        Ok(record.view())
    }

    fn node_path(&mut self, id: NodeId) -> Result<String, FsError> {
        let mut parts: Vec<String> = Vec::new();
        let mut at = id;
        for _ in 0..4096 {
            if at == ROOT {
                let mut path = String::new();
                for part in parts.iter().rev() {
                    path.push('/');
                    path.push_str(part);
                }
                if path.is_empty() {
                    path.push('/');
                }
                return Ok(path);
            }
            let (dir, _) = *self.reverse_of(at)?.first().ok_or(FsError::NotFound)?;
            let directory = self.load_record(dir)?;
            let name = self.entries_of(&directory)?.into_iter().find(|e| e.target == at).ok_or(FsError::Corrupt)?.name;
            parts.push(name);
            at = dir;
        }
        Err(FsError::Corrupt)
    }

    fn node_move(&mut self, from: NodeId, name: &str, to: NodeId, who: Credentials, now: u64) -> Result<(), FsError> {
        self.commit_if_full()?;
        let source = self.load_record(from)?;
        let destination = self.load_record(to)?;
        if !source.is_dir() || !destination.is_dir() {
            return Err(FsError::NotADirectory);
        }
        self.require(&source, who, Permissions::ACCESS, name)?;
        let (_, target) = self.lookup(&source, name)?.ok_or(FsError::NotFound)?;
        let record = self.load_record(target)?;
        self.require(&record, who, Permissions::MOVE, name)?;
        self.require(&destination, who, Permissions::WRITE, name)?;
        if target == to || (record.is_dir() && self.contains(target, to)?) {
            return Err(FsError::InvalidPath);
        }
        if self.lookup(&destination, name)?.is_some() {
            return Err(FsError::AlreadyExists);
        }
        self.add_entry(to, name, target, now)?;
        self.remove_entry(from, name, now)?;
        Ok(())
    }

    fn node_clone(&mut self, id: NodeId, to: NodeId, name: &str, who: Credentials, now: u64) -> Result<NodeId, FsError> {
        self.commit_if_full()?;
        let source = self.load_record(id)?;
        if source.is_dir() {
            return Err(FsError::IsADirectory);
        }
        self.require(&source, who, Permissions::COPY, name)?;
        self.require(&source, who, Permissions::READ, name)?;
        let dir = self.load_record(to)?;
        if !dir.is_dir() {
            return Err(FsError::NotADirectory);
        }
        self.clone_into(&source, &dir, name, who, name, now)
    }

    fn node_tag(&mut self, id: NodeId, to: NodeId, name: &str, who: Credentials, now: u64) -> Result<(), FsError> {
        self.commit_if_full()?;
        let record = self.load_record(id)?;
        if record.is_dir() {
            // A directory under two names would make the tree a graph.
            return Err(FsError::IsADirectory);
        }
        self.require(&record, who, Permissions::LINK, name)?;
        let dir = self.load_record(to)?;
        if !dir.is_dir() {
            return Err(FsError::NotADirectory);
        }
        self.require(&dir, who, Permissions::WRITE, name)?;
        self.add_entry(to, name, id, now)?;
        let mut record = self.load_record(id)?;
        record.links += 1;
        self.save_record(&record)
    }

    fn node_unlink(&mut self, dir: NodeId, name: &str, who: Credentials, now: u64) -> Result<(), FsError> {
        self.commit_if_full()?;
        let directory = self.load_record(dir)?;
        if !directory.is_dir() {
            return Err(FsError::NotADirectory);
        }
        self.require(&directory, who, Permissions::ACCESS, name)?;
        self.remove_path(&directory, name, who, name, now)
    }

    fn node_tags(&mut self, id: NodeId, who: Credentials) -> Result<Vec<TagEntry>, FsError> {
        self.load_record(id)?;
        let mut out = Vec::new();
        for (dir, _) in self.reverse_of(id)? {
            let directory = self.load_record(dir)?;
            // Only what the caller could list anyway.
            if !self.allows(&directory, who, Permissions::ACCESS) {
                continue;
            }
            for entry in self.entries_of(&directory)? {
                if entry.target == id {
                    out.push(TagEntry { directory: dir, name: entry.name });
                }
            }
        }
        Ok(out)
    }

    // --- diagnostics ---

    fn dump_state(&mut self) {
        let sb = self.sb.clone();
        crate::serial_println!(
            "[lifs] volume \"{}\" ({}) - {} blocs de 4 Kio ({} Mio), {} libres",
            sb.label,
            self.medium().as_str(),
            sb.total_blocks,
            sb.total_blocks * BLOCK as u64 / (1024 * 1024),
            sb.free_blocks
        );
        crate::serial_println!(
            "[lifs]   {} noeuds (prochain {}), journal de {} blocs a {}, transaction {} - {} validation(s), {} bloc(s) journalises, {} en cours",
            sb.node_count,
            sb.next_node,
            sb.journal_blocks,
            sb.journal_start,
            sb.journal_seq,
            self.store.commits,
            self.store.journaled_blocks,
            self.store.pending()
        );
    }

    fn dump_tree(&mut self, path: &str, who: Credentials, depth: usize, limit: usize) {
        if depth > limit {
            return;
        }
        let Ok(mut links) = Filesystem::list(self, path, who) else {
            return;
        };
        links.sort_by(|a, b| a.name.cmp(&b.name));
        for link in links {
            let Ok(record) = self.load_record(link.node) else {
                continue;
            };
            let indent: String = core::iter::repeat_n("  ", depth).collect();
            if record.is_dir() {
                crate::serial_println!("[lifs]   {}{}/", indent, link.name);
                let child = if path == "/" { alloc::format!("/{}", link.name) } else { alloc::format!("{}/{}", path, link.name) };
                self.dump_tree(&child, who, depth + 1, limit);
            } else {
                crate::serial_println!("[lifs]   {}{}  ({} o)", indent, link.name, record.size);
            }
        }
    }

    fn check(&mut self) -> Result<(usize, usize), FsError> {
        self.check_volume()
    }
}

impl Volume {
    /// Is `inner` inside the tree of directory `outer` (or `outer` itself)?
    /// Walked upwards through the reverse index.
    fn contains(&mut self, outer: u64, inner: u64) -> Result<bool, FsError> {
        let mut at = inner;
        for _ in 0..4096 {
            if at == outer {
                return Ok(true);
            }
            if at == ROOT {
                return Ok(false);
            }
            let parents = self.reverse_of(at)?;
            let Some((parent, _)) = parents.first() else {
                return Ok(false);
            };
            at = *parent;
        }
        Err(FsError::Corrupt)
    }
}

//! What a mounted volume offers, whatever its format (phase 14).
//!
//! The mount table held NovaFS volumes and nothing else; with LIFS as the
//! native format (spec 4.5) -- the root included -- and NovaFS kept for the
//! volumes that exist, the table holds `Box<dyn Filesystem>`. The upper
//! layers (`fs`, the POSIX files, the explorer, the system calls) see the
//! same operations and the same records (`format::Node` as the metadata of
//! a node, `format::Link` as a directory entry) on both.
//!
//! The by-identifier operations of LIFS (spec 6.2, `SYS_FS_NODE_*`) have a
//! default refusal: NovaFS answers `Unsupported`.

use alloc::string::String;
use alloc::vec::Vec;

use super::block::MediumKind;
use super::format::{Link, Node, NodeId};
use super::perm::{Credentials, Gid, Mode, Uid};
use super::FsError;

/// Figures of a volume, for the disk list.
#[derive(Debug, Clone)]
pub struct VolumeInfo {
    pub label: String,
    /// "LIFS" or "NovaFS".
    pub format: &'static str,
    pub total_blocks: u64,
    pub free_blocks: u64,
    /// Nodes in use, and nodes still free (LIFS: no fixed table -- the
    /// free count is the number of nodes the free blocks could hold).
    pub node_count: u64,
    pub free_nodes: u64,
}

/// A directory entry that designates a node, as the reverse index gives it.
#[derive(Debug, Clone)]
pub struct TagEntry {
    pub directory: NodeId,
    pub name: String,
}

pub trait Filesystem: Send {
    fn letter(&self) -> char;
    fn set_letter(&mut self, letter: char);
    fn info(&self) -> VolumeInfo;
    /// The medium's name, for the log.
    fn name(&self) -> String;
    fn medium(&self) -> MediumKind;

    /// Blocks of the medium, and a raw read of one (the storage service).
    fn block_count(&mut self) -> u64;
    fn raw_read(&mut self, block: u64, buffer: &mut [u8]) -> Result<(), FsError>;

    /// Makes everything so far durable: the cache written out, and on LIFS
    /// the open transaction committed.
    fn flush(&mut self) -> Result<(), FsError>;
    /// Called every couple of seconds (LIFS: the group commit).
    fn tick(&mut self) -> Result<(), FsError> {
        Ok(())
    }

    // --- by node ---
    fn read_node(&mut self, id: NodeId) -> Result<Node, FsError>;
    fn read_range(&mut self, node: &Node, offset: u64, buffer: &mut [u8]) -> Result<usize, FsError>;
    fn write_range(&mut self, node: &mut Node, offset: u64, data: &[u8], now: u64) -> Result<usize, FsError>;
    fn truncate_node(&mut self, node: &mut Node, size: u64, now: u64) -> Result<(), FsError>;
    /// Reads whole blocks of a file straight into frames.
    ///
    /// # Safety
    /// The frames belong to the caller and are mapped nowhere writable
    /// while the controller writes into them.
    unsafe fn read_blocks_into_frames(&mut self, node: &Node, first: u64, frames: &[u64]) -> Result<(), FsError>;

    // --- by path ---
    fn resolve(&mut self, path: &str, who: Credentials) -> Result<Node, FsError>;
    fn require_permission(&mut self, node: &Node, who: Credentials, permission: u64, path: &str) -> Result<(), FsError>;
    fn create_path(&mut self, path: &str, who: Credentials, mode: Mode, now: u64) -> Result<NodeId, FsError>;
    fn write_file(&mut self, path: &str, content: &[u8], who: Credentials, mode: Mode, now: u64) -> Result<NodeId, FsError>;
    fn read_file(&mut self, path: &str, who: Credentials) -> Result<Vec<u8>, FsError>;
    fn delete(&mut self, path: &str, who: Credentials, now: u64) -> Result<(), FsError>;
    fn rename(&mut self, path: &str, new_name: &str, who: Credentials, now: u64) -> Result<(), FsError>;
    fn move_entry(&mut self, from: &str, to: &str, who: Credentials, now: u64) -> Result<NodeId, FsError>;
    fn move_entries(&mut self, moves: &[(String, String)], who: Credentials, now: u64) -> Result<usize, (usize, FsError)>;
    fn link(&mut self, from: &str, to: &str, who: Credentials, now: u64) -> Result<NodeId, FsError>;
    /// Makes entry `path` designate the **file** at `target`, in one step:
    /// created if absent, **replaced** if present -- the previous file
    /// loses that Tag (and goes with its last). A power cut leaves the old
    /// target or the new one, never no entry (phase 15: the `current`
    /// pointer of an application). Rights: `link` on the target, `write`
    /// on the parent, `delete` on a replaced node. Directories are
    /// refused on either side (`IsADirectory`).
    fn retarget(&mut self, path: &str, target: &str, who: Credentials, now: u64) -> Result<NodeId, FsError>;
    fn copy(&mut self, from: &str, to: &str, who: Credentials, now: u64) -> Result<NodeId, FsError>;
    fn stat(&mut self, path: &str, who: Credentials) -> Result<Node, FsError>;
    fn chmod(&mut self, path: &str, mode: Mode, who: Credentials, now: u64) -> Result<(), FsError>;
    fn chown(&mut self, path: &str, owner: Uid, group: Gid, who: Credentials, now: u64) -> Result<(), FsError>;
    fn list(&mut self, path: &str, who: Credentials) -> Result<Vec<Link>, FsError>;
    fn is_executable(&mut self, path: &str, who: Credentials) -> Result<bool, FsError>;

    // --- LIFS, by identifier (spec 6.2) ---
    /// The metadata of node `id`, if the caller may list one of the
    /// directories that tag it (the root: always).
    fn node_stat(&mut self, _id: NodeId, _who: Credentials) -> Result<Node, FsError> {
        Err(FsError::Unsupported)
    }
    /// A path of node `id` (through its first Tag at each level), for the
    /// sandbox of spec 12.4, which reasons in paths.
    fn node_path(&mut self, _id: NodeId) -> Result<String, FsError> {
        Err(FsError::Unsupported)
    }
    /// Moves the entry `name` of directory `from` into directory `to`.
    fn node_move(&mut self, _from: NodeId, _name: &str, _to: NodeId, _who: Credentials, _now: u64) -> Result<(), FsError> {
        Err(FsError::Unsupported)
    }
    /// Clones node `id` into directory `to` under `name`: a new node
    /// sharing its extents (CoW). Returns the new node.
    fn node_clone(&mut self, _id: NodeId, _to: NodeId, _name: &str, _who: Credentials, _now: u64) -> Result<NodeId, FsError> {
        Err(FsError::Unsupported)
    }
    /// Adds a Tag entry for node `id` in directory `to`.
    fn node_tag(&mut self, _id: NodeId, _to: NodeId, _name: &str, _who: Credentials, _now: u64) -> Result<(), FsError> {
        Err(FsError::Unsupported)
    }
    /// Removes entry `name` of directory `dir`; the node goes with its
    /// last Tag.
    fn node_unlink(&mut self, _dir: NodeId, _name: &str, _who: Credentials, _now: u64) -> Result<(), FsError> {
        Err(FsError::Unsupported)
    }
    /// The entries that designate node `id`, those of directories the
    /// caller may list.
    fn node_tags(&mut self, _id: NodeId, _who: Credentials) -> Result<Vec<TagEntry>, FsError> {
        Err(FsError::Unsupported)
    }

    // --- diagnostics ---
    fn dump_state(&mut self);
    fn dump_tree(&mut self, path: &str, who: Credentials, depth: usize, limit: usize);
    /// Checks the structures; returns (nodes, blocks) in use.
    fn check(&mut self) -> Result<(usize, usize), FsError>;
}

// ============================================================
//  NovaFS
// ============================================================

use super::volume::Volume;

impl Filesystem for Volume {
    fn letter(&self) -> char {
        self.letter
    }

    fn set_letter(&mut self, letter: char) {
        self.letter = letter;
    }

    fn info(&self) -> VolumeInfo {
        VolumeInfo {
            label: self.superblock.label.clone(),
            format: "NovaFS",
            total_blocks: self.superblock.total_blocks,
            free_blocks: self.superblock.free_blocks,
            node_count: self.superblock.node_count,
            free_nodes: self.superblock.free_nodes,
        }
    }

    fn name(&self) -> String {
        Volume::name(self)
    }

    fn medium(&self) -> MediumKind {
        Volume::medium(self)
    }

    fn block_count(&mut self) -> u64 {
        self.cache().block_count()
    }

    fn raw_read(&mut self, block: u64, buffer: &mut [u8]) -> Result<(), FsError> {
        self.cache().read(block, buffer).map_err(FsError::Block)
    }

    fn flush(&mut self) -> Result<(), FsError> {
        Volume::flush(self)
    }

    fn read_node(&mut self, id: NodeId) -> Result<Node, FsError> {
        Volume::read_node(self, id)
    }

    fn read_range(&mut self, node: &Node, offset: u64, buffer: &mut [u8]) -> Result<usize, FsError> {
        Volume::read_range(self, node, offset, buffer)
    }

    fn write_range(&mut self, node: &mut Node, offset: u64, data: &[u8], now: u64) -> Result<usize, FsError> {
        Volume::write_range(self, node, offset, data, now)
    }

    fn truncate_node(&mut self, node: &mut Node, size: u64, now: u64) -> Result<(), FsError> {
        Volume::truncate_node(self, node, size, now)
    }

    unsafe fn read_blocks_into_frames(&mut self, node: &Node, first: u64, frames: &[u64]) -> Result<(), FsError> {
        // SAFETY: passed on from this function's contract.
        unsafe { Volume::read_blocks_into_frames(self, node, first, frames) }
    }

    fn resolve(&mut self, path: &str, who: Credentials) -> Result<Node, FsError> {
        Volume::resolve(self, path, who)
    }

    fn require_permission(&mut self, node: &Node, who: Credentials, permission: u64, path: &str) -> Result<(), FsError> {
        Volume::require_permission(self, node, who, permission, path)
    }

    fn create_path(&mut self, path: &str, who: Credentials, mode: Mode, now: u64) -> Result<NodeId, FsError> {
        Volume::create_path(self, path, who, mode, now)
    }

    fn write_file(&mut self, path: &str, content: &[u8], who: Credentials, mode: Mode, now: u64) -> Result<NodeId, FsError> {
        Volume::write_file(self, path, content, who, mode, now)
    }

    fn read_file(&mut self, path: &str, who: Credentials) -> Result<Vec<u8>, FsError> {
        Volume::read_file(self, path, who)
    }

    fn delete(&mut self, path: &str, who: Credentials, now: u64) -> Result<(), FsError> {
        Volume::delete(self, path, who, now)
    }

    fn rename(&mut self, path: &str, new_name: &str, who: Credentials, now: u64) -> Result<(), FsError> {
        Volume::rename(self, path, new_name, who, now)
    }

    fn move_entry(&mut self, from: &str, to: &str, who: Credentials, now: u64) -> Result<NodeId, FsError> {
        Volume::move_entry(self, from, to, who, now)
    }

    fn move_entries(&mut self, moves: &[(String, String)], who: Credentials, now: u64) -> Result<usize, (usize, FsError)> {
        Volume::move_entries(self, moves, who, now)
    }

    fn link(&mut self, from: &str, to: &str, who: Credentials, now: u64) -> Result<NodeId, FsError> {
        Volume::link(self, from, to, who, now)
    }

    fn retarget(&mut self, path: &str, target: &str, who: Credentials, now: u64) -> Result<NodeId, FsError> {
        Volume::retarget(self, path, target, who, now)
    }

    fn copy(&mut self, from: &str, to: &str, who: Credentials, now: u64) -> Result<NodeId, FsError> {
        Volume::copy(self, from, to, who, now)
    }

    fn stat(&mut self, path: &str, who: Credentials) -> Result<Node, FsError> {
        Volume::stat(self, path, who)
    }

    fn chmod(&mut self, path: &str, mode: Mode, who: Credentials, now: u64) -> Result<(), FsError> {
        Volume::chmod(self, path, mode, who, now)
    }

    fn chown(&mut self, path: &str, owner: Uid, group: Gid, who: Credentials, now: u64) -> Result<(), FsError> {
        Volume::chown(self, path, owner, group, who, now)
    }

    fn list(&mut self, path: &str, who: Credentials) -> Result<Vec<Link>, FsError> {
        Volume::list(self, path, who)
    }

    fn is_executable(&mut self, path: &str, who: Credentials) -> Result<bool, FsError> {
        Volume::is_executable(self, path, who)
    }

    fn dump_state(&mut self) {
        Volume::dump_state(self)
    }

    fn dump_tree(&mut self, path: &str, who: Credentials, depth: usize, limit: usize) {
        Volume::dump_tree(self, path, who, depth, limit)
    }

    fn check(&mut self) -> Result<(usize, usize), FsError> {
        Volume::check(self)
    }
}

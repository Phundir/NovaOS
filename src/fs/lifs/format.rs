//! LIFS on disk: the superblock, the node record, the directory entry and
//! the keys of the trees (spec 4.5). `tools/lifs.py` writes and reads the
//! same bytes; the two move together.
//!
//! ## Layout of a partition
//!
//! ```text
//!   block 0                    superblock
//!   blocks 1 ..                allocation bitmap, one bit per block
//!   then                       the journal
//!   the rest                   tree blocks and file data, from the bitmap
//! ```
//!
//! Every integer is little-endian; block numbers are the partition's own.
//! The trees (nodes, refcounts, reverse index, each directory's entries,
//! see `btree`) have 128-bit keys, stored as two u64 (`hi`, then `lo`) and
//! compared in that order.

use alloc::string::String;

use crate::fs::format::{Node, NodeKind};
use crate::fs::perm::Permissions;

pub const BLOCK: usize = 4096;

pub const MAGIC: [u8; 8] = *b"LIFS\0\0\0\x01";
pub const VERSION: u32 = 1;

/// The node of the root directory.
pub const ROOT: u64 = 1;

/// Superblock flag: the system volume (spec 4.5, the root's choice).
pub const FLAG_SYSTEM: u32 = 1;

/// Longest name of an entry, in bytes.
pub const NAME_MAX: usize = 254;

/// Extents a node record holds itself; beyond, an extent chain.
pub const INLINE_EXTENTS: usize = 7;

/// Size of the node record, the value of the node tree.
pub const NODE_SIZE: usize = 256;
/// Size of a directory entry, the value of a directory's tree.
pub const ENTRY_SIZE: usize = 264;
/// Size of a refcount record: length, references.
pub const REFCOUNT_SIZE: usize = 16;
/// Size of a reverse-index record: how many entries of that directory
/// name the node.
pub const REVERSE_SIZE: usize = 8;

pub fn get_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

pub fn get_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

pub fn get_u64(bytes: &[u8], at: usize) -> u64 {
    get_u32(bytes, at) as u64 | (get_u32(bytes, at + 4) as u64) << 32
}

pub fn put_u16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

pub fn put_u32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

pub fn put_u64(bytes: &mut [u8], at: usize, value: u64) {
    bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

pub use crate::fs::partition::crc32;

// ============================================================
//  Superblock
// ============================================================

#[derive(Debug, Clone)]
pub struct Superblock {
    pub total_blocks: u64,
    pub uuid: [u8; 16],
    pub label: String,
    pub flags: u32,
    pub bitmap_start: u64,
    pub bitmap_blocks: u64,
    pub journal_start: u64,
    pub journal_blocks: u64,
    /// Roots of the node tree, the refcount tree, the reverse index (0:
    /// empty).
    pub node_root: u64,
    pub refcount_root: u64,
    pub reverse_root: u64,
    pub next_node: u64,
    pub free_blocks: u64,
    pub node_count: u64,
    /// Last transaction checkpointed.
    pub journal_seq: u64,
    pub created: u64,
    pub modified: u64,
    pub mount_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatError {
    /// Not a LIFS superblock.
    NotLifs,
    UnsupportedVersion(u32),
    BadChecksum,
    /// Fields that contradict each other or the partition.
    Inconsistent,
}

impl Superblock {
    pub fn encode(&self) -> [u8; BLOCK] {
        let mut b = [0u8; BLOCK];
        b[0..8].copy_from_slice(&MAGIC);
        put_u32(&mut b, 8, VERSION);
        put_u32(&mut b, 12, BLOCK as u32);
        put_u64(&mut b, 16, self.total_blocks);
        b[24..40].copy_from_slice(&self.uuid);
        let label = self.label.as_bytes();
        let n = label.len().min(31);
        b[40..40 + n].copy_from_slice(&label[..n]);
        put_u32(&mut b, 72, self.flags);
        put_u64(&mut b, 80, self.bitmap_start);
        put_u64(&mut b, 88, self.bitmap_blocks);
        put_u64(&mut b, 96, self.journal_start);
        put_u64(&mut b, 104, self.journal_blocks);
        put_u64(&mut b, 112, self.node_root);
        put_u64(&mut b, 120, self.refcount_root);
        put_u64(&mut b, 128, self.reverse_root);
        put_u64(&mut b, 136, ROOT);
        put_u64(&mut b, 144, self.next_node);
        put_u64(&mut b, 152, self.free_blocks);
        put_u64(&mut b, 160, self.node_count);
        put_u64(&mut b, 168, self.journal_seq);
        put_u64(&mut b, 176, self.created);
        put_u64(&mut b, 184, self.modified);
        put_u64(&mut b, 192, self.mount_count);
        let crc = crc32(&b[..BLOCK - 4]);
        put_u32(&mut b, BLOCK - 4, crc);
        b
    }

    pub fn decode(b: &[u8], partition_blocks: u64) -> Result<Superblock, FormatError> {
        if b[0..8] != MAGIC[..8] {
            // The version sits in the magic's last bytes too: a LIFS of
            // another version is not "not LIFS".
            if b[0..4] == MAGIC[..4] {
                return Err(FormatError::UnsupportedVersion(get_u32(b, 8)));
            }
            return Err(FormatError::NotLifs);
        }
        if get_u32(b, 8) != VERSION {
            return Err(FormatError::UnsupportedVersion(get_u32(b, 8)));
        }
        if crc32(&b[..BLOCK - 4]) != get_u32(b, BLOCK - 4) {
            return Err(FormatError::BadChecksum);
        }
        let label_raw = &b[40..72];
        let end = label_raw.iter().position(|c| *c == 0).unwrap_or(32);
        let label: String = label_raw[..end].iter().map(|c| if (0x20..0x7F).contains(c) { *c as char } else { '?' }).collect();
        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(&b[24..40]);
        let sb = Superblock {
            total_blocks: get_u64(b, 16),
            uuid,
            label,
            flags: get_u32(b, 72),
            bitmap_start: get_u64(b, 80),
            bitmap_blocks: get_u64(b, 88),
            journal_start: get_u64(b, 96),
            journal_blocks: get_u64(b, 104),
            node_root: get_u64(b, 112),
            refcount_root: get_u64(b, 120),
            reverse_root: get_u64(b, 128),
            next_node: get_u64(b, 144),
            free_blocks: get_u64(b, 152),
            node_count: get_u64(b, 160),
            journal_seq: get_u64(b, 168),
            created: get_u64(b, 176),
            modified: get_u64(b, 184),
            mount_count: get_u64(b, 192),
        };
        let bitmap_needed = sb.total_blocks.div_ceil(BLOCK as u64 * 8);
        if get_u32(b, 12) as usize != BLOCK
            || sb.total_blocks == 0
            || sb.total_blocks > partition_blocks
            || sb.bitmap_start != 1
            || sb.bitmap_blocks < bitmap_needed
            || sb.journal_start != sb.bitmap_start + sb.bitmap_blocks
            || sb.journal_blocks < 16
            || sb.journal_start + sb.journal_blocks >= sb.total_blocks
            || sb.free_blocks > sb.total_blocks
        {
            return Err(FormatError::Inconsistent);
        }
        Ok(sb)
    }

    /// First block that may hold trees or data.
    pub fn data_start(&self) -> u64 {
        self.journal_start + self.journal_blocks
    }
}

// ============================================================
//  Node record
// ============================================================

pub const KIND_FILE: u8 = 1;
pub const KIND_DIR: u8 = 2;

/// A run of a file's blocks on the disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    /// First block of the file it maps.
    pub logical: u64,
    pub phys: u64,
    pub len: u64,
}

/// A LIFS node, as the node tree holds it.
#[derive(Debug, Clone)]
pub struct Record {
    pub id: u64,
    pub kind: u8,
    /// Inline extents in use; `0xFF`: the extents are in the chain at
    /// `tree`.
    pub extent_count: u8,
    pub links: u32,
    pub size: u64,
    pub owner: u32,
    pub group: u32,
    pub perm_owner: u16,
    pub perm_group: u16,
    pub perm_other: u16,
    pub created: u64,
    pub modified: u64,
    pub accessed: u64,
    /// Directory: root of its entry tree. File: first block of its extent
    /// chain, when the extents do not fit inline.
    pub tree: u64,
    /// Directory: its number of entries.
    pub entries: u64,
    pub inline: [Extent; INLINE_EXTENTS],
}

pub const CHAINED: u8 = 0xFF;

impl Record {
    pub fn new(id: u64, kind: u8, owner: u32, group: u32, now: u64) -> Record {
        Record {
            id,
            kind,
            extent_count: 0,
            links: 0,
            size: 0,
            owner,
            group,
            perm_owner: 0,
            perm_group: 0,
            perm_other: 0,
            created: now,
            modified: now,
            accessed: now,
            tree: 0,
            entries: 0,
            inline: [Extent { logical: 0, phys: 0, len: 0 }; INLINE_EXTENTS],
        }
    }

    pub fn is_dir(&self) -> bool {
        self.kind == KIND_DIR
    }

    pub fn encode(&self) -> [u8; NODE_SIZE] {
        let mut b = [0u8; NODE_SIZE];
        put_u64(&mut b, 0, self.id);
        b[8] = self.kind;
        b[9] = self.extent_count;
        put_u32(&mut b, 12, self.links);
        put_u64(&mut b, 16, self.size);
        put_u32(&mut b, 24, self.owner);
        put_u32(&mut b, 28, self.group);
        put_u16(&mut b, 32, self.perm_owner);
        put_u16(&mut b, 34, self.perm_group);
        put_u16(&mut b, 36, self.perm_other);
        put_u64(&mut b, 40, self.created);
        put_u64(&mut b, 48, self.modified);
        put_u64(&mut b, 56, self.accessed);
        put_u64(&mut b, 64, self.tree);
        put_u64(&mut b, 72, self.entries);
        for (i, e) in self.inline.iter().enumerate() {
            let at = 80 + i * 24;
            put_u64(&mut b, at, e.logical);
            put_u64(&mut b, at + 8, e.phys);
            put_u64(&mut b, at + 16, e.len);
        }
        b
    }

    pub fn decode(b: &[u8]) -> Record {
        let mut inline = [Extent { logical: 0, phys: 0, len: 0 }; INLINE_EXTENTS];
        for (i, e) in inline.iter_mut().enumerate() {
            let at = 80 + i * 24;
            *e = Extent { logical: get_u64(b, at), phys: get_u64(b, at + 8), len: get_u64(b, at + 16) };
        }
        Record {
            id: get_u64(b, 0),
            kind: b[8],
            extent_count: b[9],
            links: get_u32(b, 12),
            size: get_u64(b, 16),
            owner: get_u32(b, 24),
            group: get_u32(b, 28),
            perm_owner: get_u16(b, 32),
            perm_group: get_u16(b, 34),
            perm_other: get_u16(b, 36),
            created: get_u64(b, 40),
            modified: get_u64(b, 48),
            accessed: get_u64(b, 56),
            tree: get_u64(b, 64),
            entries: get_u64(b, 72),
            inline,
        }
    }

    /// The node as the rest of the system sees one (`format::Node`): the
    /// same fields NovaFS gives, its block pointers empty.
    pub fn view(&self) -> Node {
        let mut node = Node::empty(self.id);
        node.kind = if self.is_dir() { NodeKind::Directory } else { NodeKind::File };
        node.links = self.links.min(u16::MAX as u32) as u16;
        node.owner = self.owner;
        node.group = self.group;
        node.size = self.size;
        node.perm_owner = Permissions(self.perm_owner as u64);
        node.perm_group = Permissions(self.perm_group as u64);
        node.perm_other = Permissions(self.perm_other as u64);
        node.created = self.created;
        node.modified = self.modified;
        node.accessed = self.accessed;
        node
    }
}

// ============================================================
//  Directory entry
// ============================================================

/// An entry of a directory: a Tag link from a name to a node.
#[derive(Debug, Clone)]
pub struct Entry {
    pub target: u64,
    pub name: String,
}

impl Entry {
    pub fn encode(&self) -> [u8; ENTRY_SIZE] {
        let mut b = [0u8; ENTRY_SIZE];
        put_u64(&mut b, 0, self.target);
        let name = self.name.as_bytes();
        let n = name.len().min(NAME_MAX);
        b[9] = n as u8;
        b[10..10 + n].copy_from_slice(&name[..n]);
        b
    }

    pub fn decode(b: &[u8]) -> Entry {
        let n = (b[9] as usize).min(NAME_MAX);
        Entry { target: get_u64(b, 0), name: String::from_utf8_lossy(&b[10..10 + n]).into_owned() }
    }

    /// The entry's name, compared in place.
    pub fn named(b: &[u8], name: &[u8]) -> bool {
        let n = b[9] as usize;
        n == name.len() && &b[10..10 + n] == name
    }
}

/// The 64-bit hash of a name, the high half of its entry's key (FNV-1a):
/// entries of equal hash differ by the low half and are told apart by
/// their names.
pub fn name_hash(name: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in name {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// A name LIFS accepts: not empty, at most `NAME_MAX` bytes, no `/`, no
/// NUL, not `.` or `..`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= NAME_MAX && name != "." && name != ".." && !name.bytes().any(|b| b == b'/' || b == 0)
}

//! 4.3.2 -- Native Nova OS on-disk format.
//!
//! A volume splits into five consecutive regions, described by the
//! superblock of the first block:
//!
//! ```text
//!   block 0           superblock
//!   ...               node bitmap           1 bit per node
//!   ...               block bitmap          1 bit per block
//!   ...               node table            128 bytes per node
//!   ...               data area
//! ```
//!
//! This split follows decision 4.1: **a node has no name**. It carries a
//! content, a size and rights; the name lives in the directory that
//! references it. Moving a file therefore means rewriting a 64-byte entry,
//! never touching the data.
//!
//! Every structure is read and written **field by field** in little-endian.
//! None is overlaid on a buffer by transmutation: the format must stay
//! independent of the compiler, and a block read from a disk offers no
//! alignment guarantee at all.

use alloc::string::String;

use super::block::BLOCK_SIZE;
use super::perm::{Gid, Mode, Permissions, Uid};

/// Signature of the volume: `NOVAFS\0\0`.
pub const MAGIC: [u8; 8] = *b"NOVAFS\0\0";

/// Format version produced and accepted by this kernel.
///
/// Version 2 widens the entries of a link table from 64 to 128 bytes, so
/// a name may reach 120 characters instead of 56 -- the Linux root of the
/// browser carries names of 63, and a system where a downloaded file
/// cannot keep its name is a system that lies to its user. A volume of
/// version 1 is **refused, not reformatted** (and since phase 14 nothing
/// is formatted implicitly at all): `tools/nova-fs.py migrate`
/// rewrites it into a version 2 without losing a file.
pub const VERSION: u32 = 2;

/// Size of an entry of the node table.
pub const NODE_SIZE: usize = 128;

/// Nodes described by one block of the table.
pub const NODES_PER_BLOCK: usize = BLOCK_SIZE / NODE_SIZE;

/// Size of a link entry in a directory.
pub const LINK_SIZE: usize = 128;

/// Maximum length of a file name, terminator included.
pub const NAME_MAX: usize = 120;

/// Link entries per directory block.
pub const LINKS_PER_BLOCK: usize = BLOCK_SIZE / LINK_SIZE;

/// Direct block pointers in a node.
///
/// Five, and not one more: the node is 128 bytes, its fixed fields occupy
/// 0x00 to 0x4F, and the last eight bytes must be reserved for the
/// indirection pointer. That leaves 0x50 to 0x77, that is five pointers.
/// The assertion at the end of the file locks that computation down.
pub const DIRECT_BLOCKS: usize = 5;

/// Pointers held in the indirection block.
pub const INDIRECT_ENTRIES: usize = BLOCK_SIZE / 8;

/// Blocks a node addresses through a single level of indirection: five
/// direct plus 512, a little over 2 MiB.
pub const SINGLE_LEVEL_BLOCKS: u64 = (DIRECT_BLOCKS + INDIRECT_ENTRIES) as u64;

/// Blocks a node addresses through two levels: five direct plus 512
/// tables of 512 blocks, one gibibyte.
///
/// The second level appears only when a file outgrows the first
/// (`Node::indirect_levels`): a file of a few kilobytes keeps exactly the
/// layout it had before, and so does every volume written until now. A
/// browser's library weighs 150 MiB, where a single level stopped at two.
pub const DOUBLE_LEVEL_BLOCKS: u64 = DIRECT_BLOCKS as u64 + (INDIRECT_ENTRIES * INDIRECT_ENTRIES) as u64;

/// Maximum size of a node: one gibibyte.
pub const MAX_FILE_SIZE: u64 = DOUBLE_LEVEL_BLOCKS * BLOCK_SIZE as u64;

/// Node identifier. Zero is reserved: it means "no node" in a free link
/// entry.
pub type NodeId = u64;

/// Identifier of the root node, always the first allocated.
pub const ROOT_NODE: NodeId = 1;

// ============================================================
//  Reading and writing fields
// ============================================================

pub fn get_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

pub fn get_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

pub fn get_u64(data: &[u8], offset: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&data[offset..offset + 8]);
    u64::from_le_bytes(bytes)
}

pub fn put_u16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

pub fn put_u32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

pub fn put_u64(data: &mut [u8], offset: usize, value: u64) {
    data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// Non-cryptographic checksum of the superblock.
///
/// It detects a corrupt block 0 or a truncated image, nothing more. No
/// ambition of authenticity: that is not the role of a superblock.
pub fn checksum(data: &[u8]) -> u32 {
    let mut hash: u32 = 0x811C_9DC5;
    for &byte in data {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

// ============================================================
//  Superblock
// ============================================================

mod sb {
    pub const MAGIC: usize = 0x00;
    pub const VERSION: usize = 0x08;
    pub const BLOCK_SIZE: usize = 0x0C;
    pub const TOTAL_BLOCKS: usize = 0x10;
    pub const NODE_COUNT: usize = 0x18;
    pub const NODE_BITMAP_START: usize = 0x20;
    pub const NODE_BITMAP_BLOCKS: usize = 0x28;
    pub const BLOCK_BITMAP_START: usize = 0x30;
    pub const BLOCK_BITMAP_BLOCKS: usize = 0x38;
    pub const NODE_TABLE_START: usize = 0x40;
    pub const NODE_TABLE_BLOCKS: usize = 0x48;
    pub const DATA_START: usize = 0x50;
    pub const ROOT_NODE: usize = 0x58;
    pub const FREE_BLOCKS: usize = 0x60;
    pub const FREE_NODES: usize = 0x68;
    pub const CREATED: usize = 0x70;
    pub const FLAGS: usize = 0x78;
    /// Covers bytes 0x00 to 0x7B.
    pub const CHECKSUM: usize = 0x7C;
    pub const LABEL: usize = 0x80;
    pub const LABEL_LEN: usize = 32;
}

#[derive(Debug, Clone)]
pub struct SuperBlock {
    pub version: u32,
    pub block_size: u32,
    pub total_blocks: u64,
    pub node_count: u64,
    pub node_bitmap_start: u64,
    pub node_bitmap_blocks: u64,
    pub block_bitmap_start: u64,
    pub block_bitmap_blocks: u64,
    pub node_table_start: u64,
    pub node_table_blocks: u64,
    pub data_start: u64,
    pub root_node: NodeId,
    pub free_blocks: u64,
    pub free_nodes: u64,
    pub created: u64,
    pub flags: u64,
    pub label: String,
}

/// Reasons a volume is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatError {
    /// Block 0 does not carry the expected signature.
    NotNovaFs,
    /// Version produced by a newer kernel.
    UnsupportedVersion(u32),
    /// Block size different from the one this kernel handles.
    BadBlockSize(u32),
    /// Superblock checksum incorrect.
    CorruptSuperblock,
    /// Regions overlapping or overflowing the medium.
    InconsistentLayout,
    /// Medium too small to hold a volume.
    TooSmall,
}

impl FormatError {
    pub fn message(&self) -> &'static str {
        match self {
            FormatError::NotNovaFs => "pas un volume Nova OS",
            FormatError::UnsupportedVersion(_) => "version de format non supportee",
            FormatError::BadBlockSize(_) => "taille de bloc incompatible",
            FormatError::CorruptSuperblock => "superbloc corrompu",
            FormatError::InconsistentLayout => "disposition incoherente",
            FormatError::TooSmall => "support trop petit",
        }
    }
}

impl SuperBlock {
    /// Computes the layout of a volume from the size of the medium.
    ///
    /// One node per 32 KiB of volume: it is the ratio that keeps the
    /// table under 1 % of the capacity while allowing many small files,
    /// which is what a system tree produces.
    pub fn layout(total_blocks: u64, label: &str) -> Result<Self, FormatError> {
        if total_blocks < 64 {
            return Err(FormatError::TooSmall);
        }

        let node_count = (total_blocks / 8).max(64);
        let node_bitmap_blocks = node_count.div_ceil(8 * BLOCK_SIZE as u64).max(1);
        let node_table_blocks = node_count.div_ceil(NODES_PER_BLOCK as u64).max(1);
        let block_bitmap_blocks = total_blocks.div_ceil(8 * BLOCK_SIZE as u64).max(1);

        let node_bitmap_start = 1;
        let block_bitmap_start = node_bitmap_start + node_bitmap_blocks;
        let node_table_start = block_bitmap_start + block_bitmap_blocks;
        let data_start = node_table_start + node_table_blocks;

        if data_start >= total_blocks {
            return Err(FormatError::TooSmall);
        }

        Ok(SuperBlock {
            version: VERSION,
            block_size: BLOCK_SIZE as u32,
            total_blocks,
            node_count,
            node_bitmap_start,
            node_bitmap_blocks,
            block_bitmap_start,
            block_bitmap_blocks,
            node_table_start,
            node_table_blocks,
            data_start,
            root_node: ROOT_NODE,
            free_blocks: total_blocks - data_start,
            free_nodes: node_count,
            created: 0,
            flags: 0,
            label: String::from(label),
        })
    }

    pub fn encode(&self, block: &mut [u8]) {
        block.fill(0);
        block[sb::MAGIC..sb::MAGIC + 8].copy_from_slice(&MAGIC);
        put_u32(block, sb::VERSION, self.version);
        put_u32(block, sb::BLOCK_SIZE, self.block_size);
        put_u64(block, sb::TOTAL_BLOCKS, self.total_blocks);
        put_u64(block, sb::NODE_COUNT, self.node_count);
        put_u64(block, sb::NODE_BITMAP_START, self.node_bitmap_start);
        put_u64(block, sb::NODE_BITMAP_BLOCKS, self.node_bitmap_blocks);
        put_u64(block, sb::BLOCK_BITMAP_START, self.block_bitmap_start);
        put_u64(block, sb::BLOCK_BITMAP_BLOCKS, self.block_bitmap_blocks);
        put_u64(block, sb::NODE_TABLE_START, self.node_table_start);
        put_u64(block, sb::NODE_TABLE_BLOCKS, self.node_table_blocks);
        put_u64(block, sb::DATA_START, self.data_start);
        put_u64(block, sb::ROOT_NODE, self.root_node);
        put_u64(block, sb::FREE_BLOCKS, self.free_blocks);
        put_u64(block, sb::FREE_NODES, self.free_nodes);
        put_u64(block, sb::CREATED, self.created);
        put_u64(block, sb::FLAGS, self.flags);

        let bytes = self.label.as_bytes();
        let len = bytes.len().min(sb::LABEL_LEN);
        block[sb::LABEL..sb::LABEL + len].copy_from_slice(&bytes[..len]);

        let sum = checksum(&block[..sb::CHECKSUM]);
        put_u32(block, sb::CHECKSUM, sum);
    }

    pub fn decode(block: &[u8]) -> Result<Self, FormatError> {
        if block.len() < BLOCK_SIZE || block[sb::MAGIC..sb::MAGIC + 8] != MAGIC {
            return Err(FormatError::NotNovaFs);
        }

        let stored = get_u32(block, sb::CHECKSUM);
        if stored != checksum(&block[..sb::CHECKSUM]) {
            return Err(FormatError::CorruptSuperblock);
        }

        let version = get_u32(block, sb::VERSION);
        if version != VERSION {
            return Err(FormatError::UnsupportedVersion(version));
        }

        let block_size = get_u32(block, sb::BLOCK_SIZE);
        if block_size as usize != BLOCK_SIZE {
            return Err(FormatError::BadBlockSize(block_size));
        }

        let raw = &block[sb::LABEL..sb::LABEL + sb::LABEL_LEN];
        let len = raw.iter().position(|&b| b == 0).unwrap_or(sb::LABEL_LEN);
        let label = String::from(core::str::from_utf8(&raw[..len]).unwrap_or("?"));

        let superblock = SuperBlock {
            version,
            block_size,
            total_blocks: get_u64(block, sb::TOTAL_BLOCKS),
            node_count: get_u64(block, sb::NODE_COUNT),
            node_bitmap_start: get_u64(block, sb::NODE_BITMAP_START),
            node_bitmap_blocks: get_u64(block, sb::NODE_BITMAP_BLOCKS),
            block_bitmap_start: get_u64(block, sb::BLOCK_BITMAP_START),
            block_bitmap_blocks: get_u64(block, sb::BLOCK_BITMAP_BLOCKS),
            node_table_start: get_u64(block, sb::NODE_TABLE_START),
            node_table_blocks: get_u64(block, sb::NODE_TABLE_BLOCKS),
            data_start: get_u64(block, sb::DATA_START),
            root_node: get_u64(block, sb::ROOT_NODE),
            free_blocks: get_u64(block, sb::FREE_BLOCKS),
            free_nodes: get_u64(block, sb::FREE_NODES),
            created: get_u64(block, sb::CREATED),
            flags: get_u64(block, sb::FLAGS),
            label,
        };

        // The regions must follow one another without overlapping and
        // stay inside the medium: a superblock valid in appearance but
        // inconsistent would make us write anywhere.
        let expected_bitmap = superblock.node_bitmap_start + superblock.node_bitmap_blocks;
        let expected_table = superblock.block_bitmap_start + superblock.block_bitmap_blocks;
        let expected_data = superblock.node_table_start + superblock.node_table_blocks;

        if superblock.node_bitmap_start != 1
            || superblock.block_bitmap_start != expected_bitmap
            || superblock.node_table_start != expected_table
            || superblock.data_start != expected_data
            || superblock.data_start >= superblock.total_blocks
            || superblock.free_blocks > superblock.total_blocks
            || superblock.free_nodes > superblock.node_count
        {
            return Err(FormatError::InconsistentLayout);
        }

        Ok(superblock)
    }

    /// Block of the table holding a node, and offset in that block.
    pub fn node_location(&self, id: NodeId) -> Option<(u64, usize)> {
        if id == 0 || id > self.node_count {
            return None;
        }
        let index = (id - 1) as usize;
        Some((
            self.node_table_start + (index / NODES_PER_BLOCK) as u64,
            (index % NODES_PER_BLOCK) * NODE_SIZE,
        ))
    }
}

// ============================================================
//  4.1.1 -- Node
// ============================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NodeKind {
    Free = 0,
    File = 1,
    Directory = 2,
}

impl NodeKind {
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(NodeKind::Free),
            1 => Some(NodeKind::File),
            2 => Some(NodeKind::Directory),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            NodeKind::Free => "libre",
            NodeKind::File => "fichier",
            NodeKind::Directory => "dossier",
        }
    }

    /// Permissions that make sense on this node type (spec 5.1).
    pub fn applicable_permissions(&self) -> u64 {
        match self {
            NodeKind::Directory => Permissions::DIR_APPLICABLE,
            _ => Permissions::FILE_APPLICABLE,
        }
    }
}

/// Offsets of the fields of a node, in its 128 bytes.
///
/// ```text
///   0x00   8   identifier
///   0x08   1   nature      0x0A   2   number of links
///   0x0C   4   owner
///   0x10   4   group       0x14   4   reserved
///   0x18   8   size in bytes
///   0x20   8   owner permissions
///   0x28   8   group permissions
///   0x30   8   other permissions
///   0x38   8   creation    0x40  8  modification    0x48  8  access
///   0x09   1   levels of indirection (0 or 1: one, 2: two)
///   0x50  40   five direct block pointers
///   0x78   8   indirection block pointer
///   0x80       end
/// ```
mod nd {
    pub const ID: usize = 0x00;
    pub const KIND: usize = 0x08;
    /// Levels of indirection of `INDIRECT`: 0 or 1 for one, 2 for two.
    /// Read as 0 in a volume written before the second level existed.
    pub const INDIRECT_LEVELS: usize = 0x09;
    pub const LINKS: usize = 0x0A;
    pub const OWNER: usize = 0x0C;
    pub const GROUP: usize = 0x10;
    pub const SIZE: usize = 0x18;
    pub const PERM_OWNER: usize = 0x20;
    pub const PERM_GROUP: usize = 0x28;
    pub const PERM_OTHER: usize = 0x30;
    pub const CREATED: usize = 0x38;
    pub const MODIFIED: usize = 0x40;
    pub const ACCESSED: usize = 0x48;
    pub const DIRECT: usize = 0x50;
    pub const INDIRECT: usize = 0x78;
}

/// A node of the filesystem.
///
/// It carries **no name**: that is the starting point of the whole of
/// 4.1. Several directories may reference it under different names;
/// `links` counts those references and the node only disappears at the
/// last removal.
#[derive(Debug, Clone)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    pub links: u16,
    pub owner: Uid,
    pub group: Gid,
    pub size: u64,
    pub perm_owner: Permissions,
    pub perm_group: Permissions,
    pub perm_other: Permissions,
    pub created: u64,
    pub modified: u64,
    pub accessed: u64,
    pub direct: [u64; DIRECT_BLOCKS],
    pub indirect: u64,
    /// Levels of indirection of `indirect`: one (the historical layout)
    /// or two, for a file beyond `SINGLE_LEVEL_BLOCKS`.
    pub indirect_levels: u8,
}

impl Node {
    pub fn empty(id: NodeId) -> Self {
        Node {
            id,
            kind: NodeKind::Free,
            links: 0,
            owner: 0,
            group: 0,
            size: 0,
            perm_owner: Permissions::NONE,
            perm_group: Permissions::NONE,
            perm_other: Permissions::NONE,
            created: 0,
            modified: 0,
            accessed: 0,
            direct: [0; DIRECT_BLOCKS],
            indirect: 0,
            indirect_levels: 1,
        }
    }

    pub fn new(id: NodeId, kind: NodeKind, owner: Uid, group: Gid, mode: Mode, now: u64) -> Self {
        // The permissions with no effect on this node type are removed at
        // creation rather than ignored at read: a stored mask must
        // describe what really applies.
        let applicable = kind.applicable_permissions();
        Node {
            id,
            kind,
            links: 0,
            owner,
            group,
            size: 0,
            perm_owner: Permissions(mode.owner.0 & applicable),
            perm_group: Permissions(mode.group.0 & applicable),
            perm_other: Permissions(mode.other.0 & applicable),
            created: now,
            modified: now,
            accessed: now,
            direct: [0; DIRECT_BLOCKS],
            indirect: 0,
            indirect_levels: 1,
        }
    }

    pub fn encode(&self, slot: &mut [u8]) {
        slot[..NODE_SIZE].fill(0);
        put_u64(slot, nd::ID, self.id);
        slot[nd::KIND] = self.kind as u8;
        put_u16(slot, nd::LINKS, self.links);
        put_u32(slot, nd::OWNER, self.owner);
        put_u32(slot, nd::GROUP, self.group);
        put_u64(slot, nd::SIZE, self.size);
        put_u64(slot, nd::PERM_OWNER, self.perm_owner.0);
        put_u64(slot, nd::PERM_GROUP, self.perm_group.0);
        put_u64(slot, nd::PERM_OTHER, self.perm_other.0);
        put_u64(slot, nd::CREATED, self.created);
        put_u64(slot, nd::MODIFIED, self.modified);
        put_u64(slot, nd::ACCESSED, self.accessed);
        for (index, block) in self.direct.iter().enumerate() {
            put_u64(slot, nd::DIRECT + index * 8, *block);
        }
        put_u64(slot, nd::INDIRECT, self.indirect);
        slot[nd::INDIRECT_LEVELS] = self.indirect_levels;
    }

    pub fn decode(slot: &[u8]) -> Self {
        let mut direct = [0u64; DIRECT_BLOCKS];
        for (index, block) in direct.iter_mut().enumerate() {
            *block = get_u64(slot, nd::DIRECT + index * 8);
        }

        Node {
            id: get_u64(slot, nd::ID),
            kind: NodeKind::from_code(slot[nd::KIND]).unwrap_or(NodeKind::Free),
            links: get_u16(slot, nd::LINKS),
            owner: get_u32(slot, nd::OWNER),
            group: get_u32(slot, nd::GROUP),
            size: get_u64(slot, nd::SIZE),
            perm_owner: Permissions(get_u64(slot, nd::PERM_OWNER)),
            perm_group: Permissions(get_u64(slot, nd::PERM_GROUP)),
            perm_other: Permissions(get_u64(slot, nd::PERM_OTHER)),
            created: get_u64(slot, nd::CREATED),
            modified: get_u64(slot, nd::MODIFIED),
            accessed: get_u64(slot, nd::ACCESSED),
            direct,
            indirect: get_u64(slot, nd::INDIRECT),
            indirect_levels: slot[nd::INDIRECT_LEVELS].max(1),
        }
    }

    pub fn is_directory(&self) -> bool {
        self.kind == NodeKind::Directory
    }

    /// Number of blocks occupied by the content.
    pub fn block_count(&self) -> u64 {
        self.size.div_ceil(BLOCK_SIZE as u64)
    }
}

// ============================================================
//  4.1.2 -- Link entry
// ============================================================

/// An entry of the link table of a directory.
///
/// It is the only place where a file name exists. A null `node` marks a
/// free slot, reusable without compacting the table.
#[derive(Debug, Clone)]
pub struct Link {
    pub name: String,
    pub node: NodeId,
}

impl Link {
    pub fn encode(&self, slot: &mut [u8]) {
        slot[..LINK_SIZE].fill(0);
        let bytes = self.name.as_bytes();
        let len = bytes.len().min(NAME_MAX);
        slot[..len].copy_from_slice(&bytes[..len]);
        put_u64(slot, NAME_MAX, self.node);
    }

    pub fn decode(slot: &[u8]) -> Option<Self> {
        let node = get_u64(slot, NAME_MAX);
        if node == 0 {
            return None;
        }
        let raw = &slot[..NAME_MAX];
        let len = raw.iter().position(|&b| b == 0).unwrap_or(NAME_MAX);
        let name = core::str::from_utf8(&raw[..len]).ok()?;
        Some(Link {
            name: String::from(name),
            node,
        })
    }
}

// The format describes itself through constants; they still have to be
// consistent with each other. These assertions fail at compile time, not
// at the first formatting of a disk.
const _: () = assert!(nd::DIRECT + DIRECT_BLOCKS * 8 == nd::INDIRECT);
const _: () = assert!(nd::INDIRECT + 8 == NODE_SIZE);
const _: () = assert!(NAME_MAX + 8 == LINK_SIZE);
const _: () = assert!(BLOCK_SIZE % NODE_SIZE == 0);
const _: () = assert!(BLOCK_SIZE % LINK_SIZE == 0);

/// Is a file name acceptable?
///
/// Refused: the empty one, names that are too long, `.` and `..` -- the
/// format has no traversal entries, path resolution takes care of that --
/// and any character outside printable ASCII, `/` included. That last
/// point is not cosmetic: a `/` in a name would make two distinct paths
/// indistinguishable.
pub fn valid_name(name: &str) -> bool {
    if name.is_empty() || name.len() >= NAME_MAX {
        return false;
    }
    if name == "." || name == ".." {
        return false;
    }
    name.bytes()
        .all(|b| (0x20..=0x7E).contains(&b) && b != b'/')
}

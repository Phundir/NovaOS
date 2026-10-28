//! 4.6.2 -- ext4 export.
//!
//! ext4 is the "Linux" destination format of 4.4. Unlike FAT32, it can
//! express a good part of what Nova OS stores: owner, group, rights per
//! class, and above all **multiple links**, which are the central notion
//! of 4.1. A node referenced from two directories becomes one inode with
//! two links again here, and not two copies as in FAT32.
//!
//! ## Layout produced
//!
//! ```text
//!   block 0               super block at byte 1024, boot area before
//!   block 1..             group descriptors
//!   per group:            block bitmap, inode bitmap, inode table
//!   rest of the group     data
//! ```
//!
//! The filesystem block is exactly one medium block -- 4 KiB -- which
//! removes all sector translation: every write bears on a whole block,
//! written once. That is the main reason for this choice; the other is
//! that 4 KiB is the default size of `mke2fs` above 512 MiB, hence the
//! one the host tools handle best.
//!
//! ## Why extents and not indirect blocks
//!
//! A Nova OS file caps today at 2068 KiB, that is 517 blocks. Writing it
//! in one piece, a single extent describes it entirely, where the old
//! indirect scheme would have demanded an extra pointer block. The inode
//! carries up to four inline extents, which leaves room for a file
//! fragmented by the group metadata.
//!
//! ## What the format does not carry
//!
//! The thirteen permissions of 5 reduce to the nine POSIX bits: `read`,
//! `write` and `execute` survive, the ten others -- `overwrite`,
//! `rename`, `audit`... -- disappear. A file exported then reimported
//! would therefore have impoverished rights: the export is a transfer
//! outwards, not a backup.
//!
//! ## What is not done
//!
//! No journal. An ext4 volume with no journal is perfectly valid -- it is
//! what `mke2fs -O ^has_journal` produces -- and mounts as-is, but it
//! replays nothing after a cut. On an exchange medium unmounted cleanly,
//! that is of no consequence; building it would mean writing a complete
//! ext4 `$LogFile` for no benefit here. No checksums either
//! (`metadata_csum`): they would force recomputing a CRC32c on every
//! structure, when their absence is a normal state of the format.

use alloc::string::String;
use alloc::vec::Vec;

use super::super::FsError;
use super::super::block::{BLOCK_SIZE, BlockDevice};
use super::super::format::{NodeId, NodeKind};
use super::super::perm::{Credentials, Permissions};
use super::{ExportFormat, ExportReport, join_path};

// ============================================================
//  Constants of the format
// ============================================================

/// Size of an ext4 block: identical to the medium block, by choice.
const FS_BLOCK: usize = BLOCK_SIZE;
/// `s_log_block_size`: the size is `1024 << value`.
const LOG_BLOCK_SIZE: u32 = 2;
/// Size of an on-disk inode. 256 is the ext4 default; the 128 bytes
/// beyond the historic part carry `i_extra_isize`.
const INODE_SIZE: usize = 256;
/// Size of a group descriptor without the `64bit` feature.
const DESC_SIZE: usize = 32;
/// Blocks per group: a block bitmap fits in one block, that is
/// 4096 x 8 bits.
const BLOCKS_PER_GROUP: u32 = (FS_BLOCK * 8) as u32;

const MAGIC: u16 = 0xEF53;
/// Inode of the root directory.
const ROOT_INO: u32 = 2;
/// First freely assignable inode. The first ten are reserved by the
/// format, `lost+found` occupies the eleventh by convention.
const FIRST_INO: u32 = 11;
const LOST_FOUND_INO: u32 = FIRST_INO;
/// First inode assigned to the exported tree.
const FIRST_FREE_INO: u32 = FIRST_INO + 1;

// --- features declared ---
const INCOMPAT_FILETYPE: u32 = 0x0002;
const INCOMPAT_EXTENTS: u32 = 0x0040;
const RO_COMPAT_SPARSE_SUPER: u32 = 0x0001;
const RO_COMPAT_LARGE_FILE: u32 = 0x0002;

/// `EXT4_EXTENTS_FL`: this inode describes its blocks by extents.
const INODE_FLAG_EXTENTS: u32 = 0x0008_0000;
/// Signature of an extent header.
const EXTENT_MAGIC: u16 = 0xF30A;
/// Number of extents fitting inline in `i_block`: 60 bytes, minus the
/// 12-byte header, divided by 12.
const INLINE_EXTENTS: usize = 4;

// --- directory entry types (`filetype` feature) ---
const DIRENT_FILE: u8 = 1;
const DIRENT_DIR: u8 = 2;

/// 1 January 2026, midnight UTC, in seconds since the Unix epoch.
///
/// Same reason as in FAT32: the node timestamps count ticks since boot,
/// not seconds since 1970. A manifestly conventional date is worth more
/// than a wrong one.
const FIXED_TIME: u32 = 1_767_225_600;

// ============================================================
//  Geometry
// ============================================================

/// Layout of a block group.
struct GroupLayout {
    first_block: u32,
    blocks: u32,
    has_super: bool,
    block_bitmap: u32,
    inode_bitmap: u32,
    inode_table: u32,
}

struct Geometry {
    blocks: u32,
    groups: u32,
    gdt_blocks: u32,
    inodes_per_group: u32,
    itable_blocks: u32,
}

/// The backup copies of the super block are only placed in groups 0, 1
/// and the powers of 3, 5 and 7 -- that is the definition of
/// `sparse_super`, which avoids tying up a table descriptor in every
/// group of a volume that counts thousands.
fn has_super_backup(group: u32) -> bool {
    fn power_of(mut value: u32, base: u32) -> bool {
        if value == 0 {
            return false;
        }
        while value % base == 0 {
            value /= base;
        }
        value == 1
    }
    group == 0 || group == 1 || power_of(group, 3) || power_of(group, 5) || power_of(group, 7)
}

impl Geometry {
    fn compute(total_blocks: u64) -> Result<Self, FsError> {
        let mut blocks = total_blocks.min(u32::MAX as u64) as u32;

        loop {
            if blocks < 64 {
                return Err(FsError::NoSpace);
            }
            let groups = blocks.div_ceil(BLOCKS_PER_GROUP);

            // One inode per four blocks: a generous ratio, but a Nova OS
            // tree counts many small configuration files. Bounded by what
            // an inode bitmap can address on one side, by the cost of the
            // table on the other.
            let mut inodes_per_group = (blocks / groups / 4) & !7;
            inodes_per_group = inodes_per_group.clamp(32, 2048);

            let itable_blocks =
                (inodes_per_group as usize * INODE_SIZE).div_ceil(FS_BLOCK) as u32;
            let gdt_blocks = (groups as usize * DESC_SIZE).div_ceil(FS_BLOCK) as u32;

            // The last group must host its own metadata and leave enough
            // to write at least one data block. Otherwise it is
            // abandoned: the volume is simply shorter, which is what
            // `mke2fs` does too.
            let last = groups - 1;
            let last_first = last * BLOCKS_PER_GROUP;
            let last_size = blocks - last_first;
            let overhead = if has_super_backup(last) {
                1 + gdt_blocks
            } else {
                0
            } + 2
                + itable_blocks;

            if last_size <= overhead {
                if groups == 1 {
                    return Err(FsError::NoSpace);
                }
                blocks = last_first;
                continue;
            }

            return Ok(Geometry {
                blocks,
                groups,
                gdt_blocks,
                inodes_per_group,
                itable_blocks,
            });
        }
    }

    fn inodes(&self) -> u32 {
        self.groups * self.inodes_per_group
    }

    fn group(&self, index: u32) -> GroupLayout {
        let first_block = index * BLOCKS_PER_GROUP;
        let blocks = (self.blocks - first_block).min(BLOCKS_PER_GROUP);
        let has_super = has_super_backup(index);
        let start = first_block + if has_super { 1 + self.gdt_blocks } else { 0 };
        GroupLayout {
            first_block,
            blocks,
            has_super,
            block_bitmap: start,
            inode_bitmap: start + 1,
            inode_table: start + 2,
        }
    }
}

// ============================================================
//  Block allocation
// ============================================================

/// Occupancy map of the whole volume, one bit per block.
///
/// It serves twice: to choose where to write during the walk, and to
/// produce the group bitmaps at the end. Having derived both from the
/// same source guarantees they describe exactly what was written -- a
/// discrepancy between the two is precisely what `e2fsck` reports.
struct Allocator {
    used: Vec<u64>,
    blocks: u32,
    cursor: u32,
}

impl Allocator {
    fn new(blocks: u32) -> Self {
        Allocator {
            used: alloc::vec![0u64; (blocks as usize).div_ceil(64)],
            blocks,
            cursor: 0,
        }
    }

    fn mark(&mut self, block: u32) {
        self.used[block as usize / 64] |= 1 << (block % 64);
    }

    fn mark_range(&mut self, first: u32, count: u32) {
        for block in first..first + count {
            self.mark(block);
        }
    }

    fn is_used(&self, block: u32) -> bool {
        self.used[block as usize / 64] & (1 << (block % 64)) != 0
    }

    /// Reserves `count` blocks, in as few contiguous ranges as possible.
    /// Beyond four ranges the inode could no longer describe them inline:
    /// better to refuse than produce a truncated extent, which would
    /// silently lose the end of the file.
    fn allocate(&mut self, count: u32) -> Result<Vec<(u32, u32)>, FsError> {
        let mut runs: Vec<(u32, u32)> = Vec::new();
        let mut remaining = count;

        while remaining > 0 {
            while self.cursor < self.blocks && self.is_used(self.cursor) {
                self.cursor += 1;
            }
            if self.cursor >= self.blocks {
                return Err(FsError::NoSpace);
            }

            let start = self.cursor;
            let mut length = 0;
            while length < remaining && self.cursor < self.blocks && !self.is_used(self.cursor) {
                self.mark(self.cursor);
                self.cursor += 1;
                length += 1;
            }

            runs.push((start, length));
            remaining -= length;

            if runs.len() > INLINE_EXTENTS {
                return Err(FsError::NoSpace);
            }
        }
        Ok(runs)
    }

    /// Free blocks of a group.
    fn free_in(&self, layout: &GroupLayout) -> u32 {
        let mut free = 0;
        for block in layout.first_block..layout.first_block + layout.blocks {
            if !self.is_used(block) {
                free += 1;
            }
        }
        free
    }
}

// ============================================================
//  Inodes
// ============================================================

/// What is kept of an inode until the table is written.
///
/// The records are produced in increasing order of number, which allows
/// sweeping the table only once at the end.
struct InodeRecord {
    ino: u32,
    mode: u16,
    uid: u32,
    gid: u32,
    links: u16,
    size: u64,
    runs: Vec<(u32, u32)>,
}

impl InodeRecord {
    /// Serialises the inode over its 256 bytes.
    fn encode(&self) -> [u8; INODE_SIZE] {
        let mut raw = [0u8; INODE_SIZE];

        put_u16(&mut raw, 0x00, self.mode);
        put_u16(&mut raw, 0x02, self.uid as u16);
        put_u32(&mut raw, 0x04, self.size as u32);
        put_u32(&mut raw, 0x08, FIXED_TIME);
        put_u32(&mut raw, 0x0C, FIXED_TIME);
        put_u32(&mut raw, 0x10, FIXED_TIME);
        put_u32(&mut raw, 0x14, 0); // deletion date
        put_u16(&mut raw, 0x18, self.gid as u16);
        put_u16(&mut raw, 0x1A, self.links);

        // `i_blocks` counts 512-byte sectors, not filesystem blocks: it
        // is an ext2 legacy `e2fsck` still checks.
        let blocks: u32 = self.runs.iter().map(|(_, length)| *length).sum();
        put_u32(&mut raw, 0x1C, blocks * (FS_BLOCK / 512) as u32);
        put_u32(&mut raw, 0x20, INODE_FLAG_EXTENTS);

        // --- inline extent tree, depth zero ---
        let base = 0x28;
        put_u16(&mut raw, base, EXTENT_MAGIC);
        put_u16(&mut raw, base + 2, self.runs.len() as u16);
        put_u16(&mut raw, base + 4, INLINE_EXTENTS as u16);
        put_u16(&mut raw, base + 6, 0); // depth: leaf
        put_u32(&mut raw, base + 8, 0); // generation

        let mut logical = 0u32;
        for (index, (start, length)) in self.runs.iter().enumerate() {
            let entry = base + 12 + index * 12;
            put_u32(&mut raw, entry, logical);
            put_u16(&mut raw, entry + 4, *length as u16);
            put_u16(&mut raw, entry + 6, 0); // high 16 bits of the LBA
            put_u32(&mut raw, entry + 8, *start);
            logical += length;
        }

        put_u32(&mut raw, 0x6C, (self.size >> 32) as u32);
        // The high sixteen bits of the identifier live in `i_osd2`: the
        // Nova OS accounts are on 32 bits, the historic ext2 fields on 16.
        put_u16(&mut raw, 0x78, (self.uid >> 16) as u16);
        put_u16(&mut raw, 0x7A, (self.gid >> 16) as u16);
        // `i_extra_isize`: length used beyond the historic 128 bytes.
        // Without it, a 256-byte inode is considered corrupt.
        put_u16(&mut raw, 0x80, 32);
        raw
    }
}

fn put_u16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Translates the Nova OS permissions into POSIX bits.
///
/// Three of the thirteen permissions of 5 have an equivalent; the others
/// have none and are lost. `access` on a directory joins the read bit,
/// since that is what it allows: listing its content.
fn posix_mode(kind: NodeKind, owner: Permissions, group: Permissions, other: Permissions) -> u16 {
    let class = |permissions: Permissions| -> u16 {
        let mut bits = 0;
        match kind {
            // On a directory, `access` carries the two POSIX bits that
            // make it usable: `r` to list its content, `x` to traverse
            // it. The `execute` permission of 5, which conditions the
            // execution of the files it holds, has no equivalent and
            // disappears.
            NodeKind::Directory => {
                if permissions.contains(Permissions::ACCESS) {
                    bits |= 0o5;
                }
            }
            _ => {
                if permissions.contains(Permissions::READ) {
                    bits |= 0o4;
                }
                if permissions.contains(Permissions::EXECUTE) {
                    bits |= 0o1;
                }
            }
        }
        if permissions.contains(Permissions::WRITE) {
            bits |= 0o2;
        }
        bits
    };

    let format = match kind {
        NodeKind::Directory => 0o040_000,
        _ => 0o100_000,
    };
    format | (class(owner) << 6) | (class(group) << 3) | class(other)
}

// ============================================================
//  Directory content
//  Directory content

/// An entry to write in a directory.
struct Dirent {
    ino: u32,
    name: String,
    file_type: u8,
}

/// Lays a list of entries out as directory blocks.
///
/// Two rules govern the layout: no entry straddles a block boundary, and
/// the last entry of each block extends its `rec_len` to the end. That is
/// what lets a walk jump from entry to entry without ever reading beyond
/// the current block.
fn build_directory(entries: &[Dirent]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut block = alloc::vec![0u8; FS_BLOCK];
    let mut offset = 0usize;
    let mut last = 0usize;

    for entry in entries {
        let name = entry.name.as_bytes();
        let needed = (8 + name.len()).div_ceil(4) * 4;

        if offset + needed > FS_BLOCK {
            put_u16(&mut block, last + 4, (FS_BLOCK - last) as u16);
            out.extend_from_slice(&block);
            block.fill(0);
            offset = 0;
        }

        put_u32(&mut block, offset, entry.ino);
        put_u16(&mut block, offset + 4, needed as u16);
        block[offset + 6] = name.len() as u8;
        block[offset + 7] = entry.file_type;
        block[offset + 8..offset + 8 + name.len()].copy_from_slice(name);

        last = offset;
        offset += needed;
    }

    put_u16(&mut block, last + 4, (FS_BLOCK - last) as u16);
    out.extend_from_slice(&block);
    out
}

// ============================================================
//  Writing
// ============================================================

/// Writes arbitrary content into a series of block ranges.
fn write_runs(
    device: &mut dyn BlockDevice,
    runs: &[(u32, u32)],
    data: &[u8],
) -> Result<(), FsError> {
    let mut written = 0usize;
    let mut buffer = alloc::vec![0u8; FS_BLOCK];

    for (start, length) in runs {
        for index in 0..*length {
            let end = (written + FS_BLOCK).min(data.len());
            buffer.fill(0);
            if written < data.len() {
                let slice = &data[written..end];
                buffer[..slice.len()].copy_from_slice(slice);
                written = end;
            }
            device.write_block((start + index) as u64, &buffer)?;
        }
    }
    Ok(())
}

// ============================================================
//  Export
// ============================================================

/// A directory left to walk.
struct Pending {
    path: String,
    ino: u32,
    parent: u32,
}

/// Rewrites a Nova OS tree as ext4 on a blank medium.
pub fn export(
    source: &str,
    target: &mut dyn BlockDevice,
    who: Credentials,
) -> Result<ExportReport, FsError> {
    if target.read_only() {
        return Err(FsError::Block(super::super::block::BlockError::ReadOnly));
    }

    let mut report = ExportReport::new(ExportFormat::Ext4);
    let geometry = Geometry::compute(target.block_count())?;
    let mut allocator = Allocator::new(geometry.blocks);

    // --- reservation of the metadata ---
    for index in 0..geometry.groups {
        let layout = geometry.group(index);
        if layout.has_super {
            allocator.mark_range(layout.first_block, 1 + geometry.gdt_blocks);
        }
        allocator.mark(layout.block_bitmap);
        allocator.mark(layout.inode_bitmap);
        allocator.mark_range(layout.inode_table, geometry.itable_blocks);
    }

    let mut inodes: Vec<InodeRecord> = Vec::new();
    let mut next_ino = FIRST_FREE_INO;
    if next_ino > geometry.inodes_per_group {
        // The reserved inodes must fit in the first group.
        return Err(FsError::NoSpace);
    }

    // `lost+found` has nothing to carry, but its absence is an anomaly
    // for `e2fsck`, which then offers to create it. Laying it down at
    // once avoids a routine check reporting a defect.
    let lost_found = build_directory(&[
        Dirent {
            ino: LOST_FOUND_INO,
            name: String::from("."),
            file_type: DIRENT_DIR,
        },
        Dirent {
            ino: ROOT_INO,
            name: String::from(".."),
            file_type: DIRENT_DIR,
        },
    ]);
    let lost_runs = allocator.allocate(1)?;
    write_runs(target, &lost_runs, &lost_found)?;

    // --- walk of the tree ---
    let mut queue: Vec<Pending> = alloc::vec![Pending {
        path: String::from(source),
        ino: ROOT_INO,
        parent: ROOT_INO,
    }];

    // Node already exported -> inode assigned. Two uses: rendering a
    // multiple link of 4.1 by an ext4 hard link rather than by a copy,
    // and cutting the cycles `link` on a directory makes possible.
    let mut seen: Vec<(NodeId, u32)> = alloc::vec![(
        super::super::stat(source, who)?.id,
        ROOT_INO
    )];

    while let Some(pending) = queue.pop() {
        let links = match super::super::list(&pending.path, who) {
            Ok(links) => links,
            Err(error) => {
                report
                    .skipped
                    .push(alloc::format!("{} ({})", pending.path, error.message()));
                continue;
            }
        };

        let node = super::super::stat(&pending.path, who)?;
        let mut entries: Vec<Dirent> = alloc::vec![
            Dirent {
                ino: pending.ino,
                name: String::from("."),
                file_type: DIRENT_DIR,
            },
            Dirent {
                ino: pending.parent,
                name: String::from(".."),
                file_type: DIRENT_DIR,
            },
        ];
        if pending.ino == ROOT_INO {
            entries.push(Dirent {
                ino: LOST_FOUND_INO,
                name: String::from("lost+found"),
                file_type: DIRENT_DIR,
            });
        }

        // Two links for the directory itself -- its name at the parent
        // and its own "." -- plus one per sub-directory, which points at
        // it through its "..".
        let mut link_count: u16 = 2;
        if pending.ino == ROOT_INO {
            link_count += 1; // lost+found
        }

        let mut sorted = links;
        sorted.sort_by(|a, b| a.name.cmp(&b.name));

        for link in sorted.iter() {
            let child_path = join_path(&pending.path, &link.name);
            let child = match super::super::stat(&child_path, who) {
                Ok(child) => child,
                Err(error) => {
                    report
                        .skipped
                        .push(alloc::format!("{} ({})", child_path, error.message()));
                    continue;
                }
            };

            match child.kind {
                NodeKind::Directory => {
                    // A directory already exported is a link of 4.1 to a
                    // directory: ext4 has no hard link of directory, and
                    // descending into it again would loop if the link
                    // goes back to an ancestor. It is set aside and said.
                    if seen.iter().any(|(id, _)| *id == child.id) {
                        report.skipped.push(alloc::format!(
                            "{} (dossier deja exporte - ext4 n'a pas de lien dur de dossier)",
                            child_path
                        ));
                        continue;
                    }
                    if next_ino > geometry.inodes() {
                        return Err(FsError::NoSpace);
                    }
                    let ino = next_ino;
                    next_ino += 1;
                    link_count += 1;
                    entries.push(Dirent {
                        ino,
                        name: link.name.clone(),
                        file_type: DIRENT_DIR,
                    });
                    seen.push((child.id, ino));
                    queue.push(Pending {
                        path: child_path,
                        ino,
                        parent: pending.ino,
                    });
                    report.directories += 1;
                }
                NodeKind::File => {
                    // Second link to a node already written: the entry
                    // points at the same inode, whose link counter is
                    // incremented. Nothing is copied.
                    if let Some((_, ino)) = seen.iter().find(|(id, _)| *id == child.id) {
                        let ino = *ino;
                        if let Some(record) = inodes.iter_mut().find(|r| r.ino == ino) {
                            record.links += 1;
                        }
                        entries.push(Dirent {
                            ino,
                            name: link.name.clone(),
                            file_type: DIRENT_FILE,
                        });
                        report.files += 1;
                        continue;
                    }

                    let data = match super::super::read_file(&child_path, who) {
                        Ok(data) => data,
                        Err(error) => {
                            report
                                .skipped
                                .push(alloc::format!("{} ({})", child_path, error.message()));
                            continue;
                        }
                    };

                    if next_ino > geometry.inodes() {
                        return Err(FsError::NoSpace);
                    }
                    let ino = next_ino;
                    next_ino += 1;

                    let runs = allocator.allocate(data.len().div_ceil(FS_BLOCK) as u32)?;
                    write_runs(target, &runs, &data)?;

                    inodes.push(InodeRecord {
                        ino,
                        mode: posix_mode(
                            NodeKind::File,
                            child.perm_owner,
                            child.perm_group,
                            child.perm_other,
                        ),
                        uid: child.owner,
                        gid: child.group,
                        links: 1,
                        size: data.len() as u64,
                        runs,
                    });
                    seen.push((child.id, ino));

                    entries.push(Dirent {
                        ino,
                        name: link.name.clone(),
                        file_type: DIRENT_FILE,
                    });
                    report.files += 1;
                    report.bytes += data.len() as u64;
                }
                NodeKind::Free => {}
            }
        }

        let content = build_directory(&entries);
        let runs = allocator.allocate((content.len() / FS_BLOCK) as u32)?;
        write_runs(target, &runs, &content)?;

        inodes.push(InodeRecord {
            ino: pending.ino,
            mode: posix_mode(
                NodeKind::Directory,
                node.perm_owner,
                node.perm_group,
                node.perm_other,
            ),
            uid: node.owner,
            gid: node.group,
            links: link_count,
            size: content.len() as u64,
            runs,
        });
    }

    inodes.push(InodeRecord {
        ino: LOST_FOUND_INO,
        mode: 0o040_700,
        uid: 0,
        gid: 0,
        links: 2,
        size: FS_BLOCK as u64,
        runs: lost_runs,
    });

    // The records must be sorted so that the table is written in a single
    // sweep: the depth-first walk produced them out of order.
    inodes.sort_by_key(|record| record.ino);

    // --- inode table ---
    let per_block = FS_BLOCK / INODE_SIZE;
    let mut cursor = 0usize;
    let mut buffer = alloc::vec![0u8; FS_BLOCK];

    for index in 0..geometry.groups {
        let layout = geometry.group(index);
        for offset in 0..geometry.itable_blocks {
            buffer.fill(0);
            let first = index * geometry.inodes_per_group + offset * per_block as u32 + 1;

            // The records being sorted, those of the current block are
            // always at the head of what is left to write.
            while cursor < inodes.len() {
                let record = &inodes[cursor];
                if record.ino < first || record.ino >= first + per_block as u32 {
                    break;
                }
                let slot = (record.ino - first) as usize * INODE_SIZE;
                buffer[slot..slot + INODE_SIZE].copy_from_slice(&record.encode());
                cursor += 1;
            }

            target.write_block((layout.inode_table + offset) as u64, &buffer)?;
        }
    }

    // --- bitmaps and descriptors ---
    let mut descriptors = alloc::vec![0u8; geometry.gdt_blocks as usize * FS_BLOCK];
    let mut free_blocks_total: u64 = 0;
    let mut free_inodes_total: u32 = 0;

    for index in 0..geometry.groups {
        let layout = geometry.group(index);

        // Block bitmap. The bits beyond the end of the group must be 1:
        // failing which a checker would believe in free blocks beyond the
        // volume.
        buffer.fill(0xFF);
        for position in 0..layout.blocks {
            let block = layout.first_block + position;
            if !allocator.is_used(block) {
                buffer[position as usize / 8] &= !(1 << (position % 8));
            }
        }
        target.write_block(layout.block_bitmap as u64, &buffer)?;

        // Inode bitmap, same padding rule.
        buffer.fill(0xFF);
        let mut used_dirs: u16 = 0;
        let mut free_inodes: u32 = 0;
        for position in 0..geometry.inodes_per_group {
            let ino = index * geometry.inodes_per_group + position + 1;
            match inodes.binary_search_by_key(&ino, |record| record.ino) {
                Ok(slot) => {
                    if inodes[slot].mode & 0o170_000 == 0o040_000 {
                        used_dirs += 1;
                    }
                }
                Err(_) if ino >= FIRST_INO => {
                    buffer[position as usize / 8] &= !(1 << (position % 8));
                    free_inodes += 1;
                }
                // The first ten inodes are reserved by the format: free
                // of content, but never assignable.
                Err(_) => {}
            }
        }
        target.write_block(layout.inode_bitmap as u64, &buffer)?;

        let free_blocks = allocator.free_in(&layout);
        free_blocks_total += free_blocks as u64;
        free_inodes_total += free_inodes;

        let slot = index as usize * DESC_SIZE;
        put_u32(&mut descriptors, slot, layout.block_bitmap);
        put_u32(&mut descriptors, slot + 4, layout.inode_bitmap);
        put_u32(&mut descriptors, slot + 8, layout.inode_table);
        put_u16(&mut descriptors, slot + 12, free_blocks as u16);
        put_u16(&mut descriptors, slot + 14, free_inodes as u16);
        put_u16(&mut descriptors, slot + 16, used_dirs);
    }

    // --- super block and copies ---
    let super_block = build_super_block(&geometry, free_blocks_total, free_inodes_total);

    for index in 0..geometry.groups {
        let layout = geometry.group(index);
        if !layout.has_super {
            continue;
        }

        buffer.fill(0);
        if index == 0 {
            // The main super block lives at byte 1024: the first 1024
            // bytes of the volume belong to the boot sector, which ext4
            // does not use but does not touch either.
            buffer[1024..1024 + super_block.len()].copy_from_slice(&super_block);
        } else {
            // A backup copy occupies the start of the first block of its
            // group, and carries its own group number: that is how
            // `e2fsck -b` knows which one it has just read.
            buffer[..super_block.len()].copy_from_slice(&super_block);
            put_u16(&mut buffer, 0x5A, index as u16);
        }
        target.write_block(layout.first_block as u64, &buffer)?;

        for offset in 0..geometry.gdt_blocks {
            let start = offset as usize * FS_BLOCK;
            target.write_block(
                (layout.first_block + 1 + offset) as u64,
                &descriptors[start..start + FS_BLOCK],
            )?;
        }
    }

    target.flush()?;

    crate::serial_println!(
        "[export] ext4 : {} blocs de {} Kio, {} groupe(s), {} inoeuds dont {} libres, {} blocs libres",
        geometry.blocks,
        FS_BLOCK / 1024,
        geometry.groups,
        geometry.inodes(),
        free_inodes_total,
        free_blocks_total
    );

    Ok(report)
}

/// Builds the 1024 bytes of the super block.
fn build_super_block(geometry: &Geometry, free_blocks: u64, free_inodes: u32) -> [u8; 1024] {
    let mut raw = [0u8; 1024];

    put_u32(&mut raw, 0x00, geometry.inodes());
    put_u32(&mut raw, 0x04, geometry.blocks);
    put_u32(&mut raw, 0x08, 0); // no reserved block: exchange medium
    put_u32(&mut raw, 0x0C, free_blocks as u32);
    put_u32(&mut raw, 0x10, free_inodes);
    put_u32(&mut raw, 0x14, 0); // first data block: 0 if block > 1 KiB
    put_u32(&mut raw, 0x18, LOG_BLOCK_SIZE);
    put_u32(&mut raw, 0x1C, LOG_BLOCK_SIZE);
    put_u32(&mut raw, 0x20, BLOCKS_PER_GROUP);
    put_u32(&mut raw, 0x24, BLOCKS_PER_GROUP);
    put_u32(&mut raw, 0x28, geometry.inodes_per_group);
    put_u32(&mut raw, 0x2C, 0); // last mount
    put_u32(&mut raw, 0x30, FIXED_TIME);
    put_u16(&mut raw, 0x34, 0); // mounts since the last check
    put_u16(&mut raw, 0x36, 0xFFFF); // no periodic check
    put_u16(&mut raw, 0x38, MAGIC);
    put_u16(&mut raw, 0x3A, 1); // clean volume
    put_u16(&mut raw, 0x3C, 1); // errors: carry on
    put_u16(&mut raw, 0x3E, 0);
    put_u32(&mut raw, 0x40, FIXED_TIME);
    put_u32(&mut raw, 0x44, 0);
    put_u32(&mut raw, 0x48, 0); // creator system: Linux
    put_u32(&mut raw, 0x4C, 1); // dynamic revision
    put_u16(&mut raw, 0x50, 0);
    put_u16(&mut raw, 0x52, 0);
    put_u32(&mut raw, 0x54, FIRST_INO);
    put_u16(&mut raw, 0x58, INODE_SIZE as u16);
    put_u16(&mut raw, 0x5A, 0); // number of the group carrying this copy

    put_u32(&mut raw, 0x5C, 0);
    put_u32(&mut raw, 0x60, INCOMPAT_FILETYPE | INCOMPAT_EXTENTS);
    put_u32(&mut raw, 0x64, RO_COMPAT_SPARSE_SUPER | RO_COMPAT_LARGE_FILE);

    // Identifier of the volume. Derived from the geometry rather than
    // drawn at random: with no random generator at boot, a fixed
    // identifier would collide between two exported media. The version
    // and variant bits are set so that the tools recognise it as a
    // version 4 UUID.
    let mut seed: u64 = 0xcbf2_9ce4_8422_2325;
    for value in [
        geometry.blocks as u64,
        geometry.groups as u64,
        geometry.inodes() as u64,
        FIXED_TIME as u64,
    ] {
        seed = (seed ^ value).wrapping_mul(0x1000_0000_01b3);
    }
    for index in 0..16u8 {
        let byte = (seed >> ((index % 8) * 8)) as u8;
        raw[0x68 + index as usize] = byte ^ index.wrapping_mul(31);
    }
    raw[0x68 + 6] = (raw[0x68 + 6] & 0x0F) | 0x40;
    raw[0x68 + 8] = (raw[0x68 + 8] & 0x3F) | 0x80;

    let label = b"nova-export";
    raw[0x78..0x78 + label.len()].copy_from_slice(label);

    put_u16(&mut raw, 0xCE, 0); // no reserved descriptor: no resizing
    put_u32(&mut raw, 0x108, FIXED_TIME);
    put_u16(&mut raw, 0x15C, 32); // minimum i_extra_isize
    put_u16(&mut raw, 0x15E, 32); // wanted i_extra_isize

    raw
}

/// Reads the super block of a produced image back and checks its coherence.
///
/// Checks what a driver checks before mounting: the signature, the block
/// size, the number of groups derived against the one recorded, and the
/// presence of the declared features. Returns the number of blocks and
/// the number of groups.
pub fn verify(device: &mut dyn BlockDevice) -> Result<(u32, u32), FsError> {
    let mut block = alloc::vec![0u8; FS_BLOCK];
    device.read_block(0, &mut block)?;
    let raw = &block[1024..];

    let magic = u16::from_le_bytes([raw[0x38], raw[0x39]]);
    if magic != MAGIC {
        return Err(FsError::Corrupt);
    }

    let blocks = u32::from_le_bytes([raw[0x04], raw[0x05], raw[0x06], raw[0x07]]);
    let log_block = u32::from_le_bytes([raw[0x18], raw[0x19], raw[0x1A], raw[0x1B]]);
    let per_group = u32::from_le_bytes([raw[0x20], raw[0x21], raw[0x22], raw[0x23]]);
    let inodes = u32::from_le_bytes([raw[0x00], raw[0x01], raw[0x02], raw[0x03]]);
    let inode_size = u16::from_le_bytes([raw[0x58], raw[0x59]]);
    let incompat = u32::from_le_bytes([raw[0x60], raw[0x61], raw[0x62], raw[0x63]]);
    let state = u16::from_le_bytes([raw[0x3A], raw[0x3B]]);

    if log_block != LOG_BLOCK_SIZE
        || per_group != BLOCKS_PER_GROUP
        || inode_size as usize != INODE_SIZE
        || state != 1
        || blocks == 0
        || inodes == 0
    {
        return Err(FsError::Corrupt);
    }
    // Without the `extents` bit, a host would read `i_block` as a table
    // of indirect pointers and would find nothing but emptiness.
    if incompat & (INCOMPAT_EXTENTS | INCOMPAT_FILETYPE)
        != (INCOMPAT_EXTENTS | INCOMPAT_FILETYPE)
    {
        return Err(FsError::Corrupt);
    }

    let groups = blocks.div_ceil(per_group);
    Ok((blocks, groups))
}

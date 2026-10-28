//! 4.6.1 -- FAT32 export.
//!
//! FAT32 is the "maximum compatibility" format of 4.4: it mounts with no
//! extra driver under Windows, Linux, macOS, and on nearly every device
//! that accepts a USB stick.
//!
//! Layout produced:
//!
//! ```text
//!   sector 0         boot sector (BPB)
//!   sector 1         FSInfo
//!   sector 6         backup copy of the boot sector
//!   sector 32        first allocation table
//!   ...              second allocation table
//!   ...              data area, cluster 2 = root directory
//! ```
//!
//! ## Two ways of naming a file
//!
//! Original FAT knows nothing but the 8.3 format in uppercase. The long
//! names are stored in extra entries -- the LFN entries -- placed
//! **before** the short entry, in reverse order, and attached to it by a
//! checksum of its 8.3 name. A system ignoring the LFNs sees only the
//! short name; a system that understands them rebuilds the complete name.
//! That is what makes the format compatible with the 55-character names
//! Nova OS allows.
//!
//! ## Timestamps
//!
//! For want of a real-time clock -- the `SYS_TIME_*` domain, step 5.6.3
//! -- every entry carries a fixed date. A manifestly conventional date is
//! worth more than a wrong one derived from the tick counter.

use alloc::string::String;
use alloc::vec::Vec;

use super::super::block::{BLOCK_SIZE, BlockDevice, SECTOR_SIZE};
use super::super::format::{NodeId, NodeKind};
use super::super::perm::{Credentials, Permissions};
use super::super::FsError;
use super::{ExportFormat, ExportReport, join_path};

/// Sectors per cluster: 2 x 512 = 1 KiB.
///
/// Larger clusters would waste less allocation table, but the format
/// demands at least 65 525 clusters to be recognised as FAT32: at 4 KiB
/// per cluster, the smallest acceptable medium would be 256 MiB. At 1 KiB
/// it is 64, which makes the export usable on a small stick.
const SECTORS_PER_CLUSTER: u32 = 2;
/// Sectors reserved before the first allocation table.
const RESERVED_SECTORS: u32 = 32;
/// Two allocation tables, as usage demands.
const FAT_COUNT: u32 = 2;
/// First usable cluster: entries 0 and 1 are reserved.
const FIRST_CLUSTER: u32 = 2;
/// End-of-chain marker.
const END_OF_CHAIN: u32 = 0x0FFF_FFFF;

/// Size of a directory entry.
const DIR_ENTRY: usize = 32;

// --- attributes ---
const ATTR_READ_ONLY: u8 = 0x01;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_LONG_NAME: u8 = 0x0F;

/// Volume label, on exactly eleven bytes.
const VOLUME_LABEL: &[u8; 11] = b"NOVA EXPORT";

/// 1 January 2026, midnight -- conventional date, see the header.
const FIXED_DATE: u16 = ((2026 - 1980) << 9) | (1 << 5) | 1;
const FIXED_TIME: u16 = 0;

// ============================================================
//  Writing by sectors
// ============================================================

/// Translates 512-byte writes towards a medium that accepts only 4 KiB
/// blocks.
///
/// The current block is held in memory: writing eight consecutive sectors
/// causes a single write. Without that, producing a 64 MiB image would
/// demand eight times more I/O.
///
/// ## Why a map of the blocks already touched
///
/// A 4 KiB block covers eight sectors, and nothing guarantees the regions
/// of the format align on it: the last allocation table ends in the
/// middle of a block whose second half already belongs to the data area.
/// A blank medium must be filled with zeros where the format says
/// nothing, but **overwriting an already written block with zeros** loses
/// what it held -- that is what emptied the root directory of the image
/// produced.
///
/// Hence the map: the first access to a block starts from zeros, the
/// following ones read back what has been written there.
struct SectorWriter<'a> {
    device: &'a mut dyn BlockDevice,
    buffer: Vec<u8>,
    /// Block currently in memory.
    resident: Option<u64>,
    dirty: bool,
    /// One bit per block: has this block already been written?
    touched: Vec<u64>,
    writes: u64,
}

impl<'a> SectorWriter<'a> {
    fn new(device: &'a mut dyn BlockDevice) -> Self {
        let blocks = device.block_count() as usize;
        SectorWriter {
            device,
            buffer: alloc::vec![0u8; BLOCK_SIZE],
            resident: None,
            dirty: false,
            touched: alloc::vec![0u64; blocks.div_ceil(64)],
            writes: 0,
        }
    }

    fn is_touched(&self, block: u64) -> bool {
        let index = block as usize / 64;
        match self.touched.get(index) {
            Some(word) => word & (1 << (block % 64)) != 0,
            None => false,
        }
    }

    fn mark_touched(&mut self, block: u64) {
        let index = block as usize / 64;
        if let Some(word) = self.touched.get_mut(index) {
            *word |= 1 << (block % 64);
        }
    }

    fn total_sectors(&self) -> u32 {
        (self.device.block_count() * (BLOCK_SIZE / SECTOR_SIZE) as u64).min(u32::MAX as u64) as u32
    }

    fn load(&mut self, block: u64) -> Result<(), FsError> {
        if self.resident == Some(block) {
            return Ok(());
        }
        self.commit()?;

        if self.is_touched(block) {
            // Already written: it must be read back, on pain of erasing
            // what an earlier pass put there.
            self.device.read_block(block, &mut self.buffer)?;
        } else {
            // Never written: everything the format does not fill in must
            // be zero, including on a medium that carried something else
            // before.
            self.buffer.fill(0);
            self.mark_touched(block);
        }

        self.resident = Some(block);
        Ok(())
    }

    fn commit(&mut self) -> Result<(), FsError> {
        if let (Some(block), true) = (self.resident, self.dirty) {
            self.device.write_block(block, &self.buffer)?;
            self.writes += 1;
            self.dirty = false;
        }
        Ok(())
    }

    /// Writes a 512-byte sector.
    fn write_sector(&mut self, lba: u32, data: &[u8]) -> Result<(), FsError> {
        let per_block = (BLOCK_SIZE / SECTOR_SIZE) as u32;
        let block = (lba / per_block) as u64;
        let offset = (lba % per_block) as usize * SECTOR_SIZE;

        self.load(block)?;
        let length = data.len().min(SECTOR_SIZE);
        self.buffer[offset..offset + length].copy_from_slice(&data[..length]);
        if length < SECTOR_SIZE {
            self.buffer[offset + length..offset + SECTOR_SIZE].fill(0);
        }
        self.dirty = true;
        Ok(())
    }

    /// Fills a range of sectors with zeros.
    fn zero_range(&mut self, first: u32, count: u32) -> Result<(), FsError> {
        let empty = [0u8; SECTOR_SIZE];
        for lba in first..first + count {
            self.write_sector(lba, &empty)?;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<u64, FsError> {
        self.commit()?;
        self.device.flush()?;
        Ok(self.writes)
    }
}

fn put_u16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

// ============================================================
//  Names
// ============================================================

/// Does a name fit as-is in the 8.3 format?
fn fits_short(name: &str) -> bool {
    let (base, extension) = match name.rfind('.') {
        Some(index) if index > 0 => (&name[..index], &name[index + 1..]),
        _ => (name, ""),
    };
    if base.is_empty() || base.len() > 8 || extension.len() > 3 {
        return false;
    }
    name.bytes()
        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'.' || b == b'_' || b == b'-')
}

/// Builds an 8.3 name from an arbitrary name.
///
/// `index` is the collision number: the `~1`, `~2`... suffix is what
/// tells apart two long names reducing to the same short name.
fn short_name(name: &str, index: u32) -> [u8; 11] {
    let mut short = [b' '; 11];

    let (base, extension) = match name.rfind('.') {
        Some(position) if position > 0 => (&name[..position], &name[position + 1..]),
        _ => (name, ""),
    };

    let clean = |source: &str, target: &mut [u8]| -> usize {
        let mut written = 0;
        for byte in source.bytes() {
            if written >= target.len() {
                break;
            }
            let converted = match byte {
                b'a'..=b'z' => byte - 32,
                b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' => byte,
                // Everything FAT refuses becomes an underscore rather
                // than being removed: two distinct names stay distinct.
                _ => b'_',
            };
            target[written] = converted;
            written += 1;
        }
        written
    };

    if index == 0 && fits_short(name) {
        let written = clean(base, &mut short[..8]);
        let _ = written;
        clean(extension, &mut short[8..11]);
        return short;
    }

    // Collision suffix: `~N` occupies the end of the eight characters.
    let mut suffix = [0u8; 8];
    let mut length = 0;
    let mut value = index.max(1);
    while value > 0 && length < 6 {
        suffix[length] = b'0' + (value % 10) as u8;
        value /= 10;
        length += 1;
    }

    let keep = 8usize.saturating_sub(length + 1);
    let written = clean(&base[..base.len().min(keep)], &mut short[..keep]);
    short[written] = b'~';
    for i in 0..length {
        short[written + 1 + i] = suffix[length - 1 - i];
    }
    clean(extension, &mut short[8..11]);
    short
}

/// Checksum binding the LFN entries to their short entry.
fn short_checksum(short: &[u8; 11]) -> u8 {
    let mut sum: u8 = 0;
    for byte in short.iter() {
        sum = ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(*byte);
    }
    sum
}

// ============================================================
//  Directory entries
// ============================================================

/// Adds a complete entry -- LFN entries then short entry.
fn push_entry(
    directory: &mut Vec<u8>,
    name: &str,
    short: [u8; 11],
    attributes: u8,
    cluster: u32,
    size: u32,
) {
    // The LFN entries are only emitted if the short name is not enough:
    // generating them systematically would double the size of the
    // directories for nothing.
    if !fits_short(name) {
        let checksum = short_checksum(&short);
        let units: Vec<u16> = name.encode_utf16().collect();
        let parts = units.len().div_ceil(13);

        // Reverse order: the last portion comes first, marked by bit 6 of
        // the sequence number.
        for part in (0..parts).rev() {
            let mut entry = [0u8; DIR_ENTRY];
            entry[0] = (part as u8 + 1) | if part == parts - 1 { 0x40 } else { 0 };
            entry[0x0B] = ATTR_LONG_NAME;
            entry[0x0C] = 0;
            entry[0x0D] = checksum;
            // The cluster is always null in an LFN entry.
            put_u16(&mut entry, 0x1A, 0);

            // Five characters, then six, then two: split imposed by the
            // format, which fits around the inherited fields.
            const SLOTS: [usize; 13] = [
                0x01, 0x03, 0x05, 0x07, 0x09, 0x0E, 0x10, 0x12, 0x14, 0x16, 0x18, 0x1C, 0x1E,
            ];
            for (slot, offset) in SLOTS.iter().enumerate() {
                let index = part * 13 + slot;
                let value = if index < units.len() {
                    units[index]
                } else if index == units.len() {
                    0x0000
                } else {
                    0xFFFF
                };
                put_u16(&mut entry, *offset, value);
            }
            directory.extend_from_slice(&entry);
        }
    }

    let mut entry = [0u8; DIR_ENTRY];
    entry[..11].copy_from_slice(&short);
    entry[0x0B] = attributes;
    put_u16(&mut entry, 0x0E, FIXED_TIME);
    put_u16(&mut entry, 0x10, FIXED_DATE);
    put_u16(&mut entry, 0x12, FIXED_DATE);
    put_u16(&mut entry, 0x14, (cluster >> 16) as u16);
    put_u16(&mut entry, 0x16, FIXED_TIME);
    put_u16(&mut entry, 0x18, FIXED_DATE);
    put_u16(&mut entry, 0x1A, cluster as u16);
    put_u32(&mut entry, 0x1C, size);
    directory.extend_from_slice(&entry);
}

/// `.` and `..` entries, mandatory at the head of every sub-directory.
fn push_dot_entries(directory: &mut Vec<u8>, own: u32, parent: u32) {
    let mut dot = [b' '; 11];
    dot[0] = b'.';
    push_entry(directory, "DOT", dot, ATTR_DIRECTORY, own, 0);

    let mut dotdot = [b' '; 11];
    dotdot[0] = b'.';
    dotdot[1] = b'.';
    // The cluster of the parent is 0 when it is the root: that is what
    // the format expects, the root having no cluster number of its own in
    // that field.
    let parent = if parent == FIRST_CLUSTER { 0 } else { parent };
    push_entry(directory, "DOTDOT", dotdot, ATTR_DIRECTORY, parent, 0);
}

// ============================================================
//  Geometry
// ============================================================

struct Geometry {
    total_sectors: u32,
    fat_size: u32,
    first_data_sector: u32,
    cluster_count: u32,
}

impl Geometry {
    /// Computation of the size of the allocation tables.
    ///
    /// Formula from the Microsoft specification (fatgen103): the number
    /// of FAT sectors and the number of clusters determine each other,
    /// and the division below breaks the circularity by over-estimating.
    fn compute(total_sectors: u32) -> Result<Self, FsError> {
        if total_sectors < RESERVED_SECTORS + 128 {
            return Err(FsError::NoSpace);
        }

        let usable = total_sectors - RESERVED_SECTORS;
        let divisor = ((256 * SECTORS_PER_CLUSTER) + FAT_COUNT) / 2;
        let fat_size = usable.div_ceil(divisor).max(1);

        let first_data_sector = RESERVED_SECTORS + FAT_COUNT * fat_size;
        if first_data_sector >= total_sectors {
            return Err(FsError::NoSpace);
        }
        let cluster_count = (total_sectors - first_data_sector) / SECTORS_PER_CLUSTER;

        // Below 65 525 clusters, the format would be FAT16: a host system
        // would trust the cluster count, not the label, and would misread
        // 32-bit entries.
        if cluster_count < 65_525 {
            return Err(FsError::NoSpace);
        }

        Ok(Geometry {
            total_sectors,
            fat_size,
            first_data_sector,
            cluster_count,
        })
    }

    fn cluster_to_sector(&self, cluster: u32) -> u32 {
        self.first_data_sector + (cluster - FIRST_CLUSTER) * SECTORS_PER_CLUSTER
    }
}

fn boot_sector(geometry: &Geometry, label: &str) -> [u8; SECTOR_SIZE] {
    let mut sector = [0u8; SECTOR_SIZE];

    // Short jump to non-existent boot code: the image is not bootable,
    // but byte 0 must be 0xEB or 0xE9 for the volume to be recognised.
    // volume soit reconnu.
    sector[0] = 0xEB;
    sector[1] = 0x58;
    sector[2] = 0x90;
    sector[3..11].copy_from_slice(b"NOVAOS  ");

    put_u16(&mut sector, 0x0B, SECTOR_SIZE as u16);
    sector[0x0D] = SECTORS_PER_CLUSTER as u8;
    put_u16(&mut sector, 0x0E, RESERVED_SECTORS as u16);
    sector[0x10] = FAT_COUNT as u8;
    put_u16(&mut sector, 0x11, 0); // root entries: null on FAT32
    put_u16(&mut sector, 0x13, 0); // 16-bit total: null, see 0x20
    sector[0x15] = 0xF8; // fixed medium
    put_u16(&mut sector, 0x16, 0); // 16-bit FAT size: null on FAT32
    put_u16(&mut sector, 0x18, 32); // sectors per track
    put_u16(&mut sector, 0x1A, 8); // heads
    put_u32(&mut sector, 0x1C, 0); // hidden sectors

    put_u32(&mut sector, 0x20, geometry.total_sectors);
    put_u32(&mut sector, 0x24, geometry.fat_size);
    put_u16(&mut sector, 0x28, 0); // both FATs are kept up to date
    put_u16(&mut sector, 0x2A, 0); // format version
    put_u32(&mut sector, 0x2C, FIRST_CLUSTER); // root cluster
    put_u16(&mut sector, 0x30, 1); // FSInfo sector
    put_u16(&mut sector, 0x32, 6); // backup copy

    sector[0x40] = 0x80; // drive number
    sector[0x42] = 0x29; // extended signature present
    put_u32(&mut sector, 0x43, 0x4E4F_5641); // identifier "NOVA"

    let mut volume = [b' '; 11];
    for (index, byte) in label.bytes().take(11).enumerate() {
        volume[index] = byte.to_ascii_uppercase();
    }
    sector[0x47..0x52].copy_from_slice(&volume);
    sector[0x52..0x5A].copy_from_slice(b"FAT32   ");

    sector[0x1FE] = 0x55;
    sector[0x1FF] = 0xAA;
    sector
}

fn fsinfo_sector(free_clusters: u32, next_free: u32) -> [u8; SECTOR_SIZE] {
    let mut sector = [0u8; SECTOR_SIZE];
    put_u32(&mut sector, 0x000, 0x4161_5252);
    put_u32(&mut sector, 0x1E4, 0x6141_7272);
    put_u32(&mut sector, 0x1E8, free_clusters);
    put_u32(&mut sector, 0x1EC, next_free);
    put_u32(&mut sector, 0x1FC, 0xAA55_0000);
    sector
}

// ============================================================
//  Export
// ============================================================

/// A tree entry left to handle.
struct Pending {
    /// Nova OS path.
    path: String,
    /// Cluster already reserved for this directory.
    cluster: u32,
    parent_cluster: u32,
}

/// Rewrites a Nova OS tree as FAT32 on a blank medium.
pub fn export(
    source: &str,
    target: &mut dyn BlockDevice,
    who: Credentials,
) -> Result<ExportReport, FsError> {
    if target.read_only() {
        return Err(FsError::Block(
            super::super::block::BlockError::ReadOnly,
        ));
    }

    let mut report = ExportReport::new(ExportFormat::Fat32);
    let mut writer = SectorWriter::new(target);
    let geometry = Geometry::compute(writer.total_sectors())?;

    // Whole allocation table in memory: four bytes per cluster, that is
    // 256 KiB for a 256 MiB medium. Building it in one block avoids
    // thousands of sector re-reads during the walk.
    let mut fat: Vec<u32> = alloc::vec![0u32; (geometry.cluster_count + FIRST_CLUSTER) as usize];
    fat[0] = 0x0FFF_FFF8;
    fat[1] = END_OF_CHAIN;

    let mut next_cluster = FIRST_CLUSTER;
    let allocate = |fat: &mut Vec<u32>, count: u32, next: &mut u32| -> Result<u32, FsError> {
        if count == 0 {
            return Ok(0);
        }
        let first = *next;
        if first + count > geometry.cluster_count + FIRST_CLUSTER {
            return Err(FsError::NoSpace);
        }
        for offset in 0..count {
            let cluster = first + offset;
            fat[cluster as usize] = if offset + 1 == count {
                END_OF_CHAIN
            } else {
                cluster + 1
            };
        }
        *next = first + count;
        Ok(first)
    };

    // The root directory occupies cluster 2; its real size is only known
    // after the walk, and extra clusters will be reserved for it at the
    // moment of writing it.
    let root_cluster = allocate(&mut fat, 1, &mut next_cluster)?;
    debug_assert_eq!(root_cluster, FIRST_CLUSTER);

    let mut queue: Vec<Pending> = alloc::vec![Pending {
        path: String::from(source),
        cluster: root_cluster,
        parent_cluster: root_cluster,
    }];

    // Directory contents, written once every cluster is known.
    let mut directories: Vec<(u32, Vec<u8>)> = Vec::new();

    // Directories already walked. Spec 5 allows `link` on a directory: the
    // tree is therefore a graph, and a link back to an ancestor would make
    // this loop turn forever.
    let mut visited: Vec<NodeId> = alloc::vec![super::super::stat(source, who)?.id];

    while let Some(pending) = queue.pop() {
        let entries = match super::super::list(&pending.path, who) {
            Ok(entries) => entries,
            Err(error) => {
                report
                    .skipped
                    .push(alloc::format!("{} ({})", pending.path, error.message()));
                continue;
            }
        };

        let mut content: Vec<u8> = Vec::new();

        if pending.cluster == root_cluster {
            // The root carries the volume label, no `.` entries. It must
            // be identical to that of the boot sector: a divergence makes
            // the checking tools report an anomaly, since they compare
            // them.
            let mut label = [b' '; 11];
            label[..VOLUME_LABEL.len()].copy_from_slice(VOLUME_LABEL);
            push_entry(&mut content, "NOVA", label, ATTR_VOLUME_ID, 0, 0);
        } else {
            push_dot_entries(&mut content, pending.cluster, pending.parent_cluster);
        }

        let mut sorted = entries;
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        let mut used_shorts: Vec<[u8; 11]> = Vec::new();

        for link in sorted.iter() {
            let child_path = join_path(&pending.path, &link.name);

            let node = match super::super::stat(&child_path, who) {
                Ok(node) => node,
                Err(error) => {
                    report
                        .skipped
                        .push(alloc::format!("{} ({})", child_path, error.message()));
                    continue;
                }
            };

            // The short name must be unique in its directory: it is the
            // one a system ignoring the long entries sees.
            let mut short = short_name(&link.name, 0);
            let mut collision = 1;
            while used_shorts.contains(&short) {
                short = short_name(&link.name, collision);
                collision += 1;
            }
            used_shorts.push(short);

            match node.kind {
                NodeKind::Directory => {
                    if visited.contains(&node.id) {
                        report.skipped.push(alloc::format!(
                            "{} (dossier deja exporte - FAT32 n'a pas de lien de dossier)",
                            child_path
                        ));
                        continue;
                    }
                    visited.push(node.id);

                    let cluster = allocate(&mut fat, 1, &mut next_cluster)?;
                    push_entry(&mut content, &link.name, short, ATTR_DIRECTORY, cluster, 0);
                    queue.push(Pending {
                        path: child_path,
                        cluster,
                        parent_cluster: pending.cluster,
                    });
                    report.directories += 1;
                }
                NodeKind::File => {
                    let data = match super::super::read_file(&child_path, who) {
                        Ok(data) => data,
                        Err(error) => {
                            report
                                .skipped
                                .push(alloc::format!("{} ({})", child_path, error.message()));
                            continue;
                        }
                    };

                    let clusters =
                        (data.len() as u32).div_ceil(SECTORS_PER_CLUSTER * SECTOR_SIZE as u32);
                    let first = allocate(&mut fat, clusters, &mut next_cluster)?;

                    // Immediate write: keeping the content in memory
                    // until the end of the walk would make the whole tree
                    // fit in the heap.
                    let mut written = 0usize;
                    let mut cluster = first;
                    while written < data.len() {
                        let sector_base = geometry.cluster_to_sector(cluster);
                        for offset in 0..SECTORS_PER_CLUSTER {
                            if written >= data.len() {
                                break;
                            }
                            let end = (written + SECTOR_SIZE).min(data.len());
                            writer.write_sector(sector_base + offset, &data[written..end])?;
                            written = end;
                        }
                        cluster = fat[cluster as usize];
                        if cluster >= END_OF_CHAIN {
                            break;
                        }
                    }

                    // A file its owner cannot rewrite comes out read-only:
                    // it is the only nuance of permission FAT can carry.
                    let mut attributes = ATTR_ARCHIVE;
                    if !node.perm_owner.contains(Permissions::OVERWRITE) {
                        attributes |= ATTR_READ_ONLY;
                    }

                    push_entry(
                        &mut content,
                        &link.name,
                        short,
                        attributes,
                        if data.is_empty() { 0 } else { first },
                        data.len() as u32,
                    );
                    report.files += 1;
                    report.bytes += data.len() as u64;
                }
                NodeKind::Free => {}
            }
        }

        directories.push((pending.cluster, content));
    }

    // --- writing of the directories ---
    let cluster_bytes = SECTORS_PER_CLUSTER as usize * SECTOR_SIZE;
    for (first, content) in directories.iter() {
        let needed = content.len().div_ceil(cluster_bytes).max(1) as u32;

        // A directory reserves one cluster at its creation; if more are
        // needed, the chain is extended now that its final size is known.
        let mut cluster = *first;
        if needed > 1 {
            let extra = allocate(&mut fat, needed - 1, &mut next_cluster)?;
            fat[*first as usize] = extra;
        }

        let mut written = 0usize;
        loop {
            let sector_base = geometry.cluster_to_sector(cluster);
            for offset in 0..SECTORS_PER_CLUSTER {
                let end = (written + SECTOR_SIZE).min(content.len());
                if written >= content.len() {
                    // The rest of the cluster must be zero: an entry whose
                    // first byte is null marks the end of the directory
                    // for the host system.
                    writer.write_sector(sector_base + offset, &[0u8; SECTOR_SIZE])?;
                } else {
                    writer.write_sector(sector_base + offset, &content[written..end])?;
                    written = end;
                }
            }
            let next = fat[cluster as usize];
            if next >= END_OF_CHAIN {
                break;
            }
            cluster = next;
        }
    }

    // --- reserved areas ---
    writer.zero_range(0, RESERVED_SECTORS)?;

    let free_clusters = geometry.cluster_count + FIRST_CLUSTER - next_cluster;
    let label = core::str::from_utf8(VOLUME_LABEL).unwrap_or("NOVA");
    writer.write_sector(0, &boot_sector(&geometry, label))?;
    writer.write_sector(1, &fsinfo_sector(free_clusters, next_cluster))?;
    // Backup copy: an unreadable sector 0 does not prevent mounting.
    writer.write_sector(6, &boot_sector(&geometry, label))?;
    writer.write_sector(7, &fsinfo_sector(free_clusters, next_cluster))?;

    // --- allocation tables ---
    let entries_per_sector = SECTOR_SIZE / 4;
    let mut sector = [0u8; SECTOR_SIZE];

    for copy in 0..FAT_COUNT {
        let base = RESERVED_SECTORS + copy * geometry.fat_size;
        for index in 0..geometry.fat_size {
            sector.fill(0);
            let start = index as usize * entries_per_sector;
            for slot in 0..entries_per_sector {
                let cluster = start + slot;
                if cluster >= fat.len() {
                    break;
                }
                put_u32(&mut sector, slot * 4, fat[cluster] & 0x0FFF_FFFF);
            }
            writer.write_sector(base + index, &sector)?;
        }
    }

    let writes = writer.finish()?;
    crate::serial_println!(
        "[export] FAT32 : {} secteurs, {} clusters de {} Kio, {} clusters utilises, {} ecritures",
        geometry.total_sectors,
        geometry.cluster_count,
        SECTORS_PER_CLUSTER * SECTOR_SIZE as u32 / 1024,
        next_cluster - FIRST_CLUSTER,
        writes
    );

    Ok(report)
}

/// Reads the header of a produced image back and checks it is coherent.
///
/// Checks what a host system would check before mounting: the signature,
/// the format label, the sector and cluster size, and above all the
/// cluster count -- it is that, and not the "FAT32" label, that decides
/// how a system reads the entries.
pub fn verify(device: &mut dyn BlockDevice) -> Result<(u32, u32), FsError> {
    let mut block = alloc::vec![0u8; BLOCK_SIZE];
    device.read_block(0, &mut block)?;

    if block[0x1FE] != 0x55 || block[0x1FF] != 0xAA {
        return Err(FsError::Corrupt);
    }
    if &block[0x52..0x5A] != b"FAT32   " {
        return Err(FsError::Corrupt);
    }

    let bytes_per_sector = u16::from_le_bytes([block[0x0B], block[0x0C]]) as u32;
    let sectors_per_cluster = block[0x0D] as u32;
    let reserved = u16::from_le_bytes([block[0x0E], block[0x0F]]) as u32;
    let fat_count = block[0x10] as u32;
    let total_sectors = u32::from_le_bytes([block[0x20], block[0x21], block[0x22], block[0x23]]);
    let fat_size = u32::from_le_bytes([block[0x24], block[0x25], block[0x26], block[0x27]]);
    let root_cluster = u32::from_le_bytes([block[0x2C], block[0x2D], block[0x2E], block[0x2F]]);

    if bytes_per_sector != SECTOR_SIZE as u32
        || sectors_per_cluster == 0
        || reserved == 0
        || fat_count == 0
        || root_cluster < FIRST_CLUSTER
    {
        return Err(FsError::Corrupt);
    }

    let first_data = reserved + fat_count * fat_size;
    if first_data >= total_sectors {
        return Err(FsError::Corrupt);
    }
    let clusters = (total_sectors - first_data) / sectors_per_cluster;
    if clusters < 65_525 {
        return Err(FsError::Corrupt);
    }

    Ok((total_sectors, clusters))
}

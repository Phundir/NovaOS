//! 14.0 -- Partition tables: GPT, and MBR for older disks (spec 4.5).
//!
//! ## Why the kernel reads them at all
//!
//! Until phase 14 a volume took a whole disk, and a disk that did not carry
//! NovaFS was formatted when it came first: on a real machine, a Windows or
//! Linux disk plugged first would have been wiped (DECISIONS, "Nova ne
//! formate plus rien de lui-même"). Reading the partition table is what
//! lets Nova find its own partitions on a shared disk and leave the others
//! exactly as they are.
//!
//! ## What is read, and how far it is trusted
//!
//! - **GPT**: the protective MBR (a type `0xEE` entry), the header at LBA 1
//!   -- signature, its CRC32, the entry array's CRC32 -- and the entries. A
//!   header whose checksums do not hold is not used: a disk with a damaged
//!   table is left alone rather than guessed at. The backup header at the
//!   end of the disk is not read (a limit, stated).
//! - **MBR**: the four primary entries of a disk with the `0x55AA`
//!   signature and no `0xEE` entry. Extended partitions are listed as such
//!   and not walked.
//!
//! A partition is usable by Nova only if it starts on a 4 KiB boundary (the
//! block of the filesystem, eight sectors): everything else is reported and
//! skipped. A partition becomes a `PartitionDevice`: a window of the disk,
//! its bounds checked on every access, so that a filesystem bug in one
//! partition can never write into its neighbour.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::block::{BlockDevice, BlockError, MediumKind, BLOCK_SIZE};
use crate::vga::SpinLock;

pub mod write;

const SECTOR: u64 = 512;
const SECTORS_PER_BLOCK: u64 = BLOCK_SIZE as u64 / SECTOR;

/// A GUID as GPT stores it: the first three fields little-endian.
pub type Guid = [u8; 16];

/// Partition type of LIFS (spec 4.5): `BD095063-2621-48A5-89A3-0310280C1AF0`.
pub const LIFS_TYPE: Guid = guid(0xBD09_5063, 0x2621, 0x48A5, [0x89, 0xA3, 0x03, 0x10, 0x28, 0x0C, 0x1A, 0xF0]);
/// Partition type of a NovaFS volume in a partition:
/// `A395A30A-8F34-4F37-B123-AE9B7E682E3A`.
pub const NOVAFS_TYPE: Guid = guid(0xA395_A30A, 0x8F34, 0x4F37, [0xB1, 0x23, 0xAE, 0x9B, 0x7E, 0x68, 0x2E, 0x3A]);
/// The MBR type byte of LIFS (spec 4.5).
pub const LIFS_MBR_TYPE: u8 = 0x4C;

/// Lays a GUID out in its on-disk byte order.
pub const fn guid(a: u32, b: u16, c: u16, d: [u8; 8]) -> Guid {
    let a = a.to_le_bytes();
    let b = b.to_le_bytes();
    let c = c.to_le_bytes();
    [a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]]
}

/// A GUID in its usual text form, for the log and the command line.
pub fn guid_text(guid: &Guid) -> String {
    let a = u32::from_le_bytes([guid[0], guid[1], guid[2], guid[3]]);
    let b = u16::from_le_bytes([guid[4], guid[5]]);
    let c = u16::from_le_bytes([guid[6], guid[7]]);
    alloc::format!(
        "{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        a, b, c, guid[8], guid[9], guid[10], guid[11], guid[12], guid[13], guid[14], guid[15]
    )
}

/// Parses `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX`, any case.
pub fn parse_guid(text: &str) -> Option<Guid> {
    let hex: Vec<u8> = text.bytes().filter(|b| *b != b'-').collect();
    if hex.len() != 32 || text.len() != 36 {
        return None;
    }
    let digit = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    let mut raw = [0u8; 16];
    for (index, pair) in hex.chunks(2).enumerate() {
        raw[index] = digit(pair[0])? << 4 | digit(pair[1])?;
    }
    // Text order is big-endian throughout; GPT stores the first three
    // fields little-endian.
    Some(guid(
        u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]),
        u16::from_be_bytes([raw[4], raw[5]]),
        u16::from_be_bytes([raw[6], raw[7]]),
        [raw[8], raw[9], raw[10], raw[11], raw[12], raw[13], raw[14], raw[15]],
    ))
}

/// What a partition table entry says.
#[derive(Debug, Clone)]
pub struct Partition {
    /// Rank in its table, from 1.
    pub index: usize,
    pub kind: PartitionKind,
    /// First 512-byte sector, and number of sectors.
    pub first_sector: u64,
    pub sectors: u64,
    /// GPT: the partition's own GUID; MBR: zero.
    pub unique: Guid,
    /// GPT: its name (ASCII part); MBR: empty.
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionKind {
    Lifs,
    NovaFs,
    /// Anything else: another system's, or unknown -- never touched.
    Foreign(ForeignKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForeignKind {
    Gpt(Guid),
    Mbr(u8),
}

impl Partition {
    /// Usable by Nova: on a block boundary, a whole number of blocks.
    pub fn aligned(&self) -> bool {
        self.first_sector.is_multiple_of(SECTORS_PER_BLOCK) && self.sectors >= SECTORS_PER_BLOCK
    }

    pub fn describe(&self) -> String {
        match self.kind {
            PartitionKind::Lifs => String::from("LIFS"),
            PartitionKind::NovaFs => String::from("NovaFS"),
            PartitionKind::Foreign(ForeignKind::Gpt(guid)) => alloc::format!("type {}", guid_text(&guid)),
            PartitionKind::Foreign(ForeignKind::Mbr(byte)) => alloc::format!("type MBR 0x{:02X}", byte),
        }
    }
}

/// The table found on a disk.
#[derive(Debug, Clone)]
pub enum Table {
    Gpt { disk: Guid, partitions: Vec<Partition> },
    Mbr { partitions: Vec<Partition> },
    /// No table: the disk is one volume, or empty, or foreign.
    None,
    /// A table whose checksums or layout do not hold: the disk is left
    /// alone.
    Damaged(&'static str),
}

// ============================================================
//  CRC32 (IEEE 802.3), as GPT checks its header and entries
// ============================================================

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 != 0 { 0xEDB8_8320 ^ (value >> 1) } else { value >> 1 };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = crc_table();

/// CRC32 of `data`, the one of zlib and GPT.
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0, data)
}

/// Continues a CRC32 over more bytes (`crc` is a previous result).
pub fn crc32_update(crc: u32, data: &[u8]) -> u32 {
    let mut value = !crc;
    for byte in data {
        value = CRC_TABLE[((value ^ *byte as u32) & 0xFF) as usize] ^ (value >> 8);
    }
    !value
}

// ============================================================
//  Reading the table
// ============================================================

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u32_at(bytes, at) as u64 | (u32_at(bytes, at + 4) as u64) << 32
}

/// Reads `count` bytes from byte `offset` of the device.
fn read_bytes(device: &mut dyn BlockDevice, offset: u64, count: usize) -> Result<Vec<u8>, BlockError> {
    let mut out = Vec::with_capacity(count);
    let mut block = alloc::vec![0u8; BLOCK_SIZE];
    let mut at = offset;
    while out.len() < count {
        device.read_block(at / BLOCK_SIZE as u64, &mut block)?;
        let inside = (at % BLOCK_SIZE as u64) as usize;
        let take = (BLOCK_SIZE - inside).min(count - out.len());
        out.extend_from_slice(&block[inside..inside + take]);
        at += take as u64;
    }
    Ok(out)
}

/// Reads the partition table of a disk.
pub fn read_table(device: &mut dyn BlockDevice) -> Table {
    let Ok(head) = read_bytes(device, 0, 1024) else {
        return Table::Damaged("premiers secteurs illisibles");
    };
    if u16_at(&head, 510) != 0xAA55 {
        return Table::None;
    }
    // (status, type, first sector, sectors) of the four entries.
    let entries: Vec<(u8, u8, u32, u32)> = (0..4)
        .map(|i| {
            let e = 446 + i * 16;
            (head[e], head[e + 4], u32_at(&head, e + 8), u32_at(&head, e + 12))
        })
        .collect();
    if entries.iter().any(|(_, kind, _, _)| *kind == 0xEE) {
        return read_gpt(device, &head[512..1024]);
    }
    // A boot sector of a filesystem (FAT, NTFS) also ends in 0x55AA, and
    // its "entries" are code: an MBR is only believed if **every** entry
    // is empty or sane -- status 0x00 or 0x80, a first sector past the
    // MBR, an extent inside the disk -- and one at least is not empty.
    let total = device.block_count() * SECTORS_PER_BLOCK;
    let sane = |(status, kind, first, count): &(u8, u8, u32, u32)| {
        (*kind == 0 && *count == 0)
            || ((*status == 0 || *status == 0x80) && *kind != 0 && *first != 0 && *count != 0 && (*first as u64 + *count as u64) <= total)
    };
    if !entries.iter().all(sane) {
        return Table::None;
    }
    let partitions: Vec<Partition> = entries
        .iter()
        .enumerate()
        .filter(|(_, (_, kind, _, count))| *kind != 0 && *count != 0)
        .map(|(index, (_, kind, first, count))| Partition {
            index: index + 1,
            kind: if *kind == LIFS_MBR_TYPE { PartitionKind::Lifs } else { PartitionKind::Foreign(ForeignKind::Mbr(*kind)) },
            first_sector: *first as u64,
            sectors: *count as u64,
            unique: [0; 16],
            name: String::new(),
        })
        .collect();
    if partitions.is_empty() { Table::None } else { Table::Mbr { partitions } }
}

fn read_gpt(device: &mut dyn BlockDevice, header: &[u8]) -> Table {
    if &header[0..8] != b"EFI PART" {
        return Table::Damaged("MBR protecteur sans en-tete GPT");
    }
    let size = u32_at(header, 12) as usize;
    if !(92..=512).contains(&size) {
        return Table::Damaged("taille d'en-tete GPT invalide");
    }
    let mut copy = header[..size].to_vec();
    copy[16..20].fill(0);
    if crc32(&copy) != u32_at(header, 16) {
        return Table::Damaged("somme de controle de l'en-tete GPT fausse");
    }
    let mut disk = [0u8; 16];
    disk.copy_from_slice(&header[56..72]);
    let entries_lba = u64_at(header, 72);
    let count = u32_at(header, 80) as usize;
    let entry_size = u32_at(header, 84) as usize;
    if !(128..=1024).contains(&entry_size) || count > 1024 || entry_size % 8 != 0 {
        return Table::Damaged("tableau d'entrees GPT invalide");
    }
    let Ok(array) = read_bytes(device, entries_lba * SECTOR, count * entry_size) else {
        return Table::Damaged("entrees GPT illisibles");
    };
    if crc32(&array) != u32_at(header, 88) {
        return Table::Damaged("somme de controle des entrees GPT fausse");
    }
    let total = device.block_count() * SECTORS_PER_BLOCK;
    let mut partitions = Vec::new();
    for index in 0..count {
        let entry = &array[index * entry_size..(index + 1) * entry_size];
        let mut kind = [0u8; 16];
        kind.copy_from_slice(&entry[0..16]);
        if kind == [0; 16] {
            continue;
        }
        let mut unique = [0u8; 16];
        unique.copy_from_slice(&entry[16..32]);
        let first = u64_at(entry, 32);
        let last = u64_at(entry, 40);
        if last < first || last >= total {
            return Table::Damaged("partition GPT hors du disque");
        }
        // The name is UTF-16LE: its ASCII characters are kept.
        let name: String = (0..36)
            .map(|i| u16_at(entry, 56 + i * 2))
            .take_while(|c| *c != 0)
            .map(|c| if (0x20..0x7F).contains(&c) { c as u8 as char } else { '?' })
            .collect();
        partitions.push(Partition {
            index: index + 1,
            kind: match kind {
                LIFS_TYPE => PartitionKind::Lifs,
                NOVAFS_TYPE => PartitionKind::NovaFs,
                other => PartitionKind::Foreign(ForeignKind::Gpt(other)),
            },
            first_sector: first,
            sectors: last - first + 1,
            unique,
            name,
        });
    }
    Table::Gpt { disk, partitions }
}

// ============================================================
//  A partition as a block device
// ============================================================

/// A disk several partitions share.
pub type SharedDisk = Arc<SpinLock<Box<dyn BlockDevice>>>;

/// A window of a disk: blocks `first .. first + count`, every access
/// checked against the bounds.
pub struct PartitionDevice {
    disk: SharedDisk,
    first: u64,
    count: u64,
    name: String,
    kind: MediumKind,
}

impl PartitionDevice {
    /// The window of `partition` on `disk`. `None` if it is not on a
    /// block boundary.
    pub fn new(disk: SharedDisk, partition: &Partition) -> Option<PartitionDevice> {
        if !partition.aligned() {
            return None;
        }
        let (name, kind) = {
            let device = disk.lock();
            (alloc::format!("{} p{}", device.name(), partition.index), device.kind())
        };
        Some(PartitionDevice {
            disk,
            first: partition.first_sector / SECTORS_PER_BLOCK,
            count: partition.sectors / SECTORS_PER_BLOCK,
            name,
            kind,
        })
    }

    fn check(&self, index: u64) -> Result<u64, BlockError> {
        if index >= self.count {
            return Err(BlockError::OutOfRange);
        }
        Ok(self.first + index)
    }
}

impl BlockDevice for PartitionDevice {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn kind(&self) -> MediumKind {
        self.kind
    }

    fn block_count(&self) -> u64 {
        self.count
    }

    fn read_only(&self) -> bool {
        self.disk.lock().read_only()
    }

    fn read_block(&mut self, index: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
        let block = self.check(index)?;
        self.disk.lock().read_block(block, buffer)
    }

    fn write_block(&mut self, index: u64, buffer: &[u8]) -> Result<(), BlockError> {
        let block = self.check(index)?;
        self.disk.lock().write_block(block, buffer)
    }

    fn flush(&mut self) -> Result<(), BlockError> {
        self.disk.lock().flush()
    }

    fn read_to_frames(&mut self, sector: u64, frames: &[u64]) -> Result<(), BlockError> {
        // The window's bounds hold for the whole run.
        let last = sector + frames.len() as u64 * SECTORS_PER_BLOCK;
        if last.div_ceil(SECTORS_PER_BLOCK) > self.count {
            return Err(BlockError::OutOfRange);
        }
        self.disk.lock().read_to_frames(self.first * SECTORS_PER_BLOCK + sector, frames)
    }
}

/// A whole disk handed over as its own device, through the shared lock --
/// what a disk with no table becomes when its one volume is mounted.
pub struct WholeDisk {
    disk: SharedDisk,
}

impl WholeDisk {
    pub fn new(disk: SharedDisk) -> WholeDisk {
        WholeDisk { disk }
    }
}

impl BlockDevice for WholeDisk {
    fn name(&self) -> String {
        self.disk.lock().name()
    }

    fn kind(&self) -> MediumKind {
        self.disk.lock().kind()
    }

    fn block_count(&self) -> u64 {
        self.disk.lock().block_count()
    }

    fn read_only(&self) -> bool {
        self.disk.lock().read_only()
    }

    fn read_block(&mut self, index: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
        self.disk.lock().read_block(index, buffer)
    }

    fn write_block(&mut self, index: u64, buffer: &[u8]) -> Result<(), BlockError> {
        self.disk.lock().write_block(index, buffer)
    }

    fn flush(&mut self) -> Result<(), BlockError> {
        self.disk.lock().flush()
    }

    fn read_to_frames(&mut self, sector: u64, frames: &[u64]) -> Result<(), BlockError> {
        self.disk.lock().read_to_frames(sector, frames)
    }
}

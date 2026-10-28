//! 14.0.4 -- Writing a GPT: a new table on a blank medium, a partition
//! added in the free space of an existing one.
//!
//! Only ever on an explicit request (14.0.1): nothing in the boot path
//! calls this. The layout is the standard one -- protective MBR at LBA 0,
//! primary header at LBA 1, 128 entries of 128 bytes from LBA 2, their
//! copy and the backup header in the last 33 sectors -- and partitions
//! start on 1 MiB boundaries and span whole 4 KiB blocks, so that every
//! one is usable by Nova (`Partition::aligned`) and by the host's tools.
//!
//! The medium is written block by block (4 KiB): a sector-sized change is
//! a read-modify-write of its block.

use alloc::string::String;
use alloc::vec::Vec;

use super::{crc32, read_table, Guid, Partition, PartitionKind, Table, LIFS_TYPE, NOVAFS_TYPE, SECTOR, SECTORS_PER_BLOCK};
use crate::fs::block::{BlockDevice, BlockError, BLOCK_SIZE};

const ENTRIES: u64 = 128;
const ENTRY_SIZE: usize = 128;
/// Sectors of the entry array: 128 x 128 bytes.
const ARRAY_SECTORS: u64 = ENTRIES * ENTRY_SIZE as u64 / SECTOR;
/// Where partitions start, and their alignment: 1 MiB.
const ALIGN: u64 = 2048;

/// A partition to write.
#[derive(Debug, Clone)]
pub struct NewPartition {
    pub kind: Guid,
    pub unique: Guid,
    pub first_sector: u64,
    pub last_sector: u64,
    pub name: String,
}

/// A fresh random GUID (version 4).
pub fn random_guid() -> Guid {
    let mut guid = [0u8; 16];
    crate::random::fill(&mut guid);
    guid[7] = (guid[7] & 0x0F) | 0x40;
    guid[8] = (guid[8] & 0x3F) | 0x80;
    guid
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteError {
    Block(BlockError),
    /// The medium already holds a table (or a damaged one): a new table
    /// would wipe it, which is not this function's call.
    NotBlank,
    /// No table to add a partition to.
    NoTable,
    /// Not enough free space, or all 128 entries in use.
    NoSpace,
    /// Too small for a GPT and one partition.
    TooSmall,
}

impl From<BlockError> for WriteError {
    fn from(error: BlockError) -> Self {
        WriteError::Block(error)
    }
}

/// Writes `data` at byte `offset` of the medium, block by block.
fn write_bytes(device: &mut dyn BlockDevice, offset: u64, data: &[u8]) -> Result<(), BlockError> {
    let mut block = alloc::vec![0u8; BLOCK_SIZE];
    let mut done = 0;
    while done < data.len() {
        let at = offset + done as u64;
        let index = at / BLOCK_SIZE as u64;
        let inside = (at % BLOCK_SIZE as u64) as usize;
        let take = (BLOCK_SIZE - inside).min(data.len() - done);
        if inside != 0 || take != BLOCK_SIZE {
            device.read_block(index, &mut block)?;
        }
        block[inside..inside + take].copy_from_slice(&data[done..done + take]);
        device.write_block(index, &block)?;
        done += take;
    }
    Ok(())
}

fn entry_bytes(p: &NewPartition) -> [u8; ENTRY_SIZE] {
    let mut e = [0u8; ENTRY_SIZE];
    e[0..16].copy_from_slice(&p.kind);
    e[16..32].copy_from_slice(&p.unique);
    e[32..40].copy_from_slice(&p.first_sector.to_le_bytes());
    e[40..48].copy_from_slice(&p.last_sector.to_le_bytes());
    // The name, UTF-16LE: ASCII only, 36 characters at most.
    for (i, c) in p.name.bytes().filter(|c| (0x20..0x7F).contains(c)).take(36).enumerate() {
        e[56 + i * 2] = c;
    }
    e
}

fn header(disk: &Guid, current: u64, backup: u64, last_usable: u64, array_lba: u64, array_crc: u32) -> [u8; SECTOR as usize] {
    let mut h = [0u8; SECTOR as usize];
    h[0..8].copy_from_slice(b"EFI PART");
    h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    h[12..16].copy_from_slice(&92u32.to_le_bytes());
    h[24..32].copy_from_slice(&current.to_le_bytes());
    h[32..40].copy_from_slice(&backup.to_le_bytes());
    h[40..48].copy_from_slice(&(2 + ARRAY_SECTORS).to_le_bytes());
    h[48..56].copy_from_slice(&last_usable.to_le_bytes());
    h[56..72].copy_from_slice(disk);
    h[72..80].copy_from_slice(&array_lba.to_le_bytes());
    h[80..84].copy_from_slice(&(ENTRIES as u32).to_le_bytes());
    h[84..88].copy_from_slice(&(ENTRY_SIZE as u32).to_le_bytes());
    h[88..92].copy_from_slice(&array_crc.to_le_bytes());
    let crc = crc32(&h[..92]);
    h[16..20].copy_from_slice(&crc.to_le_bytes());
    h
}

/// Last sector a partition may use.
fn last_usable(device: &dyn BlockDevice) -> u64 {
    device.block_count() * SECTORS_PER_BLOCK - 1 - ARRAY_SECTORS - 1
}

/// Writes a whole GPT (both copies, and the protective MBR); `slots[i]`
/// is entry `i`, `None` an empty one.
fn write_all(device: &mut dyn BlockDevice, disk: &Guid, slots: &[Option<NewPartition>]) -> Result<(), BlockError> {
    let sectors = device.block_count() * SECTORS_PER_BLOCK;
    let last = sectors - 1;
    let mut array = alloc::vec![0u8; ENTRIES as usize * ENTRY_SIZE];
    for (i, slot) in slots.iter().enumerate().take(ENTRIES as usize) {
        if let Some(p) = slot {
            array[i * ENTRY_SIZE..(i + 1) * ENTRY_SIZE].copy_from_slice(&entry_bytes(p));
        }
    }
    let array_crc = crc32(&array);
    let usable = last_usable(device);

    let mut mbr = [0u8; SECTOR as usize];
    let e = 446;
    mbr[e + 1..e + 4].copy_from_slice(&[0x00, 0x02, 0x00]);
    mbr[e + 4] = 0xEE;
    mbr[e + 5..e + 8].copy_from_slice(&[0xFF, 0xFF, 0xFF]);
    mbr[e + 8..e + 12].copy_from_slice(&1u32.to_le_bytes());
    mbr[e + 12..e + 16].copy_from_slice(&(last.min(u32::MAX as u64) as u32).to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;

    let mut head = Vec::with_capacity((2 * SECTOR) as usize + array.len());
    head.extend_from_slice(&mbr);
    head.extend_from_slice(&header(disk, 1, last, usable, 2, array_crc));
    head.extend_from_slice(&array);
    write_bytes(device, 0, &head)?;

    let backup_array = last - ARRAY_SECTORS;
    let mut tail = Vec::with_capacity(array.len() + SECTOR as usize);
    tail.extend_from_slice(&array);
    tail.extend_from_slice(&header(disk, last, 1, usable, backup_array, array_crc));
    write_bytes(device, backup_array * SECTOR, &tail)?;
    device.flush()
}

/// End of `sector`'s block, as the last sector of a partition ending there.
fn block_end(sector: u64) -> u64 {
    (sector + 1) / SECTORS_PER_BLOCK * SECTORS_PER_BLOCK - 1
}

/// 14.0.4 -- A new GPT on a **blank** medium, with one partition of type
/// `kind` of `sectors` sectors (0: the whole usable space). Refused on a
/// medium that holds a table already. Returns the partition written.
pub fn create_table(device: &mut dyn BlockDevice, kind: Guid, name: &str, sectors: u64) -> Result<NewPartition, WriteError> {
    if !matches!(read_table(device), Table::None) {
        return Err(WriteError::NotBlank);
    }
    let usable = last_usable(device);
    if usable < ALIGN + 2 * ALIGN {
        return Err(WriteError::TooSmall);
    }
    let partition = NewPartition {
        kind,
        unique: random_guid(),
        first_sector: ALIGN,
        last_sector: if sectors == 0 { block_end(usable) } else { (ALIGN + sectors / SECTORS_PER_BLOCK * SECTORS_PER_BLOCK - 1).min(block_end(usable)) },
        name: String::from(name),
    };
    write_all(device, &random_guid(), &[Some(partition.clone())])?;
    Ok(partition)
}

fn as_new(p: &Partition) -> NewPartition {
    NewPartition {
        kind: match p.kind {
            PartitionKind::Lifs => LIFS_TYPE,
            PartitionKind::NovaFs => NOVAFS_TYPE,
            PartitionKind::Foreign(super::ForeignKind::Gpt(guid)) => guid,
            PartitionKind::Foreign(super::ForeignKind::Mbr(_)) => [0; 16],
        },
        unique: p.unique,
        first_sector: p.first_sector,
        last_sector: p.first_sector + p.sectors - 1,
        name: p.name.clone(),
    }
}

/// 14.0.4 -- Adds a partition of `sectors` sectors (0: all the rest) in
/// the free space after the last one. The partitions already there are
/// rewritten exactly as they were -- same types, GUIDs, bounds, names --
/// and their content is not touched.
pub fn add_partition(device: &mut dyn BlockDevice, kind: Guid, name: &str, sectors: u64) -> Result<NewPartition, WriteError> {
    let Table::Gpt { disk, partitions } = read_table(device) else {
        return Err(WriteError::NoTable);
    };
    if partitions.len() as u64 >= ENTRIES {
        return Err(WriteError::NoSpace);
    }
    let usable = last_usable(device);
    let end = partitions.iter().map(|p| p.first_sector + p.sectors).max().unwrap_or(ALIGN);
    let first = end.div_ceil(ALIGN) * ALIGN;
    let last = if sectors == 0 { block_end(usable) } else { first + sectors / SECTORS_PER_BLOCK * SECTORS_PER_BLOCK - 1 };
    if first >= usable || last > usable || last < first + SECTORS_PER_BLOCK {
        return Err(WriteError::NoSpace);
    }
    // Keep each partition at its rank: the entries are rewritten in the
    // slots they occupied, the new one in the first free slot.
    let mut slots: Vec<Option<NewPartition>> = alloc::vec![None; ENTRIES as usize];
    for p in partitions.iter() {
        slots[p.index - 1] = Some(as_new(p));
    }
    let added = NewPartition { kind, unique: random_guid(), first_sector: first, last_sector: last, name: String::from(name) };
    let free = slots.iter().position(|s| s.is_none()).ok_or(WriteError::NoSpace)?;
    slots[free] = Some(added.clone());
    write_all(device, &disk, &slots)?;
    Ok(added)
}

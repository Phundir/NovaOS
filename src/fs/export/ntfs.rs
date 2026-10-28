//! 4.6.3 -- NTFS export.
//!
//! NTFS is the "Windows" destination format of 4.4. It is the most
//! demanding of the three: where FAT32 comes down to a chaining table and
//! ext4 to bitmaps, NTFS is a *self-describing* system. Everything in it
//! is a file, including its own metadata, and a valid volume must
//! therefore hold sixteen system files before carrying any useful data at
//! all.
//!
//! ## Layout produced
//!
//! ```text
//!   cluster 0     $Boot        boot sector and its BPB
//!   cluster 2     $MFTMirr     copy of the first four records
//!   ...           $LogFile     filled with 0xFF: empty journal
//!   ...           $MFT         master file table
//!   ...           $UpCase      uppercasing table, 64 KiB of entries
//!   ...           $AttrDef     definition of the attribute types
//!   ...           $Bitmap      cluster occupancy
//!   ...           data         directory indexes and file content
//!   last sector   backup copy of the boot sector
//! ```
//!
//! ## The two mechanisms that surprise
//!
//! **Fixups.** Every 1 KiB record and every 4 KiB index block ends,
//! sector by sector, with two bytes that are not its data: they carry a
//! sequence number identical throughout the record, and the real bytes
//! are filed in a small array at the head. A half-written sector is then
//! spotted immediately -- its end marker no longer matches. Writing an
//! NTFS structure without applying that substitution produces a record
//! the driver refuses.
//!
//! **Indexes are B-trees.** The content of a directory is not a list: it
//! is a tree sorted by name, whose root fits in the MFT record and whose
//! nodes live in 4 KiB blocks. An entry carrying a child node names the
//! subtree of the names *strictly lower* than its own. The builder below
//! stacks the levels until the root fits in the room left in the record,
//! which makes the depth automatic.
//!
//! ## What the format does not carry, here
//!
//! - **Multiple links become copies.** NTFS can express them -- several
//!   `$FILE_NAME` in one record -- but that would mean rewriting a record
//!   already emitted. ext4 is the format to choose when the links of 4.1
//!   matter.
//! - **A single security descriptor for the whole volume**, granting
//!   access to everyone. Translating thirteen permissions x three classes
//!   into Windows ACLs would demand account identifiers that mean nothing
//!   outside the machine of origin.
//! - **Two names differing only in case** are distinct for Nova OS and
//!   identical for the NTFS sort order. Both are exported, but a Windows
//!   host will see only one.
//! - **No active journal.** `$LogFile` is present and filled with `0xFF`,
//!   which is the "empty" form the drivers recognise -- it is also what
//!   `mkntfs` produces. Nothing is journalled in it.

use alloc::string::String;
use alloc::vec::Vec;

use super::super::FsError;
use super::super::block::{BLOCK_SIZE, BlockDevice};
use super::super::format::{NodeId, NodeKind};
use super::super::perm::Credentials;
use super::{ExportFormat, ExportReport, join_path};

// ============================================================
//  Constants of the format
// ============================================================

const SECTOR: usize = 512;
/// A cluster is one medium block: no translation to do.
const CLUSTER: usize = BLOCK_SIZE;
const SECTORS_PER_CLUSTER: u64 = (CLUSTER / SECTOR) as u64;
/// Size of a record of the master table.
const RECORD: usize = 1024;
const RECORDS_PER_CLUSTER: usize = CLUSTER / RECORD;
/// Size of a directory index block.
const INDEX_BLOCK: usize = CLUSTER;

/// Offset of the first attribute in a record: 48-byte header, then the
/// 3-word sequence array, aligned on 8.
const ATTRS_OFFSET: usize = 0x38;
/// Offset of the sequence array in a record.
const RECORD_USA_OFFSET: usize = 0x30;
/// Offset of the sequence array in an index block.
const INDEX_USA_OFFSET: usize = 0x28;
/// Offset of the entries in an index block, once the nine-word sequence
/// array is laid down and the alignment on 8 restored.
const INDEX_ENTRIES_OFFSET: usize = 0x40;

/// Numbers of the system records.
const MFT_MFT: u64 = 0;
const MFT_MFTMIRR: u64 = 1;
const MFT_LOGFILE: u64 = 2;
const MFT_VOLUME: u64 = 3;
const MFT_ATTRDEF: u64 = 4;
const MFT_ROOT: u64 = 5;
const MFT_BITMAP: u64 = 6;
const MFT_BOOT: u64 = 7;
const MFT_BADCLUS: u64 = 8;
const MFT_SECURE: u64 = 9;
const MFT_UPCASE: u64 = 10;
const MFT_EXTEND: u64 = 11;
/// Records 12 to 15 are reserved by the format.
const MFT_FIRST_USER: u64 = 16;

// --- attribute types ---
const ATTR_STANDARD_INFORMATION: u32 = 0x10;
const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_SECURITY_DESCRIPTOR: u32 = 0x50;
const ATTR_VOLUME_NAME: u32 = 0x60;
const ATTR_VOLUME_INFORMATION: u32 = 0x70;
const ATTR_DATA: u32 = 0x80;
const ATTR_INDEX_ROOT: u32 = 0x90;
const ATTR_INDEX_ALLOCATION: u32 = 0xA0;
const ATTR_BITMAP: u32 = 0xB0;
const ATTR_END: u32 = 0xFFFF_FFFF;

/// Sparse attribute: its ranges with no LCN are holes.
const ATTR_FLAG_SPARSE: u16 = 0x8000;

// --- Windows attributes carried by $STANDARD_INFORMATION and $FILE_NAME ---
const FILE_ATTR_READONLY: u32 = 0x0000_0001;
const FILE_ATTR_HIDDEN: u32 = 0x0000_0002;
const FILE_ATTR_SYSTEM: u32 = 0x0000_0004;
const FILE_ATTR_ARCHIVE: u32 = 0x0000_0020;
/// The "this is a directory" mark as it appears in a `$FILE_NAME`: it is
/// not the `0x10` of the Win32 API, but the "an $I30 index is present"
/// bit the on-disk structure carries.
const FILE_ATTR_DIRECTORY: u32 = 0x1000_0000;

/// Flags of an MFT record.
const RECORD_IN_USE: u16 = 0x0001;
const RECORD_IS_DIRECTORY: u16 = 0x0002;

/// Flags of an index entry.
const INDEX_ENTRY_NODE: u16 = 0x0001;
const INDEX_ENTRY_END: u16 = 0x0002;

/// Name of the index stream of a directory.
const INDEX_NAME: &str = "$I30";

/// 1 January 2026 in hundreds of nanoseconds since 1 January 1601 -- the
/// origin of Windows dates. Same convention as in FAT32 and ext4: for
/// want of a real-time clock, a fixed and recognisable date.
const FIXED_TIME: u64 = 134_116_992_000_000_000;

/// Room reserved in the record of a directory for its
/// `$INDEX_ALLOCATION`, non-resident header, name and ranges included.
const INDEX_ALLOC_RESERVE: usize = 112;
/// Room reserved for the index `$BITMAP`: header, name and 64 bytes of
/// value, that is 512 index blocks per directory.
const INDEX_BITMAP_RESERVE: usize = 24 + 8 + 64;

// ============================================================
//  Writing by clusters
// ============================================================

/// Writes to the medium while holding the current cluster.
///
/// The MFT records are 1 KiB and the medium is addressed in 4 KiB:
/// writing an isolated record means reading its cluster back so as not to
/// erase the other three. The map of the clusters already touched tells
/// "read back what was written there" from "start from zero", the second
/// case holding for a medium that carried something else before.
struct ClusterWriter<'a> {
    device: &'a mut dyn BlockDevice,
    buffer: Vec<u8>,
    resident: Option<u64>,
    dirty: bool,
    touched: Vec<u64>,
    writes: u64,
}

impl<'a> ClusterWriter<'a> {
    fn new(device: &'a mut dyn BlockDevice) -> Self {
        let blocks = device.block_count() as usize;
        ClusterWriter {
            device,
            buffer: alloc::vec![0u8; CLUSTER],
            resident: None,
            dirty: false,
            touched: alloc::vec![0u64; blocks.div_ceil(64)],
            writes: 0,
        }
    }

    fn is_touched(&self, cluster: u64) -> bool {
        match self.touched.get(cluster as usize / 64) {
            Some(word) => word & (1 << (cluster % 64)) != 0,
            None => false,
        }
    }

    fn load(&mut self, cluster: u64) -> Result<(), FsError> {
        if self.resident == Some(cluster) {
            return Ok(());
        }
        self.commit()?;

        if self.is_touched(cluster) {
            self.device.read_block(cluster, &mut self.buffer)?;
        } else {
            self.buffer.fill(0);
            if let Some(word) = self.touched.get_mut(cluster as usize / 64) {
                *word |= 1 << (cluster % 64);
            }
        }
        self.resident = Some(cluster);
        Ok(())
    }

    fn commit(&mut self) -> Result<(), FsError> {
        if let (Some(cluster), true) = (self.resident, self.dirty) {
            self.device.write_block(cluster, &self.buffer)?;
            self.writes += 1;
            self.dirty = false;
        }
        Ok(())
    }

    /// Writes `data` at `offset` in cluster `cluster`.
    fn write_at(&mut self, cluster: u64, offset: usize, data: &[u8]) -> Result<(), FsError> {
        if offset + data.len() > CLUSTER {
            return Err(FsError::Corrupt);
        }
        self.load(cluster)?;
        self.buffer[offset..offset + data.len()].copy_from_slice(data);
        self.dirty = true;
        Ok(())
    }

    /// Writes a whole cluster, with no prior read-back.
    fn write_cluster(&mut self, cluster: u64, data: &[u8]) -> Result<(), FsError> {
        self.load(cluster)?;
        let length = data.len().min(CLUSTER);
        self.buffer[..length].copy_from_slice(&data[..length]);
        self.buffer[length..].fill(0);
        self.dirty = true;
        Ok(())
    }

    /// Fills a run of clusters with one byte.
    fn fill_range(&mut self, first: u64, count: u64, byte: u8) -> Result<(), FsError> {
        let pattern = alloc::vec![byte; CLUSTER];
        for cluster in first..first + count {
            self.write_cluster(cluster, &pattern)?;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<u64, FsError> {
        self.commit()?;
        self.device.flush()?;
        Ok(self.writes)
    }
}

// ============================================================
//  Small writes
// ============================================================

fn put_u16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(data: &mut [u8], offset: usize, value: u64) {
    data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn align8(value: usize) -> usize {
    value.div_ceil(8) * 8
}

/// Encodes an ASCII string as little-endian UTF-16.
///
/// The Nova OS names are bounded to printable ASCII by
/// `format::valid_name`: the conversion is therefore exact, one byte
/// becoming one 16-bit word.
fn utf16(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

/// MFT reference: record number on 48 bits, sequence number on 16. Every
/// one of our records carries sequence 1, having never been reused.
fn mft_ref(record: u64) -> u64 {
    record | (1u64 << 48)
}

// ============================================================
//  End-of-sector substitution (fixups)
// ============================================================

/// Applies the end-of-sector substitution to a structure.
///
/// `usa_offset` names the sequence array, whose first word is the current
/// number and whose following ones collect the last two bytes of each
/// sector, replaced in place by that number.
fn apply_fixups(data: &mut [u8], usa_offset: usize) {
    let sectors = data.len() / SECTOR;
    let sequence: u16 = 1;
    put_u16(data, usa_offset, sequence);

    for index in 0..sectors {
        let tail = (index + 1) * SECTOR - 2;
        let saved = [data[tail], data[tail + 1]];
        data[usa_offset + 2 + index * 2] = saved[0];
        data[usa_offset + 3 + index * 2] = saved[1];
        data[tail..tail + 2].copy_from_slice(&sequence.to_le_bytes());
    }
}

// ============================================================
//  Cluster ranges
// ============================================================

/// A range of a non-resident attribute. `None` names a hole -- that is
/// how a sparse file is described.
type Run = (Option<u64>, u64);

/// Number of bytes needed to encode `value` in two's complement.
fn signed_width(value: i64) -> usize {
    let mut width = 1;
    while width < 8 {
        let limit = 1i64 << (width * 8 - 1);
        if value >= -limit && value < limit {
            break;
        }
        width += 1;
    }
    width
}

fn push_signed(out: &mut Vec<u8>, value: i64, width: usize) {
    for index in 0..width {
        out.push((value >> (index * 8)) as u8);
    }
}

/// Encodes a list of ranges in the form NTFS expects.
///
/// Each range starts with a size byte -- high nibble for the
/// displacement, low nibble for the length -- followed by the length then
/// by the **displacement relative to the previous LCN**. That differential
/// encoding is what makes a range list compact; a displacement of zero
/// size marks a hole.
fn mapping_pairs(runs: &[Run]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut previous: i64 = 0;

    for (lcn, length) in runs {
        let length_width = signed_width(*length as i64);
        match lcn {
            Some(start) => {
                let delta = *start as i64 - previous;
                let offset_width = signed_width(delta);
                out.push(((offset_width as u8) << 4) | length_width as u8);
                push_signed(&mut out, *length as i64, length_width);
                push_signed(&mut out, delta, offset_width);
                previous = *start as i64;
            }
            None => {
                out.push(length_width as u8);
                push_signed(&mut out, *length as i64, length_width);
            }
        }
    }
    out.push(0);
    out
}

// ============================================================
//  Attributes
// ============================================================

/// Builds a complete resident attribute.
fn resident_attribute(
    kind: u32,
    name: &str,
    instance: u16,
    indexed: bool,
    value: &[u8],
) -> Vec<u8> {
    let name_units = utf16(name);
    let name_offset = 24 + name_units.len();
    let value_offset = align8(name_offset);
    let length = align8(value_offset + value.len());

    let mut raw = alloc::vec![0u8; length];
    put_u32(&mut raw, 0x00, kind);
    put_u32(&mut raw, 0x04, length as u32);
    raw[0x08] = 0; // resident
    raw[0x09] = (name_units.len() / 2) as u8;
    // The offset of the name is filled in even with no name: that is
    // where the value would start if the attribute carried one, and some
    // drivers use it to bound the header.
    put_u16(&mut raw, 0x0A, 24);
    put_u16(&mut raw, 0x0C, 0);
    put_u16(&mut raw, 0x0E, instance);
    put_u32(&mut raw, 0x10, value.len() as u32);
    put_u16(&mut raw, 0x14, value_offset as u16);
    // The "indexed" bit tells the driver that this attribute serves as a
    // key in an index: that is the case of `$FILE_NAME`, and of it alone here.
    raw[0x16] = u8::from(indexed);

    raw[24..24 + name_units.len()].copy_from_slice(&name_units);
    raw[value_offset..value_offset + value.len()].copy_from_slice(value);
    raw
}

/// Builds a complete non-resident attribute.
///
/// `total` is only filled in for a sparse attribute: NTFS files the room
/// really occupied there, which then differs from the room allocated.
fn nonresident_attribute(
    kind: u32,
    name: &str,
    instance: u16,
    runs: &[Run],
    data_size: u64,
    allocated: u64,
    initialized: u64,
    sparse: Option<u64>,
) -> Vec<u8> {
    let name_units = utf16(name);
    // A sparse attribute carries an extra field -- the room really
    // occupied -- which shifts the name and the ranges.
    let header = if sparse.is_some() { 0x48 } else { 0x40 };
    let name_offset = header + name_units.len();
    let pairs = mapping_pairs(runs);
    let pairs_offset = align8(name_offset);
    let length = align8(pairs_offset + pairs.len());

    let clusters: u64 = runs.iter().map(|(_, count)| *count).sum();
    let highest_vcn = clusters.saturating_sub(1);

    let mut raw = alloc::vec![0u8; length];
    put_u32(&mut raw, 0x00, kind);
    put_u32(&mut raw, 0x04, length as u32);
    raw[0x08] = 1; // non-resident
    raw[0x09] = (name_units.len() / 2) as u8;
    put_u16(&mut raw, 0x0A, header as u16);
    put_u16(&mut raw, 0x0C, if sparse.is_some() { ATTR_FLAG_SPARSE } else { 0 });
    put_u16(&mut raw, 0x0E, instance);
    put_u64(&mut raw, 0x10, 0); // first VCN described
    put_u64(&mut raw, 0x18, highest_vcn);
    put_u16(&mut raw, 0x20, pairs_offset as u16);
    raw[0x22] = 0; // compression unit: none
    put_u64(&mut raw, 0x28, allocated);
    put_u64(&mut raw, 0x30, data_size);
    put_u64(&mut raw, 0x38, initialized);
    if let Some(occupied) = sparse {
        put_u64(&mut raw, 0x40, occupied);
    }

    raw[header..header + name_units.len()].copy_from_slice(&name_units);
    raw[pairs_offset..pairs_offset + pairs.len()].copy_from_slice(&pairs);
    raw
}

/// Value of a `$STANDARD_INFORMATION`.
fn standard_information(attributes: u32) -> Vec<u8> {
    let mut raw = alloc::vec![0u8; 48];
    put_u64(&mut raw, 0x00, FIXED_TIME);
    put_u64(&mut raw, 0x08, FIXED_TIME);
    put_u64(&mut raw, 0x10, FIXED_TIME);
    put_u64(&mut raw, 0x18, FIXED_TIME);
    put_u32(&mut raw, 0x20, attributes);
    raw
}

/// Value of a `$FILE_NAME`, which also serves as a key in the indexes.
fn file_name_value(
    parent: u64,
    name: &str,
    attributes: u32,
    data_size: u64,
    allocated: u64,
) -> Vec<u8> {
    let name_units = utf16(name);
    let mut raw = alloc::vec![0u8; 66 + name_units.len()];
    put_u64(&mut raw, 0x00, mft_ref(parent));
    put_u64(&mut raw, 0x08, FIXED_TIME);
    put_u64(&mut raw, 0x10, FIXED_TIME);
    put_u64(&mut raw, 0x18, FIXED_TIME);
    put_u64(&mut raw, 0x20, FIXED_TIME);
    put_u64(&mut raw, 0x28, allocated);
    put_u64(&mut raw, 0x30, data_size);
    put_u32(&mut raw, 0x38, attributes);
    put_u32(&mut raw, 0x3C, 0);
    raw[0x40] = (name_units.len() / 2) as u8;
    // Win32 name space: the Nova OS names have no associated 8.3 short
    // form, and the POSIX space would make the volume case sensitive for
    // a host that does not expect it.
    raw[0x41] = 1;
    raw[0x42..].copy_from_slice(&name_units);
    raw
}

/// Single security descriptor of the volume.
///
/// One and the same descriptor for everything exported: full access
/// granted to all, owner "Administrators", group "System". Translating
/// the thirteen permissions of 5 would demand Windows account
/// identifiers that do not exist here; announcing a restriction one
/// cannot express would be worse than announcing none.
fn security_descriptor() -> Vec<u8> {
    // S-1-5-32-544, local group "Administrators".
    let mut administrators = alloc::vec![0u8; 16];
    administrators[0] = 1; // revision
    administrators[1] = 2; // two sub-authorities
    administrators[7] = 5; // NT authority
    put_u32(&mut administrators, 8, 32);
    put_u32(&mut administrators, 12, 544);

    // S-1-5-18, "System" account.
    let mut system = alloc::vec![0u8; 12];
    system[0] = 1;
    system[1] = 1;
    system[7] = 5;
    put_u32(&mut system, 8, 18);

    // S-1-1-0, "Everyone".
    let mut everyone = alloc::vec![0u8; 12];
    everyone[0] = 1;
    everyone[1] = 1;
    everyone[7] = 1;
    put_u32(&mut everyone, 8, 0);

    // A single control entry: full access, inheritable by files as by
    // directories.
    let ace_length = 8 + everyone.len();
    let mut ace = alloc::vec![0u8; ace_length];
    ace[0] = 0x00; // ACCESS_ALLOWED
    ace[1] = 0x03; // file and directory inheritance
    put_u16(&mut ace, 2, ace_length as u16);
    put_u32(&mut ace, 4, 0x001F_01FF); // FILE_ALL_ACCESS
    ace[8..].copy_from_slice(&everyone);

    let dacl_length = 8 + ace.len();
    let mut dacl = alloc::vec![0u8; dacl_length];
    dacl[0] = 2; // revision
    put_u16(&mut dacl, 2, dacl_length as u16);
    put_u16(&mut dacl, 4, 1); // one entry
    dacl[8..].copy_from_slice(&ace);

    let dacl_offset = 20;
    let owner_offset = dacl_offset + dacl.len();
    let group_offset = owner_offset + administrators.len();
    let total = group_offset + system.len();

    let mut raw = alloc::vec![0u8; total];
    raw[0] = 1; // revision
    // Self-relative: the fields are offsets, not pointers.
    put_u16(&mut raw, 2, 0x8004);
    put_u32(&mut raw, 4, owner_offset as u32);
    put_u32(&mut raw, 8, group_offset as u32);
    put_u32(&mut raw, 12, 0); // no SACL
    put_u32(&mut raw, 16, dacl_offset as u32);
    raw[dacl_offset..owner_offset].copy_from_slice(&dacl);
    raw[owner_offset..group_offset].copy_from_slice(&administrators);
    raw[group_offset..].copy_from_slice(&system);
    raw
}

// ============================================================
//  MFT records
// ============================================================

/// Assembles the attributes of a record then lays it out.
struct RecordBuilder {
    number: u64,
    flags: u16,
    attributes: Vec<u8>,
    instance: u16,
}

impl RecordBuilder {
    fn new(number: u64, directory: bool) -> Self {
        RecordBuilder {
            number,
            flags: RECORD_IN_USE | if directory { RECORD_IS_DIRECTORY } else { 0 },
            attributes: Vec::new(),
            instance: 0,
        }
    }

    fn next_instance(&mut self) -> u16 {
        let instance = self.instance;
        self.instance += 1;
        instance
    }

    fn push(&mut self, attribute: Vec<u8>) {
        self.attributes.extend_from_slice(&attribute);
    }

    /// Room still available for an attribute.
    fn remaining(&self) -> usize {
        RECORD
            .saturating_sub(ATTRS_OFFSET)
            .saturating_sub(self.attributes.len())
            .saturating_sub(8)
    }

    fn finish(self) -> Result<Vec<u8>, FsError> {
        let used = ATTRS_OFFSET + self.attributes.len() + 8;
        if used > RECORD {
            // Overflowing would demand an `$ATTRIBUTE_LIST` and an
            // extension record. Refusing is preferable to producing a
            // truncated record, which the driver would read as corrupt.
            return Err(FsError::NoSpace);
        }

        let mut raw = alloc::vec![0u8; RECORD];
        raw[0..4].copy_from_slice(b"FILE");
        put_u16(&mut raw, 0x04, RECORD_USA_OFFSET as u16);
        put_u16(&mut raw, 0x06, (RECORD / SECTOR + 1) as u16);
        put_u64(&mut raw, 0x08, 0); // journal sequence number
        put_u16(&mut raw, 0x10, 1); // record sequence
        put_u16(&mut raw, 0x12, 1); // a single file name
        put_u16(&mut raw, 0x14, ATTRS_OFFSET as u16);
        put_u16(&mut raw, 0x16, self.flags);
        put_u32(&mut raw, 0x18, used as u32);
        put_u32(&mut raw, 0x1C, RECORD as u32);
        put_u64(&mut raw, 0x20, 0); // base record: itself
        put_u16(&mut raw, 0x28, self.instance);
        put_u32(&mut raw, 0x2C, self.number as u32);

        raw[ATTRS_OFFSET..ATTRS_OFFSET + self.attributes.len()]
            .copy_from_slice(&self.attributes);
        put_u32(&mut raw, ATTRS_OFFSET + self.attributes.len(), ATTR_END);

        apply_fixups(&mut raw, RECORD_USA_OFFSET);
        Ok(raw)
    }
}

/// Unused record, but structurally valid.
///
/// The whole table is initialised this way before being filled: a record
/// left at zero has no `FILE` signature and would be read as a defect by
/// a tool sweeping the table.
fn empty_record(number: u64) -> Vec<u8> {
    let mut raw = alloc::vec![0u8; RECORD];
    raw[0..4].copy_from_slice(b"FILE");
    put_u16(&mut raw, 0x04, RECORD_USA_OFFSET as u16);
    put_u16(&mut raw, 0x06, (RECORD / SECTOR + 1) as u16);
    put_u16(&mut raw, 0x10, 1);
    put_u16(&mut raw, 0x12, 0);
    put_u16(&mut raw, 0x14, ATTRS_OFFSET as u16);
    put_u16(&mut raw, 0x16, 0); // free
    put_u32(&mut raw, 0x18, (ATTRS_OFFSET + 8) as u32);
    put_u32(&mut raw, 0x1C, RECORD as u32);
    put_u32(&mut raw, 0x2C, number as u32);
    put_u32(&mut raw, ATTRS_OFFSET, ATTR_END);
    apply_fixups(&mut raw, RECORD_USA_OFFSET);
    raw
}

// ============================================================
//  Directory index
// ============================================================

/// One index entry, before serialisation.
struct LevelEntry {
    /// `$FILE_NAME` key.
    key: Vec<u8>,
    /// Reference of the file described.
    reference: u64,
    /// Child node, for an entry of an internal node.
    child: Option<u64>,
}

impl LevelEntry {
    fn size(&self) -> usize {
        align8(16 + self.key.len()) + if self.child.is_some() { 8 } else { 0 }
    }

    fn encode(&self) -> Vec<u8> {
        let base = align8(16 + self.key.len());
        let total = self.size();
        let mut raw = alloc::vec![0u8; total];
        put_u64(&mut raw, 0x00, self.reference);
        put_u16(&mut raw, 0x08, total as u16);
        put_u16(&mut raw, 0x0A, self.key.len() as u16);
        put_u16(&mut raw, 0x0C, if self.child.is_some() { INDEX_ENTRY_NODE } else { 0 });
        raw[0x10..0x10 + self.key.len()].copy_from_slice(&self.key);
        if let Some(vcn) = self.child {
            put_u64(&mut raw, base, vcn);
        }
        raw
    }
}

/// Terminal entry of a node: it names nothing and closes the list.
fn end_entry(child: Option<u64>) -> Vec<u8> {
    let total = 16 + if child.is_some() { 8 } else { 0 };
    let mut raw = alloc::vec![0u8; total];
    put_u16(&mut raw, 0x08, total as u16);
    put_u16(&mut raw, 0x0A, 0);
    put_u16(
        &mut raw,
        0x0C,
        INDEX_ENTRY_END | if child.is_some() { INDEX_ENTRY_NODE } else { 0 },
    );
    if let Some(vcn) = child {
        put_u64(&mut raw, 16, vcn);
    }
    raw
}

/// One level of the tree: sorted entries, and the child receiving
/// everything greater than the last of them.
struct Level {
    entries: Vec<LevelEntry>,
    last_child: Option<u64>,
}

/// Result of building an index.
struct BuiltIndex {
    /// Value of the `$INDEX_ROOT` attribute.
    root: Vec<u8>,
    /// Content of each index block, in VCN order.
    blocks: Vec<Vec<u8>>,
}

/// Serialises a node into a 4 KiB index block.
fn encode_index_block(vcn: u64, entries: &[LevelEntry], last_child: Option<u64>) -> Vec<u8> {
    let mut body = Vec::new();
    for entry in entries {
        body.extend_from_slice(&entry.encode());
    }
    body.extend_from_slice(&end_entry(last_child));

    let mut raw = alloc::vec![0u8; INDEX_BLOCK];
    raw[0..4].copy_from_slice(b"INDX");
    put_u16(&mut raw, 0x04, INDEX_USA_OFFSET as u16);
    put_u16(&mut raw, 0x06, (INDEX_BLOCK / SECTOR + 1) as u16);
    put_u64(&mut raw, 0x08, 0); // journal sequence number
    put_u64(&mut raw, 0x10, vcn);

    // Index header: the offsets in it are counted from 0x18.
    put_u32(&mut raw, 0x18, (INDEX_ENTRIES_OFFSET - 0x18) as u32);
    put_u32(&mut raw, 0x1C, (INDEX_ENTRIES_OFFSET - 0x18 + body.len()) as u32);
    put_u32(&mut raw, 0x20, (INDEX_BLOCK - 0x18) as u32);
    raw[0x24] = u8::from(last_child.is_some());

    raw[INDEX_ENTRIES_OFFSET..INDEX_ENTRIES_OFFSET + body.len()].copy_from_slice(&body);
    apply_fixups(&mut raw, INDEX_USA_OFFSET);
    raw
}

/// Serialises the root of an index into the value of an `$INDEX_ROOT`.
fn encode_index_root(level: &Level, large: bool) -> Vec<u8> {
    let mut body = Vec::new();
    for entry in level.entries.iter() {
        body.extend_from_slice(&entry.encode());
    }
    body.extend_from_slice(&end_entry(level.last_child));

    let mut raw = alloc::vec![0u8; 32 + body.len()];
    put_u32(&mut raw, 0x00, ATTR_FILE_NAME); // type of the indexed keys
    put_u32(&mut raw, 0x04, 1); // sort by file name
    put_u32(&mut raw, 0x08, INDEX_BLOCK as u32);
    raw[0x0C] = (INDEX_BLOCK / CLUSTER) as u8;

    put_u32(&mut raw, 0x10, 16);
    put_u32(&mut raw, 0x14, (16 + body.len()) as u32);
    put_u32(&mut raw, 0x18, (16 + body.len()) as u32);
    raw[0x1C] = u8::from(large);

    raw[32..].copy_from_slice(&body);
    raw
}

/// Builds the index tree of a directory.
///
/// The entries arrive sorted. They are spread over 4 KiB nodes; every
/// time a node overflows, the entry causing the overflow is **promoted**
/// to the level above, where it separates the node just closed from the
/// next. The operation repeats on the level thus produced until it fits
/// in the room left by the MFT record -- the depth of the tree therefore
/// adjusts itself to the number and the length of the names.
fn build_index(entries: Vec<LevelEntry>, root_capacity: usize) -> Result<BuiltIndex, FsError> {
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    let mut level = Level {
        entries,
        last_child: None,
    };

    loop {
        // Does the current level fit in the root? If so, the tree is
        // finished: the root carries it as-is, and the nodes already
        // produced form the allocation stream.
        let root_size: usize = 32
            + level.entries.iter().map(|entry| entry.size()).sum::<usize>()
            + if level.last_child.is_some() { 24 } else { 16 };
        if root_size <= root_capacity {
            return Ok(BuiltIndex {
                root: encode_index_root(&level, level.last_child.is_some()),
                blocks,
            });
        }
        if level.entries.is_empty() {
            // An empty level does not fit in the root: the root is too
            // small for the terminal entry alone, which assumes an absurd
            // directory name. Nothing more to split.
            return Err(FsError::NoSpace);
        }

        // Splitting of the level into 4 KiB nodes.
        let capacity = INDEX_BLOCK - INDEX_ENTRIES_OFFSET;
        let mut nodes: Vec<(Vec<LevelEntry>, Option<u64>)> = Vec::new();
        let mut promoted: Vec<LevelEntry> = Vec::new();
        let mut current: Vec<LevelEntry> = Vec::new();
        let mut used = 0usize;

        for entry in level.entries.into_iter() {
            // 24 bytes are kept for the terminal entry, which may carry a
            // child VCN.
            if !current.is_empty() && used + entry.size() + 24 > capacity {
                nodes.push((core::mem::take(&mut current), entry.child));
                promoted.push(LevelEntry {
                    key: entry.key,
                    reference: entry.reference,
                    child: None,
                });
                used = 0;
                continue;
            }
            used += entry.size();
            current.push(entry);
        }
        nodes.push((current, level.last_child));

        // Emission of the nodes and building of the parent level.
        let mut parent: Vec<LevelEntry> = Vec::new();
        let mut last_child = None;
        for (index, (node_entries, node_last)) in nodes.into_iter().enumerate() {
            let vcn = blocks.len() as u64;
            blocks.push(encode_index_block(vcn, &node_entries, node_last));

            match promoted.get_mut(index) {
                Some(separator) => parent.push(LevelEntry {
                    key: core::mem::take(&mut separator.key),
                    reference: separator.reference,
                    child: Some(vcn),
                }),
                None => last_child = Some(vcn),
            }
        }

        level = Level {
            entries: parent,
            last_child,
        };
    }
}

/// Empty index root, with no allocation block.
///
/// It serves the only directory with nothing to hold, `$Extend`: giving
/// it a 4 KiB block to file a terminal entry would be waste, and the
/// format accepts a standalone root perfectly well.
fn small_index_root() -> Vec<u8> {
    let level = Level {
        entries: Vec::new(),
        last_child: None,
    };
    encode_index_root(&level, false)
}

// ============================================================
//  Name sorting
// ============================================================

/// Sort key of a name, conforming to the file-name collation.
///
/// NTFS sorts on the uppercase form of the names, the one the `$UpCase`
/// table written on the volume gives. Comparing otherwise would produce
/// an index the driver would walk wrongly: its binary search assumes the
/// order it knows, not the one we chose.
fn collation_key(name: &str) -> Vec<u16> {
    name.encode_utf16().map(upcase_unit).collect()
}

/// Uppercase of a 16-bit word, identical to what `$UpCase` carries.
fn upcase_unit(unit: u16) -> u16 {
    match unit {
        0x0061..=0x007A => unit - 0x20,                    // basic latin
        0x00E0..=0x00F6 | 0x00F8..=0x00FE => unit - 0x20,  // latin-1
        0x00FF => 0x0178,
        // Latin extended A: even/odd pairs, except two ranges where the
        // shift is inverted.
        0x0100..=0x0137 | 0x014A..=0x0177 => unit & !1,
        0x0139..=0x0148 | 0x0179..=0x017E => (unit - 1) | 1,
        0x03B1..=0x03C1 | 0x03C3..=0x03CB => unit - 0x20,  // greek
        0x0430..=0x044F => unit - 0x20,                    // cyrillic
        _ => unit,
    }
}

/// Complete `$UpCase` table: 65 536 entries of 16 bits.
fn upcase_table() -> Vec<u8> {
    let mut raw = alloc::vec![0u8; 0x10000 * 2];
    for unit in 0..0x10000u32 {
        let upper = upcase_unit(unit as u16);
        raw[unit as usize * 2..unit as usize * 2 + 2].copy_from_slice(&upper.to_le_bytes());
    }
    raw
}

// ============================================================
//  Attribute definition table
// ============================================================

/// Content of `$AttrDef`: sixteen entries of 160 bytes.
///
/// The driver refers to it to know whether an attribute may be
/// non-resident, whether it is indexable, and between which bounds its
/// size must fall. A volume whose table is missing or lying is refused.
fn attribute_definitions() -> Vec<u8> {
    // (name, type, flags, minimum size, maximum size)
    // Flags: 0x02 indexable, 0x40 always resident.
    const ENTRIES: [(&str, u32, u32, u64, u64); 16] = [
        ("$STANDARD_INFORMATION", 0x10, 0x40, 48, 72),
        ("$ATTRIBUTE_LIST", 0x20, 0x00, 0, 0),
        ("$FILE_NAME", 0x30, 0x42, 68, 578),
        ("$OBJECT_ID", 0x40, 0x40, 0, 256),
        ("$SECURITY_DESCRIPTOR", 0x50, 0x00, 0, 0),
        ("$VOLUME_NAME", 0x60, 0x40, 2, 256),
        ("$VOLUME_INFORMATION", 0x70, 0x40, 12, 12),
        ("$DATA", 0x80, 0x00, 0, 0),
        ("$INDEX_ROOT", 0x90, 0x40, 0, 0),
        ("$INDEX_ALLOCATION", 0xA0, 0x00, 0, 0),
        ("$BITMAP", 0xB0, 0x00, 0, 0),
        ("$REPARSE_POINT", 0xC0, 0x00, 0, 16384),
        ("$EA_INFORMATION", 0xD0, 0x40, 8, 8),
        ("$EA", 0xE0, 0x00, 0, 65536),
        ("$PROPERTY_SET", 0xF0, 0x00, 0, 0),
        ("$LOGGED_UTILITY_STREAM", 0x100, 0x00, 0, 65536),
    ];

    let mut raw = alloc::vec![0u8; ENTRIES.len() * 160];
    for (index, (name, kind, flags, minimum, maximum)) in ENTRIES.iter().enumerate() {
        let base = index * 160;
        let units = utf16(name);
        raw[base..base + units.len()].copy_from_slice(&units);
        put_u32(&mut raw, base + 0x80, *kind);
        put_u32(&mut raw, base + 0x84, 0); // display rule
        put_u32(&mut raw, base + 0x88, 0); // sort rule
        put_u32(&mut raw, base + 0x8C, *flags);
        put_u64(&mut raw, base + 0x90, *minimum);
        put_u64(&mut raw, base + 0x98, *maximum);
    }
    raw
}

// ============================================================
//  Boot sector
// ============================================================

struct Layout {
    total_clusters: u64,
    volume_sectors: u64,
    mft_lcn: u64,
    mft_clusters: u64,
    mftmirr_lcn: u64,
    logfile_lcn: u64,
    logfile_clusters: u64,
    upcase_lcn: u64,
    attrdef_lcn: u64,
    bitmap_lcn: u64,
    bitmap_clusters: u64,
    mft_bitmap_lcn: u64,
    mft_bitmap_clusters: u64,
    serial: u64,
}

fn boot_sector(layout: &Layout) -> [u8; SECTOR] {
    let mut raw = [0u8; SECTOR];

    // Jump to absent boot code: byte 0 must be 0xEB for the volume to be
    // recognised, even if the image is not bootable.
    raw[0] = 0xEB;
    raw[1] = 0x52;
    raw[2] = 0x90;
    raw[3..11].copy_from_slice(b"NTFS    ");

    put_u16(&mut raw, 0x0B, SECTOR as u16);
    raw[0x0D] = SECTORS_PER_CLUSTER as u8;
    put_u16(&mut raw, 0x0E, 0); // reserved sectors: none on NTFS
    raw[0x10] = 0;
    put_u16(&mut raw, 0x11, 0);
    put_u16(&mut raw, 0x13, 0);
    raw[0x15] = 0xF8; // fixed medium
    put_u16(&mut raw, 0x16, 0);
    put_u16(&mut raw, 0x18, 63); // sectors per track
    put_u16(&mut raw, 0x1A, 255); // heads
    put_u32(&mut raw, 0x1C, 0); // hidden sectors
    put_u32(&mut raw, 0x20, 0);
    raw[0x24] = 0x80; // drive number
    raw[0x26] = 0x80; // extended signature

    put_u64(&mut raw, 0x28, layout.volume_sectors);
    put_u64(&mut raw, 0x30, layout.mft_lcn);
    put_u64(&mut raw, 0x38, layout.mftmirr_lcn);

    // Size of an MFT record. It is expressed in clusters when it is worth
    // at least one, and as a negative base-2 logarithm when it is smaller
    // -- 1 KiB for a 4 KiB cluster gives -10.
    raw[0x40] = (-10i8) as u8;
    raw[0x44] = (INDEX_BLOCK / CLUSTER) as u8;
    put_u64(&mut raw, 0x48, layout.serial);
    put_u32(&mut raw, 0x50, 0); // checksum: unused

    raw[0x1FE] = 0x55;
    raw[0x1FF] = 0xAA;
    raw
}

// ============================================================
//  Export
// ============================================================

/// A directory left to walk.
struct Pending {
    path: String,
    record: u64,
    parent: u64,
}

/// An entry already named in the directory being built.
struct Child {
    name: String,
    record: u64,
    attributes: u32,
    data_size: u64,
    allocated: u64,
}

/// Counts the entries of the tree to size the master table.
///
/// The size of the MFT must be known before the first write, since it
/// occupies a fixed range of the volume. This preliminary walk reads no
/// file content: it touches only the metadata, already in the cache after
/// the first pass.
fn count_entries(source: &str, who: Credentials) -> u64 {
    let mut total = 0u64;
    let mut seen: Vec<NodeId> = Vec::new();
    let mut queue = alloc::vec![String::from(source)];

    while let Some(path) = queue.pop() {
        let Ok(links) = super::super::list(&path, who) else {
            continue;
        };
        for link in links.iter() {
            let child_path = join_path(&path, &link.name);
            let Ok(node) = super::super::stat(&child_path, who) else {
                continue;
            };
            total += 1;
            if node.kind == NodeKind::Directory {
                if seen.contains(&node.id) {
                    continue;
                }
                seen.push(node.id);
                queue.push(child_path);
            }
        }
    }
    total
}

/// Rewrites a Nova OS tree as NTFS on a blank medium.
pub fn export(
    source: &str,
    target: &mut dyn BlockDevice,
    who: Credentials,
) -> Result<ExportReport, FsError> {
    if target.read_only() {
        return Err(FsError::Block(super::super::block::BlockError::ReadOnly));
    }

    let blocks = target.block_count();
    // Eight mebibytes: below that, the one-mebibyte journal and the
    // uppercasing table would occupy most of the volume.
    if blocks < 2048 {
        return Err(FsError::NoSpace);
    }

    // The last sector of the medium does not belong to the volume: it
    // receives the backup copy of the boot sector, which the drivers go
    // looking for there when the first is unreadable.
    let volume_sectors = blocks * SECTORS_PER_CLUSTER - 1;
    let total_clusters = volume_sectors / SECTORS_PER_CLUSTER;

    // --- sizing ---
    let entries = count_entries(source, who);
    let records = (MFT_FIRST_USER + entries + 16).max(64);
    let mft_clusters = records.div_ceil(RECORDS_PER_CLUSTER as u64);

    // Journal: one mebibyte is enough for an exchange medium, and it is
    // the floor `mkntfs` applies to small volumes.
    let logfile_clusters = (total_clusters / 100).clamp(256, 2048);
    let upcase_clusters = (0x10000 * 2 / CLUSTER) as u64;
    let bitmap_bytes = total_clusters.div_ceil(8);
    let bitmap_clusters = bitmap_bytes.div_ceil(CLUSTER as u64);
    let mft_bitmap_clusters = records.div_ceil(8).div_ceil(CLUSTER as u64);

    let mftmirr_lcn = 2u64;
    let logfile_lcn = mftmirr_lcn + 1;
    let mft_lcn = logfile_lcn + logfile_clusters;
    let upcase_lcn = mft_lcn + mft_clusters;
    let attrdef_lcn = upcase_lcn + upcase_clusters;
    let bitmap_lcn = attrdef_lcn + 1;
    let mft_bitmap_lcn = bitmap_lcn + bitmap_clusters;
    let first_free = mft_bitmap_lcn + mft_bitmap_clusters;

    if first_free + 8 >= total_clusters {
        return Err(FsError::NoSpace);
    }

    // Serial number of the volume, derived from the geometry: with no
    // source of randomness at boot, a constant number would collide
    // between two media exported on the same day.
    let mut serial: u64 = 0xcbf2_9ce4_8422_2325;
    for value in [total_clusters, mft_clusters, entries, FIXED_TIME] {
        serial = (serial ^ value).wrapping_mul(0x1000_0000_01b3);
    }

    let layout = Layout {
        total_clusters,
        volume_sectors,
        mft_lcn,
        mft_clusters,
        mftmirr_lcn,
        logfile_lcn,
        logfile_clusters,
        upcase_lcn,
        attrdef_lcn,
        bitmap_lcn,
        bitmap_clusters,
        mft_bitmap_lcn,
        mft_bitmap_clusters,
        serial,
    };

    let mut report = ExportReport::new(ExportFormat::Ntfs);
    let mut writer = ClusterWriter::new(target);

    // --- cluster occupancy ---
    let mut bitmap = alloc::vec![0u8; bitmap_bytes as usize];
    let mark = |bitmap: &mut Vec<u8>, first: u64, count: u64| {
        for cluster in first..first + count {
            if cluster < total_clusters {
                bitmap[(cluster / 8) as usize] |= 1 << (cluster % 8);
            }
        }
    };
    mark(&mut bitmap, 0, first_free);

    let mut next_cluster = first_free;
    let mut next_record = MFT_FIRST_USER;

    // --- fixed regions ---
    // The empty journal is a run of 0xFF: it is what `mkntfs` writes, and
    // it is the form the drivers recognise as "nothing to replay". A
    // journal of zeros would instead be read as a corrupt restart page.
    writer.fill_range(logfile_lcn, logfile_clusters, 0xFF)?;

    {
        // 128 KiB on a heap that counts 4 096: the table is written then
        // released at once, before the walk asks for room for the file
        // content.
        let upcase = upcase_table();
        for index in 0..upcase_clusters {
            let start = index as usize * CLUSTER;
            writer.write_cluster(upcase_lcn + index, &upcase[start..start + CLUSTER])?;
        }
    }
    writer.write_cluster(attrdef_lcn, &attribute_definitions())?;

    // The whole master table is initialised before being filled.
    for index in 0..records {
        let cluster = mft_lcn + index / RECORDS_PER_CLUSTER as u64;
        let offset = (index % RECORDS_PER_CLUSTER as u64) as usize * RECORD;
        writer.write_at(cluster, offset, &empty_record(index))?;
    }

    // --- walk of the tree ---
    let root_node = super::super::stat(source, who)?;
    let mut seen: Vec<NodeId> = alloc::vec![root_node.id];
    let mut queue: Vec<Pending> = alloc::vec![Pending {
        path: String::from(source),
        record: MFT_ROOT,
        parent: MFT_ROOT,
    }];

    let security = security_descriptor();

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

        let mut children: Vec<Child> = Vec::new();

        // The root directory of the volume also carries the system files:
        // they are hidden, but they are there, and their absence is an
        // anomaly for a checker.
        if pending.record == MFT_ROOT {
            for (name, record) in SYSTEM_FILES {
                children.push(Child {
                    name: String::from(*name),
                    record: *record,
                    attributes: FILE_ATTR_HIDDEN
                        | FILE_ATTR_SYSTEM
                        | if *record == MFT_EXTEND {
                            FILE_ATTR_DIRECTORY
                        } else {
                            0
                        },
                    data_size: 0,
                    allocated: 0,
                });
            }
        }

        let mut sorted = links;
        sorted.sort_by(|a, b| a.name.cmp(&b.name));

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

            if next_record >= records {
                return Err(FsError::NoSpace);
            }

            match node.kind {
                NodeKind::Directory => {
                    if seen.contains(&node.id) {
                        report.skipped.push(alloc::format!(
                            "{} (dossier deja exporte - un lien de dossier ne se transporte pas)",
                            child_path
                        ));
                        continue;
                    }
                    seen.push(node.id);

                    let record = next_record;
                    next_record += 1;
                    children.push(Child {
                        name: link.name.clone(),
                        record,
                        attributes: FILE_ATTR_DIRECTORY,
                        data_size: 0,
                        allocated: 0,
                    });
                    queue.push(Pending {
                        path: child_path,
                        record,
                        parent: pending.record,
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

                    let record = next_record;
                    next_record += 1;

                    let mut attributes = FILE_ATTR_ARCHIVE;
                    // The only right NTFS keeps with no per-file security
                    // descriptor: the prohibition on rewriting.
                    if !node
                        .perm_owner
                        .contains(super::super::perm::Permissions::OVERWRITE)
                    {
                        attributes |= FILE_ATTR_READONLY;
                    }

                    // The residence of the content is decided before
                    // writing anything at all: `$FILE_NAME` carries the
                    // allocated size, which is zero for resident content
                    // and a whole number of clusters otherwise. Computing
                    // it afterwards would make the index entry lie.
                    let clusters = (data.len() as u64).div_ceil(CLUSTER as u64);
                    let name_length = 66 + link.name.len() * 2;
                    let head = align8(24 + 48)
                        + align8(24 + name_length)
                        + align8(24 + security.len());
                    let free = RECORD - ATTRS_OFFSET - 8 - head;
                    let resident = align8(24 + data.len()) <= free;
                    let allocated = if resident { 0 } else { clusters * CLUSTER as u64 };

                    let mut builder = RecordBuilder::new(record, false);
                    let instance = builder.next_instance();
                    builder.push(resident_attribute(
                        ATTR_STANDARD_INFORMATION,
                        "",
                        instance,
                        false,
                        &standard_information(attributes),
                    ));

                    let instance = builder.next_instance();
                    builder.push(resident_attribute(
                        ATTR_FILE_NAME,
                        "",
                        instance,
                        true,
                        &file_name_value(
                            pending.record,
                            &link.name,
                            attributes,
                            data.len() as u64,
                            allocated,
                        ),
                    ));

                    let instance = builder.next_instance();
                    builder.push(resident_attribute(
                        ATTR_SECURITY_DESCRIPTOR,
                        "",
                        instance,
                        false,
                        &security,
                    ));

                    if resident {
                        // Short content fits in the record itself: it is
                        // the normal behaviour of NTFS, and it spares a
                        // whole cluster per configuration file.
                        let instance = builder.next_instance();
                        builder.push(resident_attribute(ATTR_DATA, "", instance, false, &data));
                    } else {
                        if next_cluster + clusters > total_clusters {
                            return Err(FsError::NoSpace);
                        }
                        let start = next_cluster;
                        next_cluster += clusters;
                        mark(&mut bitmap, start, clusters);

                        let mut written = 0usize;
                        for index in 0..clusters {
                            let end = (written + CLUSTER).min(data.len());
                            writer.write_cluster(start + index, &data[written..end])?;
                            written = end;
                        }

                        let instance = builder.next_instance();
                        builder.push(nonresident_attribute(
                            ATTR_DATA,
                            "",
                            instance,
                            &[(Some(start), clusters)],
                            data.len() as u64,
                            allocated,
                            data.len() as u64,
                            None,
                        ));
                    }

                    let raw = builder.finish()?;
                    write_record(&mut writer, &layout, record, &raw)?;

                    children.push(Child {
                        name: link.name.clone(),
                        record,
                        attributes,
                        data_size: data.len() as u64,
                        allocated,
                    });
                    report.files += 1;
                    report.bytes += data.len() as u64;
                }
                NodeKind::Free => {}
            }
        }

        // --- record of the directory ---
        let name = if pending.record == MFT_ROOT {
            String::from(".")
        } else {
            pending
                .path
                .rsplit('/')
                .next()
                .unwrap_or(".")
                .into()
        };

        let raw = build_directory_record(
            &mut writer,
            &layout,
            &mut bitmap,
            &mut next_cluster,
            &security,
            pending.record,
            pending.parent,
            &name,
            &children,
        )?;
        write_record(&mut writer, &layout, pending.record, &raw)?;
    }

    // --- system files ---
    write_system_records(&mut writer, &layout, &security, records)?;

    // --- occupancy bitmap ---
    // The bits beyond the last cluster must be 1: failing which a driver
    // would believe it could write beyond the volume.
    let extra = (bitmap.len() as u64 * 8) - total_clusters;
    if let Some(last) = bitmap.last_mut() {
        for bit in 0..extra {
            *last |= 1 << (7 - bit);
        }
    }
    for index in 0..bitmap_clusters {
        let start = index as usize * CLUSTER;
        let end = (start + CLUSTER).min(bitmap.len());
        writer.write_cluster(bitmap_lcn + index, &bitmap[start..end])?;
    }

    // Bitmap of the MFT records: the records used are those written, plus
    // the sixteen reserved.
    let mut mft_bitmap = alloc::vec![0u8; (mft_bitmap_clusters * CLUSTER as u64) as usize];
    for record in 0..next_record {
        mft_bitmap[(record / 8) as usize] |= 1 << (record % 8);
    }
    for index in 0..mft_bitmap_clusters {
        let start = index as usize * CLUSTER;
        writer.write_cluster(mft_bitmap_lcn + index, &mft_bitmap[start..start + CLUSTER])?;
    }

    // --- boot sector and its copy ---
    let boot = boot_sector(&layout);
    writer.write_at(0, 0, &boot)?;
    // The copy lives in the last sector of the medium, beyond the last
    // cluster of the volume.
    let backup_cluster = blocks - 1;
    let backup_offset = (volume_sectors % SECTORS_PER_CLUSTER) as usize * SECTOR;
    writer.write_at(backup_cluster, backup_offset, &boot)?;

    // $MFTMirr: copy of the first four records. They all live in the
    // first cluster of the table; reading them back in one block avoids
    // alternating between two clusters on every record.
    let mut mirror = alloc::vec![0u8; CLUSTER];
    read_cluster(&mut writer, mft_lcn, &mut mirror)?;
    writer.write_cluster(mftmirr_lcn, &mirror)?;

    let writes = writer.finish()?;
    crate::serial_println!(
        "[export] NTFS : {} clusters de {} Kio, MFT de {} enregistrements a partir du cluster {}, {} utilises, {} ecritures",
        total_clusters,
        CLUSTER / 1024,
        records,
        mft_lcn,
        next_record,
        writes
    );

    Ok(report)
}

/// Names of the system files to make appear in the root directory.
const SYSTEM_FILES: &[(&str, u64)] = &[
    ("$MFT", MFT_MFT),
    ("$MFTMirr", MFT_MFTMIRR),
    ("$LogFile", MFT_LOGFILE),
    ("$Volume", MFT_VOLUME),
    ("$AttrDef", MFT_ATTRDEF),
    ("$Bitmap", MFT_BITMAP),
    ("$Boot", MFT_BOOT),
    ("$BadClus", MFT_BADCLUS),
    ("$Secure", MFT_SECURE),
    ("$UpCase", MFT_UPCASE),
    ("$Extend", MFT_EXTEND),
];

/// Writes a record in its place in the master table.
fn write_record(
    writer: &mut ClusterWriter,
    layout: &Layout,
    record: u64,
    raw: &[u8],
) -> Result<(), FsError> {
    let cluster = layout.mft_lcn + record / RECORDS_PER_CLUSTER as u64;
    let offset = (record % RECORDS_PER_CLUSTER as u64) as usize * RECORD;
    writer.write_at(cluster, offset, raw)
}

/// Reads an already written cluster back, through the writer cache.
fn read_cluster(writer: &mut ClusterWriter, cluster: u64, out: &mut [u8]) -> Result<(), FsError> {
    writer.load(cluster)?;
    out.copy_from_slice(&writer.buffer);
    Ok(())
}

/// Builds the record of a directory, index included.
#[allow(clippy::too_many_arguments)]
fn build_directory_record(
    writer: &mut ClusterWriter,
    layout: &Layout,
    bitmap: &mut Vec<u8>,
    next_cluster: &mut u64,
    security: &[u8],
    record: u64,
    parent: u64,
    name: &str,
    children: &[Child],
) -> Result<Vec<u8>, FsError> {
    let attributes = FILE_ATTR_DIRECTORY
        | if record == MFT_ROOT || record == MFT_EXTEND {
            FILE_ATTR_HIDDEN | FILE_ATTR_SYSTEM
        } else {
            0
        };

    let mut builder = RecordBuilder::new(record, true);

    let instance = builder.next_instance();
    builder.push(resident_attribute(
        ATTR_STANDARD_INFORMATION,
        "",
        instance,
        false,
        &standard_information(attributes),
    ));

    let instance = builder.next_instance();
    builder.push(resident_attribute(
        ATTR_FILE_NAME,
        "",
        instance,
        true,
        &file_name_value(parent, name, attributes, 0, 0),
    ));

    let instance = builder.next_instance();
    builder.push(resident_attribute(
        ATTR_SECURITY_DESCRIPTOR,
        "",
        instance,
        false,
        security,
    ));

    // Room left for the root of the index, once what the allocation
    // stream and its bitmap will take is reserved.
    let root_capacity = builder
        .remaining()
        .saturating_sub(INDEX_ALLOC_RESERVE + INDEX_BITMAP_RESERVE + 32);

    // Sort by the NTFS collation key, with the raw name to separate two
    // names differing only in case -- Nova OS allows them, NTFS confuses
    // them.
    let mut sorted: Vec<&Child> = children.iter().collect();
    sorted.sort_by(|a, b| {
        collation_key(&a.name)
            .cmp(&collation_key(&b.name))
            .then_with(|| a.name.cmp(&b.name))
    });

    let entries: Vec<LevelEntry> = sorted
        .iter()
        .map(|child| LevelEntry {
            key: file_name_value(
                record,
                &child.name,
                child.attributes,
                child.data_size,
                child.allocated,
            ),
            reference: mft_ref(child.record),
            child: None,
        })
        .collect();

    let index = build_index(entries, root_capacity)?;

    let instance = builder.next_instance();
    builder.push(resident_attribute(
        ATTR_INDEX_ROOT,
        INDEX_NAME,
        instance,
        false,
        &index.root,
    ));

    if !index.blocks.is_empty() {
        let count = index.blocks.len() as u64;
        if *next_cluster + count > layout.total_clusters {
            return Err(FsError::NoSpace);
        }
        let start = *next_cluster;
        *next_cluster += count;
        for cluster in start..start + count {
            bitmap[(cluster / 8) as usize] |= 1 << (cluster % 8);
        }

        for (index_in_run, block) in index.blocks.iter().enumerate() {
            writer.write_cluster(start + index_in_run as u64, block)?;
        }

        let allocated = count * CLUSTER as u64;
        let instance = builder.next_instance();
        builder.push(nonresident_attribute(
            ATTR_INDEX_ALLOCATION,
            INDEX_NAME,
            instance,
            &[(Some(start), count)],
            allocated,
            allocated,
            allocated,
            None,
        ));

        // Bitmap of the index blocks in service. Always a multiple of
        // eight bytes: that is what the drivers expect, since they read
        // it in words.
        let mut used = alloc::vec![0u8; ((count as usize).div_ceil(64)) * 8];
        for block in 0..count {
            used[(block / 8) as usize] |= 1 << (block % 8);
        }
        let instance = builder.next_instance();
        builder.push(resident_attribute(ATTR_BITMAP, INDEX_NAME, instance, false, &used));
    }

    builder.finish()
}

/// Writes the records of the system files.
fn write_system_records(
    writer: &mut ClusterWriter,
    layout: &Layout,
    security: &[u8],
    records: u64,
) -> Result<(), FsError> {
    let hidden = FILE_ATTR_HIDDEN | FILE_ATTR_SYSTEM;

    // --- 0: $MFT ---
    let mut builder = RecordBuilder::new(MFT_MFT, false);
    push_system_head(&mut builder, "$MFT", hidden, security);
    let mft_bytes = layout.mft_clusters * CLUSTER as u64;
    let instance = builder.next_instance();
    builder.push(nonresident_attribute(
        ATTR_DATA,
        "",
        instance,
        &[(Some(layout.mft_lcn), layout.mft_clusters)],
        mft_bytes,
        mft_bytes,
        mft_bytes,
        None,
    ));
    let mft_bitmap_bytes = layout.mft_bitmap_clusters * CLUSTER as u64;
    let instance = builder.next_instance();
    builder.push(nonresident_attribute(
        ATTR_BITMAP,
        "",
        instance,
        &[(Some(layout.mft_bitmap_lcn), layout.mft_bitmap_clusters)],
        records.div_ceil(8),
        mft_bitmap_bytes,
        records.div_ceil(8),
        None,
    ));
    let raw = builder.finish()?;
    write_record(writer, layout, MFT_MFT, &raw)?;

    // --- 1: $MFTMirr ---
    let mut builder = RecordBuilder::new(MFT_MFTMIRR, false);
    push_system_head(&mut builder, "$MFTMirr", hidden, security);
    let instance = builder.next_instance();
    builder.push(nonresident_attribute(
        ATTR_DATA,
        "",
        instance,
        &[(Some(layout.mftmirr_lcn), 1)],
        CLUSTER as u64,
        CLUSTER as u64,
        CLUSTER as u64,
        None,
    ));
    let raw = builder.finish()?;
    write_record(writer, layout, MFT_MFTMIRR, &raw)?;

    // --- 2: $LogFile ---
    let mut builder = RecordBuilder::new(MFT_LOGFILE, false);
    push_system_head(&mut builder, "$LogFile", hidden, security);
    let log_bytes = layout.logfile_clusters * CLUSTER as u64;
    let instance = builder.next_instance();
    builder.push(nonresident_attribute(
        ATTR_DATA,
        "",
        instance,
        &[(Some(layout.logfile_lcn), layout.logfile_clusters)],
        log_bytes,
        log_bytes,
        log_bytes,
        None,
    ));
    let raw = builder.finish()?;
    write_record(writer, layout, MFT_LOGFILE, &raw)?;

    // --- 3: $Volume ---
    let mut builder = RecordBuilder::new(MFT_VOLUME, false);
    push_system_head(&mut builder, "$Volume", hidden, security);
    let instance = builder.next_instance();
    builder.push(resident_attribute(
        ATTR_VOLUME_NAME,
        "",
        instance,
        false,
        &utf16("nova-export"),
    ));
    let mut information = alloc::vec![0u8; 12];
    information[8] = 3; // major version
    information[9] = 1; // minor version
    let instance = builder.next_instance();
    builder.push(resident_attribute(
        ATTR_VOLUME_INFORMATION,
        "",
        instance,
        false,
        &information,
    ));
    let instance = builder.next_instance();
    builder.push(resident_attribute(ATTR_DATA, "", instance, false, &[]));
    let raw = builder.finish()?;
    write_record(writer, layout, MFT_VOLUME, &raw)?;

    // --- 4: $AttrDef ---
    let mut builder = RecordBuilder::new(MFT_ATTRDEF, false);
    push_system_head(&mut builder, "$AttrDef", hidden, security);
    let instance = builder.next_instance();
    builder.push(nonresident_attribute(
        ATTR_DATA,
        "",
        instance,
        &[(Some(layout.attrdef_lcn), 1)],
        (16 * 160) as u64,
        CLUSTER as u64,
        (16 * 160) as u64,
        None,
    ));
    let raw = builder.finish()?;
    write_record(writer, layout, MFT_ATTRDEF, &raw)?;

    // --- 6: $Bitmap ---
    let mut builder = RecordBuilder::new(MFT_BITMAP, false);
    push_system_head(&mut builder, "$Bitmap", hidden, security);
    let bitmap_bytes = layout.total_clusters.div_ceil(8);
    let instance = builder.next_instance();
    builder.push(nonresident_attribute(
        ATTR_DATA,
        "",
        instance,
        &[(Some(layout.bitmap_lcn), layout.bitmap_clusters)],
        bitmap_bytes,
        layout.bitmap_clusters * CLUSTER as u64,
        bitmap_bytes,
        None,
    ));
    let raw = builder.finish()?;
    write_record(writer, layout, MFT_BITMAP, &raw)?;

    // --- 7: $Boot ---
    let mut builder = RecordBuilder::new(MFT_BOOT, false);
    push_system_head(&mut builder, "$Boot", hidden, security);
    let instance = builder.next_instance();
    builder.push(nonresident_attribute(
        ATTR_DATA,
        "",
        instance,
        &[(Some(0), 2)],
        8192,
        2 * CLUSTER as u64,
        8192,
        None,
    ));
    let raw = builder.finish()?;
    write_record(writer, layout, MFT_BOOT, &raw)?;

    // --- 8: $BadClus ---
    // The stream named "$Bad" covers the whole volume without consuming a
    // cluster: it is an entirely sparse file, each written range of which
    // would name a faulty sector. There is none here.
    let mut builder = RecordBuilder::new(MFT_BADCLUS, false);
    push_system_head(&mut builder, "$BadClus", hidden, security);
    let instance = builder.next_instance();
    builder.push(resident_attribute(ATTR_DATA, "", instance, false, &[]));
    let volume_bytes = layout.total_clusters * CLUSTER as u64;
    let instance = builder.next_instance();
    builder.push(nonresident_attribute(
        ATTR_DATA,
        "$Bad",
        instance,
        &[(None, layout.total_clusters)],
        volume_bytes,
        volume_bytes,
        0,
        Some(0),
    ));
    let raw = builder.finish()?;
    write_record(writer, layout, MFT_BADCLUS, &raw)?;

    // --- 9: $Secure ---
    // The security descriptors being carried by each record, this file
    // has nothing to index. It exists because the format reserves a
    // number for it, not because it serves.
    let mut builder = RecordBuilder::new(MFT_SECURE, false);
    push_system_head(&mut builder, "$Secure", hidden, security);
    let instance = builder.next_instance();
    builder.push(resident_attribute(ATTR_DATA, "", instance, false, &[]));
    let raw = builder.finish()?;
    write_record(writer, layout, MFT_SECURE, &raw)?;

    // --- 10: $UpCase ---
    let mut builder = RecordBuilder::new(MFT_UPCASE, false);
    push_system_head(&mut builder, "$UpCase", hidden, security);
    let upcase_bytes = (0x10000 * 2) as u64;
    let instance = builder.next_instance();
    builder.push(nonresident_attribute(
        ATTR_DATA,
        "",
        instance,
        &[(Some(layout.upcase_lcn), upcase_bytes / CLUSTER as u64)],
        upcase_bytes,
        upcase_bytes,
        upcase_bytes,
        None,
    ));
    let raw = builder.finish()?;
    write_record(writer, layout, MFT_UPCASE, &raw)?;

    // --- 11: $Extend ---
    let mut builder = RecordBuilder::new(MFT_EXTEND, true);
    push_system_head(
        &mut builder,
        "$Extend",
        hidden | FILE_ATTR_DIRECTORY,
        security,
    );
    let instance = builder.next_instance();
    builder.push(resident_attribute(
        ATTR_INDEX_ROOT,
        INDEX_NAME,
        instance,
        false,
        &small_index_root(),
    ));
    let raw = builder.finish()?;
    write_record(writer, layout, MFT_EXTEND, &raw)?;

    Ok(())
}

/// Attributes common to every system record: standard information, name
/// attached to the root, security descriptor.
fn push_system_head(builder: &mut RecordBuilder, name: &str, attributes: u32, security: &[u8]) {
    let instance = builder.next_instance();
    builder.push(resident_attribute(
        ATTR_STANDARD_INFORMATION,
        "",
        instance,
        false,
        &standard_information(attributes),
    ));

    let instance = builder.next_instance();
    builder.push(resident_attribute(
        ATTR_FILE_NAME,
        "",
        instance,
        true,
        &file_name_value(MFT_ROOT, name, attributes, 0, 0),
    ));

    let instance = builder.next_instance();
    builder.push(resident_attribute(
        ATTR_SECURITY_DESCRIPTOR,
        "",
        instance,
        false,
        security,
    ));
}

// ============================================================
//  Read-back
// ============================================================

/// Reads the boot sector of a produced image back and checks its
/// coherence.
///
/// Checks what a driver checks before mounting: the end-of-sector
/// signature, the `NTFS` label, the geometry, and above all that the
/// first record of the master table really carries the `FILE` signature.
/// Returns the number of clusters and the LCN of the table.
pub fn verify(device: &mut dyn BlockDevice) -> Result<(u32, u32), FsError> {
    let mut block = alloc::vec![0u8; CLUSTER];
    device.read_block(0, &mut block)?;

    if block[0x1FE] != 0x55 || block[0x1FF] != 0xAA {
        return Err(FsError::Corrupt);
    }
    if &block[3..11] != b"NTFS    " {
        return Err(FsError::Corrupt);
    }

    let bytes_per_sector = u16::from_le_bytes([block[0x0B], block[0x0C]]) as u64;
    let sectors_per_cluster = block[0x0D] as u64;
    let sectors = u64::from_le_bytes(block[0x28..0x30].try_into().unwrap_or([0; 8]));
    let mft_lcn = u64::from_le_bytes(block[0x30..0x38].try_into().unwrap_or([0; 8]));
    let mirror_lcn = u64::from_le_bytes(block[0x38..0x40].try_into().unwrap_or([0; 8]));
    let record_shift = block[0x40] as i8;

    if bytes_per_sector != SECTOR as u64
        || sectors_per_cluster != SECTORS_PER_CLUSTER
        || sectors == 0
        || mft_lcn == 0
        || mirror_lcn == 0
        || record_shift != -10
    {
        return Err(FsError::Corrupt);
    }

    let clusters = sectors / sectors_per_cluster;
    if mft_lcn >= clusters {
        return Err(FsError::Corrupt);
    }

    // The first record of the table describes the table itself: if it
    // does not carry its signature, nothing that follows is readable.
    device.read_block(mft_lcn, &mut block)?;
    if &block[0..4] != b"FILE" {
        return Err(FsError::Corrupt);
    }
    // The root directory occupies record 5, which falls in the second
    // cluster of the table: four 1 KiB records per 4 KiB cluster.
    device.read_block(mft_lcn + MFT_ROOT / RECORDS_PER_CLUSTER as u64, &mut block)?;
    let root_offset = (MFT_ROOT % RECORDS_PER_CLUSTER as u64) as usize * RECORD;
    if &block[root_offset..root_offset + 4] != b"FILE" {
        return Err(FsError::Corrupt);
    }
    let flags = u16::from_le_bytes([block[root_offset + 0x16], block[root_offset + 0x17]]);
    if flags & (RECORD_IN_USE | RECORD_IS_DIRECTORY) != (RECORD_IN_USE | RECORD_IS_DIRECTORY) {
        return Err(FsError::Corrupt);
    }

    Ok((clusters.min(u32::MAX as u64) as u32, mft_lcn as u32))
}

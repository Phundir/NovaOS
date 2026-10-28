//! 4.3.1 -- Abstraction of the storage devices.
//!
//! The filesystem knows nothing but numbered **blocks**. What lies behind
//! -- an area of RAM, a SATA disk on an AHCI controller, later an NVMe or
//! a user-space driver -- is none of its business. It is that split that
//! will allow moving the drivers out of the kernel in phase 6.1 without
//! touching the filesystem.
//!
//! The logical block is 4 KiB, whatever the sector size of the medium.
//! Three reasons: it is the size of a page, which simplifies the cache;
//! it is a multiple of every sector met in practice (512 or 4096); and it
//! divides by eight the number of bitmap entries compared with a
//! 512-byte block.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

/// Size of a logical block of the filesystem.
pub const BLOCK_SIZE: usize = 4096;

/// Assumed sector size of the SATA media.
pub const SECTOR_SIZE: usize = 512;

/// Number of sectors per logical block.
pub const SECTORS_PER_BLOCK: usize = BLOCK_SIZE / SECTOR_SIZE;

/// Failure causes of a block operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    /// Block number beyond the capacity of the medium.
    OutOfRange,
    /// The medium refuses the write.
    ReadOnly,
    /// The hardware reported an error.
    DeviceError,
    /// The controller did not answer within the allotted time.
    Timeout,
    /// Buffer of a size different from one block.
    BadBufferSize,
}

impl BlockError {
    pub fn message(&self) -> &'static str {
        match self {
            BlockError::OutOfRange => "numero de bloc hors capacite",
            BlockError::ReadOnly => "support en lecture seule",
            BlockError::DeviceError => "erreur signalee par le materiel",
            BlockError::Timeout => "le controleur n'a pas repondu",
            BlockError::BadBufferSize => "tampon de taille incorrecte",
        }
    }
}

/// Nature of the medium, for display and for the copy rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediumKind {
    /// Disk in RAM -- disappears at power-off.
    Ram,
    /// SATA disk behind an AHCI controller.
    Sata,
    /// Removable medium.
    Removable,
}

impl MediumKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            MediumKind::Ram => "RAM",
            MediumKind::Sata => "SATA",
            MediumKind::Removable => "amovible",
        }
    }
}

/// A storage medium addressable in 4 KiB blocks.
///
/// The implementations must be safe to call from several cores: it is the
/// caller that serialises, through the cache lock.
pub trait BlockDevice: Send {
    /// Readable name, shown in the disk list.
    fn name(&self) -> String;

    fn kind(&self) -> MediumKind;

    /// Number of 4 KiB blocks addressable.
    fn block_count(&self) -> u64;

    fn read_only(&self) -> bool {
        false
    }

    /// Reads a whole block. `buffer` must be exactly `BLOCK_SIZE`.
    fn read_block(&mut self, index: u64, buffer: &mut [u8]) -> Result<(), BlockError>;

    /// Writes a whole block.
    fn write_block(&mut self, index: u64, buffer: &[u8]) -> Result<(), BlockError>;

    /// Forces the effective write to the medium. No effect on a RAM disk;
    /// flushes the write cache of the controller on SATA.
    fn flush(&mut self) -> Result<(), BlockError> {
        Ok(())
    }

    /// Capacity in bytes.
    fn capacity(&self) -> u64 {
        self.block_count() * BLOCK_SIZE as u64
    }

    /// Reads `frames.len()` runs of 4 KiB starting at 512-byte sector
    /// `sector`, one run into each frame (physical addresses).
    ///
    /// Sector granularity because a tar archive aligns a file's bytes on
    /// 512, not 4096: page `p` of a file is the eight sectors from
    /// `data / 512 + 8p`, which a block device of 4 KiB blocks would
    /// straddle. A controller that can scatter one command over several
    /// buffers (AHCI's PRDT) does it in one command; the default reads
    /// the blocks one by one and shifts.
    fn read_to_frames(&mut self, sector: u64, frames: &[u64]) -> Result<(), BlockError> {
        let per_block = (BLOCK_SIZE / 512) as u64;
        let mut block = alloc::vec![0u8; BLOCK_SIZE];
        for (i, phys) in frames.iter().enumerate() {
            let start = sector + i as u64 * per_block;
            let inside = ((start % per_block) * 512) as usize;
            let destination = crate::memory::phys_to_virt(*phys) as *mut u8;
            self.read_block(start / per_block, &mut block)?;
            let first = BLOCK_SIZE - inside;
            // SAFETY: the caller hands frames it owns, 4 KiB each.
            unsafe { core::ptr::copy_nonoverlapping(block[inside..].as_ptr(), destination, first) };
            if inside != 0 {
                self.read_block(start / per_block + 1, &mut block)?;
                unsafe { core::ptr::copy_nonoverlapping(block.as_ptr(), destination.add(first), inside) };
            }
        }
        Ok(())
    }
}

// ============================================================
//  Disk in memory
// ============================================================

/// Fallback medium, entirely in RAM.
///
/// It exists for two reasons. First because no AHCI controller is
/// guaranteed: without it, the filesystem could not start at all on a
/// machine with no disk. Second because it makes the rest of phase 4
/// testable independently of the driver -- a node table bug is not
/// confused with a DMA bug.
pub struct RamDisk {
    name: String,
    blocks: Vec<u8>,
    count: u64,
}

impl RamDisk {
    /// Allocates a disk of `blocks` blocks, entirely zeroed.
    pub fn new(name: &str, blocks: u64) -> Self {
        RamDisk {
            name: String::from(name),
            blocks: alloc::vec![0u8; blocks as usize * BLOCK_SIZE],
            count: blocks,
        }
    }
}

impl BlockDevice for RamDisk {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn kind(&self) -> MediumKind {
        MediumKind::Ram
    }

    fn block_count(&self) -> u64 {
        self.count
    }

    fn read_block(&mut self, index: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
        if buffer.len() != BLOCK_SIZE {
            return Err(BlockError::BadBufferSize);
        }
        if index >= self.count {
            return Err(BlockError::OutOfRange);
        }
        let start = index as usize * BLOCK_SIZE;
        buffer.copy_from_slice(&self.blocks[start..start + BLOCK_SIZE]);
        Ok(())
    }

    fn write_block(&mut self, index: u64, buffer: &[u8]) -> Result<(), BlockError> {
        if buffer.len() != BLOCK_SIZE {
            return Err(BlockError::BadBufferSize);
        }
        if index >= self.count {
            return Err(BlockError::OutOfRange);
        }
        let start = index as usize * BLOCK_SIZE;
        self.blocks[start..start + BLOCK_SIZE].copy_from_slice(buffer);
        Ok(())
    }
}

// ============================================================
//  Disk backed by physical frames
// ============================================================

/// Simulated medium, backed directly by physical frames.
///
/// `RamDisk` takes its place in the kernel heap, which is 4 MiB: so it
/// cannot exceed that size. But a FAT32 volume must count at least
/// 65 525 clusters not to be taken for FAT16, that is several tens of
/// mebibytes. That is what justifies this second medium: it draws on the
/// free frames -- several hundred mebibytes -- with no need for a
/// contiguous virtual range, since each block is reached through the
/// physical window.
///
/// It stands in for a removable medium for the export of 4.4, until a
/// real USB device is plugged in in phase 6.1.
pub struct FrameDisk {
    name: String,
    frames: Vec<crate::frame::Frame>,
}

impl FrameDisk {
    /// Reserves `blocks` frames. Returns `None` if there are not enough --
    /// and gives back those already taken, so as not to deprive the system
    /// of an unusable partial reservation.
    pub fn new(name: &str, blocks: u64) -> Option<Self> {
        let mut frames = Vec::with_capacity(blocks as usize);
        for _ in 0..blocks {
            match crate::frame::allocate_zeroed_frame() {
                Some(frame) => frames.push(frame),
                None => {
                    for frame in frames.iter() {
                        unsafe { crate::frame::deallocate_frame(*frame) };
                    }
                    return None;
                }
            }
        }
        Some(FrameDisk {
            name: String::from(name),
            frames,
        })
    }

    /// Gives every frame back to the allocator.
    pub fn release(self) {
        for frame in self.frames.iter() {
            unsafe { crate::frame::deallocate_frame(*frame) };
        }
    }
}

impl BlockDevice for FrameDisk {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn kind(&self) -> MediumKind {
        MediumKind::Removable
    }

    fn block_count(&self) -> u64 {
        self.frames.len() as u64
    }

    fn read_block(&mut self, index: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
        if buffer.len() != BLOCK_SIZE {
            return Err(BlockError::BadBufferSize);
        }
        let frame = *self.frames.get(index as usize).ok_or(BlockError::OutOfRange)?;
        unsafe {
            core::ptr::copy_nonoverlapping(
                frame.virt_addr() as *const u8,
                buffer.as_mut_ptr(),
                BLOCK_SIZE,
            )
        };
        Ok(())
    }

    fn write_block(&mut self, index: u64, buffer: &[u8]) -> Result<(), BlockError> {
        if buffer.len() != BLOCK_SIZE {
            return Err(BlockError::BadBufferSize);
        }
        let frame = *self.frames.get(index as usize).ok_or(BlockError::OutOfRange)?;
        unsafe {
            core::ptr::copy_nonoverlapping(
                buffer.as_ptr(),
                frame.virt_addr() as *mut u8,
                BLOCK_SIZE,
            )
        };
        Ok(())
    }
}

/// Medium detected at boot, before a letter is assigned.
pub struct DetectedDevice {
    pub device: Box<dyn BlockDevice>,
    /// Does the medium already carry a Nova OS filesystem?
    pub formatted: bool,
}

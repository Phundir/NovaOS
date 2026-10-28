//! 4.3.3 -- Block cache, write-behind.
//!
//! A disk access costs thousands of times more than a memory access, and
//! the filesystem reads the same blocks over and over: superblock,
//! bitmaps, node table. With no cache, creating a file would demand ten
//! or so round trips to the disk.
//!
//! ## Write-behind rather than write-through
//!
//! A write marks the block *dirty* and stops there. The block only
//! reaches the medium on eviction or on an explicit `flush`. That is what
//! allows writing a hundred times into the same bitmap for a single real
//! write.
//!
//! The price is known: a power cut between the modification and the
//! `flush` loses the pending writes. A journal would make them
//! replayable; that is not the case yet -- see the limits of the roadmap.
//! So every operation modifying the structure of the volume ends with a
//! `flush`, which reduces the window to the duration of one operation.
//!
//! ## Clock replacement
//!
//! No LRU: keeping an exact order would demand a linked list and two
//! pointer writes on every read. The clock algorithm gets a very close
//! result with one bit per slot -- we sweep round, clear the reference
//! bits met, and take the first unreferenced slot.

use alloc::boxed::Box;
use alloc::vec::Vec;

use super::block::{BLOCK_SIZE, BlockDevice, BlockError};

/// Number of blocks kept in memory: 64 x 4 KiB = 256 KiB per volume.
/// Blocks held in memory: 512 KiB.
///
/// Raising it to 512 slots was tried and measured: worse, because every
/// `fsync` -- a database commits constantly -- writes back every dirty
/// block it holds. The cost of writing was not the size of the cache but
/// the path walked on every call; the Linux side now addresses a file by
/// its node number once it has resolved it (`fs::write_range_node`).
pub const CACHE_SLOTS: usize = 128;

struct Slot {
    /// Resident block, `None` if the slot has never been used.
    index: Option<u64>,
    data: Box<[u8]>,
    dirty: bool,
    /// Reference bit of the clock algorithm.
    referenced: bool,
}

impl Slot {
    fn new() -> Self {
        Slot {
            index: None,
            data: alloc::vec![0u8; BLOCK_SIZE].into_boxed_slice(),
            dirty: false,
            referenced: false,
        }
    }
}

/// Cache of a volume, placed in front of its block device.
pub struct BlockCache {
    device: Box<dyn BlockDevice>,
    slots: Vec<Slot>,
    /// Hand of the clock.
    hand: usize,

    // --- statistics ---
    hits: u64,
    misses: u64,
    writebacks: u64,
    reads: u64,
    writes: u64,
}

impl BlockCache {
    pub fn new(device: Box<dyn BlockDevice>) -> Self {
        let mut slots = Vec::with_capacity(CACHE_SLOTS);
        for _ in 0..CACHE_SLOTS {
            slots.push(Slot::new());
        }
        BlockCache {
            device,
            slots,
            hand: 0,
            hits: 0,
            misses: 0,
            writebacks: 0,
            reads: 0,
            writes: 0,
        }
    }

    pub fn device_name(&self) -> alloc::string::String {
        self.device.name()
    }

    /// Returns the device, abandoning the cache.
    ///
    /// The modified blocks are written first: taking a medium out of the
    /// cache without flushing it would lose what it held.
    pub fn into_device(mut self) -> Box<dyn BlockDevice> {
        let _ = self.flush();
        self.device
    }

    pub fn block_count(&self) -> u64 {
        self.device.block_count()
    }

    pub fn medium(&self) -> super::block::MediumKind {
        self.device.kind()
    }

    pub fn read_only(&self) -> bool {
        self.device.read_only()
    }

    fn find(&self, index: u64) -> Option<usize> {
        self.slots.iter().position(|s| s.index == Some(index))
    }

    /// Chooses a slot to reuse and flushes it if it was dirty.
    fn evict(&mut self) -> Result<usize, BlockError> {
        // A pristine slot is always preferable to an eviction.
        if let Some(free) = self.slots.iter().position(|s| s.index.is_none()) {
            return Ok(free);
        }

        // Clock sweep. Two laps at most are enough: the first clears
        // every reference bit, so the second necessarily finds a slot
        // that can be freed.
        for _ in 0..(CACHE_SLOTS * 2) {
            let candidate = self.hand;
            self.hand = (self.hand + 1) % CACHE_SLOTS;

            if self.slots[candidate].referenced {
                self.slots[candidate].referenced = false;
                continue;
            }

            if self.slots[candidate].dirty {
                let index = self.slots[candidate].index.unwrap();
                self.device.write_block(index, &self.slots[candidate].data)?;
                self.slots[candidate].dirty = false;
                self.writebacks += 1;
            }
            return Ok(candidate);
        }

        // Unreachable: the first lap clears every bit.
        Err(BlockError::DeviceError)
    }

    /// Loads a block into the cache and returns its slot.
    fn fetch(&mut self, index: u64) -> Result<usize, BlockError> {
        if let Some(slot) = self.find(index) {
            self.slots[slot].referenced = true;
            self.hits += 1;
            return Ok(slot);
        }

        self.misses += 1;
        let slot = self.evict()?;

        // The buffer is taken out for the duration of the read:
        // `read_block` borrows the device mutably, it cannot borrow the
        // slot at the same time.
        let mut buffer = core::mem::replace(
            &mut self.slots[slot].data,
            alloc::vec![0u8; 0].into_boxed_slice(),
        );
        let result = self.device.read_block(index, &mut buffer);
        self.slots[slot].data = buffer;
        result?;

        self.slots[slot].index = Some(index);
        self.slots[slot].dirty = false;
        self.slots[slot].referenced = true;
        self.reads += 1;
        Ok(slot)
    }

    /// Reads a block into the buffer supplied.
    pub fn read(&mut self, index: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
        if buffer.len() != BLOCK_SIZE {
            return Err(BlockError::BadBufferSize);
        }
        let slot = self.fetch(index)?;
        buffer.copy_from_slice(&self.slots[slot].data);
        Ok(())
    }

    /// Reads `frames.len()` consecutive blocks **straight into frames**,
    /// one 4 KiB block per frame.
    ///
    /// For the pages of a big file: the browser's library weighs 150 MiB,
    /// and going through a cache of 64 slots, block by block, would be
    /// one disk command per 4 KiB. The controller fills the frames itself
    /// (`BlockDevice::read_to_frames`, one command for a whole run).
    /// Blocks held dirty in the cache are copied over afterwards: the
    /// disk does not have them yet.
    ///
    /// # Safety
    /// The frames must be owned by the caller and not mapped writable
    /// anywhere else while the controller writes into them.
    pub unsafe fn read_into_frames(&mut self, first: u64, frames: &[u64]) -> Result<(), BlockError> {
        let sectors_per_block = (BLOCK_SIZE / 512) as u64;
        self.device.read_to_frames(first * sectors_per_block, frames)?;
        self.reads += frames.len() as u64;
        for (offset, phys) in frames.iter().enumerate() {
            let index = first + offset as u64;
            if let Some(slot) = self.find(index)
                && self.slots[slot].dirty
            {
                let destination = crate::memory::phys_to_virt(*phys) as *mut u8;
                // SAFETY: a frame of the caller's, 4 KiB, as promised.
                unsafe { core::ptr::copy_nonoverlapping(self.slots[slot].data.as_ptr(), destination, BLOCK_SIZE) };
            }
        }
        Ok(())
    }

    /// Writes a block -- in memory only, until the next `flush`.
    pub fn write(&mut self, index: u64, buffer: &[u8]) -> Result<(), BlockError> {
        if buffer.len() != BLOCK_SIZE {
            return Err(BlockError::BadBufferSize);
        }
        if self.device.read_only() {
            return Err(BlockError::ReadOnly);
        }

        // A whole-block write does not need to read the old content: we
        // avoid the round trip if the block is not already there.
        let slot = match self.find(index) {
            Some(slot) => {
                self.hits += 1;
                slot
            }
            None => {
                self.misses += 1;
                let slot = self.evict()?;
                self.slots[slot].index = Some(index);
                slot
            }
        };

        self.slots[slot].data.copy_from_slice(buffer);
        self.slots[slot].dirty = true;
        self.slots[slot].referenced = true;
        self.writes += 1;
        Ok(())
    }

    /// Modifies a block in place, without copying it twice.
    ///
    /// The "read, touch three bytes, write back" pattern is the commonest
    /// of the filesystem: updating a bitmap or a node never touches a
    /// whole block.
    pub fn modify<F, R>(&mut self, index: u64, action: F) -> Result<R, BlockError>
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        if self.device.read_only() {
            return Err(BlockError::ReadOnly);
        }
        let slot = self.fetch(index)?;
        let result = action(&mut self.slots[slot].data);
        self.slots[slot].dirty = true;
        Ok(result)
    }

    /// Consults a block without copying it.
    pub fn inspect<F, R>(&mut self, index: u64, action: F) -> Result<R, BlockError>
    where
        F: FnOnce(&[u8]) -> R,
    {
        let slot = self.fetch(index)?;
        Ok(action(&self.slots[slot].data))
    }

    /// Writes every modified block to the medium.
    pub fn flush(&mut self) -> Result<(), BlockError> {
        for slot in self.slots.iter_mut() {
            if !slot.dirty {
                continue;
            }
            let Some(index) = slot.index else {
                continue;
            };
            self.device.write_block(index, &slot.data)?;
            slot.dirty = false;
            self.writebacks += 1;
        }
        self.device.flush()
    }

    /// Number of modified blocks still in memory.
    pub fn dirty_count(&self) -> usize {
        self.slots.iter().filter(|s| s.dirty).count()
    }

    /// (hits, misses, disk reads, disk writes).
    pub fn stats(&self) -> (u64, u64, u64, u64) {
        (self.hits, self.misses, self.reads, self.writebacks)
    }

    pub fn dump_state(&self) {
        let total = self.hits + self.misses;
        let ratio = if total == 0 { 0 } else { self.hits * 100 / total };
        crate::serial_println!(
            "[cache] {} creneaux - {} succes / {} echecs ({} %) - {} lectures, {} ecritures disque",
            CACHE_SLOTS,
            self.hits,
            self.misses,
            ratio,
            self.reads,
            self.writebacks
        );
        crate::serial_println!(
            "[cache] {} blocs modifies en attente - {} ecritures logiques",
            self.dirty_count(),
            self.writes
        );
    }
}

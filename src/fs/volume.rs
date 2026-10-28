//! Heart of the filesystem: a mounted volume.
//!
//! Gathers the superblock, the two bitmaps, the node table and the data
//! area behind a cache. Every operation of spec 4 and 5 goes through here.
//!
//! ## Node and name are separate (spec 4.1)
//!
//! A directory is not an entity: it is a node whose content is a table of
//! `name -> node identifier` links. It follows directly that:
//!
//! * **moving** = remove an entry from one table, add one to another.
//!   The data does not move, whatever the size of the file (4.1.3);
//! * **linking** = add a second entry to the same node. The node carries
//!   a link counter and only disappears at the last removal;
//! * **copying** = allocate a new node and copy the blocks. It is the
//!   only operation that really moves bytes, and the only case where it
//!   is mandatory is going from one volume to another (4.1.4).

use alloc::string::String;
use alloc::vec::Vec;

use super::block::{BLOCK_SIZE, BlockError};
use super::cache::BlockCache;
use super::format::{
    DOUBLE_LEVEL_BLOCKS,
    DIRECT_BLOCKS, INDIRECT_ENTRIES, LINK_SIZE, LINKS_PER_BLOCK, Link, MAX_FILE_SIZE, NODE_SIZE,
    Node, NodeId, NodeKind, ROOT_NODE, SuperBlock, get_u64, put_u64, valid_name,
};
use super::perm::{Credentials, Gid, Mode, Permissions, Role, Uid};
use super::{FsError, audit};

/// A mounted Nova OS volume.
pub struct Volume {
    pub superblock: SuperBlock,
    cache: BlockCache,
    /// Letter assigned (`/` for the boot disk).
    pub letter: char,
    /// Where the last allocation stopped, block and node.
    ///
    /// The search used to start again at the beginning of the volume on
    /// every allocation, bit by bit: with a hundred thousand blocks
    /// taken, writing a 33 MiB file spent ten seconds walking the bitmap.
    /// The cursor is not written to the disk -- a fresh mount starts at
    /// the beginning, and the wrap-around finds whatever it skipped.
    next_block: u64,
    next_node: NodeId,
}

impl Volume {
    // ============================================================
    //  4.3.2 -- Formatting and mounting
    // ============================================================

    /// Writes a fresh volume on a medium.
    ///
    /// Formatting is *destructive* and asks nothing: it is up to the
    /// caller to make sure it is not destroying an existing volume. Since
    /// phase 14 the kernel formats no disk of its own accord (14.0.1): the
    /// only caller is the self-test, on a volume in memory.
    pub fn format(mut cache: BlockCache, label: &str, now: u64) -> Result<Self, FsError> {
        let total = cache.block_count();
        let mut superblock = SuperBlock::layout(total, label)?;
        superblock.created = now;

        // --- bitmaps zeroed ---
        let empty = alloc::vec![0u8; BLOCK_SIZE];
        for offset in 0..superblock.node_bitmap_blocks {
            cache.write(superblock.node_bitmap_start + offset, &empty)?;
        }
        for offset in 0..superblock.block_bitmap_blocks {
            cache.write(superblock.block_bitmap_start + offset, &empty)?;
        }
        for offset in 0..superblock.node_table_blocks {
            cache.write(superblock.node_table_start + offset, &empty)?;
        }

        let mut volume = Volume {
            superblock,
            cache,
            letter: '/',
            next_block: 0,
            next_node: 1,
        };

        // The metadata occupies the start of the volume: its blocks are
        // marked taken before any allocation, failing which the allocator
        // would hand them out as data blocks.
        for block in 0..volume.superblock.data_start {
            volume.mark_block(block, true)?;
        }
        volume.superblock.free_blocks = volume.superblock.total_blocks - volume.superblock.data_start;

        // --- root node ---
        // It is allocated first and therefore receives identifier 1, a
        // value the superblock records so as not to depend on that order.
        let root = volume.allocate_node()?;
        debug_assert_eq!(root, ROOT_NODE);

        let mut node = Node::new(
            root,
            NodeKind::Directory,
            super::perm::ROOT_UID,
            super::perm::SYSTEM_GID,
            Mode::system_dir(),
            now,
        );
        // The root references itself: it has no parent that could carry
        // its link, and a null counter would make it be freed.
        node.links = 1;
        volume.write_node(&node)?;

        volume.superblock.root_node = root;
        volume.write_superblock()?;
        volume.cache.flush()?;

        Ok(volume)
    }

    /// Mounts an existing volume.
    pub fn mount(mut cache: BlockCache) -> Result<Self, FsError> {
        let mut block = alloc::vec![0u8; BLOCK_SIZE];
        cache.read(0, &mut block)?;
        let superblock = SuperBlock::decode(&block)?;

        if superblock.total_blocks > cache.block_count() {
            return Err(FsError::Format(
                super::format::FormatError::InconsistentLayout,
            ));
        }

        Ok(Volume {
            superblock,
            cache,
            letter: '/',
            next_block: 0,
            next_node: 1,
        })
    }

    fn write_superblock(&mut self) -> Result<(), FsError> {
        let mut block = alloc::vec![0u8; BLOCK_SIZE];
        self.superblock.encode(&mut block);
        self.cache.write(0, &block)?;
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), FsError> {
        self.write_superblock()?;
        self.cache.flush()?;
        Ok(())
    }

    pub fn cache(&mut self) -> &mut BlockCache {
        &mut self.cache
    }

    pub fn name(&self) -> String {
        self.cache.device_name()
    }

    pub fn medium(&self) -> super::block::MediumKind {
        self.cache.medium()
    }

    // ============================================================
    //  Bitmaps
    // ============================================================

    fn mark_block(&mut self, block: u64, used: bool) -> Result<(), FsError> {
        if block >= self.superblock.total_blocks {
            return Err(FsError::Block(BlockError::OutOfRange));
        }
        let bit = block as usize;
        let index = self.superblock.block_bitmap_start + (bit / (8 * BLOCK_SIZE)) as u64;
        let offset = (bit / 8) % BLOCK_SIZE;
        let mask = 1u8 << (bit % 8);

        self.cache.modify(index, |data| {
            if used {
                data[offset] |= mask;
            } else {
                data[offset] &= !mask;
            }
        })?;
        Ok(())
    }

    fn block_used(&mut self, block: u64) -> Result<bool, FsError> {
        let bit = block as usize;
        let index = self.superblock.block_bitmap_start + (bit / (8 * BLOCK_SIZE)) as u64;
        let offset = (bit / 8) % BLOCK_SIZE;
        let mask = 1u8 << (bit % 8);
        Ok(self.cache.inspect(index, |data| data[offset] & mask != 0)?)
    }

    /// First free block from `from`, looking at the bitmap a byte at a
    /// time: a byte of 0xFF is eight taken blocks skipped at once.
    /// First free block from `from`, looking at the bitmap a byte at a
    /// time: a byte of 0xFF is eight taken blocks skipped at once.
    fn scan_free_block(&mut self, from: u64, to: u64) -> Result<Option<u64>, FsError> {
        let per_bitmap_block = (8 * BLOCK_SIZE) as u64;
        let mut block = from;
        while block < to {
            let index = self.superblock.block_bitmap_start + block / per_bitmap_block;
            // First block described by this block of the bitmap.
            let base = (block / per_bitmap_block) * per_bitmap_block;
            let start_byte = ((block - base) / 8) as usize;
            let start_bit = ((block - base) % 8) as u32;
            let found = self.cache.inspect(index, |data| {
                for offset in start_byte..BLOCK_SIZE {
                    // In the first byte the bits before the start read as
                    // taken: without that mask the search finds the same
                    // bit again and never moves.
                    let mut byte = data[offset];
                    if offset == start_byte {
                        byte |= ((1u16 << start_bit) - 1) as u8;
                    }
                    if byte != 0xFF {
                        return Some(base + (offset * 8) as u64 + byte.trailing_ones() as u64);
                    }
                }
                None
            })?;
            match found {
                Some(candidate) if candidate < to => return Ok(Some(candidate)),
                Some(_) => return Ok(None),
                // Nothing free here: on to the next block of the bitmap.
                None => block = base + per_bitmap_block,
            }
        }
        Ok(None)
    }

    /// Allocates a data block, zeroed.
    ///
    /// The zeroing is not a courtesy: without it, a fresh file would
    /// expose the content of a deleted one.
    fn allocate_block(&mut self) -> Result<u64, FsError> {
        let start = self.next_block.max(self.superblock.data_start);
        let total = self.superblock.total_blocks;
        let found = match self.scan_free_block(start, total)? {
            Some(block) => Some(block),
            // Wrap around: what was freed behind the cursor.
            None => self.scan_free_block(self.superblock.data_start, start)?,
        };
        if let Some(block) = found {
            self.next_block = block + 1;
            self.mark_block(block, true)?;
            self.superblock.free_blocks = self.superblock.free_blocks.saturating_sub(1);

            let empty = alloc::vec![0u8; BLOCK_SIZE];
            self.cache.write(block, &empty)?;
            return Ok(block);
        }
        Err(FsError::NoSpace)
    }

    fn free_block(&mut self, block: u64) -> Result<(), FsError> {
        if block < self.superblock.data_start {
            return Ok(());
        }
        if self.block_used(block)? {
            self.mark_block(block, false)?;
            self.superblock.free_blocks += 1;
        }
        Ok(())
    }

    fn mark_node(&mut self, id: NodeId, used: bool) -> Result<(), FsError> {
        if id == 0 || id > self.superblock.node_count {
            return Err(FsError::NotFound);
        }
        let bit = (id - 1) as usize;
        let index = self.superblock.node_bitmap_start + (bit / (8 * BLOCK_SIZE)) as u64;
        let offset = (bit / 8) % BLOCK_SIZE;
        let mask = 1u8 << (bit % 8);

        self.cache.modify(index, |data| {
            if used {
                data[offset] |= mask;
            } else {
                data[offset] &= !mask;
            }
        })?;
        Ok(())
    }

    fn node_used(&mut self, id: NodeId) -> Result<bool, FsError> {
        let bit = (id - 1) as usize;
        let index = self.superblock.node_bitmap_start + (bit / (8 * BLOCK_SIZE)) as u64;
        let offset = (bit / 8) % BLOCK_SIZE;
        let mask = 1u8 << (bit % 8);
        Ok(self.cache.inspect(index, |data| data[offset] & mask != 0)?)
    }

    fn allocate_node(&mut self) -> Result<NodeId, FsError> {
        let start = self.next_node.max(1);
        for id in (start..=self.superblock.node_count).chain(1..start) {
            if self.node_used(id)? {
                continue;
            }
            self.next_node = id + 1;
            self.mark_node(id, true)?;
            self.superblock.free_nodes = self.superblock.free_nodes.saturating_sub(1);
            return Ok(id);
        }
        Err(FsError::NoNodes)
    }

    // ============================================================
    //  Node table
    // ============================================================

    pub fn read_node(&mut self, id: NodeId) -> Result<Node, FsError> {
        let (block, offset) = self
            .superblock
            .node_location(id)
            .ok_or(FsError::NotFound)?;
        let node = self
            .cache
            .inspect(block, |data| Node::decode(&data[offset..offset + NODE_SIZE]))?;

        if node.kind == NodeKind::Free {
            return Err(FsError::NotFound);
        }
        Ok(node)
    }

    fn write_node(&mut self, node: &Node) -> Result<(), FsError> {
        let (block, offset) = self
            .superblock
            .node_location(node.id)
            .ok_or(FsError::NotFound)?;
        self.cache.modify(block, |data| {
            node.encode(&mut data[offset..offset + NODE_SIZE]);
        })?;
        Ok(())
    }

    /// Frees a node and all of its blocks.
    fn release_node(&mut self, node: &Node) -> Result<(), FsError> {
        for block in node.direct.iter() {
            if *block != 0 {
                self.free_block(*block)?;
            }
        }
        if node.indirect != 0 {
            let mut table = alloc::vec![0u8; BLOCK_SIZE];
            self.cache.read(node.indirect, &mut table)?;
            for entry in 0..INDIRECT_ENTRIES {
                let block = get_u64(&table, entry * 8);
                if block == 0 {
                    continue;
                }
                if node.indirect_levels >= 2 {
                    // A table of tables: its entries are tables to empty.
                    let mut leaves = alloc::vec![0u8; BLOCK_SIZE];
                    self.cache.read(block, &mut leaves)?;
                    for leaf in 0..INDIRECT_ENTRIES {
                        let data_block = get_u64(&leaves, leaf * 8);
                        if data_block != 0 {
                            self.free_block(data_block)?;
                        }
                    }
                }
                self.free_block(block)?;
            }
            self.free_block(node.indirect)?;
        }

        let empty = Node::empty(node.id);
        self.write_node(&empty)?;
        self.mark_node(node.id, false)?;
        self.superblock.free_nodes += 1;
        Ok(())
    }

    // ============================================================
    //  Blocks of a node
    // ============================================================

    /// Physical block carrying the `n`-th logical block of a node.
    ///
    /// Beyond the five direct blocks, through the indirection table --
    /// one level for a file of at most `SINGLE_LEVEL_BLOCKS`, two beyond
    /// (the table then holds tables).
    fn map_block(&mut self, node: &Node, logical: u64) -> Result<Option<u64>, FsError> {
        if logical < DIRECT_BLOCKS as u64 {
            let block = node.direct[logical as usize];
            return Ok(if block == 0 { None } else { Some(block) });
        }
        if node.indirect == 0 {
            return Ok(None);
        }

        let entry = (logical - DIRECT_BLOCKS as u64) as usize;
        if node.indirect_levels < 2 {
            if entry >= INDIRECT_ENTRIES {
                return Ok(None);
            }
            let block = self.cache.inspect(node.indirect, |data| get_u64(data, entry * 8))?;
            return Ok(if block == 0 { None } else { Some(block) });
        }

        let (first, second) = (entry / INDIRECT_ENTRIES, entry % INDIRECT_ENTRIES);
        if first >= INDIRECT_ENTRIES {
            return Ok(None);
        }
        let table = self.cache.inspect(node.indirect, |data| get_u64(data, first * 8))?;
        if table == 0 {
            return Ok(None);
        }
        let block = self.cache.inspect(table, |data| get_u64(data, second * 8))?;
        Ok(if block == 0 { None } else { Some(block) })
    }

    /// The same, allocating what is missing.
    fn ensure_block(&mut self, node: &mut Node, logical: u64) -> Result<u64, FsError> {
        if logical >= DOUBLE_LEVEL_BLOCKS {
            return Err(FsError::TooLarge);
        }

        if logical < DIRECT_BLOCKS as u64 {
            let slot = logical as usize;
            if node.direct[slot] == 0 {
                node.direct[slot] = self.allocate_block()?;
            }
            return Ok(node.direct[slot]);
        }

        let entry = (logical - DIRECT_BLOCKS as u64) as usize;
        // Past the first level, the table becomes a table of tables: the
        // one in place moves into its first slot, and nothing that was
        // written has to move.
        if entry >= INDIRECT_ENTRIES && node.indirect_levels < 2 {
            let top = self.allocate_zeroed_block()?;
            let previous = node.indirect;
            if previous != 0 {
                self.cache.modify(top, |data| put_u64(data, 0, previous))?;
            }
            node.indirect = top;
            node.indirect_levels = 2;
        }
        if node.indirect == 0 {
            node.indirect = self.allocate_zeroed_block()?;
        }

        if node.indirect_levels < 2 {
            let existing = self.cache.inspect(node.indirect, |data| get_u64(data, entry * 8))?;
            if existing != 0 {
                return Ok(existing);
            }
            let block = self.allocate_block()?;
            let indirect = node.indirect;
            self.cache.modify(indirect, |data| put_u64(data, entry * 8, block))?;
            return Ok(block);
        }

        let (first, second) = (entry / INDIRECT_ENTRIES, entry % INDIRECT_ENTRIES);
        let mut table = self.cache.inspect(node.indirect, |data| get_u64(data, first * 8))?;
        if table == 0 {
            table = self.allocate_zeroed_block()?;
            let top = node.indirect;
            self.cache.modify(top, |data| put_u64(data, first * 8, table))?;
        }
        let existing = self.cache.inspect(table, |data| get_u64(data, second * 8))?;
        if existing != 0 {
            return Ok(existing);
        }
        let block = self.allocate_block()?;
        self.cache.modify(table, |data| put_u64(data, second * 8, block))?;
        Ok(block)
    }

    /// A block allocated and emptied: an indirection table must not start
    /// with whatever the previous holder of the block left in it.
    fn allocate_zeroed_block(&mut self) -> Result<u64, FsError> {
        let block = self.allocate_block()?;
        self.cache.modify(block, |data| data.fill(0))?;
        Ok(block)
    }

    /// Physical blocks carrying `count` logical blocks from `first`.
    /// A zero marks a hole -- a block never written, which reads as zeros.
    pub fn physical_blocks(&mut self, node: &Node, first: u64, count: usize) -> Result<Vec<u64>, FsError> {
        let mut blocks = Vec::with_capacity(count);
        for offset in 0..count as u64 {
            blocks.push(self.map_block(node, first + offset)?.unwrap_or(0));
        }
        Ok(blocks)
    }

    /// Reads part of a file, without loading all of it.
    ///
    /// `read_all` returns the whole content; a Linux program reading
    /// 64 KiB of a 150 MiB library must not allocate 150 MiB to get them.
    pub fn read_range(&mut self, node: &Node, offset: u64, buffer: &mut [u8]) -> Result<usize, FsError> {
        if node.is_directory() {
            return Err(FsError::IsADirectory);
        }
        if offset >= node.size {
            return Ok(0);
        }
        let count = ((node.size - offset) as usize).min(buffer.len());
        let mut done = 0;
        while done < count {
            let at = offset + done as u64;
            let inside = (at % BLOCK_SIZE as u64) as usize;
            let chunk = (BLOCK_SIZE - inside).min(count - done);
            match self.map_block(node, at / BLOCK_SIZE as u64)? {
                Some(block) => {
                    self.cache.inspect(block, |data| {
                        buffer[done..done + chunk].copy_from_slice(&data[inside..inside + chunk]);
                    })?;
                }
                None => buffer[done..done + chunk].fill(0),
            }
            done += chunk;
        }
        Ok(count)
    }

    /// Writes part of a file, growing it if needed.
    ///
    /// What `write_file` does for a whole content, without rewriting it:
    /// a Linux program writing 4 KiB in the middle of a database must not
    /// carry the other megabytes through memory.
    pub fn write_range(&mut self, node: &mut Node, offset: u64, data: &[u8], now: u64) -> Result<usize, FsError> {
        if node.is_directory() {
            return Err(FsError::IsADirectory);
        }
        if offset + data.len() as u64 > MAX_FILE_SIZE {
            return Err(FsError::TooLarge);
        }
        let mut done = 0;
        while done < data.len() {
            let at = offset + done as u64;
            let inside = (at % BLOCK_SIZE as u64) as usize;
            let chunk = (BLOCK_SIZE - inside).min(data.len() - done);
            let block = self.ensure_block(node, at / BLOCK_SIZE as u64)?;
            self.cache.modify(block, |slot| {
                slot[inside..inside + chunk].copy_from_slice(&data[done..done + chunk]);
            })?;
            done += chunk;
        }
        node.size = node.size.max(offset + data.len() as u64);
        node.modified = now;
        self.write_node(node)?;
        Ok(done)
    }

    /// Cuts a file to `size`, or grows it with a hole.
    pub fn truncate_node(&mut self, node: &mut Node, size: u64, now: u64) -> Result<(), FsError> {
        if node.is_directory() {
            return Err(FsError::IsADirectory);
        }
        if size > MAX_FILE_SIZE {
            return Err(FsError::TooLarge);
        }
        if size < node.size {
            // The blocks past the end go back to the allocator, and so
            // do the indirection tables that then describe nothing.
            let first = size.div_ceil(BLOCK_SIZE as u64);
            for logical in first..node.block_count() {
                if let Some(block) = self.map_block(node, logical)? {
                    self.free_block(block)?;
                    self.clear_block_pointer(node, logical)?;
                }
            }
            self.shrink_indirect(node, first)?;
        }
        node.size = size;
        node.modified = now;
        self.write_node(node)?;
        Ok(())
    }

    /// Gives back the indirection tables of a node that has shrunk.
    ///
    /// Freeing the data blocks is not enough: a table kept for a file
    /// that no longer reaches it is a block lost until the node dies,
    /// and the coherence check says so ("garde une table d'indirection
    /// inutile"). A profile file that grows then is truncated -- the
    /// browser's databases do nothing else -- loses one that way at
    /// every boot.
    ///
    /// `blocks` is the number of logical blocks kept.
    fn shrink_indirect(&mut self, node: &mut Node, blocks: u64) -> Result<(), FsError> {
        if node.indirect == 0 {
            return Ok(());
        }
        if node.indirect_levels >= 2 {
            // Second-level tables entirely past the new end.
            let root = node.indirect;
            for entry in 0..INDIRECT_ENTRIES {
                let first = DIRECT_BLOCKS as u64 + (entry * INDIRECT_ENTRIES) as u64;
                if first < blocks {
                    continue;
                }
                let leaf = self.cache.inspect(root, |data| get_u64(data, entry * 8))?;
                if leaf != 0 {
                    self.free_block(leaf)?;
                    self.cache.modify(root, |data| put_u64(data, entry * 8, 0))?;
                }
            }
        }
        if blocks <= DIRECT_BLOCKS as u64 {
            let root = node.indirect;
            self.free_block(root)?;
            node.indirect = 0;
            node.indirect_levels = 1;
        }
        Ok(())
    }

    /// Forgets the pointer to a logical block that has just been freed.
    fn clear_block_pointer(&mut self, node: &mut Node, logical: u64) -> Result<(), FsError> {
        if logical < DIRECT_BLOCKS as u64 {
            node.direct[logical as usize] = 0;
            return Ok(());
        }
        if node.indirect == 0 {
            return Ok(());
        }
        let entry = (logical - DIRECT_BLOCKS as u64) as usize;
        if node.indirect_levels < 2 {
            let indirect = node.indirect;
            self.cache.modify(indirect, |data| put_u64(data, entry * 8, 0))?;
            return Ok(());
        }
        let (first, second) = (entry / INDIRECT_ENTRIES, entry % INDIRECT_ENTRIES);
        let table = self.cache.inspect(node.indirect, |data| get_u64(data, first * 8))?;
        if table != 0 {
            self.cache.modify(table, |data| put_u64(data, second * 8, 0))?;
        }
        Ok(())
    }

    /// Reads whole blocks of a file into frames, one command per run of
    /// consecutive blocks (`BlockCache::read_into_frames`).
    ///
    /// # Safety
    /// The frames belong to the caller and are not mapped writable
    /// elsewhere while the controller writes into them.
    pub unsafe fn read_blocks_into_frames(&mut self, node: &Node, first: u64, frames: &[u64]) -> Result<(), FsError> {
        let blocks = self.physical_blocks(node, first, frames.len())?;
        let mut index = 0;
        while index < frames.len() {
            if blocks[index] == 0 {
                // A hole: zeros, no command.
                let at = crate::memory::phys_to_virt(frames[index]) as *mut u8;
                // SAFETY: a frame of the caller's, 4 KiB.
                unsafe { core::ptr::write_bytes(at, 0, BLOCK_SIZE) };
                index += 1;
                continue;
            }
            let mut run = 1;
            while index + run < frames.len() && blocks[index + run] == blocks[index] + run as u64 {
                run += 1;
            }
            // SAFETY: passed on from this function's contract.
            unsafe { self.cache.read_into_frames(blocks[index], &frames[index..index + run])? };
            index += run;
        }
        Ok(())
    }

    // ============================================================
    //  Content of a node
    // ============================================================

    /// Reads all the content of a node.
    pub fn read_all(&mut self, node: &Node) -> Result<Vec<u8>, FsError> {
        let mut content = Vec::with_capacity(node.size as usize);
        let mut buffer = alloc::vec![0u8; BLOCK_SIZE];
        let blocks = node.block_count();

        for logical in 0..blocks {
            let remaining = (node.size - logical * BLOCK_SIZE as u64).min(BLOCK_SIZE as u64);
            match self.map_block(node, logical)? {
                Some(block) => {
                    self.cache.read(block, &mut buffer)?;
                    content.extend_from_slice(&buffer[..remaining as usize]);
                }
                // Block never written: a hole reads as zeros.
                None => content.extend(core::iter::repeat_n(0u8, remaining as usize)),
            }
        }

        Ok(content)
    }

    /// Replaces the content of a node entirely.
    pub fn write_all(&mut self, node: &mut Node, content: &[u8], now: u64) -> Result<(), FsError> {
        if content.len() as u64 > MAX_FILE_SIZE {
            return Err(FsError::TooLarge);
        }

        let needed = (content.len() as u64).div_ceil(BLOCK_SIZE as u64);
        let previous = node.block_count();

        for logical in 0..needed {
            let block = match self.ensure_block(node, logical) {
                Ok(block) => block,
                Err(error) => {
                    // The volume filled up during the write. The blocks
                    // already taken during this call are referenced by
                    // nothing -- `node.size` has not changed yet -- and
                    // would stay marked taken with no walk able to find
                    // them again. They are given back before the failure
                    // is propagated.
                    self.release_from(node, previous, needed)?;
                    return Err(error);
                }
            };
            let start = logical as usize * BLOCK_SIZE;
            let end = (start + BLOCK_SIZE).min(content.len());

            if end - start == BLOCK_SIZE {
                self.cache.write(block, &content[start..end])?;
            } else {
                // Last partial block: the rest is zeroed so as not to let
                // the old content show through.
                let mut buffer = alloc::vec![0u8; BLOCK_SIZE];
                buffer[..end - start].copy_from_slice(&content[start..end]);
                self.cache.write(block, &buffer)?;
            }
        }

        // The file has shrunk: the blocks now useless are given back.
        self.release_from(node, needed, previous)?;

        node.size = content.len() as u64;
        node.modified = now;
        self.write_node(node)?;
        Ok(())
    }

    /// Gives back the logical blocks from `first` inclusive to `last`
    /// exclusive.
    ///
    /// The indirection table is given back with them as soon as nothing
    /// beyond the direct pointers is described. Without that, a file gone
    /// from three mebibytes to a hundred bytes would keep a whole block
    /// marked taken for an entirely null table -- a block no walk could
    /// find again, given back only when the node was deleted.
    fn release_from(&mut self, node: &mut Node, first: u64, last: u64) -> Result<(), FsError> {
        for logical in first..last {
            if let Some(block) = self.map_block(node, logical)? {
                self.free_block(block)?;
                self.clear_mapping(node, logical)?;
            }
        }

        if first <= DIRECT_BLOCKS as u64 && node.indirect != 0 {
            self.free_block(node.indirect)?;
            node.indirect = 0;
        }
        Ok(())
    }

    fn clear_mapping(&mut self, node: &mut Node, logical: u64) -> Result<(), FsError> {
        if logical < DIRECT_BLOCKS as u64 {
            node.direct[logical as usize] = 0;
            return Ok(());
        }
        if node.indirect == 0 {
            return Ok(());
        }
        let entry = (logical - DIRECT_BLOCKS as u64) as usize;
        let indirect = node.indirect;
        self.cache
            .modify(indirect, |data| put_u64(data, entry * 8, 0))?;
        Ok(())
    }

    // ============================================================
    //  4.1.2 -- Link table of a directory
    // ============================================================

    /// Lists the entries of a directory.
    pub fn list_links(&mut self, node: &Node) -> Result<Vec<Link>, FsError> {
        if !node.is_directory() {
            return Err(FsError::NotADirectory);
        }

        let mut links = Vec::new();
        let mut buffer = alloc::vec![0u8; BLOCK_SIZE];

        for logical in 0..node.block_count() {
            let Some(block) = self.map_block(node, logical)? else {
                continue;
            };
            self.cache.read(block, &mut buffer)?;
            for slot in 0..LINKS_PER_BLOCK {
                if let Some(link) = Link::decode(&buffer[slot * LINK_SIZE..(slot + 1) * LINK_SIZE])
                {
                    links.push(link);
                }
            }
        }

        Ok(links)
    }

    /// Looks for an entry by its name.
    ///
    /// Compares the name in place, in the cached block. It used to decode
    /// the whole table first -- one `String` per entry -- and every path
    /// resolution goes through here once per component: moving a thousand
    /// files out of a directory of a thousand cost millions of
    /// allocations. The answer is the same: `valid_name` never stores a
    /// name of `NAME_MAX` bytes, so a stored name matches exactly when its
    /// bytes match and a terminator follows.
    pub fn find_link(&mut self, node: &Node, name: &str) -> Result<Option<NodeId>, FsError> {
        if !node.is_directory() {
            return Err(FsError::NotADirectory);
        }
        let wanted = name.as_bytes();
        if wanted.is_empty() || wanted.len() >= super::format::NAME_MAX {
            return Ok(None);
        }
        for logical in 0..node.block_count() {
            let Some(block) = self.map_block(node, logical)? else {
                continue;
            };
            let found = self.cache.inspect(block, |data| {
                (0..LINKS_PER_BLOCK).find_map(|slot| {
                    slot_named(&data[slot * LINK_SIZE..(slot + 1) * LINK_SIZE], wanted)
                })
            })?;
            if found.is_some() {
                return Ok(found);
            }
        }
        Ok(None)
    }

    /// Adds an entry, reusing a free slot if possible.
    fn insert_link(&mut self, parent: &mut Node, name: &str, target: NodeId, now: u64)
    -> Result<(), FsError> {
        if !valid_name(name) {
            return Err(FsError::InvalidName);
        }
        if self.find_link(parent, name)?.is_some() {
            return Err(FsError::AlreadyExists);
        }

        let link = Link {
            name: String::from(name),
            node: target,
        };

        // A slot freed by an earlier removal avoids growing the
        // directory.
        for logical in 0..parent.block_count() {
            let Some(block) = self.map_block(parent, logical)? else {
                continue;
            };
            let free = self.cache.inspect(block, |data| {
                (0..LINKS_PER_BLOCK)
                    .find(|slot| get_u64(data, slot * LINK_SIZE + super::format::NAME_MAX) == 0)
            })?;

            if let Some(slot) = free {
                self.cache.modify(block, |data| {
                    link.encode(&mut data[slot * LINK_SIZE..(slot + 1) * LINK_SIZE]);
                })?;
                parent.modified = now;
                self.write_node(parent)?;
                return Ok(());
            }
        }

        // Every table is full: the directory gains a block.
        let logical = parent.block_count();
        let block = self.ensure_block(parent, logical)?;
        self.cache.modify(block, |data| {
            link.encode(&mut data[..LINK_SIZE]);
        })?;
        parent.size = (logical + 1) * BLOCK_SIZE as u64;
        parent.modified = now;
        self.write_node(parent)?;
        Ok(())
    }

    /// Removes an entry and returns the node it named.
    fn remove_link(&mut self, parent: &mut Node, name: &str, now: u64) -> Result<NodeId, FsError> {
        for logical in 0..parent.block_count() {
            let Some(block) = self.map_block(parent, logical)? else {
                continue;
            };
            let found = self.cache.inspect(block, |data| {
                (0..LINKS_PER_BLOCK).find_map(|slot| {
                    slot_named(&data[slot * LINK_SIZE..(slot + 1) * LINK_SIZE], name.as_bytes())
                        .map(|node| (slot, node))
                })
            })?;

            if let Some((slot, target)) = found {
                // Zeroing the slot is enough: a null `node` marks it
                // free, no need to compact the table.
                self.cache.modify(block, |data| {
                    data[slot * LINK_SIZE..(slot + 1) * LINK_SIZE].fill(0);
                })?;
                self.shrink_links(parent)?;
                parent.modified = now;
                self.write_node(parent)?;
                return Ok(target);
            }
        }
        Err(FsError::NotFound)
    }

    /// Gives back the link blocks a directory no longer uses.
    ///
    /// A removal only zeroes its slot: the table keeps its holes, and
    /// `insert_link` fills them again rather than growing. But a whole
    /// block of holes at the end of the table serves nothing, and the
    /// invariant of DECISIONS.md is that every block comes back -- a
    /// directory grown to two blocks then emptied used to keep the
    /// second one for ever, which the explorer's self-test saw as one
    /// block lost per tree deleted.
    ///
    /// Only the trailing blocks go: a hole in the middle stays, because
    /// freeing it would mean renumbering the ones after it.
    fn shrink_links(&mut self, parent: &mut Node) -> Result<(), FsError> {
        while parent.block_count() > 0 {
            let logical = parent.block_count() - 1;
            let Some(block) = self.map_block(parent, logical)? else {
                // A hole at the end: nothing to free, the size shrinks.
                parent.size = logical * BLOCK_SIZE as u64;
                continue;
            };
            let empty = self.cache.inspect(block, |data| {
                (0..LINKS_PER_BLOCK)
                    .all(|slot| get_u64(data, slot * LINK_SIZE + super::format::NAME_MAX) == 0)
            })?;
            if !empty {
                break;
            }
            self.free_block(block)?;
            self.clear_block_pointer(parent, logical)?;
            parent.size = logical * BLOCK_SIZE as u64;
        }
        let blocks = parent.block_count();
        self.shrink_indirect(parent, blocks)?;
        Ok(())
    }

    /// Changes the name of an entry without touching the node.
    fn rename_link(
        &mut self,
        parent: &mut Node,
        from: &str,
        to: &str,
        now: u64,
    ) -> Result<(), FsError> {
        if !valid_name(to) {
            return Err(FsError::InvalidName);
        }
        if self.find_link(parent, to)?.is_some() {
            return Err(FsError::AlreadyExists);
        }

        for logical in 0..parent.block_count() {
            let Some(block) = self.map_block(parent, logical)? else {
                continue;
            };
            let found = self.cache.inspect(block, |data| {
                (0..LINKS_PER_BLOCK).find_map(|slot| {
                    slot_named(&data[slot * LINK_SIZE..(slot + 1) * LINK_SIZE], from.as_bytes())
                        .map(|node| (slot, node))
                })
            })?;

            if let Some((slot, target)) = found {
                let link = Link {
                    name: String::from(to),
                    node: target,
                };
                self.cache.modify(block, |data| {
                    link.encode(&mut data[slot * LINK_SIZE..(slot + 1) * LINK_SIZE]);
                })?;
                parent.modified = now;
                self.write_node(parent)?;
                return Ok(());
            }
        }
        Err(FsError::NotFound)
    }

    // ============================================================
    //  4.5 -- Rights checking
    // ============================================================

    /// Does the caller hold a permission on a node?
    ///
    /// Three access classes, in order: owner, group, others. The first
    /// that applies decides -- an owner from whom `read` was taken does
    /// not get it back through the "others" mask.
    pub fn allows(&self, node: &Node, who: Credentials, permission: u64) -> bool {
        node_allows(node, who, permission)
    }

    /// Checks a permission and logs the result (4.5.4).
    /// Same check as `require`, for the callers outside this module
    /// (the ranged read and write of `fs`).
    pub fn require_permission(
        &mut self,
        node: &Node,
        who: Credentials,
        permission: u64,
        path: &str,
    ) -> Result<(), FsError> {
        self.require(node, who, permission, path)
    }

    fn require(
        &mut self,
        node: &Node,
        who: Credentials,
        permission: u64,
        path: &str,
    ) -> Result<(), FsError> {
        let granted = self.allows(node, who, permission);
        audit::record(who, path, permission, granted);
        if granted {
            Ok(())
        } else {
            Err(FsError::PermissionDenied(permission))
        }
    }

    // ============================================================
    //  Path resolution
    // ============================================================

    /// Resolves an absolute path and returns the node named.
    ///
    /// Every directory traversed demands `access`: without it, one cannot
    /// see what it contains, and so no more traverse it.
    pub fn resolve(&mut self, path: &str, who: Credentials) -> Result<Node, FsError> {
        let mut node = self.read_node(self.superblock.root_node)?;

        for component in path.split('/').filter(|c| !c.is_empty()) {
            if !node.is_directory() {
                return Err(FsError::NotADirectory);
            }
            self.require(&node, who, Permissions::ACCESS, path)?;

            let target = self
                .find_link(&node, component)?
                .ok_or(FsError::NotFound)?;
            node = self.read_node(target)?;
        }

        Ok(node)
    }

    /// Resolves the parent directory of a path and returns the last segment.
    fn resolve_parent(
        &mut self,
        path: &str,
        who: Credentials,
    ) -> Result<(Node, String), FsError> {
        let trimmed = path.trim_end_matches('/');
        let (parent_path, name) = match trimmed.rfind('/') {
            Some(index) => (&trimmed[..index], &trimmed[index + 1..]),
            None => ("", trimmed),
        };
        if name.is_empty() {
            return Err(FsError::InvalidName);
        }

        let parent = self.resolve(parent_path, who)?;
        if !parent.is_directory() {
            return Err(FsError::NotADirectory);
        }
        Ok((parent, String::from(name)))
    }

    // ============================================================
    //  Operations
    // ============================================================

    /// Creates a directory.
    pub fn create_directory(
        &mut self,
        path: &str,
        who: Credentials,
        mode: Mode,
        now: u64,
    ) -> Result<NodeId, FsError> {
        let (mut parent, name) = self.resolve_parent(path, who)?;
        self.require(&parent, who, Permissions::WRITE, path)?;

        if self.find_link(&parent, &name)?.is_some() {
            return Err(FsError::AlreadyExists);
        }

        let id = self.allocate_node()?;
        let mut node = Node::new(id, NodeKind::Directory, who.uid, who.gid, mode, now);
        node.links = 1;
        self.write_node(&node)?;

        self.insert_link(&mut parent, &name, id, now)?;
        self.flush()?;
        Ok(id)
    }

    /// Creates a file and writes a content into it.
    ///
    /// `write` covers creation; replacing an existing file is the business
    /// of `overwrite`, a distinct permission (spec 5.1).
    pub fn write_file(
        &mut self,
        path: &str,
        content: &[u8],
        who: Credentials,
        mode: Mode,
        now: u64,
    ) -> Result<NodeId, FsError> {
        let (mut parent, name) = self.resolve_parent(path, who)?;

        if let Some(existing) = self.find_link(&parent, &name)? {
            let mut node = self.read_node(existing)?;
            if node.is_directory() {
                return Err(FsError::IsADirectory);
            }
            self.require(&node, who, Permissions::OVERWRITE, path)?;
            self.write_all(&mut node, content, now)?;
            self.flush()?;
            return Ok(existing);
        }

        self.require(&parent, who, Permissions::WRITE, path)?;

        let id = self.allocate_node()?;
        let mut node = Node::new(id, NodeKind::File, who.uid, who.gid, mode, now);
        node.links = 1;
        self.write_all(&mut node, content, now)?;

        self.insert_link(&mut parent, &name, id, now)?;
        self.flush()?;
        Ok(id)
    }

    /// Reads a file.
    pub fn read_file(&mut self, path: &str, who: Credentials) -> Result<Vec<u8>, FsError> {
        let node = self.resolve(path, who)?;
        if node.is_directory() {
            return Err(FsError::IsADirectory);
        }
        self.require(&node, who, Permissions::READ, path)?;
        self.read_all(&node)
    }

    /// Deletes an entry. The node is only freed at the last link.
    pub fn delete(&mut self, path: &str, who: Credentials, now: u64) -> Result<(), FsError> {
        let (mut parent, name) = self.resolve_parent(path, who)?;
        let target = self
            .find_link(&parent, &name)?
            .ok_or(FsError::NotFound)?;
        let mut node = self.read_node(target)?;

        self.require(&node, who, Permissions::DELETE, path)?;

        // A non-empty directory is not deleted: its entries would become
        // unreachable without their node being freed.
        if node.is_directory() && !self.list_links(&node)?.is_empty() {
            return Err(FsError::DirectoryNotEmpty);
        }

        self.remove_link(&mut parent, &name, now)?;

        node.links = node.links.saturating_sub(1);
        if node.links == 0 {
            self.release_node(&node)?;
        } else {
            self.write_node(&node)?;
        }

        self.flush()?;
        Ok(())
    }

    /// Renames an entry without touching the node (spec 4.1).
    pub fn rename(&mut self, path: &str, new_name: &str, who: Credentials, now: u64)
    -> Result<(), FsError> {
        let (mut parent, name) = self.resolve_parent(path, who)?;
        let target = self
            .find_link(&parent, &name)?
            .ok_or(FsError::NotFound)?;
        let node = self.read_node(target)?;

        self.require(&node, who, Permissions::RENAME, path)?;
        self.rename_link(&mut parent, &name, new_name, now)?;
        self.flush()?;
        Ok(())
    }

    /// 4.1.3 -- Moves an entry: two link tables rewritten, zero bytes of
    /// data copied, whatever the size of the file.
    pub fn move_entry(
        &mut self,
        from: &str,
        to: &str,
        who: Credentials,
        now: u64,
    ) -> Result<NodeId, FsError> {
        let result = self.relink(from, to, who, now);
        self.flush()?;
        result
    }

    /// Moves several entries, and writes to the medium **once**.
    ///
    /// Each move is two link rewrites, a few bytes in two cached blocks;
    /// what made a thousand moves slow was the thousand flushes of
    /// `move_entry`, each one a round trip to the disk. Stops at the first
    /// refusal and returns how many were moved before it -- those stay
    /// moved, and are flushed like the others.
    pub fn move_entries(
        &mut self,
        moves: &[(String, String)],
        who: Credentials,
        now: u64,
    ) -> Result<usize, (usize, FsError)> {
        let mut moved = 0;
        let mut failure = None;
        for (from, to) in moves {
            match self.relink(from, to, who, now) {
                Ok(_) => moved += 1,
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        if let Err(error) = self.flush() {
            return Err((moved, error));
        }
        match failure {
            Some(error) => Err((moved, error)),
            None => Ok(moved),
        }
    }

    /// One move, in the cache only: the link leaves its directory for the
    /// other. The node and its data do not move.
    fn relink(
        &mut self,
        from: &str,
        to: &str,
        who: Credentials,
        now: u64,
    ) -> Result<NodeId, FsError> {
        let (source_parent, source_name) = self.resolve_parent(from, who)?;
        let target = self
            .find_link(&source_parent, &source_name)?
            .ok_or(FsError::NotFound)?;
        let node = self.read_node(target)?;

        self.require(&node, who, Permissions::MOVE, from)?;

        let (destination_parent, destination_name) = self.resolve_parent(to, who)?;
        // The spec explicitly demands `write` on the destination
        // directory in addition to `move` on the entry (5.1).
        self.require(&destination_parent, who, Permissions::WRITE, to)?;

        if self
            .find_link(&destination_parent, &destination_name)?
            .is_some()
        {
            return Err(FsError::AlreadyExists);
        }

        // The nodes are read back: `insert_link` and `remove_link` change
        // their size, and working on two stale copies of the same
        // directory would crush one of the two modifications.
        let mut destination = self.read_node(destination_parent.id)?;
        self.insert_link(&mut destination, &destination_name, target, now)?;

        let mut source = self.read_node(source_parent.id)?;
        self.remove_link(&mut source, &source_name, now)?;
        Ok(target)
    }

    /// Creates an extra virtual link to an existing node.
    pub fn link(&mut self, from: &str, to: &str, who: Credentials, now: u64)
    -> Result<NodeId, FsError> {
        let node = self.resolve(from, who)?;
        self.require(&node, who, Permissions::LINK, from)?;

        let (destination_parent, name) = self.resolve_parent(to, who)?;
        self.require(&destination_parent, who, Permissions::WRITE, to)?;

        let mut destination = self.read_node(destination_parent.id)?;
        self.insert_link(&mut destination, &name, node.id, now)?;

        let mut node = self.read_node(node.id)?;
        node.links += 1;
        self.write_node(&node)?;

        self.flush()?;
        Ok(node.id)
    }

    /// Copies a file inside the volume.
    ///
    /// 4.1.4 -- It really copies the data, which is only indispensable
    /// between distinct volumes. Inside one volume, `link` gets the same
    /// result without consuming a byte; the copy stays on offer because
    /// the user sometimes wants two contents that will diverge.
    pub fn copy(&mut self, from: &str, to: &str, who: Credentials, now: u64)
    -> Result<NodeId, FsError> {
        let source = self.resolve(from, who)?;
        if source.is_directory() {
            return Err(FsError::IsADirectory);
        }
        self.require(&source, who, Permissions::COPY, from)?;
        self.require(&source, who, Permissions::READ, from)?;

        let content = self.read_all(&source)?;
        let mode = Mode {
            owner: source.perm_owner,
            group: source.perm_group,
            other: source.perm_other,
        };
        self.write_file(to, &content, who, mode, now)
    }

    /// Metadata of a node.
    pub fn stat(&mut self, path: &str, who: Credentials) -> Result<Node, FsError> {
        self.resolve(path, who)
    }

    /// Modifies the permission masks of a node.
    pub fn chmod(&mut self, path: &str, mode: Mode, who: Credentials, now: u64)
    -> Result<(), FsError> {
        let mut node = self.resolve(path, who)?;
        self.require(&node, who, Permissions::CHMOD, path)?;

        // A mask carrying an unknown bit is refused rather than
        // truncated: same reasoning as in 3.1 on application permissions,
        // granting what one does not understand is a hole.
        if mode.owner.unknown_bits() | mode.group.unknown_bits() | mode.other.unknown_bits() != 0 {
            return Err(FsError::InvalidPermission);
        }

        let applicable = node.kind.applicable_permissions();
        node.perm_owner = Permissions(mode.owner.0 & applicable);
        node.perm_group = Permissions(mode.group.0 & applicable);
        node.perm_other = Permissions(mode.other.0 & applicable);
        node.modified = now;
        self.write_node(&node)?;
        self.flush()?;
        Ok(())
    }

    /// Transfers the ownership of a node.
    pub fn chown(&mut self, path: &str, owner: Uid, group: Gid, who: Credentials, now: u64)
    -> Result<(), FsError> {
        let mut node = self.resolve(path, who)?;
        self.require(&node, who, Permissions::OWN, path)?;
        node.owner = owner;
        node.group = group;
        node.modified = now;
        self.write_node(&node)?;
        self.flush()?;
        Ok(())
    }

    /// Lists the content of a directory.
    pub fn list(&mut self, path: &str, who: Credentials) -> Result<Vec<Link>, FsError> {
        let node = self.resolve(path, who)?;
        if !node.is_directory() {
            return Err(FsError::NotADirectory);
        }
        self.require(&node, who, Permissions::ACCESS, path)?;
        self.list_links(&node)
    }

    /// 4.5.2 -- Is a file executable by the caller?
    ///
    /// The two-level rule: `execute` must be granted **on the file and on
    /// the directory containing it** (spec 5.1). One of the two is not
    /// enough, and it is the directory that decides in the last resort --
    /// a perfectly executable binary stays inert in a directory that does
    /// not allow execution.
    pub fn is_executable(&mut self, path: &str, who: Credentials) -> Result<bool, FsError> {
        let (parent, name) = self.resolve_parent(path, who)?;
        let target = self
            .find_link(&parent, &name)?
            .ok_or(FsError::NotFound)?;
        let node = self.read_node(target)?;

        let file_allows = self.allows(&node, who, Permissions::EXECUTE);
        let dir_allows = self.allows(&parent, who, Permissions::EXECUTE);
        let granted = file_allows && dir_allows;

        audit::record(who, path, Permissions::EXECUTE, granted);
        Ok(granted)
    }

    /// Creates every missing directory of a path.
    pub fn create_path(
        &mut self,
        path: &str,
        who: Credentials,
        mode: Mode,
        now: u64,
    ) -> Result<NodeId, FsError> {
        let mut current = String::new();
        let mut last = self.superblock.root_node;

        for component in path.split('/').filter(|c| !c.is_empty()) {
            current.push('/');
            current.push_str(component);

            match self.resolve(&current, who) {
                Ok(node) if node.is_directory() => {
                    last = node.id;
                }
                Ok(_) => return Err(FsError::NotADirectory),
                Err(FsError::NotFound) => {
                    last = self.create_directory(&current, who, mode, now)?;
                }
                Err(other) => return Err(other),
            }
        }

        Ok(last)
    }

    // ============================================================
    //  Diagnostics
    // ============================================================

    pub fn dump_state(&mut self) {
        let superblock = self.superblock.clone();
        crate::serial_println!(
            "[fs] volume \"{}\" ({}) - {} blocs de 4 Kio ({} Mio)",
            superblock.label,
            self.medium().as_str(),
            superblock.total_blocks,
            superblock.total_blocks * BLOCK_SIZE as u64 / (1024 * 1024)
        );
        crate::serial_println!(
            "[fs]   noeuds       : {} au total, {} libres - table aux blocs {}..{}",
            superblock.node_count,
            superblock.free_nodes,
            superblock.node_table_start,
            superblock.node_table_start + superblock.node_table_blocks
        );
        crate::serial_println!(
            "[fs]   blocs        : {} libres - donnees a partir du bloc {}",
            superblock.free_blocks,
            superblock.data_start
        );
        self.cache.dump_state();
    }

    /// Walks the tree and displays it with indentation.
    pub fn dump_tree(&mut self, path: &str, who: Credentials, depth: usize, limit: usize) {
        if depth > limit {
            return;
        }
        let Ok(links) = self.list(path, who) else {
            return;
        };

        let mut sorted = links;
        sorted.sort_by(|a, b| a.name.cmp(&b.name));

        for link in sorted.iter() {
            let Ok(node) = self.read_node(link.node) else {
                continue;
            };
            let mut indent = String::new();
            for _ in 0..depth {
                indent.push_str("  ");
            }

            if node.is_directory() {
                crate::serial_println!("[fs]   {}{}/", indent, link.name);
                let child = if path == "/" {
                    alloc::format!("/{}", link.name)
                } else {
                    alloc::format!("{}/{}", path, link.name)
                };
                self.dump_tree(&child, who, depth + 1, limit);
            } else {
                crate::serial_println!(
                    "[fs]   {}{}  ({} o, {})",
                    indent,
                    link.name,
                    node.size,
                    node.perm_owner.compact()
                );
            }
        }
    }

    /// Checks the internal coherence of the volume.
    ///
    /// Checks two invariants nothing else watches: every node marked
    /// taken really carries a type, and every block referenced by a node
    /// really is marked taken in the bitmap. A discrepancy signals an
    /// allocation bug, not a corruption of the medium.
    pub fn check(&mut self) -> Result<(usize, usize), FsError> {
        let mut nodes = 0;
        let mut blocks = 0;
        let mut errors = 0;

        for id in 1..=self.superblock.node_count {
            if !self.node_used(id)? {
                continue;
            }
            nodes += 1;
            let node = match self.read_node(id) {
                Ok(node) => node,
                Err(_) => {
                    crate::serial_println!("[fs] INCOHERENCE : noeud {} marque occupe mais libre", id);
                    errors += 1;
                    continue;
                }
            };

            if node.links == 0 {
                crate::serial_println!("[fs] INCOHERENCE : noeud {} sans lien", id);
                errors += 1;
            }

            // The indirection table is a block like any other: were it
            // not counted, a block taken for it and never given back
            // would go unnoticed.
            if node.indirect != 0 {
                blocks += 1;
                // The tables of the second level count too.
                if node.indirect_levels >= 2 {
                    let mut table = alloc::vec![0u8; BLOCK_SIZE];
                    self.cache.read(node.indirect, &mut table)?;
                    for entry in 0..INDIRECT_ENTRIES {
                        if get_u64(&table, entry * 8) != 0 {
                            blocks += 1;
                        }
                    }
                }
                if !self.block_used(node.indirect)? {
                    crate::serial_println!(
                        "[fs] INCOHERENCE : table d'indirection {} du noeud {} non marquee occupee",
                        node.indirect,
                        id
                    );
                    errors += 1;
                }
                // An indirection table describing nothing any more is a
                // lost block: the file shrank without it being given
                // back.
                if node.block_count() <= DIRECT_BLOCKS as u64 {
                    crate::serial_println!(
                        "[fs] INCOHERENCE : noeud {} garde une table d'indirection inutile",
                        id
                    );
                    errors += 1;
                }
            }

            for logical in 0..node.block_count() {
                if let Some(block) = self.map_block(&node, logical)? {
                    blocks += 1;
                    if !self.block_used(block)? {
                        crate::serial_println!(
                            "[fs] INCOHERENCE : bloc {} du noeud {} non marque occupe",
                            block,
                            id
                        );
                        errors += 1;
                    }
                }
            }
        }

        if errors > 0 {
            return Err(FsError::Corrupt);
        }
        Ok((nodes, blocks))
    }
}

/// The node a link slot names, if the slot holds `wanted` -- compared in
/// place, with no decoding. A name of `NAME_MAX` bytes or more is never
/// stored (`valid_name`), so a match is equal bytes and a terminator.
fn slot_named(entry: &[u8], wanted: &[u8]) -> Option<NodeId> {
    if wanted.is_empty() || wanted.len() >= super::format::NAME_MAX {
        return None;
    }
    let target = get_u64(entry, super::format::NAME_MAX);
    (target != 0 && &entry[..wanted.len()] == wanted && entry[wanted.len()] == 0).then_some(target)
}

/// The rule of rights, shared by NovaFS and LIFS (spec 5.1): three access
/// classes, in order -- owner, group, others; the first that applies
/// decides, and an owner from whom `read` was taken does not get it back
/// through the "others" mask.
pub fn node_allows(node: &Node, who: Credentials, permission: u64) -> bool {
    // A permission that makes no sense on this node type is never
    // granted, whatever the role.
    if permission & node.kind.applicable_permissions() == 0 {
        return false;
    }

    // The audit stays reserved to ADMIN and ROOT, masks or not (spec 5.1).
    if permission == Permissions::AUDIT {
        return who.role.may_audit();
    }

    if who.role.bypasses_masks() {
        return true;
    }

    let mask = if node.owner == who.uid {
        node.perm_owner
    } else if node.group == who.gid {
        node.perm_group
    } else {
        node.perm_other
    };

    mask.contains(permission)
}

/// Default rights of an account, derived from its role.
pub fn default_mode_for(role: Role, directory: bool) -> Mode {
    match (role, directory) {
        (Role::Root | Role::Service, true) => Mode::system_dir(),
        (Role::Root | Role::Service, false) => Mode::system_binary(),
        (_, true) => Mode::private_dir(),
        (_, false) => Mode::private_file(),
    }
}

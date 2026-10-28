//! The Linux root, served by Nova's own filesystem.
//!
//! ## Why
//!
//! The browser's files used to live on a separate disk, a tar archive
//! read by `tarfs`: a second filesystem world beside Nova's, with its own
//! root, invisible to the explorer and to every Nova application. What
//! the Linux subsystem should be is a **translation** of calls, not a
//! second system. So the files sit in the Nova volume, under a directory
//! of the tree (`ROOT`), and a Linux path is that directory's path plus
//! the rest: `/usr/lib/libxul.so` is `/apps/web-browser/root/usr/lib/...`,
//! one file of the volume, made of the same nodes and the same link
//! tables as everything else (spec 4.1).
//!
//! ## What it adds over `fs`
//!
//! Two things the Nova calls do not do, both required by a program that
//! maps a 150 MiB library:
//!
//! * **ranged reads** (`fs::read_range`): `read_file` returns the whole
//!   content, which is out of the question here;
//! * **a page cache**, shared by every process: a page of a file is read
//!   once into a frame, and every private mapping of that page maps that
//!   frame read-only (`vm::Pager::cached_frame`), copying it on the first
//!   write. Filling it reads up to `READAHEAD` pages in one disk command
//!   when their blocks follow one another (`fs::read_blocks_into_frames`).
//!
//! It is the same cache `tarfs` had; it lives here now, over the real
//! filesystem, and `tarfs` is gone with the disk it read.
//!
//! The volume is also where the session's home lives, written by the
//! programs: the pages they map shared and writable are in `shared`.

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};

use super::{DirEntry, Inode, Kind, S_IFDIR, S_IFREG, Stat};
use crate::fs::perm::{Credentials, Permissions};
use crate::posix::errno::*;
use crate::vga::SpinLock;

mod shared;

/// Where the Linux root lives in the Nova tree.
pub const ROOT: &str = "/apps/web-browser/root";

const PAGE: u64 = 4096;
/// Pages read together on a miss: 128 KiB, one disk command when the
/// blocks follow one another.
const READAHEAD: u64 = 32;

/// Pages already read, by (node, page of the file). The frames are never
/// given back: the volume is read while the system runs, and knowing
/// which spaces still map a frame would need a reverse map the `vm` does
/// not keep. The budget stops the cache from eating the memory.
static CACHE: SpinLock<alloc::collections::BTreeMap<(u64, u64), u64>> =
    SpinLock::new(alloc::collections::BTreeMap::new());
static BUDGET: AtomicU64 = AtomicU64::new(0);
static DISK_PAGES: AtomicU64 = AtomicU64::new(0);
static DISK_NS: AtomicU64 = AtomicU64::new(0);
static HITS: AtomicU64 = AtomicU64::new(0);

/// (pages read from the disk, milliseconds spent, pages held, hits,
/// shared pages).
pub fn stats() -> (u64, u64, u64, u64, u64) {
    (
        DISK_PAGES.load(Ordering::Relaxed),
        DISK_NS.load(Ordering::Relaxed) / 1_000_000,
        CACHE.lock().len() as u64,
        HITS.load(Ordering::Relaxed),
        shared::count() as u64,
    )
}

/// One node of the Nova volume, seen as a Linux inode.
pub struct NovaNode {
    /// Absolute Nova path.
    path: String,
    /// Node number, resolved on first use and kept: reads and writes
    /// then cost no walk of the tree (`fs::read_range_node`).
    id: SpinLock<Option<u64>>,
}

/// The system volume: the Linux root lives there.
const LETTER: char = '/';

impl NovaNode {
    fn new(path: String) -> NovaNode {
        NovaNode { path, id: SpinLock::new(None) }
    }
}

/// The Linux root, if the volume carries one.
pub fn mount_root() -> Option<Arc<dyn Inode>> {
    let who = Credentials::kernel();
    let node = crate::fs::stat(ROOT, who).ok()?;
    if !node.is_directory() {
        return None;
    }
    let (_, _, free) = crate::frame::stats();
    BUDGET.store((free / 3) as u64, Ordering::Relaxed);
    let (files, bytes) = weigh(ROOT);
    crate::serial_println!(
        "[novafs] racine Linux dans {} : {} fichiers, {} Mio - cache de pages jusqu'a {} Mio",
        ROOT,
        files,
        bytes >> 20,
        BUDGET.load(Ordering::Relaxed) >> 8
    );
    Some(Arc::new(NovaNode::new(ROOT.to_string())))
}

/// Any directory of the Nova tree, seen as a Linux directory: the home
/// of the session, where the browser keeps its profile.
pub fn mount_at(nova_path: &str) -> Option<Arc<dyn Inode>> {
    let node = crate::fs::stat(nova_path, Credentials::kernel()).ok()?;
    node.is_directory().then(|| Arc::new(NovaNode::new(nova_path.to_string())) as Arc<dyn Inode>)
}

/// Files and bytes under a directory -- the line printed at mount.
fn weigh(path: &str) -> (u64, u64) {
    let who = Credentials::kernel();
    let mut files = 0;
    let mut bytes = 0;
    let mut stack = alloc::vec![path.to_string()];
    while let Some(directory) = stack.pop() {
        let Ok(links) = crate::fs::list(&directory, who) else {
            continue;
        };
        for link in links {
            let child = alloc::format!("{}/{}", directory, link.name);
            match crate::fs::stat(&child, who) {
                Ok(node) if node.is_directory() => stack.push(child),
                Ok(node) => {
                    files += 1;
                    bytes += node.size;
                }
                Err(_) => {}
            }
        }
    }
    (files, bytes)
}

impl NovaNode {
    fn child(&self, name: &str) -> String {
        alloc::format!("{}/{}", self.path, name)
    }

    /// The node, by its number once known -- the path is walked once.
    fn node(&self) -> LxResult<crate::fs::format::Node> {
        if let Some(id) = *self.id.lock()
            && let Ok(node) = crate::fs::stat_node(LETTER, id)
        {
            return Ok(node);
        }
        let node = crate::fs::stat(&self.path, Credentials::kernel()).map_err(|_| ENOENT)?;
        *self.id.lock() = Some(node.id);
        Ok(node)
    }

    /// The node's number, resolving the path if it is not known yet.
    fn id(&self) -> LxResult<u64> {
        if let Some(id) = *self.id.lock() {
            return Ok(id);
        }
        Ok(self.node()?.id)
    }

    /// `read_at` from the disk and the page cache, before the shared
    /// frames are laid over it.
    fn read_disk(&self, node: &crate::fs::format::Node, offset: u64, buf: &mut [u8]) -> LxResult<usize> {
        // Whole pages come from the page cache -- that is how a program
        // reads a library, and those pages are then the ones its mappings
        // use. What is left (a tail, or a buffer that is not a whole
        // number of pages) is read normally: a short read where the file
        // still has bytes is a read error to the caller, and Firefox
        // refused its own configuration file over exactly that.
        let mut done = 0;
        if offset % PAGE == 0 {
            while done + PAGE as usize <= buf.len() {
                let at = offset + done as u64;
                if at >= node.size {
                    return Ok(done);
                }
                let Some(phys) = cached_page(node, at / PAGE) else {
                    break;
                };
                let take = ((node.size - at) as usize).min(PAGE as usize);
                let source = crate::memory::phys_to_virt(phys) as *const u8;
                // SAFETY: a cache frame, never freed, never written.
                unsafe { core::ptr::copy_nonoverlapping(source, buf[done..].as_mut_ptr(), take) };
                done += take;
                if take < PAGE as usize {
                    return Ok(done);
                }
            }
        }
        if done == buf.len() || offset + done as u64 >= node.size {
            return Ok(done);
        }
        let read = crate::fs::read_range_node(LETTER, node.id, offset + done as u64, &mut buf[done..])
            .map_err(|error| {
                crate::serial_println!(
                    "[novafs] lecture refusee : {} (noeud {}, position {}) - {}",
                    self.path, node.id, offset + done as u64, error.message()
                );
                EIO
            })?;
        Ok(done + read)
    }
}

/// Drops the cached pages of node `id`: its content has changed. The
/// frames stay with the mappings that hold them.
fn forget_pages(id: u64) {
    let mut cache = CACHE.lock();
    let keys: Vec<(u64, u64)> = cache.range((id, 0)..(id + 1, 0)).map(|(k, _)| *k).collect();
    for key in keys {
        cache.remove(&key);
    }
}

/// Node number of a path, if it names something.
fn id_of(path: &str) -> Option<u64> {
    crate::fs::stat(path, Credentials::kernel()).ok().map(|node| node.id)
}

/// A link to node `id` went. If that was the last, the node is gone and
/// its number free: neither cache may hand its pages to the file that
/// gets the number next.
fn forget_if_gone(id: u64) {
    if crate::fs::stat_node(LETTER, id).is_err() {
        forget_pages(id);
        shared::forget(id);
    }
}

/// Frame holding page `page` of the file, read from the disk with the
/// pages after it on a miss. None past the end, or on an I/O error.
fn cached_page(node: &crate::fs::format::Node, page: u64) -> Option<u64> {
    let key = (node.id, page);
    if let Some(phys) = CACHE.lock().get(&key) {
        HITS.fetch_add(1, Ordering::Relaxed);
        return Some(*phys);
    }
    let pages_in_file = node.size.div_ceil(PAGE);
    if page >= pages_in_file {
        return None;
    }

    let run = {
        let cache = CACHE.lock();
        if let Some(phys) = cache.get(&key) {
            HITS.fetch_add(1, Ordering::Relaxed);
            return Some(*phys);
        }
        let room = BUDGET.load(Ordering::Relaxed).saturating_sub(cache.len() as u64);
        let mut run = 0;
        while run < READAHEAD.min(room) && page + run < pages_in_file && !cache.contains_key(&(node.id, page + run)) {
            run += 1;
        }
        run
    };
    if run == 0 {
        return None;
    }

    let mut frames: Vec<u64> = Vec::with_capacity(run as usize);
    for _ in 0..run {
        match crate::frame::allocate_frame() {
            Some(frame) => frames.push(frame.start_addr()),
            None => break,
        }
    }
    if frames.is_empty() {
        return None;
    }

    let started = crate::posix::time::monotonic_ns();
    // SAFETY: the frames were just allocated and are mapped nowhere.
    let read = unsafe { crate::fs::read_blocks_into_frames_node(LETTER, node.id, page, &frames) };
    DISK_PAGES.fetch_add(frames.len() as u64, Ordering::Relaxed);
    DISK_NS.fetch_add(crate::posix::time::monotonic_ns() - started, Ordering::Relaxed);
    if let Err(error) = &read {
        crate::serial_println!(
            "[novafs] lecture de {} page(s) du noeud {} (page {}) refusee : {}",
            frames.len(), node.id, page, error.message()
        );
        for phys in frames {
            // SAFETY: allocated above, never mapped.
            unsafe { crate::frame::deallocate_frame(crate::frame::Frame::containing(phys)) };
        }
        return None;
    }

    // The last page of the file ends mid-block: what lies past the end
    // reads as zeros (`Pager::fill`'s contract).
    for (index, phys) in frames.iter().enumerate() {
        let start = (page + index as u64) * PAGE;
        if start + PAGE > node.size {
            let keep = node.size.saturating_sub(start) as usize;
            let at = crate::memory::phys_to_virt(*phys) as *mut u8;
            // SAFETY: a frame of ours, 4 KiB.
            unsafe { core::ptr::write_bytes(at.add(keep), 0, PAGE as usize - keep) };
        }
    }

    let mut cache = CACHE.lock();
    for (index, phys) in frames.iter().enumerate() {
        cache.insert((node.id, page + index as u64), *phys);
    }
    Some(frames[0])
}

impl Inode for NovaNode {
    fn stat(&self) -> Stat {
        let Ok(node) = self.node() else {
            return Stat::default();
        };
        let executable = node.perm_other.contains(Permissions::EXECUTE);
        let (type_bits, mode) = if node.is_directory() {
            (S_IFDIR, 0o755)
        } else if executable {
            (S_IFREG, 0o755)
        } else {
            (S_IFREG, 0o644)
        };
        Stat {
            dev: super::device_of("novafs"),
            ino: node.id,
            mode: type_bits | mode,
            nlink: node.links as u32,
            uid: node.owner,
            gid: node.group,
            size: node.size,
            mtime_ns: node.modified * 1_000_000_000,
            ..Default::default()
        }
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> LxResult<usize> {
        let node = self.node()?;
        if node.is_directory() {
            return Err(EISDIR);
        }
        let read = self.read_disk(&node, offset, buf)?;
        // What was written through a shared mapping is in its frames,
        // not yet on the disk.
        if shared::has_any(node.id) {
            shared::read_overlay(node.id, offset, &mut buf[..read]);
        }
        Ok(read)
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> LxResult<usize> {
        let id = self.id()?;
        let now = crate::idt::ticks();
        let written = crate::fs::write_range_node(LETTER, id, offset, buf, now)
            .map_err(|error| match error {
                crate::fs::FsError::IsADirectory => EISDIR,
                crate::fs::FsError::NoSpace | crate::fs::FsError::TooLarge => ENOSPC,
                _ => EIO,
            })?;
        // The pages of this file that the cache holds are no longer what
        // the file says: a mapping made afterwards must read the new
        // bytes. The frames already mapped somewhere stay where they are.
        forget_pages(id);
        // A shared mapping sees the write at once.
        shared::write_through(id, offset, &buf[..written]);
        Ok(written)
    }

    fn truncate(&self, size: u64) -> LxResult<()> {
        let id = self.id()?;
        let now = crate::idt::ticks();
        crate::fs::truncate_node(LETTER, id, size, now).map_err(|_| EIO)?;
        forget_pages(id);
        shared::truncate(id, size);
        Ok(())
    }

    fn sync(&self) -> LxResult<()> {
        let node = self.node()?;
        if node.is_directory() {
            return Ok(());
        }
        shared::write_back(node.id, node.size).map_err(|_| EIO)?;
        forget_pages(node.id);
        Ok(())
    }

    fn create(&self, name: &str, kind: Kind, _mode: u32, _target: &str) -> LxResult<Arc<dyn Inode>> {
        let path = self.child(name);
        let who = Credentials::kernel();
        let now = crate::idt::ticks();
        match kind {
            Kind::Dir => {
                crate::fs::create_directory(&path, who, crate::fs::perm::Mode::private_dir(), now)
                    .map_err(|error| match error {
                        crate::fs::FsError::AlreadyExists => EEXIST,
                        _ => EIO,
                    })?;
            }
            Kind::File => {
                crate::fs::write_file(&path, &[], who, crate::fs::perm::Mode::private_file(), now)
                    .map_err(|_| EIO)?;
            }
            // Nova's filesystem has one kind of link, the one a directory
            // holds: a symbolic link has no node of its own (spec 4.1).
            _ => return Err(EPERM),
        }
        Ok(Arc::new(NovaNode::new(path)))
    }

    fn unlink(&self, name: &str, _dir: bool) -> LxResult<()> {
        let path = self.child(name);
        // The number is taken before the link goes: afterwards the path
        // names nothing.
        let id = id_of(&path);
        crate::fs::delete(&path, Credentials::kernel(), crate::idt::ticks()).map_err(|error| match error {
            crate::fs::FsError::NotFound => ENOENT,
            crate::fs::FsError::DirectoryNotEmpty => ENOTEMPTY,
            _ => EIO,
        })?;
        if let Some(id) = id {
            forget_if_gone(id);
        }
        Ok(())
    }

    fn rename(&self, old: &str, new_dir: &Arc<dyn Inode>, new: &str) -> LxResult<()> {
        let Some(target) = new_dir.as_any().downcast_ref::<NovaNode>() else {
            return Err(EXDEV);
        };
        let (from, to) = (self.child(old), target.child(new));
        let who = Credentials::kernel();
        let now = crate::idt::ticks();
        // A destination already there is replaced, as Linux promises.
        if let Some(id) = id_of(&to) {
            crate::fs::delete(&to, who, now).map_err(|_| EIO)?;
            forget_if_gone(id);
        }
        crate::fs::move_entry(&from, &to, who, now).map_err(|error| match error {
            crate::fs::FsError::NotFound => ENOENT,
            _ => EIO,
        })?;
        Ok(())
    }

    fn set_mtime(&self, _ns: u64) -> LxResult<()> {
        Ok(())
    }

    fn chmod(&self, _mode: u32) -> LxResult<()> {
        // The Nova permissions of the node stay what the system gave
        // them: a Linux mode of nine bits cannot say the same thing.
        Ok(())
    }

    fn lookup(&self, name: &str) -> LxResult<Arc<dyn Inode>> {
        let path = self.child(name);
        crate::fs::stat(&path, Credentials::kernel()).map_err(|_| ENOENT)?;
        Ok(Arc::new(NovaNode::new(path)))
    }

    fn readdir(&self) -> LxResult<Vec<DirEntry>> {
        let who = Credentials::kernel();
        let links = crate::fs::list(&self.path, who).map_err(|_| ENOTDIR)?;
        Ok(links
            .into_iter()
            .map(|link| {
                let child = self.child(&link.name);
                let kind = match crate::fs::stat(&child, who) {
                    Ok(node) if node.is_directory() => Kind::Dir,
                    _ => Kind::File,
                };
                DirEntry { name: link.name, ino: link.node, kind }
            })
            .collect())
    }

    fn pager(self: Arc<Self>) -> Option<Arc<dyn crate::vm::Pager>> {
        let node = self.node().ok()?;
        (!node.is_directory()).then(|| self as Arc<dyn crate::vm::Pager>)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl crate::vm::Pager for NovaNode {
    fn fill(&self, offset: u64, page: &mut [u8]) -> bool {
        let Ok(node) = self.node() else {
            return false;
        };
        if offset % PAGE == 0
            && page.len() == PAGE as usize
            && let Some(phys) = shared::lookup(node.id, offset / PAGE)
        {
            // Written through a shared mapping: the frame is the truth.
            let source = crate::memory::phys_to_virt(phys) as *const u8;
            // SAFETY: a shared frame, never freed.
            unsafe { core::ptr::copy_nonoverlapping(source, page.as_mut_ptr(), PAGE as usize) };
            return true;
        }
        if offset >= node.size {
            return true;
        }
        if offset % PAGE == 0
            && page.len() == PAGE as usize
            && let Some(phys) = cached_page(&node, offset / PAGE)
        {
            let source = crate::memory::phys_to_virt(phys) as *const u8;
            // SAFETY: a cache frame, never freed.
            unsafe { core::ptr::copy_nonoverlapping(source, page.as_mut_ptr(), PAGE as usize) };
            return true;
        }
        let count = ((node.size - offset) as usize).min(page.len());
        crate::fs::read_range_node(LETTER, node.id, offset, &mut page[..count]).is_ok()
    }

    fn fill_uncached(&self, offset: u64, page: &mut [u8]) -> bool {
        let Ok(node) = self.node() else {
            return false;
        };
        if offset >= node.size {
            return true;
        }
        let count = ((node.size - offset) as usize).min(page.len());
        crate::fs::read_range_node(LETTER, node.id, offset, &mut page[..count]).is_ok()
    }

    fn cached_frame(&self, offset: u64) -> Option<u64> {
        if offset % PAGE != 0 {
            return None;
        }
        let node = self.node().ok()?;
        if shared::lookup(node.id, offset / PAGE).is_some() {
            // The cache would show what the disk holds, not what the
            // shared mappings wrote: the private mapping copies instead.
            return None;
        }
        (offset < node.size).then(|| cached_page(&node, offset / PAGE))?
    }

    fn shared_frame(&self, offset: u64) -> Option<u64> {
        let node = self.node().ok()?;
        if node.is_directory() {
            return None;
        }
        shared::frame(node.id, node.size, offset / PAGE)
    }

    fn shares_read_only(&self) -> bool {
        self.id().is_ok_and(shared::has_any)
    }

    fn sync(&self) -> bool {
        crate::posix::vfs::Inode::sync(self).is_ok()
    }
}

//! Files in memory: `/tmp`, `/dev/shm`, `/home`, and `memfd_create`.
//!
//! A file is a list of frames, one per 4 KiB page, allocated on first
//! write. Holding frames rather than a byte vector is what lets `mmap`
//! with `MAP_SHARED` work: every mapping of a page installs **the same
//! frame** (`Pager::shared_frame`), so a write through one mapping is
//! seen through the others and through `read` -- the way Firefox shares
//! graphics buffers between its threads through memfds.
//!
//! A frame stays with its file until the file is truncated below it or
//! the last reference to the file goes; a mapping keeps the file alive
//! through its `Arc`, so unlinking a mapped file does not pull pages out
//! from under a process. Truncating a file that has been mapped shared
//! does not free the pages it cuts off either: a mapping may still hold
//! them (Linux pulls them out of every mapping; the `vm` keeps no reverse
//! map to do so). They are retired, and freed with the file -- after the
//! last mapping, and the TLB invalidation of its `munmap`, are gone.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::{DirEntry, Inode, Kind, S_IFDIR, S_IFLNK, S_IFREG, Stat};
use crate::frame::{self, Frame};
use crate::posix::errno::*;
use crate::vga::SpinLock;

static NEXT_INO: AtomicU64 = AtomicU64::new(1_000_000);
/// Bytes of file data held by every tmpfs together.
static BYTES: AtomicU64 = AtomicU64::new(0);

pub fn resident_bytes() -> u64 {
    BYTES.load(Ordering::Relaxed)
}

enum Content {
    File { size: u64, pages: Vec<Option<Frame>> },
    Dir(BTreeMap<String, Arc<TmpNode>>),
    Symlink(String),
}

struct Meta {
    mode: u32,
    uid: u32,
    gid: u32,
    mtime_ns: u64,
    nlink: u32,
}

pub struct TmpNode {
    ino: u64,
    meta: SpinLock<Meta>,
    content: SpinLock<Content>,
    /// Some page was mapped shared: a page cut off by a truncation may
    /// still be mapped, and is retired instead of freed.
    mapped: AtomicBool,
    /// Pages cut off while `mapped`, freed with the file.
    retired: SpinLock<Vec<Frame>>,
}

pub struct TmpFs;

impl TmpFs {
    pub fn new_root() -> Arc<dyn Inode> {
        TmpNode::new(Kind::Dir, 0o1777, String::new())
    }

    /// An unnamed file, for `memfd_create`.
    pub fn anonymous_file() -> Arc<TmpNode> {
        TmpNode::new(Kind::File, 0o600, String::new())
    }
}

fn now_ns() -> u64 {
    crate::posix::time::realtime_ns()
}

impl TmpNode {
    fn new(kind: Kind, mode: u32, target: String) -> Arc<TmpNode> {
        let content = match kind {
            Kind::Dir => Content::Dir(BTreeMap::new()),
            Kind::Symlink => Content::Symlink(target),
            _ => Content::File { size: 0, pages: Vec::new() },
        };
        Arc::new(TmpNode {
            ino: NEXT_INO.fetch_add(1, Ordering::Relaxed),
            meta: SpinLock::new(Meta { mode: mode & 0o7777, uid: 0, gid: 0, mtime_ns: now_ns(), nlink: 1 }),
            content: SpinLock::new(content),
            mapped: AtomicBool::new(false),
            retired: SpinLock::new(Vec::new()),
        })
    }

    fn kind(&self) -> Kind {
        match &*self.content.lock() {
            Content::File { .. } => Kind::File,
            Content::Dir(_) => Kind::Dir,
            Content::Symlink(_) => Kind::Symlink,
        }
    }

    fn touch(&self) {
        self.meta.lock().mtime_ns = now_ns();
    }

    pub fn size(&self) -> u64 {
        match &*self.content.lock() {
            Content::File { size, .. } => *size,
            _ => 0,
        }
    }

    /// Frame of page `index`, allocated (zeroed) if absent.
    fn page_frame(pages: &mut Vec<Option<Frame>>, index: usize) -> Option<Frame> {
        if pages.len() <= index {
            pages.resize(index + 1, None);
        }
        if pages[index].is_none() {
            pages[index] = Some(frame::allocate_zeroed_frame()?);
            BYTES.fetch_add(4096, Ordering::Relaxed);
        }
        pages[index]
    }
}

/// Frees the pages of a file from page `from` on.
///
/// # Safety
/// No mapping may reach them any more: the file was never mapped shared,
/// or it is being dropped (every mapping holds it alive, and `munmap`
/// lets it go only after invalidating the TLBs).
unsafe fn free_pages(pages: &mut Vec<Option<Frame>>, from: usize) {
    for slot in pages.iter_mut().skip(from) {
        if let Some(f) = slot.take() {
            // SAFETY: unreachable, per the contract above.
            unsafe { frame::deallocate_frame(f) };
            BYTES.fetch_sub(4096, Ordering::Relaxed);
        }
    }
    pages.truncate(from);
}

impl Drop for TmpNode {
    fn drop(&mut self) {
        if let Content::File { pages, .. } = &mut *self.content.lock() {
            // SAFETY: the file is going, so is every mapping of it.
            unsafe { free_pages(pages, 0) };
        }
        for f in self.retired.lock().drain(..) {
            // SAFETY: as above.
            unsafe { frame::deallocate_frame(f) };
            BYTES.fetch_sub(4096, Ordering::Relaxed);
        }
    }
}

impl Inode for TmpNode {
    fn stat(&self) -> Stat {
        let meta = self.meta.lock();
        let (type_bits, size) = match &*self.content.lock() {
            Content::File { size, .. } => (S_IFREG, *size),
            Content::Dir(_) => (S_IFDIR, 4096),
            Content::Symlink(t) => (S_IFLNK, t.len() as u64),
        };
        Stat {
            dev: super::device_of("tmpfs"),
            ino: self.ino,
            mode: type_bits | meta.mode,
            nlink: meta.nlink,
            uid: meta.uid,
            gid: meta.gid,
            rdev: 0,
            size,
            mtime_ns: meta.mtime_ns,
        }
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> LxResult<usize> {
        let content = self.content.lock();
        let Content::File { size, pages } = &*content else {
            return Err(if matches!(&*content, Content::Dir(_)) { EISDIR } else { EINVAL });
        };
        if offset >= *size {
            return Ok(0);
        }
        let count = ((*size - offset) as usize).min(buf.len());
        let mut done = 0;
        while done < count {
            let at = offset + done as u64;
            let page = (at / 4096) as usize;
            let inside = (at % 4096) as usize;
            let chunk = (4096 - inside).min(count - done);
            match pages.get(page).copied().flatten() {
                Some(f) => {
                    let src = (f.virt_addr() + inside as u64) as *const u8;
                    unsafe { core::ptr::copy_nonoverlapping(src, buf[done..].as_mut_ptr(), chunk) };
                }
                None => buf[done..done + chunk].fill(0),
            }
            done += chunk;
        }
        Ok(count)
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> LxResult<usize> {
        let mut content = self.content.lock();
        let Content::File { size, pages } = &mut *content else {
            return Err(EISDIR);
        };
        let end = offset.checked_add(buf.len() as u64).ok_or(EFBIG)?;
        if end > 1 << 40 {
            return Err(EFBIG);
        }
        let mut done = 0;
        while done < buf.len() {
            let at = offset + done as u64;
            let page = (at / 4096) as usize;
            let inside = (at % 4096) as usize;
            let chunk = (4096 - inside).min(buf.len() - done);
            let f = TmpNode::page_frame(pages, page).ok_or(ENOSPC)?;
            let dst = (f.virt_addr() + inside as u64) as *mut u8;
            unsafe { core::ptr::copy_nonoverlapping(buf[done..].as_ptr(), dst, chunk) };
            done += chunk;
        }
        if end > *size {
            *size = end;
        }
        drop(content);
        self.touch();
        Ok(buf.len())
    }

    fn truncate(&self, new_size: u64) -> LxResult<()> {
        let mut content = self.content.lock();
        let Content::File { size, pages } = &mut *content else {
            return Err(EISDIR);
        };
        if new_size > 1 << 40 {
            return Err(EFBIG);
        }
        if new_size < *size {
            let keep = new_size.div_ceil(4096) as usize;
            if self.mapped.load(Ordering::Acquire) {
                // A mapping may still hold them: retired, not freed.
                let mut retired = self.retired.lock();
                retired.extend(pages.iter_mut().skip(keep).filter_map(|slot| slot.take()));
                pages.truncate(keep);
            } else {
                // SAFETY: never mapped shared, so mapped nowhere.
                unsafe { free_pages(pages, keep) };
            }
            // Zero the tail of the last partial page: bytes past the end
            // must read zero if the file grows again.
            if new_size % 4096 != 0
                && let Some(Some(f)) = pages.get(keep - 1)
            {
                let inside = (new_size % 4096) as usize;
                unsafe { core::ptr::write_bytes((f.virt_addr() + inside as u64) as *mut u8, 0, 4096 - inside) };
            }
        }
        *size = new_size;
        drop(content);
        self.touch();
        Ok(())
    }

    fn lookup(&self, name: &str) -> LxResult<Arc<dyn Inode>> {
        match &*self.content.lock() {
            Content::Dir(entries) => entries.get(name).map(|n| n.clone() as Arc<dyn Inode>).ok_or(ENOENT),
            _ => Err(ENOTDIR),
        }
    }

    fn readdir(&self) -> LxResult<Vec<DirEntry>> {
        match &*self.content.lock() {
            Content::Dir(entries) => Ok(entries
                .iter()
                .map(|(name, n)| DirEntry { name: name.clone(), ino: n.ino, kind: n.kind() })
                .collect()),
            _ => Err(ENOTDIR),
        }
    }

    fn readlink(&self) -> LxResult<String> {
        match &*self.content.lock() {
            Content::Symlink(t) => Ok(t.clone()),
            _ => Err(EINVAL),
        }
    }

    fn create(&self, name: &str, kind: Kind, mode: u32, target: &str) -> LxResult<Arc<dyn Inode>> {
        let mut content = self.content.lock();
        let Content::Dir(entries) = &mut *content else {
            return Err(ENOTDIR);
        };
        if entries.contains_key(name) {
            return Err(EEXIST);
        }
        let node = TmpNode::new(kind, mode, target.to_string());
        if kind == Kind::Dir {
            node.meta.lock().nlink = 2;
        }
        entries.insert(name.to_string(), node.clone());
        drop(content);
        self.touch();
        Ok(node)
    }

    fn unlink(&self, name: &str, dir: bool) -> LxResult<()> {
        let mut content = self.content.lock();
        let Content::Dir(entries) = &mut *content else {
            return Err(ENOTDIR);
        };
        let node = entries.get(name).ok_or(ENOENT)?;
        let is_dir = node.kind() == Kind::Dir;
        if dir && !is_dir {
            return Err(ENOTDIR);
        }
        if !dir && is_dir {
            return Err(EISDIR);
        }
        if is_dir && matches!(&*node.content.lock(), Content::Dir(e) if !e.is_empty()) {
            return Err(ENOTEMPTY);
        }
        let node = entries.remove(name).unwrap();
        let mut meta = node.meta.lock();
        meta.nlink = meta.nlink.saturating_sub(1);
        drop(meta);
        drop(content);
        self.touch();
        Ok(())
    }

    fn rename(&self, old: &str, new_dir: &Arc<dyn Inode>, new: &str) -> LxResult<()> {
        let target = new_dir.as_any().downcast_ref::<TmpNode>().ok_or(EXDEV)?;
        if core::ptr::eq(self, target) {
            let mut content = self.content.lock();
            let Content::Dir(entries) = &mut *content else {
                return Err(ENOTDIR);
            };
            let node = entries.remove(old).ok_or(ENOENT)?;
            if let Some(existing) = entries.get(new)
                && existing.kind() == Kind::Dir
                && matches!(&*existing.content.lock(), Content::Dir(e) if !e.is_empty())
            {
                entries.insert(old.to_string(), node);
                return Err(ENOTEMPTY);
            }
            entries.insert(new.to_string(), node);
            return Ok(());
        }
        // Two directories: take the node out of one, into the other. The
        // locks are never held together, so no order can deadlock.
        let node = {
            let mut content = self.content.lock();
            let Content::Dir(entries) = &mut *content else {
                return Err(ENOTDIR);
            };
            entries.remove(old).ok_or(ENOENT)?
        };
        let mut content = target.content.lock();
        match &mut *content {
            Content::Dir(entries) => {
                entries.insert(new.to_string(), node);
                Ok(())
            }
            _ => {
                drop(content);
                if let Content::Dir(entries) = &mut *self.content.lock() {
                    entries.insert(old.to_string(), node);
                }
                Err(ENOTDIR)
            }
        }
    }

    fn link(&self, name: &str, target: &Arc<dyn Inode>) -> LxResult<()> {
        let node = target.clone().into_any().downcast::<TmpNode>().map_err(|_| EXDEV)?;
        if node.kind() == Kind::Dir {
            return Err(EPERM);
        }
        let mut content = self.content.lock();
        let Content::Dir(entries) = &mut *content else {
            return Err(ENOTDIR);
        };
        if entries.contains_key(name) {
            return Err(EEXIST);
        }
        node.meta.lock().nlink += 1;
        entries.insert(name.to_string(), node);
        Ok(())
    }

    fn chmod(&self, mode: u32) -> LxResult<()> {
        self.meta.lock().mode = mode & 0o7777;
        Ok(())
    }

    fn set_mtime(&self, ns: u64) -> LxResult<()> {
        self.meta.lock().mtime_ns = ns;
        Ok(())
    }

    fn pager(self: Arc<Self>) -> Option<Arc<dyn crate::vm::Pager>> {
        (self.kind() == Kind::File).then(|| self as Arc<dyn crate::vm::Pager>)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl crate::vm::Pager for TmpNode {
    fn fill(&self, offset: u64, page: &mut [u8]) -> bool {
        self.read_at(offset, page).is_ok()
    }

    fn shared_frame(&self, offset: u64) -> Option<u64> {
        self.mapped.store(true, Ordering::Release);
        let mut content = self.content.lock();
        let Content::File { pages, .. } = &mut *content else {
            return None;
        };
        // A shared mapping may extend past the end of the file (ftruncate
        // then mmap of the full size is the memfd idiom): the page exists
        // as soon as it is mapped.
        TmpNode::page_frame(pages, (offset / 4096) as usize).map(|f| f.start_addr())
    }
}

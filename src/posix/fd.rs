//! File descriptors and the objects behind them.
//!
//! Three levels, as in POSIX, because programs rely on the difference:
//!
//! * the **object** (`FileOps`): a file of the tree, a pipe, a socket, an
//!   epoll set. It knows how to read and write itself;
//! * the **open file description** (`Description`): one per `open`, shared
//!   by `dup` and inherited across `clone`. It carries the status flags --
//!   `O_NONBLOCK` set through one descriptor is seen through its
//!   duplicates, which is what event loops count on;
//! * the **descriptor**: a number in the process table, with its own
//!   `FD_CLOEXEC`.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU32, Ordering};

use super::errno::*;
use super::vfs::{DirEntry, Inode, Kind, Stat};
use super::wait::WaitQueue;
use crate::vga::SpinLock;

pub const O_ACCMODE: u32 = 3;
pub const O_RDONLY: u32 = 0;
pub const O_WRONLY: u32 = 1;
pub const O_RDWR: u32 = 2;
pub const O_CREAT: u32 = 0o100;
pub const O_EXCL: u32 = 0o200;
pub const O_NOCTTY: u32 = 0o400;
pub const O_TRUNC: u32 = 0o1000;
pub const O_APPEND: u32 = 0o2000;
pub const O_NONBLOCK: u32 = 0o4000;
pub const O_DIRECTORY: u32 = 0o200000;
pub const O_NOFOLLOW: u32 = 0o400000;
pub const O_CLOEXEC: u32 = 0o2000000;
pub const O_PATH: u32 = 0o10000000;
pub const O_TMPFILE: u32 = 0o20000000 | O_DIRECTORY;

pub const POLLIN: u32 = 0x001;
pub const POLLPRI: u32 = 0x002;
pub const POLLOUT: u32 = 0x004;
pub const POLLERR: u32 = 0x008;
pub const POLLHUP: u32 = 0x010;
pub const POLLNVAL: u32 = 0x020;
pub const POLLRDHUP: u32 = 0x2000;

/// An object a descriptor can name.
pub trait FileOps: Send + Sync {
    fn read(&self, _buf: &mut [u8], _nonblock: bool) -> LxResult<usize> {
        Err(EINVAL)
    }
    fn write(&self, _buf: &[u8], _nonblock: bool) -> LxResult<usize> {
        Err(EINVAL)
    }
    fn pread(&self, _offset: u64, _buf: &mut [u8]) -> LxResult<usize> {
        Err(ESPIPE)
    }
    fn pwrite(&self, _offset: u64, _buf: &[u8]) -> LxResult<usize> {
        Err(ESPIPE)
    }
    fn seek(&self, _offset: i64, _whence: u32) -> LxResult<u64> {
        Err(ESPIPE)
    }
    fn stat(&self) -> LxResult<Stat>;
    fn kind(&self) -> Kind;
    /// Current readiness (`POLLIN`, `POLLOUT`, `POLLHUP`...).
    fn poll(&self) -> u32 {
        POLLIN | POLLOUT
    }
    /// Where a waiter registers to hear about readiness changes. None
    /// for objects always ready.
    fn wait_queue(&self) -> Option<&WaitQueue> {
        None
    }
    fn ioctl(&self, _cmd: u64, _arg: u64) -> LxResult<u64> {
        Err(ENOTTY)
    }
    fn truncate(&self, _size: u64) -> LxResult<()> {
        Err(EINVAL)
    }
    /// Hands directory entries to `emit`, from the current position,
    /// until `emit` returns false (the caller's buffer is full). The
    /// position advances past every entry accepted.
    fn read_dir(&self, _emit: &mut dyn FnMut(&DirEntry) -> bool) -> LxResult<()> {
        Err(ENOTDIR)
    }
    fn inode(&self) -> Option<Arc<dyn Inode>> {
        None
    }
    /// What `/proc/self/fd/N` points at.
    fn describe(&self) -> String {
        String::from("anon_inode:[file]")
    }
    /// Notified when the status flags change (`fcntl(F_SETFL)`).
    fn set_status_flags(&self, _flags: u32) {}
    fn as_any(&self) -> &dyn Any {
        &()
    }
}

/// One `open`: the object and the status flags its duplicates share.
pub struct Description {
    pub file: Arc<dyn FileOps>,
    pub flags: AtomicU32,
}

impl Description {
    pub fn new(file: Arc<dyn FileOps>, flags: u32) -> Arc<Description> {
        Arc::new(Description { file, flags: AtomicU32::new(flags & !O_CLOEXEC) })
    }

    pub fn nonblock(&self) -> bool {
        self.flags.load(Ordering::Relaxed) & O_NONBLOCK != 0
    }

    pub fn access(&self) -> u32 {
        self.flags.load(Ordering::Relaxed) & O_ACCMODE
    }
}

#[derive(Clone)]
pub struct Entry {
    pub desc: Arc<Description>,
    pub cloexec: bool,
}

pub struct FdTable {
    slots: Vec<Option<Entry>>,
    pub limit: usize,
}

impl FdTable {
    pub fn new() -> Self {
        FdTable { slots: Vec::new(), limit: 4096 }
    }

    pub fn get(&self, fd: i64) -> LxResult<Entry> {
        if fd < 0 {
            return Err(EBADF);
        }
        self.slots.get(fd as usize).and_then(|s| s.clone()).ok_or(EBADF)
    }

    /// Lowest free descriptor at or above `min`.
    pub fn insert_from(&mut self, min: usize, entry: Entry) -> LxResult<i64> {
        let mut fd = min;
        while fd < self.slots.len() && self.slots[fd].is_some() {
            fd += 1;
        }
        if fd >= self.limit {
            return Err(EMFILE);
        }
        if fd >= self.slots.len() {
            self.slots.resize(fd + 1, None);
        }
        self.slots[fd] = Some(entry);
        Ok(fd as i64)
    }

    pub fn insert(&mut self, desc: Arc<Description>, cloexec: bool) -> LxResult<i64> {
        self.insert_from(0, Entry { desc, cloexec })
    }

    /// Installs at exactly `fd`, returning what was there (to be dropped
    /// by the caller outside the lock).
    pub fn install(&mut self, fd: i64, entry: Entry) -> LxResult<Option<Entry>> {
        if fd < 0 || fd as usize >= self.limit {
            return Err(EBADF);
        }
        let fd = fd as usize;
        if fd >= self.slots.len() {
            self.slots.resize(fd + 1, None);
        }
        Ok(self.slots[fd].replace(entry))
    }

    pub fn remove(&mut self, fd: i64) -> LxResult<Entry> {
        if fd < 0 {
            return Err(EBADF);
        }
        self.slots.get_mut(fd as usize).and_then(|s| s.take()).ok_or(EBADF)
    }

    pub fn set_cloexec(&mut self, fd: i64, value: bool) -> LxResult<()> {
        match self.slots.get_mut(fd as usize).and_then(|s| s.as_mut()) {
            Some(e) if fd >= 0 => {
                e.cloexec = value;
                Ok(())
            }
            _ => Err(EBADF),
        }
    }

    pub fn list(&self) -> Vec<(i64, Entry)> {
        self.slots.iter().enumerate().filter_map(|(i, s)| s.clone().map(|e| (i as i64, e))).collect()
    }

    pub fn count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    /// Copy for `fork`: the same descriptions, shared, flags included.
    pub fn clone_table(&self) -> FdTable {
        FdTable { slots: self.slots.clone(), limit: self.limit }
    }

    /// Closes the close-on-exec descriptors (`execve`); returns them to
    /// be dropped outside any lock.
    pub fn close_on_exec(&mut self) -> Vec<Entry> {
        let mut closed = Vec::new();
        for slot in self.slots.iter_mut() {
            if slot.as_ref().is_some_and(|e| e.cloexec) {
                closed.push(slot.take().unwrap());
            }
        }
        closed
    }
}

impl Default for FdTable {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================
//  A file or directory of the tree
// ============================================================

pub struct OpenFile {
    pub inode: Arc<dyn Inode>,
    pub path: String,
    offset: SpinLock<u64>,
    append: AtomicU32,
    /// Directory listing taken at the first `getdents`, and the position
    /// in it: a directory modified while being listed shows the listing
    /// of the moment it was opened, which POSIX allows.
    listing: SpinLock<Option<(Vec<DirEntry>, usize)>>,
}

impl OpenFile {
    pub fn new(inode: Arc<dyn Inode>, path: String, flags: u32) -> Arc<OpenFile> {
        Arc::new(OpenFile {
            inode,
            path,
            offset: SpinLock::new(0),
            append: AtomicU32::new(flags & O_APPEND),
            listing: SpinLock::new(None),
        })
    }
}

impl FileOps for OpenFile {
    // The offset lock is not held across the transfer: it masks
    // interrupts, and a read of the root image can last many disk
    // commands. Two threads reading through one description concurrently
    // may then read the same bytes -- POSIX leaves that case unspecified.
    fn read(&self, buf: &mut [u8], _nonblock: bool) -> LxResult<usize> {
        let at = *self.offset.lock();
        let n = self.inode.read_at(at, buf)?;
        *self.offset.lock() = at + n as u64;
        Ok(n)
    }
    fn write(&self, buf: &[u8], _nonblock: bool) -> LxResult<usize> {
        let at = if self.append.load(Ordering::Relaxed) != 0 { self.inode.stat().size } else { *self.offset.lock() };
        let n = self.inode.write_at(at, buf)?;
        *self.offset.lock() = at + n as u64;
        Ok(n)
    }
    fn pread(&self, offset: u64, buf: &mut [u8]) -> LxResult<usize> {
        self.inode.read_at(offset, buf)
    }
    fn pwrite(&self, offset: u64, buf: &[u8]) -> LxResult<usize> {
        self.inode.write_at(offset, buf)
    }
    fn seek(&self, off: i64, whence: u32) -> LxResult<u64> {
        let mut offset = self.offset.lock();
        let base = match whence {
            0 => 0i64,
            1 => *offset as i64,
            2 => self.inode.stat().size as i64,
            // SEEK_DATA / SEEK_HOLE: the whole file is data.
            3 => off,
            4 => self.inode.stat().size as i64,
            _ => return Err(EINVAL),
        };
        let target = if whence >= 3 { base } else { base.checked_add(off).ok_or(EOVERFLOW)? };
        if target < 0 {
            return Err(EINVAL);
        }
        *offset = target as u64;
        if whence == 0 && self.inode.stat().kind() == Kind::Dir {
            *self.listing.lock() = None;
        }
        Ok(target as u64)
    }
    fn stat(&self) -> LxResult<Stat> {
        Ok(self.inode.stat())
    }
    fn kind(&self) -> Kind {
        self.inode.stat().kind()
    }
    fn truncate(&self, size: u64) -> LxResult<()> {
        self.inode.truncate(size)
    }
    fn read_dir(&self, emit: &mut dyn FnMut(&DirEntry) -> bool) -> LxResult<()> {
        let mut listing = self.listing.lock();
        if listing.is_none() {
            let mut entries = self.inode.readdir()?;
            let st = self.inode.stat();
            entries.insert(0, DirEntry { name: String::from("."), ino: st.ino, kind: Kind::Dir });
            entries.insert(1, DirEntry { name: String::from(".."), ino: st.ino, kind: Kind::Dir });
            *listing = Some((entries, 0));
        }
        let (entries, position) = listing.as_mut().unwrap();
        while *position < entries.len() {
            if !emit(&entries[*position]) {
                break;
            }
            *position += 1;
        }
        Ok(())
    }
    fn inode(&self) -> Option<Arc<dyn Inode>> {
        Some(self.inode.clone())
    }
    fn describe(&self) -> String {
        self.path.clone()
    }
    fn set_status_flags(&self, flags: u32) {
        self.append.store(flags & O_APPEND, Ordering::Relaxed);
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ============================================================
//  The console: standard descriptors of a program started by Nova
// ============================================================

/// Writes go to the serial console, prefixed per line with the program's
/// name; reads find end of file. A Linux program started from Nova has
/// no terminal, and blocking it on a read nobody can satisfy would be
/// worse than telling it there is nothing.
pub struct Console;

impl FileOps for Console {
    fn read(&self, _buf: &mut [u8], _nonblock: bool) -> LxResult<usize> {
        Ok(0)
    }
    fn write(&self, buf: &[u8], _nonblock: bool) -> LxResult<usize> {
        super::console_write(buf);
        Ok(buf.len())
    }
    fn stat(&self) -> LxResult<Stat> {
        Ok(Stat { mode: super::vfs::S_IFCHR | 0o620, rdev: 0x8800, ..Default::default() })
    }
    fn kind(&self) -> Kind {
        Kind::CharDev
    }
    fn ioctl(&self, cmd: u64, _arg: u64) -> LxResult<u64> {
        // TCGETS and friends: not a terminal. Programs (musl's stdio
        // among them) take ENOTTY as "buffer fully, no line discipline".
        let _ = cmd;
        Err(ENOTTY)
    }
    fn describe(&self) -> String {
        String::from("/dev/console")
    }
}

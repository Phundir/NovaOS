//! Memory calls: `mmap`, `munmap`, `mprotect`, `mremap`, `madvise`, `brk`.
//!
//! Thin translations onto `vm::Space`: a region per mapping, pages on
//! first touch. The two decisions worth naming:
//!
//! * `MAP_SHARED` is shared for real when the object can give each page
//!   one frame: a tmpfs file, a memfd, and a file of the Nova volume
//!   mapped writable (`novafs::shared`: SQLite's `-shm` index). A
//!   read-only shared view of a volume file that nobody maps writable
//!   maps the page cache instead, as a private mapping would: the
//!   difference shows only if the file is written meanwhile, and it
//!   spares a pinned frame per page of the archives the browser maps
//!   (omni.ja). A writable shared mapping of an object that cannot share
//!   is refused (`EACCES`).
//! * Anonymous `MAP_SHARED` memory is private: after a `fork` the child
//!   gets a copy-on-write copy, not a shared view. Programs that share
//!   memory between processes (the browser) do it with a memfd.

use alloc::sync::Arc;

use super::errno::*;
use super::Thread;
use crate::vm::{Backing, PAGE, Prot, VmError};

pub const PROT_READ: u64 = 1;
pub const PROT_WRITE: u64 = 2;
pub const PROT_EXEC: u64 = 4;

const MAP_SHARED: u64 = 0x01;
const MAP_PRIVATE: u64 = 0x02;
const MAP_SHARED_VALIDATE: u64 = 0x03;
const MAP_FIXED: u64 = 0x10;
const MAP_ANONYMOUS: u64 = 0x20;
const MAP_POPULATE: u64 = 0x8000;
const MAP_FIXED_NOREPLACE: u64 = 0x100000;

const MREMAP_MAYMOVE: u64 = 1;
const MREMAP_FIXED: u64 = 2;

fn prot(bits: u64) -> LxResult<Prot> {
    if bits & !(PROT_READ | PROT_WRITE | PROT_EXEC) & !0x0100_0000 & !0x0200_0000 != 0 {
        return Err(EINVAL);
    }
    Ok(Prot((bits & 7) as u8))
}

fn vm_err(e: VmError) -> i64 {
    match e {
        VmError::Invalid => EINVAL,
        VmError::Exists => EEXIST,
        VmError::NoMemory => ENOMEM,
        VmError::Fault => EFAULT,
    }
}

pub fn mmap(t: &Arc<Thread>, addr: u64, len: u64, prot_bits: u64, flags: u64, fd: i64, offset: u64) -> LxResult<u64> {
    if len == 0 || offset % PAGE != 0 {
        return Err(EINVAL);
    }
    let sharing = flags & 3;
    if sharing != MAP_SHARED && sharing != MAP_PRIVATE && sharing != MAP_SHARED_VALIDATE {
        return Err(EINVAL);
    }
    let p = prot(prot_bits)?;
    let space = &t.process.space;

    let (backing, name) = if flags & MAP_ANONYMOUS != 0 {
        (Backing::Anon, alloc::string::String::new())
    } else {
        let entry = t.process.files.lock().get(fd)?;
        let inode = entry.desc.file.inode().ok_or(ENODEV)?;
        if inode.stat().kind() != super::vfs::Kind::File {
            return Err(ENODEV);
        }
        let access = entry.desc.access();
        if access == super::fd::O_WRONLY {
            return Err(EACCES);
        }
        let shared_writable = sharing != MAP_PRIVATE && p.writable();
        if shared_writable && access != super::fd::O_RDWR {
            return Err(EACCES);
        }
        let pager = inode.pager().ok_or(ENODEV)?;
        let shared = sharing != MAP_PRIVATE
            && (p.writable() || pager.shares_read_only())
            && pager.shared_frame(offset).is_some();
        if shared_writable && !shared {
            // A read-only file behind a writable shared mapping.
            return Err(EACCES);
        }
        let path = entry.desc.file.describe();
        let name = path.rsplit('/').next().unwrap_or("").into();
        (Backing::File { pager, offset, shared }, name)
    };

    let result = if flags & MAP_FIXED_NOREPLACE != 0 {
        space.map_exact(addr, len, p, backing, &name)
    } else if flags & MAP_FIXED != 0 {
        space.map(addr, len, p, backing, true, &name)
    } else {
        space.map(addr, len, p, backing, false, &name)
    };
    let at = result.map_err(vm_err)?;
    if flags & MAP_POPULATE != 0 {
        let _ = space.populate(at, len, false);
    }
    Ok(at)
}

pub fn munmap(t: &Arc<Thread>, addr: u64, len: u64) -> LxResult<u64> {
    t.process.space.unmap(addr, len).map_err(vm_err)?;
    Ok(0)
}

/// `msync`: the objects behind the shared mappings of the range get
/// back what was written through them.
pub fn msync(t: &Arc<Thread>, addr: u64, len: u64) -> LxResult<u64> {
    if addr % PAGE != 0 {
        return Err(EINVAL);
    }
    let end = addr.checked_add(len).ok_or(ENOMEM)?;
    for pager in t.process.space.shared_pagers(addr, end) {
        if !pager.sync() {
            return Err(EIO);
        }
    }
    Ok(0)
}

pub fn mprotect(t: &Arc<Thread>, addr: u64, len: u64, prot_bits: u64) -> LxResult<u64> {
    let p = prot(prot_bits)?;
    t.process.space.protect(addr, len, p).map_err(vm_err)?;
    Ok(0)
}

pub fn mremap(t: &Arc<Thread>, old: u64, old_len: u64, new_len: u64, flags: u64, new_addr: u64) -> LxResult<u64> {
    if flags & MREMAP_FIXED != 0 {
        // Moving to a chosen address: unmap the target, then move.
        if flags & MREMAP_MAYMOVE == 0 {
            return Err(EINVAL);
        }
        let space = &t.process.space;
        let moved = space.remap(old, old_len, new_len, true).map_err(vm_err)?;
        if moved == new_addr {
            return Ok(moved);
        }
        // Rare: copy through a fresh fixed mapping would be needed.
        let _ = new_addr;
        return Ok(moved);
    }
    t.process.space.remap(old, old_len, new_len, flags & MREMAP_MAYMOVE != 0).map_err(vm_err)
}

pub fn madvise(t: &Arc<Thread>, addr: u64, len: u64, advice: u64) -> LxResult<u64> {
    const MADV_DONTNEED: u64 = 4;
    const MADV_FREE: u64 = 8;
    if addr % PAGE != 0 {
        return Err(EINVAL);
    }
    match advice {
        MADV_DONTNEED | MADV_FREE => {
            t.process.space.discard(addr, len).map_err(vm_err)?;
            Ok(0)
        }
        // Hints: sequential, random, willneed, hugepage, dontfork...
        _ => Ok(0),
    }
}

/// `brk(addr)`: the program break is a region grown and shrunk at its
/// end. Returns the new break, or the old one if the request fails --
/// the raw call never returns an error, callers compare.
pub fn brk(t: &Arc<Thread>, addr: u64) -> LxResult<u64> {
    // The lock is not held across the mapping change: unmapping waits
    // for every core to drop its translations, which a core spinning on
    // this lock with interrupts masked could never do.
    let (start, end) = {
        let brk = t.process.brk.lock();
        (brk.start, brk.end)
    };
    if addr < start {
        return Ok(end);
    }
    let old_top = (end + PAGE - 1) & !(PAGE - 1);
    let new_top = (addr + PAGE - 1) & !(PAGE - 1);
    let space = &t.process.space;
    if new_top > old_top {
        if space.map_exact(old_top, new_top - old_top, Prot::RW, Backing::Anon, "[heap]").is_err() {
            return Ok(end);
        }
    } else if new_top < old_top && space.unmap(new_top, old_top - new_top).is_err() {
        return Ok(end);
    }
    t.process.brk.lock().end = addr;
    Ok(addr)
}

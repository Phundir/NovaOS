//! File calls: opening, reading, writing, naming, inspecting.
//!
//! Paths are resolved by `vfs::walk` from the process's working directory
//! or from a directory descriptor (`*at` calls, `AT_FDCWD` = -100).
//! User buffers are copied through `vm::copy_*_user` in chunks of at most
//! 1 MiB, so a large `read` never needs a large kernel allocation.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::errno::*;
use super::fd::{self, Description, FileOps, OpenFile};
use super::vfs::{self, Inode, Kind, Stat};
use super::Thread;

pub const AT_FDCWD: i64 = -100;
const AT_SYMLINK_NOFOLLOW: u64 = 0x100;
const AT_REMOVEDIR: u64 = 0x200;
const AT_SYMLINK_FOLLOW: u64 = 0x400;
const AT_EMPTY_PATH: u64 = 0x1000;

/// Largest single copy between user memory and a kernel buffer.
const CHUNK: usize = 256 << 10;

/// A zeroed kernel buffer for data a program asked to move.
///
/// Its size comes from the program, and the kernel heap (64 MiB) is the
/// whole system's: an allocation the heap cannot satisfy used to reach
/// `alloc_error`, which stops the kernel -- the browser's network "cut",
/// with `ALLOCATION ECHOUEE : 5898240 octets` on screen, was a 5.6 MiB
/// IPC message received in one piece. Here a refusal is `ENOMEM` for
/// that one call. The callers also bound the size (`CHUNK`, the socket
/// buffer): a copy goes through the kernel in pieces, never whole.
pub fn user_buffer(len: usize) -> LxResult<Vec<u8>> {
    let mut data = Vec::new();
    data.try_reserve_exact(len).map_err(|_| ENOMEM)?;
    data.resize(len, 0);
    Ok(data)
}

/// Total length of a list of iovecs.
pub fn iov_total(iov: &[(u64, u64)]) -> u64 {
    iov.iter().fold(0u64, |sum, (_, len)| sum.saturating_add(*len))
}

/// At most `max` bytes of what the iovecs describe, starting `skip`
/// bytes in: one piece of a large write, copied without the rest.
pub fn gather_range(iov: &[(u64, u64)], skip: u64, max: usize) -> LxResult<Vec<u8>> {
    let len = iov_total(iov).saturating_sub(skip).min(max as u64) as usize;
    let mut data = user_buffer(len)?;
    let (mut at, mut filled) = (0u64, 0usize);
    for &(base, seg) in iov {
        let (start, end) = (at, at.saturating_add(seg));
        at = end;
        if end <= skip || seg == 0 {
            continue;
        }
        if filled == len {
            break;
        }
        let from = skip.max(start) - start;
        let n = ((seg - from) as usize).min(len - filled);
        crate::vm::copy_from_user(&mut data[filled..filled + n], base + from).map_err(|_| EFAULT)?;
        filled += n;
    }
    Ok(data)
}

pub fn read_path(addr: u64) -> LxResult<String> {
    let bytes = crate::vm::read_user_cstr(addr, vfs::PATH_MAX).map_err(|e| if e == crate::vm::VmError::Invalid { ENAMETOOLONG } else { EFAULT })?;
    String::from_utf8(bytes).map_err(|_| EINVAL)
}

/// Directory a relative path starts from: the working directory, or the
/// directory named by `dirfd`.
fn base_dir(t: &Arc<Thread>, dirfd: i64, path: &str) -> LxResult<String> {
    if path.starts_with('/') {
        return Ok(String::from("/"));
    }
    if dirfd == AT_FDCWD {
        return Ok(t.process.cwd());
    }
    let entry = t.process.files.lock().get(dirfd)?;
    let file = entry.desc.file.clone();
    let open = file.as_any().downcast_ref::<OpenFile>().ok_or(ENOTDIR)?;
    if open.inode.stat().kind() != Kind::Dir {
        return Err(ENOTDIR);
    }
    Ok(open.path.clone())
}

fn resolve(t: &Arc<Thread>, dirfd: i64, path: &str, follow: bool) -> LxResult<vfs::Found> {
    let base = base_dir(t, dirfd, path)?;
    vfs::walk(&base, path, follow)
}

fn resolve_parent(t: &Arc<Thread>, dirfd: i64, path: &str) -> LxResult<vfs::Parent> {
    let base = base_dir(t, dirfd, path)?;
    vfs::walk_parent(&base, path)
}

fn file_of(t: &Arc<Thread>, fd: i64) -> LxResult<Arc<Description>> {
    Ok(t.process.files.lock().get(fd)?.desc)
}

// ============================================================
//  Opening and closing
// ============================================================

pub fn openat(t: &Arc<Thread>, dirfd: i64, path_addr: u64, flags: u64, mode: u64) -> LxResult<u64> {
    let path = read_path(path_addr)?;
    let flags = flags as u32;
    let mode = mode as u32 & 0o7777 & !t.process.umask.load(core::sync::atomic::Ordering::Relaxed);
    let follow = flags & fd::O_NOFOLLOW == 0;

    if flags & fd::O_TMPFILE == fd::O_TMPFILE {
        let dir = resolve(t, dirfd, &path, true)?;
        if dir.inode.stat().kind() != Kind::Dir {
            return Err(ENOTDIR);
        }
        // An unnamed file: a tmpfs node nobody links.
        let node: Arc<dyn Inode> = vfs::tmpfs::TmpFs::anonymous_file();
        let file = OpenFile::new(node, alloc::format!("{}/#{} (deleted)", dir.path, t.tid), flags);
        return install(t, file, flags);
    }

    let found = match resolve(t, dirfd, &path, follow) {
        Ok(f) => {
            if flags & fd::O_CREAT != 0 && flags & fd::O_EXCL != 0 {
                return Err(EEXIST);
            }
            f
        }
        Err(ENOENT) if flags & fd::O_CREAT != 0 => {
            let parent = resolve_parent(t, dirfd, &path)?;
            let inode = parent.dir.create(&parent.name, Kind::File, mode, "")?;
            let path = if parent.dir_path == "/" { alloc::format!("/{}", parent.name) } else { alloc::format!("{}/{}", parent.dir_path, parent.name) };
            vfs::Found { inode, path }
        }
        Err(e) => return Err(e),
    };
    let st = found.inode.stat();
    if st.kind() == Kind::Symlink {
        // O_NOFOLLOW on a link (O_PATH | O_NOFOLLOW would open it).
        if flags & fd::O_PATH == 0 {
            return Err(ELOOP);
        }
    }
    if flags & fd::O_DIRECTORY != 0 && st.kind() != Kind::Dir {
        return Err(ENOTDIR);
    }
    let access = flags & fd::O_ACCMODE;
    if st.kind() == Kind::Dir && access != fd::O_RDONLY {
        return Err(EISDIR);
    }
    if flags & fd::O_PATH == 0
        && let Some(special) = found.inode.clone().open_special()
    {
        return install(t, special?, flags);
    }
    if flags & fd::O_TRUNC != 0 && access != fd::O_RDONLY && st.kind() == Kind::File {
        found.inode.truncate(0)?;
    }
    let file = OpenFile::new(found.inode, found.path, flags);
    install(t, file, flags)
}

fn install(t: &Arc<Thread>, file: Arc<dyn FileOps>, flags: u32) -> LxResult<u64> {
    let desc = Description::new(file, flags);
    t.process.files.lock().insert(desc, flags & fd::O_CLOEXEC != 0).map(|fd| fd as u64)
}

pub fn close(t: &Arc<Thread>, fd: i64) -> LxResult<u64> {
    let entry = t.process.files.lock().remove(fd)?;
    // Dropped here, outside the table lock: closing the last end of a
    // pipe wakes its readers.
    drop(entry);
    Ok(0)
}

pub fn close_range(t: &Arc<Thread>, first: u64, last: u64, flags: u64) -> LxResult<u64> {
    const CLOSE_RANGE_CLOEXEC: u64 = 4;
    let list = t.process.files.lock().list();
    let mut dropped = Vec::new();
    for (fd, _) in list {
        if (fd as u64) < first || (fd as u64) > last {
            continue;
        }
        let mut table = t.process.files.lock();
        if flags & CLOSE_RANGE_CLOEXEC != 0 {
            let _ = table.set_cloexec(fd, true);
        } else if let Ok(e) = table.remove(fd) {
            dropped.push(e);
        }
    }
    drop(dropped);
    Ok(0)
}

// ============================================================
//  Reading and writing
// ============================================================

/// Reads up to `len` bytes into the program's memory at `buf`, at the file
/// position or at `offset`, through one bounce buffer of at most `CHUNK`.
///
/// A regular file is read to the full length or to its end, piece after
/// piece: POSIX allows a short read there, Linux never gives one, and
/// programs rely on it -- the Web Browser's media reader, handed 256 KiB
/// of a larger request, showed no image for the first four seconds of a
/// video. A pipe, a socket or a device returns what one read gives, as on
/// Linux.
fn read_to_user(desc: &Description, buf: u64, len: u64, offset: Option<u64>) -> LxResult<u64> {
    let whole = desc.file.kind() == Kind::File;
    let mut bounce = user_buffer((len as usize).min(CHUNK))?;
    let mut done = 0u64;
    while done < len {
        let n = ((len - done) as usize).min(bounce.len());
        let result = match offset {
            Some(at) => desc.file.pread(at + done, &mut bounce[..n]),
            None => desc.file.read(&mut bounce[..n], desc.nonblock()),
        };
        let got = match result {
            Ok(got) => got,
            Err(e) if done == 0 => return Err(e),
            Err(_) => break,
        };
        crate::vm::copy_to_user(buf + done, &bounce[..got]).map_err(|_| EFAULT)?;
        done += got as u64;
        if got < n || !whole {
            break;
        }
    }
    Ok(done)
}

pub fn read(t: &Arc<Thread>, fd: i64, buf: u64, len: u64) -> LxResult<u64> {
    let desc = file_of(t, fd)?;
    if desc.access() == fd::O_WRONLY {
        return Err(EBADF);
    }
    read_to_user(&desc, buf, len, None)
}

pub fn write(t: &Arc<Thread>, fd: i64, buf: u64, len: u64) -> LxResult<u64> {
    let desc = file_of(t, fd)?;
    if desc.access() == fd::O_RDONLY && desc.file.kind() == Kind::File {
        return Err(EBADF);
    }
    let mut done = 0u64;
    while done < len {
        let n = ((len - done) as usize).min(CHUNK);
        let mut kernel = user_buffer(n)?;
        crate::vm::copy_from_user(&mut kernel, buf + done).map_err(|_| EFAULT)?;
        match desc.file.write(&kernel, desc.nonblock()) {
            Ok(w) => {
                done += w as u64;
                if w < n {
                    break;
                }
            }
            Err(e) if done == 0 => return Err(e),
            Err(_) => break,
        }
    }
    Ok(done)
}

pub fn pread(t: &Arc<Thread>, fd: i64, buf: u64, len: u64, offset: i64) -> LxResult<u64> {
    if offset < 0 {
        return Err(EINVAL);
    }
    let desc = file_of(t, fd)?;
    read_to_user(&desc, buf, len, Some(offset as u64))
}

pub fn pwrite(t: &Arc<Thread>, fd: i64, buf: u64, len: u64, offset: i64) -> LxResult<u64> {
    if offset < 0 {
        return Err(EINVAL);
    }
    let desc = file_of(t, fd)?;
    // Piece after piece, like `write`: a file is written whole.
    let mut done = 0u64;
    while done < len {
        let n = ((len - done) as usize).min(CHUNK);
        let mut kernel = user_buffer(n)?;
        crate::vm::copy_from_user(&mut kernel, buf + done).map_err(|_| EFAULT)?;
        match desc.file.pwrite(offset as u64 + done, &kernel) {
            Ok(w) => {
                done += w as u64;
                if w < n {
                    break;
                }
            }
            Err(e) if done == 0 => return Err(e),
            Err(_) => break,
        }
    }
    Ok(done)
}

fn iovecs(addr: u64, count: u64) -> LxResult<Vec<(u64, u64)>> {
    if count > 1024 {
        return Err(EINVAL);
    }
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let pair: [u64; 2] = crate::vm::read_user(addr + i * 16).map_err(|_| EFAULT)?;
        out.push((pair[0], pair[1]));
    }
    Ok(out)
}

/// `readv`, `preadv`, `preadv2` (offset -1: the file position).
pub fn readv(t: &Arc<Thread>, fd: i64, iov: u64, count: u64, offset: i64) -> LxResult<u64> {
    let desc = file_of(t, fd)?;
    let mut total = 0u64;
    for (base, len) in iovecs(iov, count)? {
        if len == 0 {
            continue;
        }
        let at = (offset >= 0).then(|| offset as u64 + total);
        let n = match read_to_user(&desc, base, len, at) {
            Ok(n) => n,
            Err(e) if total == 0 => return Err(e),
            Err(_) => break,
        };
        total += n;
        if n < len {
            break;
        }
    }
    Ok(total)
}

/// `writev`, `pwritev`, `pwritev2` (offset -1: the file position).
pub fn writev(t: &Arc<Thread>, fd: i64, iov: u64, count: u64, offset: i64) -> LxResult<u64> {
    let desc = file_of(t, fd)?;
    let iov = iovecs(iov, count)?;
    // Gathered a piece at a time. A piece is `CHUNK` bytes: a small write
    // stays whole on a pipe or a socket, as writev guarantees; a large one
    // goes through the kernel heap in pieces (it once went whole, up to
    // 64 MiB), and stops at the first short write, as `write` does.
    let total = iov_total(&iov);
    let mut done = 0u64;
    while done < total {
        let data = gather_range(&iov, done, CHUNK)?;
        let result = if offset >= 0 {
            desc.file.pwrite(offset as u64 + done, &data)
        } else {
            desc.file.write(&data, desc.nonblock())
        };
        let n = match result {
            Ok(n) => n,
            Err(e) if done == 0 => return Err(e),
            Err(_) => break,
        };
        done += n as u64;
        if n < data.len() {
            break;
        }
    }
    Ok(done)
}

pub fn lseek(t: &Arc<Thread>, fd: i64, offset: i64, whence: u64) -> LxResult<u64> {
    file_of(t, fd)?.file.seek(offset, whence as u32)
}

// ============================================================
//  Attributes
// ============================================================

fn stat_to_user(st: Stat, addr: u64) -> LxResult<u64> {
    crate::vm::copy_to_user(addr, &st.to_linux()).map_err(|_| EFAULT)?;
    Ok(0)
}

fn stat_of(t: &Arc<Thread>, dirfd: i64, path_addr: u64, flags: u64) -> LxResult<Stat> {
    let path = if path_addr == 0 { String::new() } else { read_path(path_addr)? };
    if path.is_empty() {
        if flags & AT_EMPTY_PATH == 0 {
            return Err(ENOENT);
        }
        if dirfd == AT_FDCWD {
            return Ok(vfs::walk("/", &t.process.cwd(), true)?.inode.stat());
        }
        return file_of(t, dirfd)?.file.stat();
    }
    Ok(resolve(t, dirfd, &path, flags & AT_SYMLINK_NOFOLLOW == 0)?.inode.stat())
}

pub fn fstat(t: &Arc<Thread>, fd: i64, out: u64) -> LxResult<u64> {
    stat_to_user(file_of(t, fd)?.file.stat()?, out)
}

pub fn fstatat(t: &Arc<Thread>, dirfd: i64, path: u64, out: u64, flags: u64) -> LxResult<u64> {
    stat_to_user(stat_of(t, dirfd, path, flags)?, out)
}

pub fn statx(t: &Arc<Thread>, dirfd: i64, path: u64, flags: u64, _mask: u64, out: u64) -> LxResult<u64> {
    let st = stat_of(t, dirfd, path, flags)?;
    let mut b = [0u8; 256];
    let put32 = |b: &mut [u8; 256], at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let put64 = |b: &mut [u8; 256], at: usize, v: u64| b[at..at + 8].copy_from_slice(&v.to_le_bytes());
    put32(&mut b, 0, 0x7FF); // STATX_BASIC_STATS
    put32(&mut b, 4, 4096);
    put32(&mut b, 16, st.nlink.max(1));
    put32(&mut b, 20, st.uid);
    put32(&mut b, 24, st.gid);
    b[28..30].copy_from_slice(&(st.mode as u16).to_le_bytes());
    put64(&mut b, 32, st.ino);
    put64(&mut b, 40, st.size);
    put64(&mut b, 48, st.size.div_ceil(512));
    let sec = st.mtime_ns / 1_000_000_000;
    let nsec = (st.mtime_ns % 1_000_000_000) as u32;
    for at in [64usize, 80, 96, 112] {
        put64(&mut b, at, sec);
        put32(&mut b, at + 8, nsec);
    }
    put32(&mut b, 128, (st.rdev >> 8) as u32);
    put32(&mut b, 132, (st.rdev & 0xFF) as u32);
    put32(&mut b, 136, (st.dev >> 8) as u32);
    put32(&mut b, 140, (st.dev & 0xFF) as u32);
    crate::vm::copy_to_user(out, &b).map_err(|_| EFAULT)?;
    Ok(0)
}

pub fn faccessat(t: &Arc<Thread>, dirfd: i64, path: u64, mode: u64, flags: u64) -> LxResult<u64> {
    let st = stat_of(t, dirfd, path, flags)?;
    const X_OK: u64 = 1;
    // Running as root: read and write granted, execute when a bit says so.
    if mode & X_OK != 0 && st.kind() != Kind::Dir && st.mode & 0o111 == 0 {
        return Err(EACCES);
    }
    // The Linux root is a directory of the Nova volume, which is
    // writable: what used to be refused here (a read-only tar image) is
    // now a question of Nova permissions, answered by the write itself.
    Ok(0)
}

pub fn readlinkat(t: &Arc<Thread>, dirfd: i64, path: u64, buf: u64, size: u64) -> LxResult<u64> {
    let p = read_path(path)?;
    let found = resolve(t, dirfd, &p, false)?;
    let target = found.inode.readlink()?;
    let n = target.len().min(size as usize);
    crate::vm::copy_to_user(buf, &target.as_bytes()[..n]).map_err(|_| EFAULT)?;
    Ok(n as u64)
}

pub fn getdents64(t: &Arc<Thread>, fd: i64, buf: u64, size: u64) -> LxResult<u64> {
    let desc = file_of(t, fd)?;
    let mut out: Vec<u8> = Vec::new();
    let limit = size.min(CHUNK as u64) as usize;
    let mut offset = 0i64;
    desc.file.read_dir(&mut |e| {
        let name = e.name.as_bytes();
        let reclen = (19 + name.len() + 1 + 7) & !7;
        if out.len() + reclen > limit {
            return false;
        }
        offset += 1;
        let start = out.len();
        out.resize(start + reclen, 0);
        out[start..start + 8].copy_from_slice(&e.ino.to_le_bytes());
        out[start + 8..start + 16].copy_from_slice(&offset.to_le_bytes());
        out[start + 16..start + 18].copy_from_slice(&(reclen as u16).to_le_bytes());
        out[start + 18] = e.kind.dirent_type();
        out[start + 19..start + 19 + name.len()].copy_from_slice(name);
        true
    })?;
    if out.is_empty() && limit < 280 {
        // Not even one entry fitted, but some remain.
        let mut any = false;
        let _ = desc.file.read_dir(&mut |_| {
            any = true;
            false
        });
        if any {
            return Err(EINVAL);
        }
    }
    crate::vm::copy_to_user(buf, &out).map_err(|_| EFAULT)?;
    Ok(out.len() as u64)
}

// ============================================================
//  Names
// ============================================================

pub fn mkdirat(t: &Arc<Thread>, dirfd: i64, path: u64, mode: u64) -> LxResult<u64> {
    let p = read_path(path)?;
    let parent = resolve_parent(t, dirfd, &p)?;
    if parent.dir.lookup(&parent.name).is_ok() {
        return Err(EEXIST);
    }
    let mode = mode as u32 & 0o7777 & !t.process.umask.load(core::sync::atomic::Ordering::Relaxed);
    parent.dir.create(&parent.name, Kind::Dir, mode, "")?;
    Ok(0)
}

pub fn unlinkat(t: &Arc<Thread>, dirfd: i64, path: u64, flags: u64) -> LxResult<u64> {
    let p = read_path(path)?;
    let parent = resolve_parent(t, dirfd, &p)?;
    parent.dir.unlink(&parent.name, flags & AT_REMOVEDIR != 0)?;
    Ok(0)
}

pub fn symlinkat(t: &Arc<Thread>, target: u64, dirfd: i64, path: u64) -> LxResult<u64> {
    let target = read_path(target)?;
    let p = read_path(path)?;
    let parent = resolve_parent(t, dirfd, &p)?;
    if parent.dir.lookup(&parent.name).is_ok() {
        return Err(EEXIST);
    }
    parent.dir.create(&parent.name, Kind::Symlink, 0o777, &target)?;
    Ok(0)
}

pub fn renameat(t: &Arc<Thread>, olddir: i64, old: u64, newdir: i64, new: u64, flags: u64) -> LxResult<u64> {
    const RENAME_NOREPLACE: u64 = 1;
    let o = read_path(old)?;
    let n = read_path(new)?;
    let from = resolve_parent(t, olddir, &o)?;
    let to = resolve_parent(t, newdir, &n)?;
    if flags & RENAME_NOREPLACE != 0 && to.dir.lookup(&to.name).is_ok() {
        return Err(EEXIST);
    }
    from.dir.rename(&from.name, &to.dir, &to.name)?;
    Ok(0)
}

pub fn linkat(t: &Arc<Thread>, olddir: i64, old: u64, newdir: i64, new: u64, flags: u64) -> LxResult<u64> {
    let o = read_path(old)?;
    let n = read_path(new)?;
    let target = resolve(t, olddir, &o, flags & AT_SYMLINK_FOLLOW != 0)?;
    let parent = resolve_parent(t, newdir, &n)?;
    parent.dir.link(&parent.name, &target.inode)?;
    Ok(0)
}

pub fn fchmodat(t: &Arc<Thread>, dirfd: i64, path: u64, mode: u64) -> LxResult<u64> {
    let p = read_path(path)?;
    let found = resolve(t, dirfd, &p, true)?;
    match found.inode.chmod(mode as u32) {
        Err(EROFS) => Ok(0),
        r => r.map(|_| 0),
    }
}

pub fn fchmod(t: &Arc<Thread>, fd: i64, mode: u64) -> LxResult<u64> {
    let desc = file_of(t, fd)?;
    match desc.file.inode() {
        Some(inode) => match inode.chmod(mode as u32) {
            Err(EROFS) => Ok(0),
            r => r.map(|_| 0),
        },
        None => Ok(0),
    }
}

pub fn utimensat(t: &Arc<Thread>, dirfd: i64, path: u64, times: u64, flags: u64) -> LxResult<u64> {
    let inode = if path == 0 {
        file_of(t, dirfd)?.file.inode().ok_or(EBADF)?
    } else {
        let p = read_path(path)?;
        resolve(t, dirfd, &p, flags & AT_SYMLINK_NOFOLLOW == 0)?.inode
    };
    let now = super::time::realtime_ns();
    let mtime = if times == 0 {
        now
    } else {
        let ts: [super::time::Timespec; 2] = crate::vm::read_user(times).map_err(|_| EFAULT)?;
        const UTIME_NOW: i64 = (1 << 30) - 1;
        const UTIME_OMIT: i64 = (1 << 30) - 2;
        match ts[1].nsec {
            UTIME_OMIT => return Ok(0),
            UTIME_NOW => now,
            _ => ts[1].to_ns().ok_or(EINVAL)?,
        }
    };
    let _ = inode.set_mtime(mtime);
    Ok(0)
}

pub fn truncate(t: &Arc<Thread>, path: u64, size: i64) -> LxResult<u64> {
    if size < 0 {
        return Err(EINVAL);
    }
    let p = read_path(path)?;
    resolve(t, AT_FDCWD, &p, true)?.inode.truncate(size as u64)?;
    Ok(0)
}

pub fn ftruncate(t: &Arc<Thread>, fd: i64, size: i64) -> LxResult<u64> {
    if size < 0 {
        return Err(EINVAL);
    }
    let desc = file_of(t, fd)?;
    if desc.access() == fd::O_RDONLY {
        return Err(EINVAL);
    }
    desc.file.truncate(size as u64)?;
    Ok(0)
}

/// `fsync`, `fdatasync`, `syncfs`: what the file holds in memory only
/// (pages its shared mappings changed) is written to it, then what the
/// volume holds in its cache reaches the disk. A program that asks for
/// its bytes to be safe (a database committing) must get exactly that.
pub fn fsync(t: &Arc<Thread>, fd: i64) -> LxResult<u64> {
    let desc = file_of(t, fd)?;
    if let Some(inode) = desc.file.inode() {
        inode.sync()?;
    }
    let _ = crate::fs::flush_all();
    Ok(0)
}

pub fn fallocate(t: &Arc<Thread>, fd: i64, mode: u64, offset: i64, len: i64) -> LxResult<u64> {
    if offset < 0 || len <= 0 {
        return Err(EINVAL);
    }
    let desc = file_of(t, fd)?;
    let inode = desc.file.inode().ok_or(ENODEV)?;
    let end = (offset + len) as u64;
    // Mode 0: make sure the file is at least that long. The pages come
    // on first write anyway.
    if mode == 0 && inode.stat().size < end {
        inode.truncate(end)?;
    }
    Ok(0)
}

// ============================================================
//  Working directory
// ============================================================

pub fn getcwd(t: &Arc<Thread>, buf: u64, size: u64) -> LxResult<u64> {
    let cwd = t.process.cwd();
    if cwd.len() + 1 > size as usize {
        return Err(ERANGE);
    }
    let mut bytes = cwd.into_bytes();
    bytes.push(0);
    crate::vm::copy_to_user(buf, &bytes).map_err(|_| EFAULT)?;
    Ok(bytes.len() as u64)
}

pub fn chdir(t: &Arc<Thread>, path: u64) -> LxResult<u64> {
    let p = read_path(path)?;
    let found = resolve(t, AT_FDCWD, &p, true)?;
    if found.inode.stat().kind() != Kind::Dir {
        return Err(ENOTDIR);
    }
    t.process.set_cwd(found.path);
    Ok(0)
}

pub fn fchdir(t: &Arc<Thread>, fd: i64) -> LxResult<u64> {
    let desc = file_of(t, fd)?;
    let open = desc.file.as_any().downcast_ref::<OpenFile>().ok_or(ENOTDIR)?;
    if open.inode.stat().kind() != Kind::Dir {
        return Err(ENOTDIR);
    }
    t.process.set_cwd(open.path.clone());
    Ok(0)
}

pub fn statfs_of(dev: u64, out: u64) -> LxResult<u64> {
    let magic: u64 = match dev {
        0x801 => 0xEF53,
        0x2a => 0x0102_1994,
        0x5 => 0x1373,
        _ => 0x9FA0,
    };
    let (total, _, free) = crate::frame::stats();
    let words: [u64; 15] = [magic, 4096, total as u64, free as u64, free as u64, 1 << 20, 1 << 19, 0, 255, 4096, 0x20, 0, 0, 0, 0];
    crate::vm::write_user(out, &words).map_err(|_| EFAULT)?;
    Ok(0)
}

pub fn statfs(t: &Arc<Thread>, path: u64, out: u64) -> LxResult<u64> {
    let p = read_path(path)?;
    let st = resolve(t, AT_FDCWD, &p, true)?.inode.stat();
    statfs_of(st.dev, out)
}

pub fn fstatfs(t: &Arc<Thread>, fd: i64, out: u64) -> LxResult<u64> {
    let st = file_of(t, fd)?.file.stat()?;
    statfs_of(st.dev, out)
}

// ============================================================
//  Descriptors
// ============================================================

pub fn dup(t: &Arc<Thread>, fd: i64) -> LxResult<u64> {
    let mut table = t.process.files.lock();
    let entry = table.get(fd)?;
    table.insert_from(0, fd::Entry { desc: entry.desc, cloexec: false }).map(|n| n as u64)
}

pub fn dup3(t: &Arc<Thread>, old: i64, new: i64, flags: u64) -> LxResult<u64> {
    if old == new {
        return Err(EINVAL);
    }
    let replaced = {
        let mut table = t.process.files.lock();
        let entry = table.get(old)?;
        table.install(new, fd::Entry { desc: entry.desc, cloexec: flags & fd::O_CLOEXEC as u64 != 0 })?
    };
    drop(replaced);
    Ok(new as u64)
}

pub fn dup2(t: &Arc<Thread>, old: i64, new: i64) -> LxResult<u64> {
    if old == new {
        t.process.files.lock().get(old)?;
        return Ok(new as u64);
    }
    dup3(t, old, new, 0)
}

pub fn fcntl(t: &Arc<Thread>, fd: i64, cmd: u64, arg: u64) -> LxResult<u64> {
    const F_DUPFD: u64 = 0;
    const F_GETFD: u64 = 1;
    const F_SETFD: u64 = 2;
    const F_GETFL: u64 = 3;
    const F_SETFL: u64 = 4;
    const F_GETLK: u64 = 5;
    const F_SETLK: u64 = 6;
    const F_SETLKW: u64 = 7;
    const F_DUPFD_CLOEXEC: u64 = 1030;
    const F_ADD_SEALS: u64 = 1033;
    const F_GET_SEALS: u64 = 1034;
    const F_OFD_GETLK: u64 = 36;
    let mut table = t.process.files.lock();
    let entry = table.get(fd)?;
    match cmd {
        F_DUPFD | F_DUPFD_CLOEXEC => {
            table.insert_from(arg as usize, fd::Entry { desc: entry.desc, cloexec: cmd == F_DUPFD_CLOEXEC }).map(|n| n as u64)
        }
        F_GETFD => Ok(entry.cloexec as u64),
        F_SETFD => {
            table.set_cloexec(fd, arg & 1 != 0)?;
            Ok(0)
        }
        F_GETFL => Ok(entry.desc.flags.load(core::sync::atomic::Ordering::Relaxed) as u64),
        F_SETFL => {
            let settable = fd::O_APPEND | fd::O_NONBLOCK;
            let old = entry.desc.flags.load(core::sync::atomic::Ordering::Relaxed);
            let new = (old & !settable) | (arg as u32 & settable);
            entry.desc.flags.store(new, core::sync::atomic::Ordering::Relaxed);
            drop(table);
            entry.desc.file.set_status_flags(new);
            Ok(0)
        }
        F_GETLK | F_OFD_GETLK => {
            // struct flock: l_type first; F_UNLCK (2) = nothing conflicts.
            drop(table);
            crate::vm::write_user(arg, &2u16).map_err(|_| EFAULT)?;
            Ok(0)
        }
        // Record locks always succeed: one process holds every file.
        F_SETLK | F_SETLKW | 37 | 38 => Ok(0),
        F_ADD_SEALS => Ok(0),
        F_GET_SEALS => Ok(0),
        _ => Err(EINVAL),
    }
}

pub fn ioctl(t: &Arc<Thread>, fd: i64, cmd: u64, arg: u64) -> LxResult<u64> {
    const FIONBIO: u64 = 0x5421;
    const FIONCLEX: u64 = 0x5450;
    const FIOCLEX: u64 = 0x5451;
    let entry = t.process.files.lock().get(fd)?;
    match cmd {
        FIONBIO => {
            let on: u32 = crate::vm::read_user(arg).map_err(|_| EFAULT)?;
            if on != 0 {
                entry.desc.flags.fetch_or(fd::O_NONBLOCK, core::sync::atomic::Ordering::Relaxed);
            } else {
                entry.desc.flags.fetch_and(!fd::O_NONBLOCK, core::sync::atomic::Ordering::Relaxed);
            }
            Ok(0)
        }
        FIOCLEX | FIONCLEX => {
            t.process.files.lock().set_cloexec(fd, cmd == FIOCLEX)?;
            Ok(0)
        }
        _ => entry.desc.file.ioctl(cmd, arg),
    }
}

pub fn pipe2(t: &Arc<Thread>, out: u64, flags: u64) -> LxResult<u64> {
    let (r, w) = super::pipe::new_pipe();
    let status = flags as u32 & fd::O_NONBLOCK;
    let cloexec = flags as u32 & fd::O_CLOEXEC != 0;
    let (fr, fw) = {
        let mut table = t.process.files.lock();
        let fr = table.insert(Description::new(r, status | fd::O_RDONLY), cloexec)?;
        let fw = match table.insert(Description::new(w, status | fd::O_WRONLY), cloexec) {
            Ok(fw) => fw,
            Err(e) => {
                let _ = table.remove(fr);
                return Err(e);
            }
        };
        (fr, fw)
    };
    crate::vm::write_user(out, &[fr as i32, fw as i32]).map_err(|_| EFAULT)?;
    Ok(0)
}

pub fn eventfd2(t: &Arc<Thread>, initial: u64, flags: u64) -> LxResult<u64> {
    const EFD_SEMAPHORE: u64 = 1;
    let e = super::pipe::new_eventfd(initial & 0xFFFF_FFFF, flags & EFD_SEMAPHORE != 0);
    let status = flags as u32 & fd::O_NONBLOCK | fd::O_RDWR;
    t.process.files.lock().insert(Description::new(e, status), flags as u32 & fd::O_CLOEXEC != 0).map(|n| n as u64)
}

pub fn epoll_create1(t: &Arc<Thread>, flags: u64) -> LxResult<u64> {
    let ep = super::poll::new_epoll();
    t.process.files.lock().insert(Description::new(ep, fd::O_RDWR), flags as u32 & fd::O_CLOEXEC != 0).map(|n| n as u64)
}

pub fn memfd_create(t: &Arc<Thread>, name: u64, flags: u64) -> LxResult<u64> {
    const MFD_CLOEXEC: u64 = 1;
    let n = read_path(name).unwrap_or_default();
    let node: Arc<dyn Inode> = vfs::tmpfs::TmpFs::anonymous_file();
    let file = OpenFile::new(node, alloc::format!("/memfd:{} (deleted)", n), fd::O_RDWR);
    t.process.files.lock().insert(Description::new(file, fd::O_RDWR), flags & MFD_CLOEXEC != 0).map(|n| n as u64)
}

/// `sendfile(out, in, offset, count)`: a read and a write, 1 MiB at a
/// time, through a kernel buffer.
pub fn sendfile(t: &Arc<Thread>, out_fd: i64, in_fd: i64, offset_addr: u64, count: u64) -> LxResult<u64> {
    let input = file_of(t, in_fd)?;
    let output = file_of(t, out_fd)?;
    let mut offset: Option<u64> = if offset_addr != 0 { Some(crate::vm::read_user(offset_addr).map_err(|_| EFAULT)?) } else { None };
    let mut done = 0u64;
    while done < count {
        let mut buf = user_buffer(((count - done) as usize).min(CHUNK))?;
        let n = match offset {
            Some(at) => input.file.pread(at, &mut buf)?,
            None => input.file.read(&mut buf, input.nonblock())?,
        };
        if n == 0 {
            break;
        }
        let w = output.file.write(&buf[..n], output.nonblock())?;
        done += w as u64;
        if let Some(at) = offset.as_mut() {
            *at += w as u64;
        }
        if w < n {
            break;
        }
    }
    if let Some(at) = offset {
        crate::vm::write_user(offset_addr, &at).map_err(|_| EFAULT)?;
    }
    Ok(done)
}

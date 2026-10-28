//! The file tree Linux programs see.
//!
//! ## Why a tree of its own
//!
//! The Nova filesystem is a table of links to nodes (spec 4.1) whose nodes
//! top out at 2 MiB -- five direct blocks and one indirect. `libxul.so`
//! weighs 150 MB. And a Linux program expects things Nova does not have
//! and should not grow for it: `/proc/self/exe`, `/dev/urandom`, a `/tmp`
//! it can fill, permission bits in POSIX form.
//!
//! So the subsystem assembles a tree from four sources, each mounted at
//! a path:
//!
//! | Mount | Source | Content |
//! |---|---|---|
//! | `/` | `novafs` | the Linux root, a directory of the Nova volume |
//! | `/tmp`, `/dev/shm`, `/home` | `tmpfs` | files in RAM, writable |
//! | `/dev` | `devfs` | `null`, `zero`, `urandom`, ... |
//! | `/proc`, `/sys` | `procfs` | generated on read |
//!
//! Without a root image (no browser disk attached), `/` is a tmpfs: the
//! subsystem still boots, it just has nothing to run.
//!
//! ## Resolution
//!
//! One function walks every path (`walk`). It keeps the stack of
//! directories it went through, so `..` climbs back the way it came --
//! across a mount point too -- and it follows symbolic links, at most 40
//! of them per lookup, as Linux does (`ELOOP` beyond).

pub mod devfs;
pub mod procfs;
pub mod novafs;
pub mod tmpfs;

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;

use super::errno::*;
use crate::vga::SpinLock;
use crate::vm::Pager;

// ============================================================
//  Node types and attributes
// ============================================================

pub const S_IFMT: u32 = 0o170000;
pub const S_IFSOCK: u32 = 0o140000;
pub const S_IFLNK: u32 = 0o120000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFBLK: u32 = 0o060000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFIFO: u32 = 0o010000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    CharDev,
    Fifo,
    Socket,
}

impl Kind {
    pub fn from_mode(mode: u32) -> Kind {
        match mode & S_IFMT {
            S_IFDIR => Kind::Dir,
            S_IFLNK => Kind::Symlink,
            S_IFCHR | S_IFBLK => Kind::CharDev,
            S_IFIFO => Kind::Fifo,
            S_IFSOCK => Kind::Socket,
            _ => Kind::File,
        }
    }

    /// `d_type` of `getdents64`.
    pub fn dirent_type(self) -> u8 {
        match self {
            Kind::Fifo => 1,
            Kind::CharDev => 2,
            Kind::Dir => 4,
            Kind::File => 8,
            Kind::Symlink => 10,
            Kind::Socket => 12,
        }
    }
}

/// Attributes of a node, in the terms of `struct stat`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stat {
    pub dev: u64,
    pub ino: u64,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u64,
    pub size: u64,
    pub mtime_ns: u64,
}

impl Stat {
    pub fn kind(&self) -> Kind {
        Kind::from_mode(self.mode)
    }

    /// The x86-64 `struct stat`, 144 bytes.
    pub fn to_linux(self) -> [u8; 144] {
        let mut out = [0u8; 144];
        let put = |out: &mut [u8; 144], at: usize, v: u64| out[at..at + 8].copy_from_slice(&v.to_le_bytes());
        put(&mut out, 0, self.dev);
        put(&mut out, 8, self.ino);
        put(&mut out, 16, self.nlink.max(1) as u64);
        out[24..28].copy_from_slice(&self.mode.to_le_bytes());
        out[28..32].copy_from_slice(&self.uid.to_le_bytes());
        out[32..36].copy_from_slice(&self.gid.to_le_bytes());
        put(&mut out, 40, self.rdev);
        put(&mut out, 48, self.size);
        put(&mut out, 56, 4096);
        put(&mut out, 64, self.size.div_ceil(512));
        let sec = self.mtime_ns / 1_000_000_000;
        let nsec = self.mtime_ns % 1_000_000_000;
        for base in [72, 88, 104] {
            put(&mut out, base, sec);
            put(&mut out, base + 8, nsec);
        }
        out
    }
}

pub struct DirEntry {
    pub name: String,
    pub ino: u64,
    pub kind: Kind,
}

/// A node of one of the filesystems. Every method has the answer a
/// node that does not support the operation gives, so each filesystem
/// only writes what it has.
pub trait Inode: Send + Sync {
    fn stat(&self) -> Stat;

    fn read_at(&self, _offset: u64, _buf: &mut [u8]) -> LxResult<usize> {
        Err(if self.stat().kind() == Kind::Dir { EISDIR } else { EINVAL })
    }
    fn write_at(&self, _offset: u64, _buf: &[u8]) -> LxResult<usize> {
        Err(EROFS)
    }
    fn truncate(&self, _size: u64) -> LxResult<()> {
        Err(EROFS)
    }
    fn lookup(&self, _name: &str) -> LxResult<Arc<dyn Inode>> {
        Err(ENOTDIR)
    }
    fn readdir(&self) -> LxResult<Vec<DirEntry>> {
        Err(ENOTDIR)
    }
    fn readlink(&self) -> LxResult<String> {
        Err(EINVAL)
    }
    /// Creates a regular file, a directory or a symbolic link in this
    /// directory. `target` is the link content for a symlink.
    fn create(&self, _name: &str, _kind: Kind, _mode: u32, _target: &str) -> LxResult<Arc<dyn Inode>> {
        Err(EROFS)
    }
    fn unlink(&self, _name: &str, _dir: bool) -> LxResult<()> {
        Err(EROFS)
    }
    fn rename(&self, _old: &str, _new_dir: &Arc<dyn Inode>, _new: &str) -> LxResult<()> {
        Err(EROFS)
    }
    fn link(&self, _name: &str, _target: &Arc<dyn Inode>) -> LxResult<()> {
        Err(EROFS)
    }
    fn chmod(&self, _mode: u32) -> LxResult<()> {
        Err(EROFS)
    }
    fn set_mtime(&self, _ns: u64) -> LxResult<()> {
        Ok(())
    }
    /// Writes what the node holds in memory only to its disk (`fsync`):
    /// the pages its shared mappings changed.
    fn sync(&self) -> LxResult<()> {
        Ok(())
    }
    /// What `mmap` reads the pages from.
    fn pager(self: Arc<Self>) -> Option<Arc<dyn Pager>> {
        None
    }
    /// A node that is not read like a file -- a device, a FIFO -- opens
    /// into its own file object.
    fn open_special(self: Arc<Self>) -> Option<LxResult<Arc<dyn super::fd::FileOps>>> {
        None
    }
    fn as_any(&self) -> &dyn Any;
    /// The same node as `Any`, to recover its concrete `Arc` (hard links).
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;
}

// ============================================================
//  Mounts
// ============================================================

struct Mount {
    /// Canonical absolute path, without trailing slash ("/" for the root).
    path: String,
    root: Arc<dyn Inode>,
    name: &'static str,
}

static MOUNTS: SpinLock<Vec<Mount>> = SpinLock::new(Vec::new());

pub fn mount(path: &str, root: Arc<dyn Inode>, name: &'static str) {
    let mut mounts = MOUNTS.lock();
    mounts.retain(|m| m.path != path);
    mounts.push(Mount { path: path.to_string(), root, name });
}

fn mount_at(path: &str) -> Option<Arc<dyn Inode>> {
    MOUNTS.lock().iter().find(|m| m.path == path).map(|m| m.root.clone())
}

pub fn mounts() -> Vec<(String, &'static str)> {
    MOUNTS.lock().iter().map(|m| (m.path.clone(), m.name)).collect()
}

/// Device number reported for a mount, so `st_dev` differs between
/// filesystems (programs compare it to detect a mount point).
pub fn device_of(name: &str) -> u64 {
    match name {
        "novafs" => 0x801,
        "tmpfs" => 0x2a,
        "devfs" => 0x5,
        _ => 0x3,
    }
}

// ============================================================
//  Path walking
// ============================================================

/// Symbolic links followed per lookup at most.
const MAX_LINKS: usize = 40;
/// Longest path accepted (PATH_MAX).
pub const PATH_MAX: usize = 4096;

/// Result of a walk: the node and its canonical path.
pub struct Found {
    pub inode: Arc<dyn Inode>,
    pub path: String,
}

/// Where a walk ended when asked for the parent of the last component.
pub struct Parent {
    pub dir: Arc<dyn Inode>,
    pub dir_path: String,
    pub name: String,
}

fn join(stack: &[String]) -> String {
    if stack.is_empty() {
        return String::from("/");
    }
    let mut s = String::new();
    for c in stack {
        s.push('/');
        s.push_str(c);
    }
    s
}

fn root() -> LxResult<Arc<dyn Inode>> {
    mount_at("/").ok_or(ENOENT)
}

/// Walks `path` from `cwd` (canonical absolute). `follow_last`: resolve
/// a symbolic link in last position, as `stat` does and `lstat` does not.
pub fn walk(cwd: &str, path: &str, follow_last: bool) -> LxResult<Found> {
    let (stack, nodes, last) = walk_inner(cwd, path, follow_last, false)?;
    let _ = last;
    Ok(Found { inode: nodes.last().cloned().ok_or(ENOENT)?, path: join(&stack) })
}

/// Walks to the directory that holds the last component of `path`,
/// without requiring that component to exist.
pub fn walk_parent(cwd: &str, path: &str) -> LxResult<Parent> {
    let (stack, nodes, last) = walk_inner(cwd, path, false, true)?;
    let name = last.ok_or(EEXIST)?;
    if name.len() > 255 {
        return Err(ENAMETOOLONG);
    }
    Ok(Parent { dir: nodes.last().cloned().ok_or(ENOENT)?, dir_path: join(&stack), name })
}

type Walked = (Vec<String>, Vec<Arc<dyn Inode>>, Option<String>);

fn walk_inner(cwd: &str, path: &str, follow_last: bool, want_parent: bool) -> LxResult<Walked> {
    if path.is_empty() {
        return Err(ENOENT);
    }
    if path.len() > PATH_MAX {
        return Err(ENAMETOOLONG);
    }
    let mut pending: Vec<String> = Vec::new();
    let full = if path.starts_with('/') { String::from(path) } else { alloc::format!("{}/{}", cwd, path) };
    push_components(&mut pending, &full);
    pending.reverse();

    let mut stack: Vec<String> = Vec::new();
    let mut nodes: Vec<Arc<dyn Inode>> = alloc::vec![root()?];
    let mut links = 0;

    while let Some(component) = pending.pop() {
        let is_last = pending.is_empty();
        if component == "." {
            if is_last && want_parent {
                return Err(EINVAL);
            }
            continue;
        }
        if component == ".." {
            if stack.pop().is_some() {
                nodes.pop();
            }
            if is_last && want_parent {
                return Err(EINVAL);
            }
            continue;
        }
        if component.len() > 255 {
            return Err(ENAMETOOLONG);
        }
        if is_last && want_parent {
            let dir = nodes.last().unwrap();
            if dir.stat().kind() != Kind::Dir {
                return Err(ENOTDIR);
            }
            // The last component may itself be a mount point (unlink of
            // "/tmp" and the like): the parent is still the right answer.
            return Ok((stack, nodes, Some(component)));
        }
        let dir = nodes.last().unwrap().clone();
        if dir.stat().kind() != Kind::Dir {
            return Err(ENOTDIR);
        }
        let mut here = stack.clone();
        here.push(component.clone());
        let here_path = join(&here);
        let child = match mount_at(&here_path) {
            Some(mounted) => mounted,
            None => dir.lookup(&component)?,
        };
        if child.stat().kind() == Kind::Symlink && (!is_last || follow_last) {
            links += 1;
            if links > MAX_LINKS {
                return Err(ELOOP);
            }
            let target = child.readlink()?;
            let mut expanded = Vec::new();
            push_components(&mut expanded, &target);
            expanded.reverse();
            pending.extend(expanded);
            if target.starts_with('/') {
                stack.clear();
                nodes.truncate(1);
            }
            continue;
        }
        stack.push(component);
        nodes.push(child);
    }
    if want_parent {
        // The path named a directory itself ("/", "a/.."): no last name.
        return Ok((stack, nodes, None));
    }
    Ok((stack, nodes, None))
}

fn push_components(out: &mut Vec<String>, path: &str) {
    for part in path.split('/') {
        if !part.is_empty() {
            out.push(String::from(part));
        }
    }
}

/// Reads a whole file by path -- for the loader and the self-test.
pub fn read_file(path: &str) -> LxResult<Vec<u8>> {
    let found = walk("/", path, true)?;
    let size = found.inode.stat().size as usize;
    let mut data = alloc::vec![0u8; size];
    let mut done = 0;
    while done < size {
        let n = found.inode.read_at(done as u64, &mut data[done..])?;
        if n == 0 {
            break;
        }
        done += n;
    }
    data.truncate(done);
    Ok(data)
}

/// Assembles the tree. Called once, by `linux::init`.
pub fn init() {
    // The Linux root is a directory of the Nova volume (`novafs`): the
    // browser's files are nodes and link tables of the same filesystem as
    // everything else. The tar image on its own disk remains as a
    // fallback while the volume has not been filled.
    // The Linux root is a directory of the Nova volume: the browser's
    // files are nodes and link tables of the same filesystem as the rest
    // (spec 4.1). Without one -- a volume that has never been filled --
    // the root is an empty tmpfs, and no Linux program starts.
    let (root, name): (Arc<dyn Inode>, &str) = match novafs::mount_root() {
        Some(fs) => (fs, "novafs"),
        None => (tmpfs::TmpFs::new_root(), "tmpfs"),
    };
    mount("/", root, name);
    mount("/tmp", tmpfs::TmpFs::new_root(), "tmpfs");
    mount("/dev", devfs::root(), "devfs");
    mount("/dev/shm", tmpfs::TmpFs::new_root(), "tmpfs");
    mount("/proc", procfs::proc_root(), "procfs");
    mount("/sys", procfs::sys_root(), "sysfs");
    // Spec 6.7: the devices of a ported program, under their Nova names.
    mount("/sys/dev", devfs::nova_devices(), "devfs");
    mount("/home", tmpfs::TmpFs::new_root(), "tmpfs");
    mount("/run", tmpfs::TmpFs::new_root(), "tmpfs");
    mount("/var/tmp", tmpfs::TmpFs::new_root(), "tmpfs");
    // Single files generated from Nova's own state, over the image's.
    mount("/etc/resolv.conf", procfs::resolv_conf(), "procfs");
    mount("/etc/hostname", procfs::hostname(), "procfs");
    mount("/etc/localtime", procfs::localtime(), "procfs");
    mount("/etc/timezone", procfs::timezone(), "procfs");
}

//! `/dev`: the handful of devices a program opens by name.
//!
//! No real device is exposed: `null`, `zero`, `full`, `random` and
//! `urandom` are defined by their behaviour alone, and `console`/`tty`
//! write to the serial console, like the standard descriptors. The disk
//! and the network reach a Linux program through the subsystem's own
//! calls; the screen and the sound through two Nova devices,
//! `nova-gui` (`linux::gui`) and `nova-audio` (`linux::audio`).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;

use super::{DirEntry, Inode, Kind, S_IFCHR, S_IFDIR, S_IFLNK, Stat};
use crate::posix::errno::*;
use crate::posix::fd::{Console, FileOps};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dev {
    Null,
    Zero,
    Full,
    Random,
    Console,
    Gui,
    Audio,
}

struct DevNode {
    dev: Dev,
    ino: u64,
    rdev: u64,
}

struct DevLink {
    target: &'static str,
    ino: u64,
}

struct DevDir {
    entries: BTreeMap<&'static str, Arc<dyn Inode>>,
}

pub fn root() -> Arc<dyn Inode> {
    let mut entries: BTreeMap<&'static str, Arc<dyn Inode>> = BTreeMap::new();
    let devices = [
        ("null", Dev::Null, 0x103),
        ("zero", Dev::Zero, 0x105),
        ("full", Dev::Full, 0x107),
        ("random", Dev::Random, 0x108),
        ("urandom", Dev::Random, 0x109),
        ("console", Dev::Console, 0x501),
        ("tty", Dev::Console, 0x500),
        ("nova-gui", Dev::Gui, 0xE000),
        ("nova-audio", Dev::Audio, 0xE001),
    ];
    for (i, (name, dev, rdev)) in devices.into_iter().enumerate() {
        entries.insert(name, Arc::new(DevNode { dev, ino: 2_000_000 + i as u64, rdev }));
    }
    let links = [("stdin", "/proc/self/fd/0"), ("stdout", "/proc/self/fd/1"), ("stderr", "/proc/self/fd/2"), ("fd", "/proc/self/fd")];
    for (i, (name, target)) in links.into_iter().enumerate() {
        entries.insert(name, Arc::new(DevLink { target, ino: 2_000_100 + i as u64 }));
    }
    Arc::new(DevDir { entries })
}

/// The devices of spec 6.7 under their Nova names, for `/sys/dev`: the
/// window (`fenetre`) and the sound output (`son`), the same objects and
/// the same interface as `/dev/nova-gui` and `/dev/nova-audio`.
pub fn nova_devices() -> Arc<dyn Inode> {
    let mut entries: BTreeMap<&'static str, Arc<dyn Inode>> = BTreeMap::new();
    entries.insert("fenetre", Arc::new(DevNode { dev: Dev::Gui, ino: 2_000_300, rdev: 0xE000 }));
    entries.insert("son", Arc::new(DevNode { dev: Dev::Audio, ino: 2_000_301, rdev: 0xE001 }));
    Arc::new(DevDir { entries })
}

impl Inode for DevDir {
    fn stat(&self) -> Stat {
        Stat { dev: super::device_of("devfs"), ino: 2_000_200, mode: S_IFDIR | 0o755, nlink: 2, ..Default::default() }
    }
    fn lookup(&self, name: &str) -> LxResult<Arc<dyn Inode>> {
        self.entries.get(name).cloned().ok_or(ENOENT)
    }
    fn readdir(&self) -> LxResult<Vec<DirEntry>> {
        Ok(self
            .entries
            .iter()
            .map(|(name, n)| {
                let st = n.stat();
                DirEntry { name: String::from(*name), ino: st.ino, kind: st.kind() }
            })
            .collect())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl Inode for DevLink {
    fn stat(&self) -> Stat {
        Stat { dev: super::device_of("devfs"), ino: self.ino, mode: S_IFLNK | 0o777, size: self.target.len() as u64, ..Default::default() }
    }
    fn readlink(&self) -> LxResult<String> {
        Ok(String::from(self.target))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl Inode for DevNode {
    fn stat(&self) -> Stat {
        Stat { dev: super::device_of("devfs"), ino: self.ino, mode: S_IFCHR | 0o666, rdev: self.rdev, ..Default::default() }
    }
    fn open_special(self: Arc<Self>) -> Option<LxResult<Arc<dyn FileOps>>> {
        let file: Arc<dyn FileOps> = match self.dev {
            Dev::Console => Arc::new(Console),
            Dev::Gui => crate::posix::gui::open(),
            Dev::Audio => match crate::posix::audio::open() {
                Ok(file) => file,
                Err(error) => return Some(Err(error)),
            },
            _ => Arc::new(DevFile { dev: self.dev }),
        };
        Some(Ok(file))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

struct DevFile {
    dev: Dev,
}

impl FileOps for DevFile {
    fn read(&self, buf: &mut [u8], _nonblock: bool) -> LxResult<usize> {
        match self.dev {
            Dev::Null => Ok(0),
            Dev::Zero | Dev::Full => {
                buf.fill(0);
                Ok(buf.len())
            }
            Dev::Random => {
                crate::posix::misc::fill_random(buf);
                Ok(buf.len())
            }
            Dev::Console | Dev::Gui | Dev::Audio => Ok(0),
        }
    }
    fn write(&self, buf: &[u8], _nonblock: bool) -> LxResult<usize> {
        match self.dev {
            Dev::Full => Err(ENOSPC),
            _ => Ok(buf.len()),
        }
    }
    fn pread(&self, _offset: u64, buf: &mut [u8]) -> LxResult<usize> {
        self.read(buf, false)
    }
    fn seek(&self, _offset: i64, _whence: u32) -> LxResult<u64> {
        Ok(0)
    }
    fn stat(&self) -> LxResult<Stat> {
        Ok(Stat { dev: super::device_of("devfs"), mode: S_IFCHR | 0o666, ..Default::default() })
    }
    fn kind(&self) -> Kind {
        Kind::CharDev
    }
}

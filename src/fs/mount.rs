//! 14.0 -- What the disks hold: partitions, volumes, and the root.
//!
//! `discover` turns the disks the drivers found into **volumes to mount**
//! and **media left alone**, and never writes a byte on the way:
//!
//! - a disk with a GPT or MBR table offers its LIFS and NovaFS partitions,
//!   each through a `PartitionDevice` (a window whose bounds are checked on
//!   every access); every other partition is listed and left untouched;
//! - a disk with no table is one volume if it carries LIFS or NovaFS at
//!   block 0; otherwise it joins the media the export of 4.4 may write --
//!   on the user's explicit request only;
//! - a damaged table, a partition of our type without a superblock, a
//!   NovaFS of another version: reported, left intact.
//!
//! Nothing is formatted here (14.0.1). `choose_root` then applies spec
//! 4.5: the partition named by `racine=<GUID>`, else the LIFS partition
//! flagged "system", else the first LIFS, else the first NovaFS.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::block::{self, BlockDevice};
use super::cache::BlockCache;
use super::format;
use super::lifs;
use super::partition::{self, Guid, PartitionDevice, PartitionKind, Table};
use crate::vga::SpinLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeKind {
    Lifs,
    NovaFs,
}

/// A volume found, not mounted yet.
pub struct Found {
    pub cache: BlockCache,
    pub kind: VolumeKind,
    /// The partition's GUID (GPT), `None` for a whole disk or MBR.
    pub unique: Option<Guid>,
    /// LIFS superblock flag "system".
    pub system: bool,
    /// For the log: the disk and the partition.
    pub place: String,
}

pub struct Discovery {
    pub volumes: Vec<Found>,
    pub media: Vec<Box<dyn BlockDevice>>,
}

fn mib(blocks: u64) -> u64 {
    blocks * block::BLOCK_SIZE as u64 / (1024 * 1024)
}

/// What block 0 of `cache` holds, as far as Nova is concerned: a volume
/// (its kind, its "system" flag), nothing -- or `Err`, a volume of ours
/// this kernel cannot mount, which is not a blank medium either.
fn probe(cache: &mut BlockCache, place: &str) -> Result<Option<(VolumeKind, bool)>, ()> {
    if let Some(flags) = lifs::flags_of(cache) {
        return Ok(Some((VolumeKind::Lifs, flags & lifs::format::FLAG_SYSTEM != 0)));
    }
    let mut raw = alloc::vec![0u8; block::BLOCK_SIZE];
    if cache.read(0, &mut raw).is_err() {
        return Ok(None);
    }
    match format::SuperBlock::decode(&raw) {
        Ok(_) => Ok(Some((VolumeKind::NovaFs, false))),
        Err(format::FormatError::UnsupportedVersion(found)) => {
            crate::serial_println!(
                "[fs] {} - NovaFS au format {}, ce noyau lit le format {} - laisse intact, reprendre avec `make disk-migrate`",
                place,
                found,
                format::VERSION
            );
            Err(())
        }
        Err(_) => Ok(None),
    }
}

pub fn discover(devices: Vec<Box<dyn BlockDevice>>) -> Discovery {
    let mut out = Discovery { volumes: Vec::new(), media: Vec::new() };
    for mut device in devices {
        let name = device.name();
        let blocks = device.block_count();
        match partition::read_table(device.as_mut()) {
            Table::Damaged(why) => {
                crate::serial_println!("[fs] \"{}\" ({} Mio) - table de partitions endommagee ({}) - disque laisse intact", name, mib(blocks), why);
            }
            Table::None => {
                let place = alloc::format!("\"{}\"", name);
                let mut cache = BlockCache::new(device);
                match probe(&mut cache, &place) {
                    Ok(Some((kind, system))) => out.volumes.push(Found { cache, kind, unique: None, system, place }),
                    Err(()) => {}
                    Ok(None) => {
                        crate::serial_println!(
                            "[fs] \"{}\" ({} Mio) - ni table de partitions ni volume Nova - laisse intact comme support amovible",
                            name,
                            mib(blocks)
                        );
                        out.media.push(cache.into_device());
                    }
                }
            }
            Table::Gpt { partitions, .. } => offer(&mut out, device, "GPT", &partitions),
            Table::Mbr { partitions } => offer(&mut out, device, "MBR", &partitions),
        }
    }
    out
}

/// The partitions of a disk with a table: ours become volumes, the others
/// are listed and left alone.
fn offer(out: &mut Discovery, device: Box<dyn BlockDevice>, table: &str, partitions: &[partition::Partition]) {
    let name = device.name();
    crate::serial_println!("[fs] \"{}\" ({} Mio) - table {}, {} partition(s)", name, mib(device.block_count()), table, partitions.len());
    let shared: partition::SharedDisk = Arc::new(SpinLock::new(device));
    for p in partitions.iter() {
        let size = mib(p.sectors / 8);
        let label = if p.name.is_empty() { String::new() } else { alloc::format!(" \"{}\"", p.name) };
        if matches!(p.kind, PartitionKind::Foreign(_)) {
            crate::serial_println!("[fs]   p{} {}{} ({} Mio) - pas a Nova, laissee intacte", p.index, p.describe(), label, size);
            continue;
        }
        let Some(window) = PartitionDevice::new(shared.clone(), p) else {
            crate::serial_println!("[fs]   p{} {}{} - non alignee sur 4 Kio, laissee intacte", p.index, p.describe(), label);
            continue;
        };
        let place = alloc::format!("\"{}\" p{}", name, p.index);
        let mut cache = BlockCache::new(Box::new(window));
        let wanted = if p.kind == PartitionKind::Lifs { VolumeKind::Lifs } else { VolumeKind::NovaFs };
        match probe(&mut cache, &place) {
            Ok(Some((kind, system))) if kind == wanted => {
                crate::serial_println!("[fs]   p{} {}{} ({} Mio){}", p.index, p.describe(), label, size, if system { " - systeme" } else { "" });
                let unique = (p.unique != [0; 16]).then_some(p.unique);
                out.volumes.push(Found { cache, kind, unique, system, place });
            }
            Err(()) => {}
            _ => crate::serial_println!(
                "[fs]   p{} {}{} ({} Mio) - pas de superbloc {} - non formatee, laissee intacte",
                p.index,
                p.describe(),
                label,
                size,
                p.describe()
            ),
        }
    }
}

/// Index of the root among `volumes` (spec 4.5).
pub fn choose_root(volumes: &[Found]) -> Option<usize> {
    if let Some(wanted) = crate::multiboot::root_partition() {
        if let Some(index) = volumes.iter().position(|v| v.unique == Some(wanted)) {
            return Some(index);
        }
        crate::serial_println!("[fs] racine={} introuvable - choix par defaut", partition::guid_text(&wanted));
    }
    volumes
        .iter()
        .position(|v| v.kind == VolumeKind::Lifs && v.system)
        .or_else(|| volumes.iter().position(|v| v.kind == VolumeKind::Lifs))
        .or_else(|| volumes.iter().position(|v| v.kind == VolumeKind::NovaFs))
}

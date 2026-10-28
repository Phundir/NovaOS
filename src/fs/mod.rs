//! Phase 4 -- Filesystem (spec 4, 5, 17).
//!
//! Split of responsibilities:
//!
//! | Module | Role | Step |
//! |---|---|---|
//! | `block` | Abstraction of the media, RAM disk | 4.3.1 |
//! | `cache` | Write-behind block cache | 4.3.3 |
//! | `format` | On-disk structures: superblock, node, link | 4.3.2, 4.1.1 |
//! | `volume` | Operations on a mounted volume | 4.1, 4.5 |
//! | `perm` | Thirteen permissions, five roles | 4.5.1 to 4.5.3 |
//! | `audit` | Access log | 4.5.4 |
//! | `tree` | System and user tree | 4.4 |
//! | `export` | Multi-compatibility | 4.6 |
//!
//! This file holds the **mount table** and the routing of the paths: `/`
//! names the boot disk, `A:/`, `B:/`... the others (spec 4.2). It is also
//! what tells the two forms of copy of 4.1 apart -- link rewriting inside
//! one volume, real copying between different volumes.

pub mod audit;
pub mod block;
pub mod cache;
pub mod export;
pub mod filesystem;
pub mod format;
pub mod lifs;
pub mod mount;
pub mod partition;
pub mod perm;
pub mod tree;
pub mod volume;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use block::{BlockDevice, BlockError, RamDisk};
use cache::BlockCache;
use format::{FormatError, Link, Node, NodeId};
use perm::{AccountTable, Credentials, Gid, Mode, Role, USERS_GID, Uid};
use filesystem::Filesystem;
use volume::Volume;

use crate::vga::SpinLock;

// ============================================================
//  Errors
// ============================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    /// The medium refused the operation.
    Block(BlockError),
    /// The volume is not usable.
    Format(FormatError),
    NotFound,
    AlreadyExists,
    NotADirectory,
    IsADirectory,
    /// Deleting a non-empty directory would make its content unreachable.
    DirectoryNotEmpty,
    /// Permission refused -- the missing bit is attached.
    PermissionDenied(u64),
    InvalidName,
    InvalidPath,
    /// Permission mask carrying an unknown bit.
    InvalidPermission,
    /// No free block left.
    NoSpace,
    /// No free node left in the table.
    NoNodes,
    /// Beyond what a node can address.
    TooLarge,
    /// Inconsistency detected in the structures of the volume.
    Corrupt,
    /// Unknown disk letter.
    NoSuchDisk,
    /// No volume mounted.
    NotMounted,
    /// Feature not implemented by this kernel.
    Unsupported,
}

impl From<BlockError> for FsError {
    fn from(error: BlockError) -> Self {
        FsError::Block(error)
    }
}

impl From<FormatError> for FsError {
    fn from(error: FormatError) -> Self {
        FsError::Format(error)
    }
}

impl FsError {
    pub fn message(&self) -> &'static str {
        match self {
            FsError::Block(error) => error.message(),
            FsError::Format(error) => error.message(),
            FsError::NotFound => "chemin introuvable",
            FsError::AlreadyExists => "existe deja",
            FsError::NotADirectory => "n'est pas un dossier",
            FsError::IsADirectory => "est un dossier",
            FsError::DirectoryNotEmpty => "dossier non vide",
            FsError::PermissionDenied(_) => "permission refusee",
            FsError::InvalidName => "nom de fichier invalide",
            FsError::InvalidPath => "chemin invalide",
            FsError::InvalidPermission => "masque de permission invalide",
            FsError::NoSpace => "plus de bloc libre",
            FsError::NoNodes => "plus de noeud libre",
            FsError::TooLarge => "fichier trop grand pour un noeud",
            FsError::Corrupt => "structures incoherentes",
            FsError::NoSuchDisk => "lettre de disque inconnue",
            FsError::NotMounted => "aucun volume monte",
            FsError::Unsupported => "non implemente par ce noyau",
        }
    }

    /// Readable detail, when the error carries one.
    pub fn detail(&self) -> String {
        match self {
            FsError::PermissionDenied(permission) => {
                String::from(perm::permission_name(*permission))
            }
            _ => String::new(),
        }
    }
}

// ============================================================
//  4.2 -- Mount table
// ============================================================

/// Letter of the boot disk. Spec 4.2 reserves `/` for it.
pub const BOOT_LETTER: char = '/';

/// First letter assigned to the extra disks.
const FIRST_LETTER: char = 'A';

pub struct Mount {
    pub letter: char,
    pub volume: Box<dyn Filesystem>,
    /// Has the volume just been formatted?
    pub fresh: bool,
}

struct FileSystem {
    mounts: Vec<Mount>,
    /// Media detected but not mounted: they carry no Nova OS volume and
    /// are not formatted on our own authority.
    media: Vec<Box<dyn BlockDevice>>,
    /// The root image of the Linux subsystem (phase 11.6), recognised by
    /// its first tar member. Kept apart from `media`: the export of 4.4
    /// writes to those, and must never be offered this one.
    accounts: AccountTable,
    /// Is the subsystem operational?
    ready: bool,
}

impl FileSystem {
    const fn new() -> Self {
        FileSystem {
            mounts: Vec::new(),
            media: Vec::new(),
            accounts: AccountTable::new(),
            ready: false,
        }
    }

    fn find(&mut self, letter: char) -> Option<&mut Mount> {
        self.mounts.iter_mut().find(|m| m.letter == letter)
    }
}

static FS: SpinLock<FileSystem> = SpinLock::new(FileSystem::new());

// ============================================================
//  Paths
// ============================================================

/// Separates the disk letter from the rest of the path (spec 4.2).
///
/// `A:/apps/editeur` -> `('A', "/apps/editeur")`
/// `/sys/config`     -> `('/', "/sys/config")`
///
/// A path with no letter aims at the boot disk: that is the case of
/// everything the system handles, the letters only appearing when the
/// user explicitly names another medium.
pub fn split_disk(path: &str) -> Result<(char, &str), FsError> {
    let bytes = path.as_bytes();

    // A disk prefix is **an uppercase letter** followed by a colon.
    // Demanding the uppercase to recognise the prefix, and not to
    // validate it once recognised, lets through the names starting with
    // a colon: `/:config` names a file at the root of the boot disk, not
    // a disk whose letter would be `/`.
    if bytes.len() >= 2 && bytes[1] == b':' && (bytes[0] as char).is_ascii_uppercase() {
        let letter = bytes[0] as char;
        let rest = &path[2..];
        if !rest.starts_with('/') {
            return Err(FsError::InvalidPath);
        }
        return Ok((letter, rest));
    }

    if !path.starts_with('/') {
        return Err(FsError::InvalidPath);
    }
    Ok((BOOT_LETTER, path))
}

/// Normalises a path: removes the empty segments and the `.`, and
/// resolves the `..` without ever going above the root.
pub fn normalize(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();

    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }

    let mut result = String::new();
    for segment in segments {
        result.push('/');
        result.push_str(segment);
    }
    if result.is_empty() {
        result.push('/');
    }
    result
}

// ============================================================
//  Initialisation
// ============================================================

/// Discovers the media, mounts the volumes and prepares the tree.
///
/// Phase 14: the disks are read for partition tables, every LIFS or NovaFS
/// volume found -- a partition, or a whole disk with no table -- gets a
/// letter of its own, the root is chosen by `mount::choose_root`, and
/// **nothing is formatted** (14.0.1) but the in-memory volume that stands
/// in for a missing root.
///
/// # Safety
/// Initialises PCI devices and grants them bus mastering.
pub unsafe fn init(now: u64) -> Result<(), FsError> {
    let devices: Vec<Box<dyn BlockDevice>> = unsafe { crate::ahci::detect() };
    let mount::Discovery { volumes, media } = mount::discover(devices);
    let root = mount::choose_root(&volumes);

    let mut fs = FS.lock();
    fs.media = media;
    let mut letter = FIRST_LETTER;
    // The root first: it is `/`, and the letters follow in disk order.
    let mut order: Vec<(usize, mount::Found)> = volumes.into_iter().enumerate().collect();
    if let Some(root) = root {
        let found = order.remove(root);
        order.insert(0, found);
    }
    for (index, found) in order {
        let assigned = if Some(index) == root { BOOT_LETTER } else { letter };
        let mount::Found { cache, kind, unique, place, .. } = found;
        let mounted: Result<Box<dyn Filesystem>, FsError> = match kind {
            mount::VolumeKind::Lifs => lifs::Volume::mount(cache).map(|mut volume| {
                volume.partition = unique;
                if volume.replayed {
                    crate::serial_println!("[fs] {} - transaction {} rejouee depuis le journal", place, volume.sb.journal_seq);
                }
                Box::new(volume) as Box<dyn Filesystem>
            }),
            mount::VolumeKind::NovaFs => Volume::mount(cache).map(|volume| Box::new(volume) as Box<dyn Filesystem>),
        };
        match mounted {
            Ok(mut volume) => {
                volume.set_letter(assigned);
                let info = volume.info();
                crate::serial_println!(
                    "[fs] {} monte en {} - {} \"{}\", {} Mio dont {} libres",
                    place,
                    if assigned == BOOT_LETTER { String::from("/") } else { alloc::format!("{}:/", assigned) },
                    info.format,
                    info.label,
                    info.total_blocks * block::BLOCK_SIZE as u64 / (1024 * 1024),
                    info.free_blocks * block::BLOCK_SIZE as u64 / (1024 * 1024)
                );
                fs.mounts.push(Mount { letter: assigned, volume, fresh: false });
                if assigned != BOOT_LETTER {
                    letter = (letter as u8 + 1) as char;
                }
            }
            Err(error) => {
                crate::serial_println!("[fs] {} refuse : {} {} - laisse intact", place, error.message(), error.detail());
            }
        }
    }

    // No root: no disk at all, or none carrying a Nova volume (left
    // untouched on purpose). The system still needs a `/`: it gets a LIFS
    // volume in memory -- the one thing the kernel formats itself, since
    // it is its own -- which keeps the accounts, the tree and the
    // applications working, and vanishes at power-off.
    if !fs.mounts.iter().any(|mount| mount.letter == BOOT_LETTER) {
        crate::serial_println!("[fs] aucun volume racine - repli sur un volume LIFS en memoire de 8 Mio");
        let cache = BlockCache::new(Box::new(RamDisk::new("volume memoire", 2048)));
        match lifs::Volume::format(cache, "nova-systeme", lifs::format::FLAG_SYSTEM, now) {
            Ok(mut volume) => {
                volume.letter = BOOT_LETTER;
                fs.mounts.insert(0, Mount { letter: BOOT_LETTER, volume: Box::new(volume), fresh: true });
            }
            Err(error) => {
                crate::serial_println!("[fs] volume memoire refuse : {}", error.message());
            }
        }
    }

    if fs.mounts.is_empty() {
        return Err(FsError::NotMounted);
    }

    // --- accounts ---
    fs.accounts
        .insert(perm::ROOT_UID, perm::SYSTEM_GID, "root", Role::Root);
    fs.accounts.insert(1, perm::SYSTEM_GID, "service", Role::Service);
    fs.accounts.insert(1000, USERS_GID, "admin", Role::Admin);
    fs.accounts.insert(1001, USERS_GID, "nova", Role::User);
    fs.accounts.insert(1002, USERS_GID, "invite", Role::Guest);

    fs.ready = true;
    drop(fs);

    // --- 4.4: tree ---
    // Audit log switched off for the duration of the build: a few
    // thousand uninteresting entries would drown the real ones.
    audit::set_enabled(false);
    build_trees(now)?;
    audit::set_enabled(true);

    Ok(())
}

/// Creates the tree on the volumes that lack it.
///
/// Run at every boot, not only on a volume this kernel has just
/// formatted: a volume may arrive formatted and filled -- the host writes
/// the Linux root of the browser into it (`tools/nova-fs.py`) -- and it
/// then needs its system tree like any other. `create_path` creates what
/// is missing and leaves the rest alone, so the pass is idempotent; the
/// line is printed only when something was really created.
fn build_trees(now: u64) -> Result<(), FsError> {
    let letters: Vec<char> = {
        let fs = FS.lock();
        fs.mounts.iter().map(|m| m.letter).collect()
    };

    for letter in letters {

        let mut fs = FS.lock();
        let Some(mount) = fs.find(letter) else {
            continue;
        };

        if letter == BOOT_LETTER {
            let created = tree::create_system_tree(mount.volume.as_mut(), now)?;
            if created != 0 {
                crate::serial_println!("[fs] arborescence systeme creee - {} dossiers (spec 17)", created);
            }

            for (name, uid, role) in [
                ("admin", 1000u32, Role::Admin),
                ("nova", 1001u32, Role::User),
                ("invite", 1002u32, Role::Guest),
            ] {
                let created =
                    tree::create_user_tree(mount.volume.as_mut(), name, uid, tree::default_gid(role), role, now)?;
                if created != 0 {
                    crate::serial_println!("[fs]   /users/{}/ - {} dossiers, role {}", name, created, role.as_str());
                }
            }
        } else {
            // A secondary disk does not receive the system tree: there is
            // only one `/sys/`. Only the shared space is created.
            tree::create_system_tree_minimal(mount.volume.as_mut(), now)?;
        }

        mount.volume.flush()?;
    }

    Ok(())
}

pub fn is_ready() -> bool {
    FS.lock().ready
}

// ============================================================
//  Access to the volumes
// ============================================================

/// Runs an action on the volume named by a path.
fn with_volume<F, R>(path: &str, action: F) -> Result<R, FsError>
where
    F: FnOnce(&mut dyn Filesystem, &str) -> Result<R, FsError>,
{
    let (letter, rest) = split_disk(path)?;
    let normalized = normalize(rest);

    let mut fs = FS.lock();
    if !fs.ready {
        return Err(FsError::NotMounted);
    }
    let mount = fs.find(letter).ok_or(FsError::NoSuchDisk)?;
    action(mount.volume.as_mut(), &normalized)
}

// ============================================================
//  High-level API
// ============================================================

pub fn read_file(path: &str, who: Credentials) -> Result<Vec<u8>, FsError> {
    with_volume(path, |volume, path| volume.read_file(path, who))
}

pub fn write_file(
    path: &str,
    content: &[u8],
    who: Credentials,
    mode: Mode,
    now: u64,
) -> Result<NodeId, FsError> {
    with_volume(path, |volume, path| {
        volume.write_file(path, content, who, mode, now)
    })
}

pub fn create_directory(
    path: &str,
    who: Credentials,
    mode: Mode,
    now: u64,
) -> Result<NodeId, FsError> {
    with_volume(path, |volume, path| {
        volume.create_path(path, who, mode, now)
    })
}

pub fn delete(path: &str, who: Credentials, now: u64) -> Result<(), FsError> {
    with_volume(path, |volume, path| volume.delete(path, who, now))
}

pub fn rename(path: &str, new_name: &str, who: Credentials, now: u64) -> Result<(), FsError> {
    with_volume(path, |volume, path| volume.rename(path, new_name, who, now))
}

/// Reads part of a file. What `read_file` does for a whole one, without
/// holding all of it in memory: a Linux program reading a few pages of a
/// 150 MiB library (`linux::vfs::novafs`).
pub fn read_range(path: &str, offset: u64, buffer: &mut [u8], who: Credentials) -> Result<usize, FsError> {
    with_volume(path, |volume, path| {
        let node = volume.resolve(path, who)?;
        volume.read_range(&node, offset, buffer)
    })
}

/// Runs `action` on the volume of a disk letter, for the callers that
/// address a node rather than a path.
fn with_letter<T>(letter: char, action: impl FnOnce(&mut dyn Filesystem) -> Result<T, FsError>) -> Result<T, FsError> {
    let mut fs = FS.lock();
    let mount = fs.mounts.iter_mut().find(|m| m.letter == letter).ok_or(FsError::NoSuchDisk)?;
    action(mount.volume.as_mut())
}

/// Metadata of a node named by its number.
pub fn stat_node(letter: char, id: NodeId) -> Result<Node, FsError> {
    with_letter(letter, |volume| volume.read_node(id))
}

/// Reads part of a file named by its node number.
///
/// The path was walked on every call before this: a program writing its
/// database 4 KiB at a time paid a walk of the tree per write. A node
/// number is what the filesystem itself uses (spec 4.1: a name lives in
/// a link table, a node does not carry one).
pub fn read_range_node(letter: char, id: NodeId, offset: u64, buffer: &mut [u8]) -> Result<usize, FsError> {
    with_letter(letter, |volume| {
        let node = volume.read_node(id)?;
        volume.read_range(&node, offset, buffer)
    })
}

/// Writes part of a file named by its node number.
pub fn write_range_node(letter: char, id: NodeId, offset: u64, data: &[u8], now: u64) -> Result<usize, FsError> {
    with_letter(letter, |volume| {
        let mut node = volume.read_node(id)?;
        volume.write_range(&mut node, offset, data, now)
    })
}

/// Cuts a file named by its node number.
pub fn truncate_node(letter: char, id: NodeId, size: u64, now: u64) -> Result<(), FsError> {
    with_letter(letter, |volume| {
        let mut node = volume.read_node(id)?;
        volume.truncate_node(&mut node, size, now)
    })
}

/// Reads whole blocks of a file named by its node number into frames.
///
/// # Safety
/// The frames belong to the caller and are mapped nowhere writable while
/// the controller writes into them.
pub unsafe fn read_blocks_into_frames_node(
    letter: char,
    id: NodeId,
    first_block: u64,
    frames: &[u64],
) -> Result<(), FsError> {
    with_letter(letter, |volume| {
        let node = volume.read_node(id)?;
        // SAFETY: passed on from this function's contract.
        unsafe { volume.read_blocks_into_frames(&node, first_block, frames) }
    })
}

/// Writes part of a file, growing it if needed (`linux::vfs::novafs`).
pub fn write_range(path: &str, offset: u64, data: &[u8], who: Credentials, now: u64) -> Result<usize, FsError> {
    with_volume(path, |volume, path| {
        let mut node = volume.resolve(path, who)?;
        volume.require_permission(&node, who, perm::Permissions::WRITE, path)?;
        // No flush here: a program writing its database 4 KiB at a time
        // would push the whole cache to the disk on every write (the
        // browser's profile, 33 MiB, took ten seconds that way). The
        // blocks leave on eviction, on `fsync` and at shutdown.
        volume.write_range(&mut node, offset, data, now)
    })
}

/// Cuts a file to `size`, or grows it with a hole.
pub fn truncate(path: &str, size: u64, who: Credentials, now: u64) -> Result<(), FsError> {
    with_volume(path, |volume, path| {
        let mut node = volume.resolve(path, who)?;
        volume.require_permission(&node, who, perm::Permissions::WRITE, path)?;
        volume.truncate_node(&mut node, size, now)?;
        volume.flush()?;
        Ok(())
    })
}

/// Reads whole blocks of a file straight into frames, one disk command
/// per run of consecutive blocks -- the page cache of the Linux root.
///
/// # Safety
/// The frames belong to the caller and are mapped nowhere writable while
/// the controller writes into them.
pub unsafe fn read_blocks_into_frames(
    path: &str,
    first_block: u64,
    frames: &[u64],
    who: Credentials,
) -> Result<(), FsError> {
    with_volume(path, |volume, path| {
        let node = volume.resolve(path, who)?;
        // SAFETY: passed on from this function's contract.
        unsafe { volume.read_blocks_into_frames(&node, first_block, frames) }
    })
}

pub fn list(path: &str, who: Credentials) -> Result<Vec<Link>, FsError> {
    with_volume(path, |volume, path| volume.list(path, who))
}

pub fn stat(path: &str, who: Credentials) -> Result<Node, FsError> {
    with_volume(path, |volume, path| volume.stat(path, who))
}

pub fn chmod(path: &str, mode: Mode, who: Credentials, now: u64) -> Result<(), FsError> {
    with_volume(path, |volume, path| volume.chmod(path, mode, who, now))
}

pub fn chown(
    path: &str,
    owner: Uid,
    group: perm::Gid,
    who: Credentials,
    now: u64,
) -> Result<(), FsError> {
    with_volume(path, |volume, path| {
        volume.chown(path, owner, group, who, now)
    })
}

/// 4.5.2 -- Is a file executable? Demands `execute` on the file **and** on
/// its directory.
pub fn is_executable(path: &str, who: Credentials) -> Result<bool, FsError> {
    with_volume(path, |volume, path| volume.is_executable(path, who))
}

/// Creates an extra virtual link to an existing node.
///
/// Both paths must be on the same volume: a node is identified by a
/// number that only means anything in its own table.
pub fn link(from: &str, to: &str, who: Credentials, now: u64) -> Result<NodeId, FsError> {
    let (source_letter, _) = split_disk(from)?;
    let (target_letter, _) = split_disk(to)?;
    if source_letter != target_letter {
        return Err(FsError::InvalidPath);
    }

    let source = normalize(split_disk(from)?.1);
    let target = normalize(split_disk(to)?.1);

    let mut fs = FS.lock();
    let mount = fs.find(source_letter).ok_or(FsError::NoSuchDisk)?;
    mount.volume.link(&source, &target, who, now)
}

/// Makes entry `path` designate the file at `target`, replacing the file
/// it designated -- atomically, so that a power cut leaves the old
/// target or the new one (`Filesystem::retarget`). Both must be on the
/// same volume: an entry names a node of its own table.
///
/// It is how phase 15 switches an application's `current` pointer: a
/// small file naming the active version, written aside then put in
/// place by this one step.
pub fn retarget(path: &str, target: &str, who: Credentials, now: u64) -> Result<NodeId, FsError> {
    let (path_letter, path_rest) = split_disk(path)?;
    let (target_letter, target_rest) = split_disk(target)?;
    if path_letter != target_letter {
        return Err(FsError::InvalidPath);
    }
    let path = normalize(path_rest);
    let target = normalize(target_rest);

    let mut fs = FS.lock();
    let mount = fs.find(path_letter).ok_or(FsError::NoSuchDisk)?;
    mount.volume.retarget(&path, &target, who, now)
}

/// 4.1.3 and 4.1.4 -- Moving.
///
/// Inside a volume, it is a rewriting of links: instantaneous,
/// independent of the size of the file. **Between volumes**, the node
/// cannot follow -- its identifier only means anything in its own table
/// -- and the move therefore becomes a real copy followed by a deletion.
/// That is exactly the distinction spec 4.1 draws.
pub fn move_entry(from: &str, to: &str, who: Credentials, now: u64) -> Result<bool, FsError> {
    let (source_letter, source_rest) = split_disk(from)?;
    let (target_letter, target_rest) = split_disk(to)?;

    // A directory moved under itself: its link would leave the tree for a
    // place only reachable through it, and the whole subtree would become
    // unreachable while its nodes stay allocated. `Volume::move_entry`
    // resolves both parents without comparing them, so the refusal is here,
    // on the paths, before the volume is touched.
    if source_letter == target_letter {
        let source = normalize(source_rest);
        let target = normalize(target_rest);
        if target == source
            || source == "/"
            || target.starts_with(&alloc::format!("{}/", source))
        {
            return Err(FsError::InvalidPath);
        }
        let mut fs = FS.lock();
        let mount = fs.find(source_letter).ok_or(FsError::NoSuchDisk)?;
        mount.volume.move_entry(&source, &target, who, now)?;
        // false: no data has been copied.
        return Ok(false);
    }

    copy_across(from, to, who, now)?;
    delete(from, who, now)?;
    Ok(true)
}

/// Moves many entries inside one volume, with one write to the medium.
///
/// `moves` holds (source, destination) pairs, every path on the same
/// disk. Each move is the rewrite of one link (4.1): a directory moves
/// with everything under it, whatever it holds. Returns the moves done;
/// on a refusal, the moves done before it and the error.
pub fn move_entries(
    moves: &[(String, String)],
    who: Credentials,
    now: u64,
) -> Result<usize, (usize, FsError)> {
    let Some((first, _)) = moves.first() else {
        return Ok(0);
    };
    let letter = split_disk(first).map_err(|error| (0, error))?.0;
    let mut normalized = Vec::with_capacity(moves.len());
    for (index, (from, to)) in moves.iter().enumerate() {
        let (source_letter, source_rest) = split_disk(from).map_err(|error| (index, error))?;
        let (target_letter, target_rest) = split_disk(to).map_err(|error| (index, error))?;
        if source_letter != letter || target_letter != letter {
            return Err((0, FsError::InvalidPath));
        }
        let source = normalize(source_rest);
        let target = normalize(target_rest);
        // Same refusal as `move_entry`: a directory under itself.
        if target == source
            || source == "/"
            || target.starts_with(&alloc::format!("{}/", source))
        {
            return Err((0, FsError::InvalidPath));
        }
        normalized.push((source, target));
    }
    let mut fs = FS.lock();
    if !fs.ready {
        return Err((0, FsError::NotMounted));
    }
    let mount = fs.find(letter).ok_or((0, FsError::NoSuchDisk))?;
    mount.volume.move_entries(&normalized, who, now)
}

/// Copies a file, including between two volumes.
///
/// Returns `true` if the bytes were really copied.
pub fn copy(from: &str, to: &str, who: Credentials, now: u64) -> Result<bool, FsError> {
    let (source_letter, source_rest) = split_disk(from)?;
    let (target_letter, target_rest) = split_disk(to)?;

    if source_letter == target_letter {
        let source = normalize(source_rest);
        let target = normalize(target_rest);
        let mut fs = FS.lock();
        let mount = fs.find(source_letter).ok_or(FsError::NoSuchDisk)?;
        mount.volume.copy(&source, &target, who, now)?;
        return Ok(true);
    }

    copy_across(from, to, who, now)?;
    Ok(true)
}

/// Real copy from one volume to another.
///
/// The two locks cannot be held at the same time -- there is only one for
/// the whole mount table -- so the content travels through a buffer in
/// memory. That is what caps the copyable size to that of a node, that is
/// a little over 2 MiB.
fn copy_across(from: &str, to: &str, who: Credentials, now: u64) -> Result<NodeId, FsError> {
    let content = read_file(from, who)?;
    let source = stat(from, who)?;
    let mode = Mode {
        owner: source.perm_owner,
        group: source.perm_group,
        other: source.perm_other,
    };
    write_file(to, &content, who, mode, now)
}

// ============================================================
//  4.2.3 -- Disks
// ============================================================

/// Description of a mounted volume, as SYS_FS_DISK_LIST returns it.
#[derive(Debug, Clone)]
pub struct DiskInfo {
    pub letter: char,
    pub label: String,
    /// "LIFS" or "NovaFS".
    pub format: &'static str,
    pub device: String,
    pub medium: &'static str,
    pub total_blocks: u64,
    pub free_blocks: u64,
    pub node_count: u64,
    pub free_nodes: u64,
}

impl DiskInfo {
    /// Access path to the root of the volume.
    pub fn root(&self) -> String {
        if self.letter == BOOT_LETTER {
            String::from("/")
        } else {
            alloc::format!("{}:/", self.letter)
        }
    }

    pub fn capacity_mib(&self) -> u64 {
        self.total_blocks * block::BLOCK_SIZE as u64 / (1024 * 1024)
    }

    pub fn free_mib(&self) -> u64 {
        self.free_blocks * block::BLOCK_SIZE as u64 / (1024 * 1024)
    }
}

/// Lists the mounted volumes.
pub fn disk_list() -> Vec<DiskInfo> {
    let mut fs = FS.lock();
    fs.mounts
        .iter_mut()
        .map(|mount| {
            let info = mount.volume.info();
            DiskInfo {
                letter: mount.letter,
                label: info.label,
                format: info.format,
                device: mount.volume.name(),
                medium: mount.volume.medium().as_str(),
                total_blocks: info.total_blocks,
                free_blocks: info.free_blocks,
                node_count: info.node_count,
                free_nodes: info.free_nodes,
            }
        })
        .collect()
}

/// Checks that a volume is mounted and returns its description.
pub fn disk_mount(letter: char) -> Result<DiskInfo, FsError> {
    disk_list()
        .into_iter()
        .find(|d| d.letter == letter)
        .ok_or(FsError::NoSuchDisk)
}

/// Media detected but not mounted, available for an export.
pub fn media_list() -> Vec<(String, u64)> {
    let fs = FS.lock();
    fs.media
        .iter()
        .map(|device| (device.name(), device.block_count()))
        .collect()
}

/// 4.6 -- Rewrites a Nova OS tree onto a removable medium.
///
/// The operation is **destructive** for the medium aimed at: that is the
/// very meaning of "choosing the destination format" of 4.4. It can only
/// bear on an unmounted medium, which forbids inadvertently wiping a
/// volume in use.
pub fn export_to_medium(
    index: usize,
    source: &str,
    format: export::ExportFormat,
    who: Credentials,
) -> Result<export::ExportReport, FsError> {
    // The medium is taken out of the pool for the duration of the export:
    // the exporter reads the source tree back through the functions of
    // this module, which take the lock. Keeping it here would deadlock.
    let mut device = {
        let mut fs = FS.lock();
        if index >= fs.media.len() {
            return Err(FsError::NoSuchDisk);
        }
        fs.media.remove(index)
    };

    let result = export::export(source, device.as_mut(), format, who);

    // Put back **in its place**, and not appended to the end of the list:
    // the index of a medium is what identifies it for the caller, and
    // shifting it would make the next check bear on a medium other than
    // the one just written.
    FS.lock().media.insert(index, device);
    result
}

/// Reads back the image written on a medium and checks that it really
/// carries the signature of the announced format.
///
/// Separate from the export because a successful export proves nothing:
/// the only proof that an image is readable is to read it back by the
/// path a host would take. The lock is held during the read-back, which
/// does not go through this layer again.
pub fn verify_medium(
    index: usize,
    format: export::ExportFormat,
) -> Result<(u32, u32), FsError> {
    let mut fs = FS.lock();
    let device = fs.media.get_mut(index).ok_or(FsError::NoSuchDisk)?;
    export::verify(device.as_mut(), format)
}

/// Reads raw bytes on a mounted volume, without going through a node.
///
/// It is the only path by which the storage service (6.1.3) reaches the
/// medium: inspecting a superblock or a partition table has no file name
/// to give. The read goes through the cache -- a block modified but not
/// yet written must appear as it will be, failing which the service would
/// describe a disk that already no longer exists.
///
/// The length is bounded by the caller and cannot cross the end of a
/// block: the service returns what fits in a message, and a straddling
/// read would demand a second block for a result the transport could not
/// carry anyway.
pub fn raw_read(letter: char, offset: u64, length: usize) -> Result<Vec<u8>, FsError> {
    let mut fs = FS.lock();
    let mount = fs.find(letter).ok_or(FsError::NoSuchDisk)?;

    let block_size = block::BLOCK_SIZE as u64;
    let index = offset / block_size;
    let inside = (offset % block_size) as usize;

    if index >= mount.volume.block_count() {
        return Err(FsError::Block(block::BlockError::OutOfRange));
    }

    let count = length.min(block::BLOCK_SIZE - inside);
    let mut buffer = alloc::vec![0u8; block::BLOCK_SIZE];
    mount.volume.raw_read(index, &mut buffer)?;

    Ok(buffer[inside..inside + count].to_vec())
}

/// The group commit of every LIFS volume (`Filesystem::tick`): what the
/// last two seconds changed goes to the journal, then home.
pub fn tick_all() {
    let mut fs = FS.lock();
    for mount in fs.mounts.iter_mut() {
        if let Err(error) = mount.volume.tick() {
            crate::serial_println!("[fs] validation du journal de {}:/ refusee : {}", mount.letter, error.message());
        }
    }
}

/// The task of the group commit: `tick_all` every half second (a volume
/// commits when its two seconds are up, not at every call).
pub fn commit_task() -> ! {
    loop {
        let _ = crate::syscall::invoke(crate::syscall::domain_time::SYS_TIME_SLEEP_MS, [500, 0, 0, 0, 0, 0]);
        if is_ready() {
            tick_all();
        }
    }
}

// ============================================================
//  14.4.1 -- By node identifier (LIFS, spec 6.2)
// ============================================================

pub fn node_stat(letter: char, id: NodeId, who: Credentials) -> Result<Node, FsError> {
    with_letter(letter, |volume| volume.node_stat(id, who))
}

/// The path of a node, with its disk letter (`A:/x`, or `/x` on the root).
pub fn node_path(letter: char, id: NodeId) -> Result<String, FsError> {
    let path = with_letter(letter, |volume| volume.node_path(id))?;
    Ok(if letter == BOOT_LETTER { path } else { alloc::format!("{}:{}", letter, path) })
}

pub fn node_move(letter: char, from: NodeId, name: &str, to: NodeId, who: Credentials, now: u64) -> Result<(), FsError> {
    with_letter(letter, |volume| volume.node_move(from, name, to, who, now))
}

pub fn node_clone(letter: char, id: NodeId, to: NodeId, name: &str, who: Credentials, now: u64) -> Result<NodeId, FsError> {
    with_letter(letter, |volume| volume.node_clone(id, to, name, who, now))
}

pub fn node_tag(letter: char, id: NodeId, to: NodeId, name: &str, who: Credentials, now: u64) -> Result<(), FsError> {
    with_letter(letter, |volume| volume.node_tag(id, to, name, who, now))
}

pub fn node_unlink(letter: char, dir: NodeId, name: &str, who: Credentials, now: u64) -> Result<(), FsError> {
    with_letter(letter, |volume| volume.node_unlink(dir, name, who, now))
}

pub fn node_tags(letter: char, id: NodeId, who: Credentials) -> Result<Vec<filesystem::TagEntry>, FsError> {
    with_letter(letter, |volume| volume.node_tags(id, who))
}

/// Writes to the media everything the caches hold.
pub fn flush_all() -> Result<(), FsError> {
    let mut fs = FS.lock();
    for mount in fs.mounts.iter_mut() {
        mount.volume.flush()?;
    }
    Ok(())
}

// ============================================================
//  Accounts
// ============================================================

/// Access context of an account, by its name.
pub fn credentials_of(name: &str) -> Option<Credentials> {
    let fs = FS.lock();
    fs.accounts
        .find_by_name(name)
        .map(|a| Credentials::new(a.uid, a.gid, a.role))
}

/// Account named by its identifier.
pub fn account_of(uid: Uid) -> Option<(String, Role, Gid)> {
    let fs = FS.lock();
    fs.accounts
        .find(uid)
        .map(|a| (a.name.clone(), a.role, a.gid))
}

/// 5.6.1 -- Creates an account.
///
/// The check of the right to do so is not here: it belongs to the
/// `SYS_USER_*` domain, the only place that knows the caller.
pub fn create_account(uid: Uid, gid: Gid, name: &str, role: Role) -> Result<(), FsError> {
    if name.is_empty() || !format::valid_name(name) {
        return Err(FsError::InvalidName);
    }
    let mut fs = FS.lock();
    if fs.accounts.find_by_name(name).is_some() {
        return Err(FsError::AlreadyExists);
    }
    if fs.accounts.find(uid).is_some() {
        return Err(FsError::AlreadyExists);
    }
    fs.accounts.insert(uid, gid, name, role);
    Ok(())
}

/// 5.6.1 -- Deletes an account.
///
/// The root account is not deleted: the kernel acts under its identity,
/// and a table without it would make every check impossible.
pub fn delete_account(name: &str) -> Result<(), FsError> {
    let mut fs = FS.lock();
    let uid = match fs.accounts.find_by_name(name) {
        Some(account) => account.uid,
        None => return Err(FsError::NotFound),
    };
    if uid == perm::ROOT_UID {
        return Err(FsError::PermissionDenied(perm::Permissions::OWN));
    }
    fs.accounts.remove(uid);
    Ok(())
}

pub fn account_names() -> Vec<(String, Role, Uid)> {
    let fs = FS.lock();
    fs.accounts
        .iter()
        .map(|a| (a.name.clone(), a.role, a.uid))
        .collect()
}

// ============================================================
//  Diagnostics
// ============================================================

pub fn dump_state() {
    let mut fs = FS.lock();
    crate::serial_println!("[fs] {} volume(s) monte(s)", fs.mounts.len());
    for mount in fs.mounts.iter_mut() {
        let letter = mount.letter;
        crate::serial_println!(
            "[fs] --- {} ---",
            if letter == BOOT_LETTER {
                String::from("/")
            } else {
                alloc::format!("{}:/", letter)
            }
        );
        mount.volume.dump_state();
    }
}

/// Displays the tree of a volume down to a given depth.
pub fn dump_tree(path: &str, who: Credentials, limit: usize) {
    let Ok((letter, rest)) = split_disk(path) else {
        return;
    };
    let normalized = normalize(rest);
    let mut fs = FS.lock();
    let Some(mount) = fs.find(letter) else {
        return;
    };
    crate::serial_println!("[fs] arborescence de \"{}\" :", path);
    mount.volume.dump_tree(&normalized, who, 0, limit);
}

/// Checks the coherence of every volume.
pub fn check_all() -> Result<(usize, usize), FsError> {
    let mut fs = FS.lock();
    let mut nodes = 0;
    let mut blocks = 0;
    for mount in fs.mounts.iter_mut() {
        let (n, b) = mount.volume.check()?;
        nodes += n;
        blocks += b;
    }
    Ok((nodes, blocks))
}

pub mod selftest;

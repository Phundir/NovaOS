//! 5.2 -- Domain 0x01, filesystem (spec 6.2).
//!
//! Twenty-three calls (the six by node identifier since phase 14), all
//! built on the same pattern: the paths arrive as a
//! `(pointer, length)` pair, the buffers too, and the access context is
//! **derived from the calling process** instead of being supplied by it.
//!
//! ## What the arrival of this domain changes
//!
//! The whole of phase 4 took a `Credentials` parameter: the rules of 5
//! were implemented but whoever called the filesystem could present
//! themselves as ROOT. The roadmap noted it as an open limit. It closes
//! here: `Call::who` queries the scheduler, and a ring 3 program has no
//! way to influence the result.
//!
//! ## Timestamps
//!
//! The operations that date a node now receive a real instant when the
//! CMOS clock could be read, and the tick counter otherwise. The choice
//! is made in a single place -- `now()` below -- so that no call dates
//! differently from the others.
//!
//! ## Sync and Async
//!
//! Spec 6.2 declares `READ`, `WRITE`, `OVERWRITE`, `COPY` and
//! `DISK_EXPORT` asynchronous because they touch a medium. Our AHCI
//! driver working by polling, those calls only hand back once the I/O is
//! over: the declared mode is an interface promise, kept the day storage
//! moves to user space. The mode is nonetheless accurate in the table,
//! because it is what `SYS_API_INFO` publishes, and a program written
//! against that promise must be able to rely on it from today.

use alloc::vec::Vec;

mod node;

pub use node::TAG_ENTRY_SIZE;

use super::error::{self, ErrorCode, SysError, SysResult};
use super::table::{Entry, entry};
use super::Call;
use crate::fs::{self, export::ExportFormat, format::NodeKind, perm::Mode as FsMode, perm::Permissions};

pub const SYS_FS_READ: u16 = 0x0101;
pub const SYS_FS_WRITE: u16 = 0x0102;
pub const SYS_FS_OVERWRITE: u16 = 0x0103;
pub const SYS_FS_DELETE: u16 = 0x0104;
pub const SYS_FS_RENAME: u16 = 0x0105;
pub const SYS_FS_MOVE: u16 = 0x0106;
pub const SYS_FS_COPY: u16 = 0x0107;
pub const SYS_FS_LINK: u16 = 0x0108;
pub const SYS_FS_STAT: u16 = 0x0109;
pub const SYS_FS_CHMOD: u16 = 0x010A;
pub const SYS_FS_CHOWN: u16 = 0x010B;
pub const SYS_FS_AUDIT: u16 = 0x010C;
pub const SYS_FS_DIR_ACCESS: u16 = 0x0110;
pub const SYS_FS_DIR_CREATE: u16 = 0x0111;
pub const SYS_FS_DISK_LIST: u16 = 0x0120;
pub const SYS_FS_DISK_MOUNT: u16 = 0x0121;
pub const SYS_FS_DISK_EXPORT: u16 = 0x0124;
pub const SYS_FS_NODE_STAT: u16 = 0x0130;
pub const SYS_FS_NODE_MOVE: u16 = 0x0131;
pub const SYS_FS_NODE_CLONE: u16 = 0x0132;
pub const SYS_FS_NODE_TAG: u16 = 0x0133;
pub const SYS_FS_NODE_UNLINK: u16 = 0x0134;
pub const SYS_FS_NODE_TAGS: u16 = 0x0135;

pub static TABLE: &[Entry] = &[
    entry!(SYS_FS_READ, "READ", Async, read),
    entry!(SYS_FS_WRITE, "WRITE", Async, write),
    entry!(SYS_FS_OVERWRITE, "OVERWRITE", Async, overwrite),
    entry!(SYS_FS_DELETE, "DELETE", Sync, delete),
    entry!(SYS_FS_RENAME, "RENAME", Sync, rename),
    entry!(SYS_FS_MOVE, "MOVE", Sync, move_entry),
    entry!(SYS_FS_COPY, "COPY", Async, copy),
    entry!(SYS_FS_LINK, "LINK", Sync, link),
    entry!(SYS_FS_STAT, "STAT", Sync, stat),
    entry!(SYS_FS_CHMOD, "CHMOD", Sync, chmod),
    entry!(SYS_FS_CHOWN, "CHOWN", Sync, chown),
    entry!(SYS_FS_AUDIT, "AUDIT", Sync, audit),
    entry!(SYS_FS_DIR_ACCESS, "DIR_ACCESS", Sync, dir_access),
    entry!(SYS_FS_DIR_CREATE, "DIR_CREATE", Sync, dir_create),
    entry!(SYS_FS_DISK_LIST, "DISK_LIST", Sync, disk_list),
    entry!(SYS_FS_DISK_MOUNT, "DISK_MOUNT", Sync, disk_mount),
    entry!(SYS_FS_DISK_EXPORT, "DISK_EXPORT", Async, disk_export),
    entry!(SYS_FS_NODE_STAT, "NODE_STAT", Sync, node::node_stat),
    entry!(SYS_FS_NODE_MOVE, "NODE_MOVE", Sync, node::node_move),
    entry!(SYS_FS_NODE_CLONE, "NODE_CLONE", Sync, node::node_clone),
    entry!(SYS_FS_NODE_TAG, "NODE_TAG", Sync, node::node_tag),
    entry!(SYS_FS_NODE_UNLINK, "NODE_UNLINK", Sync, node::node_unlink),
    entry!(SYS_FS_NODE_TAGS, "NODE_TAGS", Sync, node::node_tags),
];

/// Maximum length of a path accepted from user space.
const PATH_MAX: u64 = 1024;

/// Instant to write on a node.
///
/// The wall clock when it exists, the tick counter otherwise. The two
/// scales are not comparable, but mixing both within one volume would be
/// worse: `rtc::now` becomes available once and for all, at boot, and
/// never switches part way.
fn now() -> u64 {
    crate::rtc::now().unwrap_or_else(crate::idt::ticks)
}

fn ready() -> SysResult<()> {
    if fs::is_ready() {
        Ok(())
    } else {
        Err(SysError::with(
            ErrorCode::NoDevice,
            "aucun volume monte",
        ))
    }
}

/// Reads a path from the first two arguments.
fn path_at(call: &Call, index: usize) -> SysResult<alloc::string::String> {
    let length = call.arg(index + 1);
    if length > PATH_MAX {
        return Err(SysError::with(ErrorCode::NameTooLong, "chemin"));
    }
    let path = call.read_str(call.arg(index), length)?;
    guard(call, &path)?;
    Ok(path)
}

/// Does this call modify what it names?
///
/// Read and write do not lead to the same decision: 12.3 forbids writing
/// another application's binary outright, where reading it is merely
/// outside the sandbox and therefore a question for the user. A single
/// "access" would collapse the two.
fn writes(number: u16) -> bool {
    matches!(
        number,
        SYS_FS_WRITE
            | SYS_FS_OVERWRITE
            | SYS_FS_DELETE
            | SYS_FS_RENAME
            | SYS_FS_MOVE
            | SYS_FS_COPY
            | SYS_FS_LINK
            | SYS_FS_CHMOD
            | SYS_FS_CHOWN
            | SYS_FS_DIR_CREATE
            | SYS_FS_NODE_MOVE
            | SYS_FS_NODE_CLONE
            | SYS_FS_NODE_TAG
            | SYS_FS_NODE_UNLINK
    )
}

/// 10.3.4 -- Applies the per-type sandbox of 12.4 to a path.
///
/// ## What this closes
///
/// The roadmap carried, from phase 4 onwards: *"the path sandbox of 12.4
/// is described, not enforced. `tree::sandbox_roots` gives the table;
/// nothing makes it hold at run time."* This is the line that makes it
/// hold, and it sits in `path_at` rather than in each of the eighteen
/// handlers because a rule enforced in seventeen places out of eighteen
/// is not enforced.
///
/// ## It does not replace the permissions of chapter 5
///
/// Two different questions, both of which have to be answered. Chapter 5
/// asks "does this **account** have the right to this file"; 12.4 asks
/// "is this file inside what this **application** may see at all". A
/// `.cus` running as root still cannot read another application's
/// directory, and an application inside its sandbox still cannot read a
/// file whose mode refuses it. The filesystem answers the first, and it
/// is left exactly where it was.
///
/// A task with no identity -- every kernel service, and every self-test
/// -- is subject to neither: see the header of `apps`.
fn guard(call: &Call, path: &str) -> SysResult<()> {
    let outcome = crate::security::check_path(call.pid, path, writes(call.number));
    match outcome {
        crate::security::Outcome::Allowed => Ok(()),
        crate::security::Outcome::Denied(reason) => Err(SysError::with(
            ErrorCode::PermissionDenied,
            reason.as_str(),
        )),
    }
}

/// Translates a permission mask coming from ring 3.
///
/// An unknown bit is refused rather than ignored: accepting a mask one
/// does not understand would amount to silently granting a right
/// different from the one asked for.
fn mask(value: u64, label: &str) -> SysResult<Permissions> {
    let permissions = Permissions(value);
    if permissions.unknown_bits() != 0 {
        return Err(SysError::with(ErrorCode::InvalidArgument, label));
    }
    Ok(permissions)
}

/// Creation mode: the owner mask is enough, the rest is null.
///
/// A null mask means "the usual rights of a private file" -- it is what a
/// program writing without caring about permissions expects, and it
/// avoids inadvertently creating a file nobody, not even its author,
/// could read back.
fn creation_mode(owner: u64, directory: bool) -> SysResult<FsMode> {
    if owner == 0 {
        return Ok(if directory {
            FsMode::private_dir()
        } else {
            FsMode::private_file()
        });
    }
    let owner = mask(owner, "masque du proprietaire")?;
    Ok(FsMode::new(owner.0, 0, 0))
}

// ============================================================
//  5.2.1 -- READ / WRITE / OVERWRITE / DELETE / RENAME
// ============================================================

/// `READ(path, length, buffer, size, offset)`.
///
/// Returns the number of bytes written into the buffer. The offset allows
/// re-reading a file larger than `TRANSFER_MAX` in several goes --
/// without it, the transfer bound would become a file size bound.
fn read(call: &mut Call) -> SysResult<u64> {
    ready()?;
    let path = path_at(call, 0)?;
    let offset = call.arg(4) as usize;

    let content = fs::read_file(&path, call.who()).map_err(|e| error::from_fs(e, &path))?;
    if offset >= content.len() {
        return Ok(0);
    }
    call.write_bytes(call.arg(2), call.arg(3), &content[offset..])
}

/// `WRITE(path, length, buffer, size, mask)` -- creation.
///
/// Refuses to overwrite: it is the distinction of 5 between `write` and
/// `overwrite`, two separate permissions because they do not have the
/// same consequences. A program wanting to replace must say so.
fn write(call: &mut Call) -> SysResult<u64> {
    ready()?;
    let path = path_at(call, 0)?;
    let data = call.read_bytes(call.arg(2), call.arg(3))?;
    let mode = creation_mode(call.arg(4), false)?;

    if fs::stat(&path, call.who()).is_ok() {
        return Err(SysError::with_two(
            ErrorCode::AlreadyExists,
            &path,
            "utiliser OVERWRITE",
        ));
    }

    fs::write_file(&path, &data, call.who(), mode, now())
        .map_err(|e| error::from_fs(e, &path))?;
    Ok(data.len() as u64)
}

/// `OVERWRITE(path, length, buffer, size, mask)` -- replacement.
fn overwrite(call: &mut Call) -> SysResult<u64> {
    ready()?;
    let path = path_at(call, 0)?;
    let data = call.read_bytes(call.arg(2), call.arg(3))?;
    let mode = creation_mode(call.arg(4), false)?;

    fs::write_file(&path, &data, call.who(), mode, now())
        .map_err(|e| error::from_fs(e, &path))?;
    Ok(data.len() as u64)
}

fn delete(call: &mut Call) -> SysResult<u64> {
    ready()?;
    let path = path_at(call, 0)?;
    fs::delete(&path, call.who(), now()).map_err(|e| error::from_fs(e, &path))?;
    Ok(0)
}

/// `RENAME(path, length, new_name, length)`.
fn rename(call: &mut Call) -> SysResult<u64> {
    ready()?;
    let path = path_at(call, 0)?;
    let name = path_at(call, 2)?;
    fs::rename(&path, &name, call.who(), now()).map_err(|e| error::from_fs(e, &path))?;
    Ok(0)
}

// ============================================================
//  5.2.2 -- MOVE / COPY / LINK
// ============================================================

/// `MOVE(source, length, destination, length)`.
///
/// Returns 0 if the operation came down to rewriting a link -- the case
/// of 4.1.3, instantaneous whatever the size -- and 1 if the bytes had to
/// be copied, which only happens between volumes. The caller can thus
/// know what it has really just paid.
fn move_entry(call: &mut Call) -> SysResult<u64> {
    ready()?;
    let from = path_at(call, 0)?;
    let to = path_at(call, 2)?;
    let copied = fs::move_entry(&from, &to, call.who(), now())
        .map_err(|e| error::from_fs(e, &from))?;
    Ok(u64::from(copied))
}

fn copy(call: &mut Call) -> SysResult<u64> {
    ready()?;
    let from = path_at(call, 0)?;
    let to = path_at(call, 2)?;
    fs::copy(&from, &to, call.who(), now()).map_err(|e| error::from_fs(e, &from))?;
    Ok(0)
}

/// `LINK(source, length, destination, length)` -- virtual link of 4.1.
///
/// Returns the identifier of the now shared node: it is the proof that no
/// copy took place, both paths naming the same number.
fn link(call: &mut Call) -> SysResult<u64> {
    ready()?;
    let from = path_at(call, 0)?;
    let to = path_at(call, 2)?;
    fs::link(&from, &to, call.who(), now()).map_err(|e| error::from_fs(e, &from))
}

// ============================================================
//  5.2.3 -- STAT / CHMOD / CHOWN / AUDIT
// ============================================================

/// Layout of a `STAT` record.
///
/// ```text
///   0x00  8  node identifier
///   0x08  8  size in bytes
///   0x10  8  creation date
///   0x18  8  modification date
///   0x20  8  access date
///   0x28  8  owner mask
///   0x30  8  group mask
///   0x38  8  other mask
///   0x40  4  owner
///   0x44  4  group
///   0x48  2  number of links
///   0x4A  1  nature: 1 file, 2 directory
///   0x4B  5  reserved
/// ```
pub const STAT_SIZE: usize = 0x50;

fn encode_stat(node: &crate::fs::format::Node) -> [u8; STAT_SIZE] {
    let mut record = [0u8; STAT_SIZE];
    record[0x00..0x08].copy_from_slice(&node.id.to_le_bytes());
    record[0x08..0x10].copy_from_slice(&node.size.to_le_bytes());
    record[0x10..0x18].copy_from_slice(&node.created.to_le_bytes());
    record[0x18..0x20].copy_from_slice(&node.modified.to_le_bytes());
    record[0x20..0x28].copy_from_slice(&node.accessed.to_le_bytes());
    record[0x28..0x30].copy_from_slice(&node.perm_owner.0.to_le_bytes());
    record[0x30..0x38].copy_from_slice(&node.perm_group.0.to_le_bytes());
    record[0x38..0x40].copy_from_slice(&node.perm_other.0.to_le_bytes());
    record[0x40..0x44].copy_from_slice(&node.owner.to_le_bytes());
    record[0x44..0x48].copy_from_slice(&node.group.to_le_bytes());
    record[0x48..0x4A].copy_from_slice(&node.links.to_le_bytes());
    record[0x4A] = match node.kind {
        NodeKind::File => 1,
        NodeKind::Directory => 2,
        NodeKind::Free => 0,
    };
    record
}

fn stat(call: &mut Call) -> SysResult<u64> {
    ready()?;
    let path = path_at(call, 0)?;
    let node = fs::stat(&path, call.who()).map_err(|e| error::from_fs(e, &path))?;
    call.write_bytes(call.arg(2), call.arg(3), &encode_stat(&node))
}

/// `CHMOD(path, length, owner, group, others)`.
fn chmod(call: &mut Call) -> SysResult<u64> {
    ready()?;
    let path = path_at(call, 0)?;
    let mode = FsMode {
        owner: mask(call.arg(2), "masque du proprietaire")?,
        group: mask(call.arg(3), "masque du groupe")?,
        other: mask(call.arg(4), "masque des autres")?,
    };
    fs::chmod(&path, mode, call.who(), now()).map_err(|e| error::from_fs(e, &path))?;
    Ok(0)
}

/// `CHOWN(path, length, uid, gid)`.
fn chown(call: &mut Call) -> SysResult<u64> {
    ready()?;
    let path = path_at(call, 0)?;
    let uid = u32::try_from(call.arg(2)).map_err(|_| error::invalid("uid hors bornes"))?;
    let gid = u32::try_from(call.arg(3)).map_err(|_| error::invalid("gid hors bornes"))?;
    fs::chown(&path, uid, gid, call.who(), now()).map_err(|e| error::from_fs(e, &path))?;
    Ok(0)
}

/// Layout of an audit log entry.
///
/// ```text
///   0x00  8  instant
///   0x08  8  permission requested
///   0x10  4  uid
///   0x14  1  role
///   0x15  1  granted
///   0x16  1  length of the path
///   0x17  1  reserved
///   0x18 96  path
/// ```
pub const AUDIT_ENTRY_SIZE: usize = 0x78;

/// `AUDIT(buffer, size)` -- reserved to ADMIN and ROOT (spec 5.1).
fn audit(call: &mut Call) -> SysResult<u64> {
    let entries = fs::audit::read(call.who())
        .map_err(|_| error::denied("le journal d'audit est reserve a ADMIN"))?;

    let capacity = (call.arg(1) as usize) / AUDIT_ENTRY_SIZE;
    let count = entries.len().min(capacity);
    let mut buffer = Vec::with_capacity(count * AUDIT_ENTRY_SIZE);

    // The most recent first: it is what one wants to read when looking
    // for why an access has just been refused.
    for item in entries.iter().rev().take(count) {
        let mut record = [0u8; AUDIT_ENTRY_SIZE];
        record[0x00..0x08].copy_from_slice(&item.when.to_le_bytes());
        record[0x08..0x10].copy_from_slice(&item.permission.to_le_bytes());
        record[0x10..0x14].copy_from_slice(&item.uid.to_le_bytes());
        record[0x14] = item.role as u8;
        record[0x15] = u8::from(item.granted);
        let path = item.path.as_bytes();
        let length = path.len().min(AUDIT_ENTRY_SIZE - 0x18);
        record[0x16] = length as u8;
        record[0x18..0x18 + length].copy_from_slice(&path[..length]);
        buffer.extend_from_slice(&record);
    }

    call.write_bytes(call.arg(0), call.arg(1), &buffer)?;
    Ok(entries.len() as u64)
}

// ============================================================
//  5.2.4 -- DIR_ACCESS / DIR_CREATE
// ============================================================

/// Layout of a directory entry.
///
/// ```text
///   0x00  8  node identifier
///   0x08  1  nature: 1 file, 2 directory
///   0x09  1  length of the name
///   0x0A 56  name
/// ```
pub const DIR_ENTRY_SIZE: usize = 0x42;

/// `DIR_ACCESS(path, length, buffer, size)`.
///
/// Returns the total number of entries, whether the buffer received them
/// all or not -- same convention as `DISK_LIST` and `API_INFO`.
fn dir_access(call: &mut Call) -> SysResult<u64> {
    ready()?;
    let path = path_at(call, 0)?;
    let who = call.who();

    let links = fs::list(&path, who).map_err(|e| error::from_fs(e, &path))?;
    let capacity = (call.arg(3) as usize) / DIR_ENTRY_SIZE;
    let count = links.len().min(capacity);

    let mut buffer = Vec::with_capacity(count * DIR_ENTRY_SIZE);
    for item in links.iter().take(count) {
        let mut record = [0u8; DIR_ENTRY_SIZE];
        record[0x00..0x08].copy_from_slice(&item.node.to_le_bytes());

        // The nature of the entry demands a `stat` per name: the link
        // table carries only the node number. A directory of a hundred
        // entries therefore costs a hundred node reads, all served by the
        // cache. Filing the nature in the link would duplicate it, and it
        // would have to be kept coherent on every change.
        let child = crate::fs::export::join_path(&path, &item.name);
        record[0x08] = match fs::stat(&child, who) {
            Ok(node) => match node.kind {
                NodeKind::File => 1,
                NodeKind::Directory => 2,
                NodeKind::Free => 0,
            },
            Err(_) => 0,
        };

        let name = item.name.as_bytes();
        let length = name.len().min(DIR_ENTRY_SIZE - 0x0A);
        record[0x09] = length as u8;
        record[0x0A..0x0A + length].copy_from_slice(&name[..length]);
        buffer.extend_from_slice(&record);
    }

    call.write_bytes(call.arg(2), call.arg(3), &buffer)?;
    Ok(links.len() as u64)
}

/// `DIR_CREATE(path, length, mask)`.
fn dir_create(call: &mut Call) -> SysResult<u64> {
    ready()?;
    let path = path_at(call, 0)?;
    let mode = creation_mode(call.arg(2), true)?;
    fs::create_directory(&path, call.who(), mode, now())
        .map_err(|e| error::from_fs(e, &path))?;
    Ok(0)
}

// ============================================================
//  5.2.5 -- DISK_LIST / DISK_MOUNT / DISK_EXPORT
// ============================================================

/// Layout of a volume descriptor.
///
/// ```text
///   0x00  1  letter of the volume -- 0x2F for '/'
///   0x01  1  nature of the medium: 0 RAM, 1 SATA, 2 removable
///   0x02  6  reserved
///   0x08  8  blocks in total
///   0x10  8  free blocks
///   0x18  8  nodes in total
///   0x20  8  free nodes
/// ```
pub const DISK_ENTRY_SIZE: usize = 0x28;

fn disk_list(call: &mut Call) -> SysResult<u64> {
    ready()?;
    let disks = fs::disk_list();
    let capacity = (call.arg(1) as usize) / DISK_ENTRY_SIZE;
    let count = disks.len().min(capacity);

    let mut buffer = Vec::with_capacity(count * DISK_ENTRY_SIZE);
    for disk in disks.iter().take(count) {
        let mut record = [0u8; DISK_ENTRY_SIZE];
        record[0] = disk.letter as u8;
        record[1] = match disk.medium {
            "RAM" => 0,
            "SATA" => 1,
            _ => 2,
        };
        record[0x08..0x10].copy_from_slice(&disk.total_blocks.to_le_bytes());
        record[0x10..0x18].copy_from_slice(&disk.free_blocks.to_le_bytes());
        record[0x18..0x20].copy_from_slice(&disk.node_count.to_le_bytes());
        record[0x20..0x28].copy_from_slice(&disk.free_nodes.to_le_bytes());
        buffer.extend_from_slice(&record);
    }

    call.write_bytes(call.arg(0), call.arg(1), &buffer)?;
    Ok(disks.len() as u64)
}

/// `DISK_MOUNT(letter)` -- confirms that a letter answers, and returns
/// its capacity in blocks.
///
/// The effective mount happens at boot: detecting an insertion assumes
/// the AHCI interrupts, hence the user-space drivers.
fn disk_mount(call: &mut Call) -> SysResult<u64> {
    ready()?;
    let letter = call.arg(0);
    if letter > 0x7F {
        return Err(error::invalid("lettre de disque hors ASCII"));
    }
    fs::disk_mount(letter as u8 as char)
        .map(|disk| disk.total_blocks)
        .map_err(|e| error::from_fs(e, "lettre de disque"))
}

/// `DISK_EXPORT(index, path, length, format)` -- 4.4.
///
/// The operation is destructive for the medium aimed at, and reserved to
/// ADMIN and ROOT: rewriting a stick plugged in by mistake is not an
/// operation any program should be able to trigger.
fn disk_export(call: &mut Call) -> SysResult<u64> {
    ready()?;
    let who = call.who();
    if !matches!(
        who.role,
        crate::fs::perm::Role::Root | crate::fs::perm::Role::Admin
    ) {
        return Err(error::denied("l'export est reserve a ADMIN"));
    }

    let source = path_at(call, 1)?;
    let format = match call.arg(3) {
        0 => ExportFormat::Native,
        1 => ExportFormat::Fat32,
        2 => ExportFormat::Ext4,
        3 => ExportFormat::Ntfs,
        _ => return Err(error::invalid("format d'export inconnu")),
    };

    let report = fs::export_to_medium(call.arg(0) as usize, &source, format, who)
        .map_err(|e| error::from_fs(e, format.as_str()))?;
    Ok(report.files as u64)
}

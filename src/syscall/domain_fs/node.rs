//! 14.4.1 -- The calls by node identifier (LIFS, spec 6.2), `0x0130` to
//! `0x0135`.
//!
//! A volume is named by its letter (`/` for the root, as a character
//! code), a node by its identifier. The filesystem checks the rights of
//! chapter 5 exactly as by path; what it cannot check is the sandbox of
//! 12.4, which reasons in paths -- so the path of every node named is
//! rebuilt through the reverse index and put through the same `guard` as
//! a path argument. Without that, an identifier would be the way out of
//! the sandbox.

use alloc::vec::Vec;

use super::{encode_stat, guard, now, ready};
use crate::fs;
use crate::syscall::error::{self, ErrorCode, SysError, SysResult};
use crate::syscall::Call;

fn letter_at(call: &Call, index: usize) -> SysResult<char> {
    let value = call.arg(index);
    let letter = char::from_u32(value as u32).filter(|c| value <= 0x7F && (*c == fs::BOOT_LETTER || c.is_ascii_uppercase()));
    letter.ok_or_else(|| error::invalid("lettre de disque"))
}

/// A name of an entry, from a `(pointer, length)` pair.
fn name_at(call: &Call, index: usize) -> SysResult<alloc::string::String> {
    let length = call.arg(index + 1);
    if length > crate::fs::lifs::format::NAME_MAX as u64 {
        return Err(SysError::with(ErrorCode::NameTooLong, "nom"));
    }
    call.read_str(call.arg(index), length)
}

/// The sandbox of 12.4 on node `id` (and, with `name`, on the entry
/// `name` of directory `id`).
fn guard_node(call: &Call, letter: char, id: u64, name: Option<&str>) -> SysResult<alloc::string::String> {
    let mut path = fs::node_path(letter, id).map_err(|e| error::from_fs(e, "noeud"))?;
    if let Some(name) = name {
        if !path.ends_with('/') {
            path.push('/');
        }
        path.push_str(name);
    }
    guard(call, &path)?;
    Ok(path)
}

/// `NODE_STAT(letter, node, buffer, size)` -- the `STAT` record of a node.
pub(super) fn node_stat(call: &mut Call) -> SysResult<u64> {
    ready()?;
    let letter = letter_at(call, 0)?;
    let id = call.arg(1);
    let path = guard_node(call, letter, id, None)?;
    let node = fs::node_stat(letter, id, call.who()).map_err(|e| error::from_fs(e, &path))?;
    call.write_bytes(call.arg(2), call.arg(3), &encode_stat(&node))
}

/// `NODE_MOVE(letter, from directory, name, length, to directory)`.
pub(super) fn node_move(call: &mut Call) -> SysResult<u64> {
    ready()?;
    let letter = letter_at(call, 0)?;
    let (from, to) = (call.arg(1), call.arg(4));
    let name = name_at(call, 2)?;
    let path = guard_node(call, letter, from, Some(&name))?;
    guard_node(call, letter, to, Some(&name))?;
    fs::node_move(letter, from, &name, to, call.who(), now()).map_err(|e| error::from_fs(e, &path))?;
    Ok(0)
}

/// `NODE_CLONE(letter, node, to directory, name, length)` -- a CoW clone;
/// returns the new node.
pub(super) fn node_clone(call: &mut Call) -> SysResult<u64> {
    ready()?;
    let letter = letter_at(call, 0)?;
    let (id, to) = (call.arg(1), call.arg(2));
    let name = name_at(call, 3)?;
    let path = guard_node(call, letter, id, None)?;
    guard_node(call, letter, to, Some(&name))?;
    fs::node_clone(letter, id, to, &name, call.who(), now()).map_err(|e| error::from_fs(e, &path))
}

/// `NODE_TAG(letter, node, to directory, name, length)` -- one more Tag.
pub(super) fn node_tag(call: &mut Call) -> SysResult<u64> {
    ready()?;
    let letter = letter_at(call, 0)?;
    let (id, to) = (call.arg(1), call.arg(2));
    let name = name_at(call, 3)?;
    let path = guard_node(call, letter, id, None)?;
    guard_node(call, letter, to, Some(&name))?;
    fs::node_tag(letter, id, to, &name, call.who(), now()).map_err(|e| error::from_fs(e, &path))?;
    Ok(0)
}

/// `NODE_UNLINK(letter, directory, name, length)`.
pub(super) fn node_unlink(call: &mut Call) -> SysResult<u64> {
    ready()?;
    let letter = letter_at(call, 0)?;
    let dir = call.arg(1);
    let name = name_at(call, 2)?;
    let path = guard_node(call, letter, dir, Some(&name))?;
    fs::node_unlink(letter, dir, &name, call.who(), now()).map_err(|e| error::from_fs(e, &path))?;
    Ok(0)
}

/// Layout of a `NODE_TAGS` record.
///
/// ```text
///   0x000    8  directory node
///   0x008    2  length of the name
///   0x00A  254  name
/// ```
pub const TAG_ENTRY_SIZE: usize = 0x108;

/// `NODE_TAGS(letter, node, buffer, size)` -- the entries that tag a node,
/// in the directories the caller may list. Returns how many were written.
pub(super) fn node_tags(call: &mut Call) -> SysResult<u64> {
    ready()?;
    let letter = letter_at(call, 0)?;
    let id = call.arg(1);
    let path = guard_node(call, letter, id, None)?;
    let tags = fs::node_tags(letter, id, call.who()).map_err(|e| error::from_fs(e, &path))?;
    let capacity = (call.arg(3) as usize) / TAG_ENTRY_SIZE;
    let count = tags.len().min(capacity);
    let mut buffer = Vec::with_capacity(count * TAG_ENTRY_SIZE);
    for tag in tags.iter().take(count) {
        let mut record = [0u8; TAG_ENTRY_SIZE];
        record[0x00..0x08].copy_from_slice(&tag.directory.to_le_bytes());
        let name = tag.name.as_bytes();
        let length = name.len().min(TAG_ENTRY_SIZE - 0x0A);
        record[0x08..0x0A].copy_from_slice(&(length as u16).to_le_bytes());
        record[0x0A..0x0A + length].copy_from_slice(&name[..length]);
        buffer.extend_from_slice(&record);
    }
    call.write_bytes(call.arg(2), call.arg(3), &buffer)?;
    Ok(count as u64)
}

//! Filesystem side of the explorer: paths, listings, and the operations
//! that act on a whole tree.
//!
//! ## Why the tree operations are loops and not recursions
//!
//! Copying or deleting a directory means visiting everything under it, and
//! the depth of that tree is data read from the disk. DECISIONS.md forbids
//! a kernel recursion whose depth depends on outside data unless it checks
//! `scheduler::stack_headroom` -- task stacks have no guard page, and an
//! overflow is not reported. An explicit work list has no such limit to
//! check: the depth costs heap, not stack.
//!
//! Both are also bounded in **count** (`MAX_TREE_ITEMS`), so that a click
//! on a huge directory cannot keep the task busy for minutes with nothing
//! on screen saying so.
//!
//! ## Why a directory is never copied into itself
//!
//! The copy lists each directory when it reaches it. Copying `/a` into
//! `/a/b` creates `/a/b/a`, which the walk then reaches through `/a/b` and
//! copies again, forever. `is_inside` is checked before any work begins.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::format::{NAME_MAX, NodeKind};
use crate::fs::perm::{Credentials, Mode};
use crate::fs::FsError;

/// Upper bound on the entries one tree operation visits.
pub const MAX_TREE_ITEMS: usize = 4096;

/// One entry of the directory being shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// Could the node be read at all? An entry the account may see in its
    /// directory but not stat is still listed -- hiding it would make the
    /// directory look emptier than it is.
    pub known: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Kind,
    Size,
}

// ============================================================
//  Paths
// ============================================================

/// Canonical form of a path: disk prefix kept, `.` and `..` resolved.
///
/// `None` for something that is not a path at all -- no leading slash and
/// no disk letter.
pub fn canonical(path: &str) -> Option<String> {
    let (letter, rest) = crate::fs::split_disk(path).ok()?;
    let normalized = crate::fs::normalize(rest);
    Some(if letter == crate::fs::BOOT_LETTER {
        normalized
    } else {
        format!("{}:{}", letter, normalized)
    })
}

/// A directory and a name, joined by exactly one slash.
pub fn join(directory: &str, name: &str) -> String {
    if directory.ends_with('/') {
        format!("{}{}", directory, name)
    } else {
        format!("{}/{}", directory, name)
    }
}

/// The directory holding a path. `None` at the root of a disk.
pub fn parent(path: &str) -> Option<String> {
    let canonical = canonical(path)?;
    let (prefix, rest) = match canonical.find(":/") {
        Some(index) => (&canonical[..index + 1], &canonical[index + 1..]),
        None => ("", canonical.as_str()),
    };
    if rest == "/" {
        return None;
    }
    let cut = rest.rfind('/')?;
    let up = if cut == 0 { "/" } else { &rest[..cut] };
    Some(format!("{}{}", prefix, up))
}

/// The last component of a path.
pub fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Is `path` the same as `ancestor`, or somewhere under it?
///
/// Compared on canonical forms and on whole components: `/ab` is not inside
/// `/a`, which a plain `starts_with` would claim.
pub fn is_inside(path: &str, ancestor: &str) -> bool {
    let (Some(path), Some(ancestor)) = (canonical(path), canonical(ancestor)) else {
        return false;
    };
    if path == ancestor {
        return true;
    }
    let base = if ancestor.ends_with('/') {
        ancestor
    } else {
        format!("{}/", ancestor)
    };
    path.starts_with(&base)
}

/// Disk letter of a path, the boot disk being `/`.
pub fn disk_of(path: &str) -> char {
    crate::fs::split_disk(path)
        .map(|(letter, _)| letter)
        .unwrap_or(crate::fs::BOOT_LETTER)
}

/// Can this be the name of an entry?
///
/// The filesystem checks too; this is checked first so that the message
/// says what is wrong with the name rather than "nom de fichier invalide"
/// for four different reasons.
pub fn name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("nom vide");
    }
    if name == "." || name == ".." {
        return Some("nom reserve");
    }
    if name.contains('/') {
        return Some("le nom ne peut pas contenir /");
    }
    if name.len() > NAME_MAX {
        return Some("nom trop long (56 caracteres au plus)");
    }
    if !name.bytes().all(|byte| (0x20..=0x7E).contains(&byte)) {
        return Some("caracteres ASCII imprimables uniquement");
    }
    None
}

/// A name that is not in `taken`: `name`, else `name (copie)`, else
/// `name (copie 2)`, and so on. The extension is kept at the end.
///
/// The stem is shortened when the suffix would push the name past
/// `NAME_MAX`, rather than letting the filesystem refuse it.
pub fn unique_name(taken: &[String], name: &str) -> String {
    if !taken.iter().any(|existing| existing == name) {
        return String::from(name);
    }
    // A leading dot is part of the stem: `.profile` has no extension.
    let (stem, extension) = match name.rfind('.') {
        Some(index) if index > 0 => (&name[..index], &name[index..]),
        _ => (name, ""),
    };
    for rank in 1..1000 {
        let suffix = if rank == 1 {
            String::from(" (copie)")
        } else {
            format!(" (copie {})", rank)
        };
        let room = NAME_MAX.saturating_sub(suffix.len() + extension.len());
        let stem: String = stem.chars().take(room).collect();
        let candidate = format!("{}{}{}", stem, suffix, extension);
        if !taken.iter().any(|existing| *existing == candidate) {
            return candidate;
        }
    }
    String::from(name)
}

// ============================================================
//  Display helpers
// ============================================================

/// Size in the units 14.2 uses for storage, without a float: SSE is off.
pub fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    if bytes < KIB {
        return format!("{} o", bytes);
    }
    let (unit, name) = if bytes < MIB { (KIB, "Kio") } else { (MIB, "Mio") };
    let tenths = (bytes * 10 + unit / 2) / unit;
    format!("{}.{} {}", tenths / 10, tenths % 10, name)
}

/// What an entry is, from its extension -- the formats of 3.1 first.
pub fn kind_label(name: &str, is_dir: bool) -> &'static str {
    if is_dir {
        return "dossier";
    }
    let extension = match name.rfind('.') {
        Some(index) if index > 0 => &name[index + 1..],
        _ => "",
    };
    match extension {
        "dsa" => "systeme (.dsa)",
        "ss" => "logiciel systeme",
        "cus" => "application",
        "gs" => "jeu",
        "ras" => "acces direct (.ras)",
        "xlib" => "bibliotheque",
        "mod" => "module graphique",
        "pause" => "module en pause",
        "spm" => "paquet",
        "txt" | "md" | "log" => "texte",
        "wav" => "audio",
        "cfg" | "toml" | "lang" | "conf" => "configuration",
        "rs" | "c" | "js" | "py" | "asm" => "source",
        _ => "fichier",
    }
}

/// Extensions opened in the notepad without looking at the content.
fn is_text_extension(name: &str) -> bool {
    matches!(
        name.rsplit('.').next(),
        Some("txt" | "md" | "log" | "cfg" | "toml" | "lang" | "conf" | "rs" | "c" | "js" | "py" | "asm")
    ) && name.contains('.')
}

/// Does this content read as text? Valid UTF-8, and no control character
/// other than the three a text file uses.
pub fn looks_like_text(bytes: &[u8]) -> bool {
    match core::str::from_utf8(bytes) {
        Ok(text) => text
            .chars()
            .all(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t')),
        Err(_) => false,
    }
}

/// What opening an entry does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opening {
    Directory,
    /// A native application, by the file name `apps::launch` knows.
    Launch(&'static str),
    /// Handed to the notepad.
    Text,
    /// A WAV file, handed to the player.
    Audio,
    /// Nothing on the system reads it.
    Unknown,
}

/// Decides what opening `path` does. Reads the file only when the name
/// alone cannot tell.
pub fn opening(path: &str, item: &Item, who: Credentials) -> Result<Opening, FsError> {
    if item.is_dir {
        return Ok(Opening::Directory);
    }
    if let Some((file_name, _, _, _)) = crate::apps::native_for(&item.name) {
        return Ok(Opening::Launch(file_name));
    }
    // Read even for a known text extension: the notepad opens with its
    // own credentials, which are wider than the account's (see
    // `notepad::who`). Reading here with the account's is what keeps the
    // explorer from opening a file the account may not read.
    let content = crate::fs::read_file(path, who)?;
    // By content, not by name: a `.wav` that is not RIFF/WAVE would open
    // a player with nothing to play.
    if crate::audio::wav::parse(&content).is_ok() {
        return Ok(Opening::Audio);
    }
    if is_text_extension(&item.name) || looks_like_text(&content) {
        Ok(Opening::Text)
    } else {
        Ok(Opening::Unknown)
    }
}

// ============================================================
//  Listing
// ============================================================

/// Entries of a directory, sorted: directories first, then by `key`.
pub fn list(directory: &str, who: Credentials, key: SortKey, ascending: bool) -> Result<Vec<Item>, FsError> {
    let links = crate::fs::list(directory, who)?;
    let mut items: Vec<Item> = links
        .into_iter()
        .map(|link| {
            let path = join(directory, &link.name);
            match crate::fs::stat(&path, who) {
                Ok(node) => Item {
                    name: link.name,
                    is_dir: node.kind == NodeKind::Directory,
                    size: node.size,
                    known: true,
                },
                Err(_) => Item {
                    name: link.name,
                    is_dir: false,
                    size: 0,
                    known: false,
                },
            }
        })
        .collect();
    sort(&mut items, key, ascending);
    Ok(items)
}

pub fn sort(items: &mut [Item], key: SortKey, ascending: bool) {
    items.sort_by(|a, b| {
        // Directories stay on top whatever the direction: reversing the
        // order must not scatter them through the files.
        let group = b.is_dir.cmp(&a.is_dir);
        if group != core::cmp::Ordering::Equal {
            return group;
        }
        let by_name = compare_names(&a.name, &b.name);
        let order = match key {
            SortKey::Name => by_name,
            SortKey::Kind => kind_label(&a.name, a.is_dir)
                .cmp(kind_label(&b.name, b.is_dir))
                .then(by_name),
            SortKey::Size => a.size.cmp(&b.size).then(by_name),
        };
        if ascending { order } else { order.reverse() }
    });
}

/// Case-insensitive on ASCII, then case-sensitive so the order is total.
fn compare_names(a: &str, b: &str) -> core::cmp::Ordering {
    let folded = a
        .bytes()
        .map(|byte| byte.to_ascii_lowercase())
        .cmp(b.bytes().map(|byte| byte.to_ascii_lowercase()));
    folded.then_with(|| a.cmp(b))
}

// ============================================================
//  Tree operations
// ============================================================

/// Why a tree operation stopped: the path it was on, and the error.
pub type Failure = (String, FsError);

fn mode_of(path: &str, who: Credentials) -> Result<Mode, Failure> {
    let node = crate::fs::stat(path, who).map_err(|error| (String::from(path), error))?;
    Ok(Mode {
        owner: node.perm_owner,
        group: node.perm_group,
        other: node.perm_other,
    })
}

/// Copies a file or a whole directory to `to`. Returns the entries copied.
///
/// The caller has checked that `to` is not inside `from`.
pub fn copy_tree(from: &str, to: &str, who: Credentials, now: u64) -> Result<usize, Failure> {
    let root = crate::fs::stat(from, who).map_err(|error| (String::from(from), error))?;
    if root.kind != NodeKind::Directory {
        crate::fs::copy(from, to, who, now).map_err(|error| (String::from(from), error))?;
        return Ok(1);
    }

    let mut work: Vec<(String, String)> = alloc::vec![(String::from(from), String::from(to))];
    let mut copied = 0;
    while let Some((source, target)) = work.pop() {
        // Same masks as the source: a copy that quietly widened or
        // narrowed the permissions would be a copy of something else.
        let mode = mode_of(&source, who)?;
        crate::fs::create_directory(&target, who, mode, now)
            .map_err(|error| (target.clone(), error))?;
        copied += 1;

        let children =
            crate::fs::list(&source, who).map_err(|error| (source.clone(), error))?;
        for child in children {
            if copied + work.len() >= MAX_TREE_ITEMS {
                return Err((source, FsError::Unsupported));
            }
            let child_source = join(&source, &child.name);
            let child_target = join(&target, &child.name);
            let node = crate::fs::stat(&child_source, who)
                .map_err(|error| (child_source.clone(), error))?;
            if node.kind == NodeKind::Directory {
                work.push((child_source, child_target));
            } else {
                crate::fs::copy(&child_source, &child_target, who, now)
                    .map_err(|error| (child_source.clone(), error))?;
                copied += 1;
            }
        }
    }
    Ok(copied)
}

/// Every path of a tree, parents before children.
fn walk(root: &str, who: Credentials) -> Result<Vec<String>, Failure> {
    let mut order = alloc::vec![String::from(root)];
    let mut index = 0;
    while index < order.len() {
        let path = order[index].clone();
        index += 1;
        let node = crate::fs::stat(&path, who).map_err(|error| (path.clone(), error))?;
        if node.kind != NodeKind::Directory {
            continue;
        }
        let children = crate::fs::list(&path, who).map_err(|error| (path.clone(), error))?;
        for child in children {
            if order.len() >= MAX_TREE_ITEMS {
                return Err((path, FsError::Unsupported));
            }
            order.push(join(&path, &child.name));
        }
    }
    Ok(order)
}

/// Deletes a file or a whole directory. Returns the entries deleted.
///
/// The whole tree is walked before anything is deleted, then deleted
/// children first. A permission refused deep down therefore stops the
/// deletion with the top of the tree intact -- and what was deleted before
/// the refusal stays deleted, which the message says.
pub fn delete_tree(path: &str, who: Credentials, now: u64) -> Result<usize, (usize, Failure)> {
    let order = walk(path, who).map_err(|failure| (0, failure))?;
    let mut deleted = 0;
    for entry in order.iter().rev() {
        crate::fs::delete(entry, who, now).map_err(|error| (deleted, (entry.clone(), error)))?;
        deleted += 1;
    }
    Ok(deleted)
}

/// Moves a file or a directory to `to`.
///
/// Inside a disk, a rewrite of links (4.1). Across disks the filesystem
/// moves files but not directories -- its cross-volume path reads the
/// source as one file -- so a directory is copied, then deleted.
pub fn move_tree(from: &str, to: &str, who: Credentials, now: u64) -> Result<(), Failure> {
    if disk_of(from) == disk_of(to) {
        return crate::fs::move_entry(from, to, who, now)
            .map(|_| ())
            .map_err(|error| (String::from(from), error));
    }
    copy_tree(from, to, who, now)?;
    delete_tree(from, who, now).map_err(|(_, failure)| failure)?;
    Ok(())
}

/// One line for a failure, in the words of the filesystem.
pub fn describe(failure: &Failure) -> String {
    let (path, error) = failure;
    let detail = error.detail();
    let reason = if *error == FsError::Unsupported {
        String::from("plus de 4096 elements - operation interrompue")
    } else if detail.is_empty() {
        String::from(error.message())
    } else {
        format!("{} ({})", error.message(), detail)
    };
    format!("{} : {}", file_name(path), reason)
}

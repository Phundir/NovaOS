//! Database of the installed packages.
//!
//! ## Why a database, and not a scan
//!
//! One could derive the list of installed packages by walking `/apps/`,
//! `/games/` and the rest. Three things would be lost, and they are
//! exactly those `spm remove` and `spm update` need:
//!
//! * **the scope** -- a package laid down under `--user` and the same
//!   package laid down globally occupy different paths, but nothing in a
//!   directory says which was asked for;
//! * **the provenance** -- which repository supplied the package, hence
//!   where to go looking for its update;
//! * **the file list** -- a package lays down its binary, its resources
//!   and whatever its post-installation script created. Guessing that
//!   list at uninstall time would mean deleting what one believes belongs
//!   to it, which is the best way to carry off the user's data.
//!
//! ## Two files per scope
//!
//! `installed.db` holds one line per package, fields separated by `|`:
//!
//! ```text
//!   app_id|version|format|signature|code|prefix|repository|epoch|state
//! ```
//!
//! and `files/{app_id}.list` holds one path per line, in creation order.
//! Uninstalling reads them **backwards**: a directory is only deleted
//! once empty.
//!
//! The `|` separator appears in no field: the paths contain none, and the
//! identifiers are filtered when the package is parsed. A field
//! containing one would make the line lose its alignment, and that is why
//! writing refuses it.

use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::header::{BinaryFormat, SignatureType};
use crate::fs::perm::{Credentials, Mode};

use super::package::UpdateCode;

/// Working directory of the manager (spec 17).
pub const SPM_DIR: &str = "/sys/tools/spm";

/// Scope of an installation (spec 10.3, `--user`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Global installation, visible to every account.
    Global,
    /// Personal installation of one account.
    User(String),
}

impl Scope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::User(_) => "utilisateur",
        }
    }

    /// Directory where the scope files its metadata.
    pub fn root(&self) -> String {
        match self {
            Scope::Global => String::from(SPM_DIR),
            Scope::User(name) => alloc::format!("/users/{}/settings/spm", name),
        }
    }

    pub fn database(&self) -> String {
        alloc::format!("{}/installed.db", self.root())
    }

    pub fn files_dir(&self) -> String {
        alloc::format!("{}/files", self.root())
    }

    /// Root under which the scope installs the applications.
    ///
    /// Spec 17 gives `/users/{user}/apps/` for the personal
    /// applications; a global installation follows the 12.4 table, which
    /// files by format and not by account.
    pub fn install_root(&self, format: BinaryFormat) -> String {
        match self {
            Scope::Global => match format {
                BinaryFormat::Dsa | BinaryFormat::Ss => String::from("/sysapp"),
                BinaryFormat::Gs => String::from("/games"),
                BinaryFormat::Cus => String::from("/apps"),
                BinaryFormat::Ras => String::from("/ras"),
            },
            Scope::User(name) => alloc::format!("/users/{}/apps", name),
        }
    }
}

/// State of a package in the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Installation carried to its end, scripts included.
    Installed,
    /// Files laid down, post-installation script failed. The package may
    /// work, but `spm update` must take it up again.
    Partial,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Installed => "installe",
            Status::Partial => "partiel",
        }
    }

    fn from_str(text: &str) -> Status {
        match text {
            "partiel" => Status::Partial,
            _ => Status::Installed,
        }
    }
}

/// One line of the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub app_id: String,
    pub version: [u16; 4],
    pub format: BinaryFormat,
    pub signature: SignatureType,
    pub update_code: UpdateCode,
    /// Effective installation prefix.
    pub prefix: String,
    /// Name of the repository of origin, or `local` for a file.
    pub origin: String,
    /// Installation date, in seconds since the epoch.
    pub installed_at: u64,
    pub status: Status,
}

impl Entry {
    pub fn version_string(&self) -> String {
        alloc::format!(
            "{}.{}.{}.{}",
            self.version[0],
            self.version[1],
            self.version[2],
            self.version[3]
        )
    }

    fn to_line(&self) -> String {
        alloc::format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}\n",
            self.app_id,
            self.version_string(),
            self.format as u8,
            self.signature as u8,
            self.update_code.code(),
            self.prefix,
            self.origin,
            self.installed_at,
            self.status.as_str()
        )
    }

    fn from_line(line: &str) -> Option<Entry> {
        let fields: Vec<&str> = line.split('|').collect();
        if fields.len() < 9 {
            return None;
        }
        Some(Entry {
            app_id: String::from(fields[0]),
            version: parse_version(fields[1])?,
            format: BinaryFormat::from_code(fields[2].parse().ok()?)?,
            signature: SignatureType::from_code(fields[3].parse().ok()?)?,
            update_code: UpdateCode::from_code(fields[4].parse().ok()?)?,
            prefix: String::from(fields[5]),
            origin: String::from(fields[6]),
            installed_at: fields[7].parse().ok()?,
            status: Status::from_str(fields[8]),
        })
    }
}

/// Decodes `major.minor.patch.build`.
///
/// The absent fields are zero: `1.2` is `1.2.0.0`. A repository often
/// writes the short version, and refusing the whole line for that would
/// make the index unreadable over a matter of presentation.
pub fn parse_version(text: &str) -> Option<[u16; 4]> {
    let mut version = [0u16; 4];
    let mut seen = 0;
    for (index, part) in text.trim().split('.').enumerate() {
        if index >= 4 {
            return None;
        }
        version[index] = part.trim().parse().ok()?;
        seen += 1;
    }
    if seen == 0 { None } else { Some(version) }
}

pub fn version_string(version: &[u16; 4]) -> String {
    alloc::format!(
        "{}.{}.{}.{}",
        version[0],
        version[1],
        version[2],
        version[3]
    )
}

/// Does a field contain the separator?
fn field_safe(text: &str) -> bool {
    !text.contains('|') && !text.contains('\n')
}

/// Makes sure the directories of the scope exist.
fn ensure_dirs(scope: &Scope, now: u64) -> Result<(), crate::fs::FsError> {
    let who = Credentials::kernel();
    for path in [scope.root(), scope.files_dir()] {
        if crate::fs::stat(&path, who).is_err() {
            crate::fs::create_directory(&path, who, Mode::system_dir(), now)?;
        }
    }
    Ok(())
}

/// Reads the database of a scope. A scope with no database is an empty
/// scope, not an error: an account that has never installed anything.
pub fn load(scope: &Scope) -> Vec<Entry> {
    let who = Credentials::kernel();
    let Ok(bytes) = crate::fs::read_file(&scope.database(), who) else {
        return Vec::new();
    };
    let Ok(text) = core::str::from_utf8(&bytes) else {
        return Vec::new();
    };

    text.lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(Entry::from_line)
        .collect()
}

/// Rewrites the whole database.
///
/// Rewriting rather than appending: two lines carrying the same `app_id`
/// would make the result depend on the reading order, and an uninstall
/// would then have to remove every occurrence instead of one. It is the
/// same trade-off as `prompt::remember`.
pub fn save(scope: &Scope, entries: &[Entry], now: u64) -> Result<(), crate::fs::FsError> {
    ensure_dirs(scope, now)?;
    let who = Credentials::kernel();

    let mut content = String::from("# paquets installes - spec 10, un paquet par ligne\n");
    for entry in entries {
        content.push_str(&entry.to_line());
    }

    let path = scope.database();
    let _ = crate::fs::delete(&path, who, now);
    crate::fs::write_file(&path, content.as_bytes(), who, Mode::system_file(), now).map(|_| ())
}

/// Looks for a package in a scope.
pub fn find(scope: &Scope, app_id: &str) -> Option<Entry> {
    load(scope)
        .into_iter()
        .find(|entry| entry.app_id == app_id)
}

/// Inserts or replaces an entry.
pub fn upsert(scope: &Scope, entry: Entry, now: u64) -> Result<(), crate::fs::FsError> {
    if !field_safe(&entry.app_id) || !field_safe(&entry.prefix) || !field_safe(&entry.origin) {
        return Err(crate::fs::FsError::InvalidPath);
    }
    let mut entries = load(scope);
    match entries.iter_mut().find(|item| item.app_id == entry.app_id) {
        Some(existing) => *existing = entry,
        None => entries.push(entry),
    }
    save(scope, &entries, now)
}

/// Removes an entry. Returns `false` if it was not there.
pub fn remove(scope: &Scope, app_id: &str, now: u64) -> Result<bool, crate::fs::FsError> {
    let mut entries = load(scope);
    let before = entries.len();
    entries.retain(|entry| entry.app_id != app_id);
    if entries.len() == before {
        return Ok(false);
    }
    save(scope, &entries, now)?;
    Ok(true)
}

// ============================================================
//  File list of a package
// ============================================================

fn files_path(scope: &Scope, app_id: &str) -> String {
    alloc::format!("{}/{}.list", scope.files_dir(), app_id)
}

/// Records the files laid down by a package, in creation order.
pub fn record_files(
    scope: &Scope,
    app_id: &str,
    files: &[String],
    now: u64,
) -> Result<(), crate::fs::FsError> {
    ensure_dirs(scope, now)?;
    let who = Credentials::kernel();
    let path = files_path(scope, app_id);

    let mut content = String::new();
    for file in files {
        content.push_str(file);
        content.push('\n');
    }

    let _ = crate::fs::delete(&path, who, now);
    crate::fs::write_file(&path, content.as_bytes(), who, Mode::system_file(), now).map(|_| ())
}

/// Files of a package, in creation order.
pub fn files_of(scope: &Scope, app_id: &str) -> Vec<String> {
    let who = Credentials::kernel();
    let Ok(bytes) = crate::fs::read_file(&files_path(scope, app_id), who) else {
        return Vec::new();
    };
    let Ok(text) = core::str::from_utf8(&bytes) else {
        return Vec::new();
    };
    text.lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect()
}

/// Forgets the file list of a package.
pub fn forget_files(scope: &Scope, app_id: &str, now: u64) {
    let _ = crate::fs::delete(&files_path(scope, app_id), Credentials::kernel(), now);
}

/// Scopes to consult for a given account.
///
/// The order matters: the personal wins over the global, like the GUI
/// modules of 8.4. A user installing their own version of an application
/// must get theirs.
pub fn scopes_for(user: Option<&str>) -> Vec<Scope> {
    match user {
        Some(name) => alloc::vec![Scope::User(String::from(name)), Scope::Global],
        None => alloc::vec![Scope::Global],
    }
}

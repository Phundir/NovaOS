//! 4.6 -- Multi-compatibility: export to a foreign format (spec 4.4).
//!
//! A Nova OS volume is unreadable elsewhere: the virtual links of 4.1
//! have no equivalent in any common filesystem. Transferring data to a
//! medium that will be plugged into another machine therefore means
//! **rewriting** the tree in a known format, accepting the loss of what
//! that format cannot express.
//!
//! ## What is lost on export
//!
//! | Nova OS notion | FAT32 | ext4 | NTFS |
//! |---|---|---|---|
//! | `read` / `write` / `execute` | "read only" attribute | POSIX bits | security descriptor |
//! | The ten other permissions of 5 | lost | lost | lost |
//! | Owner and group | lost | `i_uid` / `i_gid` | lost |
//! | Multiple links to a node | each link becomes a copy | hard link | each link becomes a copy |
//! | Names up to 55 characters | 8.3 + LFN entries | as-is | as-is, in UTF-16 |
//!
//! The user must be warned of those losses: that is the role of the
//! prompt of 4.4, which makes them choose the destination format.
//!
//! ## One walk, three writings
//!
//! The three exporters share the same shape: depth-first walk of the
//! source tree through `list` / `stat` / `read_file`, immediate writing
//! of the file content, and deferral of the metadata to the end of the
//! walk when its final location is known. They share no code beyond
//! `join_path`: the three formats have neither the same allocation unit,
//! nor the same way of naming, nor the same write order, and a common
//! abstraction would cost more than it would factor out.
//!
//! ## Protection against cycles
//!
//! Spec 5 allows `link` on a directory: the tree is therefore a graph,
//! not a tree, and nothing forbids a directory containing itself through
//! a link. Each exporter remembers the node identifiers already visited
//! and refuses to descend into them again -- failing which the walk would
//! not terminate.

pub mod ext4;
pub mod fat32;
pub mod ntfs;

use alloc::string::String;
use alloc::vec::Vec;

use super::block::BlockDevice;
use super::perm::Credentials;
use super::FsError;

/// Destination formats offered by spec 4.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// Native format: virtual links kept, unreadable elsewhere.
    Native,
    /// Maximum compatibility.
    Fat32,
    /// Linux.
    Ext4,
    /// Windows.
    Ntfs,
}

impl ExportFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExportFormat::Native => "natif Nova OS",
            ExportFormat::Fat32 => "FAT32",
            ExportFormat::Ext4 => "ext4",
            ExportFormat::Ntfs => "NTFS",
        }
    }

    /// What the user loses by choosing this format.
    pub fn warning(&self) -> &'static str {
        match self {
            ExportFormat::Native => "aucune perte - illisible hors de Nova OS",
            ExportFormat::Fat32 => {
                "permissions, proprietaire et liens multiples perdus ; 4 Gio par fichier"
            }
            ExportFormat::Ext4 => {
                "permissions ramenees aux neuf bits POSIX ; liens durs et proprietaire conserves"
            }
            ExportFormat::Ntfs => {
                "permissions ramenees a un descripteur unique ; chaque lien devient une copie"
            }
        }
    }

    /// Can this kernel write this format?
    pub fn implemented(&self) -> bool {
        !matches!(self, ExportFormat::Native)
    }
}

/// What an export produced.
#[derive(Debug, Clone)]
pub struct ExportReport {
    pub format: ExportFormat,
    pub files: usize,
    pub directories: usize,
    pub bytes: u64,
    /// Entries set aside, with the reason.
    pub skipped: Vec<String>,
}

impl ExportReport {
    pub fn new(format: ExportFormat) -> Self {
        ExportReport {
            format,
            files: 0,
            directories: 0,
            bytes: 0,
            skipped: Vec::new(),
        }
    }

    pub fn dump(&self) {
        crate::serial_println!(
            "[export] {} - {} fichiers, {} dossiers, {} octets",
            self.format.as_str(),
            self.files,
            self.directories,
            self.bytes
        );
        crate::serial_println!("[export] perte annoncee : {}", self.format.warning());
        for reason in self.skipped.iter() {
            crate::serial_println!("[export]   ecarte : {}", reason);
        }
    }
}

/// 4.6.4 -- Installation setting.
///
/// Spec 4.4 makes multi-compatibility an **option enabled at install
/// time**, not a default behaviour: writing FAT32 assumes embedding a
/// second filesystem in the kernel, which not everybody wants to pay for.
pub struct Settings {
    pub enabled: bool,
    pub default_format: ExportFormat,
}

impl Settings {
    pub const fn new() -> Self {
        Settings {
            enabled: true,
            default_format: ExportFormat::Fat32,
        }
    }

    /// Formats offered to the user at the moment of a transfer.
    pub fn offered(&self) -> Vec<ExportFormat> {
        if !self.enabled {
            return alloc::vec![ExportFormat::Native];
        }
        alloc::vec![
            ExportFormat::Native,
            ExportFormat::Fat32,
            ExportFormat::Ext4,
            ExportFormat::Ntfs,
        ]
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self::new()
    }
}

/// Exports a Nova OS tree to a foreign medium.
///
/// `source` is a Nova OS path; `target` a blank block medium, which will
/// be **entirely rewritten**.
pub fn export(
    source: &str,
    target: &mut dyn BlockDevice,
    format: ExportFormat,
    who: Credentials,
) -> Result<ExportReport, FsError> {
    match format {
        ExportFormat::Fat32 => fat32::export(source, target, who),
        ExportFormat::Ext4 => ext4::export(source, target, who),
        ExportFormat::Ntfs => ntfs::export(source, target, who),
        // The native format is not an export: it is a volume-to-volume
        // copy, which assumes carrying the node table as-is. It is the
        // business of `SYS_FS_DISK_*`, not of this layer.
        ExportFormat::Native => Err(FsError::Unsupported),
    }
}

/// Concatenates a directory path and an entry name.
///
/// The exported root is the only path that may already end with `/`;
/// elsewhere, the separator is always missing. The three exporters need
/// it on every entry walked.
pub fn join_path(directory: &str, name: &str) -> String {
    if directory.ends_with('/') {
        alloc::format!("{}{}", directory, name)
    } else {
        alloc::format!("{}/{}", directory, name)
    }
}

/// Reads the image produced back and checks that it really carries the
/// signature of the announced format.
///
/// It is the indispensable counterpart of the export: a write that
/// "succeeds" proves nothing until the image has been read back by the
/// path a host would take. Returns two quantities proper to the format,
/// meant for the trace: total allocation unit and grouping unit.
/// regroupement.
pub fn verify(
    target: &mut dyn BlockDevice,
    format: ExportFormat,
) -> Result<(u32, u32), FsError> {
    match format {
        ExportFormat::Fat32 => fat32::verify(target),
        ExportFormat::Ext4 => ext4::verify(target),
        ExportFormat::Ntfs => ntfs::verify(target),
        ExportFormat::Native => Err(FsError::Unsupported),
    }
}

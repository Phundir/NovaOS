//! 8.3 -- Package repositories (spec 10.3, `spm repo add`).
//!
//! ## What a repository is here
//!
//! A **text index** and `.spm` files filed beside it. Nothing more: no
//! database, no negotiation protocol, no transitive dependency
//! resolution on the server side. A repository must be servable by
//! anything able to return a file, including a directory on a local disk
//! -- and that is exactly what makes the phase testable with no server.
//!
//! ```text
//!   # nova-spm-index v1
//!   pkg|<app_id>|<version>|<format>|<signature>|<code>|<size>|<digest>|<path>
//!   os|<version>|<type>|<size>|<digest>|<path>
//!   ancre|<digest>
//! ```
//!
//! The `pkg` lines describe applications, the `os` lines system updates
//! (11.1). The `digest` is that of the **whole file**, in lowercase
//! hexadecimal.
//!
//! ## Two checks, two questions (8.3.2)
//!
//! They are distinct and both necessary:
//!
//! 1. **Is the index the one the repository published?** The last line
//!    `ancre|<digest>` carries `SHA-256(preceding content || anchor of
//!    the repository)`, the anchor being fixed at `repo add` time. An
//!    index modified in transit does not reproduce that value.
//! 2. **Is the file received the one the index announces?** Its digest is
//!    recomputed and compared with the line. It is that check which
//!    detects a truncation, a transfer error, or a substituted file.
//!
//! Then comes, separately, the check of the **package signature** by
//! `package::verify` -- which answers "who produced it", a third
//! question. Confusing the three would suggest that a successful download
//! amounts to trust.
//!
//! The same reservation as in phase 3 applies to the anchor: it is a
//! shared secret passed through a hash, not a signature. It binds the
//! index to somebody who knew the anchor; it does not prove who. The day
//! Ed25519 exists, the anchor will become a public key and this function
//! will be the only one to change.
//!
//! ## Transport
//!
//! Three schemes. `file:` reads a path of the filesystem -- a repository
//! on a USB stick, on a second volume, or laid down by the installer.
//! `http://` and `https://` go through `spm::fetch`, the HTTP/1.1 and TLS
//! client of the browser with redirects followed (phase 15; before it, a
//! hand-made HTTP/1.0 request in clear). The network schemes are not
//! exercised by the self-test against a real server, for want of one
//! under QEMU: the transfer itself is, on a scripted connection
//! (`spm::selftest_atomic`).

use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::header::{BinaryFormat, SignatureType};
use crate::binfmt::sha256::{self, DIGEST_SIZE};
use crate::fs::perm::{Credentials, Mode};

use super::db::{self, parse_version};
use super::package::UpdateCode;

/// Configuration file of the repositories.
pub const CONFIG_PATH: &str = "/sys/tools/spm/repos.cfg";

/// Local cache of the fetched indexes.
pub const CACHE_DIR: &str = "/sys/tools/spm/cache";

/// Header every index must carry.
pub const INDEX_HEADER: &str = "# nova-spm-index v1";

/// Maximum size of an index. Beyond that, it is no longer a repository
/// index but a stream one does not want to hold in memory in one piece.
pub const MAX_INDEX: usize = 64 * 1024;

/// Maximum size of a fetched package.
pub const MAX_PACKAGE: usize = 4 * 1024 * 1024;

// ============================================================
//  Declaring a repository
// ============================================================

/// A declared repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub name: String,
    pub url: String,
    /// Trust anchor, as `repo add` fixed it. All zero: the repository has
    /// none, and its index is only checked for its form.
    pub anchor: [u8; DIGEST_SIZE],
}

impl Repo {
    pub fn has_anchor(&self) -> bool {
        self.anchor != [0u8; DIGEST_SIZE]
    }

    fn to_line(&self) -> String {
        if self.has_anchor() {
            alloc::format!("{}|{}|{}\n", self.name, self.url, hex(&self.anchor))
        } else {
            alloc::format!("{}|{}|\n", self.name, self.url)
        }
    }

    fn from_line(line: &str) -> Option<Repo> {
        let fields: Vec<&str> = line.split('|').collect();
        if fields.len() < 2 || fields[0].is_empty() || fields[1].is_empty() {
            return None;
        }
        let anchor = match fields.get(2) {
            Some(text) if !text.trim().is_empty() => unhex(text.trim())?,
            _ => [0u8; DIGEST_SIZE],
        };
        Some(Repo {
            name: String::from(fields[0].trim()),
            url: String::from(fields[1].trim()),
            anchor,
        })
    }
}

/// Digest in lowercase hexadecimal.
pub fn hex(digest: &[u8; DIGEST_SIZE]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(DIGEST_SIZE * 2);
    for byte in digest {
        text.push(DIGITS[(byte >> 4) as usize] as char);
        text.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    text
}

/// Decodes a hexadecimal digest. `None` as soon as a character is not
/// one: a half-read digest is worse than an absent digest, because it
/// looks like a value.
pub fn unhex(text: &str) -> Option<[u8; DIGEST_SIZE]> {
    let bytes = text.as_bytes();
    if bytes.len() != DIGEST_SIZE * 2 {
        return None;
    }
    let mut digest = [0u8; DIGEST_SIZE];
    for (index, slot) in digest.iter_mut().enumerate() {
        let high = (bytes[index * 2] as char).to_digit(16)?;
        let low = (bytes[index * 2 + 1] as char).to_digit(16)?;
        *slot = (high * 16 + low) as u8;
    }
    Some(digest)
}

// ============================================================
//  Index
// ============================================================

/// An application offered by a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageEntry {
    pub app_id: String,
    pub version: [u16; 4],
    pub format: BinaryFormat,
    pub signature: SignatureType,
    pub update_code: UpdateCode,
    pub size: u32,
    pub digest: [u8; DIGEST_SIZE],
    /// Path relative to the URL of the repository.
    pub path: String,
}

/// An OS update offered by a repository (spec 11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsEntry {
    pub version: [u16; 4],
    pub kind: super::update::OsUpdateKind,
    pub size: u32,
    pub digest: [u8; DIGEST_SIZE],
    pub path: String,
}

/// Decoded index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Index {
    pub packages: Vec<PackageEntry>,
    pub os: Vec<OsEntry>,
    /// Value of the `ancre` line, if it exists.
    pub seal: Option<[u8; DIGEST_SIZE]>,
}

/// Reason for refusing a repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoError {
    UnknownScheme,
    NotFound,
    TooLarge,
    NetworkDown,
    ConnectFailed,
    Truncated,
    MalformedIndex,
    SealMissing,
    SealMismatch,
    DigestMismatch,
    UnknownPackage,
    WriteFailed,
}

impl RepoError {
    pub fn message(&self) -> &'static str {
        match self {
            RepoError::UnknownScheme => "schema d'URL non gere - file:, http:// ou https:// attendus",
            RepoError::NotFound => "ressource introuvable dans le depot",
            RepoError::TooLarge => "ressource plus volumineuse que la limite",
            RepoError::NetworkDown => "aucune interface reseau configuree",
            RepoError::ConnectFailed => "connexion au depot impossible",
            RepoError::Truncated => "reponse tronquee",
            RepoError::MalformedIndex => "index illisible ou en-tete absent",
            RepoError::SealMissing => "depot ancre mais index sans ligne ancre",
            RepoError::SealMismatch => "l'index ne correspond pas a l'ancre du depot",
            RepoError::DigestMismatch => "le fichier recu ne correspond pas a l'index",
            RepoError::UnknownPackage => "paquet absent de l'index",
            RepoError::WriteFailed => "ecriture impossible dans le cache",
        }
    }
}

/// Seals an index: `SHA-256(content || anchor)`.
///
/// The anchor comes **after** the content and not before: a hash whose
/// secret precedes the data can be extended (length extension attack),
/// and although no useful extension is possible on a line-by-line format,
/// taking the opposite habit costs nothing.
pub fn seal_of(content: &str, anchor: &[u8; DIGEST_SIZE]) -> [u8; DIGEST_SIZE] {
    let mut hasher = sha256::Sha256::new();
    hasher.update(content.as_bytes());
    hasher.update(anchor);
    hasher.finish()
}

/// Parses an index.
///
/// A malformed line is **ignored**, not fatal: a newer repository may
/// publish records of a type this kernel does not know, and refusing the
/// whole index for that would make any evolution of the format
/// impossible. What is fatal is the absence of a header -- a file that is
/// not an index must not be read as if it were.
pub fn parse_index(text: &str) -> Result<Index, RepoError> {
    let mut lines = text.lines();
    match lines.next() {
        Some(first) if first.trim() == INDEX_HEADER => {}
        _ => return Err(RepoError::MalformedIndex),
    }

    let mut index = Index::default();
    for line in lines {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('|').collect();
        match fields[0] {
            "pkg" if fields.len() >= 9 => {
                let entry = (|| {
                    Some(PackageEntry {
                        app_id: String::from(fields[1]),
                        version: parse_version(fields[2])?,
                        format: BinaryFormat::from_code(fields[3].parse().ok()?)?,
                        signature: SignatureType::from_code(fields[4].parse().ok()?)?,
                        update_code: UpdateCode::from_code(fields[5].parse().ok()?)?,
                        size: fields[6].parse().ok()?,
                        digest: unhex(fields[7])?,
                        path: String::from(fields[8]),
                    })
                })();
                if let Some(entry) = entry {
                    index.packages.push(entry);
                }
            }
            "os" if fields.len() >= 6 => {
                let entry = (|| {
                    Some(OsEntry {
                        version: parse_version(fields[1])?,
                        kind: super::update::OsUpdateKind::from_code(fields[2].parse().ok()?)?,
                        size: fields[3].parse().ok()?,
                        digest: unhex(fields[4])?,
                        path: String::from(fields[5]),
                    })
                })();
                if let Some(entry) = entry {
                    index.os.push(entry);
                }
            }
            "ancre" if fields.len() >= 2 => {
                index.seal = unhex(fields[1].trim());
            }
            _ => {}
        }
    }

    Ok(index)
}

/// Portion of an index covered by the seal: everything but the `ancre` line.
///
/// The seal cannot cover itself, for the same reason that the digest of a
/// package header must be neutralised before being computed.
pub fn sealed_content(text: &str) -> String {
    let mut content = String::new();
    for line in text.lines() {
        if line.trim_start().starts_with("ancre|") {
            continue;
        }
        content.push_str(line);
        content.push('\n');
    }
    content
}

/// Checks the seal of an index against a repository.
pub fn check_seal(repo: &Repo, text: &str, index: &Index) -> Result<(), RepoError> {
    if !repo.has_anchor() {
        return Ok(());
    }
    let Some(seal) = index.seal else {
        return Err(RepoError::SealMissing);
    };
    let expected = seal_of(&sealed_content(text), &repo.anchor);
    if sha256::equal(&seal, &expected) {
        Ok(())
    } else {
        Err(RepoError::SealMismatch)
    }
}

/// Builds an index from what a repository offers.
///
/// Used by the self-test and by the repository-building tool: with no
/// producer, the format would only be checked in one direction.
pub fn build_index(
    packages: &[PackageEntry],
    os: &[OsEntry],
    anchor: Option<&[u8; DIGEST_SIZE]>,
) -> String {
    let mut content = String::from(INDEX_HEADER);
    content.push('\n');
    for entry in packages {
        content.push_str(&alloc::format!(
            "pkg|{}|{}|{}|{}|{}|{}|{}|{}\n",
            entry.app_id,
            db::version_string(&entry.version),
            entry.format as u8,
            entry.signature as u8,
            entry.update_code.code(),
            entry.size,
            hex(&entry.digest),
            entry.path
        ));
    }
    for entry in os {
        content.push_str(&alloc::format!(
            "os|{}|{}|{}|{}|{}\n",
            db::version_string(&entry.version),
            entry.kind.code(),
            entry.size,
            hex(&entry.digest),
            entry.path
        ));
    }
    if let Some(anchor) = anchor {
        let seal = seal_of(&content, anchor);
        content.push_str(&alloc::format!("ancre|{}\n", hex(&seal)));
    }
    content
}

// ============================================================
//  Transport
// ============================================================

/// Assembles the URL of a resource of a repository.
fn resource_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    alloc::format!("{}/{}", base, path)
}

/// Fetches a resource, whatever the scheme (`spm::fetch`: `file:`,
/// `http://`, `https://`).
pub fn fetch(url: &str, limit: usize) -> Result<Vec<u8>, RepoError> {
    if let Some(path) = url.strip_prefix("file:") {
        return fetch_file(path, limit);
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(RepoError::UnknownScheme);
    }
    if !crate::net::interface().configured() {
        return Err(RepoError::NetworkDown);
    }
    super::fetch::to_memory(url, limit).map_err(|message| {
        crate::serial_println!("[spm] {} : {}", url, message);
        if message.contains("limite") {
            RepoError::TooLarge
        } else if message.contains("404") {
            RepoError::NotFound
        } else {
            RepoError::ConnectFailed
        }
    })
}

fn fetch_file(path: &str, limit: usize) -> Result<Vec<u8>, RepoError> {
    let bytes = crate::fs::read_file(path, Credentials::kernel())
        .map_err(|_| RepoError::NotFound)?;
    if bytes.len() > limit {
        return Err(RepoError::TooLarge);
    }
    Ok(bytes)
}

// ============================================================
//  Repository configuration
// ============================================================

/// Reads the list of the declared repositories.
pub fn load() -> Vec<Repo> {
    let Ok(bytes) = crate::fs::read_file(CONFIG_PATH, Credentials::kernel()) else {
        return Vec::new();
    };
    let Ok(text) = core::str::from_utf8(&bytes) else {
        return Vec::new();
    };
    text.lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(Repo::from_line)
        .collect()
}

/// Rewrites the list of the repositories.
pub fn save(repos: &[Repo], now: u64) -> Result<(), crate::fs::FsError> {
    let who = Credentials::kernel();
    let mut content = String::from("# depots spm - spec 10.3, un depot par ligne\n");
    for repo in repos {
        content.push_str(&repo.to_line());
    }
    let _ = crate::fs::delete(CONFIG_PATH, who, now);
    crate::fs::write_file(CONFIG_PATH, content.as_bytes(), who, Mode::system_file(), now)
        .map(|_| ())
}

/// 8.2.4 -- `spm repo add <url>`.
///
/// Returns `false` if a repository of the same name already existed:
/// silently replacing it would change the provenance of the packages
/// already installed without anybody having asked.
pub fn add(
    name: &str,
    url: &str,
    anchor: Option<[u8; DIGEST_SIZE]>,
    now: u64,
) -> Result<bool, crate::fs::FsError> {
    let mut repos = load();
    if repos.iter().any(|repo| repo.name == name) {
        return Ok(false);
    }
    repos.push(Repo {
        name: String::from(name),
        url: String::from(url),
        anchor: anchor.unwrap_or([0u8; DIGEST_SIZE]),
    });
    save(&repos, now)?;
    Ok(true)
}

/// Removes a declared repository.
pub fn remove(name: &str, now: u64) -> Result<bool, crate::fs::FsError> {
    let mut repos = load();
    let before = repos.len();
    repos.retain(|repo| repo.name != name);
    if repos.len() == before {
        return Ok(false);
    }
    save(&repos, now)?;
    Ok(true)
}

// ============================================================
//  Operations on a repository
// ============================================================

/// Fetches and checks the index of a repository.
pub fn fetch_index(repo: &Repo) -> Result<Index, RepoError> {
    let url = resource_url(&repo.url, "index.spmi");
    let bytes = fetch(&url, MAX_INDEX)?;
    let text = core::str::from_utf8(&bytes).map_err(|_| RepoError::MalformedIndex)?;
    let index = parse_index(text)?;
    check_seal(repo, text, &index)?;
    Ok(index)
}

/// 8.3.2 -- Fetches a package and checks it is the one the index
/// announces.
///
/// The **signature** check does not happen here: it belongs to the
/// installation, which alone knows the scope and the account concerned,
/// and which must be able to refuse a perfectly transmitted package.
pub fn fetch_package(repo: &Repo, entry: &PackageEntry) -> Result<Vec<u8>, RepoError> {
    let url = resource_url(&repo.url, &entry.path);
    let bytes = fetch(&url, MAX_PACKAGE)?;

    if entry.size != 0 && bytes.len() != entry.size as usize {
        return Err(RepoError::DigestMismatch);
    }
    let digest = sha256::digest(&bytes);
    if !sha256::equal(&digest, &entry.digest) {
        return Err(RepoError::DigestMismatch);
    }
    Ok(bytes)
}

/// Fetches an OS update, same check.
pub fn fetch_os_update(repo: &Repo, entry: &OsEntry) -> Result<Vec<u8>, RepoError> {
    let url = resource_url(&repo.url, &entry.path);
    let bytes = fetch(&url, MAX_PACKAGE)?;

    if entry.size != 0 && bytes.len() != entry.size as usize {
        return Err(RepoError::DigestMismatch);
    }
    let digest = sha256::digest(&bytes);
    if !sha256::equal(&digest, &entry.digest) {
        return Err(RepoError::DigestMismatch);
    }
    Ok(bytes)
}

/// Looks for a package in every declared repository.
///
/// Returns the best version offered, all repositories together. An
/// unreachable repository is ignored and reported: `spm install` must not
/// fail because a third-party repository is offline.
pub fn resolve(app_id: &str) -> Option<(Repo, PackageEntry)> {
    let mut best: Option<(Repo, PackageEntry)> = None;

    for repo in load() {
        let index = match fetch_index(&repo) {
            Ok(index) => index,
            Err(error) => {
                crate::serial_println!(
                    "[spm] depot \"{}\" ignore - {}",
                    repo.name,
                    error.message()
                );
                continue;
            }
        };
        for entry in index.packages {
            if entry.app_id != app_id {
                continue;
            }
            let better = match &best {
                Some((_, current)) => entry.version > current.version,
                None => true,
            };
            if better {
                best = Some((repo.clone(), entry));
            }
        }
    }

    best
}

/// 8.2.3 -- `spm search`: everything containing the pattern.
pub fn search(pattern: &str) -> Vec<(String, PackageEntry)> {
    let mut found = Vec::new();
    for repo in load() {
        let Ok(index) = fetch_index(&repo) else {
            continue;
        };
        for entry in index.packages {
            if entry.app_id.contains(pattern) {
                found.push((repo.name.clone(), entry));
            }
        }
    }
    found
}

/// Files a copy of a fetched file in the local cache.
pub fn cache_write(name: &str, bytes: &[u8], now: u64) -> Result<String, RepoError> {
    let who = Credentials::kernel();
    if crate::fs::stat(CACHE_DIR, who).is_err()
        && crate::fs::create_directory(CACHE_DIR, who, Mode::system_dir(), now).is_err()
    {
        return Err(RepoError::WriteFailed);
    }
    let path = alloc::format!("{}/{}", CACHE_DIR, name);
    let _ = crate::fs::delete(&path, who, now);
    crate::fs::write_file(&path, bytes, who, Mode::system_file(), now)
        .map_err(|_| RepoError::WriteFailed)?;
    Ok(path)
}

pub fn dump_state() {
    let repos = load();
    if repos.is_empty() {
        crate::serial_println!("[spm] aucun depot declare");
        return;
    }
    for repo in &repos {
        crate::serial_println!(
            "[spm] depot \"{}\" -> {} ({})",
            repo.name,
            repo.url,
            if repo.has_anchor() {
                "ancre"
            } else {
                "sans ancre"
            }
        );
    }
}

//! 15.2 -- The registry of manifests (proposal of 24 September 2026).
//!
//! ## What a registry is
//!
//! A static web location -- a GitHub repository read through
//! `raw.githubusercontent.com`, GitHub Pages, any server, or a directory
//! (`file:`) -- holding one JSON manifest per application:
//!
//! ```text
//!   {base}/{app_id}.json
//!   {
//!     "app_name": "Demonstration",
//!     "app_id": "os.nova.demo",
//!     "current_stable_version": "1.2.0",
//!     "channels": {
//!       "release": { "version": "1.2.0", "code": 0, "size": 12345,
//!                    "checksum": "sha256-<64 hex>",
//!                    "download_url": "demo-1.2.0.spm" },
//!       "beta":    { ... },
//!       "test":    { ... }
//!     },
//!     "signature": { "cle": "<64 hex>", "ed25519": "<128 hex>" }
//!   }
//! ```
//!
//! `download_url` is absolute (`https://...`) or relative to the base;
//! `code` is the update code of spec 10.2 (the proposal had none).
//!
//! ## Three checks, three questions (15.1, point 1)
//!
//! 1. **Is the manifest the registry's?** Its Ed25519 signature, over the
//!    canonical JSON without the `signature` member, must be by the key
//!    recorded when the registry was added -- not by the key the manifest
//!    names, which anyone could choose. And it must describe the app_id
//!    asked for: a valid manifest of another application is refused.
//! 2. **Is the file the one the manifest announces?** Its SHA-256,
//!    computed over the file on the disk, must equal `checksum`, and its
//!    size `size`. A mismatch deletes the file.
//! 3. **Who produced the package?** Its own signature (spec 3.2),
//!    checked at installation like any package.

use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::signature;
use crate::fs::perm::{Credentials, Mode};
use crate::json::{self, Value};

use super::package::UpdateCode;

/// The declared registries: `name|url|key-hex` per line.
pub const CONFIG_PATH: &str = "/sys/tools/spm/registres.cfg";

/// The channels and the default choice (15.1, point 7):
/// `defaut=release`, then `{app_id}=beta` per application.
pub const CHANNELS_PATH: &str = "/sys/config/spm-canaux.cfg";

pub const CHANNELS: [&str; 3] = ["release", "beta", "test"];

/// Largest manifest read.
pub const MAX_MANIFEST: usize = 256 * 1024;

/// Largest package downloaded through a registry.
pub const MAX_DOWNLOAD: u64 = 2 << 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registry {
    pub name: String,
    pub url: String,
    /// The Ed25519 key that signs its manifests.
    pub key: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    pub name: String,
    pub version: String,
    pub numbers: [u16; 4],
    pub code: UpdateCode,
    pub size: u64,
    pub checksum: [u8; 32],
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub app_id: String,
    pub app_name: String,
    pub stable: String,
    pub channels: Vec<Channel>,
}

impl Manifest {
    pub fn channel(&self, name: &str) -> Option<&Channel> {
        self.channels.iter().find(|channel| channel.name == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    Transfer(String),
    Json(String),
    /// A member missing or of the wrong type.
    Malformed(&'static str),
    Unsigned,
    /// Signed by another key than the registry's.
    WrongKey,
    BadSignature,
    /// A manifest describing another application.
    OtherApplication,
    NoChannel(String),
    ChecksumMismatch,
    SizeMismatch,
    NotFound,
}

impl RegistryError {
    pub fn message(&self) -> String {
        match self {
            RegistryError::Transfer(text) => alloc::format!("transfert : {}", text),
            RegistryError::Json(text) => text.clone(),
            RegistryError::Malformed(what) => alloc::format!("manifeste mal forme : {}", what),
            RegistryError::Unsigned => String::from("manifeste non signe"),
            RegistryError::WrongKey => String::from("manifeste signe par une autre cle que celle du registre"),
            RegistryError::BadSignature => String::from("signature du manifeste invalide"),
            RegistryError::OtherApplication => String::from("le manifeste decrit une autre application"),
            RegistryError::NoChannel(name) => alloc::format!("canal {} absent du manifeste", name),
            RegistryError::ChecksumMismatch => String::from("empreinte du fichier differente du manifeste"),
            RegistryError::SizeMismatch => String::from("taille du fichier differente du manifeste"),
            RegistryError::NotFound => String::from("application absente des registres"),
        }
    }
}

// ============================================================
//  Declaring a registry
// ============================================================

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| alloc::format!("{:02x}", b)).collect()
}

fn unhex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 || !text.is_ascii() {
        return None;
    }
    let mut out = [0u8; N];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(out)
}

impl Registry {
    fn from_line(line: &str) -> Option<Registry> {
        let mut parts = line.split('|');
        let name = parts.next()?.trim();
        let url = parts.next()?.trim();
        let key = unhex::<32>(parts.next()?.trim())?;
        (!name.is_empty() && !url.is_empty()).then(|| Registry { name: String::from(name), url: String::from(url), key })
    }

    fn to_line(&self) -> String {
        alloc::format!("{}|{}|{}\n", self.name, self.url, hex(&self.key))
    }

    /// The address of a resource: absolute as given, else under the base.
    pub fn resolve(&self, path: &str) -> String {
        if path.starts_with("https://") || path.starts_with("http://") || path.starts_with("file:") {
            return String::from(path);
        }
        alloc::format!("{}/{}", self.url.trim_end_matches('/'), path.trim_start_matches('/'))
    }
}

pub fn load() -> Vec<Registry> {
    let Ok(bytes) = crate::fs::read_file(CONFIG_PATH, Credentials::kernel()) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&bytes)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(Registry::from_line)
        .collect()
}

fn save(registries: &[Registry], now: u64) -> bool {
    let mut text = String::from("# registres de manifestes - spec 10.5 (nom|url|cle ed25519)\n");
    for registry in registries {
        text.push_str(&registry.to_line());
    }
    let _ = crate::fs::delete(CONFIG_PATH, Credentials::kernel(), now);
    crate::fs::write_file(CONFIG_PATH, text.as_bytes(), Credentials::kernel(), Mode::system_file(), now).is_ok()
}

/// `spm registry add <url> <key>`. Returns false when the name exists.
pub fn add(name: &str, url: &str, key: [u8; 32], now: u64) -> Result<bool, ()> {
    let mut registries = load();
    if registries.iter().any(|registry| registry.name == name) {
        return Ok(false);
    }
    registries.push(Registry { name: String::from(name), url: String::from(url), key });
    if save(&registries, now) { Ok(true) } else { Err(()) }
}

pub fn remove(name: &str, now: u64) -> bool {
    let mut registries = load();
    let before = registries.len();
    registries.retain(|registry| registry.name != name);
    registries.len() != before && save(&registries, now)
}

// ============================================================
//  Channels (15.1, point 7)
// ============================================================

fn channel_settings() -> Vec<(String, String)> {
    let Ok(bytes) = crate::fs::read_file(CHANNELS_PATH, Credentials::kernel()) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|line| {
            let (key, value) = line.trim().split_once('=')?;
            Some((String::from(key.trim()), String::from(value.trim())))
        })
        .collect()
}

/// The channel followed by an application: its own, else the default,
/// else `release`.
pub fn channel_of(app_id: &str) -> String {
    let settings = channel_settings();
    let pick = |key: &str| settings.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    pick(app_id)
        .or_else(|| pick("defaut"))
        .filter(|channel| CHANNELS.contains(&channel.as_str()))
        .unwrap_or_else(|| String::from("release"))
}

/// Sets the channel of an application (`defaut` for all).
pub fn set_channel(app_id: &str, channel: &str, now: u64) -> bool {
    if !CHANNELS.contains(&channel) {
        return false;
    }
    let mut settings = channel_settings();
    settings.retain(|(key, _)| key != app_id);
    settings.push((String::from(app_id), String::from(channel)));
    let mut text = String::from("# canal suivi par application - spec 10.5\n");
    for (key, value) in settings {
        text.push_str(&alloc::format!("{}={}\n", key, value));
    }
    let _ = crate::fs::delete(CHANNELS_PATH, Credentials::kernel(), now);
    crate::fs::write_file(CHANNELS_PATH, text.as_bytes(), Credentials::kernel(), Mode::system_file(), now).is_ok()
}

// ============================================================
//  Reading a manifest
// ============================================================

/// The message a registry signs: domain tag, canonical JSON of the
/// manifest without its `signature` member (tools/novasign.py).
pub fn signed_message(document: &Value) -> Vec<u8> {
    let mut unsigned = json::Object::new();
    if let Some(object) = document.as_object() {
        for (key, value) in object.iter() {
            if key != "signature" {
                unsigned.insert(key, value.clone());
            }
        }
    }
    signature::enterprise_message(signature::domain::REGISTRY, &[json::canonical(&Value::Object(unsigned)).as_bytes()])
}

/// Checks the signature and decodes a manifest of `registry` for `app_id`.
pub fn decode(registry: &Registry, app_id: &str, text: &[u8]) -> Result<Manifest, RegistryError> {
    let document = json::parse(text).map_err(|error| RegistryError::Json(error.message()))?;

    // --- 1: the signature, before anything of the content is believed ---
    let sig = document.get("signature").ok_or(RegistryError::Unsigned)?;
    let key = sig.str_of("cle").and_then(unhex::<32>).ok_or(RegistryError::Malformed("signature.cle"))?;
    let bytes = sig.str_of("ed25519").and_then(unhex::<64>).ok_or(RegistryError::Malformed("signature.ed25519"))?;
    if key != registry.key {
        return Err(RegistryError::WrongKey);
    }
    if !crate::crypto::ed25519::verify(&key, &signed_message(&document), &bytes) {
        return Err(RegistryError::BadSignature);
    }

    // --- the content ---
    let id = document.str_of("app_id").ok_or(RegistryError::Malformed("app_id"))?;
    if id != app_id {
        return Err(RegistryError::OtherApplication);
    }
    let channels_value = document.get("channels").and_then(Value::as_object).ok_or(RegistryError::Malformed("channels"))?;
    let mut channels = Vec::new();
    for (name, value) in channels_value.iter() {
        if !CHANNELS.contains(&name) {
            continue;
        }
        let version = value.str_of("version").ok_or(RegistryError::Malformed("version"))?;
        let numbers = super::db::parse_version(version).ok_or(RegistryError::Malformed("version"))?;
        let checksum = value
            .str_of("checksum")
            .and_then(|text| text.strip_prefix("sha256-"))
            .and_then(unhex::<32>)
            .ok_or(RegistryError::Malformed("checksum"))?;
        let url = value.str_of("download_url").ok_or(RegistryError::Malformed("download_url"))?;
        let code = value
            .get("code")
            .and_then(Value::as_i64)
            .and_then(|code| UpdateCode::from_code(code as i8))
            .unwrap_or(UpdateCode::Standard);
        let size = value.get("size").and_then(Value::as_u64).unwrap_or(0);
        channels.push(Channel {
            name: String::from(name),
            version: String::from(version),
            numbers,
            code,
            size,
            checksum,
            url: String::from(url),
        });
    }
    Ok(Manifest {
        app_id: String::from(id),
        app_name: String::from(document.str_of("app_name").unwrap_or(id)),
        stable: String::from(document.str_of("current_stable_version").unwrap_or("")),
        channels,
    })
}

/// Fetches and checks the manifest of `app_id` in `registry`.
pub fn fetch_manifest(registry: &Registry, app_id: &str) -> Result<Manifest, RegistryError> {
    let url = registry.resolve(&alloc::format!("{}.json", app_id));
    let text = super::fetch::to_memory(&url, MAX_MANIFEST).map_err(RegistryError::Transfer)?;
    decode(registry, app_id, &text)
}

/// The first registry that describes `app_id`, its manifest, and the
/// channel the application follows.
///
/// A registry that cannot be read, or has no manifest for the name, does
/// not know the application: the next one is asked, and the caller may
/// fall back on the index repositories of phase 8. A manifest that **is**
/// there but fails its checks is a refusal, reported as such -- never a
/// reason to look elsewhere quietly.
pub fn resolve(app_id: &str) -> Result<(Registry, Manifest, Channel), RegistryError> {
    let wanted = channel_of(app_id);
    let mut last = RegistryError::NotFound;
    for registry in load() {
        match fetch_manifest(&registry, app_id) {
            Ok(manifest) => {
                let Some(channel) = manifest.channel(&wanted).cloned() else {
                    if last == RegistryError::NotFound {
                        last = RegistryError::NoChannel(wanted.clone());
                    }
                    continue;
                };
                return Ok((registry, manifest, channel));
            }
            Err(RegistryError::Transfer(_)) => {}
            Err(error) => {
                crate::serial_println!("[spm] registre \"{}\" : {} - {}", registry.name, app_id, error.message());
                last = error;
            }
        }
    }
    Err(last)
}

// ============================================================
//  Downloading what a manifest announces
// ============================================================

/// SHA-256 of a file of the volume, read by pieces.
pub fn digest_file(path: &str) -> Option<([u8; 32], u64)> {
    let mut hasher = crate::binfmt::sha256::Sha256::new();
    let mut buffer = alloc::vec![0u8; 64 * 1024];
    let mut offset = 0u64;
    loop {
        let read = crate::fs::read_range(path, offset, &mut buffer, Credentials::kernel()).ok()?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        offset += read as u64;
    }
    Some((hasher.finish(), offset))
}

/// Downloads the package of `channel` into `path` (resumed if a part is
/// there), then checks its size and checksum. A mismatch deletes it:
/// nothing that failed a check stays on the disk.
pub fn download(registry: &Registry, channel: &Channel, path: &str, now: u64) -> Result<u64, RegistryError> {
    let url = registry.resolve(&channel.url);
    let limit = if channel.size > 0 { channel.size } else { MAX_DOWNLOAD };
    let size = super::fetch::to_file(&url, path, limit, Credentials::kernel(), now).map_err(|error| {
        RegistryError::Transfer(error)
    })?;
    let fail = |error: RegistryError| {
        let _ = crate::fs::delete(path, Credentials::kernel(), now);
        Err(error)
    };
    if channel.size > 0 && size != channel.size {
        return fail(RegistryError::SizeMismatch);
    }
    let Some((digest, _)) = digest_file(path) else {
        return fail(RegistryError::Transfer(String::from("relecture impossible")));
    };
    if !crate::binfmt::sha256::equal(&digest, &channel.checksum) {
        return fail(RegistryError::ChecksumMismatch);
    }
    Ok(size)
}

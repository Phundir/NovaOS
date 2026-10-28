//! 9.2.1 -- The `dev.toml` manifest (spec 15.3).
//!
//! ## What the manifest decides
//!
//! 15.3 gives the list: `app_id`, `version`, `author`, `binary_type`,
//! `lang`, `entry`, `signature`, `permissions` as an allow-list, internal
//! ports, dependencies. This is not description: every one of those
//! fields turns up, byte for byte, in the header of the binary produced.
//! The manifest **is** the header, written in a form a human can read
//! back.
//!
//! ## Why a restricted TOML parser rather than a home-grown format
//!
//! 15.3 names the file `dev.toml`, which fixes the syntax. The subset
//! recognised here covers what a manifest really uses: `[section]`
//! tables, `key = value` assignments, quoted strings, integers, booleans,
//! and single-line arrays. What is missing -- inline tables, arrays of
//! tables, multi-line strings -- is **explicitly refused** rather than
//! ignored: a silently lost field produces a binary that does not declare
//! what its author believed they had declared, and that is the kind of
//! defect only production finds.
//!
//! ## The cross-validation is the useful part
//!
//! Parsing the file proves almost nothing. What counts is what `validate`
//! checks afterwards, and it comes from three different sections of the
//! specification:
//!
//! * 3.4 -- the format/signature combination. A self-signed `.dsa` is
//!   refused here, before it is even assembled;
//! * 12.4 -- the per-format permission ceiling. A `.cus` demanding
//!   `KERNEL_SPACE` is refused at the manifest, not at load time;
//! * 13.2 -- the "target formats" column. A Java project aiming at a
//!   `.dsa` is off the table, and saying so early avoids discovering at
//!   link time that the toolchain does not exist.
//!
//! Refusing early is the only moment when the message can still name the
//! offending line.

use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::header::{BinaryFormat, SignatureType, flags};
use crate::binfmt::identity::{Permissions, Version};

use super::runtime::{self, Language};

/// Name of the manifest, imposed by 15.3.
pub const FILE_NAME: &str = "dev.toml";

// ============================================================
//  Errors
// ============================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// A line the grammar does not recognise.
    Syntax(usize),
    /// Malformed table header.
    BadTable(usize),
    /// Value whose type does not match the field.
    BadValue(usize, String),
    /// Unknown key. Refused rather than ignored: see the module header.
    UnknownKey(usize, String),
    /// Mandatory field missing.
    Missing(&'static str),
    /// Format/signature combination forbidden by 3.4.
    ForbiddenCombination,
    /// Permission beyond the format ceiling (12.4).
    PermissionAboveCeiling(String),
    /// Permission whose name does not exist.
    UnknownPermission(String),
    /// The language does not target this format (13.2).
    FormatNotTargeted,
    /// 13.2 demands a SYSTEM signature for this pair.
    SystemSignatureRequired,
    /// `app_id` that is not in reverse notation.
    BadAppId,
    /// Port number out of range, or duplicated.
    BadPort(u32),
    /// Flag reserved for another format (3.4).
    FlagNotAllowed(String),
}

impl ManifestError {
    pub fn message(&self) -> String {
        match self {
            ManifestError::Syntax(line) => alloc::format!("ligne {} : syntaxe invalide", line),
            ManifestError::BadTable(line) => alloc::format!("ligne {} : table mal formee", line),
            ManifestError::BadValue(line, key) => {
                alloc::format!("ligne {} : valeur invalide pour \"{}\"", line, key)
            }
            ManifestError::UnknownKey(line, key) => {
                alloc::format!("ligne {} : cle inconnue \"{}\"", line, key)
            }
            ManifestError::Missing(key) => alloc::format!("champ obligatoire absent : {}", key),
            ManifestError::ForbiddenCombination => {
                String::from("combinaison format/signature interdite par la spec 3.4")
            }
            ManifestError::PermissionAboveCeiling(name) => {
                alloc::format!("permission {} au-dela du plafond du format (spec 12.4)", name)
            }
            ManifestError::UnknownPermission(name) => {
                alloc::format!("permission inconnue : {}", name)
            }
            ManifestError::FormatNotTargeted => {
                String::from("ce langage ne cible pas ce format (spec 13.2)")
            }
            ManifestError::SystemSignatureRequired => {
                String::from("signature SYSTEM exigee pour ce couple langage/format (spec 13.2)")
            }
            ManifestError::BadAppId => {
                String::from("app_id attendu en notation inversee : editeur.produit")
            }
            ManifestError::BadPort(port) => alloc::format!("port interne invalide : {}", port),
            ManifestError::FlagNotAllowed(name) => {
                alloc::format!("drapeau {} interdit pour ce format (spec 3.4)", name)
            }
        }
    }
}

// ============================================================
//  TOML values
// ============================================================

/// The subset of types the grammar recognises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Text(String),
    Integer(i64),
    Boolean(bool),
    List(Vec<String>),
}

impl Value {
    fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(text) => Some(text),
            _ => None,
        }
    }

    fn as_integer(&self) -> Option<i64> {
        match self {
            Value::Integer(value) => Some(*value),
            _ => None,
        }
    }

    fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Boolean(value) => Some(*value),
            _ => None,
        }
    }

    fn as_list(&self) -> Option<&[String]> {
        match self {
            Value::List(items) => Some(items),
            _ => None,
        }
    }
}

/// A located key/value pair: table, key, value, line.
#[derive(Debug, Clone)]
pub struct Assignment {
    pub table: String,
    pub key: String,
    pub value: Value,
    pub line: usize,
}

/// Splits the document into located assignments.
///
/// Returns the raw list. Conversion to a `DevManifest` is separate: the
/// splitting knows only the syntax, the conversion knows the
/// specification. Mixing the two would give a parser that cannot be
/// exercised without building a complete project.
pub fn tokenize(source: &str) -> Result<Vec<Assignment>, ManifestError> {
    let mut assignments = Vec::new();
    let mut table = String::new();

    for (index, raw) in source.lines().enumerate() {
        let number = index + 1;
        let line = strip_comment(raw);
        if line.is_empty() {
            continue;
        }

        if let Some(rest) = line.strip_prefix('[') {
            let Some(name) = rest.strip_suffix(']') else {
                return Err(ManifestError::BadTable(number));
            };
            let name = name.trim();
            if name.is_empty() || name.starts_with('[') {
                return Err(ManifestError::BadTable(number));
            }
            table = String::from(name);
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            return Err(ManifestError::Syntax(number));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(ManifestError::Syntax(number));
        }
        let value = parse_value(value.trim(), number)?;
        assignments.push(Assignment {
            table: table.clone(),
            key: String::from(key),
            value,
            line: number,
        });
    }

    Ok(assignments)
}

/// Strips the comment, respecting quotes.
fn strip_comment(line: &str) -> &str {
    let mut in_string = false;
    for (index, byte) in line.as_bytes().iter().enumerate() {
        match byte {
            b'"' => in_string = !in_string,
            b'#' if !in_string => return line[..index].trim(),
            _ => {}
        }
    }
    line.trim()
}

fn parse_value(text: &str, line: usize) -> Result<Value, ManifestError> {
    if text.is_empty() {
        return Err(ManifestError::Syntax(line));
    }
    if let Some(rest) = text.strip_prefix('"') {
        let Some(inner) = rest.strip_suffix('"') else {
            return Err(ManifestError::Syntax(line));
        };
        // Everything a manifest carries ends up in a header or on a
        // console: the printable range is the only one allowed.
        if !inner.bytes().all(|byte| (0x20..=0x7E).contains(&byte)) {
            return Err(ManifestError::Syntax(line));
        }
        return Ok(Value::Text(String::from(inner)));
    }
    if let Some(rest) = text.strip_prefix('[') {
        let Some(inner) = rest.strip_suffix(']') else {
            return Err(ManifestError::Syntax(line));
        };
        let mut items = Vec::new();
        for piece in inner.split(',') {
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            let piece = piece.trim_matches('"').trim();
            if piece.is_empty() {
                return Err(ManifestError::Syntax(line));
            }
            items.push(String::from(piece));
        }
        return Ok(Value::List(items));
    }
    if text == "true" || text == "false" {
        return Ok(Value::Boolean(text == "true"));
    }
    if let Some(rest) = text.strip_prefix("0x") {
        return i64::from_str_radix(rest, 16)
            .map(Value::Integer)
            .map_err(|_| ManifestError::Syntax(line));
    }
    text.parse::<i64>()
        .map(Value::Integer)
        .map_err(|_| ManifestError::Syntax(line))
}

// ============================================================
//  Manifest
// ============================================================

/// A project manifest, after parsing and validation.
#[derive(Debug, Clone)]
pub struct DevManifest {
    pub app_id: String,
    pub display_name: String,
    pub author: String,
    pub group: String,
    pub version: Version,
    pub format: BinaryFormat,
    pub language: Language,
    pub entry: String,
    pub signature: SignatureType,
    pub permissions: Permissions,
    pub ports: Vec<u16>,
    pub depends: Vec<String>,
    pub flags: u32,
    pub stack_size: u64,
    pub description: String,
}

impl Default for DevManifest {
    fn default() -> Self {
        DevManifest {
            app_id: String::new(),
            display_name: String::new(),
            author: String::new(),
            group: String::new(),
            version: Version::new(0, 1, 0, 1),
            format: BinaryFormat::Cus,
            language: Language::Asm,
            entry: String::new(),
            signature: SignatureType::SelfSigned,
            permissions: Permissions::NONE,
            ports: Vec::new(),
            depends: Vec::new(),
            flags: 0,
            stack_size: 64 * 1024,
            description: String::new(),
        }
    }
}

/// Table of permission names, as 12.3 names them.
const PERMISSION_NAMES: &[(&str, u64)] = &[
    ("NETWORK_INTERNET", Permissions::NETWORK_INTERNET),
    ("NETWORK_LOCAL", Permissions::NETWORK_LOCAL),
    ("MICROPHONE", Permissions::MICROPHONE),
    ("WEBCAM", Permissions::WEBCAM),
    ("AUDIO_OUTPUT", Permissions::AUDIO_OUTPUT),
    ("SYS_DIRECTORY", Permissions::SYS_DIRECTORY),
    ("HARDWARE", Permissions::HARDWARE),
    ("USER_MANAGEMENT", Permissions::USER_MANAGEMENT),
    ("KERNEL_SPACE", Permissions::KERNEL_SPACE),
    ("FS_USER_HOME", Permissions::FS_USER_HOME),
    ("FS_FULL", Permissions::FS_FULL),
    ("IPC_PUBLIC", Permissions::IPC_PUBLIC),
    ("IPC_INTERNAL_PORTS", Permissions::IPC_INTERNAL_PORTS),
    ("NOTIFICATIONS", Permissions::NOTIFICATIONS),
    ("FULLSCREEN", Permissions::FULLSCREEN),
    ("CLIPBOARD", Permissions::CLIPBOARD),
];

/// Header flags that can be named from the manifest.
const FLAG_NAMES: &[(&str, u32)] = &[
    ("GUI", flags::GUI),
    ("BACKGROUND", flags::BACKGROUND),
    ("AUTO_PAUSE", flags::AUTO_PAUSE),
    ("EXPOSES_API", flags::EXPOSES_API),
    ("REPLACES_COMPONENT", flags::REPLACES_COMPONENT),
    ("DEFAULT_CAPABLE", flags::DEFAULT_CAPABLE),
    ("REQUIRES_ADMIN", flags::REQUIRES_ADMIN),
    ("SERVICE", flags::SERVICE),
    ("NETWORK", flags::NETWORK),
];

pub fn permission_bit(name: &str) -> Option<u64> {
    PERMISSION_NAMES
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, bit)| *bit)
}

pub fn flag_bit(name: &str) -> Option<u32> {
    FLAG_NAMES
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, bit)| *bit)
}

/// Decodes `1.2.3+4`, or `1.2.3`.
fn parse_version(text: &str) -> Option<Version> {
    let (numbers, build) = match text.split_once('+') {
        Some((numbers, build)) => (numbers, build.parse::<u16>().ok()?),
        None => (text, 1),
    };
    let mut parts = numbers.split('.');
    let major = parts.next()?.parse::<u16>().ok()?;
    let minor = parts.next()?.parse::<u16>().ok()?;
    let patch = parts.next().unwrap_or("0").parse::<u16>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(Version::new(major, minor, patch, build))
}

/// Parses a complete `dev.toml`.
pub fn parse(source: &str) -> Result<DevManifest, ManifestError> {
    let assignments = tokenize(source)?;
    let mut manifest = DevManifest::default();
    let mut seen_app_id = false;
    let mut seen_type = false;
    let mut seen_lang = false;
    let mut seen_entry = false;

    for item in assignments.iter() {
        let line = item.line;
        let key = item.key.as_str();
        let scope = item.table.as_str();

        match (scope, key) {
            // --- [app]: identity (15.3) ---
            ("app", "app_id") => {
                manifest.app_id = String::from(text(item)?);
                seen_app_id = true;
            }
            ("app", "nom") => manifest.display_name = String::from(text(item)?),
            ("app", "auteur") => manifest.author = String::from(text(item)?),
            ("app", "groupe") => manifest.group = String::from(text(item)?),
            ("app", "description") => manifest.description = String::from(text(item)?),
            ("app", "version") => {
                let raw = text(item)?;
                manifest.version = parse_version(raw)
                    .ok_or_else(|| ManifestError::BadValue(line, String::from(key)))?;
            }

            // --- [build]: how the binary is produced ---
            ("build", "type") => {
                manifest.format = format_from_key(text(item)?)
                    .ok_or_else(|| ManifestError::BadValue(line, String::from(key)))?;
                seen_type = true;
            }
            ("build", "lang") => {
                manifest.language = Language::from_key(text(item)?)
                    .ok_or_else(|| ManifestError::BadValue(line, String::from(key)))?;
                seen_lang = true;
            }
            ("build", "entree") => {
                manifest.entry = String::from(text(item)?);
                seen_entry = true;
            }
            ("build", "signature") => {
                manifest.signature = signature_from_key(text(item)?)
                    .ok_or_else(|| ManifestError::BadValue(line, String::from(key)))?;
            }
            ("build", "pile") => {
                let value = item
                    .value
                    .as_integer()
                    .ok_or_else(|| ManifestError::BadValue(line, String::from(key)))?;
                if value <= 0 || value > 16 * 1024 * 1024 {
                    return Err(ManifestError::BadValue(line, String::from(key)));
                }
                manifest.stack_size = value as u64;
            }
            ("build", "drapeaux") => {
                let items = item
                    .value
                    .as_list()
                    .ok_or_else(|| ManifestError::BadValue(line, String::from(key)))?;
                for name in items {
                    let bit = flag_bit(name)
                        .ok_or_else(|| ManifestError::BadValue(line, name.clone()))?;
                    manifest.flags |= bit;
                }
            }

            // --- [permissions]: allow-list (12.3) ---
            ("permissions", "liste") => {
                let items = item
                    .value
                    .as_list()
                    .ok_or_else(|| ManifestError::BadValue(line, String::from(key)))?;
                let mut mask = 0u64;
                for name in items {
                    let bit = permission_bit(name)
                        .ok_or_else(|| ManifestError::UnknownPermission(name.clone()))?;
                    mask |= bit;
                }
                manifest.permissions = Permissions(mask);
            }

            // --- [ipc]: internal ports (12.2) ---
            ("ipc", "ports") => {
                let items = item
                    .value
                    .as_list()
                    .ok_or_else(|| ManifestError::BadValue(line, String::from(key)))?;
                for raw in items {
                    let port = raw
                        .parse::<u32>()
                        .map_err(|_| ManifestError::BadValue(line, raw.clone()))?;
                    if port == 0 || port > u16::MAX as u32 {
                        return Err(ManifestError::BadPort(port));
                    }
                    if manifest.ports.contains(&(port as u16)) {
                        return Err(ManifestError::BadPort(port));
                    }
                    manifest.ports.push(port as u16);
                }
            }

            // --- [dependances] ---
            ("dependances", "requiert") => {
                let items = item
                    .value
                    .as_list()
                    .ok_or_else(|| ManifestError::BadValue(line, String::from(key)))?;
                manifest.depends = items.to_vec();
            }

            // A lone boolean at the top of the file has no table: the
            // grammar accepts it, the specification gives it no meaning.
            // Refusing it avoids a field that acts on nothing.
            _ => {
                let _ = item.value.as_bool();
                return Err(ManifestError::UnknownKey(line, alloc::format!("{}.{}", scope, key)));
            }
        }
    }

    if !seen_app_id {
        return Err(ManifestError::Missing("app.app_id"));
    }
    if !seen_type {
        return Err(ManifestError::Missing("build.type"));
    }
    if !seen_lang {
        return Err(ManifestError::Missing("build.lang"));
    }
    if !seen_entry {
        return Err(ManifestError::Missing("build.entree"));
    }
    if manifest.display_name.is_empty() {
        manifest.display_name = manifest.app_id.clone();
    }
    // The default group is the app_id minus its last segment:
    // `com.exemple.editeur` belongs to `com.exemple`. 12.1 makes direct
    // communication depend on the group, and leaving it empty would send
    // every pair of applications from one publisher back through an
    // internal port.
    if manifest.group.is_empty()
        && let Some((prefix, _)) = manifest.app_id.rsplit_once('.')
    {
        manifest.group = String::from(prefix);
    }

    validate(&manifest)?;
    Ok(manifest)
}

fn text<'a>(item: &'a Assignment) -> Result<&'a str, ManifestError> {
    item.value
        .as_text()
        .ok_or_else(|| ManifestError::BadValue(item.line, item.key.clone()))
}

fn format_from_key(key: &str) -> Option<BinaryFormat> {
    Some(match key {
        "dsa" => BinaryFormat::Dsa,
        "ss" => BinaryFormat::Ss,
        "gs" => BinaryFormat::Gs,
        "cus" => BinaryFormat::Cus,
        "ras" => BinaryFormat::Ras,
        _ => return None,
    })
}

/// Format key, as `format_from_key` reads it back.
///
/// `BinaryFormat::extension` returns ".cus": that is the **file** suffix,
/// not the manifest key. `to_toml` used it to write `type`, so that no
/// `dev.toml` produced by the SDK survived its own parsing again -- the
/// 15.3 round trip failed, and with it everything that starts by reading
/// a generated manifest back: `dev build`, `dev run`, `dev package`.
fn format_key(format: BinaryFormat) -> &'static str {
    format.extension().trim_start_matches('.')
}

fn signature_from_key(key: &str) -> Option<SignatureType> {
    Some(match key {
        "system" => SignatureType::System,
        "enterprise" => SignatureType::Enterprise,
        "self" | "auto" => SignatureType::SelfSigned,
        "none" | "aucune" => SignatureType::None,
        _ => return None,
    })
}

/// Checks the manifest against 3.4, 12.4 and 13.2.
///
/// Three different sections, three reasons to refuse, and none redundant:
/// the first bears on the file produced, the second on what it will be
/// allowed to do, the third on the toolchain able to produce it.
pub fn validate(manifest: &DevManifest) -> Result<(), ManifestError> {
    // An app_id with no dot names no publisher: 12.1 relies on reverse
    // notation to derive the group.
    if !manifest.app_id.contains('.') || manifest.app_id.starts_with('.') {
        return Err(ManifestError::BadAppId);
    }
    if !crate::binfmt::signature::combination_allowed(manifest.format, manifest.signature) {
        return Err(ManifestError::ForbiddenCombination);
    }

    let ceiling = Permissions::ceiling(manifest.format);
    let excess = manifest.permissions.0 & !ceiling;
    if excess != 0 {
        let name = PERMISSION_NAMES
            .iter()
            .find(|(_, bit)| excess & *bit != 0)
            .map(|(name, _)| String::from(*name))
            .unwrap_or_else(|| String::from("inconnue"));
        return Err(ManifestError::PermissionAboveCeiling(name));
    }

    if !runtime::targets(manifest.language, manifest.format) {
        return Err(ManifestError::FormatNotTargeted);
    }
    if runtime::requires_system_signature(manifest.language, manifest.format)
        && manifest.signature != SignatureType::System
    {
        return Err(ManifestError::SystemSignatureRequired);
    }

    // 3.4: three flags are reserved to one precise format.
    if manifest.flags & flags::EXPOSES_API != 0 && manifest.format != BinaryFormat::Dsa {
        return Err(ManifestError::FlagNotAllowed(String::from("EXPOSES_API")));
    }
    if manifest.flags & flags::REPLACES_COMPONENT != 0 && manifest.format != BinaryFormat::Ss {
        return Err(ManifestError::FlagNotAllowed(String::from(
            "REPLACES_COMPONENT",
        )));
    }
    if manifest.flags & flags::AUTO_PAUSE != 0 && manifest.format != BinaryFormat::Gs {
        return Err(ManifestError::FlagNotAllowed(String::from("AUTO_PAUSE")));
    }

    Ok(())
}

/// Reproduces the manifest in its textual form.
///
/// Used by `dev new`, which writes the file, and by the self-test, which
/// checks that parsing then rewriting composes to the identity. A round
/// trip that does not close signals a lost field.
pub fn to_toml(manifest: &DevManifest) -> String {
    let mut text = String::from("# manifest de projet - spec 15.3\n\n[app]\n");
    text.push_str(&alloc::format!("app_id = \"{}\"\n", manifest.app_id));
    text.push_str(&alloc::format!("nom = \"{}\"\n", manifest.display_name));
    text.push_str(&alloc::format!("auteur = \"{}\"\n", manifest.author));
    text.push_str(&alloc::format!("groupe = \"{}\"\n", manifest.group));
    if !manifest.description.is_empty() {
        text.push_str(&alloc::format!(
            "description = \"{}\"\n",
            manifest.description
        ));
    }
    text.push_str(&alloc::format!(
        "version = \"{}.{}.{}+{}\"\n",
        manifest.version.major,
        manifest.version.minor,
        manifest.version.patch,
        manifest.version.build
    ));

    text.push_str("\n[build]\n");
    text.push_str(&alloc::format!("type = \"{}\"\n", format_key(manifest.format)));
    text.push_str(&alloc::format!(
        "lang = \"{}\"\n",
        manifest.language.key()
    ));
    text.push_str(&alloc::format!("entree = \"{}\"\n", manifest.entry));
    text.push_str(&alloc::format!(
        "signature = \"{}\"\n",
        signature_key(manifest.signature)
    ));
    text.push_str(&alloc::format!("pile = {}\n", manifest.stack_size));
    if manifest.flags != 0 {
        let names: Vec<&str> = FLAG_NAMES
            .iter()
            .filter(|(_, bit)| manifest.flags & *bit != 0)
            .map(|(name, _)| *name)
            .collect();
        text.push_str(&alloc::format!("drapeaux = [{}]\n", quoted(&names)));
    }

    text.push_str("\n[permissions]\n");
    let granted: Vec<&str> = PERMISSION_NAMES
        .iter()
        .filter(|(_, bit)| manifest.permissions.contains(*bit))
        .map(|(name, _)| *name)
        .collect();
    text.push_str(&alloc::format!("liste = [{}]\n", quoted(&granted)));

    if !manifest.ports.is_empty() {
        text.push_str("\n[ipc]\n");
        let ports: Vec<String> = manifest
            .ports
            .iter()
            .map(|port| alloc::format!("\"{}\"", port))
            .collect();
        text.push_str(&alloc::format!("ports = [{}]\n", ports.join(", ")));
    }

    if !manifest.depends.is_empty() {
        text.push_str("\n[dependances]\n");
        let names: Vec<&str> = manifest.depends.iter().map(|item| item.as_str()).collect();
        text.push_str(&alloc::format!("requiert = [{}]\n", quoted(&names)));
    }

    text
}

fn quoted(items: &[&str]) -> String {
    let pieces: Vec<String> = items
        .iter()
        .map(|item| alloc::format!("\"{}\"", item))
        .collect();
    pieces.join(", ")
}

fn signature_key(signature: SignatureType) -> &'static str {
    match signature {
        SignatureType::System => "system",
        SignatureType::Enterprise => "enterprise",
        SignatureType::SelfSigned => "self",
        SignatureType::None => "none",
    }
}

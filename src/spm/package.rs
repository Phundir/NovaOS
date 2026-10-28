//! 8.1 -- `.spm` package format (spec 10.1).
//!
//! ## Why yet another format
//!
//! 10.1 describes a "single binary file embedding: SPM header, manifest,
//! main binary, resources and pre/post installation scripts". None of the
//! five formats of 3.3 can play that role: a `.cus` is *one* executable
//! image, whereas a package carries one **plus** what it takes to lay it
//! down in the right place. Confusing the two would make the loader carry
//! fields it has no use for, and the installer fields it should ignore.
//!
//! `.spm` therefore has its own header, decoded field by field like
//! everything coming from a file -- nothing guarantees the alignment of a
//! buffer read from a disk or a socket.
//!
//! ```text
//!   0x00  4  magic "SPM\0"
//!   0x04  2  format version
//!   0x06  1  update code (spec 10.2, i8: -1, 0, 1, 2)
//!   0x07  1  signature type (spec 3.2)
//!   0x08  1  format of the main binary (spec 3.1)
//!   0x09  1  reserved
//!   0x0A  2  reserved
//!   0x0C  4  flags
//!   0x10  4  offset of the manifest
//!   0x14  4  length of the manifest
//!   0x18  4  offset of the main binary
//!   0x1C  4  length of the main binary
//!   0x20  4  offset of the resource block
//!   0x24  4  length of the resource block
//!   0x28  4  offset of the pre-installation script
//!   0x2C  4  length of the pre-installation script
//!   0x30  4  offset of the post-installation script
//!   0x34  4  length of the post-installation script
//!   0x38  8  package version (major, minor, patch, build)
//!   0x40 32  app_id
//!   0x60 32  display name
//!   0x80 32  publisher
//!   0xA0 32  SHA-256 digest of the payload
//!   0xC0 32  certificate binding (ENTERPRISE)
//!   0xE0 32  digest of the issuing authority key (ENTERPRISE)
//!   0x100    end of the header, start of the payload
//! ```
//!
//! ## Two digests, two questions
//!
//! Same split as in phase 3, for the same reason:
//!
//! * the **payload digest** covers the bytes following the header. It
//!   answers "did the file arrive whole".
//! * the **authenticity digest** covers the header -- digest field
//!   neutralised, failing which it would refer to itself -- followed by
//!   the payload. It answers "is this package exactly the one a publisher
//!   produced". It is the one a trust store recognises, and it changes as
//!   soon as the app_id, the update code or a single byte of content is
//!   touched.
//!
//! ## The certificate fits in the header
//!
//! A binary of 3 carries its certificate in a separate block, because it
//! also lodges readable legal information there. A package does not need
//! that: what it carries is **already** a binary, which carries its own.
//! Two fields are therefore enough here -- the authority key and the
//! binding -- and the same reservation as in phase 3 applies: with no
//! asymmetric primitive, this chain detects an alteration, it
//! authenticates nobody.

use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::header::{BinaryFormat, SignatureType};
use crate::binfmt::sha256::{self, DIGEST_SIZE};
use crate::binfmt::signature::{Reason, Verdict, combination_allowed, expected_binding};

/// Fixed size of the header. Any other value makes the package be rejected.
pub const HEADER_SIZE: usize = 256;

/// Magic of a package.
pub const MAGIC: [u8; 4] = [0x53, 0x50, 0x4D, 0x00]; // "SPM\0"

/// Format version produced and accepted by this kernel.
pub const FORMAT_VERSION: u16 = 1;

/// Length of the text fields of the header.
pub const NAME_LEN: usize = 32;

/// Extension of a package.
pub const EXTENSION: &str = "spm";

/// Flags of the header.
pub mod flags {
    /// The package delivers an OS update and not an application (spec
    /// 11). Its destination is not `/apps/` but `/sys/updates/`.
    pub const OS_UPDATE: u32 = 1 << 0;
    /// The installation demands a reboot to take effect.
    pub const NEEDS_REBOOT: u32 = 1 << 1;
    /// The package replaces an already installed version instead of
    /// adding to it -- the case of an ordinary update.
    pub const UPGRADE: u32 = 1 << 2;
}

// ============================================================
//  10.2 -- Update codes
// ============================================================

/// The four codes of the 10.2 table.
///
/// Not to be confused with the codes of 11.1, which reuse the same
/// numbers to name **types** of OS update. The two tables are distinct
/// name spaces: code 2 means "silent" here, and "MAJOR, reboot
/// mandatory" there. Mixing them would silently apply an update that
/// demands a reboot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i8)]
pub enum UpdateCode {
    /// -1 -- forced notification, button that cannot be dismissed.
    Mandatory = -1,
    /// 0 -- notification and confirmation by the user.
    Standard = 0,
    /// 1 -- discreet notification, dismissible.
    Optional = 1,
    /// 2 -- no notification, applied in the background.
    Silent = 2,
}

impl UpdateCode {
    pub fn from_code(code: i8) -> Option<Self> {
        match code {
            -1 => Some(UpdateCode::Mandatory),
            0 => Some(UpdateCode::Standard),
            1 => Some(UpdateCode::Optional),
            2 => Some(UpdateCode::Silent),
            _ => None,
        }
    }

    pub fn code(&self) -> i8 {
        *self as i8
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            UpdateCode::Mandatory => "obligatoire",
            UpdateCode::Standard => "standard",
            UpdateCode::Optional => "optionnelle",
            UpdateCode::Silent => "silencieuse",
        }
    }

    /// "Behaviour" column of 10.2.
    pub fn behaviour(&self) -> &'static str {
        match self {
            UpdateCode::Mandatory => "notification forcee - bouton non ignorable",
            UpdateCode::Standard => "notification + confirmation utilisateur",
            UpdateCode::Optional => "notification discrete, ignorable",
            UpdateCode::Silent => "aucune notification, appliquee en arriere-plan",
        }
    }

    /// The "Reboot" column reads "if necessary" only for the mandatory
    /// code; the other three are noted "No".
    pub fn may_require_reboot(&self) -> bool {
        *self == UpdateCode::Mandatory
    }

    /// May the user dismiss the notification without acting?
    pub fn dismissible(&self) -> bool {
        !matches!(self, UpdateCode::Mandatory)
    }

    /// Do we merely emit a notification?
    pub fn notifies(&self) -> bool {
        !matches!(self, UpdateCode::Silent)
    }

    /// Does the application demand explicit agreement before being laid down?
    pub fn needs_confirmation(&self) -> bool {
        matches!(self, UpdateCode::Standard)
    }
}

// ============================================================
//  Manifest (8.1.2)
// ============================================================

/// Metadata of the package, described as `key=value` text.
///
/// The choice of text rather than a binary structure is deliberately the
/// same as for `theme.cfg` and `user.cfg`: those files are read back by a
/// human when an installation goes wrong, and one more binary decoder
/// would be one more decoder to audit. The cost is nil -- the manifest is
/// a few hundred bytes -- and the header stays binary because it is
/// walked before any allocation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    pub app_id: String,
    pub name: String,
    pub publisher: String,
    pub description: String,
    /// Name of the file the main binary is written under.
    pub binary: String,
    /// Installation prefix imposed by the package. Empty: derived from
    /// the format, as `tree::create_sandbox` does.
    pub prefix: String,
    /// app_id of the required packages.
    pub depends: Vec<String>,
    /// Raw keys not recognised, kept as-is: a manifest produced by a
    /// newer SDK must not lose its fields on every read-back.
    /// chaque relecture.
    pub extra: Vec<(String, String)>,
}

impl Manifest {
    /// Decodes a manifest. A line with no `=` is ignored, not fatal: a
    /// manifest is a document, and a document tolerates an empty line or
    /// a comment.
    pub fn parse(text: &str) -> Manifest {
        let mut manifest = Manifest::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            match key {
                "app_id" => manifest.app_id = String::from(value),
                "nom" => manifest.name = String::from(value),
                "editeur" => manifest.publisher = String::from(value),
                "description" => manifest.description = String::from(value),
                "binaire" => manifest.binary = String::from(value),
                "prefixe" => manifest.prefix = String::from(value),
                "depend" => {
                    manifest.depends = value
                        .split(',')
                        .map(|item| item.trim())
                        .filter(|item| !item.is_empty())
                        .map(String::from)
                        .collect();
                }
                _ => manifest
                    .extra
                    .push((String::from(key), String::from(value))),
            }
        }
        manifest
    }

    /// Reproduces the text of the manifest. Used by the package builder
    /// and by the self-test, which checks that decoding then re-encoding
    /// composes to the identity.
    pub fn to_text(&self) -> String {
        let mut text = String::from("# manifest .spm - spec 10.1\n");
        text.push_str(&alloc::format!("app_id={}\n", self.app_id));
        text.push_str(&alloc::format!("nom={}\n", self.name));
        text.push_str(&alloc::format!("editeur={}\n", self.publisher));
        if !self.description.is_empty() {
            text.push_str(&alloc::format!("description={}\n", self.description));
        }
        if !self.binary.is_empty() {
            text.push_str(&alloc::format!("binaire={}\n", self.binary));
        }
        if !self.prefix.is_empty() {
            text.push_str(&alloc::format!("prefixe={}\n", self.prefix));
        }
        if !self.depends.is_empty() {
            text.push_str(&alloc::format!("depend={}\n", self.depends.join(",")));
        }
        for (key, value) in &self.extra {
            text.push_str(&alloc::format!("{}={}\n", key, value));
        }
        text
    }
}

// ============================================================
//  Resources
// ============================================================

/// Size of an entry of the resource table.
pub const RESOURCE_ENTRY: usize = 40;

/// A file accompanying the main binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    pub name: String,
    /// Offset relative to the start of the resource block.
    pub offset: u32,
    pub length: u32,
}

// ============================================================
//  Header
// ============================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageHeader {
    pub version: u16,
    pub update_code: UpdateCode,
    pub signature: SignatureType,
    pub format: BinaryFormat,
    pub flags: u32,
    pub manifest_offset: u32,
    pub manifest_len: u32,
    pub binary_offset: u32,
    pub binary_len: u32,
    pub resources_offset: u32,
    pub resources_len: u32,
    pub pre_offset: u32,
    pub pre_len: u32,
    pub post_offset: u32,
    pub post_len: u32,
    pub package_version: [u16; 4],
    pub app_id: String,
    pub display_name: String,
    pub publisher: String,
    pub payload_hash: [u8; DIGEST_SIZE],
    pub binding: [u8; DIGEST_SIZE],
    pub authority_key: [u8; DIGEST_SIZE],
}

impl PackageHeader {
    pub fn has_flag(&self, flag: u32) -> bool {
        self.flags & flag != 0
    }

    pub fn is_os_update(&self) -> bool {
        self.has_flag(flags::OS_UPDATE)
    }

    pub fn version_string(&self) -> String {
        alloc::format!(
            "{}.{}.{}.{}",
            self.package_version[0],
            self.package_version[1],
            self.package_version[2],
            self.package_version[3]
        )
    }

    /// Order of the versions, major first. Used to decide whether a
    /// repository offers better than what is installed.
    pub fn newer_than(&self, other: &[u16; 4]) -> bool {
        self.package_version > *other
    }
}

/// Reasons for a refusal at parse time.
///
/// Every refusal is named: a package set aside with no explanation is the
/// worst of diagnostics for whoever is trying to install something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageError {
    TooShort,
    BadMagic,
    BadVersion,
    UnknownUpdateCode,
    UnknownSignature,
    UnknownFormat,
    SectionOutOfBounds,
    SectionOverlap,
    ResourceTableMalformed,
    EmptyManifest,
}

impl PackageError {
    pub fn message(&self) -> &'static str {
        match self {
            PackageError::TooShort => "fichier plus court qu'un en-tete SPM",
            PackageError::BadMagic => "magic absent - ce n'est pas un .spm",
            PackageError::BadVersion => "version de format non geree",
            PackageError::UnknownUpdateCode => "code de mise a jour hors du tableau 10.2",
            PackageError::UnknownSignature => "type de signature inconnu",
            PackageError::UnknownFormat => "format de binaire inconnu",
            PackageError::SectionOutOfBounds => "une section deborde du fichier",
            PackageError::SectionOverlap => "deux sections se recouvrent",
            PackageError::ResourceTableMalformed => "table des ressources incoherente",
            PackageError::EmptyManifest => "manifest absent ou vide",
        }
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

/// Reads a fixed-size text field.
///
/// Any byte outside printable ASCII becomes `?`: these strings end up on
/// the serial console and on screen, where the VGA writer would replace a
/// foreign byte with a solid square without saying why.
fn read_name(bytes: &[u8], offset: usize, length: usize) -> String {
    bytes[offset..offset + length]
        .iter()
        .copied()
        .take_while(|byte| *byte != 0)
        .map(|byte| {
            if (0x20..=0x7e).contains(&byte) {
                byte as char
            } else {
                '?'
            }
        })
        .collect()
}

fn read_digest(bytes: &[u8], offset: usize) -> [u8; DIGEST_SIZE] {
    let mut digest = [0u8; DIGEST_SIZE];
    digest.copy_from_slice(&bytes[offset..offset + DIGEST_SIZE]);
    digest
}

/// An empty section (null length) is allowed and bounds nothing: a
/// package with no post-installation script is the ordinary case.
fn section_ok(offset: u32, length: u32, total: usize) -> bool {
    if length == 0 {
        return true;
    }
    let start = offset as usize;
    let end = start.saturating_add(length as usize);
    start >= HEADER_SIZE && end <= total && end > start
}

/// Two non-empty sections must not overlap.
///
/// The check is not cosmetic: two overlapping sections would make the
/// content of the package depend on the reading order, and a package
/// could present an innocuous manifest to inspection while installing
/// something else.
fn overlaps(a: (u32, u32), b: (u32, u32)) -> bool {
    if a.1 == 0 || b.1 == 0 {
        return false;
    }
    let a_end = a.0 as usize + a.1 as usize;
    let b_end = b.0 as usize + b.1 as usize;
    (a.0 as usize) < b_end && (b.0 as usize) < a_end
}

/// Parses a package without installing anything.
pub fn parse(data: &[u8]) -> Result<PackageHeader, PackageError> {
    if data.len() < HEADER_SIZE {
        return Err(PackageError::TooShort);
    }
    if data[0..4] != MAGIC {
        return Err(PackageError::BadMagic);
    }
    let version = read_u16(data, 0x04);
    if version != FORMAT_VERSION {
        return Err(PackageError::BadVersion);
    }

    let update_code =
        UpdateCode::from_code(data[0x06] as i8).ok_or(PackageError::UnknownUpdateCode)?;
    let signature =
        SignatureType::from_code(data[0x07]).ok_or(PackageError::UnknownSignature)?;
    let format = BinaryFormat::from_code(data[0x08]).ok_or(PackageError::UnknownFormat)?;

    let header = PackageHeader {
        version,
        update_code,
        signature,
        format,
        flags: read_u32(data, 0x0C),
        manifest_offset: read_u32(data, 0x10),
        manifest_len: read_u32(data, 0x14),
        binary_offset: read_u32(data, 0x18),
        binary_len: read_u32(data, 0x1C),
        resources_offset: read_u32(data, 0x20),
        resources_len: read_u32(data, 0x24),
        pre_offset: read_u32(data, 0x28),
        pre_len: read_u32(data, 0x2C),
        post_offset: read_u32(data, 0x30),
        post_len: read_u32(data, 0x34),
        package_version: [
            read_u16(data, 0x38),
            read_u16(data, 0x3A),
            read_u16(data, 0x3C),
            read_u16(data, 0x3E),
        ],
        app_id: read_name(data, 0x40, NAME_LEN),
        display_name: read_name(data, 0x60, NAME_LEN),
        publisher: read_name(data, 0x80, NAME_LEN),
        payload_hash: read_digest(data, 0xA0),
        binding: read_digest(data, 0xC0),
        authority_key: read_digest(data, 0xE0),
    };

    let sections = [
        (header.manifest_offset, header.manifest_len),
        (header.binary_offset, header.binary_len),
        (header.resources_offset, header.resources_len),
        (header.pre_offset, header.pre_len),
        (header.post_offset, header.post_len),
    ];
    for (offset, length) in sections {
        if !section_ok(offset, length, data.len()) {
            return Err(PackageError::SectionOutOfBounds);
        }
    }
    for index in 0..sections.len() {
        for other in index + 1..sections.len() {
            if overlaps(sections[index], sections[other]) {
                return Err(PackageError::SectionOverlap);
            }
        }
    }

    if header.manifest_len == 0 {
        return Err(PackageError::EmptyManifest);
    }

    // The resource table is checked at parse time: a package one of whose
    // entries points outside the block must not be declared valid and
    // then fail mid-installation, when half the files are already laid
    // down on the disk.
    if header.resources_len > 0 && read_resources(&header, data).is_none() {
        return Err(PackageError::ResourceTableMalformed);
    }

    Ok(header)
}

fn slice<'a>(data: &'a [u8], offset: u32, length: u32) -> &'a [u8] {
    if length == 0 {
        return &[];
    }
    &data[offset as usize..offset as usize + length as usize]
}

/// Text of the manifest.
pub fn manifest_bytes<'a>(header: &PackageHeader, data: &'a [u8]) -> &'a [u8] {
    slice(data, header.manifest_offset, header.manifest_len)
}

/// Decoded manifest.
pub fn manifest(header: &PackageHeader, data: &[u8]) -> Manifest {
    Manifest::parse(core::str::from_utf8(manifest_bytes(header, data)).unwrap_or(""))
}

/// Main binary.
pub fn binary<'a>(header: &PackageHeader, data: &'a [u8]) -> &'a [u8] {
    slice(data, header.binary_offset, header.binary_len)
}

/// Pre-installation script (8.1.3).
pub fn pre_script<'a>(header: &PackageHeader, data: &'a [u8]) -> &'a [u8] {
    slice(data, header.pre_offset, header.pre_len)
}

/// Post-installation script (8.1.3).
pub fn post_script<'a>(header: &PackageHeader, data: &'a [u8]) -> &'a [u8] {
    slice(data, header.post_offset, header.post_len)
}

/// Resource table, or `None` if an entry overflows the block.
pub fn read_resources(header: &PackageHeader, data: &[u8]) -> Option<Vec<Resource>> {
    if header.resources_len == 0 {
        return Some(Vec::new());
    }
    let block = slice(data, header.resources_offset, header.resources_len);
    if block.len() < 4 {
        return None;
    }
    let count = read_u32(block, 0) as usize;
    let table_end = 4 + count.checked_mul(RESOURCE_ENTRY)?;
    if table_end > block.len() {
        return None;
    }

    let mut resources = Vec::with_capacity(count);
    for index in 0..count {
        let base = 4 + index * RESOURCE_ENTRY;
        let name = read_name(block, base, NAME_LEN);
        let offset = read_u32(block, base + NAME_LEN);
        let length = read_u32(block, base + NAME_LEN + 4);
        let end = (offset as usize).checked_add(length as usize)?;
        if name.is_empty() || (offset as usize) < table_end || end > block.len() {
            return None;
        }
        resources.push(Resource {
            name,
            offset,
            length,
        });
    }
    Some(resources)
}

/// Content of a resource.
pub fn resource_bytes<'a>(
    header: &PackageHeader,
    data: &'a [u8],
    resource: &Resource,
) -> &'a [u8] {
    let block = slice(data, header.resources_offset, header.resources_len);
    &block[resource.offset as usize..resource.offset as usize + resource.length as usize]
}

// ============================================================
//  Digests and verification
// ============================================================

/// Payload digest: everything following the header.
pub fn payload_digest(data: &[u8]) -> [u8; DIGEST_SIZE] {
    if data.len() <= HEADER_SIZE {
        return sha256::digest(&[]);
    }
    sha256::digest(&data[HEADER_SIZE..])
}

/// Authenticity digest: header with derived fields neutralised, then
/// payload.
///
/// **Two** fields are neutralised, and not one:
///
/// * the payload digest (0xA0), for the obvious reason that a digest
///   cannot contain itself;
/// * the **certificate binding** (0xC0), for exactly the same reason,
///   less obvious because it goes through a detour. The binding is
///   `SHA-256(key || authenticity digest)`: it therefore depends on the
///   digest, which would cover the header, which contains the binding.
///   The builder writes the binding **after** computing the digest;
///   without neutralisation, the verifier would recompute on a header
///   where the binding is now filled in, and would never obtain the same
///   value. No ENTERPRISE package would be acceptable.
///
/// The authority key (0xE0), on the other hand, stays covered: it is an
/// input of the computation, not a result. Covering it is what stops one
/// authority being substituted for another while keeping the binding.
pub fn authenticity_digest(data: &[u8]) -> [u8; DIGEST_SIZE] {
    let mut hasher = sha256::Sha256::new();
    hasher.update(&data[..0xA0]);
    // Payload digest and binding, contiguous: 64 bytes.
    hasher.update_zeros(DIGEST_SIZE * 2);
    hasher.update(&data[0xE0..HEADER_SIZE.min(data.len())]);
    if data.len() > HEADER_SIZE {
        hasher.update(&data[HEADER_SIZE..]);
    }
    hasher.finish()
}

/// Checks the integrity and the signature of an already parsed package.
///
/// The verdict and the reasons are those of phase 3: they are the same
/// questions, put on another container. Having two sets would force
/// translating one into the other at every boundary, and a translation is
/// a place where a rejection can be lost.
pub fn verify(header: &PackageHeader, data: &[u8]) -> Verdict {
    // 3.4 applies to the binary carried: a package cannot deliver a
    // self-signed `.dsa` on the pretext that the wrapper is in order.
    if !combination_allowed(header.format, header.signature) {
        return Verdict::Rejected(Reason::ForbiddenCombination);
    }

    let payload = payload_digest(data);
    if !sha256::equal(&payload, &header.payload_hash) {
        return Verdict::Rejected(Reason::PayloadTampered);
    }

    let digest = authenticity_digest(data);

    match header.signature {
        SignatureType::System => {
            if crate::binfmt::signature::is_official(&digest) {
                Verdict::System
            } else {
                Verdict::Rejected(Reason::UnknownSystemBinary)
            }
        }
        SignatureType::Enterprise => {
            if header.authority_key == [0u8; DIGEST_SIZE] {
                return Verdict::Rejected(Reason::MalformedCertificate);
            }
            if !crate::binfmt::signature::is_trusted_authority(&header.authority_key) {
                return Verdict::Rejected(Reason::UntrustedAuthority);
            }
            if !sha256::equal(
                &header.binding,
                &expected_binding(&header.authority_key, &digest),
            ) {
                return Verdict::Rejected(Reason::BindingMismatch);
            }
            Verdict::Enterprise
        }
        SignatureType::SelfSigned => Verdict::SelfSigned,
        SignatureType::None => Verdict::Unsigned,
    }
}

// ============================================================
//  10.4 -- Warning levels at installation
// ============================================================

/// The five lines of the 10.4 table, each carrying its **action**.
///
/// The "Level" column already exists as text in
/// `SignatureType::warning_level`; what was missing, and what phase 8
/// adds, is what the system must *do* with each line. The text stays
/// borrowed from phase 3: a spec table transcribed twice is a table that
/// will diverge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarningLevel {
    /// SYSTEM + any type -- official badge.
    OfficialBadge,
    /// ENTERPRISE + `.cus`/`.gs` -- verified badge.
    VerifiedBadge,
    /// ENTERPRISE + `.ss`/`.dsa` -- mandatory admin prompt.
    AdminPrompt,
    /// SELF + any type -- yellow warning.
    Unverified,
    /// No signature -- blocking red warning.
    Dangerous,
}

impl WarningLevel {
    /// Transcription of the 10.4 table.
    ///
    /// **Point where the spec leaves the choice**: the table names the
    /// last line "None + `.ras`", whereas 3.4 also allows "None +
    /// `.cus`". So the combination exists with no line of its own. The
    /// criterion telling that line from the others is the absence of a
    /// signature, not the format: that is the reading kept, and an
    /// unsigned `.cus` receives the same blocking red warning as a `.ras`.
    pub fn of(signature: SignatureType, format: BinaryFormat) -> WarningLevel {
        match (signature, format) {
            (SignatureType::System, _) => WarningLevel::OfficialBadge,
            (SignatureType::Enterprise, BinaryFormat::Cus | BinaryFormat::Gs) => {
                WarningLevel::VerifiedBadge
            }
            (SignatureType::Enterprise, _) => WarningLevel::AdminPrompt,
            (SignatureType::SelfSigned, _) => WarningLevel::Unverified,
            (SignatureType::None, _) => WarningLevel::Dangerous,
        }
    }

    /// "Level" column.
    pub fn as_str(&self) -> &'static str {
        match self {
            WarningLevel::OfficialBadge => "badge officiel",
            WarningLevel::VerifiedBadge => "badge verifie",
            WarningLevel::AdminPrompt => "avertissement",
            WarningLevel::Unverified => "non verifie",
            WarningLevel::Dangerous => "dangereux",
        }
    }

    /// "Action" column.
    pub fn action(&self) -> &'static str {
        match self {
            WarningLevel::OfficialBadge | WarningLevel::VerifiedBadge => "aucun avertissement",
            WarningLevel::AdminPrompt => "prompt admin obligatoire",
            WarningLevel::Unverified => "avertissement jaune",
            WarningLevel::Dangerous => "avertissement rouge bloquant",
        }
    }

    /// Does the installation demand explicit agreement?
    ///
    /// "Blocking" and "mandatory prompt" both translate into a question
    /// asked; "yellow" is only a warning displayed.
    pub fn requires_consent(&self) -> bool {
        matches!(self, WarningLevel::AdminPrompt | WarningLevel::Dangerous)
    }

    /// Only the administrator role can lift this warning.
    pub fn requires_admin(&self) -> bool {
        *self == WarningLevel::AdminPrompt
    }

    /// Must the user see something?
    pub fn warns(&self) -> bool {
        !matches!(
            self,
            WarningLevel::OfficialBadge | WarningLevel::VerifiedBadge
        )
    }
}

// ============================================================
//  Building a package
// ============================================================

/// Build parameters.
pub struct BuildSpec<'a> {
    pub update_code: UpdateCode,
    pub signature: SignatureType,
    pub format: BinaryFormat,
    pub flags: u32,
    pub version: [u16; 4],
    pub display_name: &'a str,
    pub publisher: &'a str,
    pub manifest: &'a Manifest,
    pub binary: &'a [u8],
    pub resources: &'a [(&'a str, &'a [u8])],
    pub pre_script: &'a str,
    pub post_script: &'a str,
    /// Key of the issuing authority, for an ENTERPRISE package. The
    /// binding is computed, never supplied: leaving it as a parameter
    /// would allow building the inconsistent package `verify` must
    /// reject, and it is up to the test to corrupt the byte, not to the
    /// builder to offer to do it.
    pub authority_key: Option<[u8; DIGEST_SIZE]>,
}

fn put_name(buffer: &mut [u8], offset: usize, text: &str) {
    for (index, byte) in text.bytes().take(NAME_LEN - 1).enumerate() {
        buffer[offset + index] = byte;
    }
}

/// Assembles the resource block.
fn build_resources(resources: &[(&str, &[u8])]) -> Vec<u8> {
    if resources.is_empty() {
        return Vec::new();
    }
    let table = 4 + resources.len() * RESOURCE_ENTRY;
    let total: usize = resources.iter().map(|(_, bytes)| bytes.len()).sum();
    let mut block = alloc::vec![0u8; table + total];

    block[0..4].copy_from_slice(&(resources.len() as u32).to_le_bytes());
    let mut cursor = table;
    for (index, (name, bytes)) in resources.iter().enumerate() {
        let base = 4 + index * RESOURCE_ENTRY;
        put_name(&mut block, base, name);
        block[base + NAME_LEN..base + NAME_LEN + 4].copy_from_slice(&(cursor as u32).to_le_bytes());
        block[base + NAME_LEN + 4..base + NAME_LEN + 8]
            .copy_from_slice(&(bytes.len() as u32).to_le_bytes());
        block[cursor..cursor + bytes.len()].copy_from_slice(bytes);
        cursor += bytes.len();
    }
    block
}

/// Assembles a complete `.spm` file.
pub fn build(spec: &BuildSpec) -> Vec<u8> {
    let manifest = spec.manifest.to_text();
    let manifest = manifest.as_bytes();
    let resources = build_resources(spec.resources);
    let pre = spec.pre_script.as_bytes();
    let post = spec.post_script.as_bytes();

    let mut file = alloc::vec![0u8; HEADER_SIZE];
    let push = |file: &mut Vec<u8>, bytes: &[u8]| -> (u32, u32) {
        if bytes.is_empty() {
            return (0, 0);
        }
        let offset = file.len() as u32;
        file.extend_from_slice(bytes);
        (offset, bytes.len() as u32)
    };

    let (manifest_offset, manifest_len) = push(&mut file, manifest);
    let (binary_offset, binary_len) = push(&mut file, spec.binary);
    let (resources_offset, resources_len) = push(&mut file, &resources);
    let (pre_offset, pre_len) = push(&mut file, pre);
    let (post_offset, post_len) = push(&mut file, post);

    file[0..4].copy_from_slice(&MAGIC);
    file[0x04..0x06].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    file[0x06] = spec.update_code.code() as u8;
    file[0x07] = spec.signature as u8;
    file[0x08] = spec.format as u8;
    file[0x0C..0x10].copy_from_slice(&spec.flags.to_le_bytes());
    file[0x10..0x14].copy_from_slice(&manifest_offset.to_le_bytes());
    file[0x14..0x18].copy_from_slice(&manifest_len.to_le_bytes());
    file[0x18..0x1C].copy_from_slice(&binary_offset.to_le_bytes());
    file[0x1C..0x20].copy_from_slice(&binary_len.to_le_bytes());
    file[0x20..0x24].copy_from_slice(&resources_offset.to_le_bytes());
    file[0x24..0x28].copy_from_slice(&resources_len.to_le_bytes());
    file[0x28..0x2C].copy_from_slice(&pre_offset.to_le_bytes());
    file[0x2C..0x30].copy_from_slice(&pre_len.to_le_bytes());
    file[0x30..0x34].copy_from_slice(&post_offset.to_le_bytes());
    file[0x34..0x38].copy_from_slice(&post_len.to_le_bytes());
    for (index, part) in spec.version.iter().enumerate() {
        let base = 0x38 + index * 2;
        file[base..base + 2].copy_from_slice(&part.to_le_bytes());
    }
    put_name(&mut file, 0x40, &spec.manifest.app_id);
    put_name(&mut file, 0x60, spec.display_name);
    put_name(&mut file, 0x80, spec.publisher);

    // The payload digest first: it depends only on what follows the
    // header. The binding next, because it covers the header -- digest
    // included -- and must therefore be computed afterwards.
    let payload = payload_digest(&file);
    file[0xA0..0xA0 + DIGEST_SIZE].copy_from_slice(&payload);

    if let Some(key) = spec.authority_key {
        file[0xE0..0xE0 + DIGEST_SIZE].copy_from_slice(&key);
        let digest = authenticity_digest(&file);
        let binding = expected_binding(&key, &digest);
        file[0xC0..0xC0 + DIGEST_SIZE].copy_from_slice(&binding);
    }

    file
}

/// Logs the detail of a package on the serial console.
pub fn dump(header: &PackageHeader, data: &[u8], verdict: Verdict) {
    let level = WarningLevel::of(header.signature, header.format);
    crate::serial_println!(
        "[spm]   paquet       : {} \"{}\" v{}",
        header.app_id,
        header.display_name,
        header.version_string()
    );
    crate::serial_println!(
        "[spm]   editeur      : {} - format {} - signature {}",
        header.publisher,
        header.format.extension(),
        header.signature.as_str()
    );
    crate::serial_println!(
        "[spm]   code MAJ     : {} ({}) - {}",
        header.update_code.code(),
        header.update_code.as_str(),
        header.update_code.behaviour()
    );
    crate::serial_println!(
        "[spm]   sections     : manifest {} o, binaire {} o, ressources {} o, scripts {}+{} o",
        header.manifest_len,
        header.binary_len,
        header.resources_len,
        header.pre_len,
        header.post_len
    );
    let hex = sha256::short_hex(&authenticity_digest(data));
    crate::serial_println!(
        "[spm]   empreinte    : {}... - verdict {}",
        core::str::from_utf8(&hex).unwrap_or("?"),
        verdict.as_str()
    );
    crate::serial_println!(
        "[spm]   spec 10.4    : {} - {}",
        level.as_str(),
        level.action()
    );
    if let Verdict::Rejected(reason) = verdict {
        crate::serial_println!("[spm]   cause        : {}", reason.message());
    }
}

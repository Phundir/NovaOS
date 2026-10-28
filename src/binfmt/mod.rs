//! Phase 3 -- Nova OS binary formats (spec 3).
//!
//! This module decodes and validates; it loads nothing. The split of
//! responsibilities follows that of the roadmap:
//!
//!   * this file and `header` / `identity` -- structural parsing and
//!     confrontation with the rules of the specification (3.1);
//!   * `loader` -- section mapping, relocations, transfer of control and
//!     enforcement of the per-type rules (3.2);
//!   * `sha256` and `signature` -- digests and verification of the three
//!     signature levels (3.3);
//!   * `sample` -- generator of test binaries, for want of a filesystem
//!     before phase 4.
//!
//! Validation happens in two stages. *Structural* coherence rejects what
//! is malformed -- unknown magic, inconsistent sizes, out-of-bounds
//! offsets. *Regulatory* coherence rejects what is well formed but
//! forbidden by the specification: a self-signed `.dsa`, a `.ras`
//! claiming IPC, a `.cus` asking for kernel space access.

pub mod header;
pub mod identity;
pub mod loader;
pub mod sample;
pub mod sha256;
pub mod signature;

use alloc::string::String;
use alloc::vec::Vec;

use header::{
    BinaryFormat, CERT_SIZE, HEADER_SIZE, HEADER_VERSION, NAME_LEN, RELOC_ENTRY_SIZE, Reloc,
    RelocKind, SECTION_ENTRY_SIZE, Section, SectionKind, SignatureType, flags, offsets,
    read_u16, read_u32, read_u64,
};
use identity::{AppIdentity, Permissions, Version};

// ============================================================
//  Errors
// ============================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// Fewer than 256 bytes: not even a complete header.
    TooSmall,
    /// The first four bytes match no known format.
    UnknownMagic,
    /// The `binary_type` field contradicts the magic number.
    TypeMismatch,
    /// Header version produced by a newer tool.
    UnsupportedHeaderVersion(u16),
    /// `header_size` field different from the expected constant.
    BadHeaderSize(u16),
    /// Signature code outside the four defined values.
    UnknownSignature(u8),
    /// Text field not conforming to UTF-8.
    InvalidString(&'static str),
    /// `app_id` empty -- the identity is mandatory.
    MissingAppId,
    /// An offset or a size exceeds the limits of the file.
    OutOfBounds(&'static str),
    /// Entry point outside the loaded image.
    EntryPointOutOfImage,
    /// Permission bit unknown to this kernel.
    UnknownPermission(u64),
    /// Flag unknown to this kernel.
    UnknownFlag(u32),
    /// Section nature not defined.
    UnknownSectionKind(u8),
    /// Relocation type unknown to this kernel.
    UnknownRelocKind(u8),
    /// Relocation bearing on a word outside the image.
    RelocOutOfImage,
    /// Relocation table badly sized.
    BadRelocTable,
    /// Certificate block outside the file or malformed.
    BadCertificate(&'static str),
    /// Non-loadable section carrying a virtual address.
    StraySectionAddress,
}

impl ParseError {
    pub fn message(&self) -> &'static str {
        match self {
            ParseError::TooSmall => "fichier trop court pour contenir un header",
            ParseError::UnknownMagic => "magic number inconnu",
            ParseError::TypeMismatch => "le type declare contredit le magic number",
            ParseError::UnsupportedHeaderVersion(_) => "version de header non supportee",
            ParseError::BadHeaderSize(_) => "taille de header incorrecte",
            ParseError::UnknownSignature(_) => "type de signature inconnu",
            ParseError::InvalidString(_) => "champ textuel non conforme a l'UTF-8",
            ParseError::MissingAppId => "app_id absent",
            ParseError::OutOfBounds(_) => "offset ou taille hors limites",
            ParseError::EntryPointOutOfImage => "point d'entree hors de l'image",
            ParseError::UnknownPermission(_) => "permission inconnue de ce noyau",
            ParseError::UnknownFlag(_) => "drapeau inconnu de ce noyau",
            ParseError::UnknownSectionKind(_) => "nature de section inconnue",
            ParseError::UnknownRelocKind(_) => "type de relocation inconnu",
            ParseError::RelocOutOfImage => "relocation hors de l'image",
            ParseError::BadRelocTable => "table de relocations mal dimensionnee",
            ParseError::BadCertificate(_) => "bloc de certificat invalide",
            ParseError::StraySectionAddress => {
                "section non chargeable portant une adresse virtuelle"
            }
        }
    }
}

/// Violation of a rule of the specification, on an otherwise well-formed
/// binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleViolation {
    /// `.dsa` with no SYSTEM signature (spec 3.4).
    DsaRequiresSystem,
    /// `.ss` signed SELF or unsigned (spec 3.4).
    SsRequiresSystemOrEnterprise,
    /// `.gs` signed SYSTEM -- reserved to official applications.
    GsSignatureNotAllowed,
    /// Permission beyond the format ceiling (spec 12.4).
    PermissionAboveCeiling(u64),
    /// `.ras` claiming inter-process communication (spec 12.3).
    RasCannotUseIpc,
    /// `.ras` declaring itself the default application (spec 3.4).
    RasCannotBeDefault,
    /// `EXPOSES_API` outside a `.dsa` (spec 3.4).
    OnlyDsaExposesApi,
    /// `REPLACES_COMPONENT` outside a `.ss` (spec 3.4).
    OnlySsReplacesComponent,
    /// `AUTO_PAUSE` outside a `.gs` (spec 3.4).
    OnlyGsAutoPauses,
    /// Section both writable and executable.
    SectionViolatesWx,
    /// Internal ports declared without the matching permission.
    PortsWithoutPermission,
    /// `.ras` declaring itself signed: the format is by definition
    /// unverified, letting it claim a level of trust would open a
    /// laundering of privileges.
    RasMustBeUnsigned,
    /// ENTERPRISE signature announced with no certificate block (spec 3.2).
    EnterpriseNeedsCertificate,
    /// Certificate block present on a non-ENTERPRISE binary.
    CertificateNotExpected,
    /// Two sections with different rights share a page: the loader could
    /// not apply distinct permissions to them.
    SectionsSharePage,
}

impl RuleViolation {
    pub fn message(&self) -> &'static str {
        match self {
            RuleViolation::DsaRequiresSystem => {
                "un .dsa exige une signature SYSTEM (spec 3.4)"
            }
            RuleViolation::SsRequiresSystemOrEnterprise => {
                "un .ss exige une signature SYSTEM ou ENTERPRISE (spec 3.4)"
            }
            RuleViolation::GsSignatureNotAllowed => {
                "un .gs accepte ENTERPRISE ou SELF, pas SYSTEM (spec 3.4)"
            }
            RuleViolation::PermissionAboveCeiling(_) => {
                "permission au-dela du plafond du format (spec 12.4)"
            }
            RuleViolation::RasCannotUseIpc => {
                "un .ras est totalement isole de l'IPC (spec 12.3)"
            }
            RuleViolation::RasCannotBeDefault => {
                "un .ras ne peut pas etre application par defaut (spec 3.4)"
            }
            RuleViolation::OnlyDsaExposesApi => {
                "seul un .dsa peut exposer une API publique (spec 3.4)"
            }
            RuleViolation::OnlySsReplacesComponent => {
                "seul un .ss peut remplacer un composant systeme (spec 3.4)"
            }
            RuleViolation::OnlyGsAutoPauses => {
                "la bascule automatique est propre aux .gs (spec 3.4)"
            }
            RuleViolation::SectionViolatesWx => {
                "section a la fois modifiable et executable"
            }
            RuleViolation::PortsWithoutPermission => {
                "ports internes declares sans IPC_INTERNAL_PORTS (spec 12.2)"
            }
            RuleViolation::RasMustBeUnsigned => {
                "un .ras ne peut revendiquer aucune signature (spec 3.4)"
            }
            RuleViolation::EnterpriseNeedsCertificate => {
                "signature ENTERPRISE sans bloc de certificat (spec 3.2)"
            }
            RuleViolation::CertificateNotExpected => {
                "bloc de certificat sur un binaire non ENTERPRISE (spec 3.2)"
            }
            RuleViolation::SectionsSharePage => {
                "deux sections aux droits differents partagent une page"
            }
        }
    }
}

// ============================================================
//  Decoded header
// ============================================================

/// Header fully decoded and structurally valid.
#[derive(Debug, Clone)]
pub struct BinaryHeader {
    // --- 3.1.3: identity ---
    pub identity: AppIdentity,

    // --- memory layout ---
    pub entry_point: u64,
    pub load_base: u64,
    pub image_size: u64,
    pub bss_size: u64,
    pub stack_size: u64,

    // --- declarations ---
    pub permissions: Permissions,
    pub flags: u32,
    /// Internal application ports (spec 12.2). Two applications may use
    /// the same number without conflict: those ports are never mapped
    /// outwards.
    pub internal_ports: Vec<u16>,

    // --- content ---
    pub sections: Vec<Section>,
    /// 3.2.2 -- Fixes to apply if the image is not loaded at the base it
    /// was linked for.
    pub relocations: Vec<Reloc>,
    pub payload_offset: u32,
    pub payload_size: u32,
    /// SHA-256 digest of the payload, checked at load time.
    pub payload_hash: [u8; 32],
    /// Offset of the certificate block in the file, 0 if absent.
    pub cert_offset: u32,

    pub abi_version: u16,
}

impl BinaryHeader {
    pub fn format(&self) -> BinaryFormat {
        self.identity.format
    }

    pub fn signature(&self) -> SignatureType {
        self.identity.signature
    }

    pub fn has_flag(&self, flag: u32) -> bool {
        self.flags & flag != 0
    }

    /// Scheduling priority, derived from the format (spec 7.3).
    pub fn priority(&self) -> u8 {
        self.identity.format.priority()
    }

    /// Total memory to reserve: loaded image plus uninitialised area plus
    /// stack.
    pub fn total_memory(&self) -> u64 {
        self.image_size + self.bss_size + self.stack_size
    }

    /// Does the binary carry a certificate block?
    pub fn has_certificate(&self) -> bool {
        self.cert_offset != 0
    }

    /// Sections actually loaded into memory, in file order.
    pub fn loadable_sections(&self) -> impl Iterator<Item = &Section> {
        self.sections.iter().filter(|s| s.kind.is_loadable())
    }
}

// ============================================================
//  Parsing
// ============================================================

/// Extracts a fixed-size text field, stopping at the first null byte.
fn read_name(data: &[u8], offset: usize, field: &'static str) -> Result<String, ParseError> {
    if offset + NAME_LEN > data.len() {
        return Err(ParseError::OutOfBounds(field));
    }
    let raw = &data[offset..offset + NAME_LEN];
    let len = raw.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
    core::str::from_utf8(&raw[..len])
        .map(String::from)
        .map_err(|_| ParseError::InvalidString(field))
}

/// 3.1.2 -- Recognises the format of a file without decoding it fully.
///
/// Useful to the file manager and to SPM, which must display a type
/// without bearing the cost of a complete parse.
pub fn identify(data: &[u8]) -> Option<BinaryFormat> {
    BinaryFormat::from_magic(data.get(..4)?)
}

/// 3.1.1 and 3.1.3 -- Decodes and structurally validates a header.
///
/// Does not check the rules of the specification: call `check_rules`
/// afterwards, or go through `inspect`, which chains the two.
pub fn parse(data: &[u8]) -> Result<BinaryHeader, ParseError> {
    parse_prefix(data, data.len())
}

/// `parse` from the beginning of a file of `file_len` bytes: `data` must
/// hold the header and its tables, the payload may lie beyond it. A
/// linked image (spec 3.5) weighs megabytes; its payload is checked
/// against its digest by streaming, not read whole into the heap.
pub fn parse_prefix(data: &[u8], file_len: usize) -> Result<BinaryHeader, ParseError> {
    if data.len() < HEADER_SIZE {
        return Err(ParseError::TooSmall);
    }

    // --- 3.1.2: magic number ---
    let format =
        BinaryFormat::from_magic(&data[offsets::MAGIC..]).ok_or(ParseError::UnknownMagic)?;

    // The type is declared twice: any divergence gives away a file whose
    // extension or magic was changed after the fact.
    let declared = data[offsets::BINARY_TYPE];
    match BinaryFormat::from_code(declared) {
        Some(t) if t == format => {}
        _ => return Err(ParseError::TypeMismatch),
    }

    let header_version = read_u16(data, offsets::HEADER_VERSION);
    if header_version != HEADER_VERSION {
        return Err(ParseError::UnsupportedHeaderVersion(header_version));
    }

    let header_size = read_u16(data, offsets::HEADER_SIZE);
    if header_size as usize != HEADER_SIZE {
        return Err(ParseError::BadHeaderSize(header_size));
    }

    let abi_version = read_u16(data, offsets::ABI_VERSION);

    let signature = SignatureType::from_code(data[offsets::SIGNATURE_TYPE])
        .ok_or(ParseError::UnknownSignature(data[offsets::SIGNATURE_TYPE]))?;

    // --- flags ---
    let flag_bits = read_u32(data, offsets::FLAGS);
    if flag_bits & !flags::ALL != 0 {
        return Err(ParseError::UnknownFlag(flag_bits & !flags::ALL));
    }

    // --- memory layout ---
    let entry_point = read_u64(data, offsets::ENTRY_POINT);
    let load_base = read_u64(data, offsets::LOAD_BASE);
    let image_size = read_u64(data, offsets::IMAGE_SIZE);
    let bss_size = read_u64(data, offsets::BSS_SIZE);
    let stack_size = read_u64(data, offsets::STACK_SIZE);

    if image_size == 0 {
        return Err(ParseError::OutOfBounds("image_size nul"));
    }
    if entry_point < load_base || entry_point >= load_base + image_size {
        return Err(ParseError::EntryPointOutOfImage);
    }

    // --- 3.1.3: identity ---
    let app_id = read_name(data, offsets::APP_ID, "app_id")?;
    if app_id.is_empty() {
        return Err(ParseError::MissingAppId);
    }
    let author = read_name(data, offsets::AUTHOR, "author")?;
    let group = read_name(data, offsets::GROUP, "group")?;
    let display_name = read_name(data, offsets::DISPLAY_NAME, "display_name")?;

    let version = Version::new(
        read_u16(data, offsets::VERSION_MAJOR),
        read_u16(data, offsets::VERSION_MINOR),
        read_u16(data, offsets::VERSION_PATCH),
        read_u16(data, offsets::VERSION_BUILD),
    );

    // --- permissions ---
    let permissions = Permissions(read_u64(data, offsets::PERMISSIONS));
    let unknown = permissions.unknown_bits();
    if unknown != 0 {
        // Refuse rather than ignore: silently granting a permission not
        // understood would be a hole.
        return Err(ParseError::UnknownPermission(unknown));
    }

    // --- internal ports (spec 12.2) ---
    let port_count = read_u16(data, offsets::PORT_COUNT) as usize;
    let port_table = read_u16(data, offsets::PORT_TABLE) as usize;
    let mut internal_ports = Vec::with_capacity(port_count);

    if port_count > 0 {
        let end = port_table
            .checked_add(port_count * 2)
            .ok_or(ParseError::OutOfBounds("table des ports"))?;
        if port_table < HEADER_SIZE || end > data.len() {
            return Err(ParseError::OutOfBounds("table des ports"));
        }
        for i in 0..port_count {
            internal_ports.push(read_u16(data, port_table + i * 2));
        }
    }

    // --- section table ---
    let section_count = read_u32(data, offsets::SECTION_COUNT) as usize;
    let section_table = read_u32(data, offsets::SECTION_TABLE) as usize;
    let mut sections = Vec::with_capacity(section_count);

    if section_count > 0 {
        let end = section_table
            .checked_add(section_count * SECTION_ENTRY_SIZE)
            .ok_or(ParseError::OutOfBounds("table des sections"))?;
        if section_table < HEADER_SIZE || end > data.len() {
            return Err(ParseError::OutOfBounds("table des sections"));
        }

        for i in 0..section_count {
            let base = section_table + i * SECTION_ENTRY_SIZE;

            let mut name = [0u8; 8];
            name.copy_from_slice(&data[base..base + 8]);

            let kind = SectionKind::from_code(data[base + 8])
                .ok_or(ParseError::UnknownSectionKind(data[base + 8]))?;
            let flags = data[base + 9];
            let align = read_u16(data, base + 10);
            let file_offset = read_u32(data, base + 12);
            let file_size = read_u32(data, base + 16);
            let mem_size = read_u32(data, base + 20);
            let virt_addr = read_u64(data, base + 24);

            // A non-empty section must fit in the file.
            if file_size > 0 {
                let section_end = (file_offset as usize)
                    .checked_add(file_size as usize)
                    .ok_or(ParseError::OutOfBounds("section"))?;
                if section_end > data.len() || (file_offset as usize) < HEADER_SIZE {
                    return Err(ParseError::OutOfBounds("contenu de section"));
                }
            }

            if kind.is_loadable() {
                // A loaded section must fit in the announced image.
                if virt_addr < load_base
                    || virt_addr.checked_add(mem_size as u64).is_none()
                    || virt_addr + mem_size as u64 > load_base + image_size
                {
                    return Err(ParseError::OutOfBounds("section hors de l'image"));
                }
            } else if virt_addr != 0 || mem_size != 0 {
                // Relocations and certificate exist only in the file:
                // giving them an address would suggest they will be
                // mapped, which the loader will never do.
                return Err(ParseError::StraySectionAddress);
            }

            sections.push(Section {
                name,
                kind,
                flags,
                align,
                file_offset,
                file_size,
                mem_size,
                virt_addr,
            });
        }
    }

    // --- payload ---
    let payload_offset = read_u32(data, offsets::PAYLOAD_OFFSET);
    let payload_size = read_u32(data, offsets::PAYLOAD_SIZE);

    if payload_size > 0 {
        let end = (payload_offset as usize)
            .checked_add(payload_size as usize)
            .ok_or(ParseError::OutOfBounds("charge utile"))?;
        if (payload_offset as usize) < HEADER_SIZE || end > file_len {
            return Err(ParseError::OutOfBounds("charge utile"));
        }
    }

    let mut payload_hash = [0u8; 32];
    payload_hash.copy_from_slice(&data[offsets::PAYLOAD_HASH..offsets::PAYLOAD_HASH + 32]);

    // --- 3.2.2: relocation table ---
    // Carried by a dedicated section rather than by a header field: the
    // format stays extensible without touching the frozen 256 bytes.
    let mut relocations = Vec::new();
    for section in sections.iter().filter(|s| s.kind == SectionKind::Reloc) {
        if section.file_size as usize % RELOC_ENTRY_SIZE != 0 {
            return Err(ParseError::BadRelocTable);
        }
        let count = section.file_size as usize / RELOC_ENTRY_SIZE;
        relocations.reserve(count);

        for i in 0..count {
            let base = section.file_offset as usize + i * RELOC_ENTRY_SIZE;
            let virt_addr = read_u64(data, base);
            let kind = RelocKind::from_code(data[base + 8])
                .ok_or(ParseError::UnknownRelocKind(data[base + 8]))?;
            let addend = read_u32(data, base + 12);

            // The fixed word must fit entirely in the image: an
            // overflowing relocation would write into the next page,
            // which belongs to another application.
            if virt_addr < load_base
                || virt_addr.checked_add(8).is_none()
                || virt_addr + 8 > load_base + image_size
            {
                return Err(ParseError::RelocOutOfImage);
            }

            relocations.push(Reloc {
                virt_addr,
                kind,
                addend,
            });
        }
    }

    // --- certificate block (spec 3.2) ---
    let cert_offset = read_u32(data, offsets::CERT_OFFSET);
    if cert_offset != 0 {
        let end = (cert_offset as usize)
            .checked_add(CERT_SIZE)
            .ok_or(ParseError::BadCertificate("offset aberrant"))?;
        if (cert_offset as usize) < HEADER_SIZE || end > data.len() {
            return Err(ParseError::BadCertificate("hors du fichier"));
        }
    }

    Ok(BinaryHeader {
        identity: AppIdentity {
            app_id,
            author,
            group,
            display_name,
            version,
            format,
            signature,
        },
        entry_point,
        load_base,
        image_size,
        bss_size,
        stack_size,
        permissions,
        flags: flag_bits,
        internal_ports,
        sections,
        relocations,
        payload_offset,
        payload_size,
        payload_hash,
        cert_offset,
        abi_version,
    })
}

/// Confronts a header with the rules of the specification.
///
/// Returns the list of violations: an empty list means the binary is
/// admissible. The cryptographic verification proper stays the business
/// of 3.3 -- here only the coherence of the declarations is judged.
pub fn check_rules(header: &BinaryHeader) -> Vec<RuleViolation> {
    use BinaryFormat::*;
    use SignatureType::*;

    let mut violations = Vec::new();
    let format = header.format();
    let signature = header.signature();

    // --- format / signature combinations (spec 3.4) ---
    match (format, signature) {
        (Dsa, System) => {}
        (Dsa, _) => violations.push(RuleViolation::DsaRequiresSystem),

        (Ss, System) | (Ss, Enterprise) => {}
        (Ss, _) => violations.push(RuleViolation::SsRequiresSystemOrEnterprise),

        (Gs, Enterprise) | (Gs, SelfSigned) => {}
        (Gs, _) => violations.push(RuleViolation::GsSignatureNotAllowed),

        // The spec demands no signature of a `.ras`, but letting it claim
        // one would amount to offering a laundering path: anyone can
        // produce a `.ras`, and the format is the one claiming the most
        // hardware access.
        (Ras, None) => {}
        (Ras, _) => violations.push(RuleViolation::RasMustBeUnsigned),

        // A `.cus` accepts the four levels: SYSTEM for the tools shipped
        // with the OS, NONE at the price of a red warning.
        (Cus, _) => {}
    }

    // --- 3.3.3: coherence of the certificate block ---
    if signature == Enterprise && !header.has_certificate() {
        violations.push(RuleViolation::EnterpriseNeedsCertificate);
    }
    if signature != Enterprise && header.has_certificate() {
        violations.push(RuleViolation::CertificateNotExpected);
    }

    // --- permission ceiling (spec 12.4) ---
    let excess = header.permissions.0 & !Permissions::ceiling(format);
    if excess != 0 {
        violations.push(RuleViolation::PermissionAboveCeiling(excess));
    }

    // --- deny-list of `.ras` (spec 12.3) ---
    if format == Ras {
        if header.permissions.contains(Permissions::IPC_PUBLIC)
            || header.permissions.contains(Permissions::IPC_INTERNAL_PORTS)
            || !header.internal_ports.is_empty()
        {
            violations.push(RuleViolation::RasCannotUseIpc);
        }
        if header.has_flag(flags::DEFAULT_CAPABLE) {
            violations.push(RuleViolation::RasCannotBeDefault);
        }
    }

    // --- flags reserved to one format (spec 3.4) ---
    if header.has_flag(flags::EXPOSES_API) && format != Dsa {
        violations.push(RuleViolation::OnlyDsaExposesApi);
    }
    if header.has_flag(flags::REPLACES_COMPONENT) && format != Ss {
        violations.push(RuleViolation::OnlySsReplacesComponent);
    }
    if header.has_flag(flags::AUTO_PAUSE) && format != Gs {
        violations.push(RuleViolation::OnlyGsAutoPauses);
    }

    // --- internal coherence ---
    if !header.internal_ports.is_empty()
        && !header.permissions.contains(Permissions::IPC_INTERNAL_PORTS)
        && format != Ras
    {
        violations.push(RuleViolation::PortsWithoutPermission);
    }
    if header.sections.iter().any(|s| s.violates_wx()) {
        violations.push(RuleViolation::SectionViolatesWx);
    }
    if sections_share_page(header) {
        violations.push(RuleViolation::SectionsSharePage);
    }

    violations
}

/// Do two loadable sections with different rights overlap in the same
/// 4 KiB page?
///
/// Paging is the only protection mechanism the loader has, and its
/// granularity is the page. A `.text` and a `.data` neighbouring in the
/// same page would force a choice between a page both writable **and**
/// executable -- which W^X forbids -- or a page that would break one of
/// the two sections. The format therefore demands the producer align what
/// must be protected separately.
fn sections_share_page(header: &BinaryHeader) -> bool {
    let loadable: Vec<&Section> = header.loadable_sections().collect();

    for (i, a) in loadable.iter().enumerate() {
        if a.mem_size == 0 {
            continue;
        }
        let a_first = a.virt_addr / 4096;
        let a_last = (a.virt_addr + a.mem_size as u64 - 1) / 4096;

        for b in loadable.iter().skip(i + 1) {
            if b.mem_size == 0 || a.page_flags() == b.page_flags() {
                continue;
            }
            let b_first = b.virt_addr / 4096;
            let b_last = (b.virt_addr + b.mem_size as u64 - 1) / 4096;

            if a_first <= b_last && b_first <= a_last {
                return true;
            }
        }
    }
    false
}

/// Complete result of an inspection.
pub struct Inspection {
    pub header: BinaryHeader,
    pub violations: Vec<RuleViolation>,
}

impl Inspection {
    /// Can the binary be loaded?
    pub fn is_admissible(&self) -> bool {
        self.violations.is_empty()
    }
}

/// Complete parse: decoding then confrontation with the rules.
pub fn inspect(data: &[u8]) -> Result<Inspection, ParseError> {
    let header = parse(data)?;
    let violations = check_rules(&header);
    Ok(Inspection { header, violations })
}

// ============================================================
//  Diagnostics
// ============================================================

/// Logs a decoded header on the serial console.
pub fn dump(header: &BinaryHeader) {
    let id = &header.identity;

    crate::serial_println!(
        "[binfmt]   format       : {} ({}) - priorite {}",
        id.format.extension(),
        id.format.full_name(),
        id.format.priority()
    );
    crate::serial_println!(
        "[binfmt]   app_id       : {} - \"{}\"",
        id.app_id,
        id.display_name
    );
    crate::serial_println!(
        "[binfmt]   auteur       : {} - groupe \"{}\"",
        id.author,
        id.group
    );
    crate::serial_println!(
        "[binfmt]   version      : {} - ABI {}",
        id.version,
        header.abi_version
    );
    crate::serial_println!(
        "[binfmt]   signature    : {} - {}",
        id.signature.as_str(),
        id.signature.warning_level(id.format)
    );
    crate::serial_println!(
        "[binfmt]   memoire      : image {} o + bss {} o + pile {} o = {} o",
        header.image_size,
        header.bss_size,
        header.stack_size,
        header.total_memory()
    );
    crate::serial_println!(
        "[binfmt]   entree       : 0x{:x} (base 0x{:x})",
        header.entry_point,
        header.load_base
    );

    // Flags set.
    let mut line = String::new();
    for (name, set) in flags::names(header.flags) {
        if set {
            if !line.is_empty() {
                line.push_str(" ");
            }
            line.push_str(name);
        }
    }
    crate::serial_println!(
        "[binfmt]   drapeaux     : {}",
        if line.is_empty() { "aucun" } else { &line }
    );

    // Permissions declared.
    let mut line = String::new();
    for (name, set) in header.permissions.names() {
        if set {
            if !line.is_empty() {
                line.push_str(" ");
            }
            line.push_str(name);
        }
    }
    crate::serial_println!(
        "[binfmt]   permissions  : {} ({} declarees)",
        if line.is_empty() { "aucune" } else { &line },
        header.permissions.count()
    );

    if !header.internal_ports.is_empty() {
        crate::serial_println!(
            "[binfmt]   ports        : {:?} ({} declares)",
            header.internal_ports.as_slice(),
            header.internal_ports.len()
        );
    }

    crate::serial_println!(
        "[binfmt]   sections     : {} - charge utile {} o a l'offset 0x{:x}",
        header.sections.len(),
        header.payload_size,
        header.payload_offset
    );

    if !header.relocations.is_empty() {
        crate::serial_println!(
            "[binfmt]   relocations  : {} entree(s) - premiere a 0x{:x} ({})",
            header.relocations.len(),
            header.relocations[0].virt_addr,
            header.relocations[0].kind.as_str()
        );
    }
    if header.has_certificate() {
        crate::serial_println!(
            "[binfmt]   certificat   : bloc de {} o a l'offset 0x{:x}",
            header::CERT_SIZE,
            header.cert_offset
        );
    }

    for section in header.sections.iter() {
        crate::serial_println!(
            "[binfmt]     {:<8} {:<7} {}{}{} - 0x{:x} ({} o fichier / {} o memoire)",
            section.name_str(),
            section.kind.as_str(),
            if section.flags & header::section_flags::READ != 0 { "R" } else { "-" },
            if section.is_writable() { "W" } else { "-" },
            if section.is_executable() { "X" } else { "-" },
            section.virt_addr,
            section.file_size,
            section.mem_size
        );
    }
}

/// Parses a buffer and logs the verdict.
///
/// Returns true if the binary is admissible.
pub fn inspect_and_report(label: &str, data: &[u8]) -> bool {
    crate::serial_println!("[binfmt] --- {} ({} octets) ---", label, data.len());

    match inspect(data) {
        Ok(inspection) => {
            dump(&inspection.header);

            if inspection.is_admissible() {
                crate::serial_println!("[binfmt]   verdict      : ADMISSIBLE");
                true
            } else {
                crate::serial_println!(
                    "[binfmt]   verdict      : REFUSE - {} violation(s)",
                    inspection.violations.len()
                );
                for violation in inspection.violations.iter() {
                    crate::serial_println!("[binfmt]     * {}", violation.message());
                }
                false
            }
        }
        Err(error) => {
            crate::serial_println!(
                "[binfmt]   verdict      : REJETE - {} ({:?})",
                error.message(),
                error
            );
            false
        }
    }
}

// ============================================================
//  Self-test
// ============================================================

/// Exercises the whole of phase 3 on a set of binaries built in memory.
///
/// As long as no filesystem exists (phase 4), it is the only way to
/// confront the parser, the signature verifier and the loader with
/// realistic data.
///
/// Returns (passed, failed).
pub fn selftest() -> (usize, usize) {
    crate::serial_println!("[binfmt] autotest de la phase 3");
    crate::serial_println!(
        "[binfmt] header {} octets version {} - entree de section {} o - entree de relocation {} o",
        HEADER_SIZE,
        HEADER_VERSION,
        SECTION_ENTRY_SIZE,
        RELOC_ENTRY_SIZE
    );

    // --- 3.3.1: the hash primitive first ---
    // Everything else depends on it: a wrong digest would make every
    // signature check as confident as it is useless.
    let (mut passed, mut failed) = sha256::selftest();

    // --- 3.1: parsing ---
    for (label, data) in sample::valid_set() {
        if inspect_and_report(&label, &data) {
            passed += 1;
        } else {
            failed += 1;
            crate::serial_println!("[binfmt] ECHEC : \"{}\" aurait du etre admis", label);
        }
    }

    for (label, data) in sample::invalid_set() {
        if inspect_and_report(&label, &data) {
            failed += 1;
            crate::serial_println!("[binfmt] ECHEC : \"{}\" aurait du etre refuse", label);
        } else {
            passed += 1;
        }
    }

    // --- communication rules (spec 12.1) ---
    sample::communication_matrix();

    // --- 3.3 then 3.2 ---
    let (p, f) = signature_selftest();
    passed += p;
    failed += f;

    let (p, f) = loader_selftest();
    passed += p;
    failed += f;

    crate::serial_println!(
        "[binfmt] autotest termine : {} reussites, {} echecs",
        passed,
        failed
    );

    (passed, failed)
}

/// Compares an expected verdict with the verdict obtained.
fn expect_verdict(label: &str, data: &[u8], expected: signature::Verdict) -> bool {
    let Ok(header) = parse(data) else {
        crate::serial_println!("[sign] ECHEC : \"{}\" ne s'analyse pas", label);
        return false;
    };
    let verdict = signature::verify(&header, data);

    crate::serial_println!("[sign] --- {} ---", label);
    signature::dump(&header, data, verdict);

    if verdict == expected {
        true
    } else {
        crate::serial_println!(
            "[sign] ECHEC : \"{}\" - attendu {:?}, obtenu {:?}",
            label,
            expected,
            verdict
        );
        false
    }
}

/// 3.3.2 to 3.3.5 -- verification of the three signature levels.
fn signature_selftest() -> (usize, usize) {
    use signature::{Reason, Verdict};

    let mut passed = 0;
    let mut failed = 0;

    crate::serial_println!("[sign] verification des trois niveaux (spec 3.2)");

    // The ENTERPRISE root is laid down first: without it, no certificate
    // can be valid.
    sample::register_demo_authority();

    // --- 3.3.2: SYSTEM ---
    // The same binary, before and after registration in the kernel table.
    // Nothing else changes: it really is the anchor that decides.
    let dsa = sample::official_dsa();
    if expect_verdict(
        ".dsa SYSTEM non enregistre",
        &dsa,
        Verdict::Rejected(Reason::UnknownSystemBinary),
    ) {
        passed += 1;
    } else {
        failed += 1;
    }

    sample::register_as_official(&dsa);
    if expect_verdict(".dsa SYSTEM enregistre", &dsa, Verdict::System) {
        passed += 1;
    } else {
        failed += 1;
    }

    // One byte modified in the payload and the digest no longer matches.
    let mut altered = dsa.clone();
    let last = altered.len() - 1;
    altered[last] ^= 0xFF;
    if expect_verdict(
        ".dsa SYSTEM altere",
        &altered,
        Verdict::Rejected(Reason::PayloadTampered),
    ) {
        passed += 1;
    } else {
        failed += 1;
    }

    // --- 3.3.3: ENTERPRISE ---
    let gs = sample::certified_gs();
    if expect_verdict(".gs ENTERPRISE certifie", &gs, Verdict::Enterprise) {
        passed += 1;
    } else {
        failed += 1;
    }

    let mut spec = sample::BuildSpec::minimal(
        header::BinaryFormat::Gs,
        header::SignatureType::Enterprise,
    );
    spec.certificate = true;
    spec.unknown_authority = true;
    let rogue = sample::build(&spec);
    if expect_verdict(
        ".gs ENTERPRISE d'une autorite inconnue",
        &rogue,
        Verdict::Rejected(Reason::UntrustedAuthority),
    ) {
        passed += 1;
    } else {
        failed += 1;
    }

    // Valid certificate, but copied onto another binary: the binding
    // bears on the digest, it does not follow.
    let mut transplanted = sample::certified_gs();
    let cert_start = parse(&gs).map(|h| h.cert_offset as usize).unwrap_or(0);
    if cert_start > 0 && cert_start + header::CERT_SIZE <= transplanted.len() {
        // We change a declared permission: the certificate stays
        // syntactically valid, but no longer matches the binary.
        let current = read_u64(&transplanted, offsets::PERMISSIONS);
        let modified = current | identity::Permissions::CLIPBOARD;
        transplanted[offsets::PERMISSIONS..offsets::PERMISSIONS + 8]
            .copy_from_slice(&modified.to_le_bytes());

        if expect_verdict(
            ".gs ENTERPRISE aux permissions modifiees",
            &transplanted,
            Verdict::Rejected(Reason::BindingMismatch),
        ) {
            passed += 1;
        } else {
            failed += 1;
        }
    }

    // --- 3.3.4: SELF ---
    let cus = sample::executable_cus();
    if expect_verdict(".cus auto-signe", &cus, Verdict::SelfSigned) {
        passed += 1;
    } else {
        failed += 1;
    }

    // --- 3.3.5: forbidden combinations ---
    let ras = sample::executable_ras();
    if expect_verdict(".ras non signe", &ras, Verdict::Unsigned) {
        passed += 1;
    } else {
        failed += 1;
    }

    let (system_anchors, roots) = signature::store_size();
    crate::serial_println!(
        "[sign] magasin : {} empreinte(s) SYSTEM, {} racine(s) ENTERPRISE",
        system_anchors,
        roots
    );

    (passed, failed)
}

/// 3.2 -- loading, relocation, protection and transfer of control.
fn loader_selftest() -> (usize, usize) {
    let mut passed = 0;
    let mut failed = 0;

    crate::serial_println!("[load] autotest du chargeur (spec 3.4)");

    // --- expected refusals: nothing must be installed ---
    let refusals: [(&str, alloc::vec::Vec<u8>); 3] = [
        (
            "binaire tronque",
            sample::executable_cus()[..64].to_vec(),
        ),
        (
            ".dsa auto-signe",
            sample::build(&sample::BuildSpec::minimal(
                header::BinaryFormat::Dsa,
                header::SignatureType::SelfSigned,
            )),
        ),
        ("charge utile alteree", {
            let mut data = sample::executable_cus();
            let last = data.len() - 1;
            data[last] ^= 0xFF;
            data
        }),
    ];

    for (label, data) in refusals.iter() {
        match loader::load(data) {
            Ok(image) => {
                crate::serial_println!("[load] ECHEC : \"{}\" aurait du etre refuse", label);
                unsafe { loader::unload(image) };
                failed += 1;
            }
            Err(error) => {
                crate::serial_println!(
                    "[load]   \"{}\" -> {} ({})",
                    label,
                    error.message(),
                    if error.is_rejection() { "rejete" } else { "refuse" }
                );
                passed += 1;
            }
        }
    }

    // --- load then unload: the frames must come back ---
    // The certified `.gs` contains only `nop`, it will never run. It
    // serves to check that the loader gives back exactly what it took: a
    // discrepancy here would be a physical memory leak on every
    // application closed.
    let (_, _, free_before) = crate::frame::stats();
    match loader::load(&sample::certified_gs()) {
        Ok(image) => {
            crate::serial_println!("[load] --- .gs certifie (chargement puis liberation) ---");
            loader::dump(&image);

            if loader::verify_protection(&image) {
                passed += 1;
            } else {
                failed += 1;
            }

            // 3.4.1: the priority comes from the format, nothing else.
            if image.priority() == 3 && image.binary_type == crate::process::BinaryType::Gs {
                passed += 1;
            } else {
                failed += 1;
                crate::serial_println!(
                    "[load] ECHEC : priorite {} pour un .gs (attendu 3)",
                    image.priority()
                );
            }

            unsafe { loader::unload(image) };
        }
        Err(error) => {
            crate::serial_println!("[load] ECHEC : .gs certifie refuse - {}", error.message());
            failed += 1;
        }
    }

    let (_, _, free_after) = crate::frame::stats();
    if free_after == free_before {
        crate::serial_println!(
            "[load]   liberation verifiee : {} frames libres avant comme apres",
            free_after
        );
        passed += 1;
    } else {
        crate::serial_println!(
            "[load] ECHEC : {} frames libres avant, {} apres - fuite de {} frames",
            free_before,
            free_after,
            free_before as i64 - free_after as i64
        );
        failed += 1;
    }

    // --- 3.2.2: relocation really applied ---
    match loader::load(&sample::executable_cus()) {
        Ok(image) => {
            crate::serial_println!("[load] --- .cus executable ---");
            loader::dump(&image);

            if !loader::verify_protection(&image) {
                failed += 1;
            } else {
                passed += 1;
            }

            // The relocated word lives at the start of `.data`, one page
            // after the start of the image. It held the link base, it
            // must now hold the effective base.
            let pointer_addr = image.base + 4096;
            match unsafe { loader::read_user_u64(pointer_addr) } {
                Some(value) if value == image.base => {
                    crate::serial_println!(
                        "[load]   relocation verifiee : 0x{:x} contient 0x{:x} (etait 0x{:x})",
                        pointer_addr,
                        value,
                        image.link_base
                    );
                    passed += 1;
                }
                Some(value) => {
                    crate::serial_println!(
                        "[load] ECHEC : 0x{:x} contient 0x{:x}, attendu 0x{:x}",
                        pointer_addr,
                        value,
                        image.base
                    );
                    failed += 1;
                }
                None => {
                    crate::serial_println!("[load] ECHEC : mot relocalise non lisible");
                    failed += 1;
                }
            }

            if image.relocations_applied == 1 {
                passed += 1;
            } else {
                failed += 1;
                crate::serial_println!(
                    "[load] ECHEC : {} relocation(s) appliquee(s), attendu 1",
                    image.relocations_applied
                );
            }

            // 3.2.3: transfer of control. The task will run when the
            // scheduler starts.
            let pid = loader::spawn(&image);
            crate::serial_println!(
                "[load]   tache {} creee - le code s'executera au demarrage du scheduler",
                pid
            );
            passed += 1;

            // The image stays loaded: the task needs it. It will be freed
            // with SYS_PROC_WAIT (phase 5.3.3).
            loader::keep(pid, image);
        }
        Err(error) => {
            crate::serial_println!(
                "[load] ECHEC : .cus executable refuse - {}",
                error.message()
            );
            failed += 1;
        }
    }

    // --- 3.2.4.3: foreground/background switch of a .gs ---
    match loader::load(&sample::executable_gs()) {
        Ok(image) => {
            crate::serial_println!("[load] --- .gs executable ---");
            loader::dump(&image);

            let pid = loader::spawn(&image);

            // In the foreground: 20 ticks. In the background: 2, that is
            // the 2 % of CPU announced by spec 3.4 against the 40 ticks
            // of a `.dsa`. The switch touches neither priority nor core.
            let foreground = crate::scheduler::task_profile(pid);
            crate::scheduler::set_foreground(pid, false);
            let background = crate::scheduler::task_profile(pid);
            crate::scheduler::set_foreground(pid, true);
            let restored = crate::scheduler::task_profile(pid);

            match (foreground, background, restored) {
                (Some(f), Some(b), Some(r)) => {
                    crate::serial_println!(
                        "[load]   .gs tranche : {} tics au premier plan -> {} en arriere-plan -> {} au retour",
                        f.3,
                        b.3,
                        r.3
                    );
                    crate::serial_println!(
                        "[load]   priorite {} et coeur {} inchanges par la bascule",
                        b.1,
                        b.2
                    );
                    if f.3 == 20 && b.3 == 2 && r.3 == 20 && f.1 == b.1 && f.2 == b.2 {
                        passed += 1;
                    } else {
                        crate::serial_println!(
                            "[load] ECHEC : tranches {} / {} / {} (attendu 20 / 2 / 20)",
                            f.3,
                            b.3,
                            r.3
                        );
                        failed += 1;
                    }
                }
                _ => {
                    crate::serial_println!("[load] ECHEC : profil de tache {} introuvable", pid);
                    failed += 1;
                }
            }

            loader::keep(pid, image);
        }
        Err(error) => {
            crate::serial_println!("[load] ECHEC : .gs executable refuse - {}", error.message());
            failed += 1;
        }
    }

    // --- 3.2.4.4: the same program as a .ras ---
    match loader::load(&sample::executable_ras()) {
        Ok(image) => {
            crate::serial_println!("[load] --- .ras executable ---");
            loader::dump(&image);
            let pid = loader::spawn(&image);
            crate::serial_println!(
                "[load]   tache {} creee - avertissement rouge attendu au lancement",
                pid
            );
            loader::keep(pid, image);
            passed += 1;
        }
        Err(error) => {
            crate::serial_println!("[load] ECHEC : .ras executable refuse - {}", error.message());
            failed += 1;
        }
    }

    let (loaded, refused, rejected) = loader::stats();
    let (resident, pages) = loader::resident_stats();
    crate::serial_println!(
        "[load] bilan : {} images chargees, {} refusees, {} rejetees",
        loaded,
        refused,
        rejected
    );
    crate::serial_println!(
        "[load] residentes : {} images occupant {} pages utilisateur",
        resident,
        pages
    );

    (passed, failed)
}

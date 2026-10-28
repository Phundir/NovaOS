//! 3.1.1 and 3.1.2 -- Header common to the five binary formats (spec 3).
//!
//! Layout over 256 bytes, little-endian:
//!
//! ```text
//!   0x00   4   magic number (spec 3.3)
//!   0x04   2   header format version
//!   0x06   2   header size
//!   0x08   2   system ABI version
//!   0x0A   1   binary type (redundant with the magic, checked)
//!   0x0B   1   signature type
//!   0x0C   4   flags
//!   0x10   8   entry point (virtual address)
//!   0x18   8   wanted load base
//!   0x20   8   size of the image in memory
//!   0x28   8   size of the uninitialised area
//!   0x30   8   stack size requested
//!   0x38   4   number of sections
//!   0x3C   4   offset of the section table
//!   0x40  32   app_id
//!   0x60  32   author
//!   0x80  32   group
//!   0xA0  32   display name
//!   0xC0   8   version (major, minor, patch, build)
//!   0xC8   8   declared permissions (allow-list, spec 12.3)
//!   0xD0   2   number of internal ports (spec 12.2)
//!   0xD2   2   offset of the port table
//!   0xD4   4   offset of the payload
//!   0xD8   4   size of the payload
//!   0xDC   4   offset of the certificate block (0 = absent, spec 3.2)
//!   0xE0  32   SHA-256 digest of the payload
//!   0x100      end
//! ```
//!
//! No `repr(C)` structure is overlaid on the file: the fields are
//! extracted one by one, which avoids any alignment assumption and makes
//! the format independent of the compiler.

use crate::process;

/// Fixed size of the header. Any other value makes the binary be rejected.
pub const HEADER_SIZE: usize = 256;

/// Header format version produced and accepted by this kernel.
pub const HEADER_VERSION: u16 = 1;

/// System API version the binary expects (spec 6).
pub const ABI_VERSION: u16 = 1;

/// Length of the fixed-size text fields, padded with zeros.
pub const NAME_LEN: usize = 32;

// ============================================================
//  3.1.2 -- Magic numbers (spec 3.3)
// ============================================================

pub const MAGIC_DSA: [u8; 4] = [0x44, 0x53, 0x41, 0x00]; // "DSA\0"
pub const MAGIC_SS: [u8; 4] = [0x53, 0x53, 0x00, 0x00]; // "SS\0\0"
pub const MAGIC_GS: [u8; 4] = [0x47, 0x53, 0x00, 0x00]; // "GS\0\0"
pub const MAGIC_CUS: [u8; 4] = [0x43, 0x55, 0x53, 0x00]; // "CUS\0"
pub const MAGIC_RAS: [u8; 4] = [0x52, 0x41, 0x53, 0x00]; // "RAS\0"

/// The five formats of the hierarchy (spec 3.1).
///
/// Distinct from `process::BinaryType`, which additionally carries the
/// internal `Kernel` and `Idle` categories: a file can never declare
/// itself so. The correspondence is made by `scheduler_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum BinaryFormat {
    /// Direct System Access -- official system apps.
    Dsa = 1,
    /// System Software -- explorer, task manager, compositor.
    Ss = 2,
    /// Game Software -- automatic pause mode in the background.
    Gs = 3,
    /// Common Utility Software -- all other applications.
    Cus = 4,
    /// Random Access Software -- total access, not recommended.
    Ras = 5,
}

impl BinaryFormat {
    /// Recognises a format from the first four bytes.
    pub fn from_magic(magic: &[u8]) -> Option<Self> {
        if magic.len() < 4 {
            return None;
        }
        match [magic[0], magic[1], magic[2], magic[3]] {
            MAGIC_DSA => Some(BinaryFormat::Dsa),
            MAGIC_SS => Some(BinaryFormat::Ss),
            MAGIC_GS => Some(BinaryFormat::Gs),
            MAGIC_CUS => Some(BinaryFormat::Cus),
            MAGIC_RAS => Some(BinaryFormat::Ras),
            _ => None,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(BinaryFormat::Dsa),
            2 => Some(BinaryFormat::Ss),
            3 => Some(BinaryFormat::Gs),
            4 => Some(BinaryFormat::Cus),
            5 => Some(BinaryFormat::Ras),
            _ => None,
        }
    }

    pub fn magic(&self) -> [u8; 4] {
        match self {
            BinaryFormat::Dsa => MAGIC_DSA,
            BinaryFormat::Ss => MAGIC_SS,
            BinaryFormat::Gs => MAGIC_GS,
            BinaryFormat::Cus => MAGIC_CUS,
            BinaryFormat::Ras => MAGIC_RAS,
        }
    }

    pub fn extension(&self) -> &'static str {
        match self {
            BinaryFormat::Dsa => ".dsa",
            BinaryFormat::Ss => ".ss",
            BinaryFormat::Gs => ".gs",
            BinaryFormat::Cus => ".cus",
            BinaryFormat::Ras => ".ras",
        }
    }

    pub fn full_name(&self) -> &'static str {
        match self {
            BinaryFormat::Dsa => "Direct System Access",
            BinaryFormat::Ss => "System Software",
            BinaryFormat::Gs => "Game Software",
            BinaryFormat::Cus => "Common Utility Software",
            BinaryFormat::Ras => "Random Access Software",
        }
    }

    /// Fixed scheduling priority (spec 3.1 and 7.3).
    ///
    /// Not modifiable by the user: it is a settled design decision, not a
    /// setting.
    pub fn priority(&self) -> u8 {
        *self as u8
    }

    /// 3.4.1 -- Correspondence to the type the scheduler handles.
    pub fn scheduler_type(&self) -> process::BinaryType {
        match self {
            BinaryFormat::Dsa => process::BinaryType::Dsa,
            BinaryFormat::Ss => process::BinaryType::Ss,
            BinaryFormat::Gs => process::BinaryType::Gs,
            BinaryFormat::Cus => process::BinaryType::Cus,
            BinaryFormat::Ras => process::BinaryType::Ras,
        }
    }

    /// May the format be set as the default application? Forbidden to
    /// `.ras` (spec 3.4).
    pub fn can_be_default(&self) -> bool {
        !matches!(self, BinaryFormat::Ras)
    }

    /// Does installation demand a blocking red warning (spec 10.4)?
    pub fn requires_red_warning(&self) -> bool {
        matches!(self, BinaryFormat::Ras)
    }
}

// ============================================================
//  Signatures (spec 3.2)
// ============================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SignatureType {
    /// No signature -- the only case accepted for `.ras`.
    None = 0,
    /// Self-signed by an independent developer.
    SelfSigned = 1,
    /// Third-party certificate carrying legal information.
    Enterprise = 2,
    /// Official application, digest verified by the kernel.
    System = 3,
}

impl SignatureType {
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(SignatureType::None),
            1 => Some(SignatureType::SelfSigned),
            2 => Some(SignatureType::Enterprise),
            3 => Some(SignatureType::System),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            SignatureType::None => "AUCUNE",
            SignatureType::SelfSigned => "SELF",
            SignatureType::Enterprise => "ENTERPRISE",
            SignatureType::System => "SYSTEM",
        }
    }

    /// Warning level shown at install time (spec 10.4).
    pub fn warning_level(&self, format: BinaryFormat) -> &'static str {
        match (self, format) {
            (SignatureType::System, _) => "badge officiel - aucun avertissement",
            (SignatureType::Enterprise, BinaryFormat::Cus)
            | (SignatureType::Enterprise, BinaryFormat::Gs) => {
                "badge verifie - aucun avertissement"
            }
            (SignatureType::Enterprise, _) => "avertissement - prompt admin obligatoire",
            (SignatureType::SelfSigned, _) => "non verifie - avertissement jaune",
            (SignatureType::None, _) => "dangereux - avertissement rouge bloquant",
        }
    }
}

// ============================================================
//  Flags
// ============================================================

pub mod flags {
    /// The application opens a window and talks to the compositor.
    pub const GUI: u32 = 1 << 0;
    /// May keep running in the background.
    pub const BACKGROUND: u32 = 1 << 1;
    /// Automatic foreground/background switch -- proper to `.gs` (spec 3.4).
    pub const AUTO_PAUSE: u32 = 1 << 2;
    /// Exposes a public API through IPC -- reserved to `.dsa` (spec 3.4).
    pub const EXPOSES_API: u32 = 1 << 3;
    /// Replaces a system component -- reserved to `.ss` (spec 3.4).
    pub const REPLACES_COMPONENT: u32 = 1 << 4;
    /// May be named as the default application.
    pub const DEFAULT_CAPABLE: u32 = 1 << 5;
    /// Requires an ADMIN elevation at launch.
    pub const REQUIRES_ADMIN: u32 = 1 << 6;
    /// Service with no interface, started by init.dsa (spec 2.1).
    pub const SERVICE: u32 = 1 << 7;
    /// The application accesses the network (spec 6.6, `SYS_NET_*` domain).
    ///
    /// Format extension decided in phase 5.6.4. 6.6 makes network access
    /// conditional on a header declaration, without saying where it lives:
    /// this bit is that declaration. It did not exist until now, and its
    /// absence made the rule unenforceable.
    ///
    /// Adding a bit breaks nothing: an earlier binary does not set it, and
    /// is therefore refused the network domain -- which is exactly the
    /// expected behaviour of a program that never declared wanting to
    /// reach it.
    pub const NETWORK: u32 = 1 << 8;
    /// The payload is an ELF64 x86-64 image linked against the Nova C
    /// library, not a Nova image (spec 3.5, extension of 23 September
    /// 2026). Such a binary declares ABI version 2 and is started by
    /// `native::spawn`, which maps the ELF and the library's dynamic
    /// linker; the header, its digest and its signature cover it as they
    /// cover any binary.
    pub const LINKED_IMAGE: u32 = 1 << 9;

    pub const ALL: u32 = GUI
        | BACKGROUND
        | AUTO_PAUSE
        | EXPOSES_API
        | REPLACES_COMPONENT
        | DEFAULT_CAPABLE
        | REQUIRES_ADMIN
        | SERVICE
        | NETWORK
        | LINKED_IMAGE;

    pub fn names(value: u32) -> [(&'static str, bool); 10] {
        [
            ("GUI", value & GUI != 0),
            ("BACKGROUND", value & BACKGROUND != 0),
            ("AUTO_PAUSE", value & AUTO_PAUSE != 0),
            ("EXPOSES_API", value & EXPOSES_API != 0),
            ("REPLACES_COMPONENT", value & REPLACES_COMPONENT != 0),
            ("DEFAULT_CAPABLE", value & DEFAULT_CAPABLE != 0),
            ("REQUIRES_ADMIN", value & REQUIRES_ADMIN != 0),
            ("SERVICE", value & SERVICE != 0),
            ("IMAGE_LIEE", value & LINKED_IMAGE != 0),
            ("NETWORK", value & NETWORK != 0),
        ]
    }
}

// ============================================================
//  Sections
// ============================================================

/// Size of an entry of the section table.
pub const SECTION_ENTRY_SIZE: usize = 32;

/// Nature of a section -- determines the rights of the page carrying it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SectionKind {
    Null = 0,
    Code = 1,
    ReadOnly = 2,
    Data = 3,
    /// Uninitialised: occupies memory without occupying the file.
    Bss = 4,
    /// 3.2.2 -- Relocation table. Symmetric of `Bss`: occupies the file
    /// without occupying memory, and is never mapped.
    Reloc = 5,
    /// ENTERPRISE certificate block (spec 3.2). Like `Reloc`, present in
    /// the file only.
    Certificate = 6,
}

impl SectionKind {
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(SectionKind::Null),
            1 => Some(SectionKind::Code),
            2 => Some(SectionKind::ReadOnly),
            3 => Some(SectionKind::Data),
            4 => Some(SectionKind::Bss),
            5 => Some(SectionKind::Reloc),
            6 => Some(SectionKind::Certificate),
            _ => None,
        }
    }

    /// Does the section occupy memory once the image is loaded?
    ///
    /// The purely documentary sections -- relocations, certificate --
    /// live in the file and disappear at load time: they are therefore
    /// not required to carry a virtual address in the image.
    pub fn is_loadable(&self) -> bool {
        matches!(
            self,
            SectionKind::Code | SectionKind::ReadOnly | SectionKind::Data | SectionKind::Bss
        )
    }

    /// Does the content come from the file, or is it produced as zeros?
    pub fn has_file_content(&self) -> bool {
        !matches!(self, SectionKind::Null | SectionKind::Bss)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            SectionKind::Null => "null",
            SectionKind::Code => "code",
            SectionKind::ReadOnly => "rodata",
            SectionKind::Data => "data",
            SectionKind::Bss => "bss",
            SectionKind::Reloc => "reloc",
            SectionKind::Certificate => "cert",
        }
    }
}

pub mod section_flags {
    pub const READ: u8 = 1 << 0;
    pub const WRITE: u8 = 1 << 1;
    pub const EXEC: u8 = 1 << 2;
}

/// One entry of the section table, decoded.
///
/// ```text
///   0x00   8   name
///   0x08   1   nature
///   0x09   1   rights
///   0x0A   2   alignment
///   0x0C   4   offset in the file
///   0x10   4   size in the file
///   0x14   4   size in memory
///   0x18   8   virtual address
/// ```
#[derive(Debug, Clone, Copy)]
pub struct Section {
    pub name: [u8; 8],
    pub kind: SectionKind,
    pub flags: u8,
    pub align: u16,
    pub file_offset: u32,
    pub file_size: u32,
    pub mem_size: u32,
    pub virt_addr: u64,
}

impl Section {
    /// Name of the section, without the padding zeros.
    pub fn name_str(&self) -> &str {
        let len = self.name.iter().position(|&b| b == 0).unwrap_or(8);
        core::str::from_utf8(&self.name[..len]).unwrap_or("?")
    }

    pub fn is_writable(&self) -> bool {
        self.flags & section_flags::WRITE != 0
    }

    pub fn is_executable(&self) -> bool {
        self.flags & section_flags::EXEC != 0
    }

    /// A section that is both writable and executable violates the W^X
    /// principle: the loader of 3.2 will refuse it.
    pub fn violates_wx(&self) -> bool {
        self.is_writable() && self.is_executable()
    }

    /// Rights reduced to what paging can express: (W, X).
    ///
    /// The read bit is not there -- a present page is always readable on
    /// x86-64, there is no executable non-readable page.
    pub fn page_flags(&self) -> (bool, bool) {
        (self.is_writable(), self.is_executable())
    }

    /// First and last 4 KiB page covered in memory. Returns `None` for a
    /// section that occupies no memory.
    pub fn page_span(&self) -> Option<(u64, u64)> {
        if self.mem_size == 0 {
            return None;
        }
        Some((
            self.virt_addr / 4096,
            (self.virt_addr + self.mem_size as u64 - 1) / 4096,
        ))
    }
}

// ============================================================
//  3.2.2 -- Relocations
// ============================================================

/// Size of a relocation entry.
pub const RELOC_ENTRY_SIZE: usize = 16;

/// Nature of a relocation.
///
/// A single type for now, and it is enough: the code produced by the SDK
/// is position-independent (RIP-relative addressing), only the **data**
/// carries absolute addresses -- function pointers in a table, strings
/// referenced by address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RelocKind {
    /// Adds the load offset to a 64-bit word.
    Abs64 = 1,
}

impl RelocKind {
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(RelocKind::Abs64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            RelocKind::Abs64 => "ABS64",
        }
    }
}

/// One relocation entry, decoded.
///
/// ```text
///   0x00   8   virtual address of the word to fix (link base)
///   0x08   1   nature
///   0x09   3   reserved
///   0x0C   4   addend
/// ```
///
/// The address is expressed in the frame of the declared `load_base`: it
/// is the loader that transposes it to the effective base.
#[derive(Debug, Clone, Copy)]
pub struct Reloc {
    pub virt_addr: u64,
    pub kind: RelocKind,
    pub addend: u32,
}

// ============================================================
//  ENTERPRISE certificate block (spec 3.2)
// ============================================================

/// Fixed size of the certificate block.
pub const CERT_SIZE: usize = 256;

/// Signature of the block: "NCRT".
pub const CERT_MAGIC: [u8; 4] = [0x4E, 0x43, 0x52, 0x54];

/// Version of the certificate format.
pub const CERT_VERSION: u16 = 1;

/// Offsets of the fields of the certificate block.
///
/// ```text
///   0x00   4   magic "NCRT"
///   0x04   2   format version
///   0x06   2   signature type covered
///   0x08  32   issuing authority (readable name)
///   0x28  32   certified legal entity
///   0x48  32   legal identifier (company register, country)
///   0x68   8   start of validity (days since 2000-01-01)
///   0x70   8   end of validity
///   0x78  32   digest of the authority key -- root of trust
///   0x98  32   binding to the binary: SHA-256(authority key || digest)
///   0xB8  72   reserved
///   0x100      end
/// ```
pub mod cert_offsets {
    pub const MAGIC: usize = 0x00;
    pub const VERSION: usize = 0x04;
    pub const SIGNATURE_TYPE: usize = 0x06;
    pub const AUTHORITY: usize = 0x08;
    pub const SUBJECT: usize = 0x28;
    pub const LEGAL_ID: usize = 0x48;
    pub const VALID_FROM: usize = 0x68;
    pub const VALID_UNTIL: usize = 0x70;
    pub const AUTHORITY_KEY: usize = 0x78;
    pub const BINDING: usize = 0x98;
}

// ============================================================
//  Low-level reading
// ============================================================

/// Extracts a little-endian u16, or 0 if the slice is too short.
pub(crate) fn read_u16(data: &[u8], offset: usize) -> u16 {
    if offset + 2 > data.len() {
        return 0;
    }
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

pub(crate) fn read_u32(data: &[u8], offset: usize) -> u32 {
    if offset + 4 > data.len() {
        return 0;
    }
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

pub(crate) fn read_u64(data: &[u8], offset: usize) -> u64 {
    if offset + 8 > data.len() {
        return 0;
    }
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&data[offset..offset + 8]);
    u64::from_le_bytes(bytes)
}

// --- field offsets ---
pub(crate) mod offsets {
    pub const MAGIC: usize = 0x00;
    pub const HEADER_VERSION: usize = 0x04;
    pub const HEADER_SIZE: usize = 0x06;
    pub const ABI_VERSION: usize = 0x08;
    pub const BINARY_TYPE: usize = 0x0A;
    pub const SIGNATURE_TYPE: usize = 0x0B;
    pub const FLAGS: usize = 0x0C;
    pub const ENTRY_POINT: usize = 0x10;
    pub const LOAD_BASE: usize = 0x18;
    pub const IMAGE_SIZE: usize = 0x20;
    pub const BSS_SIZE: usize = 0x28;
    pub const STACK_SIZE: usize = 0x30;
    pub const SECTION_COUNT: usize = 0x38;
    pub const SECTION_TABLE: usize = 0x3C;
    pub const APP_ID: usize = 0x40;
    pub const AUTHOR: usize = 0x60;
    pub const GROUP: usize = 0x80;
    pub const DISPLAY_NAME: usize = 0xA0;
    pub const VERSION_MAJOR: usize = 0xC0;
    pub const VERSION_MINOR: usize = 0xC2;
    pub const VERSION_PATCH: usize = 0xC4;
    pub const VERSION_BUILD: usize = 0xC6;
    pub const PERMISSIONS: usize = 0xC8;
    pub const PORT_COUNT: usize = 0xD0;
    pub const PORT_TABLE: usize = 0xD2;
    pub const PAYLOAD_OFFSET: usize = 0xD4;
    pub const PAYLOAD_SIZE: usize = 0xD8;
    pub const CERT_OFFSET: usize = 0xDC;
    pub const PAYLOAD_HASH: usize = 0xE0;
}

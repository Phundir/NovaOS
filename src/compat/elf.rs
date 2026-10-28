//! 10.4.1 -- ELF64 parsing.
//!
//! ## Field by field, never by transmutation
//!
//! An ELF comes from a file, so nothing guarantees its alignment and
//! nothing guarantees its contents. Every field is decoded with
//! `from_le_bytes` at an explicit offset -- the rule the repository has
//! applied to firmware tables, filesystem structures and package headers
//! since phase 1, and for the same reason: casting a pointer to a struct
//! over a byte slice is undefined behaviour the first time a file is one
//! byte short.
//!
//! ## What is decoded
//!
//! ```text
//!   e_ident   16  magic, class, data, version, ABI
//!   e_type     2  ET_EXEC, ET_DYN, ET_REL, ET_CORE
//!   e_machine  2  EM_X86_64 = 62
//!   e_entry    8  virtual address of the entry point
//!   e_phoff    8  offset of the program header table
//!   e_shoff    8  offset of the section header table
//!   e_phentsize/e_phnum, e_shentsize/e_shnum
//! ```
//!
//! Program headers give what to load; section headers give what to
//! **refuse**. A `.rela.dyn` full of absolute relocations is what tells
//! the difference between a binary the subsystem can run and one it
//! cannot, and it is only visible in the sections.
//!
//! ## Every count is bounded before it is trusted
//!
//! `e_phnum` is a 16-bit field read from the file: it can say 65 535, and
//! multiplying it by an entry size read from the same file is how a
//! parser ends up indexing far outside its buffer. Both are checked
//! against the actual length before a single entry is read.

use alloc::string::String;
use alloc::vec::Vec;

/// The four bytes every ELF starts with.
pub const MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];

/// 64-bit class.
pub const CLASS_64: u8 = 2;
/// Little-endian encoding.
pub const DATA_LSB: u8 = 1;
/// The only machine this kernel runs.
pub const EM_X86_64: u16 = 62;

/// Size of the ELF64 header, and of one program header entry.
pub const EHDR_SIZE: usize = 64;
pub const PHDR_SIZE: usize = 56;
pub const SHDR_SIZE: usize = 64;

/// Ceiling on the number of entries accepted in either table.
///
/// A real binary has a handful of program headers and a few dozen
/// sections. The bound is what stops a corrupt `e_shnum` from making the
/// parser allocate megabytes before it discovers the file is short.
pub const MAX_ENTRIES: usize = 256;

/// Object file type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectType {
    /// Relocatable object -- a `.o`. Needs a linker, not a loader.
    Relocatable,
    /// Fixed-address executable.
    Executable,
    /// Shared object, which is also what a position-independent
    /// executable is.
    Shared,
    /// Core dump.
    Core,
    Other(u16),
}

impl ObjectType {
    fn from_code(code: u16) -> Self {
        match code {
            1 => ObjectType::Relocatable,
            2 => ObjectType::Executable,
            3 => ObjectType::Shared,
            4 => ObjectType::Core,
            other => ObjectType::Other(other),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ObjectType::Relocatable => "objet relogeable",
            ObjectType::Executable => "executable",
            ObjectType::Shared => "objet partage",
            ObjectType::Core => "image memoire",
            ObjectType::Other(_) => "type inconnu",
        }
    }
}

/// Kinds of program header this parser names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    /// Loadable segment.
    Load,
    /// Dynamic linking information -- the segment whose presence means
    /// the binary needs an interpreter.
    Dynamic,
    /// Path of the interpreter.
    Interp,
    /// Notes.
    Note,
    /// Thread-local storage.
    Tls,
    /// Stack permissions.
    GnuStack,
    Other(u32),
}

impl SegmentKind {
    fn from_code(code: u32) -> Self {
        match code {
            1 => SegmentKind::Load,
            2 => SegmentKind::Dynamic,
            3 => SegmentKind::Interp,
            4 => SegmentKind::Note,
            7 => SegmentKind::Tls,
            0x6474_E551 => SegmentKind::GnuStack,
            other => SegmentKind::Other(other),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            SegmentKind::Load => "LOAD",
            SegmentKind::Dynamic => "DYNAMIC",
            SegmentKind::Interp => "INTERP",
            SegmentKind::Note => "NOTE",
            SegmentKind::Tls => "TLS",
            SegmentKind::GnuStack => "GNU_STACK",
            SegmentKind::Other(_) => "AUTRE",
        }
    }
}

/// Segment permission flags.
pub mod flags {
    pub const EXEC: u32 = 1;
    pub const WRITE: u32 = 2;
    pub const READ: u32 = 4;
}

/// One program header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub kind: SegmentKind,
    pub flags: u32,
    pub file_offset: u64,
    pub virt_addr: u64,
    pub file_size: u64,
    pub mem_size: u64,
    pub align: u64,
}

impl Segment {
    pub fn readable(&self) -> bool {
        self.flags & flags::READ != 0
    }
    pub fn writable(&self) -> bool {
        self.flags & flags::WRITE != 0
    }
    pub fn executable(&self) -> bool {
        self.flags & flags::EXEC != 0
    }

    /// Permissions written the way `ls` writes them.
    pub fn permissions(&self) -> [u8; 3] {
        [
            if self.readable() { b'r' } else { b'-' },
            if self.writable() { b'w' } else { b'-' },
            if self.executable() { b'x' } else { b'-' },
        ]
    }
}

/// One section header, reduced to what the decision needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionHeader {
    pub name: String,
    pub kind: u32,
    pub flags: u64,
    pub addr: u64,
    pub offset: u64,
    pub size: u64,
}

/// A parsed ELF64 file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Elf {
    pub object_type: ObjectType,
    pub machine: u16,
    pub entry: u64,
    pub segments: Vec<Segment>,
    pub sections: Vec<SectionHeader>,
}

/// Why a file was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElfError {
    TooShort,
    NotElf,
    NotSixtyFour,
    NotLittleEndian,
    WrongMachine(u16),
    TableOutOfRange,
    /// The entry size in the header does not match the format.
    BadEntrySize,
    /// The file claims more entries than this parser accepts.
    TooManyEntries(usize),
    /// A segment names bytes the file does not contain.
    SegmentOutOfRange,
    /// A segment whose memory extent is smaller than its file extent --
    /// meaningless, and the shape in which a copy overruns.
    ShrinkingSegment,
}

impl ElfError {
    pub fn message(&self) -> String {
        match self {
            ElfError::TooShort => String::from("fichier trop court pour un en-tete ELF"),
            ElfError::NotElf => String::from("nombre magique ELF absent"),
            ElfError::NotSixtyFour => String::from("ELF 32 bits : seul le 64 bits est gere"),
            ElfError::NotLittleEndian => String::from("ELF gros-boutiste : x86_64 est petit-boutiste"),
            ElfError::WrongMachine(code) => {
                alloc::format!("machine {} : seul x86_64 (62) est gere", code)
            }
            ElfError::TableOutOfRange => String::from("table d'en-tetes hors du fichier"),
            ElfError::BadEntrySize => String::from("taille d'entree non conforme au format"),
            ElfError::TooManyEntries(count) => {
                alloc::format!("{} entrees : au-dela de la limite du parseur", count)
            }
            ElfError::SegmentOutOfRange => String::from("segment hors du fichier"),
            ElfError::ShrinkingSegment => {
                String::from("segment dont l'etendue memoire est plus petite que l'etendue fichier")
            }
        }
    }
}

fn u16_at(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn u64_at(data: &[u8], offset: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&data[offset..offset + 8]);
    u64::from_le_bytes(bytes)
}

/// Is this byte slice an ELF at all?
///
/// Separate from `parse` so a caller holding a file of unknown kind can
/// ask without paying for the whole decode -- which is what
/// `compat::identify` does.
pub fn is_elf(data: &[u8]) -> bool {
    data.len() >= 4 && data[..4] == MAGIC
}

/// Parses an ELF64 file.
pub fn parse(data: &[u8]) -> Result<Elf, ElfError> {
    if data.len() < EHDR_SIZE {
        return Err(ElfError::TooShort);
    }
    if !is_elf(data) {
        return Err(ElfError::NotElf);
    }
    if data[4] != CLASS_64 {
        return Err(ElfError::NotSixtyFour);
    }
    if data[5] != DATA_LSB {
        return Err(ElfError::NotLittleEndian);
    }

    let machine = u16_at(data, 18);
    if machine != EM_X86_64 {
        return Err(ElfError::WrongMachine(machine));
    }

    let object_type = ObjectType::from_code(u16_at(data, 16));
    let entry = u64_at(data, 24);
    let phoff = u64_at(data, 32) as usize;
    let shoff = u64_at(data, 40) as usize;
    let phentsize = u16_at(data, 54) as usize;
    let phnum = u16_at(data, 56) as usize;
    let shentsize = u16_at(data, 58) as usize;
    let shnum = u16_at(data, 60) as usize;
    let shstrndx = u16_at(data, 62) as usize;

    // --- program headers ---
    let mut segments = Vec::new();
    if phnum > 0 {
        if phentsize != PHDR_SIZE {
            return Err(ElfError::BadEntrySize);
        }
        if phnum > MAX_ENTRIES {
            return Err(ElfError::TooManyEntries(phnum));
        }
        // The multiplication is checked, not assumed: `phnum` comes from
        // the file, and `phoff + phnum * 56` is exactly where a parser
        // wraps and then indexes inside its own buffer with a small
        // offset that looks legitimate.
        let end = phoff
            .checked_add(phnum.checked_mul(PHDR_SIZE).ok_or(ElfError::TableOutOfRange)?)
            .ok_or(ElfError::TableOutOfRange)?;
        if end > data.len() {
            return Err(ElfError::TableOutOfRange);
        }

        for index in 0..phnum {
            let base = phoff + index * PHDR_SIZE;
            let segment = Segment {
                kind: SegmentKind::from_code(u32_at(data, base)),
                flags: u32_at(data, base + 4),
                file_offset: u64_at(data, base + 8),
                virt_addr: u64_at(data, base + 16),
                file_size: u64_at(data, base + 32),
                mem_size: u64_at(data, base + 40),
                align: u64_at(data, base + 48),
            };

            if segment.kind == SegmentKind::Load {
                if segment.mem_size < segment.file_size {
                    return Err(ElfError::ShrinkingSegment);
                }
                let end = segment
                    .file_offset
                    .checked_add(segment.file_size)
                    .ok_or(ElfError::SegmentOutOfRange)?;
                if end > data.len() as u64 {
                    return Err(ElfError::SegmentOutOfRange);
                }
            }

            segments.push(segment);
        }
    }

    // --- section headers ---
    //
    // Optional: a file with none is not malformed, it is stripped. The
    // sections are read for what they let us **refuse**, so their absence
    // means "nothing to refuse on that basis", not "refuse".
    let mut sections = Vec::new();
    if shnum > 0 && shoff > 0 {
        if shentsize != SHDR_SIZE {
            return Err(ElfError::BadEntrySize);
        }
        if shnum > MAX_ENTRIES {
            return Err(ElfError::TooManyEntries(shnum));
        }
        let end = shoff
            .checked_add(shnum.checked_mul(SHDR_SIZE).ok_or(ElfError::TableOutOfRange)?)
            .ok_or(ElfError::TableOutOfRange)?;
        if end > data.len() {
            return Err(ElfError::TableOutOfRange);
        }

        // The string table holding the section names, when it is there.
        let strings = if shstrndx < shnum {
            let base = shoff + shstrndx * SHDR_SIZE;
            let offset = u64_at(data, base + 24) as usize;
            let size = u64_at(data, base + 32) as usize;
            match offset.checked_add(size) {
                Some(end) if end <= data.len() => Some(&data[offset..end]),
                _ => None,
            }
        } else {
            None
        };

        for index in 0..shnum {
            let base = shoff + index * SHDR_SIZE;
            let name_offset = u32_at(data, base) as usize;
            let name = strings
                .map(|table| read_c_string(table, name_offset))
                .unwrap_or_default();

            sections.push(SectionHeader {
                name,
                kind: u32_at(data, base + 4),
                flags: u64_at(data, base + 8),
                addr: u64_at(data, base + 16),
                offset: u64_at(data, base + 24),
                size: u64_at(data, base + 32),
            });
        }
    }

    Ok(Elf {
        object_type,
        machine,
        entry,
        segments,
        sections,
    })
}

/// Reads a NUL-terminated name out of a string table.
///
/// Bounded by the table: an offset past its end gives an empty name, not
/// a walk into whatever follows.
fn read_c_string(table: &[u8], offset: usize) -> String {
    if offset >= table.len() {
        return String::new();
    }
    let end = table[offset..]
        .iter()
        .position(|byte| *byte == 0)
        .map(|length| offset + length)
        .unwrap_or(table.len());
    String::from_utf8_lossy(&table[offset..end]).into_owned()
}

impl Elf {
    /// The loadable segments, in address order.
    ///
    /// Sorted rather than taken as they come: the program header table is
    /// conventionally in address order and nothing enforces it, and a
    /// flattening that trusted the order would produce an image with a
    /// segment in the wrong place.
    pub fn loadable(&self) -> Vec<Segment> {
        let mut load: Vec<Segment> = self
            .segments
            .iter()
            .copied()
            .filter(|segment| segment.kind == SegmentKind::Load && segment.mem_size > 0)
            .collect();
        load.sort_by_key(|segment| segment.virt_addr);
        load
    }

    /// Does this binary need a dynamic linker?
    ///
    /// Either an `INTERP` segment -- it names one outright -- or a
    /// `DYNAMIC` one, which carries the tables a linker would consume.
    pub fn needs_interpreter(&self) -> bool {
        self.segments.iter().any(|segment| {
            matches!(segment.kind, SegmentKind::Interp | SegmentKind::Dynamic)
        })
    }

    /// Does it carry relocations that would have to be applied?
    ///
    /// `SHT_RELA` is 4 and `SHT_REL` is 9. A binary carrying either
    /// expects somebody to fix its addresses up, and this subsystem is
    /// not that somebody -- see the header of `linux`.
    pub fn has_relocations(&self) -> bool {
        self.sections
            .iter()
            .any(|section| matches!(section.kind, 4 | 9) && section.size > 0)
    }

    /// Lowest and highest virtual address of the loadable segments.
    pub fn extent(&self) -> Option<(u64, u64)> {
        let load = self.loadable();
        let first = load.first()?;
        let low = first.virt_addr;
        let high = load
            .iter()
            .map(|segment| segment.virt_addr + segment.mem_size)
            .max()?;
        Some((low, high))
    }

    /// Is the entry point inside a loadable, executable segment?
    ///
    /// An entry point outside the image is the one malformation a loader
    /// cannot survive: it jumps to an unmapped address before anything
    /// has had a chance to notice.
    pub fn entry_is_executable(&self) -> bool {
        self.loadable().iter().any(|segment| {
            segment.executable()
                && self.entry >= segment.virt_addr
                && self.entry < segment.virt_addr + segment.mem_size
        })
    }
}

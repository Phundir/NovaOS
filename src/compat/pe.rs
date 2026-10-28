//! 10.4.2 -- PE32+ parsing.
//!
//! ## The layout, and the two jumps it takes to reach it
//!
//! A PE file starts with a DOS executable from 1981. Byte 0x3C of that
//! stub holds the offset of the real header, which is why every PE parser
//! begins with the same two hops:
//!
//! ```text
//!   0x00  "MZ"                       DOS stub
//!   0x3C  e_lfanew ---------------.   offset of the PE header
//!                                 v
//!   e_lfanew    "PE\0\0"              signature
//!   +4          COFF header       20 bytes: machine, sections, ...
//!   +24         optional header       magic 0x20B = PE32+
//!   +24+size    section table         40 bytes per entry
//! ```
//!
//! `e_lfanew` comes from the file, so it is checked before it is used --
//! a value of 0xFFFF_FFF0 is how a parser reads its header from somewhere
//! else entirely.
//!
//! ## Why the imports matter more than the code
//!
//! A Linux binary can be self-contained: a static ELF calls the kernel
//! directly. A Windows binary essentially never is -- it calls
//! `kernel32.dll`, which calls `ntdll.dll`, which issues the syscall. The
//! import directory is therefore not a detail of the format, it is the
//! **list of what would have to exist** for the file to run, and reading
//! it is what lets the subsystem refuse by name instead of crashing at
//! the first call.
//!
//! ## Field by field
//!
//! Same rule as everywhere: `from_le_bytes` at explicit offsets, no
//! transmutation over a byte slice. A PE comes from a file.

use alloc::string::String;
use alloc::vec::Vec;

/// The two bytes of the DOS stub.
pub const MZ: [u8; 2] = [b'M', b'Z'];
/// The four bytes of the PE signature.
pub const PE_SIGNATURE: [u8; 4] = [b'P', b'E', 0, 0];

/// Optional header magic for a 64-bit image.
pub const MAGIC_PE32_PLUS: u16 = 0x020B;
/// ... and for a 32-bit one, which is refused by name.
pub const MAGIC_PE32: u16 = 0x010B;

/// x86-64 machine code.
pub const MACHINE_AMD64: u16 = 0x8664;

/// Size of one section table entry.
pub const SECTION_ENTRY_SIZE: usize = 40;

/// Ceiling on the number of sections accepted.
pub const MAX_SECTIONS: usize = 96;
/// Ceiling on the number of imported names read.
pub const MAX_IMPORTS: usize = 512;

/// Section characteristics.
pub mod characteristics {
    pub const CODE: u32 = 0x0000_0020;
    pub const INITIALISED_DATA: u32 = 0x0000_0040;
    pub const UNINITIALISED_DATA: u32 = 0x0000_0080;
    pub const EXECUTE: u32 = 0x2000_0000;
    pub const READ: u32 = 0x4000_0000;
    pub const WRITE: u32 = 0x8000_0000;
}

/// Windows subsystem the image asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subsystem {
    Native,
    WindowsGui,
    WindowsConsole,
    Other(u16),
}

impl Subsystem {
    fn from_code(code: u16) -> Self {
        match code {
            1 => Subsystem::Native,
            2 => Subsystem::WindowsGui,
            3 => Subsystem::WindowsConsole,
            other => Subsystem::Other(other),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Subsystem::Native => "natif",
            Subsystem::WindowsGui => "graphique",
            Subsystem::WindowsConsole => "console",
            Subsystem::Other(_) => "inconnu",
        }
    }
}

/// One section of the image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    pub virtual_size: u32,
    pub virtual_address: u32,
    pub raw_size: u32,
    pub raw_offset: u32,
    pub characteristics: u32,
}

impl Section {
    pub fn readable(&self) -> bool {
        self.characteristics & characteristics::READ != 0
    }
    pub fn writable(&self) -> bool {
        self.characteristics & characteristics::WRITE != 0
    }
    pub fn executable(&self) -> bool {
        self.characteristics & characteristics::EXECUTE != 0
    }
    pub fn is_bss(&self) -> bool {
        self.characteristics & characteristics::UNINITIALISED_DATA != 0
    }
}

/// A parsed PE32+ image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pe {
    pub machine: u16,
    pub subsystem: Subsystem,
    /// Entry point, relative to the image base.
    pub entry_rva: u32,
    pub image_base: u64,
    pub section_alignment: u32,
    pub file_alignment: u32,
    pub size_of_image: u32,
    pub sections: Vec<Section>,
    /// Names read from the import directory: `dll!fonction`.
    pub imports: Vec<String>,
    /// Is the image a .NET assembly?
    pub managed: bool,
}

/// Why a file was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeError {
    TooShort,
    NotMz,
    HeaderOutOfRange,
    NotPe,
    NotSixtyFour,
    WrongMachine(u16),
    TooManySections(usize),
    SectionOutOfRange,
}

impl PeError {
    pub fn message(&self) -> String {
        match self {
            PeError::TooShort => String::from("fichier trop court pour un en-tete PE"),
            PeError::NotMz => String::from("signature MZ absente"),
            PeError::HeaderOutOfRange => String::from("en-tete PE hors du fichier"),
            PeError::NotPe => String::from("signature PE absente"),
            PeError::NotSixtyFour => String::from("PE32 32 bits : seul PE32+ est gere"),
            PeError::WrongMachine(code) => {
                alloc::format!("machine 0x{:04x} : seul AMD64 (0x8664) est gere", code)
            }
            PeError::TooManySections(count) => {
                alloc::format!("{} sections : au-dela de la limite du parseur", count)
            }
            PeError::SectionOutOfRange => String::from("section hors du fichier"),
        }
    }
}

fn u16_at(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes([
        *data.get(offset)?,
        *data.get(offset + 1)?,
    ]))
}

fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *data.get(offset)?,
        *data.get(offset + 1)?,
        *data.get(offset + 2)?,
        *data.get(offset + 3)?,
    ]))
}

fn u64_at(data: &[u8], offset: usize) -> Option<u64> {
    let slice = data.get(offset..offset + 8)?;
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(slice);
    Some(u64::from_le_bytes(bytes))
}

/// Is this byte slice a PE at all?
pub fn is_pe(data: &[u8]) -> bool {
    if data.len() < 0x40 || data[..2] != MZ {
        return false;
    }
    match u32_at(data, 0x3C) {
        Some(offset) => {
            let offset = offset as usize;
            data.get(offset..offset + 4) == Some(&PE_SIGNATURE)
        }
        None => false,
    }
}

/// Parses a PE32+ image.
pub fn parse(data: &[u8]) -> Result<Pe, PeError> {
    if data.len() < 0x40 {
        return Err(PeError::TooShort);
    }
    if data[..2] != MZ {
        return Err(PeError::NotMz);
    }

    // `e_lfanew` comes from the file. Checked before it is used: a value
    // near the top of the range is how a parser reads its header from
    // somewhere else entirely.
    let lfanew = u32_at(data, 0x3C).ok_or(PeError::TooShort)? as usize;
    if lfanew.checked_add(24).is_none_or(|end| end > data.len()) {
        return Err(PeError::HeaderOutOfRange);
    }
    if data.get(lfanew..lfanew + 4) != Some(&PE_SIGNATURE) {
        return Err(PeError::NotPe);
    }

    let coff = lfanew + 4;
    let machine = u16_at(data, coff).ok_or(PeError::TooShort)?;
    let section_count = u16_at(data, coff + 2).ok_or(PeError::TooShort)? as usize;
    let optional_size = u16_at(data, coff + 16).ok_or(PeError::TooShort)? as usize;

    if machine != MACHINE_AMD64 {
        return Err(PeError::WrongMachine(machine));
    }

    let optional = coff + 20;
    let magic = u16_at(data, optional).ok_or(PeError::TooShort)?;
    if magic == MAGIC_PE32 {
        return Err(PeError::NotSixtyFour);
    }
    if magic != MAGIC_PE32_PLUS {
        return Err(PeError::NotPe);
    }

    let entry_rva = u32_at(data, optional + 16).ok_or(PeError::TooShort)?;
    let image_base = u64_at(data, optional + 24).ok_or(PeError::TooShort)?;
    let section_alignment = u32_at(data, optional + 32).ok_or(PeError::TooShort)?;
    let file_alignment = u32_at(data, optional + 36).ok_or(PeError::TooShort)?;
    let size_of_image = u32_at(data, optional + 56).ok_or(PeError::TooShort)?;
    let subsystem = Subsystem::from_code(u16_at(data, optional + 68).ok_or(PeError::TooShort)?);
    let directory_count = u32_at(data, optional + 108).ok_or(PeError::TooShort)? as usize;

    // Data directories: eight bytes each, right after the fixed part of
    // the optional header. Entry 1 is the imports, entry 14 the CLR
    // header -- the one that says a file is a .NET assembly and not
    // native code at all.
    let directories = optional + 112;
    let directory = |index: usize| -> Option<(u32, u32)> {
        if index >= directory_count {
            return None;
        }
        let base = directories + index * 8;
        Some((u32_at(data, base)?, u32_at(data, base + 4)?))
    };
    let import_directory = directory(1).filter(|(rva, size)| *rva != 0 && *size != 0);
    let managed = directory(14).is_some_and(|(rva, size)| rva != 0 && size != 0);

    // --- sections ---
    if section_count > MAX_SECTIONS {
        return Err(PeError::TooManySections(section_count));
    }
    let table = optional + optional_size;
    let table_end = table
        .checked_add(
            section_count
                .checked_mul(SECTION_ENTRY_SIZE)
                .ok_or(PeError::SectionOutOfRange)?,
        )
        .ok_or(PeError::SectionOutOfRange)?;
    if table_end > data.len() {
        return Err(PeError::SectionOutOfRange);
    }

    let mut sections = Vec::with_capacity(section_count);
    for index in 0..section_count {
        let base = table + index * SECTION_ENTRY_SIZE;
        let raw_name = &data[base..base + 8];
        let end = raw_name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(raw_name.len());
        let section = Section {
            name: String::from_utf8_lossy(&raw_name[..end]).into_owned(),
            virtual_size: u32_at(data, base + 8).ok_or(PeError::TooShort)?,
            virtual_address: u32_at(data, base + 12).ok_or(PeError::TooShort)?,
            raw_size: u32_at(data, base + 16).ok_or(PeError::TooShort)?,
            raw_offset: u32_at(data, base + 20).ok_or(PeError::TooShort)?,
            characteristics: u32_at(data, base + 36).ok_or(PeError::TooShort)?,
        };

        // A section naming bytes the file does not hold is refused before
        // anything copies from it. `.bss` is the exception that proves
        // the rule: it legitimately has a raw size of zero.
        if section.raw_size > 0 {
            let end = (section.raw_offset as u64)
                .checked_add(section.raw_size as u64)
                .ok_or(PeError::SectionOutOfRange)?;
            if end > data.len() as u64 {
                return Err(PeError::SectionOutOfRange);
            }
        }
        sections.push(section);
    }

    let imports = match import_directory {
        Some((rva, size)) => read_imports(data, &sections, rva, size),
        None => Vec::new(),
    };

    Ok(Pe {
        machine,
        subsystem,
        entry_rva,
        image_base,
        section_alignment,
        file_alignment,
        size_of_image,
        sections,
        imports,
        managed,
    })
}

/// Translates a relative virtual address into a file offset.
///
/// Returns `None` when the address falls in no section, which is what
/// happens with a corrupt directory entry -- and is the reason this is a
/// function rather than a subtraction written at each call site.
pub fn rva_to_offset(sections: &[Section], rva: u32) -> Option<usize> {
    for section in sections {
        let start = section.virtual_address;
        let span = section.virtual_size.max(section.raw_size);
        if rva >= start && rva < start.saturating_add(span) {
            let delta = rva - start;
            if delta >= section.raw_size {
                // Inside the section's memory extent but past what the
                // file holds: uninitialised space, with no file offset.
                return None;
            }
            return Some((section.raw_offset + delta) as usize);
        }
    }
    None
}

/// Reads the import directory into `dll!function` names.
///
/// Best effort: a malformed entry stops the walk instead of failing the
/// parse. The imports exist to be **reported**, and a partial list still
/// names what is missing -- which is more useful than refusing to say
/// anything because the twelfth entry was wrong.
fn read_imports(data: &[u8], sections: &[Section], rva: u32, size: u32) -> Vec<String> {
    let mut names = Vec::new();
    // Twenty bytes per descriptor, terminated by an all-zero one.
    let count = (size as usize / 20).min(MAX_IMPORTS);

    for index in 0..count {
        let Some(entry) = rva_to_offset(sections, rva + (index * 20) as u32) else {
            break;
        };
        let Some(lookup_rva) = u32_at(data, entry) else {
            break;
        };
        let Some(name_rva) = u32_at(data, entry + 12) else {
            break;
        };
        let Some(thunk_rva) = u32_at(data, entry + 16) else {
            break;
        };
        if lookup_rva == 0 && name_rva == 0 && thunk_rva == 0 {
            break;
        }

        let library = rva_to_offset(sections, name_rva)
            .map(|offset| read_c_string(data, offset))
            .unwrap_or_default();

        // The lookup table, when present, holds the function names; the
        // thunk table is its copy and serves when the first is absent.
        let table_rva = if lookup_rva != 0 { lookup_rva } else { thunk_rva };
        let Some(mut cursor) = rva_to_offset(sections, table_rva) else {
            names.push(alloc::format!("{}!*", library));
            continue;
        };

        loop {
            if names.len() >= MAX_IMPORTS {
                return names;
            }
            let Some(value) = u64_at(data, cursor) else {
                break;
            };
            if value == 0 {
                break;
            }
            // The top bit means "imported by ordinal": there is no name
            // to read, and saying so is more honest than an empty one.
            if value & (1 << 63) != 0 {
                names.push(alloc::format!("{}!#{}", library, value & 0xFFFF));
            } else {
                let function = rva_to_offset(sections, value as u32)
                    // Two bytes of hint precede the name.
                    .map(|offset| read_c_string(data, offset + 2))
                    .unwrap_or_default();
                names.push(alloc::format!("{}!{}", library, function));
            }
            cursor += 8;
        }
    }

    names
}

fn read_c_string(data: &[u8], offset: usize) -> String {
    if offset >= data.len() {
        return String::new();
    }
    let end = data[offset..]
        .iter()
        .position(|byte| *byte == 0)
        .map(|length| offset + length)
        .unwrap_or(data.len());
    String::from_utf8_lossy(&data[offset..end]).into_owned()
}

impl Pe {
    /// Sections that occupy memory, in address order.
    pub fn loadable(&self) -> Vec<Section> {
        let mut sections: Vec<Section> = self
            .sections
            .iter()
            .filter(|section| section.virtual_size > 0 || section.raw_size > 0)
            .cloned()
            .collect();
        sections.sort_by_key(|section| section.virtual_address);
        sections
    }

    /// Libraries the image imports, without duplicates.
    pub fn libraries(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for name in self.imports.iter() {
            let library = name.split('!').next().unwrap_or("").to_ascii_lowercase();
            if !library.is_empty() && !out.contains(&library) {
                out.push(library);
            }
        }
        out
    }

    /// Is the entry point inside an executable section?
    pub fn entry_is_executable(&self) -> bool {
        self.sections.iter().any(|section| {
            section.executable()
                && self.entry_rva >= section.virtual_address
                && self.entry_rva
                    < section.virtual_address + section.virtual_size.max(section.raw_size)
        })
    }
}

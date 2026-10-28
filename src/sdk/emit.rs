//! 9.1.2 -- Linking and production of a Nova OS binary.
//!
//! ## What this module is, next to `binfmt::sample`
//!
//! Both write a file in the phase 3 format, and the resemblance stops
//! there. `sample` builds **specimens**: it can produce a binary whose
//! sections overlap, whose certificate comes from an unknown authority,
//! whose payload has been tampered with -- everything needed to put the
//! parser through its paces. Its `.data` is filled with a pattern, its
//! `.text` with `nop` when no code is supplied.
//!
//! `emit` produces **deliverables**: the code comes from a real assembly,
//! the data is the program's own, and nothing lets you build an incorrect
//! file. Merging the two would give a function half of whose parameters
//! serve to break what the other half constructs. They share what must be
//! shared -- the header offset table, in `binfmt::header::offsets` -- and
//! nothing else.
//!
//! ## No relocation
//!
//! `sdk::asm` emits position-independent code only: every reference to a
//! datum is a RIP-relative displacement, every jump is relative. The
//! binary produced here therefore has **no `.reloc` section**, and the
//! loader has nothing to fix up. That is not a simplification: it is the
//! safest mode, since a relocation table is precisely what an attacker
//! would forge to make the loader write anywhere.
//!
//! The price is that the layout must be known **before** the
//! displacements are resolved -- hence `link`, which places the sections
//! and then fills the holes the assembler left.
//!
//! ## Layout
//!
//! ```text
//!   0x0000  header (256 bytes)
//!           port table
//!           section table
//!   page    .text          -> virtual address LOAD_BASE
//!   page    .data          -> LOAD_BASE + extent of .text
//!           (.bss)         -> after .data, nothing in the file
//!           (certificate)  -> after the payload
//! ```
//!
//! `.text` and `.data` each start on a page boundary: the loader maps one
//! read-execute and the other read-write, and two different rights cannot
//! share one page. That is the W^X of phase 3, and it is decided here.

use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::header::{
    BinaryFormat, HEADER_SIZE, SECTION_ENTRY_SIZE, SectionKind, SignatureType, cert_offsets,
    offsets, section_flags,
};
use crate::binfmt::identity::Version;
use crate::binfmt::sha256;
use crate::binfmt::signature;

use super::asm::{Object, Section};

const PAGE: usize = 4096;

/// Link base. The loader does not honour it -- its arena starts elsewhere
/// -- but position-independent code does not care: that is exactly what
/// emitting no relocation buys.
pub const LOAD_BASE: u64 = 0x0040_0000;

/// Size of the certificate block, same as in phase 3.
const CERT_SIZE: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmitError {
    /// A `Fixup` names a label absent from the table.
    UnresolvedSymbol(String),
    /// The RIP displacement does not fit in signed 32 bits: the image
    /// exceeds 2 GiB, which will not happen, but checking costs one
    /// comparison and avoids a silently wrong binary.
    DisplacementOverflow(String),
    /// The requested format and signature form a combination 3.4
    /// forbids.
    ForbiddenCombination(BinaryFormat, SignatureType),
    /// An ENTERPRISE signature was requested with no issuing authority.
    MissingAuthority,
}

impl EmitError {
    pub fn message(&self) -> String {
        match self {
            EmitError::UnresolvedSymbol(name) => {
                alloc::format!("symbole non resolu : {}", name)
            }
            EmitError::DisplacementOverflow(name) => {
                alloc::format!("deplacement trop grand vers {}", name)
            }
            EmitError::ForbiddenCombination(format, signature) => alloc::format!(
                "combinaison interdite par la spec 3.4 : {} + {}",
                format.extension(),
                signature.as_str()
            ),
            EmitError::MissingAuthority => {
                String::from("signature ENTERPRISE sans autorite emettrice")
            }
        }
    }
}

// ============================================================
//  Linking
// ============================================================

/// Layout settled for one image.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    /// Extent of `.text` in memory, rounded up to the page.
    pub text_span: usize,
    /// Extent of `.data` in memory, rounded up to the page.
    pub data_span: usize,
    pub bss_size: u64,
}

impl Layout {
    fn of(object: &Object) -> Layout {
        Layout {
            text_span: align_up(object.text.len().max(1), PAGE),
            // A program with no initialised data occupies no page for
            // `.data`. Reserving one anyway would cost a page per image
            // and, above all, would push `.bss` past a dead page the
            // loader would map read-write with nothing in it.
            data_span: if object.data.is_empty() {
                0
            } else {
                align_up(object.data.len(), PAGE)
            },
            bss_size: object.bss_size,
        }
    }
}

/// Resolves the RIP displacements the assembler left behind.
///
/// The computation involves **image-internal offsets only**: the load
/// address cancels out on both sides of the subtraction. That is what
/// makes the result valid at any base, and it is checkable right here --
/// `LOAD_BASE` does not appear in the formula.
pub fn link(object: &Object, layout: &Layout) -> Result<Vec<u8>, EmitError> {
    let mut text = object.text.clone();

    for fixup in object.fixups.iter() {
        let Some(symbol) = object.symbol(&fixup.symbol) else {
            return Err(EmitError::UnresolvedSymbol(fixup.symbol.clone()));
        };
        // Offset of the hosting section relative to the start of `.text`.
        let section_base = match symbol.section {
            Section::Text => 0i64,
            Section::Data => layout.text_span as i64,
            Section::Bss => (layout.text_span + layout.data_span) as i64,
        };
        let displacement = section_base + symbol.offset as i64 - fixup.next as i64;
        if displacement > i32::MAX as i64 || displacement < i32::MIN as i64 {
            return Err(EmitError::DisplacementOverflow(fixup.symbol.clone()));
        }
        let bytes = (displacement as i32).to_le_bytes();
        text[fixup.at..fixup.at + 4].copy_from_slice(&bytes);
    }

    Ok(text)
}

// ============================================================
//  Emission
// ============================================================

/// What must be known to write a binary.
pub struct EmitSpec<'a> {
    pub format: BinaryFormat,
    pub signature: SignatureType,
    pub app_id: &'a str,
    pub author: &'a str,
    pub group: &'a str,
    pub display_name: &'a str,
    pub version: Version,
    pub permissions: u64,
    pub flags: u32,
    pub ports: &'a [u16],
    pub stack_size: u64,
    /// Issuing authority, mandatory for an ENTERPRISE signature.
    pub authority: Option<Authority<'a>>,
}

/// Certificate authority, for an ENTERPRISE binary.
pub struct Authority<'a> {
    pub name: &'a str,
    pub legal_id: &'a str,
    pub key: [u8; 32],
    /// Days since 2000-01-01. Not checked, for want of a real-time clock
    /// (limit opened by phase 3, step 5.6.3).
    pub valid_from: u64,
    pub valid_until: u64,
}

/// Assembles the complete binary file.
pub fn emit(object: &Object, spec: &EmitSpec) -> Result<Vec<u8>, EmitError> {
    if !signature::combination_allowed(spec.format, spec.signature) {
        return Err(EmitError::ForbiddenCombination(spec.format, spec.signature));
    }
    if spec.signature == SignatureType::Enterprise && spec.authority.is_none() {
        return Err(EmitError::MissingAuthority);
    }

    let layout = Layout::of(object);
    let text = link(object, &layout)?;
    let data = &object.data;

    let has_data = !data.is_empty();
    let section_count: u32 = 1 + u32::from(has_data) + u32::from(layout.bss_size > 0);

    // --- file layout ---
    let port_table = HEADER_SIZE;
    let port_bytes = spec.ports.len() * 2;
    let section_table = align_up(port_table + port_bytes, 8);
    let section_bytes = section_count as usize * SECTION_ENTRY_SIZE;

    let text_offset = align_up(section_table + section_bytes, PAGE);
    let data_offset = text_offset + layout.text_span;
    // The file stops at the last useful byte, not at the end of the
    // memory extent of `.text`: the space between the two exists only in
    // memory, where the loader fills it with zeros. Otherwise a binary
    // with no initialised data weighed a whole page of padding.
    let content_end = if data.is_empty() {
        text_offset + text.len()
    } else {
        data_offset + data.len()
    };
    let payload_end = align_up(content_end, 8);

    let certified = spec.signature == SignatureType::Enterprise;
    let cert_offset = if certified { payload_end } else { 0 };
    let total = payload_end + if certified { CERT_SIZE } else { 0 };

    let mut file = alloc::vec![0u8; total];

    // --- virtual addresses ---
    let text_addr = LOAD_BASE;
    let data_addr = text_addr + layout.text_span as u64;
    let bss_addr = data_addr + layout.data_span as u64;
    let image_size = layout.text_span as u64 + layout.data_span as u64 + layout.bss_size;

    // --- header ---
    file[offsets::MAGIC..offsets::MAGIC + 4].copy_from_slice(&spec.format.magic());
    put_u16(&mut file, offsets::HEADER_VERSION, crate::binfmt::header::HEADER_VERSION);
    put_u16(&mut file, offsets::HEADER_SIZE, HEADER_SIZE as u16);
    put_u16(&mut file, offsets::ABI_VERSION, crate::binfmt::header::ABI_VERSION);
    file[offsets::BINARY_TYPE] = spec.format as u8;
    file[offsets::SIGNATURE_TYPE] = spec.signature as u8;
    put_u32(&mut file, offsets::FLAGS, spec.flags);

    put_u64(&mut file, offsets::ENTRY_POINT, text_addr + object.entry);
    put_u64(&mut file, offsets::LOAD_BASE, text_addr);
    put_u64(&mut file, offsets::IMAGE_SIZE, image_size);
    put_u64(&mut file, offsets::BSS_SIZE, layout.bss_size);
    put_u64(&mut file, offsets::STACK_SIZE, spec.stack_size);

    put_u32(&mut file, offsets::SECTION_COUNT, section_count);
    put_u32(&mut file, offsets::SECTION_TABLE, section_table as u32);

    put_name(&mut file, offsets::APP_ID, spec.app_id);
    put_name(&mut file, offsets::AUTHOR, spec.author);
    put_name(&mut file, offsets::GROUP, spec.group);
    put_name(&mut file, offsets::DISPLAY_NAME, spec.display_name);

    put_u16(&mut file, offsets::VERSION_MAJOR, spec.version.major);
    put_u16(&mut file, offsets::VERSION_MINOR, spec.version.minor);
    put_u16(&mut file, offsets::VERSION_PATCH, spec.version.patch);
    put_u16(&mut file, offsets::VERSION_BUILD, spec.version.build);

    put_u64(&mut file, offsets::PERMISSIONS, spec.permissions);
    put_u16(&mut file, offsets::PORT_COUNT, spec.ports.len() as u16);
    put_u16(&mut file, offsets::PORT_TABLE, port_table as u16);
    put_u32(&mut file, offsets::CERT_OFFSET, cert_offset as u32);

    put_u32(&mut file, offsets::PAYLOAD_OFFSET, HEADER_SIZE as u32);
    put_u32(
        &mut file,
        offsets::PAYLOAD_SIZE,
        (payload_end - HEADER_SIZE) as u32,
    );

    // --- port table ---
    for (index, port) in spec.ports.iter().enumerate() {
        put_u16(&mut file, port_table + index * 2, *port);
    }

    // --- section table ---
    let mut slot = section_table;
    write_section(
        &mut file,
        slot,
        b".text",
        SectionKind::Code,
        section_flags::READ | section_flags::EXEC,
        text_offset as u32,
        text.len() as u32,
        text.len() as u32,
        text_addr,
    );
    slot += SECTION_ENTRY_SIZE;

    if has_data {
        write_section(
            &mut file,
            slot,
            b".data",
            SectionKind::Data,
            section_flags::READ | section_flags::WRITE,
            data_offset as u32,
            data.len() as u32,
            data.len() as u32,
            data_addr,
        );
        slot += SECTION_ENTRY_SIZE;
    }

    if layout.bss_size > 0 {
        write_section(
            &mut file,
            slot,
            b".bss",
            SectionKind::Bss,
            section_flags::READ | section_flags::WRITE,
            0,
            0,
            layout.bss_size as u32,
            bss_addr,
        );
    }

    // --- content ---
    file[text_offset..text_offset + text.len()].copy_from_slice(&text);
    // The file stops at the end of `.text` when there is no data:
    // `data_offset` then names a position **past** its last cell. An
    // empty slice is bounded all the same -- Rust checks the start index,
    // even when the length is zero -- and `file[data_offset..data_offset]`
    // panicked on every binary without a `.data` section, that is to say
    // on the five templates of 15.5.
    if has_data {
        file[data_offset..data_offset + data.len()].copy_from_slice(data);
    }

    // --- payload digest ---
    let digest = sha256::digest(&file[HEADER_SIZE..payload_end]);
    file[offsets::PAYLOAD_HASH..offsets::PAYLOAD_HASH + 32].copy_from_slice(&digest);

    // --- certificate ---
    if let Some(authority) = spec.authority.as_ref()
        && certified
    {
        write_certificate(&mut file, cert_offset, spec, authority);
    }

    Ok(file)
}

/// Writes the certificate block and its binding to the binary.
///
/// The binding covers the authenticity digest, which covers the header
/// and the payload: it is therefore computed last, on an already frozen
/// file. Same order as in phase 3, same reason.
fn write_certificate(file: &mut [u8], offset: usize, spec: &EmitSpec, authority: &Authority) {
    file[offset..offset + 4].copy_from_slice(&crate::binfmt::header::CERT_MAGIC);
    put_u16(
        file,
        offset + cert_offsets::VERSION,
        crate::binfmt::header::CERT_VERSION,
    );
    put_u16(
        file,
        offset + cert_offsets::SIGNATURE_TYPE,
        SignatureType::Enterprise as u16,
    );
    put_cert_name(file, offset + cert_offsets::AUTHORITY, authority.name);
    put_cert_name(file, offset + cert_offsets::SUBJECT, spec.author);
    put_cert_name(file, offset + cert_offsets::LEGAL_ID, authority.legal_id);
    put_u64(file, offset + cert_offsets::VALID_FROM, authority.valid_from);
    put_u64(
        file,
        offset + cert_offsets::VALID_UNTIL,
        authority.valid_until,
    );
    file[offset + cert_offsets::AUTHORITY_KEY..offset + cert_offsets::AUTHORITY_KEY + 32]
        .copy_from_slice(&authority.key);

    let Ok(header) = crate::binfmt::parse(file) else {
        return;
    };
    let digest = signature::authenticity_digest(&header, file);
    let binding = signature::expected_binding(&authority.key, &digest);
    file[offset + cert_offsets::BINDING..offset + cert_offsets::BINDING + 32]
        .copy_from_slice(&binding);
}

// ============================================================
//  Low-level writing
// ============================================================

fn align_up(value: usize, align: usize) -> usize {
    value.div_ceil(align) * align
}

fn put_u16(buffer: &mut [u8], offset: usize, value: u16) {
    buffer[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(buffer: &mut [u8], offset: usize, value: u64) {
    buffer[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// Writes a name into a 32-byte field, null terminator included.
fn put_name(buffer: &mut [u8], offset: usize, text: &str) {
    for (index, byte) in text.bytes().take(31).enumerate() {
        buffer[offset + index] = byte;
    }
}

fn put_cert_name(buffer: &mut [u8], offset: usize, text: &str) {
    for (index, byte) in text.bytes().take(31).enumerate() {
        buffer[offset + index] = byte;
    }
}

#[allow(clippy::too_many_arguments)]
fn write_section(
    buffer: &mut [u8],
    offset: usize,
    name: &[u8],
    kind: SectionKind,
    flags: u8,
    file_offset: u32,
    file_size: u32,
    mem_size: u32,
    virt_addr: u64,
) {
    let length = name.len().min(8);
    buffer[offset..offset + length].copy_from_slice(&name[..length]);
    buffer[offset + 8] = kind as u8;
    buffer[offset + 9] = flags;
    put_u16(buffer, offset + 10, 16);
    put_u32(buffer, offset + 12, file_offset);
    put_u32(buffer, offset + 16, file_size);
    put_u32(buffer, offset + 20, mem_size);
    put_u64(buffer, offset + 24, virt_addr);
}

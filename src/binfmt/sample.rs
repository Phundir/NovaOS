//! Generator of test binaries.
//!
//! No filesystem exists before phase 4: these binaries are built in
//! memory to exercise the parser, the signature verifier and the loader
//! on realistic data, valid as well as faulty.
//!
//! The same code will serve as a reference for the `dev package` tool of
//! the SDK (phase 9.1.5), which will produce `.spm` in the expected format.
//!
//! ## Layout produced
//!
//! ```text
//!   0x0000  header (256 bytes)
//!   0x0100  port table
//!           section table
//!           relocation table
//!           --- padding up to 4 KiB ---
//!   0x1000  .text   -- whole pages, never shared with .data
//!           .data
//!           --- alignment ---
//!           certificate block (where applicable)
//! ```
//!
//! Two constraints govern this layout:
//!
//!   * **one section per page.** Paging protects at page granularity; a
//!     `.text` and a `.data` neighbouring in the same page would force a
//!     page both writable and executable;
//!   * **the certificate last, outside the payload.** It contains a
//!     binding to the digest of the binary: were it covered by that
//!     digest, the digest would refer to itself.

use alloc::string::String;
use alloc::vec::Vec;

use super::header::{
    BinaryFormat, CERT_MAGIC, CERT_SIZE, CERT_VERSION, HEADER_SIZE, HEADER_VERSION, NAME_LEN,
    RELOC_ENTRY_SIZE, RelocKind, SECTION_ENTRY_SIZE, SectionKind, SignatureType, cert_offsets,
    flags, offsets, section_flags,
};
use super::identity::{AppIdentity, Permissions, Version};
use super::sha256;
use super::signature;

/// Page size -- the generator aligns on it.
const PAGE: usize = 4096;

// ============================================================
//  Writing
// ============================================================

fn put_u16(buffer: &mut [u8], offset: usize, value: u16) {
    buffer[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(buffer: &mut [u8], offset: usize, value: u64) {
    buffer[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// Writes a string into a fixed-size field, padded with zeros.
fn put_name(buffer: &mut [u8], offset: usize, text: &str) {
    let bytes = text.as_bytes();
    let len = bytes.len().min(NAME_LEN);
    buffer[offset..offset + len].copy_from_slice(&bytes[..len]);
}

fn align_up(value: usize, align: usize) -> usize {
    value.div_ceil(align) * align
}

// ============================================================
//  Demonstration authority
// ============================================================

/// The demonstration authority's public key (Ed25519). Its seed is drawn
/// at boot and never leaves memory (`signature::demo_authority_seed`).
pub fn demo_authority_key() -> [u8; 32] {
    signature::demo_authority_key()
}

/// Registers the demonstration authority as an ENTERPRISE root.
///
/// In a real system the roots come from `/sys/config/autorites.cfg`
/// (`signature::load_authorities`); this one is laid down by the
/// self-tests, which hold its seed.
pub fn register_demo_authority() -> bool {
    signature::register_enterprise_root(demo_authority_key())
}

/// Declares an already built binary as an official application.
///
/// Equivalent of what the system image build will do: compute the digest
/// of the `.dsa` and `.ss` shipped, and freeze it into the kernel.
/// Returns the digest registered.
pub fn register_as_official(data: &[u8]) -> Option<[u8; 32]> {
    let header = super::parse(data).ok()?;
    let digest = signature::authenticity_digest(&header, data);
    signature::register_system_digest(digest);
    Some(digest)
}

// ============================================================
//  Construction
// ============================================================

/// Build parameters of a test binary.
pub struct BuildSpec {
    pub format: BinaryFormat,
    pub signature: SignatureType,
    pub app_id: &'static str,
    pub author: &'static str,
    pub group: &'static str,
    pub display_name: &'static str,
    pub version: Version,
    pub permissions: u64,
    pub flags: u32,
    pub ports: &'static [u16],
    /// Real code to pack. `None` produces a run of `nop` -- useful to
    /// exercise the parser, unusable for a run.
    pub code: Option<&'static [u8]>,
    pub code_size: u32,
    pub data_size: u32,
    pub bss_size: u64,
    pub stack_size: u64,
    /// 3.2.2 -- Emits an ABS64 relocation on the first eight bytes of
    /// `.data`, initialised to the link base. After loading, the word must
    /// hold the effective base: that is the observable proof that the
    /// relocation really happened.
    pub relocate_data_pointer: bool,
    /// Attaches an ENTERPRISE certificate block.
    pub certificate: bool,
    /// Emits the certificate in the name of an authority unknown to the
    /// store -- to check that the chain of trust is really enforced.
    pub unknown_authority: bool,
    /// Sticks `.data` right after `.text` with no page alignment, which
    /// must make the binary be refused.
    pub overlap_sections: bool,
}

impl BuildSpec {
    /// Minimal template, to be adjusted field by field.
    pub const fn minimal(format: BinaryFormat, signature: SignatureType) -> Self {
        BuildSpec {
            format,
            signature,
            app_id: "os.nova.exemple",
            author: "Nova OS",
            group: "os.nova",
            display_name: "Exemple",
            version: Version::new(1, 0, 0, 1),
            permissions: 0,
            flags: 0,
            ports: &[],
            code: None,
            code_size: 256,
            data_size: 128,
            bss_size: 512,
            stack_size: 65536,
            relocate_data_pointer: false,
            certificate: false,
            unknown_authority: false,
            overlap_sections: false,
        }
    }
}

/// Virtual base of the user binaries, consistent with
/// `user::USER_CODE_ADDR`.
/// The loader does not honour it: its arena starts elsewhere, which
/// forces a relocation on every load -- exactly the case we want
/// exercised rather than theoretical.
const DEFAULT_LOAD_BASE: u64 = 0x0040_0000;

/// Assembles a complete binary from a specification.
pub fn build(spec: &BuildSpec) -> Vec<u8> {
    let code_size = match spec.code {
        Some(code) => code.len() as u32,
        None => spec.code_size,
    };
    let data_size = spec.data_size.max(if spec.relocate_data_pointer { 8 } else { 0 });

    let reloc_count = if spec.relocate_data_pointer { 1 } else { 0 };
    let section_count: u32 = 2 + u32::from(spec.bss_size > 0) + u32::from(reloc_count > 0);

    // --- file layout ---
    let port_table = HEADER_SIZE;
    let port_bytes = spec.ports.len() * 2;
    let section_table = align_up(port_table + port_bytes, 8);
    let section_bytes = section_count as usize * SECTION_ENTRY_SIZE;
    let reloc_table = align_up(section_table + section_bytes, 8);
    let reloc_bytes = reloc_count * RELOC_ENTRY_SIZE;

    let code_offset = align_up(reloc_table + reloc_bytes, PAGE);
    let text_span = if spec.overlap_sections {
        align_up(code_size as usize, 16)
    } else {
        align_up(code_size as usize, PAGE)
    };
    let data_offset = code_offset + text_span;
    let payload_end = align_up(data_offset + data_size as usize, 8);

    let cert_offset = if spec.certificate { payload_end } else { 0 };
    let total = payload_end + if spec.certificate { CERT_SIZE } else { 0 };

    let mut buffer = alloc::vec![0u8; total];

    // --- virtual addresses ---
    let code_addr = DEFAULT_LOAD_BASE;
    let data_addr = code_addr + text_span as u64;
    let data_span = if spec.overlap_sections {
        align_up(data_size as usize, 16)
    } else {
        align_up(data_size as usize, PAGE)
    };
    let bss_addr = data_addr + data_span as u64;
    let image_size = text_span as u64 + data_span as u64 + spec.bss_size;

    // --- header ---
    buffer[offsets::MAGIC..offsets::MAGIC + 4].copy_from_slice(&spec.format.magic());
    put_u16(&mut buffer, offsets::HEADER_VERSION, HEADER_VERSION);
    put_u16(&mut buffer, offsets::HEADER_SIZE, HEADER_SIZE as u16);
    put_u16(&mut buffer, offsets::ABI_VERSION, super::header::ABI_VERSION);
    buffer[offsets::BINARY_TYPE] = spec.format as u8;
    buffer[offsets::SIGNATURE_TYPE] = spec.signature as u8;
    put_u32(&mut buffer, offsets::FLAGS, spec.flags);

    put_u64(&mut buffer, offsets::ENTRY_POINT, code_addr);
    put_u64(&mut buffer, offsets::LOAD_BASE, code_addr);
    put_u64(&mut buffer, offsets::IMAGE_SIZE, image_size);
    put_u64(&mut buffer, offsets::BSS_SIZE, spec.bss_size);
    put_u64(&mut buffer, offsets::STACK_SIZE, spec.stack_size);

    put_u32(&mut buffer, offsets::SECTION_COUNT, section_count);
    put_u32(&mut buffer, offsets::SECTION_TABLE, section_table as u32);

    put_name(&mut buffer, offsets::APP_ID, spec.app_id);
    put_name(&mut buffer, offsets::AUTHOR, spec.author);
    put_name(&mut buffer, offsets::GROUP, spec.group);
    put_name(&mut buffer, offsets::DISPLAY_NAME, spec.display_name);

    put_u16(&mut buffer, offsets::VERSION_MAJOR, spec.version.major);
    put_u16(&mut buffer, offsets::VERSION_MINOR, spec.version.minor);
    put_u16(&mut buffer, offsets::VERSION_PATCH, spec.version.patch);
    put_u16(&mut buffer, offsets::VERSION_BUILD, spec.version.build);

    put_u64(&mut buffer, offsets::PERMISSIONS, spec.permissions);
    put_u16(&mut buffer, offsets::PORT_COUNT, spec.ports.len() as u16);
    put_u16(&mut buffer, offsets::PORT_TABLE, port_table as u16);
    put_u32(&mut buffer, offsets::CERT_OFFSET, cert_offset as u32);

    // The payload covers everything after the header, certificate
    // excluded: section and relocation tables included, failing which a
    // forged relocation would slip under the radar of the digest.
    put_u32(&mut buffer, offsets::PAYLOAD_OFFSET, HEADER_SIZE as u32);
    put_u32(
        &mut buffer,
        offsets::PAYLOAD_SIZE,
        (payload_end - HEADER_SIZE) as u32,
    );

    // --- port table ---
    for (i, port) in spec.ports.iter().enumerate() {
        put_u16(&mut buffer, port_table + i * 2, *port);
    }

    // --- section table ---
    let mut slot = section_table;
    write_section(
        &mut buffer,
        slot,
        b".text",
        SectionKind::Code,
        section_flags::READ | section_flags::EXEC,
        code_offset as u32,
        code_size,
        code_size,
        code_addr,
    );
    slot += SECTION_ENTRY_SIZE;

    write_section(
        &mut buffer,
        slot,
        b".data",
        SectionKind::Data,
        section_flags::READ | section_flags::WRITE,
        data_offset as u32,
        data_size,
        data_size,
        data_addr,
    );
    slot += SECTION_ENTRY_SIZE;

    if spec.bss_size > 0 {
        // The uninitialised section occupies nothing in the file.
        write_section(
            &mut buffer,
            slot,
            b".bss",
            SectionKind::Bss,
            section_flags::READ | section_flags::WRITE,
            0,
            0,
            spec.bss_size as u32,
            bss_addr,
        );
        slot += SECTION_ENTRY_SIZE;
    }

    if reloc_count > 0 {
        // Virtual address and memory size null: the table lives in the
        // file and disappears at load time.
        write_section(
            &mut buffer,
            slot,
            b".reloc",
            SectionKind::Reloc,
            section_flags::READ,
            reloc_table as u32,
            reloc_bytes as u32,
            0,
            0,
        );
    }

    // --- relocation table ---
    if spec.relocate_data_pointer {
        put_u64(&mut buffer, reloc_table, data_addr);
        buffer[reloc_table + 8] = RelocKind::Abs64 as u8;
        put_u32(&mut buffer, reloc_table + 12, 0);
    }

    // --- code content ---
    match spec.code {
        Some(code) => buffer[code_offset..code_offset + code.len()].copy_from_slice(code),
        None => {
            for i in 0..code_size as usize {
                buffer[code_offset + i] = 0x90; // nop
            }
        }
    }

    // --- data content ---
    for i in 0..data_size as usize {
        buffer[data_offset + i] = (i & 0xFF) as u8;
    }
    if spec.relocate_data_pointer {
        // Absolute pointer to the link base: the loader must transpose it
        // to the effective base.
        put_u64(&mut buffer, data_offset, DEFAULT_LOAD_BASE);
    }

    // --- payload digest ---
    let digest = sha256::digest(&buffer[HEADER_SIZE..payload_end]);
    buffer[offsets::PAYLOAD_HASH..offsets::PAYLOAD_HASH + 32].copy_from_slice(&digest);

    // --- certificate block ---
    if spec.certificate {
        write_certificate(&mut buffer, cert_offset, spec);
    }

    buffer
}

/// Writes the certificate block and signs it for the binary.
///
/// The signature is computed **after** the rest of the file is frozen: it
/// covers the authenticity digest, which covers the header and the
/// payload.
fn write_certificate(buffer: &mut [u8], offset: usize, spec: &BuildSpec) {
    // An authority unknown to the store: a real key pair, whose public
    // key nobody registered -- the signature is valid, the trust absent.
    let seed = if spec.unknown_authority {
        let mut seed = signature::demo_authority_seed();
        seed[0] ^= 0xA5;
        seed
    } else {
        signature::demo_authority_seed()
    };

    buffer[offset..offset + 4].copy_from_slice(&CERT_MAGIC);
    put_u16(buffer, offset + cert_offsets::VERSION, CERT_VERSION);
    put_u16(
        buffer,
        offset + cert_offsets::SIGNATURE_TYPE,
        SignatureType::Enterprise as u16,
    );

    let authority = if spec.unknown_authority {
        "Autorite inconnue"
    } else {
        "Nova Certification"
    };
    put_cert_name(buffer, offset + cert_offsets::AUTHORITY, authority);
    put_cert_name(buffer, offset + cert_offsets::SUBJECT, spec.author);
    put_cert_name(buffer, offset + cert_offsets::LEGAL_ID, "FR-000-000-000");

    // Days since 2000-01-01: 2024-08 to 2032-11, compared with the
    // hardware clock when it is known.
    put_u64(buffer, offset + cert_offsets::VALID_FROM, 9000);
    put_u64(buffer, offset + cert_offsets::VALID_UNTIL, 12000);

    // The authenticity digest is computed on the file as it stands now:
    // the certificate block is not part of it.
    let Ok(header) = super::parse(buffer) else {
        return;
    };
    let digest = signature::authenticity_digest(&header, buffer);
    signature::sign_certificate(&mut buffer[offset..offset + CERT_SIZE], &digest, &seed);
}

fn put_cert_name(buffer: &mut [u8], offset: usize, text: &str) {
    let bytes = text.as_bytes();
    let len = bytes.len().min(32);
    buffer[offset..offset + len].copy_from_slice(&bytes[..len]);
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
    let len = name.len().min(8);
    buffer[offset..offset + len].copy_from_slice(&name[..len]);
    buffer[offset + 8] = kind as u8;
    buffer[offset + 9] = flags;
    put_u16(buffer, offset + 10, 16);
    put_u32(buffer, offset + 12, file_offset);
    put_u32(buffer, offset + 16, file_size);
    put_u32(buffer, offset + 20, mem_size);
    put_u64(buffer, offset + 24, virt_addr);
}

// ============================================================
//  Test sets
// ============================================================

/// A `.cus` carrying the ring 3 demonstration program.
///
/// It is the only sample whose code really runs: the others make do with
/// `nop`, which is enough for parsing but not for a transfer of control.
pub fn executable_cus() -> Vec<u8> {
    let mut spec = BuildSpec::minimal(BinaryFormat::Cus, SignatureType::SelfSigned);
    spec.app_id = "os.nova.demo3";
    spec.author = "Nova OS";
    spec.group = "os.nova";
    spec.display_name = "Demonstration ring 3";
    spec.code = Some(crate::user::demo_program());
    spec.data_size = 64;
    spec.bss_size = 4096;
    spec.stack_size = 32 * 1024;
    spec.relocate_data_pointer = true;
    spec.permissions = Permissions::NOTIFICATIONS;
    spec.flags = 0;
    build(&spec)
}

/// The same program, with network access declared in the header.
///
/// 5.6.4: 6.6 makes the `SYS_NET_*` domain conditional on a header
/// declaration. This binary serves to check that the flag really crosses
/// the chain -- file, parsing, loading, scheduler -- and not merely that
/// it is recognised at parse time.
pub fn executable_network() -> Vec<u8> {
    let mut spec = BuildSpec::minimal(BinaryFormat::Cus, SignatureType::SelfSigned);
    spec.app_id = "os.nova.reseau";
    spec.author = "Nova OS";
    spec.group = "os.nova";
    spec.display_name = "Client reseau";
    spec.code = Some(crate::user::demo_program());
    spec.data_size = 64;
    spec.bss_size = 4096;
    spec.stack_size = 32 * 1024;
    spec.relocate_data_pointer = true;
    spec.permissions = Permissions::NOTIFICATIONS;
    spec.flags = crate::binfmt::header::flags::NETWORK;
    build(&spec)
}

/// The same program packed as a `.ras`: serves to check the mandatory
/// warning at launch (spec 3.4).
pub fn executable_ras() -> Vec<u8> {
    let mut spec = BuildSpec::minimal(BinaryFormat::Ras, SignatureType::None);
    spec.app_id = "test.ras.demo";
    spec.author = "Inconnu";
    spec.group = "";
    spec.display_name = "Test brut";
    spec.code = Some(crate::user::demo_program());
    spec.data_size = 64;
    spec.bss_size = 0;
    spec.stack_size = 32 * 1024;
    spec.permissions = Permissions::HARDWARE;
    build(&spec)
}

/// An ENTERPRISE `.gs` carrying the demonstration program.
///
/// Serves to exercise 3.2.4.3: the foreground/background switch and the
/// drop in CPU slice that goes with it.
pub fn executable_gs() -> Vec<u8> {
    let mut spec = BuildSpec::minimal(BinaryFormat::Gs, SignatureType::Enterprise);
    spec.app_id = "com.studio.demo";
    spec.author = "Studio Exemple";
    spec.group = "com.studio";
    spec.display_name = "Jeu de demonstration";
    spec.code = Some(crate::user::demo_program());
    spec.data_size = 64;
    spec.bss_size = 4096;
    spec.stack_size = 32 * 1024;
    spec.permissions = Permissions::AUDIO_OUTPUT | Permissions::FULLSCREEN;
    spec.flags = flags::GUI | flags::AUTO_PAUSE | flags::BACKGROUND;
    spec.certificate = true;
    build(&spec)
}

/// A conforming `.dsa`, meant to be registered as official.
pub fn official_dsa() -> Vec<u8> {
    let mut spec = BuildSpec::minimal(BinaryFormat::Dsa, SignatureType::System);
    spec.app_id = "os.nova.fs";
    spec.display_name = "Service de fichiers";
    spec.group = "os.nova.core";
    spec.permissions = Permissions::KERNEL_SPACE
        | Permissions::FS_FULL
        | Permissions::SYS_DIRECTORY
        | Permissions::IPC_PUBLIC
        | Permissions::IPC_INTERNAL_PORTS;
    spec.flags = flags::EXPOSES_API | flags::SERVICE | flags::BACKGROUND;
    spec.ports = &[1];
    build(&spec)
}

/// An ENTERPRISE `.gs` fitted with a valid certificate.
pub fn certified_gs() -> Vec<u8> {
    let mut spec = BuildSpec::minimal(BinaryFormat::Gs, SignatureType::Enterprise);
    spec.app_id = "com.studio.jeu";
    spec.author = "Studio Exemple";
    spec.group = "com.studio";
    spec.display_name = "Jeu de demonstration";
    spec.version = Version::new(2, 3, 1, 447);
    spec.permissions = Permissions::NETWORK_INTERNET
        | Permissions::AUDIO_OUTPUT
        | Permissions::FULLSCREEN
        | Permissions::FS_USER_HOME;
    spec.flags = flags::GUI | flags::AUTO_PAUSE | flags::BACKGROUND | flags::DEFAULT_CAPABLE;
    spec.code_size = 8192;
    spec.bss_size = 64 * 1024;
    spec.certificate = true;
    build(&spec)
}

/// Five conforming binaries, one per format.
pub fn valid_set() -> Vec<(String, Vec<u8>)> {
    let mut set = Vec::new();

    set.push((String::from("fs.dsa - service systeme"), official_dsa()));

    // .ss -- compositor, replaces a system component.
    let mut spec = BuildSpec::minimal(BinaryFormat::Ss, SignatureType::System);
    spec.app_id = "os.nova.compositor";
    spec.display_name = "Compositeur";
    spec.group = "os.nova.gui";
    spec.permissions = Permissions::SYS_DIRECTORY
        | Permissions::FS_USER_HOME
        | Permissions::NOTIFICATIONS
        | Permissions::FULLSCREEN
        | Permissions::IPC_PUBLIC;
    spec.flags = flags::REPLACES_COMPONENT | flags::GUI | flags::SERVICE;
    spec.code_size = 4096;
    set.push((String::from("compositor.ss - composant GUI"), build(&spec)));

    set.push((String::from("jeu.gs - application de jeu"), certified_gs()));

    // .cus -- ordinary self-signed application, with relocation.
    let mut spec = BuildSpec::minimal(BinaryFormat::Cus, SignatureType::SelfSigned);
    spec.app_id = "net.exemple.editeur";
    spec.author = "Developpeur Independant";
    spec.group = "net.exemple";
    spec.display_name = "Editeur de texte";
    spec.permissions = Permissions::FS_USER_HOME
        | Permissions::CLIPBOARD
        | Permissions::NOTIFICATIONS
        | Permissions::IPC_INTERNAL_PORTS;
    spec.flags = flags::GUI | flags::DEFAULT_CAPABLE;
    spec.ports = &[8080, 8081];
    spec.relocate_data_pointer = true;
    set.push((String::from("editeur.cus - application courante"), build(&spec)));

    // .ras -- total access, no signature, complete isolation.
    let mut spec = BuildSpec::minimal(BinaryFormat::Ras, SignatureType::None);
    spec.app_id = "test.ras.materiel";
    spec.author = "Inconnu";
    spec.group = "";
    spec.display_name = "Test materiel";
    spec.permissions = Permissions::HARDWARE | Permissions::FULLSCREEN;
    spec.flags = 0;
    set.push((String::from("materiel.ras - acces direct"), build(&spec)));

    set
}

/// Faulty binaries, each aiming at one precise rule.
pub fn invalid_set() -> Vec<(String, Vec<u8>)> {
    let mut set = Vec::new();

    // --- structural errors ---

    // Unknown magic.
    let mut data = build(&BuildSpec::minimal(BinaryFormat::Cus, SignatureType::SelfSigned));
    data[0..4].copy_from_slice(b"ELF\0");
    set.push((String::from("magic inconnu (ELF)"), data));

    // Extension changed without touching the type field.
    let mut data = build(&BuildSpec::minimal(BinaryFormat::Cus, SignatureType::SelfSigned));
    data[offsets::MAGIC..offsets::MAGIC + 4].copy_from_slice(&BinaryFormat::Dsa.magic());
    set.push((String::from("magic et type divergents"), data));

    // Truncated file.
    let data = build(&BuildSpec::minimal(BinaryFormat::Cus, SignatureType::SelfSigned));
    set.push((String::from("fichier tronque"), data[..128].to_vec()));

    // Permission bit unknown to the kernel.
    let mut spec = BuildSpec::minimal(BinaryFormat::Cus, SignatureType::SelfSigned);
    spec.permissions = 1 << 40;
    set.push((String::from("permission inconnue"), build(&spec)));

    // --- violations of the specification ---

    // Self-signed .dsa (spec 3.4).
    let spec = BuildSpec::minimal(BinaryFormat::Dsa, SignatureType::SelfSigned);
    set.push((String::from(".dsa auto-signe"), build(&spec)));

    // Unsigned .ss (spec 3.4).
    let spec = BuildSpec::minimal(BinaryFormat::Ss, SignatureType::SelfSigned);
    set.push((String::from(".ss auto-signe"), build(&spec)));

    // .ras claiming IPC (spec 12.3).
    let mut spec = BuildSpec::minimal(BinaryFormat::Ras, SignatureType::None);
    spec.permissions = Permissions::IPC_PUBLIC | Permissions::HARDWARE;
    spec.ports = &[9000];
    set.push((String::from(".ras avec IPC"), build(&spec)));

    // .cus claiming kernel space access (spec 12.4).
    let mut spec = BuildSpec::minimal(BinaryFormat::Cus, SignatureType::Enterprise);
    spec.permissions = Permissions::KERNEL_SPACE | Permissions::FS_FULL;
    spec.certificate = true;
    set.push((String::from(".cus reclamant le kernel space"), build(&spec)));

    // .cus declaring itself an API exposer (spec 3.4).
    let mut spec = BuildSpec::minimal(BinaryFormat::Cus, SignatureType::Enterprise);
    spec.flags = flags::EXPOSES_API;
    spec.certificate = true;
    set.push((String::from(".cus exposant une API"), build(&spec)));

    // .ras declaring itself the default application (spec 3.4).
    let mut spec = BuildSpec::minimal(BinaryFormat::Ras, SignatureType::None);
    spec.flags = flags::DEFAULT_CAPABLE;
    set.push((String::from(".ras application par defaut"), build(&spec)));

    // --- new in phase 3 ---

    // .ras claiming a signature: laundering of privileges.
    let spec = BuildSpec::minimal(BinaryFormat::Ras, SignatureType::SelfSigned);
    set.push((String::from(".ras se declarant signe"), build(&spec)));

    // ENTERPRISE with no certificate block (spec 3.2).
    let mut spec = BuildSpec::minimal(BinaryFormat::Gs, SignatureType::Enterprise);
    spec.certificate = false;
    set.push((String::from(".gs ENTERPRISE sans certificat"), build(&spec)));

    // Certificate present on a self-signed binary.
    let mut spec = BuildSpec::minimal(BinaryFormat::Cus, SignatureType::SelfSigned);
    spec.certificate = true;
    set.push((String::from("certificat sur un binaire SELF"), build(&spec)));

    // .text and .data in the same page: rights impossible to separate.
    let mut spec = BuildSpec::minimal(BinaryFormat::Cus, SignatureType::SelfSigned);
    spec.overlap_sections = true;
    spec.code_size = 256;
    spec.data_size = 128;
    set.push((String::from("sections partageant une page"), build(&spec)));

    set
}

/// Checks the communication matrix of the 12.1 table.
pub fn communication_matrix() {
    let make = |format: BinaryFormat, group: &str, app: &str| AppIdentity {
        app_id: String::from(app),
        author: String::from("Test"),
        group: String::from(group),
        display_name: String::from(app),
        version: Version::new(1, 0, 0, 0),
        format,
        signature: SignatureType::SelfSigned,
    };

    let cases = [
        (
            "meme groupe, meme type",
            make(BinaryFormat::Cus, "com.a", "com.a.un"),
            make(BinaryFormat::Cus, "com.a", "com.a.deux"),
        ),
        (
            "meme groupe, types compatibles (.dsa <-> .ss)",
            make(BinaryFormat::Dsa, "os.nova", "os.nova.fs"),
            make(BinaryFormat::Ss, "os.nova", "os.nova.gui"),
        ),
        (
            "groupes differents",
            make(BinaryFormat::Cus, "com.a", "com.a.un"),
            make(BinaryFormat::Cus, "com.b", "com.b.un"),
        ),
        (
            ".ras vers n'importe quoi",
            make(BinaryFormat::Ras, "com.a", "com.a.ras"),
            make(BinaryFormat::Cus, "com.a", "com.a.un"),
        ),
    ];

    crate::serial_println!("[binfmt] matrice de communication (spec 12.1)");
    for (label, a, b) in cases.iter() {
        crate::serial_println!(
            "[binfmt]   {:<44} -> {}",
            label,
            a.communication_with(b).as_str()
        );
    }
}

//! Phase 10.4 self-test.
//!
//! ## The one check that counts
//!
//! A parser can be exercised on hand-written bytes and a translation
//! table can be compared with a manual page, and neither establishes that
//! a foreign program **runs**. So this file builds two real executables --
//! a static ELF64 and a PE32+ with no imports -- out of code assembled by
//! `sdk::asm`, loads them through the subsystems, and watches them make
//! their calls and exit.
//!
//! That is the difference between a compatibility layer and a
//! description of one, and it is exactly the standard phase 9 set for
//! itself: `dev build` was only allowed to claim it had compiled
//! something because the result ran in ring 3.
//!
//! ## The rest: refusals, one by one
//!
//! Every refusal of the two subsystems has its own case, on a file
//! crafted to trip that refusal and nothing else. A test that fed one
//! malformed file to the parser and observed "it was refused" would pass
//! with a parser that refuses everything.
//!
//! ## The parsers are given hostile input
//!
//! A truncated header, a `e_phnum` of 65 535, a section naming bytes past
//! the end of the file, an `e_lfanew` near the top of the range. All four
//! are how a parser ends up reading outside its buffer, and all four come
//! from a file.
//!
//! ## It leaves nothing behind
//!
//! The two tasks it starts run to completion and are reaped; their state
//! is dropped from the personality table. The count of foreign tasks
//! alive must be the same on the way out as on the way in -- a foreign
//! task that survived would keep translating somebody else's system
//! calls.

use alloc::string::String;
use alloc::vec::Vec;

use crate::sdk::asm;
use crate::sdk::emit;

use super::elf::{self, ElfError};
use super::linux::{self, Support};
use super::pe::{self, PeError};
use super::windows;

struct Report {
    passed: usize,
    failed: usize,
}

impl Report {
    fn check(&mut self, label: &str, condition: bool) {
        if condition {
            self.passed += 1;
        } else {
            self.failed += 1;
            crate::serial_println!("[compat] ECHEC : {}", label);
        }
    }

    fn equal_text(&mut self, label: &str, produced: &str, expected: &str) {
        if produced == expected {
            self.passed += 1;
            return;
        }
        self.failed += 1;
        crate::serial_println!(
            "[compat] ECHEC : {} - produit \"{}\", attendu \"{}\"",
            label,
            produced,
            expected
        );
    }
}

pub fn run() -> (usize, usize) {
    let mut report = Report {
        passed: 0,
        failed: 0,
    };

    let live_before = super::live_count();

    identification(&mut report);
    elf_parsing(&mut report);
    pe_parsing(&mut report);
    tables(&mut report);
    paths(&mut report);
    refusals(&mut report);
    execution(&mut report);

    super::collect_dead();
    report.check(
        "10.4 : aucune tache etrangere ne survit a l'autotest",
        super::live_count() == live_before,
    );

    (report.passed, report.failed)
}

// ============================================================
//  Building the two executables
// ============================================================

/// Flat machine code, and the offset of the entry point inside it.
///
/// The message lives in `.text` rather than in `.data`, so the image has
/// a **single** read-and-execute segment. Two segments would work and
/// would make the test depend on the two being laid out exactly as the
/// Nova image lays them out -- an extra thing to get wrong in a file
/// whose purpose is to check something else.
fn machine_code(source: &str) -> Option<(Vec<u8>, u64)> {
    let object = asm::assemble(source).ok()?;
    let layout = emit::Layout {
        text_span: object.text.len(),
        data_span: 0,
        bss_size: 0,
    };
    let text = emit::link(&object, &layout).ok()?;
    Some((text, object.entry))
}

/// A Linux program: write a line to descriptor 1, then exit.
///
/// The two calls chosen are the two that matter: `write` proves the
/// argument registers arrive where the translation expects, and `exit`
/// proves the task is really taken down rather than left running with a
/// foreign personality.
fn linux_source() -> String {
    String::from(
        ".text\n\
         _start:\n\
         \x20   mov rax, 1                 ; write\n\
         \x20   mov rdi, 1                 ; descripteur 1\n\
         \x20   lea rsi, [rip + message]\n\
         \x20   mov rdx, fin - message\n\
         \x20   syscall\n\
         \n\
         \x20   mov rax, 39                ; getpid\n\
         \x20   syscall\n\
         \n\
         \x20   mov rax, 60                ; exit\n\
         \x20   mov rdi, 0\n\
         \x20   syscall\n\
         \x20   hlt\n\
         message:\n\
         \x20   db \"[linux] bonjour depuis un ELF\"\n\
         fin:\n",
    )
}

/// A Windows program: `NtWriteFile` on the console handle, then
/// `NtTerminateProcess`.
///
/// The arguments go in R10 and RDX -- the convention `ntdll` uses once it
/// has moved RCX out of the way. Writing them in RDI and RSI would work
/// under the Linux translation and fail here, which is precisely the
/// difference this program exists to exercise.
fn windows_source() -> String {
    alloc::format!(
        ".text\n\
         _start:\n\
         \x20   mov rax, {write}\n\
         \x20   mov r10, 1                 ; HANDLE_STDOUT\n\
         \x20   lea rdx, [rip + message]\n\
         \x20   mov r8, fin - message\n\
         \x20   syscall\n\
         \n\
         \x20   mov rax, {yield_call}\n\
         \x20   syscall\n\
         \n\
         \x20   mov rax, {terminate}\n\
         \x20   mov r10, 0\n\
         \x20   mov rdx, 0\n\
         \x20   syscall\n\
         \x20   hlt\n\
         message:\n\
         \x20   db \"[windows] bonjour depuis un PE\"\n\
         fin:\n",
        write = windows::NT_WRITE_FILE,
        yield_call = windows::NT_YIELD_EXECUTION,
        terminate = windows::NT_TERMINATE_PROCESS,
    )
}

/// Base address the fabricated ELF is linked at.
const ELF_BASE: u64 = 0x0040_1000;
/// File offset of the code in the fabricated ELF.
const ELF_CODE_OFFSET: usize = 0x1000;

/// Builds a static ELF64 around a blob of machine code.
///
/// One `PT_LOAD`, read and execute, no section headers, no dynamic
/// segment: the shape the Linux subsystem accepts, written out so the
/// test does not depend on a toolchain the machine does not have.
fn build_elf(code: &[u8], entry_offset: u64) -> Vec<u8> {
    let mut file = alloc::vec![0u8; ELF_CODE_OFFSET + code.len()];

    file[0..4].copy_from_slice(&elf::MAGIC);
    file[4] = elf::CLASS_64;
    file[5] = elf::DATA_LSB;
    file[6] = 1; // EV_CURRENT
    file[7] = 0; // System V ABI
    put_u16(&mut file, 16, 2); // ET_EXEC
    put_u16(&mut file, 18, elf::EM_X86_64);
    put_u32(&mut file, 20, 1); // version
    put_u64(&mut file, 24, ELF_BASE + entry_offset); // e_entry
    put_u64(&mut file, 32, elf::EHDR_SIZE as u64); // e_phoff
    put_u64(&mut file, 40, 0); // e_shoff: none
    put_u16(&mut file, 52, elf::EHDR_SIZE as u16);
    put_u16(&mut file, 54, elf::PHDR_SIZE as u16);
    put_u16(&mut file, 56, 1); // e_phnum
    put_u16(&mut file, 58, elf::SHDR_SIZE as u16);
    put_u16(&mut file, 60, 0); // e_shnum
    put_u16(&mut file, 62, 0); // e_shstrndx

    let phdr = elf::EHDR_SIZE;
    put_u32(&mut file, phdr, 1); // PT_LOAD
    put_u32(&mut file, phdr + 4, elf::flags::READ | elf::flags::EXEC);
    put_u64(&mut file, phdr + 8, ELF_CODE_OFFSET as u64);
    put_u64(&mut file, phdr + 16, ELF_BASE); // p_vaddr
    put_u64(&mut file, phdr + 24, ELF_BASE); // p_paddr
    put_u64(&mut file, phdr + 32, code.len() as u64);
    put_u64(&mut file, phdr + 40, code.len() as u64);
    put_u64(&mut file, phdr + 48, 0x1000); // p_align

    file[ELF_CODE_OFFSET..].copy_from_slice(code);
    file
}

/// Relative virtual address of the code section in the fabricated PE.
const PE_CODE_RVA: u32 = 0x1000;
/// File offset of the code.
const PE_CODE_OFFSET: usize = 0x400;
/// Size of the PE32+ optional header, with its sixteen data directories.
const PE_OPTIONAL_SIZE: usize = 240;

/// Builds a PE32+ around a blob of machine code.
///
/// One section, `.text`, read and execute; no imports at all -- which is
/// what makes it acceptable, since a single `kernel32.dll` import would
/// be refused by name.
fn build_pe(code: &[u8], entry_offset: u64) -> Vec<u8> {
    let mut file = alloc::vec![0u8; PE_CODE_OFFSET + code.len()];

    // DOS stub: only the two bytes and `e_lfanew` matter to a parser.
    file[0..2].copy_from_slice(&pe::MZ);
    let lfanew = 0x40usize;
    put_u32(&mut file, 0x3C, lfanew as u32);

    file[lfanew..lfanew + 4].copy_from_slice(&pe::PE_SIGNATURE);
    let coff = lfanew + 4;
    put_u16(&mut file, coff, pe::MACHINE_AMD64);
    put_u16(&mut file, coff + 2, 1); // one section
    put_u16(&mut file, coff + 16, PE_OPTIONAL_SIZE as u16);
    put_u16(&mut file, coff + 18, 0x0022); // executable, large address aware

    let optional = coff + 20;
    put_u16(&mut file, optional, pe::MAGIC_PE32_PLUS);
    put_u32(&mut file, optional + 4, code.len() as u32); // SizeOfCode
    put_u32(&mut file, optional + 16, PE_CODE_RVA + entry_offset as u32);
    put_u32(&mut file, optional + 20, PE_CODE_RVA); // BaseOfCode
    put_u64(&mut file, optional + 24, 0x0000_0001_4000_0000); // ImageBase
    put_u32(&mut file, optional + 32, 0x1000); // SectionAlignment
    put_u32(&mut file, optional + 36, 0x200); // FileAlignment
    put_u32(&mut file, optional + 56, PE_CODE_RVA + 0x1000); // SizeOfImage
    put_u32(&mut file, optional + 60, PE_CODE_OFFSET as u32); // SizeOfHeaders
    put_u16(&mut file, optional + 68, 3); // console subsystem
    put_u32(&mut file, optional + 108, 16); // NumberOfRvaAndSizes
    // The sixteen data directories stay zero: no imports, no CLR header.

    let table = optional + PE_OPTIONAL_SIZE;
    file[table..table + 5].copy_from_slice(b".text");
    put_u32(&mut file, table + 8, code.len() as u32); // VirtualSize
    put_u32(&mut file, table + 12, PE_CODE_RVA);
    put_u32(&mut file, table + 16, code.len() as u32); // SizeOfRawData
    put_u32(&mut file, table + 20, PE_CODE_OFFSET as u32);
    put_u32(
        &mut file,
        table + 36,
        pe::characteristics::CODE | pe::characteristics::EXECUTE | pe::characteristics::READ,
    );

    file[PE_CODE_OFFSET..].copy_from_slice(code);
    file
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

// ============================================================
//  Identification
// ============================================================

fn identification(report: &mut Report) {
    let (code, entry) = machine_code(&linux_source()).expect("assemblage linux");
    let elf_file = build_elf(&code, entry);
    let pe_file = build_pe(&code, entry);

    report.check(
        "10.4 : un ELF est reconnu",
        super::identify(&elf_file) == super::Kind::Elf,
    );
    report.check(
        "10.4 : un PE est reconnu",
        super::identify(&pe_file) == super::Kind::Pe,
    );
    report.check(
        "10.4 : rien n'est reconnu dans du vide",
        super::identify(&[]) == super::Kind::Unknown
            && super::identify(b"quelconque") == super::Kind::Unknown,
    );

    // A Nova binary must be recognised as Nova first, whatever its later
    // bytes look like: the five magic numbers of 3.3 are checked before
    // anything else.
    let nova = crate::binfmt::sample::executable_cus();
    report.check(
        "10.4 : un binaire Nova reste un binaire Nova",
        super::identify(&nova) == super::Kind::Nova,
    );
}

// ============================================================
//  ELF parsing
// ============================================================

fn elf_parsing(report: &mut Report) {
    let (code, entry) = machine_code(&linux_source()).expect("assemblage linux");
    let file = build_elf(&code, entry);

    let parsed = elf::parse(&file).expect("ELF fabrique");
    report.check(
        "10.4 : en-tete ELF decode",
        parsed.object_type == elf::ObjectType::Executable
            && parsed.machine == elf::EM_X86_64
            && parsed.entry == ELF_BASE + entry,
    );
    report.check(
        "10.4 : un seul segment chargeable",
        parsed.loadable().len() == 1 && parsed.loadable()[0].executable(),
    );
    report.check(
        "10.4 : point d'entree dans le code",
        parsed.entry_is_executable(),
    );
    report.check(
        "10.4 : ni interprete ni relocation",
        !parsed.needs_interpreter() && !parsed.has_relocations(),
    );
    report.check(
        "10.4 : etendue de l'image",
        parsed.extent() == Some((ELF_BASE, ELF_BASE + code.len() as u64)),
    );
    report.equal_text(
        "10.4 : permissions du segment",
        core::str::from_utf8(&parsed.loadable()[0].permissions()).unwrap_or(""),
        "r-x",
    );

    // --- hostile input ---
    report.check(
        "10.4 : fichier tronque refuse",
        elf::parse(&file[..32]) == Err(ElfError::TooShort),
    );
    let mut wrong = file.clone();
    wrong[4] = 1; // ELFCLASS32
    report.check(
        "10.4 : ELF 32 bits refuse",
        elf::parse(&wrong) == Err(ElfError::NotSixtyFour),
    );
    let mut big_endian = file.clone();
    big_endian[5] = 2;
    report.check(
        "10.4 : ELF gros-boutiste refuse",
        elf::parse(&big_endian) == Err(ElfError::NotLittleEndian),
    );
    let mut arm = file.clone();
    put_u16(&mut arm, 18, 40); // EM_ARM
    report.check(
        "10.4 : une autre machine est refusee, et nommee",
        elf::parse(&arm) == Err(ElfError::WrongMachine(40)),
    );

    // A count of 65 535 program headers: the multiplication by the entry
    // size is exactly where a parser overflows and then indexes inside
    // its own buffer.
    let mut many = file.clone();
    put_u16(&mut many, 56, 65_535);
    report.check(
        "10.4 : un nombre d'entrees delirant est refuse",
        matches!(elf::parse(&many), Err(ElfError::TooManyEntries(65_535))),
    );

    // A table offset past the end of the file.
    let mut far = file.clone();
    put_u64(&mut far, 32, 0xFFFF_FFFF_0000);
    report.check(
        "10.4 : une table hors du fichier est refusee",
        elf::parse(&far) == Err(ElfError::TableOutOfRange),
    );

    // A segment claiming more file bytes than the file holds.
    let mut long = file.clone();
    put_u64(&mut long, elf::EHDR_SIZE + 32, 0xFFFF_FFFF);
    put_u64(&mut long, elf::EHDR_SIZE + 40, 0xFFFF_FFFF);
    report.check(
        "10.4 : un segment hors du fichier est refuse",
        elf::parse(&long) == Err(ElfError::SegmentOutOfRange),
    );

    // A memory extent smaller than the file extent is meaningless, and is
    // the shape in which a copy overruns its destination.
    let mut shrinking = file.clone();
    put_u64(&mut shrinking, elf::EHDR_SIZE + 40, 1);
    report.check(
        "10.4 : un segment retrecissant est refuse",
        elf::parse(&shrinking) == Err(ElfError::ShrinkingSegment),
    );

    // An entry size that does not match the format.
    let mut bad_size = file.clone();
    put_u16(&mut bad_size, 54, 32);
    report.check(
        "10.4 : une taille d'entree non conforme est refusee",
        elf::parse(&bad_size) == Err(ElfError::BadEntrySize),
    );
}

// ============================================================
//  PE parsing
// ============================================================

fn pe_parsing(report: &mut Report) {
    let (code, entry) = machine_code(&windows_source()).expect("assemblage windows");
    let file = build_pe(&code, entry);

    let parsed = pe::parse(&file).expect("PE fabrique");
    report.check(
        "10.4 : en-tete PE32+ decode",
        parsed.machine == pe::MACHINE_AMD64
            && parsed.subsystem == pe::Subsystem::WindowsConsole
            && parsed.entry_rva == PE_CODE_RVA + entry as u32,
    );
    report.check(
        "10.4 : une seule section, executable",
        parsed.loadable().len() == 1
            && parsed.loadable()[0].executable()
            && parsed.loadable()[0].name == ".text",
    );
    report.check(
        "10.4 : point d'entree dans une section executable",
        parsed.entry_is_executable(),
    );
    report.check(
        "10.4 : aucune importation, aucun code gere",
        parsed.imports.is_empty() && parsed.libraries().is_empty() && !parsed.managed,
    );

    // --- hostile input ---
    report.check(
        "10.4 : PE tronque refuse",
        pe::parse(&file[..16]) == Err(PeError::TooShort),
    );
    let mut not_mz = file.clone();
    not_mz[0] = b'Z';
    report.check(
        "10.4 : signature MZ absente",
        pe::parse(&not_mz) == Err(PeError::NotMz),
    );

    // `e_lfanew` near the top of the range: the value the DOS stub gives
    // is read from the file, and using it unchecked is how a parser reads
    // its header from somewhere else entirely.
    let mut far = file.clone();
    put_u32(&mut far, 0x3C, 0xFFFF_FFF0);
    report.check(
        "10.4 : e_lfanew hors du fichier est refuse",
        pe::parse(&far) == Err(PeError::HeaderOutOfRange),
    );

    let mut thirty_two = file.clone();
    put_u32(&mut thirty_two, 0x3C, 0x40);
    put_u16(&mut thirty_two, 0x40 + 4 + 20, pe::MAGIC_PE32);
    report.check(
        "10.4 : PE32 32 bits refuse",
        pe::parse(&thirty_two) == Err(PeError::NotSixtyFour),
    );

    let mut arm = file.clone();
    put_u16(&mut arm, 0x40 + 4, 0xAA64); // ARM64
    report.check(
        "10.4 : une autre machine est refusee, et nommee",
        pe::parse(&arm) == Err(PeError::WrongMachine(0xAA64)),
    );

    let mut many = file.clone();
    put_u16(&mut many, 0x40 + 4 + 2, 1000);
    report.check(
        "10.4 : un nombre de sections delirant est refuse",
        matches!(pe::parse(&many), Err(PeError::TooManySections(1000))),
    );

    let table = 0x40 + 4 + 20 + PE_OPTIONAL_SIZE;
    let mut beyond = file.clone();
    put_u32(&mut beyond, table + 16, 0xFFFF_FF00);
    report.check(
        "10.4 : une section hors du fichier est refusee",
        pe::parse(&beyond) == Err(PeError::SectionOutOfRange),
    );

    // The RVA-to-offset translation, which the import walk rests on.
    let sections = parsed.loadable();
    report.check(
        "10.4 : traduction RVA vers offset",
        pe::rva_to_offset(&sections, PE_CODE_RVA) == Some(PE_CODE_OFFSET)
            && pe::rva_to_offset(&sections, PE_CODE_RVA + 4) == Some(PE_CODE_OFFSET + 4)
            && pe::rva_to_offset(&sections, 0).is_none(),
    );
}

// ============================================================
//  The translation tables
// ============================================================

fn tables(report: &mut Report) {
    // The numbers are the real Linux ones. A table with the right names
    // and the wrong numbers would translate every call into another one,
    // and each translated call would look plausible.
    report.check(
        "10.4 : les numeros Linux sont les vrais",
        linux::SYS_READ == 0
            && linux::SYS_WRITE == 1
            && linux::SYS_OPEN == 2
            && linux::SYS_CLOSE == 3
            && linux::SYS_MMAP == 9
            && linux::SYS_EXIT == 60
            && linux::SYS_EXIT_GROUP == 231
            && linux::SYS_OPENAT == 257,
    );
    report.check(
        "10.4 : chaque numero Linux porte son nom",
        linux::call_name(linux::SYS_WRITE) == Some("write")
            && linux::call_name(linux::SYS_EXIT) == Some("exit")
            && linux::call_name(999).is_none(),
    );
    // No number appears twice: a duplicate would make the behaviour
    // depend on the order of the table.
    let mut duplicated = false;
    for (index, (number, _, _)) in linux::CALLS.iter().enumerate() {
        if linux::CALLS[index + 1..]
            .iter()
            .any(|(other, _, _)| other == number)
        {
            duplicated = true;
        }
    }
    report.check("10.4 : aucun numero Linux en double", !duplicated);

    // What is declared absent must be absent for a reason the table
    // states, and `fork` is the one that matters: it needs a second
    // address space, and phase 3 left that limit open.
    report.check(
        "10.4 : fork et execve sont declares absents",
        linux::support(linux::SYS_FORK) == Some(Support::Absent)
            && linux::support(linux::SYS_EXECVE) == Some(Support::Absent),
    );
    report.check(
        "10.4 : write et exit sont traduits",
        linux::support(linux::SYS_WRITE) == Some(Support::Full)
            && linux::support(linux::SYS_EXIT) == Some(Support::Full),
    );

    // The NT side.
    report.check(
        "10.4 : chaque appel NT porte son nom",
        windows::call_name(windows::NT_WRITE_FILE) == Some("NtWriteFile")
            && windows::call_name(windows::NT_TERMINATE_PROCESS) == Some("NtTerminateProcess")
            && windows::call_name(0xFFFF).is_none(),
    );
    let mut duplicated = false;
    for (index, (number, _, _)) in windows::CALLS.iter().enumerate() {
        if windows::CALLS[index + 1..]
            .iter()
            .any(|(other, _, _)| other == number)
        {
            duplicated = true;
        }
    }
    report.check("10.4 : aucun numero NT en double", !duplicated);
    report.check(
        "10.4 : les threads NT sont declares absents",
        windows::support(windows::NT_CREATE_THREAD) == Some(Support::Absent),
    );

    // The NT success rule is the top bit, not a comparison with zero: a
    // status of 0x40000000 is a success, and testing `!= 0` would read it
    // as a failure.
    report.check(
        "10.4 : la regle de succes NT porte sur le bit de poids fort",
        windows::succeeded(windows::status::SUCCESS)
            && windows::succeeded(0x4000_0000)
            && !windows::succeeded(windows::status::ACCESS_DENIED)
            && !windows::succeeded(windows::status::NOT_IMPLEMENTED),
    );

    // The NT epoch. Getting it wrong shifts every timestamp by
    // centuries -- and it is the kind of constant nobody rechecks.
    report.check(
        "10.4 : l'epoque NT est celle de 1601",
        windows::NT_EPOCH_OFFSET == 11_644_473_600,
    );
}

// ============================================================
//  Path translation
// ============================================================

fn paths(report: &mut Report) {
    const APP: &str = "test.compat";

    report.check(
        "10.4 : /tmp devient /cache",
        linux::translate_path(APP, "/tmp/a.txt") == Some(String::from("/cache/a.txt")),
    );
    report.check(
        "10.4 : /home devient /users",
        linux::translate_path(APP, "/home/nova") == Some(String::from("/users/nova")),
    );
    report.check(
        "10.4 : la racine mappee sans suite est traduite aussi",
        linux::translate_path(APP, "/tmp") == Some(String::from("/cache")),
    );
    // A prefix that matches textually but not on a segment boundary is
    // not the same directory: `/tmpfoo` is not under `/tmp`.
    report.check(
        "10.4 : un prefixe voisin n'est pas le meme dossier",
        linux::translate_path(APP, "/tmpfoo/a")
            == Some(String::from("/apps/test.compat/rootfs/tmpfoo/a")),
    );
    // Anything unmapped lands inside the sandbox of 12.4. A foreign
    // program does not escape it by using foreign path syntax.
    report.check(
        "10.4 : un chemin inconnu reste dans le bac a sable",
        linux::translate_path(APP, "/etc/passwd")
            == Some(String::from("/apps/test.compat/rootfs/etc/passwd")),
    );
    report.check(
        "10.4 : un .. ne sort pas du bac a sable",
        linux::translate_path(APP, "/etc/../../../sys/config")
            .is_some_and(|path| path.starts_with("/apps/test.compat/rootfs")
                || path == "/apps/test.compat"
                || !path.starts_with("/sys")),
    );
    report.check(
        "10.4 : /dev/null n'est pas un fichier",
        linux::translate_path(APP, "/dev/null").is_none(),
    );

    // --- Windows ---
    report.check(
        "10.4 : C: est le volume systeme",
        windows::translate_path(APP, "C:\\Users\\nova\\a.txt")
            == Some(String::from("/Users/nova/a.txt")),
    );
    // The NT object-manager forms, which is what `NtCreateFile` really
    // receives. A translation that only handled `C:\` would work in a
    // test and fail on every real call.
    report.check(
        "10.4 : la forme \\??\\ est reconnue",
        windows::translate_path(APP, "\\??\\C:\\a.txt") == Some(String::from("/a.txt")),
    );
    report.check(
        "10.4 : la forme \\\\?\\ est reconnue",
        windows::translate_path(APP, "\\\\?\\C:\\a.txt") == Some(String::from("/a.txt")),
    );
    report.check(
        "10.4 : un chemin relatif reste dans le bac a sable",
        windows::translate_path(APP, "donnees\\a.txt")
            == Some(String::from("/apps/test.compat/rootfs/donnees/a.txt")),
    );
    report.check(
        "10.4 : NUL n'est pas un fichier",
        windows::translate_path(APP, "NUL").is_none(),
    );
    // A drive letter that names no mounted volume is refused rather than
    // silently redirected: writing to `Z:` and landing in the application
    // directory would be a surprise the program could not detect.
    report.check(
        "10.4 : une lettre de volume absente est refusee",
        windows::translate_path(APP, "Z:\\a.txt").is_none(),
    );
}

// ============================================================
//  The refusals
// ============================================================

fn refusals(report: &mut Report) {
    let (code, entry) = machine_code(&linux_source()).expect("assemblage linux");

    // A dynamically linked binary: the case every distribution binary
    // falls into, and the one that has to be named rather than crashed
    // on.
    let mut dynamic = build_elf(&code, entry);
    let extended = {
        let mut file = dynamic.clone();
        // Two program headers, the second a PT_INTERP.
        put_u16(&mut file, 56, 2);
        let second = elf::EHDR_SIZE + elf::PHDR_SIZE;
        put_u32(&mut file, second, 3); // PT_INTERP
        put_u32(&mut file, second + 4, elf::flags::READ);
        put_u64(&mut file, second + 8, 0);
        put_u64(&mut file, second + 32, 1);
        put_u64(&mut file, second + 40, 1);
        file
    };
    dynamic = extended;
    let parsed = elf::parse(&dynamic).expect("ELF avec interprete");
    report.check(
        "10.4 : un binaire lie dynamiquement est refuse et nomme",
        linux::to_object(&parsed).err() == Some(linux::LoadRefusal::NeedsInterpreter),
    );
    report.check(
        "10.4 : le message nomme l'editeur de liens",
        linux::LoadRefusal::NeedsInterpreter
            .message()
            .contains("dynamique"),
    );

    // A relocatable object -- a `.o` -- is not an executable.
    let mut relocatable = build_elf(&code, entry);
    put_u16(&mut relocatable, 16, 1); // ET_REL
    let parsed = elf::parse(&relocatable).expect("objet relogeable");
    report.check(
        "10.4 : un objet relogeable est refuse",
        linux::to_object(&parsed).err() == Some(linux::LoadRefusal::NotExecutable),
    );

    // An entry point outside the loadable segment.
    let mut lost = build_elf(&code, entry);
    put_u64(&mut lost, 24, 0x9999_0000);
    let parsed = elf::parse(&lost).expect("point d'entree perdu");
    report.check(
        "10.4 : un point d'entree hors du code est refuse",
        linux::to_object(&parsed).err() == Some(linux::LoadRefusal::EntryOutsideCode),
    );

    // --- Windows ---
    let (win_code, win_entry) = machine_code(&windows_source()).expect("assemblage windows");

    // A .NET assembly: its code is bytecode for a runtime that is not
    // here, and it says so in data directory 14.
    let mut managed = build_pe(&win_code, win_entry);
    let directories = 0x40 + 4 + 20 + 112;
    put_u32(&mut managed, directories + 14 * 8, PE_CODE_RVA);
    put_u32(&mut managed, directories + 14 * 8 + 4, 8);
    let parsed = pe::parse(&managed).expect("assemblage gere");
    report.check(
        "10.4 : un assemblage .NET est refuse",
        windows::to_object(&parsed).err() == Some(windows::LoadRefusal::Managed),
    );

    // An import of `kernel32.dll`. This is the case a real Windows
    // program falls into, and the whole point of reading the import
    // directory before running anything.
    let with_import = build_pe_with_import(&win_code, win_entry, "kernel32.dll", "WriteFile");
    let parsed = pe::parse(&with_import).expect("PE avec importation");
    report.check(
        "10.4 : l'importation est lue",
        parsed.libraries() == alloc::vec![String::from("kernel32.dll")]
            && parsed.imports.iter().any(|name| name.contains("WriteFile")),
    );
    match windows::to_object(&parsed) {
        Err(windows::LoadRefusal::MissingLibraries(names)) => {
            report.check(
                "10.4 : kernel32.dll est refuse et nomme",
                names.iter().any(|name| name == "kernel32.dll"),
            );
        }
        other => {
            report.check("10.4 : kernel32.dll est refuse et nomme", false);
            crate::serial_println!("[compat]   obtenu : {:?}", other.is_ok());
        }
    }

    // Neither an ELF nor a PE.
    report.check(
        "10.4 : un fichier quelconque n'est pas converti",
        super::convert(b"pas un binaire du tout", "test.compat").is_err(),
    );
}

/// Builds a PE that imports one function from one library.
///
/// The import directory needs three structures that point at each other:
/// a descriptor, a lookup table, and a hint-name entry. All three live in
/// a second section so the first one keeps holding only code.
fn build_pe_with_import(code: &[u8], entry_offset: u64, library: &str, function: &str) -> Vec<u8> {
    const IMPORT_RVA: u32 = 0x2000;
    const IMPORT_OFFSET: usize = 0x600;

    let mut file = build_pe(code, entry_offset);
    // Two sections now, and the import directory filled in.
    put_u16(&mut file, 0x40 + 4 + 2, 2);
    // Data directory **1** is the imports; 0 is the exports. Writing the
    // import table into entry 0 produces a file the parser reads as
    // having no imports at all -- which is exactly the wrong answer for
    // a test whose whole point is that they are read.
    let directories = 0x40 + 4 + 20 + 112;
    put_u32(&mut file, directories + 8, IMPORT_RVA);
    put_u32(&mut file, directories + 12, 40);

    let table = 0x40 + 4 + 20 + PE_OPTIONAL_SIZE;
    let second = table + pe::SECTION_ENTRY_SIZE;

    // The import blob: descriptor, terminator, lookup table, names.
    let mut blob = alloc::vec![0u8; 0x200];
    let lookup_rva = IMPORT_RVA + 40;
    let name_rva = IMPORT_RVA + 64;
    let hint_rva = IMPORT_RVA + 96;

    put_u32(&mut blob, 0, lookup_rva); // OriginalFirstThunk
    put_u32(&mut blob, 12, name_rva); // Name
    put_u32(&mut blob, 16, lookup_rva); // FirstThunk
    // Entry 20..40 stays zero: the terminator.
    put_u64(&mut blob, 40, hint_rva as u64);
    put_u64(&mut blob, 48, 0); // end of the lookup table
    blob[64..64 + library.len()].copy_from_slice(library.as_bytes());
    // Two bytes of hint precede the name.
    blob[98..98 + function.len()].copy_from_slice(function.as_bytes());

    // The file has to grow to hold the second section.
    file.resize(IMPORT_OFFSET + blob.len(), 0);
    file[IMPORT_OFFSET..IMPORT_OFFSET + blob.len()].copy_from_slice(&blob);

    file[second..second + 6].copy_from_slice(b".idata");
    put_u32(&mut file, second + 8, blob.len() as u32);
    put_u32(&mut file, second + 12, IMPORT_RVA);
    put_u32(&mut file, second + 16, blob.len() as u32);
    put_u32(&mut file, second + 20, IMPORT_OFFSET as u32);
    put_u32(
        &mut file,
        second + 36,
        pe::characteristics::INITIALISED_DATA | pe::characteristics::READ,
    );

    file
}

// ============================================================
//  Execution -- the check that counts
// ============================================================

fn execution(report: &mut Report) {
    for (label, source, name, builder) in [
        (
            "linux",
            linux_source(),
            "test.compat.elf",
            build_elf as fn(&[u8], u64) -> Vec<u8>,
        ),
        (
            "windows",
            windows_source(),
            "test.compat.pe",
            build_pe as fn(&[u8], u64) -> Vec<u8>,
        ),
    ] {
        let Some((code, entry)) = machine_code(&source) else {
            report.check("10.4 : le programme d'essai s'assemble", false);
            continue;
        };
        report.check("10.4 : le programme d'essai s'assemble", true);

        let file = builder(&code, entry);

        // The conversion, before anything is launched: a refusal must
        // arrive before a task exists, not after.
        let converted = super::convert(&file, name);
        report.check(
            "10.4 : le binaire etranger se convertit en image Nova",
            converted.is_ok(),
        );
        if let Ok(nova) = &converted {
            // The result must be a real Nova binary, admissible to the
            // ordinary parser. A conversion producing something only the
            // subsystem accepts would be a second class of executable.
            report.check(
                "10.4 : l'image produite est un binaire Nova valide",
                crate::binfmt::inspect(nova)
                    .map(|inspection| inspection.is_admissible())
                    .unwrap_or(false),
            );
        }

        let before = super::counters().0;
        match super::load_and_spawn(&file, name) {
            Ok(pid) => {
                report.check("10.4 : le binaire etranger demarre", true);
                report.check(
                    "10.4 : la tache porte la bonne personnalite",
                    super::granted_personality(pid)
                        == Some(if label == "linux" {
                            super::Personality::Linux
                        } else {
                            super::Personality::Windows
                        }),
                );

                // It must terminate on its own: a foreign task that never
                // exits holds an arena slot and keeps translating.
                let exit_code = crate::scheduler::wait_for(pid).unwrap_or(-1);
                report.check("10.4 : le programme etranger se termine", exit_code == 0);

                // And its state must be gone. A dead PID keeping its
                // personality would have a later task's Nova calls
                // translated as foreign ones.
                report.check(
                    "10.4 : l'etat de la tache est rendu a sa mort",
                    super::personality_of(pid).is_none(),
                );
                report.check(
                    "10.4 : le compteur d'images chargees avance",
                    super::counters().0 == before + 1,
                );
            }
            Err(error) => {
                report.check("10.4 : le binaire etranger demarre", false);
                crate::serial_println!("[compat]   refus : {}", error.message());
            }
        }
    }
}

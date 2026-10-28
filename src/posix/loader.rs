//! Loading an ELF executable into a fresh address space.
//!
//! What the kernel of a Linux system does at `execve`, and no more:
//!
//! 1. map every `PT_LOAD` segment of the executable, as regions of the
//!    space backed by the file -- nothing is read yet, the pages come in
//!    on first touch;
//! 2. if the executable names an interpreter (`PT_INTERP`, the dynamic
//!    linker `ld-musl-x86_64.so.1`), map it the same way, anywhere;
//! 3. build the initial stack -- `argc`, `argv`, `envp`, and the
//!    auxiliary vector through which the dynamic linker finds the
//!    program's headers, its entry point and sixteen random bytes;
//! 4. start at the interpreter's entry point, or the program's.
//!
//! Relocations, shared libraries, TLS: all of that is the dynamic
//! linker's work, done in ring 3 with `open` and `mmap` like any program.
//! The kernel's share is the four steps above, which is why a real
//! `ld.so` can run here unmodified.
//!
//! ## A linked image (spec 3.5)
//!
//! A program of the Nova interface (`Abi::Nova`) may be a `.cus` whose
//! payload is the ELF: the header is parsed and checked like any Nova
//! binary's (format, signature rules, permissions), the payload's digest
//! is recomputed by streaming it from the file, and the ELF is mapped
//! from the payload's offset -- a page boundary, so that its segments map
//! straight from the file's pages. Its interpreter, whatever path the ELF
//! names, is the dynamic linker of the Nova C library (`NOVA_LDSO`): the
//! program then speaks only the Nova interface, down to its loader.
//!
//! ## The page after a segment's file bytes
//!
//! A segment usually ends mid-page, and its `.bss` starts there. The page
//! is backed by the file, which holds other bytes past the segment -- the
//! start of the next section, the symbol table. Those must read as zero:
//! `SegmentPager` cuts every fill at the segment's end in the file.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::errno::*;
use super::vfs;
use super::Abi;
use crate::vm::{Backing, PAGE, Pager, Prot, Space, UserRegs};

/// The Nova C library: its dynamic linker is the library itself, as in
/// musl (spec 6.7, 3.5).
pub const NOVA_LDSO: &str = "/sys/runtimes/c/libc.so";

/// Where a position-independent executable goes (as on Linux).
const PIE_BASE: u64 = 0x5555_5555_4000;
/// Top of the main thread's stack, and its reserved size.
pub const STACK_TOP: u64 = 0x7FFF_FFFF_F000;
pub const STACK_SIZE: u64 = 8 * 1024 * 1024;

const PT_LOAD: u32 = 1;
const PT_INTERP: u32 = 3;
const PT_PHDR: u32 = 6;

const AT_NULL: u64 = 0;
const AT_PHDR: u64 = 3;
const AT_PHENT: u64 = 4;
const AT_PHNUM: u64 = 5;
const AT_PAGESZ: u64 = 6;
const AT_BASE: u64 = 7;
const AT_FLAGS: u64 = 8;
const AT_ENTRY: u64 = 9;
const AT_UID: u64 = 11;
const AT_EUID: u64 = 12;
const AT_GID: u64 = 13;
const AT_EGID: u64 = 14;
const AT_PLATFORM: u64 = 15;
const AT_HWCAP: u64 = 16;
const AT_CLKTCK: u64 = 17;
const AT_SECURE: u64 = 23;
const AT_RANDOM: u64 = 25;
const AT_HWCAP2: u64 = 26;
const AT_EXECFN: u64 = 31;
const AT_MINSIGSTKSZ: u64 = 51;

pub struct Image {
    pub regs: UserRegs,
    /// Start of the program break, just past the executable.
    pub brk: u64,
    pub exe: String,
    /// The binary type a linked image declares (spec 3.1), for the
    /// scheduler; `None` for a bare ELF.
    pub format: Option<crate::binfmt::header::BinaryFormat>,
}

struct Phdr {
    kind: u32,
    flags: u32,
    offset: u64,
    vaddr: u64,
    filesz: u64,
    memsz: u64,
}

struct Elf {
    kind: u16,
    entry: u64,
    phoff: u64,
    phnum: u16,
    phdrs: Vec<Phdr>,
    /// Where the ELF starts in its file: 0, or a linked image's payload.
    at: u64,
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(v)
}

/// Reads and checks the ELF header and program headers, field by field.
fn parse(inode: &Arc<dyn vfs::Inode>, at: u64) -> LxResult<Elf> {
    let mut head = [0u8; 64];
    if inode.read_at(at, &mut head)? < 64 || &head[0..4] != b"\x7fELF" {
        return Err(ENOEXEC);
    }
    // 64-bit, little endian, version 1, x86-64.
    if head[4] != 2 || head[5] != 1 || head[6] != 1 || u16_at(&head, 18) != 62 {
        return Err(ENOEXEC);
    }
    let kind = u16_at(&head, 16);
    if kind != 2 && kind != 3 {
        return Err(ENOEXEC);
    }
    let phoff = u64_at(&head, 32);
    let phentsize = u16_at(&head, 54) as usize;
    let phnum = u16_at(&head, 56);
    if phentsize != 56 || phnum == 0 || phnum > 128 {
        return Err(ENOEXEC);
    }
    let mut table = alloc::vec![0u8; phentsize * phnum as usize];
    if inode.read_at(at + phoff, &mut table)? != table.len() {
        return Err(ENOEXEC);
    }
    let phdrs = table
        .chunks(phentsize)
        .map(|p| Phdr {
            kind: u32_at(p, 0),
            flags: u32_at(p, 4),
            offset: u64_at(p, 8),
            vaddr: u64_at(p, 16),
            filesz: u64_at(p, 32),
            memsz: u64_at(p, 40),
        })
        .collect();
    Ok(Elf { kind, entry: u64_at(&head, 24), phoff, phnum, phdrs, at })
}

/// The file's pages, cut at the end of one segment's file bytes.
struct SegmentPager {
    inner: Arc<dyn Pager>,
    /// End of the segment's bytes, as an offset in the file.
    file_end: u64,
}

impl Pager for SegmentPager {
    fn fill(&self, offset: u64, page: &mut [u8]) -> bool {
        if offset >= self.file_end {
            return true;
        }
        if !self.inner.fill(offset, page) {
            return false;
        }
        let keep = (self.file_end - offset).min(page.len() as u64) as usize;
        page[keep..].fill(0);
        true
    }
}

fn prot_of(flags: u32) -> Prot {
    let mut p = 0;
    if flags & 4 != 0 {
        p |= Prot::READ;
    }
    if flags & 2 != 0 {
        p |= Prot::WRITE;
    }
    if flags & 1 != 0 {
        p |= Prot::EXEC;
    }
    Prot(p)
}

/// Maps the `PT_LOAD` segments of `elf` at `base` (0 for a fixed-address
/// executable). Returns the end of the highest segment.
fn map_segments(space: &Space, elf: &Elf, inode: &Arc<dyn vfs::Inode>, base: u64, name: &str) -> LxResult<u64> {
    let pager = inode.clone().pager().ok_or(EACCES)?;
    let mut highest = 0;
    for ph in elf.phdrs.iter().filter(|p| p.kind == PT_LOAD) {
        if ph.memsz == 0 {
            continue;
        }
        if ph.offset % PAGE != ph.vaddr % PAGE {
            return Err(ENOEXEC);
        }
        let start = base + ph.vaddr;
        let page_start = start & !(PAGE - 1);
        let file_page = elf.at + (ph.offset & !(PAGE - 1));
        let file_end_va = start + ph.filesz;
        let mem_end = start + ph.memsz;
        let prot = prot_of(ph.flags);
        let file_part_end = (file_end_va + PAGE - 1) & !(PAGE - 1);
        if ph.filesz > 0 {
            let backing = Backing::File {
                pager: Arc::new(SegmentPager { inner: pager.clone(), file_end: elf.at + ph.offset + ph.filesz }),
                offset: file_page,
                shared: false,
            };
            space.map(page_start, file_part_end - page_start, prot, backing, true, name).map_err(|_| ENOMEM)?;
        }
        let bss_start = if ph.filesz > 0 { file_part_end } else { page_start };
        let bss_end = (mem_end + PAGE - 1) & !(PAGE - 1);
        if bss_end > bss_start {
            space.map(bss_start, bss_end - bss_start, prot, Backing::Anon, true, name).map_err(|_| ENOMEM)?;
        }
        highest = highest.max(mem_end);
    }
    Ok(highest)
}

/// Span of the loadable segments, for choosing a base.
fn span(elf: &Elf) -> (u64, u64) {
    let loads = elf.phdrs.iter().filter(|p| p.kind == PT_LOAD);
    let low = loads.clone().map(|p| p.vaddr & !(PAGE - 1)).min().unwrap_or(0);
    let high = loads.map(|p| p.vaddr + p.memsz).max().unwrap_or(0);
    (low, (high + PAGE - 1) & !(PAGE - 1))
}

/// Address of the program headers once loaded.
fn phdr_address(elf: &Elf, base: u64) -> u64 {
    if let Some(p) = elf.phdrs.iter().find(|p| p.kind == PT_PHDR) {
        return base + p.vaddr;
    }
    for p in elf.phdrs.iter().filter(|p| p.kind == PT_LOAD) {
        if elf.phoff >= p.offset && elf.phoff < p.offset + p.filesz {
            return base + p.vaddr + (elf.phoff - p.offset);
        }
    }
    0
}

/// Loads `path` into `space` and prepares its first thread.
/// The header of a linked image (spec 3.5), checked: returns where its
/// ELF starts and the type it declares, `None` for a file that is not a
/// Nova binary at all.
fn linked_image(inode: &Arc<dyn vfs::Inode>, path: &str) -> LxResult<Option<(u64, crate::binfmt::header::BinaryFormat)>> {
    use crate::binfmt::{self, header::flags};
    let mut head = alloc::vec![0u8; PAGE as usize];
    let n = inode.read_at(0, &mut head)?;
    head.truncate(n);
    if binfmt::identify(&head).is_none() {
        return Ok(None);
    }
    let size = inode.stat().size as usize;
    let refuse = |why: &str| {
        crate::serial_println!("[natif] {} refuse : {}", path, why);
        Err(ENOEXEC)
    };
    let header = match binfmt::parse_prefix(&head, size) {
        Ok(header) => header,
        Err(error) => return refuse(error.message()),
    };
    if !header.has_flag(flags::LINKED_IMAGE) || header.abi_version != 2 {
        return refuse("pas une image liee (drapeau IMAGE_LIEE, ABI 2)");
    }
    if let Some(violation) = binfmt::check_rules(&header).first() {
        return refuse(violation.message());
    }
    let (offset, length) = (header.payload_offset as u64, header.payload_size as u64);
    if offset % PAGE != 0 || length == 0 {
        return refuse("charge utile hors d'une frontiere de page");
    }
    // The digest, streamed: the payload is megabytes, the heap the whole
    // system's.
    let mut hasher = crate::binfmt::sha256::Sha256::default();
    let mut chunk = super::files::user_buffer(64 * 1024)?;
    let mut done = 0u64;
    while done < length {
        let want = ((length - done) as usize).min(chunk.len());
        let got = inode.read_at(offset + done, &mut chunk[..want])?;
        if got == 0 {
            return refuse("charge utile tronquee");
        }
        hasher.update(&chunk[..got]);
        done += got as u64;
    }
    if !crate::binfmt::sha256::equal(&hasher.finish(), &header.payload_hash) {
        return refuse("empreinte de la charge utile fausse");
    }
    Ok(Some((offset, header.format())))
}

pub fn load(space: &Arc<Space>, path: &str, argv: &[&str], envp: &[&str], abi: Abi) -> LxResult<Image> {
    let found = vfs::walk("/", path, true)?;
    if found.inode.stat().kind() != vfs::Kind::File {
        return Err(EACCES);
    }
    let exe_name = path.rsplit('/').next().unwrap_or(path).trim_end_matches(".cus").to_string();
    // A linked image is a program of the Nova interface only; the Linux
    // personality runs ELF files, as Linux does.
    let (at, format) = match abi {
        Abi::Nova => match linked_image(&found.inode, &found.path)? {
            Some((at, format)) => (at, Some(format)),
            None => (0, None),
        },
        Abi::Linux => (0, None),
    };
    let elf = parse(&found.inode, at)?;

    // --- the executable ---
    let base = if elf.kind == 3 {
        let (low, high) = span(&elf);
        space.map(PIE_BASE, high - low, Prot::NONE, Backing::Anon, false, "").map_err(|_| ENOMEM).and_then(|b| {
            // Reserve, then map over the reservation: the base is where the
            // span fits, segments go at their offsets from it.
            space.unmap(b, high - low).map_err(|_| ENOMEM)?;
            Ok(b - low)
        })?
    } else {
        0
    };
    let end = map_segments(space, &elf, &found.inode, base, &exe_name)?;
    let entry = base + elf.entry;

    // --- the interpreter ---
    let interp = elf.phdrs.iter().find(|p| p.kind == PT_INTERP);
    let (start_rip, interp_base) = match interp {
        Some(p) => {
            let mut raw = alloc::vec![0u8; p.filesz.min(4096) as usize];
            found.inode.read_at(at + p.offset, &mut raw)?;
            let named = String::from_utf8_lossy(raw.split(|b| *b == 0).next().unwrap_or(&[])).to_string();
            // Whatever the ELF names (`ld-musl-x86_64.so.1` for a program
            // built with Alpine's toolchain), a Nova program gets the Nova
            // library's linker, and every library it loads resolves `libc`
            // to it (musl's linker answers every `libc.*` name itself).
            let interp_path = match abi {
                Abi::Nova => String::from(NOVA_LDSO),
                Abi::Linux => named,
            };
            let interp_found = vfs::walk("/", &interp_path, true)?;
            let interp_elf = parse(&interp_found.inode, 0)?;
            if interp_elf.kind != 3 {
                return Err(ENOEXEC);
            }
            let (low, high) = span(&interp_elf);
            let at = space.map(0, high - low, Prot::NONE, Backing::Anon, false, "").map_err(|_| ENOMEM)?;
            space.unmap(at, high - low).map_err(|_| ENOMEM)?;
            let ibase = at - low;
            let iname = interp_path.rsplit('/').next().unwrap_or("ld.so").to_string();
            map_segments(space, &interp_elf, &interp_found.inode, ibase, &iname)?;
            (ibase + interp_elf.entry, ibase)
        }
        None => (entry, 0),
    };

    // --- the stack ---
    space
        .map(STACK_TOP - STACK_SIZE, STACK_SIZE, Prot::RW, Backing::Anon, true, "[stack]")
        .map_err(|_| ENOMEM)?;
    let rsp = build_stack(space, &found.path.trim_end_matches(".cus").to_string(), argv, envp, &[
        (AT_PHDR, phdr_address(&elf, base)),
        (AT_PHENT, 56),
        (AT_PHNUM, elf.phnum as u64),
        (AT_PAGESZ, PAGE),
        (AT_BASE, interp_base),
        (AT_FLAGS, 0),
        (AT_ENTRY, entry),
        (AT_UID, 0),
        (AT_EUID, 0),
        (AT_GID, 0),
        (AT_EGID, 0),
        (AT_HWCAP, hwcap()),
        (AT_HWCAP2, 0),
        (AT_CLKTCK, crate::TIMER_HZ as u64),
        (AT_SECURE, 0),
        (AT_MINSIGSTKSZ, 2048),
    ])?;

    let brk = (end + PAGE - 1) & !(PAGE - 1);
    crate::serial_println!(
        "[{}] {} charge - base 0x{:x}, entree 0x{:x}{}",
        if abi == Abi::Nova { "natif" } else { "linux" },
        found.path,
        base,
        start_rip,
        if interp_base != 0 { format!(", interprete a 0x{:x}", interp_base) } else { String::new() }
    );
    Ok(Image {
        regs: UserRegs { rip: start_rip, rsp, rflags: 0x202, ..Default::default() },
        brk,
        exe: found.path,
        format,
    })
}

fn hwcap() -> u64 {
    let edx: u32;
    unsafe {
        core::arch::asm!(
            "mov {t}, rbx", "cpuid", "mov rbx, {t}",
            t = out(reg) _,
            inout("eax") 1u32 => _,
            inout("ecx") 0u32 => _,
            out("edx") edx,
            options(nomem, nostack, preserves_flags)
        );
    }
    edx as u64
}

/// Lays out the initial stack, top-down:
///
/// ```text
///   strings: execfn, argv[], envp[], "x86_64", 16 random bytes
///   (padding to 16)
///   auxv pairs ... AT_NULL
///   envp pointers ... NULL
///   argv pointers ... NULL
///   argc                                  <- RSP
/// ```
fn build_stack(space: &Space, execfn: &str, argv: &[&str], envp: &[&str], auxv: &[(u64, u64)]) -> LxResult<u64> {
    let mut sp = STACK_TOP;
    let mut push_bytes = |bytes: &[u8]| -> LxResult<u64> {
        sp -= bytes.len() as u64;
        space.poke(sp, bytes).map_err(|_| EFAULT)?;
        Ok(sp)
    };
    let cstr = |s: &str| -> Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    };
    let execfn_at = push_bytes(&cstr(execfn))?;
    let mut argv_at = Vec::new();
    for a in argv {
        argv_at.push(push_bytes(&cstr(a))?);
    }
    let mut envp_at = Vec::new();
    for e in envp {
        envp_at.push(push_bytes(&cstr(e))?);
    }
    let platform_at = push_bytes(b"x86_64\0")?;
    let mut random = [0u8; 16];
    super::misc::fill_random(&mut random);
    let random_at = push_bytes(&random)?;

    let mut words: Vec<u64> = Vec::new();
    words.push(argv.len() as u64);
    words.extend(argv_at.iter().copied());
    words.push(0);
    words.extend(envp_at.iter().copied());
    words.push(0);
    for (key, value) in auxv {
        words.push(*key);
        words.push(*value);
    }
    words.extend_from_slice(&[AT_RANDOM, random_at, AT_EXECFN, execfn_at, AT_PLATFORM, platform_at, AT_NULL, 0]);

    // RSP must be 16-aligned at the entry point, argc at [rsp].
    let total = words.len() as u64 * 8;
    let mut rsp = (sp - total) & !15;
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    space.poke(rsp, &bytes).map_err(|_| EFAULT)?;
    rsp &= !15;
    Ok(rsp)
}

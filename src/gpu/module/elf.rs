//! The ELF loader for modules: maps a position-independent shared object
//! into the module window, applies its `R_X86_64_RELATIVE` relocations and
//! enforces W^X -- no page both writable and executable, relro made
//! read-only once relocated. Everything else is refused before a byte of
//! the module runs.

use super::*;

pub(super) const PT_LOAD: u32 = 1;
pub(super) const PT_DYNAMIC: u32 = 2;
pub(super) const PT_GNU_RELRO: u32 = 0x6474_E552;
pub(super) const PF_X: u32 = 1;
pub(super) const PF_W: u32 = 2;
pub(super) const DT_NULL: i64 = 0;
pub(super) const DT_NEEDED: i64 = 1;
pub(super) const DT_PLTRELSZ: i64 = 2;
pub(super) const DT_RELA: i64 = 7;
pub(super) const DT_RELASZ: i64 = 8;
pub(super) const DT_RELAENT: i64 = 9;
pub(super) const DT_REL: i64 = 17;
pub(super) const DT_TEXTREL: i64 = 22;
pub(super) const R_X86_64_NONE: u32 = 0;
pub(super) const R_X86_64_RELATIVE: u32 = 8;
pub(super) const PAGE: u64 = 4096;

/// A module in memory.
pub struct Image {
    pub(super) base: u64,
    /// Frames of the image, one per page from `base`.
    pub(super) frames: Vec<u64>,
    pub(super) entry: u64,
}

impl Image {
    /// Unmaps the image and frees its frames. Nothing may run in it any
    /// more.
    pub fn unload(self) {
        for (page, frame) in self.frames.iter().enumerate() {
            // SAFETY: the pages were mapped by `load` and nothing uses them.
            unsafe {
                memory::unmap_page(self.base + page as u64 * PAGE);
                crate::frame::deallocate_frame(crate::frame::Frame(*frame));
            }
        }
    }
}

pub(super) struct Segment {
    pub(super) kind: u32,
    pub(super) flags: u32,
    pub(super) offset: u64,
    pub(super) vaddr: u64,
    pub(super) filesz: u64,
    pub(super) memsz: u64,
}

/// Loads the ELF of a module: segments placed, relocations applied, pages
/// protected. Refuses before running anything.
pub fn load(elf: &[u8]) -> Result<Image, Refusal> {
    if elf.len() < 64 || &elf[0..4] != b"\x7FELF" || elf[4] != 2 || elf[5] != 1 {
        return Err("charge utile : pas un ELF 64 bits petit-boutiste");
    }
    if u16_at(elf, 16) != 3 || u16_at(elf, 18) != 62 {
        return Err("charge utile : pas un objet partage x86-64");
    }
    let entry = u64_at(elf, 24);
    let phoff = u64_at(elf, 32) as usize;
    let phentsize = u16_at(elf, 54) as usize;
    let phnum = u16_at(elf, 56) as usize;
    if phentsize != 56 || phnum == 0 || phnum > 32 || phoff.checked_add(phnum * 56).is_none_or(|end| end > elf.len()) {
        return Err("charge utile : en-tetes de programme illisibles");
    }
    let segments: Vec<Segment> = (0..phnum)
        .map(|index| {
            let at = phoff + index * 56;
            Segment {
                kind: u32_at(elf, at),
                flags: u32_at(elf, at + 4),
                offset: u64_at(elf, at + 8),
                vaddr: u64_at(elf, at + 16),
                filesz: u64_at(elf, at + 32),
                memsz: u64_at(elf, at + 40),
            }
        })
        .collect();

    // --- the extent, and each loaded segment inside the file ---
    let mut span = 0u64;
    for segment in segments.iter().filter(|s| s.kind == PT_LOAD) {
        let end = segment.vaddr.checked_add(segment.memsz).ok_or("segment hors limites")?;
        let file_end = segment.offset.checked_add(segment.filesz).ok_or("segment hors du fichier")?;
        if segment.filesz > segment.memsz || file_end > elf.len() as u64 {
            return Err("segment hors du fichier");
        }
        if segment.flags & PF_W != 0 && segment.flags & PF_X != 0 {
            return Err("segment a la fois modifiable et executable");
        }
        span = span.max(end);
    }
    let span = span.checked_add(PAGE - 1).ok_or("module trop grand")? & !(PAGE - 1);
    if span == 0 || span > MAX_IMAGE {
        return Err("module vide ou trop grand");
    }
    if !segments.iter().any(|s| s.kind == PT_LOAD && s.flags & PF_X != 0 && (s.vaddr..s.vaddr + s.memsz).contains(&entry)) {
        return Err("point d'entree hors du code");
    }
    // Page by page: which protections each needs. A page wanted both
    // writable and executable is refused (W^X).
    let pages = (span / PAGE) as usize;
    let mut writable = alloc::vec![false; pages];
    let mut executable = alloc::vec![false; pages];
    for segment in segments.iter().filter(|s| s.kind == PT_LOAD) {
        let first = (segment.vaddr / PAGE) as usize;
        let last = ((segment.vaddr + segment.memsz).div_ceil(PAGE) as usize).max(first + 1);
        for page in first..last.min(pages) {
            writable[page] |= segment.flags & PF_W != 0;
            executable[page] |= segment.flags & PF_X != 0;
            if writable[page] && executable[page] {
                return Err("une page serait a la fois modifiable et executable");
            }
        }
    }

    // --- the dynamic section: no library, relocations with addends ---
    let mut rela = (0u64, 0u64, 24u64);
    if let Some(dynamic) = segments.iter().find(|s| s.kind == PT_DYNAMIC) {
        let start = dynamic.offset as usize;
        let end = start.checked_add(dynamic.filesz as usize).filter(|e| *e <= elf.len()).ok_or("section dynamique hors du fichier")?;
        let mut at = start;
        while at + 16 <= end {
            let tag = i64_at(elf, at);
            let value = u64_at(elf, at + 8);
            match tag {
                DT_NULL => break,
                DT_NEEDED => return Err("le module demande une bibliotheque"),
                DT_PLTRELSZ if value != 0 => return Err("le module appelle des fonctions a resoudre"),
                DT_REL => return Err("relocations sans addende non prises en charge"),
                DT_TEXTREL => return Err("relocations dans le code"),
                DT_RELA => rela.0 = value,
                DT_RELASZ => rela.1 = value,
                DT_RELAENT => rela.2 = value,
                _ => {}
            }
            at += 16;
        }
    }

    // --- memory, and the bytes ---
    let base = NEXT_MODULE.fetch_add(span + PAGE, Ordering::Relaxed);
    let mut image = Image { base, frames: Vec::with_capacity(pages), entry: base + entry };
    let mut kernel_rw = flags::PRESENT | flags::WRITABLE;
    if memory::nx_enabled() {
        kernel_rw |= flags::NO_EXECUTE;
    }
    for page in 0..pages {
        let Some(frame) = crate::frame::allocate_zeroed_frame() else {
            image.unload();
            return Err("plus de cadre pour le module");
        };
        // SAFETY: the window of modules is the kernel's, and this range
        // was just reserved.
        if unsafe { memory::map_page(base + page as u64 * PAGE, frame.start_addr(), kernel_rw) }.is_err() {
            // SAFETY: the frame was never mapped.
            unsafe { crate::frame::deallocate_frame(frame) };
            image.unload();
            return Err("projection du module impossible");
        }
        image.frames.push(frame.start_addr());
    }
    for segment in segments.iter().filter(|s| s.kind == PT_LOAD) {
        let source = &elf[segment.offset as usize..(segment.offset + segment.filesz) as usize];
        // SAFETY: the range lies inside the image just mapped writable.
        unsafe { core::ptr::copy_nonoverlapping(source.as_ptr(), (base + segment.vaddr) as *mut u8, source.len()) };
    }

    // --- relocations: base + addend, and nothing else ---
    if rela.1 != 0 {
        if rela.2 != 24 || rela.1 % 24 != 0 {
            image.unload();
            return Err("table de relocations mal formee");
        }
        // The table is in the image (it is part of a loaded segment).
        if rela.0.checked_add(rela.1).is_none_or(|end| end > span) {
            image.unload();
            return Err("table de relocations hors du module");
        }
        for index in 0..rela.1 / 24 {
            let at = base + rela.0 + index * 24;
            // SAFETY: inside the image, checked just above.
            let (offset, info, addend) = unsafe {
                ((at as *const u64).read_unaligned(), ((at + 8) as *const u64).read_unaligned(), ((at + 16) as *const i64).read_unaligned())
            };
            match (info & 0xFFFF_FFFF) as u32 {
                R_X86_64_NONE => {}
                R_X86_64_RELATIVE => {
                    if offset.checked_add(8).is_none_or(|end| end > span) {
                        image.unload();
                        return Err("relocation hors du module");
                    }
                    // SAFETY: inside the image, and still writable.
                    unsafe { ((base + offset) as *mut u64).write_unaligned(base.wrapping_add(addend as u64)) };
                }
                _ => {
                    image.unload();
                    return Err("relocation autre que R_X86_64_RELATIVE");
                }
            }
        }
    }

    // --- the protections: code executable, data not, relro read-only ---
    let relro: Vec<(u64, u64)> = segments
        .iter()
        .filter(|s| s.kind == PT_GNU_RELRO)
        .map(|s| (s.vaddr / PAGE, (s.vaddr + s.memsz) / PAGE))
        .collect();
    for page in 0..pages {
        let frozen = relro.iter().any(|(first, end)| (*first..*end).contains(&(page as u64)));
        let mut page_flags = flags::PRESENT;
        if writable[page] && !frozen {
            page_flags |= flags::WRITABLE;
        }
        if !executable[page] && memory::nx_enabled() {
            page_flags |= flags::NO_EXECUTE;
        }
        // SAFETY: the page is the image's own, remapped to its final rights.
        unsafe {
            let _ = memory::map_page(base + page as u64 * PAGE, image.frames[page], page_flags);
            memory::flush_page(base + page as u64 * PAGE);
        }
    }
    memory::shootdown_and_wait();
    Ok(image)
}

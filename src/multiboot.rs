//! Reading of the Multiboot2 information structure.
//!
//! GRUB places in physical memory an 8-byte header followed by a series
//! of 8-byte-aligned tags, terminated by a tag of type 0. The address
//! received being physical, every access goes through the PHYS_OFFSET window.

use core::slice;
use core::str;

use crate::memory::phys_to_virt;

/// Boot mode requested from the GRUB menu (spec 2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootMode {
    Normal,
    Recovery,
    Debug,
}

impl BootMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            BootMode::Normal => "normal",
            BootMode::Recovery => "recovery",
            BootMode::Debug => "debug",
        }
    }
}

/// The mode the kernel booted in, for the code that runs long after
/// `kernel_main` read the command line (init.dsa, the serial shell).
static BOOT_MODE: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

/// Records the boot mode. Called once, by `kernel_main`.
pub fn set_boot_mode(mode: BootMode) {
    let code = match mode {
        BootMode::Normal => 1,
        BootMode::Recovery => 2,
        BootMode::Debug => 3,
    };
    BOOT_MODE.store(code, core::sync::atomic::Ordering::Relaxed);
}

/// The boot mode, once `kernel_main` has recorded it.
pub fn boot_mode() -> Option<BootMode> {
    match BOOT_MODE.load(core::sync::atomic::Ordering::Relaxed) {
        1 => Some(BootMode::Normal),
        2 => Some(BootMode::Recovery),
        3 => Some(BootMode::Debug),
        _ => None,
    }
}

pub fn is_recovery() -> bool {
    boot_mode() == Some(BootMode::Recovery)
}

// ============================================================
//  2.2.1 -- Memory map (tag 6)
// ============================================================

/// Region types defined by the Multiboot2 specification.
pub const AREA_AVAILABLE: u32 = 1;
pub const AREA_RESERVED: u32 = 2;
pub const AREA_ACPI_RECLAIMABLE: u32 = 3;
pub const AREA_NVS: u32 = 4;
pub const AREA_BADRAM: u32 = 5;
const TAG_ACPI_V1: u32 = 14;
const TAG_ACPI_V2: u32 = 15;

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct MemoryArea {
    pub base_addr: u64,
    pub length: u64,
    pub area_type: u32,
    _reserved: u32,
}

impl MemoryArea {
    pub fn end_addr(&self) -> u64 {
        self.base_addr + self.length
    }

    /// Only the regions of this type may feed the frame allocator of
    /// step 2.3.1.
    pub fn is_usable(&self) -> bool {
        self.area_type == AREA_AVAILABLE
    }

    pub fn type_name(&self) -> &'static str {
        match self.area_type {
            AREA_AVAILABLE => "disponible",
            AREA_RESERVED => "reservee",
            AREA_ACPI_RECLAIMABLE => "ACPI recuperable",
            AREA_NVS => "ACPI NVS",
            AREA_BADRAM => "defectueuse",
            _ => "inconnue",
        }
    }
}

/// View on the region array supplied by GRUB.
///
/// The entry size is read from the tag rather than hard-coded: the
/// specification explicitly allows it to grow in future.
#[derive(Clone, Copy)]
pub struct MemoryMap {
    first: *const u8,
    entry_size: u32,
    count: usize,
}

impl MemoryMap {
    pub const fn empty() -> Self {
        MemoryMap {
            first: core::ptr::null(),
            entry_size: 0,
            count: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn get(&self, index: usize) -> Option<MemoryArea> {
        if index >= self.count {
            return None;
        }
        let ptr = unsafe { self.first.add(index * self.entry_size as usize) };
        Some(unsafe { (ptr as *const MemoryArea).read_unaligned() })
    }

    pub fn iter(&self) -> MemoryAreaIter {
        MemoryAreaIter {
            map: *self,
            index: 0,
        }
    }

    /// Total usable RAM, all available segments together.
    pub fn total_usable(&self) -> u64 {
        self.iter().filter(|a| a.is_usable()).map(|a| a.length).sum()
    }

    /// Highest physical address reported by the firmware.
    pub fn highest_addr(&self) -> u64 {
        self.iter().map(|a| a.end_addr()).max().unwrap_or(0)
    }
}

pub struct MemoryAreaIter {
    map: MemoryMap,
    index: usize,
}

impl Iterator for MemoryAreaIter {
    type Item = MemoryArea;

    fn next(&mut self) -> Option<MemoryArea> {
        let area = self.map.get(self.index)?;
        self.index += 1;
        Some(area)
    }
}

// ============================================================
//  7.1.1 -- Framebuffer (tag 8)
// ============================================================

/// Nature of the framebuffer described by tag 8.
pub const FB_INDEXED: u8 = 0;
pub const FB_RGB: u8 = 1;
pub const FB_EGA_TEXT: u8 = 2;

/// Description of the display mode programmed by the bootloader.
///
/// The field positions and widths are not assumed: a 16-bit 5-6-5 mode
/// and a 32-bit 8-8-8 mode are described by the same six bytes, and this
/// is the only way to compose a colour without hard-coding a depth.
#[derive(Debug, Clone, Copy)]
pub struct FramebufferInfo {
    /// **Physical** address of the first pixel.
    pub addr: u64,
    /// Bytes from one row to the next -- not necessarily `width * bpp / 8`.
    pub pitch: u32,
    pub width: u32,
    pub height: u32,
    pub bpp: u8,
    pub kind: u8,
    pub red_pos: u8,
    pub red_size: u8,
    pub green_pos: u8,
    pub green_size: u8,
    pub blue_pos: u8,
    pub blue_size: u8,
}

impl FramebufferInfo {
    /// Total size occupied, in bytes.
    pub fn bytes(&self) -> u64 {
        self.pitch as u64 * self.height as u64
    }

    /// Number of bytes per pixel, rounded up.
    pub fn bytes_per_pixel(&self) -> usize {
        (self.bpp as usize).div_ceil(8)
    }

    /// Is the mode usable by the graphics driver?
    ///
    /// Only direct-color is. Indexed mode would require programming a
    /// DAC palette, and EGA text mode is not a framebuffer at all -- it
    /// is the one the VGA writer knows how to write.
    pub fn is_graphic(&self) -> bool {
        self.kind == FB_RGB && self.bpp >= 15 && self.pitch != 0 && self.width != 0
    }

    pub fn kind_name(&self) -> &'static str {
        match self.kind {
            FB_INDEXED => "indexe",
            FB_RGB => "RGB direct",
            FB_EGA_TEXT => "texte EGA",
            _ => "inconnu",
        }
    }
}

// ============================================================
//  BootInfo
// ============================================================

#[derive(Clone, Copy)]
pub struct BootInfo {
    pub mode: BootMode,
    pub rsdp_addr: u64,
    pub verbose: bool,
    pub rollback: bool,
    pub cmdline: &'static str,
    pub bootloader: &'static str,
    pub mem_lower_kb: u32,
    pub mem_upper_kb: u32,
    pub total_size: u32,
    pub phys_addr: u64,
    pub memory_map: MemoryMap,
    pub framebuffer: Option<FramebufferInfo>,
}

impl BootInfo {
    const fn empty() -> Self {
        BootInfo {
            mode: BootMode::Normal,
            rsdp_addr: 0,
            verbose: false,
            rollback: false,
            cmdline: "",
            bootloader: "inconnu",
            mem_lower_kb: 0,
            mem_upper_kb: 0,
            total_size: 0,
            phys_addr: 0,
            memory_map: MemoryMap::empty(),
            framebuffer: None,
        }
    }
}

const TAG_END: u32 = 0;
const TAG_CMDLINE: u32 = 1;
const TAG_BOOTLOADER: u32 = 2;
const TAG_BASIC_MEMINFO: u32 = 4;
const TAG_MMAP: u32 = 6;
const TAG_FRAMEBUFFER: u32 = 8;

/// Parses the Multiboot2 structure.
///
/// # Safety
/// `phys_addr` must be the **physical** address handed over by the
/// bootloader, and the PHYS_OFFSET window must be active.
pub unsafe fn parse(phys_addr: u64) -> BootInfo {
    let mut info = BootInfo::empty();
    info.phys_addr = phys_addr;

    if phys_addr == 0 || phys_addr % 8 != 0 {
        return info;
    }

    let base = phys_to_virt(phys_addr) as *const u8;
    info.total_size = unsafe { (base as *const u32).read_unaligned() };

    let mut offset: u32 = 8;

    while offset + 8 <= info.total_size {
        let tag = unsafe { base.add(offset as usize) };
        let tag_type = unsafe { (tag as *const u32).read_unaligned() };
        let tag_size = unsafe { (tag.add(4) as *const u32).read_unaligned() };

        if tag_size < 8 {
            break;
        }

        match tag_type {
            TAG_END => break,

            TAG_CMDLINE => {
                let s = unsafe { cstr_from(tag.add(8), (tag_size - 8) as usize) };
                info.cmdline = s;
                parse_cmdline(s, &mut info);
            }

            TAG_BOOTLOADER => {
                info.bootloader = unsafe { cstr_from(tag.add(8), (tag_size - 8) as usize) };
            }

            TAG_BASIC_MEMINFO => {
                info.mem_lower_kb = unsafe { (tag.add(8) as *const u32).read_unaligned() };
                info.mem_upper_kb = unsafe { (tag.add(12) as *const u32).read_unaligned() };
            }

            TAG_MMAP => {
                let entry_size = unsafe { (tag.add(8) as *const u32).read_unaligned() };
                // tag_size covers: type, size, entry_size, entry_version, then the entries.
                if entry_size >= 24 {
                    let bytes = tag_size - 16;
                    info.memory_map = MemoryMap {
                        first: unsafe { tag.add(16) },
                        entry_size,
                        count: (bytes / entry_size) as usize,
                    };
                }
            }
            // The tag is decoded field by field: nothing guarantees the
            // structure is aligned the way a `repr(C)` would want, and
            // the six format bytes only start after two reserved bytes.
            TAG_FRAMEBUFFER => {
                if tag_size >= 32 {
                    let byte = |offset: usize| unsafe { *tag.add(offset) };
                    let word = |offset: usize| unsafe {
                        (tag.add(offset) as *const u32).read_unaligned()
                    };

                    let mut fb = FramebufferInfo {
                        addr: unsafe { (tag.add(8) as *const u64).read_unaligned() },
                        pitch: word(16),
                        width: word(20),
                        height: word(24),
                        bpp: byte(28),
                        kind: byte(29),
                        red_pos: 0,
                        red_size: 0,
                        green_pos: 0,
                        green_size: 0,
                        blue_pos: 0,
                        blue_size: 0,
                    };

                    if fb.kind == FB_RGB && tag_size >= 38 {
                        fb.red_pos = byte(32);
                        fb.red_size = byte(33);
                        fb.green_pos = byte(34);
                        fb.green_size = byte(35);
                        fb.blue_pos = byte(36);
                        fb.blue_size = byte(37);
                    }

                    info.framebuffer = Some(fb);
                }
            }

            // GRUB copies the RSDP into the tag: its address is therefore
            // that of the tag content, not that of the original table.
            TAG_ACPI_V1 | TAG_ACPI_V2 => {
                if info.rsdp_addr == 0 || tag_type == TAG_ACPI_V2 {
                    let offset_in_struct = unsafe { tag.add(8) as u64 } - base as u64;
                    info.rsdp_addr = phys_addr + offset_in_struct;
                }
            }
            _ => {}
        }

        offset += (tag_size + 7) & !7;
    }

    info
}

unsafe fn cstr_from(ptr: *const u8, max_len: usize) -> &'static str {
    let mut len = 0;
    while len < max_len && unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    let bytes = unsafe { slice::from_raw_parts(ptr, len) };
    str::from_utf8(bytes).unwrap_or("<non-utf8>")
}

/// `essai-gpu=garde` on the command line: the GPU self-test runs the
/// watchdog trial (13.11.3) -- a draw that holds the host's GPU past the
/// kernel's two seconds. Not at an ordinary boot: it borrows the host's
/// GPU, and the host's own desktop, for those seconds.
pub static GPU_WATCHDOG_TRIAL: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// `racine=<GUID>` on the command line: the partition mounted as `/` (spec
/// 4.5), before the "system" flag is looked at.
static ROOT_PARTITION: crate::vga::SpinLock<Option<[u8; 16]>> = crate::vga::SpinLock::new(None);

pub fn root_partition() -> Option<[u8; 16]> {
    *ROOT_PARTITION.lock()
}

fn parse_cmdline(cmdline: &str, info: &mut BootInfo) {
    for token in cmdline.split(' ') {
        match token {
            "mode=normal" => info.mode = BootMode::Normal,
            "mode=recovery" => info.mode = BootMode::Recovery,
            "mode=debug" => {
                info.mode = BootMode::Debug;
                info.verbose = true;
            }
            "verbose=1" => info.verbose = true,
            "rollback=1" => info.rollback = true,
            "essai-gpu=garde" => GPU_WATCHDOG_TRIAL.store(true, core::sync::atomic::Ordering::Relaxed),
            _ if token.starts_with("racine=") => match crate::fs::partition::parse_guid(&token["racine=".len()..]) {
                Some(guid) => *ROOT_PARTITION.lock() = Some(guid),
                None => crate::serial_println!("[boot] racine=\"{}\" : GUID illisible - ignore", &token["racine=".len()..]),
            },
            _ => {}
        }
    }
}

/// Logs the complete memory map on the serial console.
pub fn dump_memory_map(map: &MemoryMap) {
    crate::serial_println!("[mmap] {} regions rapportees par le firmware", map.len());

    for (i, area) in map.iter().enumerate() {
        crate::serial_println!(
            "[mmap]   {:2}  0x{:012x}-0x{:012x}  {:>9} Kio  {}",
            i,
            area.base_addr,
            area.end_addr(),
            area.length / 1024,
            area.type_name()
        );
    }

    crate::serial_println!(
        "[mmap] total exploitable : {} Kio ({} Mio)",
        map.total_usable() / 1024,
        map.total_usable() / (1024 * 1024)
    );
}

/// Walks and logs every tag present -- used in debug mode.
///
/// # Safety
/// Same prerequisites as `parse`.
pub unsafe fn dump_tags(phys_addr: u64) {
    if phys_addr == 0 || phys_addr % 8 != 0 {
        crate::serial_println!("[multiboot] adresse invalide : 0x{:x}", phys_addr);
        return;
    }

    let base = phys_to_virt(phys_addr) as *const u8;
    let total_size = unsafe { (base as *const u32).read_unaligned() };
    crate::serial_println!(
        "[multiboot] structure de {} octets - phys 0x{:x} / virt 0x{:x}",
        total_size,
        phys_addr,
        base as u64
    );

    let mut offset: u32 = 8;

    while offset + 8 <= total_size {
        let tag = unsafe { base.add(offset as usize) };
        let tag_type = unsafe { (tag as *const u32).read_unaligned() };
        let tag_size = unsafe { (tag.add(4) as *const u32).read_unaligned() };

        if tag_size < 8 {
            break;
        }
        if tag_type == TAG_END {
            crate::serial_println!("[multiboot]   tag  0 (fin)");
            break;
        }

        crate::serial_println!(
            "[multiboot]   tag {:2} ({:>4} octets) - {}",
            tag_type,
            tag_size,
            tag_name(tag_type)
        );

        offset += (tag_size + 7) & !7;
    }
}

fn tag_name(tag_type: u32) -> &'static str {
    match tag_type {
        1 => "ligne de commande",
        2 => "nom du bootloader",
        3 => "module",
        4 => "memoire de base",
        5 => "peripherique de boot BIOS",
        6 => "memory map",
        7 => "info VBE",
        8 => "framebuffer",
        9 => "sections ELF",
        10 => "APM",
        14 => "RSDP ACPI v1",
        15 => "RSDP ACPI v2",
        21 => "adresse de chargement",
        _ => "non traite",
    }
}

//! ACPI tables -- hardware discovery.
//!
//! Resolution chain: RSDP -> RSDT (32-bit) or XSDT (64-bit) -> MADT.
//! The MADT (Multiple APIC Description Table, signature "APIC")
//! enumerates the Local APICs present -- hence the cores -- as well as
//! the I/O APICs and the IRQ redirections inherited from the PIC.
//!
//! The FADT (signature "FACP") is read for the power controls only: the
//! PM1 control ports, the reset register, and through its DSDT pointer
//! the `\_S5` package that gives the sleep type of "soft off". The FADT
//! is decoded field by field with `from_le_bytes`, unlike the MADT above:
//! nothing guarantees the alignment of a firmware table, and the MADT
//! code predates that rule of CLAUDE.md.
//!
//! ## Why `\_S5` is found by searching, not by interpreting
//!
//! The sleep types live in AML bytecode, and a real AML interpreter is a
//! project the size of this kernel. `\_S5` is, on every firmware seen in
//! practice, a plain `Name` holding a `Package` of constants, which reads
//! with a dozen bytes of pattern. What does not fit the pattern -- a
//! package computed by a method -- is refused, not guessed: a wrong
//! SLP_TYP puts the machine in some other sleep state, or in none.

use alloc::vec::Vec;

use crate::memory::phys_to_virt;

// ============================================================
//  RSDP
// ============================================================

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Rsdp {
    signature: [u8; 8],
    checksum: u8,
    oem_id: [u8; 6],
    revision: u8,
    rsdt_address: u32,
    // --- ACPI 2.0 and beyond ---
    length: u32,
    xsdt_address: u64,
    extended_checksum: u8,
    reserved: [u8; 3],
}

/// Header common to every ACPI table.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct SdtHeader {
    signature: [u8; 4],
    length: u32,
    revision: u8,
    checksum: u8,
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    creator_id: u32,
    creator_revision: u32,
}

/// ACPI checksum: the sum of all the bytes must be zero modulo 256.
unsafe fn checksum_ok(ptr: *const u8, len: usize) -> bool {
    let mut sum: u8 = 0;
    for i in 0..len {
        sum = sum.wrapping_add(unsafe { *ptr.add(i) });
    }
    sum == 0
}

// ============================================================
//  Informations extraites
// ============================================================

/// One logical core declared by the MADT.
#[derive(Debug, Clone, Copy)]
pub struct CpuEntry {
    pub processor_id: u8,
    pub lapic_id: u8,
    /// Is the core usable? Bit 0 of the flags.
    pub enabled: bool,
    /// Can it be started hot? Bit 1.
    pub online_capable: bool,
}

/// Redirection of a legacy IRQ to an I/O APIC entry (type 2).
///
/// Indispensable: on many machines, IRQ 0 of the PIT is wired to line 2
/// of the I/O APIC. Ignoring these redirections leads to routing an
/// interrupt onto the wrong line.
#[derive(Debug, Clone, Copy)]
pub struct InterruptOverride {
    pub source_irq: u8,
    pub global_irq: u32,
    pub flags: u16,
}

#[derive(Clone)]
pub struct AcpiInfo {
    pub cpus: Vec<CpuEntry>,
    pub overrides: Vec<InterruptOverride>,
    pub lapic_addr: u64,
    pub ioapic_addr: u64,
    pub ioapic_gsi_base: u32,
    pub revision: u8,
    pub uses_xsdt: bool,
    /// Power controls, from the FADT and the DSDT.
    pub power: crate::power::PowerInfo,
}

impl AcpiInfo {
    fn empty() -> Self {
        AcpiInfo {
            cpus: Vec::new(),
            overrides: Vec::new(),
            lapic_addr: 0xFEE0_0000,
            ioapic_addr: 0xFEC0_0000,
            ioapic_gsi_base: 0,
            revision: 0,
            uses_xsdt: false,
            power: crate::power::PowerInfo::empty(),
        }
    }

    /// Cores that can really be started.
    pub fn usable_cpus(&self) -> usize {
        self.cpus.iter().filter(|c| c.enabled).count()
    }

    /// Applies the declared redirections to a legacy IRQ.
    pub fn resolve_irq(&self, irq: u8) -> u32 {
        self.overrides
            .iter()
            .find(|o| o.source_irq == irq)
            .map(|o| o.global_irq)
            .unwrap_or(irq as u32)
    }
}

// ============================================================
//  Finding the RSDP
// ============================================================

/// Finds the RSDP by scanning low memory.
///
/// Used only if Multiboot2 did not supply tags 14 or 15. The signature is
/// 16-byte aligned, in the EBDA or the BIOS area.
unsafe fn scan_for_rsdp() -> Option<u64> {
    const SIGNATURE: &[u8; 8] = b"RSD PTR ";

    // The pointer to the EBDA is in the BDA, at 0x40E, in paragraphs.
    let ebda_ptr = unsafe { *(phys_to_virt(0x40E) as *const u16) } as u64 * 16;

    let ranges: [(u64, u64); 2] = [(ebda_ptr, ebda_ptr + 1024), (0xE0000, 0x100000)];

    for (start, end) in ranges {
        if start == 0 {
            continue;
        }
        let mut addr = start;
        while addr < end {
            let ptr = phys_to_virt(addr) as *const u8;
            let sig = unsafe { core::slice::from_raw_parts(ptr, 8) };
            if sig == SIGNATURE && unsafe { checksum_ok(ptr, 20) } {
                return Some(addr);
            }
            addr += 16;
        }
    }
    None
}

/// Parses the ACPI tables and returns the topology discovered.
///
/// # Safety
/// The PHYS_OFFSET window must cover the memory holding the tables.
pub unsafe fn parse(rsdp_phys: u64) -> AcpiInfo {
    let mut info = AcpiInfo::empty();

    let rsdp_addr = if rsdp_phys != 0 {
        rsdp_phys
    } else {
        match unsafe { scan_for_rsdp() } {
            Some(addr) => addr,
            None => {
                crate::serial_println!("[acpi] RSDP introuvable");
                return info;
            }
        }
    };

    let rsdp = unsafe { &*(phys_to_virt(rsdp_addr) as *const Rsdp) };
    info.revision = rsdp.revision;

    // Revision 0 = ACPI 1.0, 32-bit RSDT. Revision 2+ = 64-bit XSDT.
    let (sdt_phys, entry_size) = if rsdp.revision >= 2 && rsdp.xsdt_address != 0 {
        info.uses_xsdt = true;
        (rsdp.xsdt_address, 8usize)
    } else {
        (rsdp.rsdt_address as u64, 4usize)
    };

    let header = unsafe { &*(phys_to_virt(sdt_phys) as *const SdtHeader) };
    let total_len = header.length as usize;
    let header_len = core::mem::size_of::<SdtHeader>();

    if total_len <= header_len {
        crate::serial_println!("[acpi] table racine vide");
        return info;
    }

    let count = (total_len - header_len) / entry_size;
    let entries = unsafe { (phys_to_virt(sdt_phys) as *const u8).add(header_len) };

    // Walks the referenced tables until the MADT is found.
    for i in 0..count {
        let table_phys = unsafe {
            let ptr = entries.add(i * entry_size);
            if entry_size == 8 {
                (ptr as *const u64).read_unaligned()
            } else {
                (ptr as *const u32).read_unaligned() as u64
            }
        };

        if table_phys == 0 {
            continue;
        }

        let table = unsafe { &*(phys_to_virt(table_phys) as *const SdtHeader) };
        if &table.signature == b"APIC" {
            unsafe { parse_madt(table_phys, &mut info) };
        } else if &table.signature == b"FACP" {
            unsafe { parse_fadt(table_phys, &mut info) };
        }
    }

    info
}

/// Parses the MADT: list of LAPICs, I/O APICs and redirections.
unsafe fn parse_madt(phys: u64, info: &mut AcpiInfo) {
    let base = phys_to_virt(phys) as *const u8;
    let header = unsafe { &*(base as *const SdtHeader) };
    let length = header.length as usize;

    // After the header: LAPIC address (u32) and flags (u32).
    info.lapic_addr = unsafe { (base.add(36) as *const u32).read_unaligned() } as u64;

    let mut offset = 44;

    while offset + 2 <= length {
        let entry_type = unsafe { *base.add(offset) };
        let entry_len = unsafe { *base.add(offset + 1) } as usize;

        if entry_len < 2 {
            break;
        }

        match entry_type {
            // Type 0: Processor Local APIC -- one core.
            0 if entry_len >= 8 => {
                let processor_id = unsafe { *base.add(offset + 2) };
                let lapic_id = unsafe { *base.add(offset + 3) };
                let flags =
                    unsafe { (base.add(offset + 4) as *const u32).read_unaligned() };

                info.cpus.push(CpuEntry {
                    processor_id,
                    lapic_id,
                    enabled: flags & 1 != 0,
                    online_capable: flags & 2 != 0,
                });
            }

            // Type 1: I/O APIC.
            1 if entry_len >= 12 => {
                info.ioapic_addr =
                    unsafe { (base.add(offset + 4) as *const u32).read_unaligned() } as u64;
                info.ioapic_gsi_base =
                    unsafe { (base.add(offset + 8) as *const u32).read_unaligned() };
            }

            // Type 2: Interrupt Source Override.
            2 if entry_len >= 10 => {
                let source_irq = unsafe { *base.add(offset + 3) };
                let global_irq =
                    unsafe { (base.add(offset + 4) as *const u32).read_unaligned() };
                let flags = unsafe { (base.add(offset + 8) as *const u16).read_unaligned() };

                info.overrides.push(InterruptOverride {
                    source_irq,
                    global_irq,
                    flags,
                });
            }

            // Type 5: redefinition of the LAPIC address on 64 bits.
            5 if entry_len >= 12 => {
                info.lapic_addr =
                    unsafe { (base.add(offset + 4) as *const u64).read_unaligned() };
            }

            _ => {}
        }

        offset += entry_len;
    }
}

// ============================================================
//  FADT and \_S5 -- power controls
// ============================================================

/// Physical addresses the PHYS_OFFSET window reaches from the start (4 GiB).
/// The window may grow later (`memory::extend_window`), but the firmware
/// places its tables below 4 GiB, and a pointer beyond is corrupt.
const WINDOW_LIMIT: u64 = 4 << 30;

/// Upper bound on a table length read from firmware.
///
/// A corrupt length would otherwise turn into a slice over gigabytes. A
/// DSDT is tens of kilobytes on the largest machines; a megabyte leaves
/// room and still bounds the damage.
const MAX_TABLE_LEN: usize = 1 << 20;

fn le_u8(bytes: &[u8], offset: usize) -> Option<u8> {
    bytes.get(offset).copied()
}

fn le_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let b = bytes.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn le_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let b = bytes.get(offset..offset + 8)?;
    Some(u64::from_le_bytes([
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
    ]))
}

/// A whole firmware table as a byte slice, its length read from its header.
///
/// # Safety
/// `phys` must be the physical address of an ACPI table inside the
/// PHYS_OFFSET window, and the memory must stay mapped and unmodified for
/// the lifetime of the slice -- true of ACPI tables, which the kernel never
/// reclaims.
unsafe fn table_bytes(phys: u64) -> Option<&'static [u8]> {
    if phys == 0 || phys + 8 > WINDOW_LIMIT {
        return None;
    }
    let base = phys_to_virt(phys) as *const u8;
    // The length is read byte by byte for the same alignment reason as
    // every field below.
    let head = unsafe { core::slice::from_raw_parts(base, 8) };
    let length = le_u32(head, 4)? as usize;
    if !(36..=MAX_TABLE_LEN).contains(&length) || phys + length as u64 > WINDOW_LIMIT {
        return None;
    }
    Some(unsafe { core::slice::from_raw_parts(base, length) })
}

/// Reads the power controls from the FADT and its DSDT.
///
/// # Safety
/// `phys` must be the physical address of the FADT, inside the window.
unsafe fn parse_fadt(phys: u64, info: &mut AcpiInfo) {
    let Some(fadt) = (unsafe { table_bytes(phys) }) else {
        return;
    };

    // Offsets of ACPI 6.x, table 5.9. Every field past the ACPI 1.0 part
    // is read through `get`, so a short revision-1 table yields `None`
    // rather than a read past its end.
    let power = &mut info.power;
    power.present = true;
    power.smi_command = le_u32(fadt, 48).unwrap_or(0) as u16;
    power.acpi_enable = le_u8(fadt, 52).unwrap_or(0);
    power.pm1a_control = le_u32(fadt, 64).unwrap_or(0) as u16;
    power.pm1b_control = le_u32(fadt, 68).unwrap_or(0) as u16;

    // RESET_REG is a Generic Address Structure at 116, its value at 128,
    // and it only means something when bit 10 of the flags says so. Only
    // the I/O space form is kept: a memory-mapped reset register would
    // need a page mapped `NO_CACHE`, for a path the 0xCF9 fallback covers
    // on the machines this kernel runs on.
    let flags = le_u32(fadt, 112).unwrap_or(0);
    if flags & (1 << 10) != 0
        && let (Some(space), Some(address), Some(value)) =
            (le_u8(fadt, 116), le_u64(fadt, 120), le_u8(fadt, 128))
        && space == 1
        && address != 0
        && address <= 0xFFFF
    {
        power.reset = Some((address as u16, value));
    }

    // X_DSDT wins when present: on a machine with both, the 32-bit field
    // may be zero or stale.
    let dsdt_phys = match le_u64(fadt, 140) {
        Some(x) if x != 0 => x,
        _ => le_u32(fadt, 40).unwrap_or(0) as u64,
    };
    if let Some(dsdt) = unsafe { table_bytes(dsdt_phys) }
        && &dsdt[0..4] == b"DSDT"
    {
        power.s5 = find_s5(&dsdt[36..]);
    }
}

/// Finds the `\_S5` package in AML and returns `(SLP_TYPa, SLP_TYPb)`.
///
/// Accepted shape, which is how every firmware seen in practice encodes it:
///
/// ```text
/// 08            NameOp            (optionally followed by 5C, the root)
/// 5F 53 35 5F   "_S5_"
/// 12            PackageOp
/// xx [..]       PkgLength, 1 to 4 bytes (count in bits 6-7 of the first)
/// xx            NumElements
/// then two elements, each ZeroOp (00), OneOp (01) or BytePrefix (0A) + byte
/// ```
///
/// Every occurrence of `_S5_` is tried: the first may be a reference from
/// inside a method, which is not preceded by `NameOp`.
pub fn find_s5(aml: &[u8]) -> Option<(u8, u8)> {
    let mut start = 0;
    while let Some(found) = aml[start..].windows(4).position(|window| window == b"_S5_") {
        let at = start + found;
        start = at + 1;

        let named = (at >= 1 && aml[at - 1] == 0x08)
            || (at >= 2 && aml[at - 2] == 0x08 && aml[at - 1] == b'\\');
        if !named || aml.get(at + 4) != Some(&0x12) {
            continue;
        }

        let mut cursor = at + 5;
        let Some(lead) = aml.get(cursor) else {
            continue;
        };
        cursor += 1 + (lead >> 6) as usize;
        // NumElements.
        cursor += 1;

        let Some(a) = aml_constant(aml, &mut cursor) else {
            continue;
        };
        let Some(b) = aml_constant(aml, &mut cursor) else {
            continue;
        };
        // SLP_TYP is a three-bit field of PM1_CNT.
        return Some((a & 0x7, b & 0x7));
    }
    None
}

/// One package element that is a byte-sized constant.
fn aml_constant(aml: &[u8], cursor: &mut usize) -> Option<u8> {
    match *aml.get(*cursor)? {
        0x00 => {
            *cursor += 1;
            Some(0)
        }
        0x01 => {
            *cursor += 1;
            Some(1)
        }
        0x0A => {
            let value = *aml.get(*cursor + 1)?;
            *cursor += 2;
            Some(value)
        }
        _ => None,
    }
}

pub fn dump_state(info: &AcpiInfo) {
    crate::serial_println!(
        "[acpi] revision {} - table racine : {}",
        info.revision,
        if info.uses_xsdt { "XSDT" } else { "RSDT" }
    );
    crate::serial_println!(
        "[acpi] LAPIC a 0x{:08x} - I/O APIC a 0x{:08x} (GSI base {})",
        info.lapic_addr,
        info.ioapic_addr,
        info.ioapic_gsi_base
    );
    crate::serial_println!(
        "[acpi] {} coeurs declares, {} utilisables",
        info.cpus.len(),
        info.usable_cpus()
    );

    for cpu in info.cpus.iter() {
        crate::serial_println!(
            "[acpi]   processeur {} - LAPIC id {} - {}",
            cpu.processor_id,
            cpu.lapic_id,
            if cpu.enabled { "actif" } else { "desactive" }
        );
    }

    let power = &info.power;
    if !power.present {
        crate::serial_println!("[acpi] FADT absente - extinction ACPI indisponible");
    } else {
        match power.s5 {
            Some((a, b)) => crate::serial_println!(
                "[acpi] extinction : PM1a 0x{:04x}, PM1b 0x{:04x}, \\_S5 typ {}/{}",
                power.pm1a_control,
                power.pm1b_control,
                a,
                b
            ),
            None => crate::serial_println!(
                "[acpi] extinction : PM1a 0x{:04x}, \\_S5 introuvable dans la DSDT",
                power.pm1a_control
            ),
        }
        match power.reset {
            Some((port, value)) => crate::serial_println!(
                "[acpi] reinitialisation : registre FADT port 0x{:04x} valeur 0x{:02x}",
                port,
                value
            ),
            None => crate::serial_println!(
                "[acpi] reinitialisation : pas de registre FADT - repli 0xCF9 puis 8042"
            ),
        }
    }

    for ovr in info.overrides.iter() {
        crate::serial_println!(
            "[acpi]   IRQ {} redirigee vers GSI {} (flags 0x{:04x})",
            ovr.source_irq,
            ovr.global_irq,
            ovr.flags
        );
    }
}

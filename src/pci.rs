//! Enumeration of the PCI bus through the legacy configuration space.
//!
//! Two ports are enough: `0xCF8` receives the address -- bus, device,
//! function and offset encoded in a single word -- and `0xCFC` carries
//! the data. It is the oldest mechanism and the only one available
//! without going through the ACPI tables (MCFG), which are not parsed
//! yet.
//!
//! The scan is exhaustive: 256 buses x 32 devices x 8 functions. That is
//! 65 536 reads, a few milliseconds at boot, and it avoids having to
//! follow the bridges to discover the secondary buses.

use alloc::vec::Vec;

use crate::port::{inl, outl};

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

/// Standard offsets of the configuration header (type 0).
pub mod offsets {
    pub const VENDOR_ID: u8 = 0x00;
    pub const DEVICE_ID: u8 = 0x02;
    pub const COMMAND: u8 = 0x04;
    pub const STATUS: u8 = 0x06;
    pub const REVISION: u8 = 0x08;
    pub const PROG_IF: u8 = 0x09;
    pub const SUBCLASS: u8 = 0x0A;
    pub const CLASS: u8 = 0x0B;
    pub const HEADER_TYPE: u8 = 0x0E;
    pub const BAR0: u8 = 0x10;
    pub const BAR5: u8 = 0x24;
}

/// Bits of the command register.
pub mod command {
    /// The device answers accesses in the I/O space.
    pub const IO_SPACE: u16 = 1 << 0;
    /// The device answers memory accesses -- indispensable to MMIO.
    pub const MEMORY_SPACE: u16 = 1 << 1;
    /// The device may initiate DMA transfers.
    pub const BUS_MASTER: u16 = 1 << 2;
}

/// Address of a PCI function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Address {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl Address {
    pub const fn new(bus: u8, device: u8, function: u8) -> Self {
        Address {
            bus,
            device,
            function,
        }
    }

    /// Encodes the address for port 0xCF8. Bit 31 arms the mechanism, and
    /// the two low bits of the offset are forced to zero: the access is
    /// always 32-bit aligned.
    fn encode(&self, offset: u8) -> u32 {
        0x8000_0000
            | ((self.bus as u32) << 16)
            | ((self.device as u32) << 11)
            | ((self.function as u32) << 8)
            | ((offset as u32) & 0xFC)
    }
}

/// Reads 32 bits in the configuration space.
pub fn read32(address: Address, offset: u8) -> u32 {
    unsafe {
        outl(CONFIG_ADDRESS, address.encode(offset));
        inl(CONFIG_DATA)
    }
}

/// Writes 32 bits in the configuration space.
///
/// # Safety
/// Reconfigures the device: a wrong BAR address can make an MMIO region
/// unreachable or make it overlap another.
pub unsafe fn write32(address: Address, offset: u8, value: u32) {
    unsafe {
        outl(CONFIG_ADDRESS, address.encode(offset));
        outl(CONFIG_DATA, value);
    }
}

pub fn read16(address: Address, offset: u8) -> u16 {
    let value = read32(address, offset & 0xFC);
    ((value >> ((offset & 2) * 8)) & 0xFFFF) as u16
}

pub fn read8(address: Address, offset: u8) -> u8 {
    let value = read32(address, offset & 0xFC);
    ((value >> ((offset & 3) * 8)) & 0xFF) as u8
}

/// # Safety
/// Same precautions as `write32`.
pub unsafe fn write16(address: Address, offset: u8, value: u16) {
    let shift = (offset & 2) * 8;
    let mut current = read32(address, offset & 0xFC);
    current &= !(0xFFFFu32 << shift);
    current |= (value as u32) << shift;
    unsafe { write32(address, offset & 0xFC, current) };
}

// ============================================================
//  MSI-X
// ============================================================

/// Capability identifier of MSI-X.
const CAP_MSIX: u8 = 0x11;
/// Message control: MSI-X enabled.
const MSIX_ENABLE: u16 = 1 << 15;
/// Message control: every vector of the function masked.
const MSIX_FUNCTION_MASK: u16 = 1 << 14;

/// Where a function keeps its MSI-X table.
///
/// ## Why MSI-X, when the legacy lines exist
///
/// A PCI line is level triggered: it stays asserted until the driver has
/// read the device's cause register, and the kernel's relay reads no
/// device register (`drivers::VECTOR_DEVICE_BASE` says why the PCI
/// drivers therefore polled). An MSI-X interrupt is a **message** -- a
/// write the device makes to the LAPIC's address -- so it is an edge by
/// nature: acknowledging the LAPIC is enough, nothing stays asserted, and
/// the relay can stay the same two gestures, count and wake.
#[derive(Debug, Clone, Copy)]
pub struct Msix {
    /// Offset of the capability in the configuration space.
    pointer: u8,
    /// BAR holding the table, and the table's offset in it.
    pub table_bar: u8,
    pub table_offset: u32,
    /// Entries of the table.
    pub entries: u16,
}

/// The MSI-X capability of a function, if it has one.
pub fn msix(device: &Device) -> Option<Msix> {
    // Bit 4 of the status register: a capability list exists.
    if read16(device.address, offsets::STATUS) & (1 << 4) == 0 {
        return None;
    }
    let mut pointer = read8(device.address, 0x34) & 0xFC;
    // Bounded: a malformed list that loops must not hang the boot.
    for _ in 0..48 {
        if pointer == 0 || pointer > 0xF4 {
            return None;
        }
        if read8(device.address, pointer) == CAP_MSIX {
            let control = read16(device.address, pointer + 2);
            let table = read32(device.address, pointer + 4);
            return Some(Msix {
                pointer,
                table_bar: (table & 7) as u8,
                table_offset: table & !7,
                entries: (control & 0x7FF) + 1,
            });
        }
        pointer = read8(device.address, pointer + 1) & 0xFC;
    }
    None
}

impl Msix {
    /// Aims entry `entry` of the table at `vector` of the LAPIC `apic_id`
    /// (fixed delivery, edge, physical destination), unmasked, and enables
    /// MSI-X on the function.
    ///
    /// The function is masked while the entry is written, so that no
    /// message leaves with half an address.
    ///
    /// # Safety
    /// `table` must be the virtual address of this capability's table,
    /// mapped uncached, and `vector` must have a handler in the IDT.
    pub unsafe fn route(&self, device: &Device, table: u64, entry: u16, vector: u8, apic_id: u8) {
        let control = read16(device.address, self.pointer + 2);
        unsafe { write16(device.address, self.pointer + 2, control | MSIX_ENABLE | MSIX_FUNCTION_MASK) };
        let slot = table + entry as u64 * 16;
        // SAFETY: the caller maps the table; each entry is four 32-bit
        // registers: address low, address high, data, vector control.
        unsafe {
            ((slot) as *mut u32).write_volatile(0xFEE0_0000 | (apic_id as u32) << 12);
            ((slot + 4) as *mut u32).write_volatile(0);
            ((slot + 8) as *mut u32).write_volatile(vector as u32);
            ((slot + 12) as *mut u32).write_volatile(0);
        }
        let control = read16(device.address, self.pointer + 2);
        unsafe { write16(device.address, self.pointer + 2, (control | MSIX_ENABLE) & !MSIX_FUNCTION_MASK) };
    }
}

/// One discovered PCI function.
#[derive(Debug, Clone, Copy)]
pub struct Device {
    pub address: Address,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
}

impl Device {
    /// Reads a BAR and returns its base address.
    ///
    /// The four low bits carry type indicators, not address: a memory BAR
    /// aligns on 16 bytes at least. A 64-bit BAR borrows the next
    /// register for its high half.
    pub fn bar(&self, index: u8) -> u64 {
        let offset = offsets::BAR0 + index * 4;
        let low = read32(self.address, offset);

        if low & 1 != 0 {
            // I/O space BAR: the two low bits are indicators, the address
            // fits in 16 useful bits.
            return (low & 0xFFFF_FFFC) as u64;
        }

        let is_64bit = (low >> 1) & 0b11 == 0b10;
        let base = (low & 0xFFFF_FFF0) as u64;

        if is_64bit {
            let high = read32(self.address, offset + 4);
            base | ((high as u64) << 32)
        } else {
            base
        }
    }

    /// Arms bits of the command register without touching the others.
    ///
    /// # Safety
    /// Enables the memory response or bus mastering: the device will be
    /// able to write to memory by DMA.
    pub unsafe fn enable(&self, bits: u16) {
        let current = read16(self.address, offsets::COMMAND);
        unsafe { write16(self.address, offsets::COMMAND, current | bits) };
    }

    /// Class label, for the boot log.
    pub fn class_name(&self) -> &'static str {
        match (self.class, self.subclass) {
            (0x01, 0x01) => "controleur IDE",
            (0x01, 0x06) => "controleur AHCI",
            (0x01, 0x08) => "controleur NVMe",
            (0x01, _) => "controleur de stockage",
            (0x02, _) => "controleur reseau",
            (0x03, _) => "controleur graphique",
            (0x04, _) => "controleur multimedia",
            (0x06, 0x00) => "pont hote",
            (0x06, 0x01) => "pont ISA",
            (0x06, _) => "pont",
            (0x0C, 0x03) => "controleur USB",
            _ => "peripherique",
        }
    }
}

/// Is a function present at this address?
fn probe(address: Address) -> Option<Device> {
    let vendor_id = read16(address, offsets::VENDOR_ID);
    // 0xFFFF is what the bus returns when nobody answers.
    if vendor_id == 0xFFFF {
        return None;
    }

    Some(Device {
        address,
        vendor_id,
        device_id: read16(address, offsets::DEVICE_ID),
        class: read8(address, offsets::CLASS),
        subclass: read8(address, offsets::SUBCLASS),
        prog_if: read8(address, offsets::PROG_IF),
    })
}

/// Walks the whole configuration space.
pub fn enumerate() -> Vec<Device> {
    let mut devices = Vec::new();

    for bus in 0..=255u16 {
        for slot in 0..32u8 {
            // Function 0 must exist for the following ones to be
            // consulted; its header says whether the device is
            // multi-function.
            let base = Address::new(bus as u8, slot, 0);
            let Some(device) = probe(base) else {
                continue;
            };
            let multifunction = read8(base, offsets::HEADER_TYPE) & 0x80 != 0;
            devices.push(device);

            if !multifunction {
                continue;
            }
            for function in 1..8u8 {
                if let Some(device) = probe(Address::new(bus as u8, slot, function)) {
                    devices.push(device);
                }
            }
        }
    }

    devices
}

/// Looks for the first function matching a given class.
pub fn find(class: u8, subclass: u8) -> Option<Device> {
    enumerate()
        .into_iter()
        .find(|d| d.class == class && d.subclass == subclass)
}

pub fn dump_state() {
    let devices = enumerate();
    crate::serial_println!("[pci] {} fonctions detectees", devices.len());
    for device in devices.iter() {
        crate::serial_println!(
            "[pci]   {:02x}:{:02x}.{} - {:04x}:{:04x} classe {:02x}.{:02x}.{:02x} - {}",
            device.address.bus,
            device.address.device,
            device.address.function,
            device.vendor_id,
            device.device_id,
            device.class,
            device.subclass,
            device.prog_if,
            device.class_name()
        );
    }
}

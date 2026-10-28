//! 5.6.2 -- Domain 0x06, hardware (spec 6.6).
//!
//! ## Who has the right to enter
//!
//! 6.6 reserves this domain to the `.dsa`, the `.ss` and the `.ras`. The
//! rule is applied on the **type of the binary**, not on the account: a
//! `.cus` launched by ROOT stays a `.cus`, and obtains nothing. That is
//! coherent with 3.4, which makes the format the source of every
//! structural right -- a program does not change nature because it is run
//! differently.
//!
//! The internal kernel tasks reach it too: they have no binary format,
//! and refusing them access would make the self-test unable to check
//! anything at all.
//!
//! ## What this domain is not
//!
//! It **describes** the hardware, it does not drive it. No call opens an
//! I/O port nor maps device memory: those operations belong to the
//! drivers of step 6.1, which talk to their clients through a named IPC
//! channel. Giving direct port access here would make `SYS_HW_*` a way
//! round the whole microkernel architecture.
//!
//! `DRIVER_LIST` (6.1.6) is the exception that proves the rule: it gives
//! no handle on a device, it says **which driver serves it and under what
//! name**. That is what a program needs to find the channel to address,
//! and it is all it gets.

use alloc::vec::Vec;

use super::error::{self, SysResult};
use super::table::{Entry, entry};
use super::Call;
use crate::process::BinaryType;

pub const SYS_HW_INFO: u16 = 0x0601;
pub const SYS_HW_CPU_LIST: u16 = 0x0602;
pub const SYS_HW_MEMORY: u16 = 0x0603;
pub const SYS_HW_PCI_LIST: u16 = 0x0604;
pub const SYS_HW_DISK_INFO: u16 = 0x0605;
pub const SYS_HW_DRIVER_LIST: u16 = 0x0606;

pub static TABLE: &[Entry] = &[
    entry!(SYS_HW_INFO, "INFO", Sync, info),
    entry!(SYS_HW_CPU_LIST, "CPU_LIST", Sync, cpu_list),
    entry!(SYS_HW_MEMORY, "MEMORY", Sync, memory),
    entry!(SYS_HW_PCI_LIST, "PCI_LIST", Sync, pci_list),
    entry!(SYS_HW_DISK_INFO, "DISK_INFO", Sync, disk_info),
    entry!(SYS_HW_DRIVER_LIST, "DRIVER_LIST", Sync, driver_list),
];

/// Access check of 6.6.
fn admitted(call: &Call) -> SysResult<()> {
    if call.origin == super::Origin::Kernel {
        return Ok(());
    }
    match crate::scheduler::task_info(call.pid) {
        Some(info) => match info.binary_type {
            BinaryType::Dsa | BinaryType::Ss | BinaryType::Ras | BinaryType::Kernel => Ok(()),
            other => Err(error::denied(other.as_str())),
        },
        None => Err(error::missing("tache appelante")),
    }
}

/// Layout of the general state of the machine.
///
/// ```text
///   0x00  8  physical frames in total
///   0x08  8  free frames
///   0x10  8  heap bytes allocated
///   0x18  8  heap bytes free
///   0x20  8  ticks since boot
///   0x28  4  cores online
///   0x2C  1  NX bit armed
///   0x2D  1  wall clock available
///   0x2E  2  timer frequency in Hz
///   0x30  8  PCI devices detected
/// ```
pub const HW_INFO_SIZE: usize = 0x38;

fn info(call: &mut Call) -> SysResult<u64> {
    admitted(call)?;

    let (total, _, free) = crate::frame::stats();
    let (allocated, free_heap, _, _) = crate::heap::stats();

    let mut record = [0u8; HW_INFO_SIZE];
    record[0x00..0x08].copy_from_slice(&(total as u64).to_le_bytes());
    record[0x08..0x10].copy_from_slice(&(free as u64).to_le_bytes());
    record[0x10..0x18].copy_from_slice(&(allocated as u64).to_le_bytes());
    record[0x18..0x20].copy_from_slice(&(free_heap as u64).to_le_bytes());
    record[0x20..0x28].copy_from_slice(&crate::idt::ticks().to_le_bytes());
    record[0x28..0x2C].copy_from_slice(&(crate::smp_core_count() as u32).to_le_bytes());
    record[0x2C] = u8::from(crate::memory::nx_enabled());
    record[0x2D] = u8::from(crate::rtc::is_available());
    record[0x2E..0x30].copy_from_slice(&(crate::TIMER_HZ as u16).to_le_bytes());
    record[0x30..0x38].copy_from_slice(&(crate::pci::enumerate().len() as u64).to_le_bytes());

    call.write_bytes(call.arg(0), call.arg(1), &record)
}

/// Layout of a core descriptor.
///
/// ```text
///   0x00  4  logical index
///   0x04  4  LAPIC identifier
///   0x08  8  ticks handled by this core
///   0x10  1  online
///   0x11  7  reserved
/// ```
pub const CPU_ENTRY_SIZE: usize = 0x18;

fn cpu_list(call: &mut Call) -> SysResult<u64> {
    admitted(call)?;

    let cores = crate::smp_core_count();
    let capacity = (call.arg(1) as usize) / CPU_ENTRY_SIZE;
    let count = cores.min(capacity);

    let mut buffer = Vec::with_capacity(count * CPU_ENTRY_SIZE);
    for cpu in 0..count {
        let mut record = [0u8; CPU_ENTRY_SIZE];
        record[0x00..0x04].copy_from_slice(&(cpu as u32).to_le_bytes());
        record[0x04..0x08].copy_from_slice(&(crate::smp::lapic_of(cpu).unwrap_or(0) as u32).to_le_bytes());
        record[0x08..0x10].copy_from_slice(&crate::smp::cpu_ticks(cpu).to_le_bytes());
        record[0x10] = 1;
        buffer.extend_from_slice(&record);
    }

    call.write_bytes(call.arg(0), call.arg(1), &buffer)?;
    Ok(cores as u64)
}

/// Layout of the memory state.
///
/// ```text
///   0x00  8  frames in total
///   0x08  8  frames taken
///   0x10  8  free frames
///   0x18  8  heap bytes allocated
///   0x20  8  heap bytes free
///   0x28  8  heap allocations
///   0x30  8  heap frees
///   0x38  8  pages of the user arena
/// ```
pub const HW_MEMORY_SIZE: usize = 0x40;

fn memory(call: &mut Call) -> SysResult<u64> {
    admitted(call)?;

    let (total, used, free) = crate::frame::stats();
    let (allocated, free_heap, allocations, deallocations) = crate::heap::stats();
    let (pages, _, _) = super::domain_mem::arena_stats();

    let mut record = [0u8; HW_MEMORY_SIZE];
    record[0x00..0x08].copy_from_slice(&(total as u64).to_le_bytes());
    record[0x08..0x10].copy_from_slice(&(used as u64).to_le_bytes());
    record[0x10..0x18].copy_from_slice(&(free as u64).to_le_bytes());
    record[0x18..0x20].copy_from_slice(&(allocated as u64).to_le_bytes());
    record[0x20..0x28].copy_from_slice(&(free_heap as u64).to_le_bytes());
    record[0x28..0x30].copy_from_slice(&(allocations as u64).to_le_bytes());
    record[0x30..0x38].copy_from_slice(&(deallocations as u64).to_le_bytes());
    record[0x38..0x40].copy_from_slice(&(pages as u64).to_le_bytes());

    call.write_bytes(call.arg(0), call.arg(1), &record)
}

/// Layout of a PCI device descriptor.
///
/// ```text
///   0x00  2  vendor identifier
///   0x02  2  product identifier
///   0x04  1  bus
///   0x05  1  device
///   0x06  1  function
///   0x07  1  class
///   0x08  1  subclass
///   0x09  1  programming interface
///   0x0A  6  reserved
/// ```
pub const PCI_ENTRY_SIZE: usize = 0x10;

fn pci_list(call: &mut Call) -> SysResult<u64> {
    admitted(call)?;

    let devices = crate::pci::enumerate();
    let capacity = (call.arg(1) as usize) / PCI_ENTRY_SIZE;
    let count = devices.len().min(capacity);

    let mut buffer = Vec::with_capacity(count * PCI_ENTRY_SIZE);
    for device in devices.iter().take(count) {
        let mut record = [0u8; PCI_ENTRY_SIZE];
        record[0x00..0x02].copy_from_slice(&device.vendor_id.to_le_bytes());
        record[0x02..0x04].copy_from_slice(&device.device_id.to_le_bytes());
        record[0x04] = device.address.bus;
        record[0x05] = device.address.device;
        record[0x06] = device.address.function;
        record[0x07] = device.class;
        record[0x08] = device.subclass;
        record[0x09] = device.prog_if;
        buffer.extend_from_slice(&record);
    }

    call.write_bytes(call.arg(0), call.arg(1), &buffer)?;
    Ok(devices.len() as u64)
}

/// `DISK_INFO(index, buffer, size)` -- **unmounted** medium.
///
/// The mounted volumes are listed by `SYS_FS_DISK_LIST`; what a disk
/// manager lacks is the inventory of the media the system left intact
/// because it recognised no volume on them.
///
/// ```text
///   0x00  8  blocks of 4 KiB
///   0x08  1  length of the name
///   0x09 55  name of the device
/// ```
pub const DISK_INFO_SIZE: usize = 0x40;

fn disk_info(call: &mut Call) -> SysResult<u64> {
    admitted(call)?;
    if !crate::fs::is_ready() {
        return Err(error::unsupported("aucun volume monte"));
    }

    let media = crate::fs::media_list();
    let index = call.arg(0) as usize;
    let Some((name, blocks)) = media.get(index) else {
        return Err(error::missing("index de support"));
    };

    let mut record = [0u8; DISK_INFO_SIZE];
    record[0x00..0x08].copy_from_slice(&blocks.to_le_bytes());
    let bytes = name.as_bytes();
    let length = bytes.len().min(DISK_INFO_SIZE - 0x09);
    record[0x08] = length as u8;
    record[0x09..0x09 + length].copy_from_slice(&bytes[..length]);

    call.write_bytes(call.arg(1), call.arg(2), &record)?;
    Ok(media.len() as u64)
}

// ============================================================
//  6.1.6 -- Declared drivers
// ============================================================

/// Layout of a driver descriptor.
///
/// ```text
///   0x00  1  family: 1 input, 2 storage, 3 network, 4 audio
///   0x01  1  state: 0 starting, 1 ready, 2 absent, 3 failed
///   0x02  1  interrupt vector, zero if there is none
///   0x03  1  length of the channel name
///   0x04  4  PID of the driver task
///   0x08  8  events handled
///   0x10  32 channel name, padded with zeros
/// ```
///
/// The **channel name** is the useful field: a program wanting the
/// keyboard does not need the PID of the driver, it needs the name to
/// subscribe to. The PID is attached for diagnostics, not for use.
pub const DRIVER_ENTRY_SIZE: usize = 0x30;

/// Maximum length of a channel name in a descriptor.
const CHANNEL_MAX: usize = 32;

fn driver_list(call: &mut Call) -> SysResult<u64> {
    admitted(call)?;

    let drivers = crate::drivers::list();
    let capacity = (call.arg(1) as usize) / DRIVER_ENTRY_SIZE;
    let count = drivers.len().min(capacity);

    let mut buffer = Vec::with_capacity(count * DRIVER_ENTRY_SIZE);
    for driver in drivers.iter().take(count) {
        let mut record = [0u8; DRIVER_ENTRY_SIZE];
        record[0x00] = driver.class.code();
        record[0x01] = driver.state.code();
        record[0x02] = driver.vector.unwrap_or(0);

        let name = driver.channel.as_bytes();
        let length = name.len().min(CHANNEL_MAX);
        record[0x03] = length as u8;
        record[0x04..0x08].copy_from_slice(&(driver.pid as u32).to_le_bytes());
        record[0x08..0x10].copy_from_slice(&driver.events.to_le_bytes());
        record[0x10..0x10 + length].copy_from_slice(&name[..length]);

        buffer.extend_from_slice(&record);
    }

    call.write_bytes(call.arg(0), call.arg(1), &buffer)?;
    Ok(drivers.len() as u64)
}

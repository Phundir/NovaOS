//! QEMU firmware configuration (`fw_cfg`): named files the host hands the
//! guest, read over two I/O ports.
//!
//! ## Why it exists here
//!
//! The kernel boots from an ISO through GRUB, whose command line is fixed
//! in `grub.cfg`: QEMU cannot add a parameter to it. `fw_cfg` is how QEMU
//! passes a value anyway -- `-fw_cfg name=opt/nova/<key>,string=<value>`
//! -- and it is how a test harness asks for something the normal boot
//! must not do, such as the network throughput measurement
//! (`net::bench`). A machine that is not QEMU has no such device: the
//! signature check fails and every lookup answers `None`.
//!
//! ## Protocol (QEMU `docs/specs/fw_cfg.rst`)
//!
//! A 16-bit selector is written to port 0x510, then the item is read byte
//! by byte from port 0x511. Selector 0 holds the signature `QEMU`;
//! selector 0x19 the file directory: a big-endian count, then 64-byte
//! entries `size (u32 BE), select (u16 BE), reserved (u16), name[56]`.
//! Every multi-byte field of the directory is big-endian, unlike the rest
//! of the machine.
//!
//! Selecting and reading are two steps on shared ports: a lock keeps two
//! cores from interleaving them.

use alloc::string::String;
use alloc::vec::Vec;

use crate::port::{inb, outw};
use crate::vga::SpinLock;

const PORT_SELECTOR: u16 = 0x510;
const PORT_DATA: u16 = 0x511;

const SELECT_SIGNATURE: u16 = 0x0000;
const SELECT_DIRECTORY: u16 = 0x0019;

/// Largest file read: the values carried are short strings.
const MAX_FILE: usize = 4096;
/// Largest directory walked: QEMU publishes a few dozen files.
const MAX_ENTRIES: u32 = 1024;

static PORTS: SpinLock<()> = SpinLock::new(());

/// Selects an item and reads `buffer.len()` bytes of it.
fn read_item(selector: u16, buffer: &mut [u8]) {
    // Port I/O on the fw_cfg device: harmless on a machine without it,
    // where the reads return 0xFF and the signature check fails.
    unsafe {
        outw(PORT_SELECTOR, selector);
        for byte in buffer.iter_mut() {
            *byte = inb(PORT_DATA);
        }
    }
}

/// Reads the named file, if QEMU published it. Only the start of a file
/// larger than `MAX_FILE` is returned.
pub fn read_file(name: &str) -> Option<Vec<u8>> {
    let _guard = PORTS.lock();

    let mut signature = [0u8; 4];
    read_item(SELECT_SIGNATURE, &mut signature);
    if &signature != b"QEMU" {
        return None;
    }

    // The directory is one item read sequentially: the count, then the
    // entries, without selecting again in between.
    let mut count = [0u8; 4];
    read_item(SELECT_DIRECTORY, &mut count);
    let count = u32::from_be_bytes(count).min(MAX_ENTRIES);
    let mut found: Option<(u16, usize)> = None;
    for _ in 0..count {
        let mut entry = [0u8; 64];
        for byte in entry.iter_mut() {
            *byte = unsafe { inb(PORT_DATA) };
        }
        let size = u32::from_be_bytes([entry[0], entry[1], entry[2], entry[3]]) as usize;
        let select = u16::from_be_bytes([entry[4], entry[5]]);
        let raw = &entry[8..];
        let length = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        if found.is_none() && &raw[..length] == name.as_bytes() {
            found = Some((select, size));
        }
    }

    let (select, size) = found?;
    let mut data = alloc::vec![0u8; size.min(MAX_FILE)];
    read_item(select, &mut data);
    Some(data)
}

/// A file of `opt/nova/`, as trimmed printable ASCII.
pub fn option(key: &str) -> Option<String> {
    let data = read_file(&alloc::format!("opt/nova/{}", key))?;
    let text: String = data
        .iter()
        .take_while(|&&b| b != 0)
        .filter(|&&b| (0x20..0x7F).contains(&b))
        .map(|&b| b as char)
        .collect();
    let text = text.trim();
    if text.is_empty() { None } else { Some(String::from(text)) }
}

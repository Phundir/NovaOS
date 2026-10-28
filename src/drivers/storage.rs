//! 6.1.3 -- Storage service.
//!
//! ## What this service is, and what it is not
//!
//! The phase 4 AHCI driver lives in `src/ahci.rs` and serves the
//! filesystem by function call. Moving it entirely behind IPC would mean
//! passing **every 4 KiB block** of the cache through a mailbox, when an
//! IPC payload caps at 256 bytes and a real transfer demands shared
//! memory -- which opens no access yet, for want of a CR3 per process
//! (limit opened by phase 3). Doing it today would replace a correct path
//! with a slow and wrong one.
//!
//! What is done here, and has value from now on:
//!
//! * storage **exposes a named service**, `dev.storage`, which any task
//!   reaches without knowing a PID;
//! * enumerating the media, their state and their flush go through that
//!   service, no longer through a kernel function;
//! * a raw read is possible, bounded by the size of a message: that is
//!   what is needed to inspect a superblock or a partition table, and it
//!   is explicitly insufficient for a stream.
//!
//! The data path of the filesystem, on the other hand, keeps going
//! straight through the cache. It is an acknowledged limit, written in
//! the roadmap, and it will fall with real shared memory.
//!
//! ## NVMe
//!
//! The PCI scan recognises class 01.08 and declares it. No driver is
//! written: an admin queue, an I/O queue and doorbells demand work
//! comparable to that of AHCI, and nothing would validate it as long as
//! the test machine exposes no such controller. Declaring it present and
//! not driven is accurate; claiming the opposite would not be.
//! ## Protocol
//! ## Protocole
//! Request, sixteen bytes:
//! Requete, seize octets :
//!
//! ```text
//!   0x00  1  operation: 1 LIST, 2 INFO, 3 READ, 4 FLUSH
//!   0x01  1  volume index, for INFO and READ
//!   0x02  2  length requested, for READ
//!   0x04  4  reserved
//!   0x08  8  byte offset, for READ
//! ```
//!
//! Answer of LIST and INFO, sixteen bytes per volume:
//!
//! ```text
//!   0x00  1  letter
//!   0x01  1  medium: 0 RAM, 1 SATA, 2 removable
//!   0x02  1  read only
//!   0x03  1  reserved
//!   0x04  4  total blocks
//!   0x08  4  free blocks
//!   0x0C  4  free nodes
//! ```

use alloc::vec::Vec;

use crate::ipc::{self, Message, MessageKind};

use super::DriverClass;

pub const CHANNEL: &str = "dev.storage";

/// Size of a volume descriptor in an answer.
pub const VOLUME_ENTRY: usize = 16;

pub const OP_LIST: u8 = 1;
pub const OP_INFO: u8 = 2;
pub const OP_READ: u8 = 3;
pub const OP_FLUSH: u8 = 4;

/// Maximum length returned by a raw read.
///
/// The payload of a message caps at 256 bytes and the header consumes
/// sixteen; the rest is rounded to a power of two so that the caller can
/// reason in sectors.
pub const READ_MAX: usize = 128;

/// PCI class of an NVMe controller.
const PCI_CLASS_STORAGE: u8 = 0x01;
const PCI_SUBCLASS_NVME: u8 = 0x08;

fn describe(index: usize, info: &crate::fs::DiskInfo) -> [u8; VOLUME_ENTRY] {
    let _ = index;
    let mut record = [0u8; VOLUME_ENTRY];
    record[0x00] = info.letter as u8;
    record[0x01] = match info.medium {
        "RAM" => 0,
        "SATA" => 1,
        _ => 2,
    };
    record[0x04..0x08].copy_from_slice(&(info.total_blocks as u32).to_le_bytes());
    record[0x08..0x0C].copy_from_slice(&(info.free_blocks as u32).to_le_bytes());
    record[0x0C..0x10].copy_from_slice(&(info.free_nodes as u32).to_le_bytes());
    record
}

/// Handles a request and returns the payload of the answer.
///
/// An empty answer signals a refusal: the caller asked for a volume that
/// does not exist, or for an unknown operation.
fn serve(request: &[u8]) -> Vec<u8> {
    if request.len() < 16 {
        return Vec::new();
    }

    let operation = request[0];
    let index = request[1] as usize;
    let length = u16::from_le_bytes([request[2], request[3]]) as usize;
    let offset = u64::from_le_bytes(request[0x08..0x10].try_into().unwrap_or([0; 8]));

    let disks = crate::fs::disk_list();

    match operation {
        OP_LIST => {
            let mut answer = Vec::new();
            for (position, disk) in disks.iter().enumerate() {
                if answer.len() + VOLUME_ENTRY > ipc::MAX_PAYLOAD {
                    break;
                }
                answer.extend_from_slice(&describe(position, disk));
            }
            answer
        }
        OP_INFO => match disks.get(index) {
            Some(disk) => describe(index, disk).to_vec(),
            None => Vec::new(),
        },
        OP_READ => {
            let Some(disk) = disks.get(index) else {
                return Vec::new();
            };
            let wanted = length.min(READ_MAX);
            // A read that fails returns an empty answer, that is to say a
            // refusal: the service never fabricates zeros the caller
            // would take for data.
            crate::fs::raw_read(disk.letter, offset, wanted).unwrap_or_default()
        }
        OP_FLUSH => {
            alloc::vec![u8::from(crate::fs::flush_all().is_ok())]
        }
        _ => Vec::new(),
    }
}

/// Driver task.
pub fn task() -> ! {
    let handle = match super::register("stockage", DriverClass::Storage, CHANNEL) {
        Some(handle) => handle,
        None => crate::scheduler::exit_current_with(-1),
    };

    if ipc::open_channel(CHANNEL).is_err() {
        super::failed(handle, "canal deja ouvert");
        crate::scheduler::exit_current_with(-1);
    }

    // Enumeration of the hardware: what the service drives, and what it
    // sees without driving it.
    let mut description = alloc::string::String::new();
    let disks = crate::fs::disk_list();
    description.push_str(&alloc::format!("{} volume(s) montes", disks.len()));

    let nvme = crate::pci::enumerate()
        .into_iter()
        .filter(|d| d.class == PCI_CLASS_STORAGE && d.subclass == PCI_SUBCLASS_NVME)
        .count();
    if nvme != 0 {
        description.push_str(&alloc::format!(", {} controleur(s) NVMe non pilotes", nvme));
        crate::serial_println!(
            "[drv-stockage] {} controleur(s) NVMe detectes - aucun pilote (limite connue)",
            nvme
        );
    }

    if disks.is_empty() {
        super::absent(handle, "aucun volume monte");
    } else {
        super::ready(handle, &description, None);
    }
    crate::serial_println!("[drv-stockage] pret - canal \"{}\"", CHANNEL);

    loop {
        let message = ipc::recv();
        if message.kind != MessageKind::Request {
            continue;
        }

        let answer = serve(&message.payload);
        let kind = if answer.is_empty() {
            MessageKind::Signal
        } else {
            MessageKind::Response
        };

        let reply = Message::new(crate::scheduler::current_pid(), kind, message.tag, &answer);
        if ipc::send(message.sender, reply).is_err() {
            super::count_error(handle);
        } else {
            super::count_event(handle, 1);
        }
    }
}

//! Virtio 1.x over PCI: finding the register blocks, negotiating the
//! features, and bringing up the two queues of virtio-gpu.
//!
//! ## Two queues
//!
//! virtio-gpu has a **control** queue (number 0), which carries every
//! command, and a **cursor** queue (number 1), which carries only
//! `UPDATE_CURSOR` and `MOVE_CURSOR`. The cursor queue exists so that the
//! pointer moves without waiting behind a long command stream: the host
//! services it apart. The control queue may interrupt (MSI-X, see
//! `queue`); the cursor queue never needs to -- its answers say nothing
//! but "done", and are reaped when the next cursor command is posted.

use crate::memory::{self, flags};
use crate::pci::{self, Device};

use super::queue::{Queue, CACHE_PRIMED};

// --- PCI capability of a virtio register block (virtio 1.2, 4.1.4) ---
const CAP_VENDOR: u8 = 0x09;
const CFG_COMMON: u8 = 1;
const CFG_NOTIFY: u8 = 2;
const CFG_DEVICE: u8 = 4;
/// `VIRTIO_PCI_CAP_SHARED_MEMORY_CFG`: a window of memory the device
/// shares with the driver (virtio 1.2, 4.1.4.7).
const CFG_SHARED_MEMORY: u8 = 8;
/// Shared memory region of virtio-gpu holding the host-visible blobs
/// (`VIRTIO_GPU_SHM_ID_HOST_VISIBLE`).
const SHM_HOST_VISIBLE: u8 = 1;

// --- common configuration, byte offsets ---
const DEVICE_FEATURE_SELECT: u64 = 0;
const DEVICE_FEATURE: u64 = 4;
const DRIVER_FEATURE_SELECT: u64 = 8;
const DRIVER_FEATURE: u64 = 12;
const MSIX_CONFIG: u64 = 16;
const DEVICE_STATUS: u64 = 20;
pub(super) const QUEUE_SELECT: u64 = 22;
pub(super) const QUEUE_SIZE: u64 = 24;
pub(super) const QUEUE_MSIX_VECTOR: u64 = 26;
pub(super) const QUEUE_ENABLE: u64 = 28;
pub(super) const QUEUE_NOTIFY_OFF: u64 = 30;
pub(super) const QUEUE_DESC: u64 = 32;
pub(super) const QUEUE_DRIVER: u64 = 40;
pub(super) const QUEUE_DEVICE: u64 = 48;

// --- device status ---
const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;
const STATUS_FAILED: u8 = 128;

// --- feature bits (low word, then high word) ---
/// `VIRTIO_GPU_F_VIRGL`: 3D commands (contexts, `SUBMIT_3D`).
pub const FEATURE_VIRGL: u32 = 1 << 0;
/// `VIRTIO_GPU_F_RESOURCE_BLOB`: resources whose memory the host owns.
pub const FEATURE_RESOURCE_BLOB: u32 = 1 << 3;
/// `VIRTIO_GPU_F_CONTEXT_INIT`: a context names its protocol (capset).
pub const FEATURE_CONTEXT_INIT: u32 = 1 << 4;
/// `VIRTIO_RING_F_INDIRECT_DESC`, bit 28: see `queue`.
const FEATURE_INDIRECT_DESC: u32 = 1 << 28;
/// `VIRTIO_F_VERSION_1`, bit 32: the modern interface.
const FEATURE_VERSION_1_HIGH: u32 = 1;

/// The 3D features, asked for together or not at all: a context without
/// blobs cannot have memory the program maps, and one without
/// `CONTEXT_INIT` would be a virgl context, not a Venus one.
pub const FEATURES_3D: u32 = FEATURE_VIRGL | FEATURE_RESOURCE_BLOB | FEATURE_CONTEXT_INIT;

pub(super) const NO_VECTOR: u16 = 0xFFFF;

/// Ring entries of each queue, at most. The control queue holds many
/// requests in flight; the cursor queue never more than a few.
const CONTROL_ENTRIES: u16 = 256;
const CURSOR_ENTRIES: u16 = 16;

const PAGE: u64 = 4096;

/// Virtual window where the register blocks are mapped uncached.
///
/// Mapped explicitly rather than read through the physical window: the
/// firmware may place a 64-bit BAR above the RAM, where that window stops,
/// and `set_mmio_uncached` works on a whole gibibyte of the window --
/// which could be RAM as well.
const MMIO_VMA: u64 = super::super::FRONT_VMA + 3 * super::SLOT;

pub struct Transport {
    common: u64,
    pub(super) notify: u64,
    device: u64,
    /// `notify_off_multiplier` of the notify capability.
    pub(super) notify_multiplier: u32,
}

impl Transport {
    pub(super) fn read8(&self, offset: u64) -> u8 {
        unsafe { ((self.common + offset) as *const u8).read_volatile() }
    }
    pub(super) fn write8(&self, offset: u64, value: u8) {
        unsafe { ((self.common + offset) as *mut u8).write_volatile(value) }
    }
    pub(super) fn read16(&self, offset: u64) -> u16 {
        unsafe { ((self.common + offset) as *const u16).read_volatile() }
    }
    pub(super) fn write16(&self, offset: u64, value: u16) {
        unsafe { ((self.common + offset) as *mut u16).write_volatile(value) }
    }
    pub(super) fn read32(&self, offset: u64) -> u32 {
        unsafe { ((self.common + offset) as *const u32).read_volatile() }
    }
    pub(super) fn write32(&self, offset: u64, value: u32) {
        unsafe { ((self.common + offset) as *mut u32).write_volatile(value) }
    }
    /// A 64-bit field, as two 32-bit writes -- low half first, which the
    /// specification allows and which avoids depending on 64-bit MMIO.
    pub(super) fn write64(&self, offset: u64, value: u64) {
        self.write32(offset, value as u32);
        self.write32(offset + 4, (value >> 32) as u32);
    }

    /// Where the device-specific configuration is mapped, for a reader
    /// that must not hold the device's lock (`super::poll_resize`).
    pub fn device_base(&self) -> u64 {
        self.device
    }

    /// A 32-bit field of the device-specific configuration.
    pub fn device_read32(&self, offset: u64) -> u32 {
        unsafe { ((self.device + offset) as *const u32).read_volatile() }
    }

    pub fn device_write32(&self, offset: u64, value: u32) {
        unsafe { ((self.device + offset) as *mut u32).write_volatile(value) }
    }

    /// Declares the device failed: it stops using the driver's memory.
    pub(super) fn fail(&self) {
        self.write8(DEVICE_STATUS, STATUS_FAILED);
    }
}

/// Maps `[phys, phys + length)` uncached, at the next free place of the
/// MMIO window. Returns the virtual address of `phys`.
///
/// # Safety
/// `phys` must be device memory described by a BAR.
unsafe fn map_mmio(next: &mut u64, phys: u64, length: u64) -> Option<u64> {
    let base = phys & !(PAGE - 1);
    let end = phys.checked_add(length.max(1))?;
    let pages = (end - base).div_ceil(PAGE);
    let mut page_flags = flags::PRESENT | flags::WRITABLE | flags::NO_CACHE | flags::WRITE_THROUGH;
    if memory::nx_enabled() {
        page_flags |= flags::NO_EXECUTE;
    }
    let virt = *next;
    for page in 0..pages {
        let offset = page * PAGE;
        unsafe { memory::map_page(virt + offset, base + offset, page_flags) }.ok()?;
    }
    *next += pages * PAGE;
    Some(virt + (phys - base))
}

/// A register block found in the capability list.
#[derive(Clone, Copy)]
struct Block {
    bar: u8,
    offset: u64,
    length: u64,
    /// Only meaningful for the notify block.
    multiplier: u32,
}

/// The blocks of the capability list the driver uses.
struct Blocks {
    common: Block,
    notify: Block,
    specific: Block,
    /// The host-visible shared memory region, when the device has one
    /// (`hostmem=` on QEMU's command line).
    shared: Option<Block>,
}

/// Walks the capability list.
fn find_blocks(device: &Device) -> Option<Blocks> {
    // Bit 4 of the status register: a capability list exists.
    if pci::read16(device.address, pci::offsets::STATUS) & (1 << 4) == 0 {
        return None;
    }
    let (mut common, mut notify, mut specific, mut shared) = (None, None, None, None);
    let mut pointer = pci::read8(device.address, 0x34) & 0xFC;
    // Bounded: a malformed list that loops must not hang the boot.
    for _ in 0..48 {
        // The longest capability read is 24 bytes: one starting past 0xE7
        // would run off the 256-byte header -- and `pointer + 20` would
        // overflow the `u8` before that.
        if pointer == 0 || pointer > 0xE7 {
            break;
        }
        let id = pci::read8(device.address, pointer);
        if id == CAP_VENDOR {
            let mut block = Block {
                bar: pci::read8(device.address, pointer + 4),
                offset: pci::read32(device.address, pointer + 8) as u64,
                length: pci::read32(device.address, pointer + 12) as u64,
                multiplier: pci::read32(device.address, pointer + 16),
            };
            match pci::read8(device.address, pointer + 3) {
                CFG_COMMON if common.is_none() => common = Some(block),
                CFG_NOTIFY if notify.is_none() => notify = Some(block),
                CFG_DEVICE if specific.is_none() => specific = Some(block),
                // `virtio_pci_cap64`: the high halves of offset and length
                // follow the ordinary capability. The region's identifier
                // is the byte after the BAR number.
                CFG_SHARED_MEMORY
                    if shared.is_none() && pci::read8(device.address, pointer + 5) == SHM_HOST_VISIBLE =>
                {
                    block.offset |= (pci::read32(device.address, pointer + 16) as u64) << 32;
                    block.length |= (pci::read32(device.address, pointer + 20) as u64) << 32;
                    shared = Some(block);
                }
                _ => {}
            }
        }
        pointer = pci::read8(device.address, pointer + 1) & 0xFC;
    }
    Some(Blocks { common: common?, notify: notify?, specific: specific?, shared })
}

/// What the device agreed to.
pub struct Negotiated {
    /// Low word of the features accepted (GPU bits, indirect descriptors).
    pub features: u32,
    /// Physical address and length of the host-visible region, if any.
    pub shared_memory: Option<(u64, u64)>,
    /// Does the control queue interrupt?
    pub interrupts: bool,
}

/// What `bring_up` hands back.
pub struct Device3 {
    pub transport: Transport,
    pub control: Queue,
    /// `None` when the device has no second queue -- the pointer is then
    /// drawn by the compositor, as before.
    pub cursor: Option<Queue>,
    pub negotiated: Negotiated,
}

/// Brings the device up: control queue, cursor queue.
///
/// `want` is the set of GPU feature bits wished for; the device's answer
/// says which were granted. `msix` aims the control queue at a vector when
/// the function has MSI-X: `(vector, apic_id)`.
///
/// # Safety
/// `device` must be a virtio-gpu function; its BARs are mapped and it is
/// given bus mastering. The vector, if given, must have a relay in the IDT.
pub unsafe fn bring_up(device: &Device, want: u32, msix: Option<(u8, u8)>) -> Result<Device3, &'static str> {
    let blocks = find_blocks(device).ok_or("capacites virtio absentes")?;
    unsafe { device.enable(pci::command::MEMORY_SPACE | pci::command::BUS_MASTER) };

    let mut next = MMIO_VMA;
    let map = |next: &mut u64, block: &Block| -> Option<u64> {
        let bar = device.bar(block.bar);
        if bar == 0 {
            return None;
        }
        unsafe { map_mmio(next, bar + block.offset, block.length) }
    };
    let transport = Transport {
        common: map(&mut next, &blocks.common).ok_or("bloc commun inaccessible")?,
        notify: map(&mut next, &blocks.notify).ok_or("bloc de notification inaccessible")?,
        device: map(&mut next, &blocks.specific).ok_or("bloc de configuration inaccessible")?,
        notify_multiplier: blocks.notify.multiplier,
    };
    // The MSI-X table, when the function has one, in its own BAR window.
    let table = pci::msix(device).and_then(|cap| {
        let table = map(
            &mut next,
            &Block { bar: cap.table_bar, offset: cap.table_offset as u64, length: cap.entries as u64 * 16, multiplier: 0 },
        )?;
        Some((cap, table))
    });

    // --- reset, then the status sequence of 3.1.1 ---
    transport.write8(DEVICE_STATUS, 0);
    for _ in 0..1_000_000 {
        if transport.read8(DEVICE_STATUS) == 0 {
            break;
        }
        core::hint::spin_loop();
    }
    transport.write8(DEVICE_STATUS, STATUS_ACKNOWLEDGE);
    transport.write8(DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

    transport.write32(DEVICE_FEATURE_SELECT, 1);
    if transport.read32(DEVICE_FEATURE) & FEATURE_VERSION_1_HIGH == 0 {
        transport.fail();
        return Err("interface virtio 1.0 non proposee");
    }
    transport.write32(DEVICE_FEATURE_SELECT, 0);
    let offered = transport.read32(DEVICE_FEATURE);
    if offered & FEATURE_INDIRECT_DESC == 0 {
        transport.fail();
        return Err("descripteurs indirects non proposes");
    }
    // The 3D set is all or nothing (see FEATURES_3D).
    let gpu = if offered & want & FEATURES_3D == FEATURES_3D { FEATURES_3D } else { 0 };
    let low = gpu | FEATURE_INDIRECT_DESC;
    transport.write32(DRIVER_FEATURE_SELECT, 0);
    transport.write32(DRIVER_FEATURE, low);
    transport.write32(DRIVER_FEATURE_SELECT, 1);
    transport.write32(DRIVER_FEATURE, FEATURE_VERSION_1_HIGH);

    let status = STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK;
    transport.write8(DEVICE_STATUS, status);
    if transport.read8(DEVICE_STATUS) & STATUS_FEATURES_OK == 0 {
        transport.fail();
        return Err("fonctions refusees par le peripherique");
    }
    transport.write16(MSIX_CONFIG, NO_VECTOR);

    // --- the control queue interrupts when a vector could be routed ---
    let mut vector_entry = NO_VECTOR;
    if let (Some((cap, table)), Some((vector, apic_id))) = (table, msix) {
        // SAFETY: `table` maps this capability's table, uncached, and the
        // caller promises the vector has its relay.
        unsafe { cap.route(device, table, 0, vector, apic_id) };
        vector_entry = 0;
    }
    let (control, interrupts) =
        unsafe { Queue::set_up(&transport, 0, CONTROL_ENTRIES, vector_entry, CACHE_PRIMED) }.inspect_err(|_| transport.fail())?;
    // The cursor queue is polled; a device without one keeps working.
    let cursor = unsafe { Queue::set_up(&transport, 1, CURSOR_ENTRIES, NO_VECTOR, 0) }.ok().map(|(queue, _)| queue);

    transport.write8(DEVICE_STATUS, status | STATUS_DRIVER_OK);

    let shared_memory = blocks.shared.and_then(|block| {
        let bar = device.bar(block.bar);
        (bar != 0 && block.length != 0).then_some((bar + block.offset, block.length))
    });
    Ok(Device3 { transport, control, cursor, negotiated: Negotiated { features: gpu, shared_memory, interrupts } })
}

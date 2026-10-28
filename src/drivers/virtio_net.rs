//! virtio-net -- the paravirtual network card, for throughput.
//!
//! ## Why a second card
//!
//! The e1000 of 6.1.4 is an emulated piece of real hardware: QEMU
//! reproduces its registers one by one, and every register access of the
//! driver is a trap into the emulator. It is also a 1 Gbit/s card, a
//! ceiling of 125 MB/s before any overhead. virtio-net was designed for
//! virtual machines: the rings live in guest memory, the host reads them
//! directly, and the only trap left is the **notification** that new
//! descriptors wait -- which the host can even ask the guest to skip
//! while it is already busy (`USED_NO_NOTIFY`). No line speed is
//! emulated. It is what lets a download go past 200 MB/s.
//!
//! The e1000 stays: it is the card of real machines of the era and the
//! fallback when no virtio device is present. `drivers::nic` probes this
//! driver first, then the e1000, and the stack above sees neither.
//!
//! ## What is negotiated
//!
//! `VERSION_1` (the modern interface), `MAC` (the address in the device
//! configuration) and `STATUS` (the link state) when offered. **No
//! offload**: no checksum delegation, no segmentation, no merged
//! buffers. Each receive buffer holds one whole frame plus the 12-byte
//! header, which is what 2 KiB buffers allow without `MRG_RXBUF`. The
//! offloads would save the checksum of every segment, and would demand
//! the stack accept packets whose checksum is not yet computed -- a
//! second path through TCP for a gain the measurement has not asked for.
//!
//! ## Rings, polling, and one interrupt
//!
//! Two split virtqueues: 0 receives, 1 transmits. Legacy INTx is disabled
//! in the PCI command register: the reasons are those of the e1000 -- a
//! level-triggered line nobody acknowledges would storm. Frames are
//! collected by polling, from the driver task and from whichever task
//! waits on TCP (`nic::poll`).
//!
//! The reception queue alone raises an **MSI-X** message
//! (`drivers::VECTOR_NIC_RX`, aimed at core 0), when the device offers
//! MSI-X. The driver task used to poll every tick: a frame arriving just
//! after a lap waited up to 10 ms -- twenty for a sleep rounded up -- and a
//! page load is a dozen round trips (DNS, TCP, TLS, HTTP, then each
//! resource), each paying it. The message wakes the task at once. While
//! the task drains the ring it switches the interrupt off
//! (`AVAIL_NO_INTERRUPT`, `arm_rx`), and on again before it sleeps, the
//! way Linux's NAPI does: a download of 150 000 frames a second must not
//! become 150 000 interrupts. Transmission never interrupts.
//!
//! Every receive descriptor owns one buffer for good: descriptor `i`
//! always points at buffer `i`. Giving a buffer back is writing its index
//! in the available ring, nothing more. Transmit descriptors work the
//! same way, with a free list refilled from the used ring.
//!
//! ## Register blocks above 4 GiB
//!
//! The modern interface lives in a BAR the firmware may place above
//! the RAM, where the physical window stops. The blocks are therefore
//! mapped explicitly, uncached, at `MMIO_VMA` -- the virtio-gpu driver
//! does the same for the same reason, in its own window.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU16, AtomicU64, Ordering, fence};

use crate::memory::{self, flags, phys_to_virt};
use crate::pci::{self, Device};
use crate::vga::SpinLock;

const VENDOR_VIRTIO: u16 = 0x1AF4;
/// Transitional (0x1000) and modern-only (0x1041) network devices. Both
/// carry the modern capabilities in QEMU; a device without them is
/// refused, since the legacy I/O interface is not driven.
const DEVICES: &[u16] = &[0x1000, 0x1041];

// --- PCI capability of a virtio register block (virtio 1.2, 4.1.4) ---
const CAP_VENDOR: u8 = 0x09;
const CFG_COMMON: u8 = 1;
const CFG_NOTIFY: u8 = 2;
const CFG_DEVICE: u8 = 4;

// --- common configuration, byte offsets ---
const DEVICE_FEATURE_SELECT: u64 = 0;
const DEVICE_FEATURE: u64 = 4;
const DRIVER_FEATURE_SELECT: u64 = 8;
const DRIVER_FEATURE: u64 = 12;
const MSIX_CONFIG: u64 = 16;
const DEVICE_STATUS: u64 = 20;
const QUEUE_SELECT: u64 = 22;
const QUEUE_SIZE: u64 = 24;
const QUEUE_MSIX_VECTOR: u64 = 26;
const QUEUE_ENABLE: u64 = 28;
const QUEUE_NOTIFY_OFF: u64 = 30;
const QUEUE_DESC: u64 = 32;
const QUEUE_DRIVER: u64 = 40;
const QUEUE_DEVICE: u64 = 48;

// --- device status ---
const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;
const STATUS_FAILED: u8 = 128;

// --- features ---
/// `VIRTIO_NET_F_MAC`, bit 5 of the low word.
const FEATURE_MAC: u32 = 1 << 5;
/// `VIRTIO_NET_F_STATUS`, bit 16 of the low word.
const FEATURE_STATUS: u32 = 1 << 16;
/// `VIRTIO_F_VERSION_1`, bit 32: bit 0 of the high word.
const FEATURE_VERSION_1_HIGH: u32 = 1;

// --- device configuration (5.1.4) ---
const CONFIG_MAC: u64 = 0;
const CONFIG_STATUS: u64 = 6;
const LINK_UP: u16 = 1;

const NO_VECTOR: u16 = 0xFFFF;
/// Bit 10 of the PCI command register: legacy INTx disabled.
const COMMAND_INTX_DISABLE: u16 = 1 << 10;

// --- split ring ---
const DESC_WRITE: u16 = 2;
const AVAIL_NO_INTERRUPT: u16 = 1;
const USED_NO_NOTIFY: u16 = 1;

/// Queue depth wanted. 256 descriptors of 16 bytes fill one frame, and
/// 256 receive buffers hold 370 KiB: what arrives between two polls of a
/// task that is not waiting, at 200 MB/s, for about two milliseconds.
const MAX_QUEUE: u16 = 256;
/// Offset of the used ring in the frame shared with the available ring:
/// the available ring of 256 entries takes 518 bytes, the used ring 2054,
/// and 4-byte alignment is all the specification asks.
const USED_OFFSET: u64 = 1024;

/// A buffer: the 12-byte `virtio_net_hdr` (VERSION_1 makes it 12 even
/// without merged buffers), then a whole Ethernet frame.
const BUFFER_SIZE: usize = 2048;
const HEADER_SIZE: usize = 12;
const PAGE: u64 = 4096;

/// Frames taken per poll, so that the lock is never held long.
pub const BATCH: usize = 64;

/// Spins while the transmit ring is full before dropping the frame. The
/// wait is paid with interrupts masked: see the e1000 for the reasoning.
const TX_SPINS: u32 = 20_000;

/// Virtual window of the register blocks. Its own PML4 entry, next to the
/// back buffer of the framebuffer; mapped page by page as needed.
const MMIO_VMA: u64 = 0xFFFF_8200_0000_0000;

fn read8(address: u64) -> u8 {
    unsafe { (address as *const u8).read_volatile() }
}
fn write8(address: u64, value: u8) {
    unsafe { (address as *mut u8).write_volatile(value) }
}
fn read16(address: u64) -> u16 {
    unsafe { (address as *const u16).read_volatile() }
}
fn write16(address: u64, value: u16) {
    unsafe { (address as *mut u16).write_volatile(value) }
}
fn read32(address: u64) -> u32 {
    unsafe { (address as *const u32).read_volatile() }
}
fn write32(address: u64, value: u32) {
    unsafe { (address as *mut u32).write_volatile(value) }
}
/// Low half first, as two 32-bit writes: the specification allows it and
/// it avoids depending on 64-bit MMIO.
fn write64(address: u64, value: u64) {
    write32(address, value as u32);
    write32(address + 4, (value >> 32) as u32);
}

/// A register block found in the capability list.
struct Block {
    bar: u8,
    offset: u32,
    length: u32,
    /// Only meaningful for the notify block.
    multiplier: u32,
}

/// Walks the capability list for the three blocks the driver uses.
fn find_blocks(device: &Device) -> Option<(Block, Block, Block)> {
    // Bit 4 of the status register: a capability list exists.
    if pci::read16(device.address, pci::offsets::STATUS) & (1 << 4) == 0 {
        return None;
    }
    let (mut common, mut notify, mut specific) = (None, None, None);
    let mut pointer = pci::read8(device.address, 0x34) & 0xFC;
    // Bounded: a malformed list that loops must not hang the boot.
    for _ in 0..48 {
        // A capability is read 20 bytes from its start: past 0xEB it would
        // run off the 256-byte header.
        if pointer == 0 || pointer > 0xEB {
            break;
        }
        if pci::read8(device.address, pointer) == CAP_VENDOR {
            let block = Block {
                bar: pci::read8(device.address, pointer + 4),
                offset: pci::read32(device.address, pointer + 8),
                length: pci::read32(device.address, pointer + 12),
                multiplier: pci::read32(device.address, pointer + 16),
            };
            match pci::read8(device.address, pointer + 3) {
                CFG_COMMON if common.is_none() => common = Some(block),
                CFG_NOTIFY if notify.is_none() => notify = Some(block),
                CFG_DEVICE if specific.is_none() => specific = Some(block),
                _ => {}
            }
        }
        pointer = pci::read8(device.address, pointer + 1) & 0xFC;
    }
    Some((common?, notify?, specific?))
}

/// Maps `[phys, phys + length)` uncached at `*next`, and advances it.
/// Returns the virtual address of `phys`.
///
/// # Safety
/// `phys` must be device memory described by a BAR, and `*next` a free
/// virtual range.
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

/// One split virtqueue, with a buffer tied to each descriptor.
struct Queue {
    size: u16,
    /// Physical frame of the descriptor table.
    desc: u64,
    /// Physical frame of the available ring; the used ring is at
    /// `USED_OFFSET` in the same frame.
    rings: u64,
    /// Physical address of the buffer of each descriptor.
    buffers: Vec<u64>,
    /// Next value of the available index.
    avail_index: u16,
    /// Used index already consumed.
    used_seen: u16,
    /// Virtual address of the queue's notification register.
    notify: u64,
    /// The queue number written to `notify`.
    number: u16,
}

impl Queue {
    fn avail(&self) -> u64 {
        phys_to_virt(self.rings)
    }
    fn used(&self) -> u64 {
        phys_to_virt(self.rings) + USED_OFFSET
    }

    fn used_index(&self) -> u16 {
        read16(self.used() + 2)
    }

    /// The used element at a position: (descriptor, length written).
    fn used_element(&self, position: u16) -> (u16, u32) {
        let slot = (position % self.size) as u64;
        let element = self.used() + 4 + slot * 8;
        (read32(element) as u16, read32(element + 4))
    }

    fn write_descriptor(&self, index: u16, length: u32, desc_flags: u16) {
        let base = phys_to_virt(self.desc) + index as u64 * 16;
        write64(base, self.buffers[index as usize]);
        write32(base + 8, length);
        write16(base + 12, desc_flags);
        write16(base + 14, 0);
    }

    /// Places a descriptor in the available ring, without publishing it.
    fn offer(&mut self, index: u16) {
        let slot = (self.avail_index % self.size) as u64;
        write16(self.avail() + 4 + slot * 2, index);
        self.avail_index = self.avail_index.wrapping_add(1);
    }

    /// Publishes what `offer` placed, and notifies unless the device said
    /// it is already looking.
    fn publish(&mut self) {
        // The ring entries must be visible before the index that publishes
        // them, and the index before the device is told to look.
        fence(Ordering::SeqCst);
        write16(self.avail() + 2, self.avail_index);
        fence(Ordering::SeqCst);
        if read16(self.used()) & USED_NO_NOTIFY == 0 {
            write16(self.notify, self.number);
        }
    }
}

/// The driven card.
pub struct VirtioNet {
    common: u64,
    config: u64,
    mac: [u8; 6],
    has_status: bool,
    rx: Queue,
    tx: Queue,
    /// Transmit descriptors not in the device's hands.
    tx_free: Vec<u16>,
    pub sent: u64,
    pub received: u64,
    pub dropped: u64,
}

static NIC: SpinLock<Option<VirtioNet>> = SpinLock::new(None);

/// Virtual address of the receive queue's used index, and the value of it
/// already consumed: enough to see that nothing arrived **without the
/// lock**. A waiting task polls in a tight loop; taking the lock -- which
/// masks interrupts -- on every empty lap starved the other cores of it,
/// core 0 among them, whose timer then lost ticks.
static RX_USED_INDEX: AtomicU64 = AtomicU64::new(0);
static RX_SEEN: AtomicU16 = AtomicU16::new(0);
/// Virtual address of the reception queue's available flags, when the
/// queue interrupts (MSI-X routed); zero when it is polled only.
static RX_AVAIL_FLAGS: AtomicU64 = AtomicU64::new(0);

impl VirtioNet {
    /// Takes back the transmit descriptors the device has finished with.
    fn reclaim(&mut self) {
        let used = self.tx.used_index();
        while self.tx.used_seen != used {
            let (index, _) = self.tx.used_element(self.tx.used_seen);
            if (index as usize) < self.tx.buffers.len() {
                self.tx_free.push(index);
            }
            self.tx.used_seen = self.tx.used_seen.wrapping_add(1);
        }
    }

    fn transmit(&mut self, frame: &[u8]) -> Result<(), &'static str> {
        if frame.is_empty() || frame.len() > BUFFER_SIZE - HEADER_SIZE {
            return Err("taille de trame hors bornes");
        }
        self.reclaim();
        let mut spins = 0;
        while self.tx_free.is_empty() {
            spins += 1;
            if spins > TX_SPINS {
                self.dropped += 1;
                return Err("anneau d'emission sature");
            }
            core::hint::spin_loop();
            self.reclaim();
        }
        let Some(index) = self.tx_free.pop() else {
            return Err("anneau d'emission sature");
        };

        let buffer = phys_to_virt(self.tx.buffers[index as usize]) as *mut u8;
        // The buffer belongs to the driver until it is offered: the header
        // is all zeros (no offload), the frame follows it.
        unsafe {
            core::ptr::write_bytes(buffer, 0, HEADER_SIZE);
            core::ptr::copy_nonoverlapping(frame.as_ptr(), buffer.add(HEADER_SIZE), frame.len());
        }
        self.tx.write_descriptor(index, (HEADER_SIZE + frame.len()) as u32, 0);
        self.tx.offer(index);
        self.tx.publish();
        self.sent += 1;
        Ok(())
    }

    /// Takes up to `BATCH` received buffers: (descriptor, virtual address
    /// of the frame, its length). The buffers stay the driver's until
    /// `give_back`.
    fn take(&mut self, out: &mut [(u16, u64, usize); BATCH]) -> usize {
        let used = self.rx.used_index();
        // The used entries are read after the index that announced them.
        fence(Ordering::SeqCst);
        let mut count = 0;
        while self.rx.used_seen != used && count < BATCH {
            let (index, length) = self.rx.used_element(self.rx.used_seen);
            self.rx.used_seen = self.rx.used_seen.wrapping_add(1);
            let length = length as usize;
            if (index as usize) >= self.rx.buffers.len() {
                self.dropped += 1;
                continue;
            }
            if length <= HEADER_SIZE || length > BUFFER_SIZE {
                // An empty or impossible length: the buffer goes straight
                // back, the frame is counted as lost.
                self.dropped += 1;
                self.rx.offer(index);
                continue;
            }
            let frame = phys_to_virt(self.rx.buffers[index as usize]) + HEADER_SIZE as u64;
            out[count] = (index, frame, length - HEADER_SIZE);
            count += 1;
            self.received += 1;
        }
        RX_SEEN.store(self.rx.used_seen, Ordering::Release);
        count
    }

    /// Returns buffers to the device, all published at once.
    fn give_back(&mut self, taken: &[(u16, u64, usize)]) {
        for &(index, _, _) in taken {
            self.rx.offer(index);
        }
        self.rx.publish();
    }

    fn link_up(&self) -> bool {
        !self.has_status || read16(self.config + CONFIG_STATUS) & LINK_UP != 0
    }
}

/// Hardware address of the card, when it is in service.
pub fn mac() -> Option<[u8; 6]> {
    NIC.lock().as_ref().map(|nic| nic.mac)
}

pub fn link_up() -> bool {
    NIC.lock().as_ref().is_some_and(|nic| nic.link_up())
}

/// Counters: (sent, received, lost).
pub fn counters() -> (u64, u64, u64) {
    match NIC.lock().as_ref() {
        Some(nic) => (nic.sent, nic.received, nic.dropped),
        None => (0, 0, 0),
    }
}

pub fn transmit(frame: &[u8]) -> Result<(), &'static str> {
    match NIC.lock().as_mut() {
        Some(nic) => nic.transmit(frame),
        None => Err("aucune carte reseau pilotee"),
    }
}

/// Hands the received frames to the stack. Returns how many.
///
/// ## No copy, and no lock held across the stack
///
/// The e1000 copies each frame into a vector under its lock, then gives
/// the vectors to the stack. At 150 000 frames a second that is as many
/// heap allocations. Here the buffers are **lent**: taken from the used
/// ring under the lock, read in place with the lock released, then given
/// back under the lock. Nothing else touches a buffer in between: the
/// device only writes descriptors it has been offered, and `nic::poll`
/// lets a single task poll at a time.
pub fn poll() -> usize {
    let used_index = RX_USED_INDEX.load(Ordering::Acquire);
    if used_index == 0 || read16(used_index) == RX_SEEN.load(Ordering::Acquire) {
        return 0;
    }

    let mut taken = [(0u16, 0u64, 0usize); BATCH];
    let count = {
        let mut nic = NIC.lock();
        let Some(nic) = nic.as_mut() else {
            return 0;
        };
        nic.take(&mut taken)
    };
    if count == 0 {
        return 0;
    }
    let taken = &taken[..count];

    for &(_, address, length) in taken {
        // The buffer is out of the device's hands until `give_back`, and
        // `length` was bounded against its size in `take`.
        let frame = unsafe { core::slice::from_raw_parts(address as *const u8, length) };
        crate::net::input(frame);
    }

    if let Some(nic) = NIC.lock().as_mut() {
        nic.give_back(taken);
    }
    count
}

/// Does the reception queue interrupt? Then its vector.
pub fn rx_vector() -> Option<u8> {
    (RX_AVAIL_FLAGS.load(Ordering::Acquire) != 0).then_some(super::VECTOR_NIC_RX)
}

/// Switches the reception interrupt on (the task is about to sleep) or
/// off (it is draining the ring). A hint to the device, not a barrier:
/// a frame may still interrupt just after it is switched off, which costs
/// one needless wake-up and nothing else.
pub fn arm_rx(on: bool) {
    let flags = RX_AVAIL_FLAGS.load(Ordering::Acquire);
    if flags != 0 {
        write16(flags, if on { 0 } else { AVAIL_NO_INTERRUPT });
        // The flag must be visible before the used index is read again.
        fence(Ordering::SeqCst);
    }
}

/// Are frames waiting in the reception ring? Read without the lock.
pub fn rx_pending() -> bool {
    let used_index = RX_USED_INDEX.load(Ordering::Acquire);
    used_index != 0 && read16(used_index) != RX_SEEN.load(Ordering::Acquire)
}

/// Allocates the rings and buffers of one queue and hands it to the
/// device. The queue must be selected. `msix` is the table entry the
/// queue's interrupt goes to, if it is to interrupt at all.
fn setup_queue(
    common: u64,
    notify_base: u64,
    multiplier: u32,
    number: u16,
    msix: Option<u16>,
) -> Result<(Queue, bool), &'static str> {
    write16(common + QUEUE_SELECT, number);
    let offered = read16(common + QUEUE_SIZE);
    if offered == 0 {
        return Err("file virtio absente");
    }
    // The largest power of two within both limits.
    let mut size = 1u16;
    while size * 2 <= offered.min(MAX_QUEUE) {
        size *= 2;
    }

    let desc = crate::frame::allocate_zeroed_frame().ok_or("plus de cadre pour une file")?.start_addr();
    let rings = crate::frame::allocate_zeroed_frame().ok_or("plus de cadre pour une file")?.start_addr();
    // Two buffers per frame: one frame per buffer would waste half the
    // memory tied up.
    let mut buffers = Vec::with_capacity(size as usize);
    while buffers.len() < size as usize {
        let frame = crate::frame::allocate_zeroed_frame().ok_or("plus de cadre pour les tampons")?.start_addr();
        buffers.push(frame);
        buffers.push(frame + BUFFER_SIZE as u64);
    }

    let queue = Queue {
        size,
        desc,
        rings,
        buffers,
        avail_index: 0,
        used_seen: 0,
        notify: 0,
        number,
    };
    // Set before the queue is enabled: the device must not interrupt
    // before the driver is ready to hear it (`arm_rx` lifts it).
    write16(queue.avail(), AVAIL_NO_INTERRUPT);

    write16(common + QUEUE_SIZE, size);
    // The device reads the entry back as NO_VECTOR when it could not take
    // it: the queue is then polled only, as before.
    write16(common + QUEUE_MSIX_VECTOR, msix.unwrap_or(NO_VECTOR));
    let interrupts = msix.is_some() && read16(common + QUEUE_MSIX_VECTOR) != NO_VECTOR;
    write64(common + QUEUE_DESC, desc);
    write64(common + QUEUE_DRIVER, rings);
    write64(common + QUEUE_DEVICE, rings + USED_OFFSET);
    let notify_off = read16(common + QUEUE_NOTIFY_OFF) as u64;
    write16(common + QUEUE_ENABLE, 1);

    Ok((Queue { notify: notify_base + notify_off * multiplier as u64, ..queue }, interrupts))
}

/// Looks for and initialises a virtio network device.
pub(super) fn probe() -> Result<&'static str, &'static str> {
    let device = crate::pci::enumerate()
        .into_iter()
        .find(|d| d.vendor_id == VENDOR_VIRTIO && DEVICES.contains(&d.device_id))
        .ok_or("aucune carte virtio-net sur le bus PCI")?;
    let (common_block, notify_block, device_block) =
        find_blocks(&device).ok_or("capacites virtio modernes absentes")?;

    unsafe { device.enable(pci::command::MEMORY_SPACE | pci::command::BUS_MASTER) };
    let command = pci::read16(device.address, pci::offsets::COMMAND);
    // Only the interrupt-disable bit is added to what `enable` just set.
    unsafe { pci::write16(device.address, pci::offsets::COMMAND, command | COMMAND_INTX_DISABLE) };

    let mut next = MMIO_VMA;
    let mut map = |block: &Block| -> Option<u64> {
        let bar = device.bar(block.bar);
        if bar == 0 {
            return None;
        }
        // A BAR described by the device's own capability list.
        unsafe { map_mmio(&mut next, bar + block.offset as u64, block.length as u64) }
    };
    let common = map(&common_block).ok_or("bloc commun inaccessible")?;
    let notify = map(&notify_block).ok_or("bloc de notification inaccessible")?;
    let config = map(&device_block).ok_or("bloc de configuration inaccessible")?;
    // The MSI-X table, when the function has one, in its own BAR window.
    let msix = pci::msix(&device).and_then(|cap| {
        let table = map(&Block {
            bar: cap.table_bar,
            offset: cap.table_offset,
            length: cap.entries as u32 * 16,
            multiplier: 0,
        })?;
        Some((cap, table))
    });

    // --- reset, then the status sequence of 3.1.1 ---
    write8(common + DEVICE_STATUS, 0);
    for _ in 0..1_000_000 {
        if read8(common + DEVICE_STATUS) == 0 {
            break;
        }
        core::hint::spin_loop();
    }
    write8(common + DEVICE_STATUS, STATUS_ACKNOWLEDGE);
    write8(common + DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

    write32(common + DEVICE_FEATURE_SELECT, 1);
    if read32(common + DEVICE_FEATURE) & FEATURE_VERSION_1_HIGH == 0 {
        write8(common + DEVICE_STATUS, STATUS_FAILED);
        return Err("interface virtio 1.0 non proposee");
    }
    write32(common + DEVICE_FEATURE_SELECT, 0);
    let offered = read32(common + DEVICE_FEATURE);
    let low = offered & (FEATURE_MAC | FEATURE_STATUS);
    write32(common + DRIVER_FEATURE_SELECT, 0);
    write32(common + DRIVER_FEATURE, low);
    write32(common + DRIVER_FEATURE_SELECT, 1);
    write32(common + DRIVER_FEATURE, FEATURE_VERSION_1_HIGH);

    let status = STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK;
    write8(common + DEVICE_STATUS, status);
    if read8(common + DEVICE_STATUS) & STATUS_FEATURES_OK == 0 {
        write8(common + DEVICE_STATUS, STATUS_FAILED);
        return Err("fonctions refusees par le peripherique");
    }
    if low & FEATURE_MAC == 0 {
        // Without it the address would have to be invented, and the host
        // side would not know it.
        write8(common + DEVICE_STATUS, STATUS_FAILED);
        return Err("adresse materielle non fournie");
    }

    let mut mac = [0u8; 6];
    for (offset, byte) in mac.iter_mut().enumerate() {
        *byte = read8(config + CONFIG_MAC + offset as u64);
    }
    if mac == [0; 6] {
        write8(common + DEVICE_STATUS, STATUS_FAILED);
        return Err("adresse materielle nulle");
    }

    write16(common + MSIX_CONFIG, NO_VECTOR);
    let fail = |reason| {
        write8(common + DEVICE_STATUS, STATUS_FAILED);
        reason
    };
    // Entry 0 of the table carries the reception interrupt to core 0,
    // whose IDT has the relay (`idt::msi_irq_0`) and where the driver
    // task lives.
    let rx_entry = match (msix, crate::smp::lapic_of(0)) {
        (Some((cap, table)), Some(apic_id)) => {
            // SAFETY: `table` maps this capability's table, uncached, and
            // the vector has its relay in the IDT since `idt::init`.
            unsafe { cap.route(&device, table, 0, super::VECTOR_NIC_RX, apic_id) };
            Some(0)
        }
        _ => None,
    };
    let (mut rx, rx_interrupts) =
        setup_queue(common, notify, notify_block.multiplier, 0, rx_entry).map_err(fail)?;
    let (tx, _) = setup_queue(common, notify, notify_block.multiplier, 1, None).map_err(fail)?;

    // Every receive buffer is the device's from the start.
    for index in 0..rx.size {
        rx.write_descriptor(index, BUFFER_SIZE as u32, DESC_WRITE);
        rx.offer(index);
    }
    let tx_free: Vec<u16> = (0..tx.size).rev().collect();

    write8(common + DEVICE_STATUS, status | STATUS_DRIVER_OK);
    // Published after DRIVER_OK: a notification before it may be ignored.
    rx.publish();

    let nic = VirtioNet {
        common,
        config,
        mac,
        has_status: low & FEATURE_STATUS != 0,
        rx,
        tx,
        tx_free,
        sent: 0,
        received: 0,
        dropped: 0,
    };
    let depth = (nic.rx.size, nic.tx.size);
    let used_index = nic.rx.used() + 2;
    let avail_flags = nic.rx.avail();
    *NIC.lock() = Some(nic);
    RX_USED_INDEX.store(used_index, Ordering::Release);
    if rx_interrupts {
        RX_AVAIL_FLAGS.store(avail_flags, Ordering::Release);
    }

    crate::serial_println!(
        "[drv-reseau] virtio-net - MAC {}, files {}/{}, BAR{} 0x{:x} - reception {}{}",
        crate::net::mac_text(&mac),
        depth.0,
        depth.1,
        common_block.bar,
        device.bar(common_block.bar),
        if rx_interrupts { "par interruption MSI-X, vecteur " } else { "scrutee" },
        if rx_interrupts { alloc::format!("{}", super::VECTOR_NIC_RX) } else { alloc::string::String::new() }
    );
    Ok("virtio-net")
}

/// Description of the card for the driver registry.
pub(super) fn description() -> alloc::string::String {
    match NIC.lock().as_ref() {
        Some(nic) => alloc::format!("virtio-net - {}", crate::net::mac_text(&nic.mac)),
        None => alloc::string::String::new(),
    }
}

/// Device status, for the diagnostics: FAILED or NEEDS_RESET set means the
/// device gave up.
pub fn device_status() -> u8 {
    NIC.lock().as_ref().map(|nic| read8(nic.common + DEVICE_STATUS)).unwrap_or(0)
}

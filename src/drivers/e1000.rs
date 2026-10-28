//! 6.1.4 -- Intel 8254x (e1000) network card driver.
//!
//! It is the card QEMU exposes by default on x86_64, and one of the
//! best documented controllers there is: the Intel manual describes every
//! register, and the QEMU model follows it faithfully. Driving hardware
//! that is really present is worth more than driving an invented
//! interface -- the phase 6.2 stack is built on top of frames that really
//! cross a DMA ring.
//!
//! ## Descriptor rings
//!
//! The card does not receive an order frame by frame: it reads in memory
//! a **descriptor ring** the driver fills in advance, each pointing at a
//! buffer. Two indexes turn the ring: the card advances the head
//! (`HEAD`), the driver advances the tail (`TAIL`). The driver can never
//! catch the card up, which is enough to do without a lock between the
//! two -- that is the property making the mechanism usable with no
//! hardware coordination.
//!
//! On reception, the tail names **the last descriptor the driver gives
//! back to the card**. So it is initialised to `N-1`: every descriptor
//! belongs to the hardware. After handling descriptor `i`, we write
//! `RDT = i` -- not `i+1`: writing the current position would give back
//! to the card a descriptor we are reading.
//!
//! ## DMA coherence
//!
//! The rings and the buffers live in ordinary frames, addressed through
//! the physical window, in cacheable memory. On x86 that is correct: the
//! hardware snoops the bus and invalidates the cache lines the DMA
//! overwrites. The register MMIO, on the other hand, is explicitly
//! uncacheable -- a register write left in the cache would never reach
//! the card. The two rules are different and confusing them gives a
//! driver working backwards: registers lost, buffers corrupted.
//!
//! ## Polling rather than interrupts
//!
//! The PCI interrupt lines are **level triggered**: the card keeps the
//! line asserted until its cause register is read. The generic handler of
//! the kernel does nothing but count and acknowledge the LAPIC -- it
//! reads no device register, by construction -- and would therefore leave
//! the line asserted: an interrupt storm that would freeze the core
//! before the driver task was even elected.
//!
//! Fixing that requires masking the line in the I/O APIC from the handler
//! and reopening it from the task once the cause is read. It is feasible
//! and it will be done; until then, the task polls the ring every ten
//! milliseconds. The added latency has no effect on DHCP, DNS and TCP,
//! whose deadlines are counted in hundreds of milliseconds. **So the
//! driver arms no interrupt on the card**: not asking for one is the only
//! sure way not to suffer one.
//!
//! ## What lives elsewhere
//!
//! The driver task and the `dev.net` channel are in `drivers::nic`, shared
//! with `virtio_net`: this file only probes the card and moves frames.

use alloc::vec::Vec;

use crate::memory::phys_to_virt;
use crate::vga::SpinLock;

/// Identifiers of the models driven.
const VENDOR_INTEL: u16 = 0x8086;
const SUPPORTED: &[(u16, &str)] = &[
    (0x100E, "82540EM"),
    (0x100F, "82545EM"),
    (0x10D3, "82574L"),
    (0x153A, "I217-LM"),
    (0x10EA, "I219-V"),
];

/// Registers, as byte offsets from BAR 0.
mod reg {
    pub const CTRL: u64 = 0x0000;
    pub const STATUS: u64 = 0x0008;
    pub const ICR: u64 = 0x00C0;
    pub const IMC: u64 = 0x00D8;
    pub const RCTL: u64 = 0x0100;
    pub const TCTL: u64 = 0x0400;
    pub const TIPG: u64 = 0x0410;
    pub const RDBAL: u64 = 0x2800;
    pub const RDBAH: u64 = 0x2804;
    pub const RDLEN: u64 = 0x2808;
    pub const RDH: u64 = 0x2810;
    pub const RDT: u64 = 0x2818;
    pub const TDBAL: u64 = 0x3800;
    pub const TDBAH: u64 = 0x3804;
    pub const TDLEN: u64 = 0x3808;
    pub const TDH: u64 = 0x3810;
    pub const TDT: u64 = 0x3818;
    pub const MTA: u64 = 0x5200;
    pub const RAL: u64 = 0x5400;
    pub const RAH: u64 = 0x5404;
}

/// Bits of CTRL.
const CTRL_SLU: u32 = 1 << 6;
const CTRL_ASDE: u32 = 1 << 5;
const CTRL_RST: u32 = 1 << 26;
const CTRL_LRST: u32 = 1 << 3;
const CTRL_ILOS: u32 = 1 << 7;
const CTRL_VME: u32 = 1 << 30;
const CTRL_PHY_RST: u32 = 1 << 31;

/// Bit 1 of STATUS: the link is established.
const STATUS_LINK_UP: u32 = 1 << 1;

/// Bits of RCTL.
const RCTL_EN: u32 = 1 << 1;
const RCTL_BAM: u32 = 1 << 15;
const RCTL_SECRC: u32 = 1 << 26;

/// Bits of TCTL.
const TCTL_EN: u32 = 1 << 1;
const TCTL_PSP: u32 = 1 << 3;

/// Bit 31 of RAH: the address is valid and the filter applies it.
const RAH_AV: u32 = 1 << 31;

/// State of a receive descriptor: data written, end of packet.
const RX_STATUS_DD: u8 = 1 << 0;
const RX_STATUS_EOP: u8 = 1 << 1;

/// Commands of a transmit descriptor.
const TX_CMD_EOP: u8 = 1 << 0;
const TX_CMD_IFCS: u8 = 1 << 1;
const TX_CMD_RS: u8 = 1 << 3;
const TX_STATUS_DD: u8 = 1 << 0;

/// Depth of the rings: 256 descriptors of 16 bytes, one 4 KiB frame each
/// -- a ring must be physically contiguous, and the frame allocator gives
/// frames one at a time. It was 32 and 8: 32 frames of reception is 46
/// KiB, all a burst could deliver between two polls, and a ceiling of a
/// few megabytes a second. 256 hold 370 KiB.
const RX_COUNT: usize = 256;
const TX_COUNT: usize = 256;

/// Size of a frame buffer. 2048 bytes cover a complete Ethernet frame and
/// match the `BSIZE = 00` encoding of RCTL.
const BUFFER_SIZE: usize = 2048;

/// Size of a descriptor, in bytes -- identical on transmit and receive.
/// reception.
const DESCRIPTOR_SIZE: u64 = 16;

fn read_reg(base: u64, offset: u64) -> u32 {
    unsafe { ((base + offset) as *const u32).read_volatile() }
}

fn write_reg(base: u64, offset: u64, value: u32) {
    unsafe { ((base + offset) as *mut u32).write_volatile(value) }
}

/// Initialised card.
pub struct Nic {
    mmio: u64,
    mac: [u8; 6],
    rx_ring: u64,
    tx_ring: u64,
    rx_buffers: [u64; RX_COUNT],
    tx_buffers: [u64; TX_COUNT],
    rx_index: usize,
    tx_index: usize,
    model: &'static str,
    pub sent: u64,
    pub received: u64,
    pub dropped: u64,
}

static NIC: SpinLock<Option<Nic>> = SpinLock::new(None);

/// Hardware address of the card, when it is in service.
pub fn mac() -> Option<[u8; 6]> {
    NIC.lock().as_ref().map(|nic| nic.mac)
}

/// Is the physical link established?
pub fn link_up() -> bool {
    let nic = NIC.lock();
    match nic.as_ref() {
        Some(nic) => read_reg(nic.mmio, reg::STATUS) & STATUS_LINK_UP != 0,
        None => false,
    }
}

/// Counters: (sent, received, lost).
pub fn counters() -> (u64, u64, u64) {
    let nic = NIC.lock();
    match nic.as_ref() {
        Some(nic) => (nic.sent, nic.received, nic.dropped),
        None => (0, 0, 0),
    }
}

/// Allocates a frame for DMA and returns (physical, virtual).
fn dma_frame() -> Option<(u64, u64)> {
    let frame = crate::frame::allocate_zeroed_frame()?;
    Some((frame.start_addr(), frame.virt_addr()))
}

impl Nic {
    /// Writes a descriptor field without assuming any type alignment.
    fn descriptor(ring: u64, index: usize) -> u64 {
        phys_to_virt(ring) + index as u64 * DESCRIPTOR_SIZE
    }

    /// Sends a frame. Blocks until the card has taken charge of it, for
    /// at most a few milliseconds.
    fn transmit(&mut self, frame: &[u8]) -> Result<(), &'static str> {
        if frame.is_empty() || frame.len() > BUFFER_SIZE {
            return Err("taille de trame hors bornes");
        }

        let index = self.tx_index;
        let descriptor = Self::descriptor(self.tx_ring, index);

        // The descriptor previously placed in this slot must have been
        // handled: without this check, a burst of more than eight frames
        // would overwrite a buffer the card is reading.
        let status = unsafe { ((descriptor + 12) as *const u8).read_volatile() };
        let used = unsafe { ((descriptor + 8) as *const u16).read_volatile() } != 0;
        if used && status & TX_STATUS_DD == 0 {
            // The wait is paid **with interrupts masked** -- the card lock
            // masks them -- and every register re-read is an uncacheable
            // MMIO access. A bound of a million rounds would represent
            // hundreds of milliseconds with no clock; twenty thousand
            // amply cover the sending of eight frames, which is counted
            // in tens of microseconds.
            let mut spins = 0;
            while unsafe { ((descriptor + 12) as *const u8).read_volatile() } & TX_STATUS_DD == 0 {
                spins += 1;
                if spins > 20_000 {
                    self.dropped += 1;
                    return Err("anneau d'emission sature");
                }
                core::hint::spin_loop();
            }
        }

        let buffer = phys_to_virt(self.tx_buffers[index]);
        unsafe {
            core::ptr::copy_nonoverlapping(frame.as_ptr(), buffer as *mut u8, frame.len());

            ((descriptor) as *mut u64).write_volatile(self.tx_buffers[index]);
            ((descriptor + 8) as *mut u16).write_volatile(frame.len() as u16);
            ((descriptor + 10) as *mut u8).write_volatile(0);
            ((descriptor + 11) as *mut u8).write_volatile(TX_CMD_EOP | TX_CMD_IFCS | TX_CMD_RS);
            ((descriptor + 12) as *mut u8).write_volatile(0);
            ((descriptor + 13) as *mut u8).write_volatile(0);
            ((descriptor + 14) as *mut u16).write_volatile(0);
        }

        self.tx_index = (index + 1) % TX_COUNT;
        write_reg(self.mmio, reg::TDT, self.tx_index as u32);
        self.sent += 1;
        Ok(())
    }

    /// Removes the received frames from the ring, 64 at most per call so
    /// the lock is not held for long.
    fn receive(&mut self) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();

        while frames.len() < 64 {
            let index = self.rx_index;
            let descriptor = Self::descriptor(self.rx_ring, index);

            let status = unsafe { ((descriptor + 12) as *const u8).read_volatile() };
            if status & RX_STATUS_DD == 0 {
                break;
            }

            let length = unsafe { ((descriptor + 8) as *const u16).read_volatile() } as usize;
            let errors = unsafe { ((descriptor + 13) as *const u8).read_volatile() };

            // A frame with no end marker is the fragment of a packet
            // larger than the buffer: giving it to the stack would make
            // it parse a truncated header.
            if errors == 0 && status & RX_STATUS_EOP != 0 && length != 0 && length <= BUFFER_SIZE {
                let buffer = phys_to_virt(self.rx_buffers[index]) as *const u8;
                let mut frame = alloc::vec![0u8; length];
                unsafe { core::ptr::copy_nonoverlapping(buffer, frame.as_mut_ptr(), length) };
                frames.push(frame);
                self.received += 1;
            } else {
                self.dropped += 1;
            }

            // Give the descriptor back to the card: state cleared, then
            // the tail points at this descriptor -- never at the next.
            unsafe {
                ((descriptor + 8) as *mut u16).write_volatile(0);
                ((descriptor + 12) as *mut u8).write_volatile(0);
                ((descriptor + 13) as *mut u8).write_volatile(0);
            }
            write_reg(self.mmio, reg::RDT, index as u32);
            self.rx_index = (index + 1) % RX_COUNT;
        }

        frames
    }
}

/// Drains the reception ring into the stack, from whatever task calls it.
/// Returns the frames handled.
///
/// ## Why any task may poll
///
/// The driver task polls every ten milliseconds -- the clock allows no
/// less -- and it is a `.dsa`: letting it spin would starve every task
/// below it, the reader of the data among them. So a task **waiting** for
/// data polls the ring itself (`tcp::recv`, the NAPI principle of Linux):
/// the frames come up as fast as it consumes them, at its own priority,
/// and only while someone waits. The lock orders the pollers; the stack
/// above was written to run from the driver task, which never sleeps in
/// `input`, and neither does any caller of this.
pub fn poll() -> usize {
    let frames = {
        let mut nic = NIC.lock();
        match nic.as_mut() {
            Some(nic) => nic.receive(),
            None => return 0,
        }
    };
    let count = frames.len();
    for frame in frames {
        crate::net::input(&frame);
    }
    count
}

/// Sends a frame on the card, if it is in service.
///
/// It is the only entry point of the network stack towards the hardware.
pub fn transmit(frame: &[u8]) -> Result<(), &'static str> {
    let mut nic = NIC.lock();
    match nic.as_mut() {
        Some(nic) => nic.transmit(frame),
        None => Err("aucune carte reseau pilotee"),
    }
}

/// Looks for and initialises a recognised card.
pub(super) fn probe() -> Result<&'static str, &'static str> {
    let device = crate::pci::enumerate()
        .into_iter()
        .find(|d| {
            d.vendor_id == VENDOR_INTEL && SUPPORTED.iter().any(|(id, _)| *id == d.device_id)
        })
        .ok_or("aucune carte Intel e1000 sur le bus PCI")?;

    let model = SUPPORTED
        .iter()
        .find(|(id, _)| *id == device.device_id)
        .map(|(_, name)| *name)
        .unwrap_or("e1000");

    // MEMORY_SPACE so that the BAR answers, BUS_MASTER so that the card
    // can read the rings prepared for it. Without the second, everything
    // writes correctly and nothing circulates.
    unsafe { device.enable(crate::pci::command::MEMORY_SPACE | crate::pci::command::BUS_MASTER) };

    let bar0 = device.bar(0);
    if bar0 == 0 || bar0 & 1 != 0 {
        return Err("BAR 0 absent ou en espace d'E/S");
    }
    unsafe { crate::memory::set_mmio_uncached(bar0) };
    let mmio = phys_to_virt(bar0);

    // The hardware address is read **before** the reset: on some models
    // the filter is reloaded from the EEPROM, on others not. Reading it
    // back afterwards would give a null address on the latter, and a card
    // with no address receives nothing.
    let ral = read_reg(mmio, reg::RAL);
    let rah = read_reg(mmio, reg::RAH);
    let mac = [
        ral as u8,
        (ral >> 8) as u8,
        (ral >> 16) as u8,
        (ral >> 24) as u8,
        rah as u8,
        (rah >> 8) as u8,
    ];
    if mac == [0; 6] {
        return Err("adresse materielle nulle");
    }

    // No interrupt: see the module header. IMC clears every mask, ICR
    // acknowledges what would already be pending.
    write_reg(mmio, reg::IMC, 0xFFFF_FFFF);
    let _ = read_reg(mmio, reg::ICR);

    write_reg(mmio, reg::CTRL, read_reg(mmio, reg::CTRL) | CTRL_RST);
    for _ in 0..100_000 {
        if read_reg(mmio, reg::CTRL) & CTRL_RST == 0 {
            break;
        }
        core::hint::spin_loop();
    }
    write_reg(mmio, reg::IMC, 0xFFFF_FFFF);
    let _ = read_reg(mmio, reg::ICR);

    // Link up, automatic speed negotiation, no loopback and no VLAN
    // tagging.
    let ctrl = read_reg(mmio, reg::CTRL);
    write_reg(
        mmio,
        reg::CTRL,
        (ctrl | CTRL_SLU | CTRL_ASDE) & !(CTRL_LRST | CTRL_ILOS | CTRL_VME | CTRL_PHY_RST),
    );

    // Filter: the address of the card, plus multicast entirely closed. A
    // multicast table left to chance brings up frames nobody expects.
    write_reg(mmio, reg::RAL, u32::from_le_bytes([mac[0], mac[1], mac[2], mac[3]]));
    write_reg(mmio, reg::RAH, u32::from_le_bytes([mac[4], mac[5], 0, 0]) | RAH_AV);
    for entry in 0..128u64 {
        write_reg(mmio, reg::MTA + entry * 4, 0);
    }

    // --- rings ---
    let (rx_ring, _) = dma_frame().ok_or("plus de cadre pour l'anneau de reception")?;
    let (tx_ring, _) = dma_frame().ok_or("plus de cadre pour l'anneau d'emission")?;

    let mut rx_buffers = [0u64; RX_COUNT];
    let mut tx_buffers = [0u64; TX_COUNT];

    // Two 2 KiB buffers per 4 KiB frame: allocating one frame per buffer
    // would waste half the memory tied up.
    let mut pending: Option<u64> = None;
    for slot in rx_buffers.iter_mut().chain(tx_buffers.iter_mut()) {
        match pending.take() {
            Some(second) => *slot = second,
            None => {
                let (phys, _) = dma_frame().ok_or("plus de cadre pour les tampons")?;
                *slot = phys;
                pending = Some(phys + BUFFER_SIZE as u64);
            }
        }
    }

    for (index, buffer) in rx_buffers.iter().enumerate() {
        let descriptor = Nic::descriptor(rx_ring, index);
        unsafe {
            (descriptor as *mut u64).write_volatile(*buffer);
            ((descriptor + 8) as *mut u64).write_volatile(0);
        }
    }
    for index in 0..TX_COUNT {
        let descriptor = Nic::descriptor(tx_ring, index);
        unsafe {
            (descriptor as *mut u64).write_volatile(0);
            ((descriptor + 8) as *mut u64).write_volatile(0);
        }
    }

    write_reg(mmio, reg::RDBAL, rx_ring as u32);
    write_reg(mmio, reg::RDBAH, (rx_ring >> 32) as u32);
    write_reg(mmio, reg::RDLEN, (RX_COUNT as u32) * DESCRIPTOR_SIZE as u32);
    write_reg(mmio, reg::RDH, 0);
    write_reg(mmio, reg::RDT, (RX_COUNT - 1) as u32);
    write_reg(mmio, reg::RCTL, RCTL_EN | RCTL_BAM | RCTL_SECRC);

    write_reg(mmio, reg::TDBAL, tx_ring as u32);
    write_reg(mmio, reg::TDBAH, (tx_ring >> 32) as u32);
    write_reg(mmio, reg::TDLEN, (TX_COUNT as u32) * DESCRIPTOR_SIZE as u32);
    write_reg(mmio, reg::TDH, 0);
    write_reg(mmio, reg::TDT, 0);
    // Inter-packet gap recommended by the manual for copper: 10 bits of
    // transmission, 8 then 6 of recovery.
    write_reg(mmio, reg::TIPG, 10 | (8 << 10) | (6 << 20));
    write_reg(
        mmio,
        reg::TCTL,
        TCTL_EN | TCTL_PSP | (0x0F << 4) | (0x40 << 12),
    );

    *NIC.lock() = Some(Nic {
        mmio,
        mac,
        rx_ring,
        tx_ring,
        rx_buffers,
        tx_buffers,
        rx_index: 0,
        tx_index: 0,
        model,
        sent: 0,
        received: 0,
        dropped: 0,
    });

    crate::serial_println!(
        "[drv-reseau] Intel {} - MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, BAR0 0x{:x}",
        model,
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5],
        bar0
    );

    Ok(model)
}

/// Description of the card for the driver registry.
pub(super) fn description() -> alloc::string::String {
    let nic = NIC.lock();
    match nic.as_ref() {
        Some(nic) => alloc::format!(
            "Intel {} - {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            nic.model,
            nic.mac[0],
            nic.mac[1],
            nic.mac[2],
            nic.mac[3],
            nic.mac[4],
            nic.mac[5]
        ),
        None => alloc::string::String::new(),
    }
}

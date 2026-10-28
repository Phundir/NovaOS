//! xHCI controller (eXtensible Host Controller Interface 1.2).
//!
//! ## Why xHCI and not UHCI
//!
//! UHCI would be five times shorter: a frame list, transfer descriptors,
//! a few I/O ports. But that is USB 1.1, and **no machine built since
//! 2010 has any**. A UHCI driver would only work under emulation, which
//! would amount to writing a driver for QEMU rather than for a computer.
//! xHCI drives every speed, from USB 1.1 to SuperSpeed, on the same
//! register set -- it is the only controller an OS needs to know.
//!
//! ## The four register banks
//!
//! BAR 0 exposes four areas, three of which are found through an offset
//! read in the first. None is at a fixed address:
//!
//! ```text
//!   BAR0                        capability  (read only)
//!   BAR0 + CAPLENGTH            operational (USBCMD, USBSTS, CRCR, DCBAAP, ports)
//!   BAR0 + RTSOFF               runtime     (interrupters, event ring)
//!   BAR0 + DBOFF                doorbells   (one per slot)
//! ```
//!
//! ## Rings
//!
//! Everything goes through TRB rings -- sixteen-byte records. Three
//! families:
//!
//! * the **command ring**, where the driver drops what it asks of the
//!   controller (enable a slot, address a device);
//! * a **transfer ring per endpoint**, where it drops the transfers;
//! * the **event ring**, where the controller drops its answers, and
//!   which the driver alone consumes.
//!
//! The direction of travel is therefore reversed between the first two
//! and the third, and that is what makes the **cycle bit**
//! indispensable: a ring has neither observable head nor tail, only the
//! alternation of that bit tells a new record from an old already
//! consumed one. The reader keeps its own cycle state and inverts it on
//! every lap.
//!
//! ## Why polling rather than interrupts
//!
//! The controller can raise an MSI-X interrupt. Consuming it would
//! demand programming an MSI-X table, which nothing else in the system
//! does yet. So the driver task polls the event ring every few
//! milliseconds -- the same fallback `drivers::irq` documents for a line
//! badly described by the firmware, costing one coherent read per lap.

use alloc::vec::Vec;

use crate::frame::{self, Frame};
use crate::memory::phys_to_virt;
use crate::pci;

use super::descriptor::{self, Interface};

// ============================================================
//  Registers
// ============================================================

/// Offsets in the capability bank.
mod cap {
    pub const CAPLENGTH: u64 = 0x00;
    pub const HCSPARAMS1: u64 = 0x04;
    pub const HCSPARAMS2: u64 = 0x08;
    pub const HCCPARAMS1: u64 = 0x10;
    pub const DBOFF: u64 = 0x14;
    pub const RTSOFF: u64 = 0x18;
}

/// Offsets in the operational bank.
mod op {
    pub const USBCMD: u64 = 0x00;
    pub const USBSTS: u64 = 0x04;
    pub const CRCR: u64 = 0x18;
    pub const DCBAAP: u64 = 0x30;
    pub const CONFIG: u64 = 0x38;
    pub const PORTSC: u64 = 0x400;

    pub const CMD_RUN: u32 = 1 << 0;
    pub const CMD_RESET: u32 = 1 << 1;

    pub const STS_HALTED: u32 = 1 << 0;
    /// The controller has not finished its internal initialisation.
    pub const STS_NOT_READY: u32 = 1 << 11;
}

/// Bits of the PORTSC register.
mod port {
    /// A device is connected.
    pub const CCS: u32 = 1 << 0;
    /// The port is enabled.
    pub const PED: u32 = 1 << 1;
    /// Reset request.
    pub const RESET: u32 = 1 << 4;
    /// Connection state change.
    pub const CSC: u32 = 1 << 17;
    /// End of reset.
    pub const PRC: u32 = 1 << 21;
    /// "Write one to clear" bits: copying them back as-is would clear
    /// changes we have not handled.
    pub const CHANGE_MASK: u32 = 0x00FE_0000;
    /// Bits to preserve on a write.
    pub const PRESERVE: u32 = !(CHANGE_MASK | (1 << 1) | (1 << 16));
}

/// TRB types used.
mod trb {
    pub const NORMAL: u32 = 1;
    pub const SETUP: u32 = 2;
    pub const DATA: u32 = 3;
    pub const STATUS: u32 = 4;
    pub const LINK: u32 = 6;
    pub const ENABLE_SLOT: u32 = 9;
    pub const ADDRESS_DEVICE: u32 = 11;
    pub const CONFIGURE_ENDPOINT: u32 = 12;
    pub const TRANSFER_EVENT: u32 = 32;
    pub const COMMAND_COMPLETION: u32 = 33;
    pub const PORT_STATUS_CHANGE: u32 = 34;

    pub const CYCLE: u32 = 1 << 0;
    /// Cycle toggle, on a link TRB.
    pub const TOGGLE: u32 = 1 << 1;
    pub const IOC: u32 = 1 << 5;
    /// Immediate data, lodged in the parameter field.
    pub const IDT: u32 = 1 << 6;
}

/// Completion codes.
const COMPLETION_SUCCESS: u32 = 1;
const COMPLETION_SHORT_PACKET: u32 = 13;

/// Number of TRBs per ring. The last is always a link TRB, so it cannot
/// be used for a transfer.
const RING_SIZE: usize = 256;

/// Size of a TRB, in bytes.
const TRB_SIZE: usize = 16;

// ============================================================
//  Register access
// ============================================================

/// # Safety
/// `addr` must name an MMIO register of the controller, mapped uncached.
unsafe fn read32(addr: u64) -> u32 {
    unsafe { (addr as *const u32).read_volatile() }
}

/// # Safety
/// Same, and the write must be legal in the current controller state.
unsafe fn write32(addr: u64, value: u32) {
    unsafe { (addr as *mut u32).write_volatile(value) }
}

/// # Safety
/// See `write32`. The low-then-high order is imposed by the specification.
unsafe fn write64(addr: u64, value: u64) {
    unsafe { write32(addr, value as u32) };
    unsafe { write32(addr + 4, (value >> 32) as u32) };
}

// ============================================================
//  Memory shared with the controller
// ============================================================

/// A page handed to the controller over DMA.
///
/// The frames are physically contiguous and 4 KiB aligned, which
/// satisfies every alignment constraint of the specification -- the
/// most demanding being 64 bytes for the contexts.
///
/// The kernel physical window is write-back. On x86, coherence between
/// the processor cache and a PCI master is guaranteed by the hardware: no
/// explicit flush is needed. That would be false on an architecture with
/// non-coherent DMA, and it is the only reason this module is not
/// portable as-is.
struct DmaPage {
    frame: Frame,
}

impl DmaPage {
    fn new() -> Option<DmaPage> {
        frame::allocate_zeroed_frame().map(|frame| DmaPage { frame })
    }

    fn phys(&self) -> u64 {
        self.frame.start_addr()
    }

    fn virt(&self) -> u64 {
        self.frame.virt_addr()
    }

    /// # Safety
    /// The offset must stay inside the page.
    unsafe fn write_u32(&self, offset: usize, value: u32) {
        unsafe { ((self.virt() + offset as u64) as *mut u32).write_volatile(value) };
    }

    /// # Safety
    /// See above.
    unsafe fn read_u32(&self, offset: usize) -> u32 {
        unsafe { ((self.virt() + offset as u64) as *const u32).read_volatile() }
    }

    /// # Safety
    /// See above.
    unsafe fn write_u64(&self, offset: usize, value: u64) {
        unsafe { self.write_u32(offset, value as u32) };
        unsafe { self.write_u32(offset + 4, (value >> 32) as u32) };
    }

    fn as_slice(&self, length: usize) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.virt() as *const u8, length.min(4096)) }
    }

    fn zero(&self) {
        unsafe { core::ptr::write_bytes(self.virt() as *mut u8, 0, 4096) };
    }
}

/// A production ring: commands or transfers.
struct Ring {
    page: DmaPage,
    enqueue: usize,
    cycle: u32,
}

impl Ring {
    fn new() -> Option<Ring> {
        let page = DmaPage::new()?;
        let ring = Ring {
            page,
            enqueue: 0,
            cycle: 1,
        };
        ring.install_link();
        Some(ring)
    }

    /// The last TRB of the ring points back to the first.
    ///
    /// The toggle bit is set on it: the controller inverts its own cycle
    /// state on crossing it, failing which it would consider the whole
    /// second lap already consumed.
    fn install_link(&self) {
        let offset = (RING_SIZE - 1) * TRB_SIZE;
        unsafe {
            self.page.write_u64(offset, self.page.phys());
            self.page.write_u32(offset + 8, 0);
            self.page.write_u32(offset + 12, (trb::LINK << 10) | trb::TOGGLE);
        }
    }

    fn phys(&self) -> u64 {
        self.page.phys()
    }

    /// Drops a TRB and advances.
    ///
    /// The control field is written **last**, because it is the one
    /// carrying the cycle bit: the controller may read the ring at any
    /// moment, and a TRB whose cycle were valid before its other fields
    /// would be executed half written.
    fn push(&mut self, parameter: u64, status: u32, control: u32) -> u64 {
        let offset = self.enqueue * TRB_SIZE;
        let address = self.page.phys() + offset as u64;

        unsafe {
            self.page.write_u64(offset, parameter);
            self.page.write_u32(offset + 8, status);
            core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
            self.page.write_u32(offset + 12, control | self.cycle);
        }

        self.enqueue += 1;
        if self.enqueue == RING_SIZE - 1 {
            // The link TRB takes the current cycle, then the reader
            // toggles: the next lap is written with the inverse cycle.
            let link = (RING_SIZE - 1) * TRB_SIZE;
            unsafe {
                self.page.write_u32(
                    link + 12,
                    (trb::LINK << 10) | trb::TOGGLE | self.cycle,
                );
            }
            self.enqueue = 0;
            self.cycle ^= 1;
        }

        address
    }
}

/// The event ring, which only the driver consumes.
struct EventRing {
    segment: DmaPage,
    table: DmaPage,
    dequeue: usize,
    cycle: u32,
}

/// A decoded event.
#[derive(Debug, Clone, Copy)]
struct Event {
    kind: u32,
    /// Pointer carried by the event: command TRB, or transfer TRB.
    parameter: u64,
    completion: u32,
    /// Bytes not transferred, for a transfer event.
    residual: u32,
    slot: u8,
    endpoint: u8,
    /// Port number, for a port state change.
    port: u8,
}

impl EventRing {
    fn new() -> Option<EventRing> {
        let segment = DmaPage::new()?;
        let table = DmaPage::new()?;

        // Segment table: a single entry, covering the whole ring.
        unsafe {
            table.write_u64(0, segment.phys());
            table.write_u32(8, RING_SIZE as u32);
            table.write_u32(12, 0);
        }

        Some(EventRing {
            segment,
            table,
            dequeue: 0,
            cycle: 1,
        })
    }

    /// Removes the next event, if there is one.
    ///
    /// The stop condition is the **cycle bit**, not a counter: the
    /// controller does not announce how many events it has produced, it
    /// writes and sets the cycle. A TRB whose cycle does not match ours is
    /// an old event from the previous lap.
    fn pop(&mut self) -> Option<Event> {
        let offset = self.dequeue * TRB_SIZE;
        let control = unsafe { self.segment.read_u32(offset + 12) };

        if control & trb::CYCLE != self.cycle {
            return None;
        }

        // Reading the other fields follows that of the cycle: the
        // controller writes the control last, we read it first.
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);

        let low = unsafe { self.segment.read_u32(offset) } as u64;
        let high = unsafe { self.segment.read_u32(offset + 4) } as u64;
        let status = unsafe { self.segment.read_u32(offset + 8) };
        let parameter = low | (high << 32);

        let event = Event {
            kind: (control >> 10) & 0x3F,
            parameter,
            completion: (status >> 24) & 0xFF,
            residual: status & 0x00FF_FFFF,
            slot: ((control >> 24) & 0xFF) as u8,
            endpoint: ((control >> 16) & 0x1F) as u8,
            port: ((parameter >> 24) & 0xFF) as u8,
        };

        self.dequeue += 1;
        if self.dequeue == RING_SIZE {
            self.dequeue = 0;
            self.cycle ^= 1;
        }

        Some(event)
    }

    fn dequeue_phys(&self) -> u64 {
        self.segment.phys() + (self.dequeue * TRB_SIZE) as u64
    }
}

// ============================================================
//  Device context
// ============================================================

/// An addressed and configured device.
pub struct UsbDevice {
    pub slot: u8,
    pub port: u8,
    pub speed: u8,
    pub descriptor: descriptor::Device,
    pub interface: Interface,
    /// Transfer ring of the interrupt endpoint.
    transfer: Ring,
    /// Buffer where the controller drops the reports.
    report: DmaPage,
    /// Report size, as the endpoint announces it.
    report_length: usize,
    /// Is a transfer pending on the ring?
    armed: bool,
}

impl UsbDevice {
    /// Expected layout of the reports of this device.
    ///
    /// The interface is authoritative when it declares itself a boot
    /// mouse: that is an assertion by the device, the report length is
    /// only a hint. A boot mouse whose endpoint announced eight bytes
    /// would otherwise be taken for a tablet, and its second byte -- a
    /// displacement -- read as the low half of a position.
    ///
    /// The length only serves the devices that declare nothing, which is
    /// the case of the tablets.
    pub fn shape(&self) -> super::hid::Shape {
        if self.interface.is_boot_mouse() {
            super::hid::Shape::BootMouse
        } else {
            super::hid::Shape::of(self.report_length)
        }
    }
}

// ============================================================
//  Controller
// ============================================================

pub struct Controller {
    base: u64,
    operational: u64,
    runtime: u64,
    doorbell: u64,
    max_slots: u8,
    max_ports: u8,
    /// Size of a context: 32 or 64 bytes according to HCCPARAMS1.CSZ.
    context_size: usize,
    dcbaa: DmaPage,
    command: Ring,
    events: EventRing,
    /// Scratchpad pages demanded by the controller, kept alive.
    _scratchpad: Vec<DmaPage>,
    /// Contexts per slot, kept alive for the lifetime of the device.
    contexts: Vec<(DmaPage, DmaPage, Ring)>,
    pub devices: Vec<UsbDevice>,
}

/// Speeds as PORTSC encodes them.
fn speed_name(speed: u8) -> &'static str {
    match speed {
        1 => "Full 12 Mb/s",
        2 => "Low 1,5 Mb/s",
        3 => "High 480 Mb/s",
        4 => "Super 5 Gb/s",
        5 => "SuperPlus 10 Gb/s",
        _ => "inconnue",
    }
}

/// Default packet size of endpoint 0, according to the speed.
///
/// The USB specification imposes it before the descriptor is even
/// readable: it is the value that allows reading the descriptor that will
/// give the real one. Getting it wrong fails the very first transfer.
fn default_packet_size(speed: u8) -> u16 {
    match speed {
        2 => 8,   // Low
        1 => 64,  // Full: 64 is allowed and avoids a second round trip
        3 => 64,  // High
        _ => 512, // Super and beyond
    }
}

impl Controller {
    /// Takes possession of a controller discovered on the PCI bus.
    ///
    /// # Safety
    /// Enables bus mastering: the controller will write to memory by DMA.
    /// The caller guarantees no other driver handles it.
    pub unsafe fn attach(device: &pci::Device) -> Option<Controller> {
        unsafe { device.enable(pci::command::MEMORY_SPACE | pci::command::BUS_MASTER) };

        let bar = device.bar(0);
        if bar == 0 {
            crate::serial_println!("[xhci] BAR0 nul - controleur inutilisable");
            return None;
        }

        // MMIO must never be cached: a register write left in the cache
        // would never reach the controller.
        unsafe { crate::memory::set_mmio_uncached(bar) };
        let base = phys_to_virt(bar);

        let caplength = (unsafe { read32(base + cap::CAPLENGTH) } & 0xFF) as u64;
        let hcs1 = unsafe { read32(base + cap::HCSPARAMS1) };
        let hcs2 = unsafe { read32(base + cap::HCSPARAMS2) };
        let hcc1 = unsafe { read32(base + cap::HCCPARAMS1) };
        let dboff = (unsafe { read32(base + cap::DBOFF) } & !0x1F) as u64;
        let rtsoff = (unsafe { read32(base + cap::RTSOFF) } & !0x1F) as u64;

        let max_slots = (hcs1 & 0xFF) as u8;
        let max_ports = ((hcs1 >> 24) & 0xFF) as u8;
        let context_size = if hcc1 & (1 << 2) != 0 { 64 } else { 32 };

        crate::serial_println!(
            "[xhci] controleur {:04x}:{:04x} - {} slots, {} ports, contextes de {} octets",
            device.vendor_id,
            device.device_id,
            max_slots,
            max_ports,
            context_size
        );

        let mut controller = Controller {
            base,
            operational: base + caplength,
            runtime: base + rtsoff,
            doorbell: base + dboff,
            max_slots,
            max_ports,
            context_size,
            dcbaa: DmaPage::new()?,
            command: Ring::new()?,
            events: EventRing::new()?,
            _scratchpad: Vec::new(),
            contexts: Vec::new(),
            devices: Vec::new(),
        };

        if !unsafe { controller.reset() } {
            crate::serial_println!("[xhci] reinitialisation sans reponse");
            return None;
        }
        unsafe { controller.configure(hcs2) }?;
        Some(controller)
    }

    /// Stops then resets the controller.
    ///
    /// The stop precedes the reset: resetting a controller that is still
    /// running leaves transfers in flight whose events will arrive
    /// afterwards, into rings that no longer exist.
    ///
    /// # Safety
    /// Writes into the operational registers.
    unsafe fn reset(&mut self) -> bool {
        let usbcmd = self.operational + op::USBCMD;
        let usbsts = self.operational + op::USBSTS;

        let command = unsafe { read32(usbcmd) };
        unsafe { write32(usbcmd, command & !op::CMD_RUN) };

        for _ in 0..1000 {
            if unsafe { read32(usbsts) } & op::STS_HALTED != 0 {
                break;
            }
            crate::apic::busy_wait_micros(1000);
        }

        unsafe { write32(usbcmd, op::CMD_RESET) };

        // Two distinct waits: the reset bit falls when the controller has
        // finished zeroing itself, but it then stays "not ready" while it
        // rebuilds its internal state. Writing DCBAAP during that window
        // is explicitly forbidden.
        for _ in 0..1000 {
            if unsafe { read32(usbcmd) } & op::CMD_RESET == 0 {
                break;
            }
            crate::apic::busy_wait_micros(1000);
        }
        for _ in 0..1000 {
            if unsafe { read32(usbsts) } & op::STS_NOT_READY == 0 {
                return true;
            }
            crate::apic::busy_wait_micros(1000);
        }
        false
    }

    /// Installs the shared structures and starts the controller.
    ///
    /// # Safety
    /// The controller must have been reset and be ready.
    unsafe fn configure(&mut self, hcsparams2: u32) -> Option<()> {
        // --- scratchpad pages ---
        //
        // The controller may demand working pages it manages alone. The
        // number is spread over two non-contiguous fields of HCSPARAMS2,
        // which is an oddity of the specification and not a misreading.
        let high = (hcsparams2 >> 21) & 0x1F;
        let low = (hcsparams2 >> 27) & 0x1F;
        let scratchpads = ((high << 5) | low) as usize;

        if scratchpads > 0 {
            let array = DmaPage::new()?;
            for index in 0..scratchpads.min(512) {
                let page = DmaPage::new()?;
                unsafe { array.write_u64(index * 8, page.phys()) };
                self._scratchpad.push(page);
            }
            // Entry 0 of the DCBAA names the scratchpad array, not a
            // device: slot 0 does not exist.
            unsafe { self.dcbaa.write_u64(0, array.phys()) };
            self._scratchpad.push(array);
            crate::serial_println!("[xhci] {} page(s) de reserve fournies", scratchpads);
        }

        // --- slots, DCBAA, command ring ---
        let config = self.operational + op::CONFIG;
        unsafe { write32(config, self.max_slots as u32) };
        unsafe { write64(self.operational + op::DCBAAP, self.dcbaa.phys()) };
        // Bit 0: initial cycle state of the command ring.
        unsafe { write64(self.operational + op::CRCR, self.command.phys() | 1) };

        // --- interrupter 0 and event ring ---
        let interrupter = self.runtime + 0x20;
        unsafe { write32(interrupter + 0x08, 1) };
        unsafe { write64(interrupter + 0x10, self.events.table.phys()) };
        unsafe { write64(interrupter + 0x18, self.events.dequeue_phys()) };

        // --- start ---
        let usbcmd = self.operational + op::USBCMD;
        let command = unsafe { read32(usbcmd) };
        unsafe { write32(usbcmd, command | op::CMD_RUN) };

        for _ in 0..1000 {
            if unsafe { read32(self.operational + op::USBSTS) } & op::STS_HALTED == 0 {
                return Some(());
            }
            crate::apic::busy_wait_micros(1000);
        }
        crate::serial_println!("[xhci] le controleur ne demarre pas");
        None
    }

    /// Tells the controller that a ring has something to read.
    ///
    /// # Safety
    /// `slot` must name an active slot, or zero for the commands.
    unsafe fn ring_doorbell(&self, slot: u8, target: u32) {
        unsafe { write32(self.doorbell + slot as u64 * 4, target) };
    }

    /// Advances the read pointer of the event ring.
    ///
    /// Bit 3 acknowledges the occupancy: without it the controller
    /// considers that the driver has consumed nothing and stops producing
    /// events as soon as the ring looks full.
    ///
    /// # Safety
    /// Writes a runtime register.
    unsafe fn advance_events(&self) {
        unsafe { write64(self.runtime + 0x20 + 0x18, self.events.dequeue_phys() | (1 << 3)) };
    }

    /// Waits for an event satisfying a predicate, or for the deadline.
    ///
    /// ## Waiting in two regimes
    ///
    /// `busy_wait_micros` **masks the local timer** for the duration of
    /// its wait (see `docs/DECISIONS.md`, trap 15): preemption stops on
    /// this core. That is of no consequence for the few microseconds a
    /// control transfer takes to complete, and unacceptable if one had to
    /// wait the whole second of a missed deadline.
    /// echeance manquee.
    ///
    /// Hence two regimes: tight polling for five milliseconds, which
    /// covers every normal completion; then ordinary sleep, where the
    /// core becomes available again. The fast case stays fast, the
    /// degraded case freezes nothing.
    ///
    /// The events that do not fit are not thrown away in silence: a port
    /// state change occurring while waiting for a command is
    /// acknowledged. That is the only way to understand, later, why an
    /// enumeration failed.
    fn wait_event(
        &mut self,
        timeout_ms: u64,
        mut accept: impl FnMut(&Event) -> bool,
    ) -> Option<Event> {
        const SPIN_MICROS: u64 = 5_000;
        let per_tick = 1000 / crate::TIMER_HZ as u64;
        let deadline = crate::idt::ticks() + timeout_ms.div_ceil(per_tick).max(1);
        let mut spun = 0u64;

        loop {
            let mut consumed = false;
            let mut found = None;

            while let Some(event) = self.events.pop() {
                consumed = true;
                if found.is_none() && accept(&event) {
                    found = Some(event);
                } else if event.kind == trb::PORT_STATUS_CHANGE {
                    self.acknowledge_port(event.port);
                }
            }
            if consumed {
                unsafe { self.advance_events() };
            }
            if found.is_some() {
                return found;
            }
            if crate::idt::ticks() >= deadline {
                return None;
            }

            if spun < SPIN_MICROS {
                crate::apic::busy_wait_micros(100);
                spun += 100;
            } else {
                let _ = crate::syscall::invoke(
                    crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
                    [per_tick, 0, 0, 0, 0, 0],
                );
            }
        }
    }

    /// Acknowledges the change bits of a port.
    ///
    /// They are "write one to clear" bits: copying them back as-is
    /// acknowledges them, leaving them at zero **preserves** them. That is
    /// the classic mistake with this register, and it shows as a port
    /// signalling a change indefinitely.
    fn acknowledge_port(&self, port: u8) {
        if port == 0 || port > self.max_ports {
            return;
        }
        let register = self.port_register(port);
        let status = unsafe { read32(register) };
        unsafe {
            write32(
                register,
                (status & port::PRESERVE) | (status & port::CHANGE_MASK),
            )
        };
    }

    /// Drops a command and waits for its completion.
    fn command(&mut self, parameter: u64, control: u32) -> Option<Event> {
        let address = self.command.push(parameter, 0, control);
        unsafe { self.ring_doorbell(0, 0) };

        self.wait_event(1000, |event| {
            event.kind == trb::COMMAND_COMPLETION && event.parameter == address
        })
    }

    // --------------------------------------------------------
    //  Ports
    // --------------------------------------------------------

    fn port_register(&self, port: u8) -> u64 {
        self.operational + op::PORTSC + (port as u64 - 1) * 0x10
    }

    /// Resets a port and waits for the link to be established.
    ///
    /// The change bits are copied back as-is in the write: they are
    /// "write one to clear" bits, and leaving them at zero **would
    /// preserve them** instead of acknowledging them. That is the classic
    /// mistake with this register, and it shows as a port that seems to
    /// change state indefinitely.
    ///
    /// # Safety
    /// Writes a port register.
    unsafe fn reset_port(&self, port: u8) -> bool {
        let register = self.port_register(port);
        let status = unsafe { read32(register) };

        if status & port::CCS == 0 {
            return false;
        }

        // A port already enabled stays so: the SuperSpeed ports train
        // themselves and need no reset.
        if status & port::PED != 0 {
            unsafe { write32(register, (status & port::PRESERVE) | port::CSC) };
            return true;
        }

        unsafe { write32(register, (status & port::PRESERVE) | port::RESET) };

        for _ in 0..500 {
            let current = unsafe { read32(register) };
            if current & port::PRC != 0 {
                unsafe {
                    write32(
                        register,
                        (current & port::PRESERVE) | port::PRC | port::CSC,
                    )
                };
                return unsafe { read32(register) } & port::PED != 0;
            }
            crate::apic::busy_wait_micros(1000);
        }
        false
    }

    fn port_speed(&self, port: u8) -> u8 {
        ((unsafe { read32(self.port_register(port)) } >> 10) & 0x0F) as u8
    }

    // --------------------------------------------------------
    //  Contexts
    // --------------------------------------------------------

    /// Offset of a context inside an input context.
    ///
    /// The input context starts with a control context, followed by the
    /// slot context, then one context per endpoint. The size of each
    /// depends on CSZ: computing it rather than hard-coding it is what
    /// makes the driver usable on both families of controllers.
    /// controleurs.
    fn input_offset(&self, index: usize) -> usize {
        index * self.context_size
    }

    /// Prepares an input context for `Address Device`.
    ///
    /// # Safety
    /// `input` must be a page of ours, and the slot must be active.
    unsafe fn build_address_context(
        &self,
        input: &DmaPage,
        transfer: &Ring,
        port: u8,
        speed: u8,
    ) {
        input.zero();

        // Control context: we add the slot (A0) and endpoint 0 (A1).
        unsafe { input.write_u32(self.input_offset(0) + 4, 0b11) };

        // Slot context.
        let slot = self.input_offset(1);
        // One context entry: only endpoint 0 exists for now.
        unsafe { input.write_u32(slot, ((speed as u32) << 20) | (1 << 27)) };
        unsafe { input.write_u32(slot + 4, (port as u32) << 16) };

        // Context of endpoint 0: Control type, three attempts.
        let endpoint = self.input_offset(2);
        unsafe { input.write_u32(endpoint, 0) };
        unsafe {
            input.write_u32(
                endpoint + 4,
                (3 << 1) | (4 << 3) | ((default_packet_size(speed) as u32) << 16),
            )
        };
        // Queue pointer, bit 0 = initial cycle state.
        unsafe { input.write_u64(endpoint + 8, transfer.phys() | 1) };
        // Average TRB length: eight bytes for control.
        unsafe { input.write_u32(endpoint + 16, 8) };
    }

    // --------------------------------------------------------
    //  Control transfers
    // --------------------------------------------------------

    /// Runs an incoming control transfer.
    ///
    /// Three TRBs: the request, the data, the status. Only the last
    /// carries "interrupt on completion" -- one event per stage would
    /// triple the traffic for information nobody reads.
    fn control_in(
        &mut self,
        slot: u8,
        ring_index: usize,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        buffer: &DmaPage,
        length: u16,
    ) -> Option<usize> {
        let setup = (request_type as u64)
            | ((request as u64) << 8)
            | ((value as u64) << 16)
            | ((index as u64) << 32)
            | ((length as u64) << 48);

        let has_data = length > 0;
        // Transfer type: 3 = incoming with data, 0 = with no data.
        let transfer_type = if has_data { 3u32 } else { 0 };

        let ring = &mut self.contexts[ring_index].2;

        ring.push(
            setup,
            8,
            (trb::SETUP << 10) | trb::IDT | (transfer_type << 16),
        );
        if has_data {
            ring.push(
                buffer.phys(),
                length as u32,
                (trb::DATA << 10) | (1 << 16),
            );
        }
        let last = ring.push(
            0,
            0,
            (trb::STATUS << 10) | trb::IOC | if has_data { 0 } else { 1 << 16 },
        );

        // Endpoint 1 of the doorbell: that is bidirectional endpoint 0.
        unsafe { self.ring_doorbell(slot, 1) };

        let event = self.wait_event(1000, |event| {
            event.kind == trb::TRANSFER_EVENT && event.parameter == last
        })?;

        if event.completion != COMPLETION_SUCCESS && event.completion != COMPLETION_SHORT_PACKET {
            crate::serial_println!(
                "[xhci] transfert de controle refuse - code {}",
                event.completion
            );
            return None;
        }

        Some(length as usize - event.residual as usize)
    }

    /// Outgoing control transfer with no data -- `SET_CONFIGURATION`,
    /// `SET_PROTOCOL`.
    fn control_out(
        &mut self,
        slot: u8,
        ring_index: usize,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
    ) -> bool {
        let setup = (request_type as u64)
            | ((request as u64) << 8)
            | ((value as u64) << 16)
            | ((index as u64) << 32);

        let ring = &mut self.contexts[ring_index].2;
        ring.push(setup, 8, (trb::SETUP << 10) | trb::IDT);
        let last = ring.push(0, 0, (trb::STATUS << 10) | trb::IOC | (1 << 16));

        unsafe { self.ring_doorbell(slot, 1) };

        self.wait_event(1000, |event| {
            event.kind == trb::TRANSFER_EVENT && event.parameter == last
        })
        .map(|event| event.completion == COMPLETION_SUCCESS)
        .unwrap_or(false)
    }

    // --------------------------------------------------------
    //  Enumeration
    // --------------------------------------------------------

    /// Walks the ports and adopts the pointing devices.
    pub fn enumerate(&mut self) {
        for port in 1..=self.max_ports {
            let status = unsafe { read32(self.port_register(port)) };
            if status & port::CCS == 0 {
                continue;
            }
            if !unsafe { self.reset_port(port) } {
                crate::serial_println!("[xhci] port {} : lien non etabli", port);
                continue;
            }
            let speed = self.port_speed(port);
            crate::serial_println!(
                "[xhci] port {} : peripherique connecte, vitesse {}",
                port,
                speed_name(speed)
            );

            if let Err(reason) = self.adopt(port, speed) {
                crate::serial_println!("[xhci] port {} abandonne - {}", port, reason);
            }
        }
    }

    /// Addresses, queries and configures the device of a port.
    fn adopt(&mut self, port: u8, speed: u8) -> Result<(), &'static str> {
        // --- 1: obtain a slot ---
        let event = self
            .command(0, trb::ENABLE_SLOT << 10)
            .ok_or("aucune reponse a l'activation de slot")?;
        if event.completion != COMPLETION_SUCCESS {
            return Err("activation de slot refusee");
        }
        let slot = event.slot;

        // --- 2: contexts and control ring ---
        let input = DmaPage::new().ok_or("memoire insuffisante")?;
        let output = DmaPage::new().ok_or("memoire insuffisante")?;
        let control_ring = Ring::new().ok_or("memoire insuffisante")?;

        unsafe { self.build_address_context(&input, &control_ring, port, speed) };
        unsafe { self.dcbaa.write_u64(slot as usize * 8, output.phys()) };

        let ring_index = self.contexts.len();
        self.contexts.push((input, output, control_ring));

        // --- 3: address ---
        let input_phys = self.contexts[ring_index].0.phys();
        let event = self
            .command(
                input_phys,
                (trb::ADDRESS_DEVICE << 10) | ((slot as u32) << 24),
            )
            .ok_or("aucune reponse a l'adressage")?;
        if event.completion != COMPLETION_SUCCESS {
            return Err("adressage refuse");
        }

        // --- 4: device descriptor ---
        let buffer = DmaPage::new().ok_or("memoire insuffisante")?;
        let read = self
            .control_in(slot, ring_index, 0x80, 6, 0x0100, 0, &buffer, 18)
            .ok_or("descripteur de peripherique illisible")?;
        let device = descriptor::Device::parse(buffer.as_slice(read))
            .ok_or("descripteur de peripherique mal forme")?;

        crate::serial_println!(
            "[xhci] slot {} : {:04x}:{:04x}, USB {}.{}, classe {}",
            slot,
            device.vendor,
            device.product,
            device.usb_version >> 8,
            (device.usb_version >> 8) & 0xFF,
            device.class
        );

        // --- 5: configuration ---
        //
        // Two transfers: the header alone gives the total length, which is
        // then asked for exactly. Claiming an arbitrary size would
        // truncate a rich configuration without our knowing.
        let read = self
            .control_in(slot, ring_index, 0x80, 6, 0x0200, 0, &buffer, 9)
            .ok_or("descripteur de configuration illisible")?;
        let total = descriptor::configuration_total_length(buffer.as_slice(read))
            .ok_or("descripteur de configuration mal forme")?;

        let total = total.min(4096);
        let read = self
            .control_in(slot, ring_index, 0x80, 6, 0x0200, 0, &buffer, total)
            .ok_or("configuration complete illisible")?;
        let configuration = descriptor::parse_configuration(buffer.as_slice(read))
            .ok_or("configuration mal formee")?;

        let interface = configuration.hid.ok_or("aucune interface HID de pointage")?;

        // --- 6: declare the interrupt endpoint ---
        let transfer = Ring::new().ok_or("memoire insuffisante")?;
        let endpoint_id = interface.xhci_endpoint_id();
        unsafe { self.build_endpoint_context(ring_index, endpoint_id, &interface, &transfer, speed) };

        let input_phys = self.contexts[ring_index].0.phys();
        let event = self
            .command(
                input_phys,
                (trb::CONFIGURE_ENDPOINT << 10) | ((slot as u32) << 24),
            )
            .ok_or("aucune reponse a la configuration d'endpoint")?;
        if event.completion != COMPLETION_SUCCESS {
            return Err("configuration d'endpoint refusee");
        }

        // --- 7: enable the configuration, then the protocol ---
        if !self.control_out(slot, ring_index, 0x00, 9, configuration.value as u16, 0) {
            return Err("SET_CONFIGURATION refuse");
        }

        // `SET_PROTOCOL(0)` asks for the boot layout. A device that does
        // not know it refuses the request: that is not an error, it is a
        // tablet, and its report is read differently. So it is only
        // attempted for an interface that announces itself as such.
        if interface.is_boot_mouse() {
            let _ = self.control_out(slot, ring_index, 0x21, 0x0B, 0, interface.number as u16);
        }

        let report_length = interface.max_packet.min(64) as usize;

        crate::serial_println!(
            "[xhci] slot {} : interface HID {}.{}.{} - endpoint 0x{:02x}, rapport de {} octets",
            slot,
            interface.class,
            interface.subclass,
            interface.protocol,
            interface.endpoint,
            report_length
        );

        let report = DmaPage::new().ok_or("memoire insuffisante")?;
        let usb = UsbDevice {
            slot,
            port,
            speed,
            descriptor: device,
            interface,
            transfer,
            report,
            report_length,
            armed: false,
        };

        // The first transfer is armed straight away: without it, the
        // controller has nothing to fill and the first mouse movement
        // would be lost.
        self.devices.push(usb);
        let index = self.devices.len() - 1;
        self.arm(index);
        Ok(())
    }

    /// Adds the interrupt endpoint to the input context.
    ///
    /// # Safety
    /// `ring_index` must name a context of ours.
    unsafe fn build_endpoint_context(
        &self,
        ring_index: usize,
        endpoint_id: u8,
        interface: &Interface,
        transfer: &Ring,
        speed: u8,
    ) {
        let input = &self.contexts[ring_index].0;

        // Control context: nothing to drop, we add the slot and the new
        // endpoint. The slot must be there because its number of entries
        // changes.
        unsafe { input.write_u32(self.input_offset(0), 0) };
        unsafe {
            input.write_u32(
                self.input_offset(0) + 4,
                1 | (1 << endpoint_id),
            )
        };

        // The slot context now announces as many entries as the highest
        // endpoint: the controller does not read beyond.
        let slot = self.input_offset(1);
        let dw0 = unsafe { input.read_u32(slot) };
        unsafe {
            input.write_u32(
                slot,
                (dw0 & 0x07FF_FFFF) | ((endpoint_id as u32) << 27),
            )
        };

        // The xHCI interval is an exponent of 125 microseconds. The
        // descriptor expresses it in milliseconds for the low and full
        // speeds, and already as an exponent for high speed: the two
        // conventions do not convert the same way.
        let interval = match speed {
            1 | 2 => {
                let millis = interface.interval.max(1) as u32;
                // 8 micro-frames per millisecond, that is the base-two
                // logarithm exponent, plus three.
                (31 - (millis.leading_zeros() ^ 31)).min(10) + 3
            }
            _ => (interface.interval.max(1) as u32 - 1).min(15),
        };

        let endpoint = self.input_offset(1 + endpoint_id as usize);
        unsafe { input.write_u32(endpoint, interval << 16) };
        // Type 7: Interrupt IN. Three attempts before giving up.
        unsafe {
            input.write_u32(
                endpoint + 4,
                (3 << 1) | (7 << 3) | ((interface.max_packet as u32) << 16),
            )
        };
        unsafe { input.write_u64(endpoint + 8, transfer.phys() | 1) };
        unsafe {
            input.write_u32(
                endpoint + 16,
                interface.max_packet as u32 | ((interface.max_packet as u32) << 16),
            )
        };
    }

    /// Drops an incoming transfer on the interrupt endpoint.
    ///
    /// Takes the index of the device rather than a reference: dropping on
    /// the ring needs the device mutably, ringing needs the controller,
    /// and the two borrows overlap. The index separates them in time.
    fn arm(&mut self, index: usize) {
        let (slot, target) = {
            let Some(device) = self.devices.get_mut(index) else {
                return;
            };
            if device.armed {
                return;
            }
            let length = device.report_length as u32;
            let buffer = device.report.phys();
            device
                .transfer
                .push(buffer, length, (trb::NORMAL << 10) | trb::IOC);
            device.armed = true;
            (device.slot, device.interface.xhci_endpoint_id() as u32)
        };

        unsafe { self.ring_doorbell(slot, target) };
    }

    /// Consumes the pending events and returns the reports received.
    ///
    /// Returns `(slot, report)` for each completed transfer. The transfer
    /// is re-armed immediately: an interrupt endpoint with no pending TRB
    /// produces nothing more, and the mouse would freeze after its first
    /// movement.
    pub fn poll(&mut self) -> Vec<(u8, Vec<u8>)> {
        let mut reports = Vec::new();
        let mut consumed = false;

        while let Some(event) = self.events.pop() {
            consumed = true;
            match event.kind {
                trb::TRANSFER_EVENT => {
                    // The slot **and** the endpoint must match: a device
                    // may expose several, and taking the report of one
                    // for that of the other would fill the buffer wrong.
                    let Some(index) = self.devices.iter().position(|device| {
                        device.slot == event.slot
                            && device.interface.xhci_endpoint_id() == event.endpoint
                    }) else {
                        continue;
                    };

                    self.devices[index].armed = false;

                    if event.completion == COMPLETION_SUCCESS
                        || event.completion == COMPLETION_SHORT_PACKET
                    {
                        let device = &self.devices[index];
                        let length = device.report_length - event.residual as usize;
                        reports.push((device.slot, device.report.as_slice(length).to_vec()));
                    }
                }

                // Hot plug is not handled: the change is acknowledged so
                // that the port stops signalling it, but no enumeration is
                // relaunched. Recorded limit.
                trb::PORT_STATUS_CHANGE => self.acknowledge_port(event.port),

                _ => {}
            }
        }

        if consumed {
            unsafe { self.advance_events() };
        }

        // Re-arming once the event loop is over: an interrupt endpoint
        // with no pending TRB produces nothing more, and the mouse would
        // freeze after its first movement.
        for index in 0..self.devices.len() {
            self.arm(index);
        }

        reports
    }

    pub fn device_count(&self) -> usize {
        self.devices.len()
    }

    pub fn base_address(&self) -> u64 {
        self.base
    }
}

//! USB stack -- xHCI controller and HID pointing devices.
//!
//! ## Why USB on top of PS/2
//!
//! PS/2 works: phase 6.1.2 proves it, IRQ 12 included. But a PS/2 mouse
//! returns only **displacements**. It has no position to communicate, so
//! a host emulating it must commandeer the physical pointer before
//! transmitting anything at all -- that is what the capture of an
//! emulator window does, and that is what gives the impression that "the
//! mouse does not work" until one has clicked.
//!
//! An **absolute** USB pointing device returns a position. It means
//! something with no capture: the guest cursor follows the host's
//! directly. On real hardware the question does not arise otherwise --
//! machines built in the last fifteen years have no PS/2 port at all, and
//! the 8042 they still expose is itself emulated by the firmware.
//!
//! Both sources therefore coexist, and feed **the same pointer state**:
//! `drivers::mouse` holds the position, the button and the surface, and
//! publishes on `dev.mouse`. Nothing downstream knows which of the two
//! produced an event, and that is intended -- the compositor has no
//! business knowing the topology of the bus.
//!
//! ## What this stack does, and what it does not
//!
//! It enumerates the devices present at boot, keeps the HID pointing
//! interfaces, and reads their reports. It handles neither hubs, nor hot
//! plug, nor mass storage, nor anything but pointing. Each of those
//! points is a recorded limit, not an oversight.

pub mod descriptor;
pub mod hid;
pub mod xhci;

use core::sync::atomic::{AtomicU64, Ordering};

use crate::pci;
use crate::vga::SpinLock;

/// PCI class of a USB controller.
const PCI_CLASS_SERIAL: u8 = 0x0C;
const PCI_SUBCLASS_USB: u8 = 0x03;
/// xHCI programming interface.
const PCI_PROGIF_XHCI: u8 = 0x30;

/// The controller, once adopted.
///
/// One only: a machine may carry several, but the first presenting a
/// pointing device is enough, and nothing justifies keeping several
/// stacks for one mouse.
static CONTROLLER: SpinLock<Option<xhci::Controller>> = SpinLock::new(None);

static REPORTS: AtomicU64 = AtomicU64::new(0);
static MOVES: AtomicU64 = AtomicU64::new(0);

/// State of the subsystem, for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// No xHCI controller on the PCI bus.
    Absent,
    /// Controller present, no pointing device.
    NoPointer,
    /// A pointing device answers.
    Ready,
}

static STATE: SpinLock<State> = SpinLock::new(State::Absent);

pub fn state() -> State {
    *STATE.lock()
}

/// Counters: (reports received, movements applied).
pub fn counters() -> (u64, u64) {
    (
        REPORTS.load(Ordering::Relaxed),
        MOVES.load(Ordering::Relaxed),
    )
}

/// Looks for an xHCI controller and enumerates what is plugged into it.
///
/// Returns the number of pointing devices adopted.
///
/// # Safety
/// Takes possession of a PCI controller and enables its bus mastering.
/// Must be called once only, from the driver task.
pub unsafe fn init() -> usize {
    let candidates: alloc::vec::Vec<pci::Device> = pci::enumerate()
        .into_iter()
        .filter(|device| {
            device.class == PCI_CLASS_SERIAL
                && device.subclass == PCI_SUBCLASS_USB
                && device.prog_if == PCI_PROGIF_XHCI
        })
        .collect();

    if candidates.is_empty() {
        // A USB controller of another generation is reported rather than
        // ignored: knowing there is one and that we do not drive it is
        // worth more than believing the machine has no USB.
        let others = pci::enumerate()
            .into_iter()
            .filter(|device| {
                device.class == PCI_CLASS_SERIAL && device.subclass == PCI_SUBCLASS_USB
            })
            .count();
        if others > 0 {
            crate::serial_println!(
                "[usb] {} controleur(s) USB non xHCI ignore(s) - seul xHCI est pilote",
                others
            );
        } else {
            crate::serial_println!("[usb] aucun controleur USB sur le bus PCI");
        }
        *STATE.lock() = State::Absent;
        return 0;
    }

    for device in candidates {
        let Some(mut controller) = (unsafe { xhci::Controller::attach(&device) }) else {
            continue;
        };
        controller.enumerate();

        let count = controller.device_count();
        if count == 0 {
            crate::serial_println!("[usb] controleur pret, aucun peripherique de pointage");
            *STATE.lock() = State::NoPointer;
            *CONTROLLER.lock() = Some(controller);
            return 0;
        }

        for pointer in controller.devices.iter() {
            crate::serial_println!(
                "[usb] pointage adopte - slot {}, port {}, {}",
                pointer.slot,
                pointer.port,
                pointer.shape().as_str()
            );
        }

        *STATE.lock() = State::Ready;
        *CONTROLLER.lock() = Some(controller);
        return count;
    }

    *STATE.lock() = State::Absent;
    0
}

/// Consumes the pending reports and updates the pointer state.
///
/// Returns the number of events published on `dev.mouse`.
///
/// The controller lock is **released before** the publication: the latter
/// takes the IPC lock and may wake the compositor, two things one does
/// not nest under a driver lock.
pub fn poll() -> usize {
    let reports = {
        let mut guard = CONTROLLER.lock();
        let Some(controller) = guard.as_mut() else {
            return 0;
        };

        let raw = controller.poll();
        if raw.is_empty() {
            return 0;
        }

        // The form is read back with the report: two devices of different
        // forms can coexist, and reading the report of one with the
        // layout of the other would send the cursor anywhere.
        raw.into_iter()
            .filter_map(|(slot, bytes)| {
                controller
                    .devices
                    .iter()
                    .find(|device| device.slot == slot)
                    .map(|device| (device.shape(), bytes))
            })
            .collect::<alloc::vec::Vec<_>>()
    };

    let mut published = 0;
    for (shape, bytes) in reports {
        REPORTS.fetch_add(1, Ordering::Relaxed);
        let Some(report) = hid::decode(shape, &bytes) else {
            continue;
        };

        // A report where nothing moved is not published: a tablet emits
        // at a fixed rate, and republishing an identical position would
        // repaint the cursor a hundred times a second for nothing.
        let applied = match report.absolute {
            Some((x, y)) => crate::drivers::mouse::set_absolute(x, y, report.buttons, report.wheel),
            None => {
                crate::drivers::mouse::apply_relative(report.dx, report.dy, report.buttons, report.wheel)
            }
        };

        if applied {
            MOVES.fetch_add(1, Ordering::Relaxed);
            published += 1;
        }
    }

    published
}

/// Does a USB pointing device answer?
pub fn has_pointer() -> bool {
    state() == State::Ready
}

pub fn dump_state() {
    let (reports, moves) = counters();
    match state() {
        State::Absent => crate::serial_println!("[usb] aucun controleur xHCI"),
        State::NoPointer => {
            crate::serial_println!("[usb] controleur xHCI pret, aucun pointage branche")
        }
        State::Ready => {
            let guard = CONTROLLER.lock();
            let count = guard.as_ref().map(|c| c.device_count()).unwrap_or(0);
            drop(guard);
            crate::serial_println!(
                "[usb] {} peripherique(s) de pointage - {} rapports recus, {} mouvements",
                count,
                reports,
                moves
            );
        }
    }
}

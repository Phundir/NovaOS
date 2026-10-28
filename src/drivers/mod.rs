//! Phase 6.1 -- User-space drivers.
//!
//! ## What "user space" means here, and what it does not
//!
//! Spec 1.1 places the drivers above the system API, outside the
//! microkernel. Making them today as real ring 3 binaries would assume an
//! SDK able to produce a standalone `.dsa` -- that is phase 9.1, and
//! nothing before it compiles a separate program.
//!
//! What is real from now on is the **interface discipline**, and it
//! carries most of the architectural value:
//!
//! * a driver is a **scheduled task**, with its stack, its priority and
//!   the right to sleep -- not a function called from an IRQ;
//! * it receives its interrupts only through `drivers::irq`, whose kernel
//!   handler does no more than count and wake;
//! * it exposes its service only through a **named IPC channel**, never
//!   through a function call; its clients know nothing but that name;
//! * it sleeps with `SYS_TIME_SLEEP_MS`, reads the time with
//!   `SYS_TIME_*`, writes its logs with `SYS_FS_*`: a program's own calls.
//!
//! What is not: the driver tasks run in ring 0 and share the address
//! space of the kernel. A faulty driver can therefore still corrupt the
//! system. The move to ring 3 will not change a line of the interfaces
//! above -- which is precisely why they are laid down now.
//!
//! ## The registry
//!
//! A driver declares itself, then announces that it is ready or that it
//! has failed. That state is not decorative: `init.dsa` (6.5) queries it
//! to decide whether to carry on booting or to emit a `BOOT_ERR_*`, and
//! `SYS_HW_DRIVER_LIST` returns it to user space.

pub mod audio;
pub mod e1000;
pub mod nic;
pub mod virtio_net;
pub mod irq;
pub mod keyboard;
pub mod mouse;
pub mod ps2;
pub mod selftest;
pub mod storage;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::process::Pid;
use crate::vga::SpinLock;

/// Maximum number of registered drivers.
const MAX_DRIVERS: usize = 16;

/// Device family, as spec 6.1 enumerates them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverClass {
    Input,
    Storage,
    Network,
    Audio,
}

impl DriverClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            DriverClass::Input => "entree",
            DriverClass::Storage => "stockage",
            DriverClass::Network => "reseau",
            DriverClass::Audio => "audio",
        }
    }

    /// Stable code, returned to user space by `SYS_HW_DRIVER_LIST`.
    pub fn code(&self) -> u8 {
        match self {
            DriverClass::Input => 1,
            DriverClass::Storage => 2,
            DriverClass::Network => 3,
            DriverClass::Audio => 4,
        }
    }
}

/// Life cycle of a driver (spec 2.1, phases 6 and 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverState {
    /// Declared, initialisation in progress.
    Starting,
    /// Hardware found and initialised, service open.
    Ready,
    /// No hardware of this family -- the driver withdraws.
    Absent,
    /// Initialisation failed on hardware that is nonetheless present.
    Failed,
}

impl DriverState {
    pub fn as_str(&self) -> &'static str {
        match self {
            DriverState::Starting => "demarrage",
            DriverState::Ready => "pret",
            DriverState::Absent => "absent",
            DriverState::Failed => "echec",
        }
    }

    pub fn code(&self) -> u8 {
        match self {
            DriverState::Starting => 0,
            DriverState::Ready => 1,
            DriverState::Absent => 2,
            DriverState::Failed => 3,
        }
    }
}

/// Public state of a driver.
#[derive(Debug, Clone)]
pub struct DriverInfo {
    pub name: String,
    pub class: DriverClass,
    /// IPC channel through which the driver offers its service.
    pub channel: String,
    /// Description of the hardware driven, or the reason for its absence.
    pub device: String,
    pub state: DriverState,
    pub pid: Pid,
    /// Interrupt vector attached, when there is one.
    pub vector: Option<u8>,
    /// Events produced -- frames, packets, buffers.
    pub events: u64,
    pub errors: u64,
}

struct Slot {
    used: bool,
    info: DriverInfo,
}

impl Slot {
    const fn empty() -> Self {
        Slot {
            used: false,
            info: DriverInfo {
                name: String::new(),
                class: DriverClass::Input,
                channel: String::new(),
                device: String::new(),
                state: DriverState::Starting,
                pid: 0,
                vector: None,
                events: 0,
                errors: 0,
            },
        }
    }
}

static REGISTRY: SpinLock<[Slot; MAX_DRIVERS]> =
    SpinLock::new([const { Slot::empty() }; MAX_DRIVERS]);

/// Opaque reference handed to a driver when it declares itself.
#[derive(Debug, Clone, Copy)]
pub struct Handle(usize);

/// Declares a driver. Called by the driver task itself, at boot.
pub fn register(name: &str, class: DriverClass, channel: &str) -> Option<Handle> {
    let pid = crate::scheduler::current_pid();
    let mut registry = REGISTRY.lock();

    for (index, slot) in registry.iter_mut().enumerate() {
        if !slot.used {
            slot.used = true;
            slot.info = DriverInfo {
                name: name.to_string(),
                class,
                channel: channel.to_string(),
                device: String::new(),
                state: DriverState::Starting,
                pid,
                vector: None,
                events: 0,
                errors: 0,
            };
            return Some(Handle(index));
        }
    }
    None
}

/// Announces that the hardware is in service.
pub fn ready(handle: Handle, device: &str, vector: Option<u8>) {
    let mut registry = REGISTRY.lock();
    let slot = &mut registry[handle.0];
    slot.info.state = DriverState::Ready;
    slot.info.device = device.to_string();
    slot.info.vector = vector;
}

/// Announces that no hardware of this family is present.
pub fn absent(handle: Handle, reason: &str) {
    let mut registry = REGISTRY.lock();
    let slot = &mut registry[handle.0];
    slot.info.state = DriverState::Absent;
    slot.info.device = reason.to_string();
}

/// Announces an initialisation failure on hardware that is present.
pub fn failed(handle: Handle, reason: &str) {
    let mut registry = REGISTRY.lock();
    let slot = &mut registry[handle.0];
    slot.info.state = DriverState::Failed;
    slot.info.device = reason.to_string();
    slot.info.errors += 1;
}

/// Counts a handled event.
pub fn count_event(handle: Handle, howmany: u64) {
    let mut registry = REGISTRY.lock();
    registry[handle.0].info.events += howmany;
}

/// Counts a handling error.
pub fn count_error(handle: Handle) {
    let mut registry = REGISTRY.lock();
    registry[handle.0].info.errors += 1;
}

/// State of every declared driver.
pub fn list() -> Vec<DriverInfo> {
    let registry = REGISTRY.lock();
    registry
        .iter()
        .filter(|slot| slot.used)
        .map(|slot| slot.info.clone())
        .collect()
}

/// State of a driver named by its name.
pub fn find(name: &str) -> Option<DriverInfo> {
    let registry = REGISTRY.lock();
    registry
        .iter()
        .filter(|slot| slot.used)
        .find(|slot| slot.info.name == name)
        .map(|slot| slot.info.clone())
}

/// Is a driver of this family in service?
pub fn class_ready(class: DriverClass) -> bool {
    let registry = REGISTRY.lock();
    registry
        .iter()
        .any(|slot| slot.used && slot.info.class == class && slot.info.state == DriverState::Ready)
}

/// Vector assigned to a hardware line: 48 + IRQ number.
///
/// Vectors 32 and 33 are already taken by the timer and the keyboard, and
/// vectors 34 to 47 stay free for future IPIs. Starting at 48 gives an
/// immediate correspondence between line number and vector, which makes
/// an interrupt log readable with no conversion table.
///
/// No **PCI** line is routed today, and that is deliberate: a PCI line is
/// level triggered and active low, so it stays asserted until the cause
/// register of the device has been read. The generic relay of the kernel
/// reads no device register -- which is what lets it ignore what it
/// relays -- and would leave the line asserted: an interrupt storm.
/// Fixing that requires masking the line in the I/O APIC from the relay
/// and reopening it from the task once the cause is read. Until then, the
/// PCI device drivers poll.
pub const VECTOR_DEVICE_BASE: u8 = 48;

/// First of the vectors that PCI functions reach by MSI-X message
/// (`pci::Msix`): an edge, so the same count-and-wake relay serves them
/// with no storm to fear. Four are laid down; the network card uses the
/// first.
pub const VECTOR_MSI_BASE: u8 = 64;
pub const MSI_VECTORS: u8 = 4;
/// Vector of the network card's reception queue.
pub const VECTOR_NIC_RX: u8 = VECTOR_MSI_BASE;
/// Vector of the virtio-gpu control queue: the host returning requests,
/// among them the fenced submissions whose GPU work is done (phase 13).
pub const VECTOR_GPU: u8 = VECTOR_MSI_BASE + 1;

// ============================================================
//  Startup
// ============================================================

/// Creates the driver tasks.
///
/// The order follows spec 2.1: the input devices first -- they condition
/// the login screen -- then storage, network and audio. None is blocking:
/// a driver that does not find its hardware declares itself `Absent` and
/// withdraws, the boot carries on.
pub fn start_all() {
    use crate::process::BinaryType;

    // The drivers are system services: spec 7.2 reserves core 0 to `.dsa`,
    // and that is indeed the class of a driver -- a component with no
    // interface on which everything else depends.
    crate::scheduler::spawn("drv-clavier", BinaryType::Dsa, keyboard::task);
    crate::scheduler::spawn("drv-souris", BinaryType::Dsa, mouse::task);
    crate::scheduler::spawn("drv-stockage", BinaryType::Dsa, storage::task);
    crate::scheduler::spawn("drv-reseau", BinaryType::Dsa, nic::task);
    crate::scheduler::spawn("drv-audio", BinaryType::Dsa, audio::task);
}

/// Waits for every declared driver to leave the `Starting` state.
///
/// Returns `false` if the deadline is reached while a driver has still
/// announced nothing -- that is the `BOOT_ERR_0x0040` of 2.5.
pub fn wait_settled(expected: usize, timeout_ms: u64) -> bool {
    let per_tick = 1000 / crate::TIMER_HZ as u64;
    let deadline = crate::idt::ticks() + timeout_ms.div_ceil(per_tick).max(1);

    loop {
        let drivers = list();
        let settled = drivers
            .iter()
            .filter(|d| d.state != DriverState::Starting)
            .count();
        if drivers.len() >= expected && settled >= expected {
            return true;
        }
        if crate::idt::ticks() >= deadline {
            return false;
        }
        let _ = crate::syscall::invoke(
            crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
            [10, 0, 0, 0, 0, 0],
        );
    }
}

pub fn dump_state() {
    let drivers = list();
    crate::serial_println!("[drv] {} pilotes declares", drivers.len());
    for driver in drivers.iter() {
        crate::serial_println!(
            "[drv]   {:<12} {:<9} {:<10} PID {:<3} canal \"{}\" - {}",
            driver.name,
            driver.class.as_str(),
            driver.state.as_str(),
            driver.pid,
            driver.channel,
            driver.device
        );
        if driver.events != 0 || driver.errors != 0 {
            crate::serial_println!(
                "[drv]     {} evenements, {} erreurs{}",
                driver.events,
                driver.errors,
                match driver.vector {
                    Some(v) => alloc::format!(", vecteur {} ({} IRQ)", v, irq::count(v)),
                    None => String::new(),
                }
            );
        }
    }
    irq::dump_state();
}

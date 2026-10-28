//! The network card, whichever it is: the one entry point of the stack
//! towards the hardware, the driver task, and the `dev.net` channel.
//!
//! ## Two cards, one face
//!
//! `virtio_net` is probed first -- it is the fast path of a virtual
//! machine -- then `e1000`, the emulated real card and the fallback. The
//! first that answers is the card in service for the whole boot; `ACTIVE`
//! says which, and every call here goes to it. The stack (`net`) calls
//! `transmit` and receives through `net::input`, and knows nothing more:
//! adding the second card did not change a line of it.
//!
//! ## One poller at a time
//!
//! Frames are collected by polling, from the driver task and from any
//! task that waits on TCP (see `e1000::poll`). The driver task sleeps a
//! tick between laps, or -- when the card interrupts on reception
//! (virtio-net with MSI-X) -- until the next frame. Two pollers on
//! two cores would each take a batch and hand them to the stack in
//! whatever order they finish -- a reordering TCP takes for loss, and
//! answers with duplicate acknowledgements. `POLLING` lets one task in; a
//! second one finds it taken and returns at once, the frames being in
//! good hands. It is also what lets `virtio_net` lend its buffers to the
//! stack without copying them.
//!
//! ## Protocol of the `dev.net` channel
//!
//! The first byte of the payload carries the operation. An empty answer
//! counts as a refusal.
//!
//! ```text
//!   1 STATE     -> 4 bytes: card driven, link established, 2 reserved
//!   2 MAC       -> 6 bytes: hardware address
//!   3 COUNTERS  -> 24 bytes: sent, received, lost (u64 each)
//! ```
//!
//! The service carries no frames: an IPC payload caps at 256 bytes, which
//! is below an MTU. Sending and receiving stay direct calls of the `net`
//! stack, for exactly the reason that keeps the storage data path out of
//! IPC.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};

use crate::ipc::{self, Message, MessageKind};

use super::{DriverClass, e1000, virtio_net};

pub const CHANNEL: &str = "dev.net";

/// Operations of the `dev.net` channel, first byte of the payload.
pub const OP_STATE: u8 = 1;
pub const OP_MAC: u8 = 2;
pub const OP_COUNTERS: u8 = 3;

const NONE: u8 = 0;
const VIRTIO: u8 = 1;
const E1000: u8 = 2;

/// The card in service, set once by the driver task.
static ACTIVE: AtomicU8 = AtomicU8::new(NONE);
static POLLING: AtomicBool = AtomicBool::new(false);
/// Tick of the last poll that brought frames up.
static LAST_FRAMES: AtomicU64 = AtomicU64::new(0);

/// Drains the reception ring into the stack. Returns the frames handled;
/// zero also when another task is polling.
pub fn poll() -> usize {
    if POLLING.swap(true, Ordering::Acquire) {
        return 0;
    }
    let count = match ACTIVE.load(Ordering::Acquire) {
        VIRTIO => virtio_net::poll(),
        E1000 => e1000::poll(),
        _ => 0,
    };
    if count != 0 {
        // Still inside the poller's turn: the ACKs of this batch leave
        // before another task can bring up the next one.
        crate::net::tcp::flush_acks();
        LAST_FRAMES.store(crate::idt::ticks(), Ordering::Relaxed);
    }
    POLLING.store(false, Ordering::Release);
    count
}

/// Did frames arrive during the last two ticks? A task waiting on the
/// network keeps polling rather than sleeping while this holds: see
/// `tcp::wait_step`.
pub fn recently_active() -> bool {
    crate::idt::ticks().saturating_sub(LAST_FRAMES.load(Ordering::Relaxed)) < 2
}

/// Frames a poll takes at most: a full batch means more are waiting.
fn batch() -> usize {
    match ACTIVE.load(Ordering::Acquire) {
        VIRTIO => virtio_net::BATCH,
        _ => 64,
    }
}

/// Sends a frame on the card in service.
pub fn transmit(frame: &[u8]) -> Result<(), &'static str> {
    match ACTIVE.load(Ordering::Acquire) {
        VIRTIO => virtio_net::transmit(frame),
        E1000 => e1000::transmit(frame),
        _ => Err("aucune carte reseau pilotee"),
    }
}

/// Hardware address of the card, when one is in service.
pub fn mac() -> Option<[u8; 6]> {
    match ACTIVE.load(Ordering::Acquire) {
        VIRTIO => virtio_net::mac(),
        E1000 => e1000::mac(),
        _ => None,
    }
}

pub fn is_up() -> bool {
    ACTIVE.load(Ordering::Acquire) != NONE
}

/// Is the physical (or virtual) link established?
pub fn link_up() -> bool {
    match ACTIVE.load(Ordering::Acquire) {
        VIRTIO => virtio_net::link_up(),
        E1000 => e1000::link_up(),
        _ => false,
    }
}

/// Counters: (sent, received, lost).
pub fn counters() -> (u64, u64, u64) {
    match ACTIVE.load(Ordering::Acquire) {
        VIRTIO => virtio_net::counters(),
        E1000 => e1000::counters(),
        _ => (0, 0, 0),
    }
}

/// Model of the card in service, for display.
pub fn model() -> &'static str {
    match ACTIVE.load(Ordering::Acquire) {
        VIRTIO => "virtio-net",
        E1000 => "e1000",
        _ => "aucune",
    }
}

fn description() -> String {
    match ACTIVE.load(Ordering::Acquire) {
        VIRTIO => virtio_net::description(),
        E1000 => e1000::description(),
        _ => String::new(),
    }
}

/// Probes virtio-net, then the e1000. Both reasons are kept for the log
/// when neither answers.
fn probe() -> Result<(), String> {
    let virtio = match virtio_net::probe() {
        Ok(_) => {
            ACTIVE.store(VIRTIO, Ordering::Release);
            return Ok(());
        }
        Err(reason) => reason,
    };
    match e1000::probe() {
        Ok(_) => {
            ACTIVE.store(E1000, Ordering::Release);
            Ok(())
        }
        Err(reason) => Err(alloc::format!("{} ; {}", virtio, reason)),
    }
}

/// Answers a request of the `dev.net` channel.
///
/// Returning an empty vector is refusing: the service never fabricates a
/// default value a caller would take for an answer.
fn serve(payload: &[u8]) -> Vec<u8> {
    let operation = payload.first().copied().unwrap_or(0);

    match operation {
        OP_STATE => {
            let mut answer = Vec::with_capacity(4);
            answer.push(u8::from(is_up()));
            answer.push(u8::from(link_up()));
            answer.push(0);
            answer.push(0);
            answer
        }
        OP_MAC => match mac() {
            Some(address) => address.to_vec(),
            None => Vec::new(),
        },
        OP_COUNTERS => {
            let (sent, received, dropped) = counters();
            let mut answer = Vec::with_capacity(24);
            answer.extend_from_slice(&sent.to_le_bytes());
            answer.extend_from_slice(&received.to_le_bytes());
            answer.extend_from_slice(&dropped.to_le_bytes());
            answer
        }
        _ => Vec::new(),
    }
}

/// Driver task: probes, then polls the ring and serves the channel.
pub fn task() -> ! {
    let handle = match super::register("reseau", DriverClass::Network, CHANNEL) {
        Some(handle) => handle,
        None => crate::scheduler::exit_current_with(-1),
    };

    // The channel opens before the probe: a client resolving "dev.net"
    // must find the service even when no card is present, failing which
    // it cannot tell "no driver" from "no hardware".
    if ipc::open_channel(CHANNEL).is_err() {
        super::failed(handle, "canal deja ouvert");
        crate::scheduler::exit_current_with(-1);
    }

    match probe() {
        Ok(()) => super::ready(handle, &description(), None),
        Err(reason) => {
            super::absent(handle, &reason);
            crate::serial_println!("[drv-reseau] {}", reason);
            crate::scheduler::exit_current_with(0);
        }
    }

    // The stack learns the hardware address before the first frame: an
    // ARP answer received before the stack knows who it is would be
    // rejected.
    if let Some(address) = mac() {
        crate::net::attach(address);
    }

    crate::serial_println!("[drv-reseau] pret - carte {}, canal \"{}\"", model(), CHANNEL);

    // The reception interrupt, when the card has one routed.
    let mut line = match ACTIVE.load(Ordering::Acquire) {
        VIRTIO => virtio_net::rx_vector().and_then(super::irq::IrqLine::attach),
        _ => None,
    };

    loop {
        // The task polls the ring: so it cannot block on `recv`. The
        // requests are drained with no waiting at the top of the lap,
        // which bounds the answer latency to one polling period.
        while let Some(request) = ipc::try_recv() {
            if request.kind != MessageKind::Request {
                continue;
            }
            let answer = serve(&request.payload);
            let kind = if answer.is_empty() {
                MessageKind::Signal
            } else {
                MessageKind::Response
            };
            let reply = Message::new(crate::scheduler::current_pid(), kind, request.tag, &answer);
            if ipc::send(request.sender, reply).is_err() {
                super::count_error(handle);
            }
        }

        let mut handled = 0;
        loop {
            let count = poll();
            handled += count;
            if count < batch() {
                break;
            }
        }
        if handled != 0 {
            super::count_event(handle, handled as u64);
        }
        // TCP's timer: retransmissions, closed connections lingering.
        // Every lap, which is at least every tick (the waits below).
        crate::net::tcp::tick();

        match line.as_mut() {
            Some(line) => {
                // Interrupt on, then one more look: a frame that arrived
                // between the last poll and the arming raised no message,
                // and would otherwise wait for the deadline.
                virtio_net::arm_rx(true);
                if !virtio_net::rx_pending() {
                    // A tick at most: the `dev.net` requests are served
                    // between two waits, as they were between two sleeps.
                    line.wait(10);
                }
                virtio_net::arm_rx(false);
            }
            // Ten milliseconds: one tick. Going lower would serve no
            // purpose, the wake-up cannot be finer than the clock.
            None => {
                let _ = crate::syscall::invoke(crate::syscall::domain_time::SYS_TIME_SLEEP_MS, [10, 0, 0, 0, 0, 0]);
            }
        }
    }
}

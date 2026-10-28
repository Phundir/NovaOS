//! Requests of the run time, answered with the device's lock released.
//!
//! The boot-time `Gpu::run` spins for its answer with the lock held and
//! interrupts masked: right for the microseconds QEMU takes at boot,
//! wrong for the seconds it can take while a program holds the host's GPU
//! (13.11.3). Here the requests are built and sent under the lock, and
//! their answers waited for without it.

use alloc::vec::Vec;

use super::proto;
use super::queue::{Exchange, Ticket};
use super::{Gpu, GPU};


/// Longest wait for an answer at run time, past which the device is
/// declared out of service. Generous on purpose: while a program holds the
/// host's GPU, QEMU answers nothing that needs it -- for as long as the
/// watchdog takes to give up on that program (2 s) and the draw runs on
/// (the host cannot stop it). Seconds of silence are a busy host, not a
/// dead device.
const ANSWER_TIMEOUT_MS: u64 = 10_000;

/// A request built under the lock: its parts, and the size of its answer.
pub(super) struct Request {
    parts: Vec<Vec<u8>>,
    pub response: usize,
}

impl Request {
    pub(super) fn new(parts: Vec<Vec<u8>>, response: usize) -> Request {
        Request { parts, response }
    }

    /// A command answered with `OK_NODATA`.
    pub(super) fn nodata(parts: Vec<Vec<u8>>) -> Request {
        Request { parts, response: proto::HEADER_SIZE }
    }
}

/// Sends the requests `build` makes -- under the lock, with the device's
/// state at hand -- and waits for their answers **with the lock
/// released**: a short watch, then yields, then sleeps, bounded by
/// `ANSWER_TIMEOUT_MS`. The run-time path of the 3D side and of the
/// screen: the boot-time `run` spins with the lock held, which is right
/// for a few microseconds and wrong for the seconds a busy host can take.
pub(super) fn transact<R>(
    build: impl FnOnce(&mut Gpu) -> Result<(Vec<Request>, R), &'static str>,
) -> Result<(Vec<Vec<u8>>, R), &'static str> {
    let (tickets, extra) = {
        let mut guard = GPU.lock();
        let gpu = guard.as_mut().ok_or("pas de virtio-gpu")?;
        if gpu.broken {
            return Err("peripherique hors service");
        }
        let (requests, extra) = build(gpu)?;
        let parts: Vec<Vec<&[u8]>> = requests.iter().map(|r| r.parts.iter().map(|p| p.as_slice()).collect()).collect();
        let exchanges: Vec<Exchange> =
            parts.iter().zip(requests.iter()).map(|(p, r)| Exchange { request: p, response: r.response }).collect();
        let tickets = gpu.queue.submit(&exchanges);
        if tickets.iter().any(|ticket| ticket.is_none()) {
            for ticket in tickets.into_iter().flatten() {
                gpu.queue.forget(ticket);
            }
            return Err("file de controle pleine");
        }
        (tickets.into_iter().flatten().collect::<Vec<Ticket>>(), extra)
    };
    wait_answers(&tickets).map(|answers| (answers, extra))
}

/// The answers of `tickets`, in order, waited for with the lock released.
fn wait_answers(tickets: &[Ticket]) -> Result<Vec<Vec<u8>>, &'static str> {
    let take_all = || -> Option<Vec<Vec<u8>>> {
        let mut guard = GPU.lock();
        let gpu = guard.as_mut()?;
        gpu.queue.reap();
        if !tickets.iter().all(|ticket| gpu.queue.is_done(*ticket)) {
            return None;
        }
        Some(tickets.iter().filter_map(|ticket| gpu.queue.take(*ticket)).collect())
    };
    // QEMU answers most commands within microseconds: watch first, the
    // lock released and interrupts on between two looks; then give the
    // core away; then sleep a tick at a time.
    let started = crate::posix::time::monotonic_ns();
    loop {
        if let Some(answers) = take_all() {
            return Ok(answers);
        }
        let elapsed = crate::posix::time::monotonic_ns() - started;
        if elapsed >= ANSWER_TIMEOUT_MS * 1_000_000 {
            break;
        }
        if elapsed < 300_000 {
            core::hint::spin_loop();
        } else if elapsed < 20_000_000 {
            crate::scheduler::yield_now();
        } else {
            let _ = crate::syscall::invoke(crate::syscall::domain_time::SYS_TIME_SLEEP_MS, [10, 0, 0, 0, 0, 0]);
        }
    }
    if let Some(gpu) = GPU.lock().as_mut() {
        for ticket in tickets {
            gpu.queue.forget(*ticket);
        }
        gpu.broken = true;
    }
    crate::serial_println!("[virtio-gpu] le peripherique ne repond plus depuis 10 s - hors service");
    Err("peripherique sans reponse")
}

/// Were all answers `OK_NODATA`?
pub(super) fn all_ok(answers: &[Vec<u8>]) -> bool {
    answers.iter().all(|answer| proto::response_type(answer) == Some(proto::RESP_OK_NODATA))
}

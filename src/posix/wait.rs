//! Waiting: wait queues, timeouts, interruption by a signal.
//!
//! Every blocking call of the subsystem -- a read on an empty pipe, a
//! `poll`, a futex -- comes down to the same loop, written once here:
//!
//! ```text
//!   arm the sleep          (scheduler::arm_sleep)
//!   register on the queues
//!   test the condition     -> ready: cancel the sleep, done
//!   publish the deadline
//!   yield                  -> woken by a queue, the deadline, or a signal
//!   unregister, loop
//! ```
//!
//! The order is the one `scheduler::arm_sleep` documents: armed before the
//! test, a wake-up arriving between the test and the yield finds the task
//! asleep and makes it ready; tested after, it would be lost.
//!
//! Deadlines are counted in ticks (10 ms). A timeout of one millisecond
//! therefore lasts up to ten -- never less, which is the one guarantee
//! POSIX gives.

use alloc::vec::Vec;

use crate::process::Pid;
use crate::vga::SpinLock;

/// Tasks waiting for something to change.
pub struct WaitQueue {
    waiters: SpinLock<Vec<Pid>>,
    /// Incremented by every `notify_all`: an edge-triggered epoll reports
    /// an object again only once this has moved since its last report.
    generation: core::sync::atomic::AtomicU64,
}

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl WaitQueue {
    pub const fn new() -> Self {
        WaitQueue { waiters: SpinLock::new(Vec::new()), generation: core::sync::atomic::AtomicU64::new(0) }
    }

    pub fn register(&self, pid: Pid) {
        let mut waiters = self.waiters.lock();
        if !waiters.contains(&pid) {
            waiters.push(pid);
        }
    }

    pub fn unregister(&self, pid: Pid) {
        self.waiters.lock().retain(|p| *p != pid);
    }

    /// Wakes every waiter. They re-test their own condition: a queue
    /// says "something changed", never "your turn".
    pub fn notify_all(&self) {
        self.generation.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
        let woken: Vec<Pid> = core::mem::take(&mut *self.waiters.lock());
        for pid in woken {
            crate::scheduler::wake(pid);
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(core::sync::atomic::Ordering::Acquire)
    }

    pub fn is_empty(&self) -> bool {
        self.waiters.lock().is_empty()
    }
}

/// Why a wait ended without its condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitError {
    TimedOut,
    /// A signal is pending: the call returns EINTR (or restarts).
    Interrupted,
}

/// Blocks until `ready` returns `Some`, the tick `deadline` passes, or a
/// signal arrives for the caller.
pub fn wait_until<T>(
    queues: &[&WaitQueue],
    deadline: Option<u64>,
    mut ready: impl FnMut() -> Option<T>,
) -> Result<T, WaitError> {
    let pid = crate::scheduler::current_pid();
    loop {
        crate::scheduler::arm_sleep();
        for queue in queues {
            queue.register(pid);
        }
        if let Some(value) = ready() {
            crate::scheduler::cancel_sleep();
            unregister_all(queues, pid);
            return Ok(value);
        }
        if deadline.is_some_and(|d| crate::idt::fine_ticks() >= d) {
            crate::scheduler::cancel_sleep();
            unregister_all(queues, pid);
            return Err(WaitError::TimedOut);
        }
        if super::signal::interrupting() {
            crate::scheduler::cancel_sleep();
            unregister_all(queues, pid);
            return Err(WaitError::Interrupted);
        }
        if let Some(d) = deadline {
            add_timer(pid, d);
        }
        crate::scheduler::yield_now();
        unregister_all(queues, pid);
        if deadline.is_some() {
            remove_timer(pid);
        }
    }
}

fn unregister_all(queues: &[&WaitQueue], pid: Pid) {
    for queue in queues {
        queue.unregister(pid);
    }
}

/// Sleeps until the tick `deadline`, or a signal.
pub fn sleep_until(deadline: u64) -> Result<(), WaitError> {
    match wait_until::<()>(&[], Some(deadline), || None) {
        Err(WaitError::TimedOut) => Ok(()),
        Err(e) => Err(e),
        Ok(()) => Ok(()),
    }
}

// ============================================================
//  Deadlines
// ============================================================

/// Pending deadlines, (tick, pid). Short: one per blocked thread.
static TIMERS: SpinLock<Vec<(u64, Pid)>> = SpinLock::new(Vec::new());

fn add_timer(pid: Pid, deadline: u64) {
    let mut timers = TIMERS.lock();
    timers.retain(|(_, p)| *p != pid);
    timers.push((deadline, pid));
}

fn remove_timer(pid: Pid) {
    TIMERS.lock().retain(|(_, p)| *p != pid);
}

/// Wakes the threads whose deadline has come. Called from the timer
/// handler of core 0; the wake-ups happen outside the lock, as in
/// `domain_time::tick`.
pub fn tick(now: u64) {
    let due: Vec<Pid> = {
        let mut timers = TIMERS.lock();
        if timers.is_empty() {
            return;
        }
        let mut due = Vec::new();
        timers.retain(|(deadline, pid)| {
            if *deadline <= now {
                due.push(*pid);
                false
            } else {
                true
            }
        });
        due
    };
    for pid in due {
        crate::scheduler::wake(pid);
    }
}

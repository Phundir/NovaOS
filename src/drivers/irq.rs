//! 6.1.6 -- Demultiplexing of the interrupts towards the driver tasks.
//!
//! In a microkernel architecture, a driver cannot install its own
//! interrupt handler: the IDT belongs to the kernel, and a scheduled task
//! does not run in the context of an IRQ. The classic solution -- that of
//! L4 as of seL4 -- is to reduce the kernel handler to two gestures:
//! **count** the occurrence, and **wake** the task waiting for it. All
//! the logic of the device then lives in that task, with a stack, a
//! schedule and the right to sleep.
//!
//! That is exactly what this module does. The IDT handler calls
//! `notify(vector)`; the driver task calls `IrqLine::wait`.
//!
//! ## Why a counter rather than a flag
//!
//! A boolean flag loses the interrupts arriving while the task handles
//! the previous one: two close IRQs become a single handling pass, and
//! the device is left with an unread frame. A monotonic counter, compared
//! with the last value seen by the task, loses nothing: it says *how
//! many* events have happened since the last wake-up, and a single
//! handling pass can empty them all.
//!
//! ## Why a wait bounded in time
//!
//! `wait` sleeps in slices and hands back at the deadline even if no
//! interrupt has come. That is not a decorative precaution: the routing
//! of a PCI line to the I/O APIC depends on the redirection table of the
//! firmware, which nothing guarantees to be accurate. A driver waiting
//! forever would freeze on a machine where the line is badly described,
//! with no diagnostic at all. With the deadline, it degrades into
//! polling: slower, but alive -- and the counter of IRQs received,
//! visible in `drivers::dump_state`, says which of the two regimes is in
//! force.
//!
//! ## Missed wake-up window
//!
//! An IRQ occurring while the task is RUNNING calls `wake` on a task that
//! is not asleep: the wake-up has no effect. The task then goes to sleep
//! and waits for the deadline. That is not a loss -- the counter has
//! indeed been incremented, and `wait` tests it **before** sleeping -- it
//! is only a one-tick latency in a rare case.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::process::Pid;
use crate::vga::SpinLock;

/// Number of tasks able to wait on an interrupt at the same time.
///
/// One per active driver: the keyboard, the mouse, storage, the network
/// and audio consume five. Sixteen leaves margin without imposing an
/// allocation in the interrupt path.
const MAX_WAITERS: usize = 16;

/// Number of occurrences per vector, never reset.
static COUNTS: [AtomicU64; 256] = [const { AtomicU64::new(0) }; 256];

/// Interrupts arrived with nobody waiting for them -- diagnostics.
static UNCLAIMED: AtomicU64 = AtomicU64::new(0);

/// Table of the waits. A null vector marks a free slot: vector 0 is the
/// divide-by-zero exception, never an IRQ.
///
/// A fixed array and not a `Vec`: `notify` runs in interrupt context, and
/// the interrupt path must allocate nothing.
static WAITERS: SpinLock<[(u8, Pid); MAX_WAITERS]> = SpinLock::new([(0, 0); MAX_WAITERS]);

/// Number of interrupts received on a vector since boot.
pub fn count(vector: u8) -> u64 {
    COUNTS[vector as usize].load(Ordering::Relaxed)
}

/// Interrupts that occurred while no driver was waiting for them.
pub fn unclaimed() -> u64 {
    UNCLAIMED.load(Ordering::Relaxed)
}

/// Signals an interrupt. Called from an IDT handler.
///
/// The PIDs to wake are copied under the lock into a stack buffer, then
/// woken with the lock released: `scheduler::wake` takes the scheduler
/// lock and may raise an IPI, two operations one does not nest under a
/// lock taken in interrupt context.
pub fn notify(vector: u8) {
    COUNTS[vector as usize].fetch_add(1, Ordering::Relaxed);

    let mut due = [0u64; MAX_WAITERS];
    let mut found = 0;
    {
        let waiters = WAITERS.lock();
        for (line, pid) in waiters.iter() {
            if *line == vector {
                due[found] = *pid;
                found += 1;
            }
        }
    }

    if found == 0 {
        UNCLAIMED.fetch_add(1, Ordering::Relaxed);
        return;
    }

    for pid in due.iter().take(found) {
        crate::scheduler::wake(*pid);
    }
}

/// One interrupt line awaited by the current task.
///
/// The object remembers the last counter value observed: that is what
/// tells "nothing happened" from "three things happened while I was
/// working".
pub struct IrqLine {
    vector: u8,
    seen: u64,
    pid: Pid,
}

impl IrqLine {
    /// Attaches the current task to a vector.
    ///
    /// Returns `None` if the wait table is full -- one driver more than
    /// the system can serve must know it, not find itself silently
    /// polling.
    pub fn attach(vector: u8) -> Option<Self> {
        let pid = crate::scheduler::current_pid();
        let mut waiters = WAITERS.lock();

        for slot in waiters.iter_mut() {
            if slot.0 == 0 {
                *slot = (vector, pid);
                return Some(IrqLine {
                    vector,
                    seen: COUNTS[vector as usize].load(Ordering::Relaxed),
                    pid,
                });
            }
        }
        None
    }

    /// Vector watched.
    pub fn vector(&self) -> u8 {
        self.vector
    }

    /// Waits for an interrupt, for at most `timeout_ms` milliseconds.
    ///
    /// Returns the number of interrupts that occurred since the last call
    /// -- zero if the deadline was reached with nothing.
    pub fn wait(&mut self, timeout_ms: u64) -> u64 {
        let per_tick = 1000 / crate::TIMER_HZ as u64;
        let deadline = crate::idt::ticks() + timeout_ms.div_ceil(per_tick).max(1);

        loop {
            let now = COUNTS[self.vector as usize].load(Ordering::Relaxed);
            if now != self.seen {
                let delta = now - self.seen;
                self.seen = now;
                return delta;
            }
            if crate::idt::ticks() >= deadline {
                return 0;
            }
            // The sleep goes through the system call, not through a
            // direct access to the scheduler: a driver has no means other
            // than those of a program, that is the whole discipline of
            // phase 6.1.
            let _ = crate::syscall::invoke(
                crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
                [per_tick, 0, 0, 0, 0, 0],
            );
        }
    }

    /// Consumes the pending interrupts without sleeping.
    pub fn poll(&mut self) -> u64 {
        let now = COUNTS[self.vector as usize].load(Ordering::Relaxed);
        let delta = now - self.seen;
        self.seen = now;
        delta
    }
}

impl Drop for IrqLine {
    fn drop(&mut self) {
        let mut waiters = WAITERS.lock();
        for slot in waiters.iter_mut() {
            if slot.0 == self.vector && slot.1 == self.pid {
                *slot = (0, 0);
                return;
            }
        }
    }
}

/// Logs the active lines.
pub fn dump_state() {
    let waiters = *WAITERS.lock();
    crate::serial_println!("[irq] {} interruptions sans destinataire", unclaimed());
    for (vector, pid) in waiters.iter() {
        if *vector != 0 {
            crate::serial_println!(
                "[irq]   vecteur {} -> PID {} - {} recues",
                vector,
                pid,
                count(*vector)
            );
        }
    }
}

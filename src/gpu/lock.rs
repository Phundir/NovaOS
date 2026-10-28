//! A lock whose holder may sleep.
//!
//! The kernel's `SpinLock` masks interrupts for as long as it is held --
//! the right tool for a structure an interrupt handler touches, and the
//! wrong one for a GPU context: creating a pipeline waits for the host to
//! answer, milliseconds during which the core must keep taking its timer
//! and other tasks must keep running. This lock parks a contender on a
//! wait queue instead of spinning, and its holder may sleep.
//!
//! It is not reentrant, and it is not for interrupt context: `lock` may
//! sleep.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::posix::wait::{self, WaitQueue};

pub struct SleepLock<T> {
    locked: AtomicBool,
    queue: WaitQueue,
    value: UnsafeCell<T>,
}

// SAFETY: access to `value` is serialized by `locked`.
unsafe impl<T: Send> Sync for SleepLock<T> {}
unsafe impl<T: Send> Send for SleepLock<T> {}

impl<T> SleepLock<T> {
    pub const fn new(value: T) -> Self {
        SleepLock { locked: AtomicBool::new(false), queue: WaitQueue::new(), value: UnsafeCell::new(value) }
    }

    fn try_take(&self) -> bool {
        self.locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok()
    }

    pub fn lock(&self) -> SleepGuard<'_, T> {
        loop {
            if self.try_take() {
                return SleepGuard { lock: self };
            }
            // Woken by the holder's release; a pending signal only makes
            // the wait return early, and the loop tries again after
            // giving the core away once.
            if wait::wait_until(&[&self.queue], None, || (!self.locked.load(Ordering::Acquire)).then_some(())).is_err() {
                crate::scheduler::yield_now();
            }
        }
    }

    pub fn try_lock(&self) -> Option<SleepGuard<'_, T>> {
        // Not `try_take().then_some(SleepGuard { .. })`: `then_some`
        // evaluates its argument whatever the outcome, and the guard built
        // for nothing would be dropped at once -- releasing the lock that
        // another task holds. That was the first version, and two tasks
        // then worked in one GPU context at the same time.
        if self.try_take() { Some(SleepGuard { lock: self }) } else { None }
    }
}

pub struct SleepGuard<'a, T> {
    lock: &'a SleepLock<T>,
}

impl<T> Deref for SleepGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the guard proves the lock is held.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for SleepGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the guard proves the lock is held, exclusively.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for SleepGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        self.lock.queue.notify_all();
    }
}

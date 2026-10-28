//! The session's command ring (13.5.2; see the parent module): its
//! layout in shared memory, how a batch is written and published, how a
//! caller waits for the host's thread, and how the ring is opened.

use super::*;

/// The ring's layout in its blob: the control words each on a line of
/// their own, then the buffer, a power of two -- as large as the largest
/// batch the control queue carries, so a batch never has to be cut for
/// the ring's sake.
pub(super) const RING_HEAD: u64 = 0;
pub(super) const RING_TAIL: u64 = 64;
pub(super) const RING_STATUS: u64 = 128;
pub(super) const RING_EXTRA: u64 = 192;
pub(super) const RING_EXTRA_SIZE: u64 = 64;
pub(super) const RING_BUFFER: u64 = 4096;
pub(super) const RING_BUFFER_SIZE: u64 = 1 << 20;
pub(super) const RING_SIZE: u64 = RING_BUFFER + RING_BUFFER_SIZE;

/// Time the host's ring thread keeps looking for work before it sleeps.
pub(super) const RING_IDLE_NS: u64 = 20_000_000;

/// `VK_RING_STATUS_*_BIT_MESA`.
pub(super) const RING_STATUS_IDLE: u32 = 1;
pub(super) const RING_STATUS_FATAL: u32 = 2;

/// The id the session's ring has in the host's context.
pub(super) const RING_ID: u64 = 1;

/// Batches written into rings, and wake-ups sent to the host's sleeping
/// ring threads, since boot: for the state dump and the self-test.
pub static RING_BATCHES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static RING_WAKEUPS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
/// Sessions that could not open a ring.
pub static RING_REFUSED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// The session's ring: its blob, its kernel view, and the tail the
/// kernel last published.
pub(super) struct Ring {
    pub(super) blob: Mapping,
    va: u64,
    pub(super) tail: u32,
}

impl Ring {
    fn word(&self, offset: u64) -> *mut u32 {
        (self.va + offset) as *mut u32
    }

    pub(super) fn head(&self) -> u32 {
        // SAFETY: the control words stay mapped for the ring's life.
        let head = unsafe { self.word(RING_HEAD).read_volatile() };
        // The host stores `head` with release after writing its replies.
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        head
    }

    fn status(&self) -> u32 {
        // SAFETY: as above.
        unsafe { self.word(RING_STATUS).read_volatile() }
    }
}

impl Session {
    /// Writes a batch into the ring and publishes it. Returns the tail
    /// after it: the batch is decoded once `head` reaches that value.
    pub(super) fn ring_write(&mut self, bytes: &[u8]) -> Result<u32> {
        let length = bytes.len() as u64;
        if length > RING_BUFFER_SIZE {
            return Err(Error::Encoding);
        }
        // Room: what the host has not read yet stays.
        let tail = self.ring.as_ref().map(|r| r.tail).ok_or(Error::Lost)?;
        self.ring_wait(|ring| (tail.wrapping_sub(ring.head()) as u64) + length <= RING_BUFFER_SIZE)?;
        let ring = self.ring.as_mut().ok_or(Error::Lost)?;
        let start = (tail as u64) & (RING_BUFFER_SIZE - 1);
        let first = (RING_BUFFER_SIZE - start).min(length) as usize;
        let buffer = ring.va + RING_BUFFER;
        // SAFETY: both pieces lie inside the ring's buffer, mapped for the
        // ring's life; the host reads them only once the tail passes them.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), (buffer + start) as *mut u8, first);
            core::ptr::copy_nonoverlapping(bytes[first..].as_ptr(), buffer as *mut u8, bytes.len() - first);
        }
        let tail = tail.wrapping_add(length as u32);
        ring.tail = tail;
        // The bytes before the tail that publishes them; the tail before
        // the look at `status` -- the host sets IDLE, then looks at the
        // tail once more, so one of the two sides sees the other.
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        // SAFETY: the tail word, as above.
        unsafe { ring.word(RING_TAIL).write_volatile(tail) };
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        let status = ring.status();
        if status & RING_STATUS_FATAL != 0 {
            self.lost = true;
            return Err(Error::Lost);
        }
        RING_BATCHES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        if status & RING_STATUS_IDLE != 0 {
            RING_WAKEUPS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            let mut notify = Enc::new();
            notify_ring_mesa(&mut notify, 0, RING_ID, tail, 0);
            let submission = transport::submit(self.context, notify.bytes(), Fence::None).map_err(Error::Transport)?;
            transport::forget(submission);
        }
        Ok(tail)
    }

    /// Waits, bounded, for `ready` about the ring: first watching, then
    /// giving the core away, then sleeping a tick at a time. A ring the
    /// host declared fatal, or a wait past `REPLY_TIMEOUT_MS`, loses the
    /// session.
    pub(super) fn ring_wait(&mut self, ready: impl Fn(&Ring) -> bool) -> Result<()> {
        let started = crate::posix::time::monotonic_ns();
        loop {
            let ring = self.ring.as_ref().ok_or(Error::Lost)?;
            if ready(ring) {
                return Ok(());
            }
            if ring.status() & RING_STATUS_FATAL != 0 {
                self.lost = true;
                return Err(Error::Transport("anneau Venus refuse par l'hote"));
            }
            let elapsed = crate::posix::time::monotonic_ns() - started;
            if elapsed > REPLY_TIMEOUT_MS * 1_000_000 {
                self.lost = true;
                return Err(Error::Transport("anneau Venus sans reponse"));
            }
            if elapsed < 200_000 {
                core::hint::spin_loop();
            } else if elapsed < 20_000_000 {
                crate::scheduler::yield_now();
            } else {
                let _ = crate::syscall::invoke(crate::syscall::domain_time::SYS_TIME_SLEEP_MS, [10, 0, 0, 0, 0, 0]);
            }
        }
    }

    /// Opens the ring: its blob, mapped and zeroed (the host expects head
    /// and status at zero), created in the host's context through the
    /// control queue, then given the reply stream. `None` if any step
    /// fails: the session stays on the control queue.
    pub(super) fn open_ring(&mut self) -> Option<Ring> {
        let resource = transport::create_blob(self.context, 0, RING_SIZE, transport::BlobUse::Mapped).ok()?;
        let blob = match transport::map_blob(self.context, resource, RING_SIZE) {
            Ok(blob) => blob,
            Err(_) => {
                transport::destroy_resource(resource);
                return None;
            }
        };
        let give_back = |blob: Mapping| {
            transport::unmap_blob(self.context, blob);
            transport::destroy_resource(blob.resource);
        };
        let Some(va) = transport::map_kernel(&blob) else {
            give_back(blob);
            return None;
        };
        // SAFETY: the blob is mapped for RING_SIZE bytes just above.
        unsafe { core::ptr::write_bytes(va as *mut u8, 0, RING_BUFFER as usize) };
        let info = VkRingCreateInfoMESA {
            resource_id: blob.resource,
            offset: 0,
            size: RING_SIZE,
            idle_timeout: RING_IDLE_NS,
            head_offset: RING_HEAD,
            tail_offset: RING_TAIL,
            status_offset: RING_STATUS,
            buffer_offset: RING_BUFFER,
            buffer_size: RING_BUFFER_SIZE,
            extra_offset: RING_EXTRA,
            extra_size: RING_EXTRA_SIZE,
            ..Default::default()
        };
        let mut enc = Enc::new();
        create_ring_mesa(&mut enc, 0, RING_ID, &info);
        // Decoded before anything goes into the ring.
        let created = transport::submit(self.context, enc.bytes(), Fence::Decoded)
            .and_then(|submission| transport::wait(submission, REPLY_TIMEOUT_MS));
        if created.is_err() {
            transport::unmap_kernel(&blob);
            give_back(blob);
            return None;
        }
        Some(Ring { blob, va, tail: 0 })
    }

}

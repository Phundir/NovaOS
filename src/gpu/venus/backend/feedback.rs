//! The completion word: where the GPU says a command buffer is done.
//!
//! QEMU looks at the host's fences every ten milliseconds, so a virtio
//! fence says "done" up to a frame late. The last command of every
//! command buffer instead fills a word of host memory with the number of
//! its work (`record`); the kernel reads that word, and sees the end of
//! the work as soon as the GPU is there. Through the ring there is no
//! virtio fence at all: the word is the one sign.

use super::*;

/// Where the GPU writes the number of the last command buffer it
/// finished (see `record`): a word of host memory the kernel reads.
pub(super) struct Feedback {
    pub(super) memory: u64,
    pub(super) buffer: u64,
    pub(super) mapping: Mapping,
    address: u64,
}

impl VenusContext {
    /// Makes the completion word: a page of host memory (cached and
    /// coherent: the kernel reads it without invalidating), a buffer over
    /// it for the GPU's fill, and the kernel's view of it. It starts at
    /// `u32::MAX`, "before any work".
    pub(super) fn make_feedback(&mut self) -> Result<Feedback> {
        use crate::gpu::backend::Context as _;
        let (memory, _) = self.alloc(4096, MemKind::Readback)?;
        let made = (|| {
            self.map(memory)?;
            let mapping = self.memories.get(&memory).and_then(|info| info.blob).ok_or(Error::Device("mot de fin non projete"))?;
            let buffer = self.make_buffer(&backend::BufferDesc { size: 4096, usage: backend::buffer_usage::STORAGE }, memory, 0)?;
            let address = transport::map_kernel(&mapping).ok_or(Error::NoMemory)?;
            // SAFETY: the page is this context's, mapped just above.
            unsafe { (address as *mut u32).write_volatile(u32::MAX) };
            Ok(Feedback { memory, buffer, mapping, address })
        })();
        if made.is_err() {
            self.free(memory);
        }
        made
    }

    /// Has the GPU written the completion word of `work`, or a later one?
    pub(super) fn feedback_reached(&self, work: u64) -> bool {
        let Some(feedback) = self.feedback.as_ref() else {
            return false;
        };
        // SAFETY: the page stays mapped until `close`.
        let done = unsafe { (feedback.address as *const u32).read_volatile() };
        done != u32::MAX && done.wrapping_sub(work as u32) as i32 >= 0
    }

    /// Command buffers whose virtio answer came back since the completion
    /// word said they were done: free to record again.
    pub(super) fn collect_retired(&mut self) {
        let mut index = 0;
        while index < self.retired_ring.len() {
            let (work, buffer) = self.retired_ring[index];
            if self.feedback_reached(work + 1) {
                self.retired_ring.swap_remove(index);
                self.free_buffers.push(buffer);
            } else {
                index += 1;
            }
        }
        let mut index = 0;
        while index < self.retired.len() {
            let (submission, buffer) = self.retired[index];
            match transport::poll(submission) {
                None => index += 1,
                Some(answer) => {
                    self.retired.swap_remove(index);
                    if answer.is_ok() {
                        self.free_buffers.push(buffer);
                    } else {
                        self.session.mark_lost();
                    }
                }
            }
        }
    }

}

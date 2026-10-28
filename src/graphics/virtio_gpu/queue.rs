//! A virtqueue of virtio-gpu -- many requests in flight, answered in any
//! order. The control queue and the cursor queue are both one of these;
//! finding the device and negotiating is `transport`'s work.
//!
//! ## Why requests stay in flight
//!
//! The 2D driver of 7.1 waited for every answer, and could: a transfer or
//! a flush is answered as soon as the host has read it. The 3D commands of
//! phase 13 are not. A `SUBMIT_3D` carrying a fence is answered only once
//! the host's GPU has **finished** the work it submitted -- which is the
//! whole point of the fence, and may take milliseconds. The queue must
//! therefore hold many requests at once and hand each answer to whoever
//! asked, in whatever order the host completes them. Each request gets a
//! `Ticket`; the device's used ring says which ticket finished.
//!
//! ## One ring slot per request: indirect descriptors
//!
//! A request is a chain of segments -- the command, a scatter list, a
//! Venus stream of dozens of pages -- plus one writable segment for the
//! answer. Laid directly in the ring, a 200-page stream would take 201 of
//! its 256 descriptors. With `VIRTIO_RING_F_INDIRECT_DESC`, the ring slot
//! points at a **table** of descriptors living in a frame of its own: one
//! slot per request whatever its size, up to 255 segments of a page each.
//! Every slot owns its table frame for good; only the payload frames come
//! and go, through a small cache so that the present of each frame does
//! not go back to the frame allocator.
//!
//! ## Interrupts
//!
//! When the function has MSI-X, the control queue raises vector
//! `VECTOR_GPU` on core 0 whenever the device returns a request: the
//! relay counts and wakes the GPU task, which reaps the used ring. Without
//! MSI-X the ring is polled -- by that task every tick, and by whoever
//! waits for an answer. Nothing else changes: `reap` is the one place
//! that reads the used ring, and it is idempotent.
//!
//! ## The bound on waiting
//!
//! A synchronous command (`run`) spins, lock held and interrupts masked,
//! for a bounded number of turns: the lesson of DECISIONS.md "une attente
//! bornee se paie interruptions masquees". QEMU answers such commands in
//! tens of microseconds; a device that does not answer at all is declared

use alloc::vec::Vec;
use core::sync::atomic::{Ordering, fence};

use crate::memory::phys_to_virt;

use super::transport::{
    Transport, NO_VECTOR, QUEUE_DESC, QUEUE_DEVICE, QUEUE_DRIVER, QUEUE_ENABLE, QUEUE_MSIX_VECTOR, QUEUE_NOTIFY_OFF,
    QUEUE_SELECT, QUEUE_SIZE,
};

// --- split ring ---
const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;
const DESC_INDIRECT: u16 = 4;
const AVAIL_NO_INTERRUPT: u16 = 1;

/// Offsets of the available and used rings in their shared frame. For 256
/// entries: 518 bytes of available ring, 2054 of used ring.
const AVAIL_OFFSET: u64 = 0;
const USED_OFFSET: u64 = 1024;

const PAGE: usize = 4096;
/// Segments one request may carry: an indirect table fills a frame with
/// 256 descriptors, one of which is the answer.
pub const MAX_SEGMENTS: usize = 255;
/// Largest request, in bytes.
pub const MAX_REQUEST: usize = MAX_SEGMENTS * PAGE;

/// Frames kept for payloads between requests. A present posts two small
/// commands a frame; a Venus submission a few pages. Beyond this, frames
/// go back to the allocator.
const CACHE_FRAMES: usize = 512;
/// Frames put in the cache at bring-up: the steady state of a desktop --
/// presents, a few Venus calls in flight -- never reaches the allocator.
pub(super) const CACHE_PRIMED: usize = 64;

/// Spins before a synchronous request is declared lost.
const WAIT_SPINS: u32 = 50_000_000;

/// A request's place in the queue: the ring slot and the generation it
/// was issued under. A ticket of a finished and reused slot is stale, and
/// nothing it asks for can be answered by the slot's new owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticket {
    slot: u16,
    generation: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Free,
    /// Handed to the device; the issuer will collect the answer.
    Pending,
    /// Handed to the device; nobody will read the answer.
    Discarded,
    /// Returned by the device, answer not collected yet.
    Done,
}

struct Slot {
    /// Frame of the indirect descriptor table, owned for good.
    table: u64,
    /// Payload frames of the request in flight, and the answer frame.
    frames: Vec<u64>,
    answer: u64,
    answer_len: usize,
    state: State,
    generation: u32,
}

pub struct Queue {
    size: u16,
    /// Frame of the descriptor ring, and frame of the available and used
    /// rings.
    desc: u64,
    rings: u64,
    notify: u64,
    slots: Vec<Slot>,
    free: Vec<u16>,
    /// Next value of the available index, and used index read so far.
    avail_index: u16,
    used_seen: u16,
    cache: Vec<u64>,
    /// Requests handed to the device and not yet returned.
    in_flight: usize,
}

impl Queue {
    /// Sets up queue `index` with at most `max` entries: its rings, one
    /// indirect table per slot, and `primed` payload frames in its cache.
    /// `vector_entry` is the MSI-X table entry it raises, or `NO_VECTOR`.
    /// Returns the queue and whether it interrupts.
    ///
    /// # Safety
    /// `transport` is the device's, between FEATURES_OK and DRIVER_OK.
    pub(super) unsafe fn set_up(
        transport: &Transport,
        index: u16,
        max: u16,
        vector_entry: u16,
        primed: usize,
    ) -> Result<(Queue, bool), &'static str> {
        transport.write16(QUEUE_SELECT, index);
        let offered_size = transport.read16(QUEUE_SIZE);
        if offered_size == 0 {
            return Err("file virtio absente");
        }
        // The largest power of two within both limits.
        let mut size = 1u16;
        while size * 2 <= offered_size.min(max) {
            size *= 2;
        }

        let mut owned: Vec<u64> = Vec::new();
        let take = |owned: &mut Vec<u64>| -> Option<u64> {
            let frame = crate::frame::allocate_zeroed_frame()?.start_addr();
            owned.push(frame);
            Some(frame)
        };
        let give_back = |owned: &Vec<u64>| {
            for frame in owned {
                unsafe { crate::frame::deallocate_frame(crate::frame::Frame(*frame)) };
            }
        };
        let (Some(desc), Some(rings)) = (take(&mut owned), take(&mut owned)) else {
            give_back(&owned);
            return Err("plus de cadre pour l'anneau");
        };
        let mut slots = Vec::with_capacity(size as usize);
        for _ in 0..size {
            let Some(table) = take(&mut owned) else {
                give_back(&owned);
                return Err("plus de cadre pour les tables indirectes");
            };
            slots.push(Slot { table, frames: Vec::new(), answer: 0, answer_len: 0, state: State::Free, generation: 0 });
        }

        transport.write16(QUEUE_SIZE, size);
        transport.write16(QUEUE_MSIX_VECTOR, vector_entry);
        let interrupts = vector_entry != NO_VECTOR && transport.read16(QUEUE_MSIX_VECTOR) == vector_entry;
        if !interrupts {
            transport.write16(QUEUE_MSIX_VECTOR, NO_VECTOR);
            // Polled: the device need not interrupt at all.
            unsafe { ((phys_to_virt(rings) + AVAIL_OFFSET) as *mut u16).write_volatile(AVAIL_NO_INTERRUPT) };
        }
        transport.write64(QUEUE_DESC, desc);
        transport.write64(QUEUE_DRIVER, rings + AVAIL_OFFSET);
        transport.write64(QUEUE_DEVICE, rings + USED_OFFSET);
        let notify_off = transport.read16(QUEUE_NOTIFY_OFF) as u64;
        transport.write16(QUEUE_ENABLE, 1);

        let mut cache = Vec::with_capacity(CACHE_FRAMES);
        for _ in 0..primed {
            match crate::frame::allocate_frame() {
                Some(frame) => cache.push(frame.start_addr()),
                None => break,
            }
        }
        let queue = Queue {
            size,
            desc,
            rings,
            notify: transport.notify + notify_off * transport.notify_multiplier as u64,
            free: (0..size).rev().collect(),
            slots,
            avail_index: 0,
            used_seen: 0,
            cache,
            in_flight: 0,
        };
        Ok((queue, interrupts))
    }
}

/// One command: what the device reads, and how much it writes back.
pub struct Exchange<'a> {
    pub request: &'a [&'a [u8]],
    pub response: usize,
}

impl Queue {
    pub fn size(&self) -> u16 {
        self.size
    }

    /// Requests handed to the device and not returned yet.
    pub fn in_flight(&self) -> usize {
        self.in_flight
    }

    /// Ring slots free for new requests.
    pub fn free_slots(&self) -> usize {
        self.free.len()
    }

    fn frame(&mut self) -> Option<u64> {
        if let Some(frame) = self.cache.pop() {
            return Some(frame);
        }
        crate::frame::allocate_frame().map(|frame| frame.start_addr())
    }

    fn recycle(&mut self, frame: u64) {
        if self.cache.len() < CACHE_FRAMES {
            self.cache.push(frame);
        } else {
            unsafe { crate::frame::deallocate_frame(crate::frame::Frame(frame)) };
        }
    }

    /// Hands a request to the device, without notifying it. `None` if no
    /// slot is free, the request is too large, or no frame is left.
    fn enqueue(&mut self, request: &[&[u8]], response: usize, state: State) -> Option<Ticket> {
        let bytes: usize = request.iter().map(|part| part.len()).sum();
        let segments: usize = request.iter().map(|part| part.len().div_ceil(PAGE)).sum();
        if segments > MAX_SEGMENTS || bytes > MAX_REQUEST || response > PAGE || response == 0 {
            return None;
        }
        let index = *self.free.last()?;

        // --- payload, one frame per page of each part ---
        let mut frames: Vec<(u64, usize)> = Vec::with_capacity(segments);
        for part in request {
            for chunk in part.chunks(PAGE) {
                let Some(frame) = self.frame() else {
                    for (frame, _) in frames {
                        self.recycle(frame);
                    }
                    return None;
                };
                unsafe {
                    core::ptr::copy_nonoverlapping(chunk.as_ptr(), phys_to_virt(frame) as *mut u8, chunk.len())
                };
                frames.push((frame, chunk.len()));
            }
        }
        let Some(answer) = self.frame() else {
            for (frame, _) in frames {
                self.recycle(frame);
            }
            return None;
        };
        unsafe { core::ptr::write_bytes(phys_to_virt(answer) as *mut u8, 0, response) };

        // --- the indirect table: readable segments, then the answer ---
        let table = self.slots[index as usize].table;
        let base = phys_to_virt(table);
        for (rank, (frame, length)) in frames.iter().enumerate() {
            write_descriptor(base, rank as u16, *frame, *length as u32, DESC_NEXT, rank as u16 + 1);
        }
        write_descriptor(base, frames.len() as u16, answer, response as u32, DESC_WRITE, 0);
        let table_len = (frames.len() + 1) * 16;

        // --- the ring slot points at the table ---
        write_descriptor(phys_to_virt(self.desc), index, table, table_len as u32, DESC_INDIRECT, 0);

        self.free.pop();
        let slot = &mut self.slots[index as usize];
        slot.frames = frames.into_iter().map(|(frame, _)| frame).collect();
        slot.answer = answer;
        slot.answer_len = response;
        slot.state = state;
        slot.generation = slot.generation.wrapping_add(1);
        let ticket = Ticket { slot: index, generation: slot.generation };

        let avail = phys_to_virt(self.rings) + AVAIL_OFFSET;
        let position = self.avail_index as usize % self.size as usize;
        unsafe { ((avail + 4 + 2 * position as u64) as *mut u16).write_volatile(index) };
        self.avail_index = self.avail_index.wrapping_add(1);
        self.in_flight += 1;
        Some(ticket)
    }

    /// Publishes the requests enqueued so far and tells the device.
    fn kick(&mut self) {
        let avail = phys_to_virt(self.rings) + AVAIL_OFFSET;
        // The ring entries and the tables must be visible before the index
        // that publishes them, and the index before the notification.
        fence(Ordering::SeqCst);
        unsafe { ((avail + 2) as *mut u16).write_volatile(self.avail_index) };
        fence(Ordering::SeqCst);
        unsafe { (self.notify as *mut u16).write_volatile(0) };
    }

    /// Hands requests to the device and notifies it once. Returns one
    /// ticket per request, `None` for each that did not fit (the others
    /// are sent all the same).
    pub fn submit(&mut self, exchanges: &[Exchange]) -> Vec<Option<Ticket>> {
        self.reap();
        let tickets: Vec<Option<Ticket>> = exchanges
            .iter()
            .map(|exchange| self.enqueue(exchange.request, exchange.response, State::Pending))
            .collect();
        if tickets.iter().any(|ticket| ticket.is_some()) {
            self.kick();
        }
        tickets
    }

    /// Hands requests to the device whose answers nobody will read: their
    /// slots come back by themselves once the device returns them. `false`
    /// if one of them did not fit.
    pub fn post(&mut self, exchanges: &[Exchange]) -> bool {
        self.reap();
        let mut all = true;
        let mut any = false;
        for exchange in exchanges {
            match self.enqueue(exchange.request, exchange.response, State::Discarded) {
                Some(_) => any = true,
                None => all = false,
            }
        }
        if any {
            self.kick();
        }
        all
    }

    /// Reads the used ring: every request the device returned is marked
    /// done (or freed, if nobody wants its answer). Returns how many came
    /// back.
    pub fn reap(&mut self) -> usize {
        let used = phys_to_virt(self.rings) + USED_OFFSET;
        let index = unsafe { ((used + 2) as *const u16).read_volatile() };
        // The entries are read after the index that published them.
        fence(Ordering::Acquire);
        let mut returned = 0;
        while self.used_seen != index {
            let position = self.used_seen as usize % self.size as usize;
            let element = used + 4 + 8 * position as u64;
            let id = unsafe { (element as *const u32).read_volatile() };
            self.used_seen = self.used_seen.wrapping_add(1);
            returned += 1;
            let Some(state) = self.slots.get(id as usize).map(|slot| slot.state) else {
                continue;
            };
            match state {
                State::Pending => {
                    self.slots[id as usize].state = State::Done;
                    self.in_flight -= 1;
                }
                State::Discarded => {
                    self.in_flight -= 1;
                    self.release(id as u16);
                }
                // A device returning a slot it does not hold: ignored,
                // never trusted to free what is not in flight.
                State::Free | State::Done => {}
            }
        }
        returned
    }

    fn release(&mut self, index: u16) {
        let slot = &mut self.slots[index as usize];
        let frames = core::mem::take(&mut slot.frames);
        let answer = slot.answer;
        slot.state = State::Free;
        slot.answer = 0;
        for frame in frames {
            self.recycle(frame);
        }
        if answer != 0 {
            self.recycle(answer);
        }
        self.free.push(index);
    }

    /// Has the request of `ticket` come back?
    pub fn is_done(&self, ticket: Ticket) -> bool {
        self.slots
            .get(ticket.slot as usize)
            .is_some_and(|slot| slot.generation == ticket.generation && slot.state == State::Done)
    }

    /// Takes the answer of a returned request and frees its slot. `None`
    /// while it is in flight, or if the ticket is stale.
    pub fn take(&mut self, ticket: Ticket) -> Option<Vec<u8>> {
        if !self.is_done(ticket) {
            return None;
        }
        let slot = &self.slots[ticket.slot as usize];
        let mut bytes = alloc::vec![0u8; slot.answer_len];
        unsafe {
            core::ptr::copy_nonoverlapping(phys_to_virt(slot.answer) as *const u8, bytes.as_mut_ptr(), slot.answer_len)
        };
        self.release(ticket.slot);
        Some(bytes)
    }

    /// Gives up on the answer of a request: taken now if it came back,
    /// freed on its return otherwise.
    pub fn forget(&mut self, ticket: Ticket) {
        let Some(slot) = self.slots.get_mut(ticket.slot as usize) else {
            return;
        };
        if slot.generation != ticket.generation {
            return;
        }
        match slot.state {
            State::Done => self.release(ticket.slot),
            State::Pending => slot.state = State::Discarded,
            State::Free | State::Discarded => {}
        }
    }

    /// Spins, bounded, until `ticket` comes back; then takes its answer.
    pub fn wait(&mut self, ticket: Ticket) -> Option<Vec<u8>> {
        let mut spins = 0u32;
        loop {
            self.reap();
            if self.is_done(ticket) {
                return self.take(ticket);
            }
            spins += 1;
            if spins >= WAIT_SPINS {
                // Its slot stays in flight: the device may still write the
                // answer frame, which must therefore not be reused.
                self.forget(ticket);
                return None;
            }
            core::hint::spin_loop();
        }
    }

    /// Submits, waits, and returns the answers in order. `None` if one
    /// does not fit or the device does not answer in time.
    pub fn run(&mut self, exchanges: &[Exchange]) -> Option<Vec<Vec<u8>>> {
        let tickets = self.submit(exchanges);
        if tickets.iter().any(|ticket| ticket.is_none()) {
            for ticket in tickets.into_iter().flatten() {
                self.forget(ticket);
            }
            return None;
        }
        let mut answers = Vec::with_capacity(tickets.len());
        let mut lost = false;
        for ticket in tickets.into_iter().flatten() {
            if lost {
                self.forget(ticket);
                continue;
            }
            match self.wait(ticket) {
                Some(answer) => answers.push(answer),
                None => lost = true,
            }
        }
        (!lost).then_some(answers)
    }
}

fn write_descriptor(base: u64, index: u16, address: u64, length: u32, flags: u16, next: u16) {
    let entry = base + index as u64 * 16;
    unsafe {
        (entry as *mut u64).write_volatile(address);
        ((entry + 8) as *mut u32).write_volatile(length);
        ((entry + 12) as *mut u16).write_volatile(flags);
        ((entry + 14) as *mut u16).write_volatile(next);
    }
}

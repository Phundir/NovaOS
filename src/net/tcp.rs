//! TCP -- outgoing connections (RFC 9293, with RFC 7323 window scaling).
//!
//! ## What is implemented
//!
//! A client: active open, bidirectional transfer, orderly close.
//!
//! * **Reception** keeps up to `RECV_BUFFER` bytes and announces that room
//!   with window scaling, so a fast link is not held to 64 KiB per round
//!   trip. Segments that arrive ahead of a lost one are **kept** (the
//!   out-of-order queue) and delivered once the gap is filled; every
//!   segment is acknowledged at once, which gives the sender the duplicate
//!   acknowledgements its fast retransmission waits for.
//! * **Emission** is asynchronous. `write` copies into the connection's
//!   send buffer (`SEND_BUFFER`) and returns; segments leave within the
//!   other end's window and a congestion window (slow start, then
//!   additive increase, halved on loss), and every acknowledgement that
//!   arrives frees the buffer and lets the next segments go -- from the
//!   task that polled the card. A timer (`tick`, run by the network
//!   driver task every lap) resends from the first unacknowledged byte
//!   when the acknowledgements stop (go-back-N); three duplicate
//!   acknowledgements resend the missing segment at once.
//!
//!   The first version sent and then **waited for every byte to be
//!   acknowledged** before returning, with 64 KiB in flight at most: one
//!   round trip per write, and the browser's socket thread -- which serves
//!   every connection -- blocked meanwhile. A speed test uploaded at 18
//!   Mbit/s where the host's Linux did 278.
//!
//! What is refused rather than simulated: passive open, urgent data,
//! selective acknowledgements, timestamps.
//!
//! ## Waiting polls the card
//!
//! A task waiting for data or for an acknowledgement drains the card's
//! reception ring itself (`nic::poll`) and spins briefly before sleeping
//! a tick. The clock ticks every ten milliseconds, and a transfer that
//! only moved when the driver task woke was bounded to one ring every ten
//! milliseconds; now the frames come up as fast as the reader consumes
//! them. Only a task that waits spins, and only for `SPIN_LAPS` empty
//! polls: an idle connection sleeps as before.
//!
//! ## Sequence number arithmetic
//!
//! The numbers wrap on thirty-two bits. The comparison is the RFC one --
//! the difference, read as a signed thirty-two-bit integer -- in `after`.
//!
//! ## Point of execution
//!
//! `input` runs in whatever task polls the card; `connect`, `send` and
//! `recv` in the calling task. They meet on the connection table, under a
//! lock never held across a sleep or an emission.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use super::{checksum, ip, Ipv4};
use crate::vga::SpinLock;

/// Length of a header with no options.
pub const HEADER: usize = 20;

/// Segment size announced. 1460 = Ethernet MTU minus the IP and TCP
/// headers: it is the value that avoids all fragmentation.
const MSS: usize = 1460;

/// Window scale announced (RFC 7323): windows count in 64-byte units, up
/// to four megabytes.
const WINDOW_SCALE: u8 = 6;

/// Bytes kept on reception per connection, in order and out of order
/// together.
const RECV_BUFFER: usize = 4 * 1024 * 1024;

/// Heap reserved for reception by all connections together -- what the
/// buffers **occupy**, not what they hold. Every buffer lives in the
/// kernel heap (64 MiB), where an allocation that fails stops the
/// machine. The windows announced shrink as this fills.
///
/// Counting the bytes held was not enough: a queue grows by doubling and
/// kept its capacity once read. A speed test runs a dozen connections
/// flat out; each queue reached 4 to 8 MiB and stayed there, the table
/// "held" a few kilobytes, and the heap ran out ("ALLOCATION ECHOUEE :
/// 1440 octets", a segment kept out of order).
const TOTAL_BUFFER: usize = 12 * 1024 * 1024;

/// Heap an out-of-order segment costs beyond its bytes: its vector and
/// its share of a map node, roughly.
const AHEAD_OVERHEAD: usize = 64;

/// Capacity a drained queue may keep. Above it, the memory goes back to
/// the heap as soon as the program has read everything: an idle
/// connection must not hold what an active one needs.
const IDLE_CAPACITY: usize = 16 * 1024;

/// Bytes a connection keeps to send: written and not yet acknowledged.
/// Enough for a round trip of 30 ms at 280 Mbit/s.
const SEND_BUFFER: usize = 1024 * 1024;

/// Heap the send buffers of every connection together may occupy
/// (capacities, as for `TOTAL_BUFFER`).
const TOTAL_SEND: usize = 8 * 1024 * 1024;

/// Congestion window at the start (RFC 6928: ten segments).
const INITIAL_CWND: usize = 10 * MSS;

/// Segments one preparation hands out at most: the rest leave with the
/// next acknowledgement or the next tick.
const BURST: usize = 64;

/// How long a connection closed by its owner may take to deliver what it
/// still had to send, and its FIN, before its entry is taken back.
const LINGER_MS: u64 = 10_000;

/// In-order segments acknowledged by one ACK, at most. See `flush_acks`.
const ACK_EVERY: u8 = 2;

/// Number of simultaneous connections, for the whole system.
///
/// Sixteen was the first value, and a browser alone exceeded it: Firefox
/// keeps up to six idle connections per server for two minutes, and four
/// sites visited in one tab held fourteen of the sixteen. Every new
/// connection of every program was then refused until one of them timed
/// out -- the network seemed to cut out for the whole system. The buffers
/// are bounded by `TOTAL_BUFFER`, not by this count: an idle connection
/// costs its entry and nothing else.
const MAX_CONNECTIONS: usize = 256;

/// Retransmissions before giving up, and wait between two attempts.
const RETRIES: u32 = 6;
const RETRY_MS: u64 = 400;

/// Empty polls of the card before a waiting task sleeps a tick.
const SPIN_LAPS: u32 = 2000;
/// `pause` instructions between two empty polls of an active transfer.
const IDLE_PAUSES: u32 = 64;

const FIN: u8 = 1 << 0;
const SYN: u8 = 1 << 1;
const RST: u8 = 1 << 2;
const PSH: u8 = 1 << 3;
const ACK: u8 = 1 << 4;

const EPHEMERAL_FIRST: u16 = 49152;
const EPHEMERAL_LAST: u16 = 65535;
static NEXT_EPHEMERAL: AtomicU64 = AtomicU64::new(EPHEMERAL_FIRST as u64 + 1024);

/// State of a connection, as the RFC names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Closed,
    SynSent,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    LastAck,
    TimeWait,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Closed => "CLOSED",
            State::SynSent => "SYN_SENT",
            State::Established => "ESTABLISHED",
            State::FinWait1 => "FIN_WAIT_1",
            State::FinWait2 => "FIN_WAIT_2",
            State::CloseWait => "CLOSE_WAIT",
            State::LastAck => "LAST_ACK",
            State::TimeWait => "TIME_WAIT",
        }
    }
}

struct Connection {
    used: bool,
    /// Incremented each time the entry is given to a new connection: a
    /// `Handle` names one generation, never the entry that succeeds it.
    generation: u32,
    state: State,
    local_port: u16,
    remote: Ipv4,
    remote_port: u16,
    /// First byte sent not yet acknowledged.
    snd_una: u32,
    /// Next sequence number to send.
    snd_nxt: u32,
    /// Next byte expected from the other end.
    rcv_nxt: u32,
    /// Window announced by the other end, in bytes (scale applied).
    snd_wnd: u32,
    /// Shift of the other end's window field, once agreed.
    snd_scale: u8,
    /// Shift of ours, once the other end agreed to scaling.
    rcv_scale: u8,
    incoming: VecDeque<u8>,
    /// Segments ahead of `rcv_nxt`, by sequence number.
    ahead: BTreeMap<u32, Vec<u8>>,
    ahead_bytes: usize,
    /// Has the other end closed its sending direction?
    peer_finished: bool,
    /// Has the connection been abruptly closed?
    reset: bool,
    segments_in: u64,
    segments_out: u64,
    retransmits: u64,
    out_of_order: u64,
    /// In-order segments received and not yet acknowledged.
    ack_pending: u8,
    /// Bytes from `snd_una` on: sent and unacknowledged, then unsent.
    outgoing: VecDeque<u8>,
    /// Congestion window and slow-start threshold, in bytes.
    cwnd: usize,
    ssthresh: usize,
    /// Acknowledgements in a row that did not move `snd_una`.
    dup_acks: u8,
    /// Tick of the last progress of `snd_una`, or of the first segment
    /// sent into an empty flight: what the retransmission timer counts
    /// from.
    last_progress: u64,
    /// Retransmissions in a row without progress, and the current timeout.
    retries: u32,
    rto_ms: u64,
    /// The owner closed it: nobody reads or writes any more; it delivers
    /// its send buffer and its FIN, then frees its entry (`LINGER_MS`).
    orphan: bool,
    linger_until: u64,
    /// The FIN is to be sent once the send buffer is empty; its sequence
    /// number once sent.
    closing: bool,
    fin_seq: Option<u32>,
}

impl Connection {
    const fn new() -> Self {
        Connection {
            used: false,
            generation: 0,
            state: State::Closed,
            local_port: 0,
            remote: Ipv4::UNSPECIFIED,
            remote_port: 0,
            snd_una: 0,
            snd_nxt: 0,
            rcv_nxt: 0,
            snd_wnd: 0,
            snd_scale: 0,
            rcv_scale: 0,
            incoming: VecDeque::new(),
            ahead: BTreeMap::new(),
            ahead_bytes: 0,
            peer_finished: false,
            reset: false,
            segments_in: 0,
            segments_out: 0,
            retransmits: 0,
            out_of_order: 0,
            ack_pending: 0,
            outgoing: VecDeque::new(),
            cwnd: INITIAL_CWND,
            ssthresh: usize::MAX,
            dup_acks: 0,
            last_progress: 0,
            retries: 0,
            rto_ms: RETRY_MS,
            orphan: false,
            linger_until: 0,
            closing: false,
            fin_seq: None,
        }
    }

    fn held(&self) -> usize {
        self.incoming.len() + self.ahead_bytes
    }

    /// Heap this connection's reception occupies (see `TOTAL_BUFFER`).
    fn memory(&self) -> usize {
        self.incoming.capacity() + self.ahead_bytes + self.ahead.len() * AHEAD_OVERHEAD
    }

    /// Gives back what the read queue no longer needs: all of it once
    /// drained (beyond `IDLE_CAPACITY`), or down to twice its content when
    /// it is three quarters empty. The smaller queue is allocated before
    /// the larger one goes, and only if the heap has room: a shrink must
    /// never be the allocation that stops the kernel. Returns the bytes
    /// released.
    fn release_unused(&mut self) -> usize {
        let before = self.incoming.capacity();
        if self.incoming.is_empty() {
            if before > IDLE_CAPACITY {
                self.incoming = VecDeque::new();
            }
        } else if before > 4 * IDLE_CAPACITY && self.incoming.len() <= before / 4 {
            let mut smaller = VecDeque::new();
            if smaller.try_reserve_exact(self.incoming.len() * 2).is_ok() {
                smaller.extend(self.incoming.drain(..));
                self.incoming = smaller;
            }
        }
        before.saturating_sub(self.incoming.capacity())
    }

    /// Bytes in flight: sent, not yet acknowledged (the FIN counts one).
    fn in_flight(&self) -> usize {
        self.snd_nxt.wrapping_sub(self.snd_una) as usize
    }

    /// Bytes of the send buffer not yet sent.
    fn unsent(&self) -> usize {
        self.outgoing.len().saturating_sub(self.in_flight())
    }

    /// Room left in the send buffer.
    fn send_room(&self) -> usize {
        SEND_BUFFER.saturating_sub(self.outgoing.len())
    }

    /// Can the program still write?
    fn writable(&self) -> bool {
        !self.reset && !self.closing && matches!(self.state, State::Established | State::CloseWait)
    }

    /// Hands out the segments the windows allow now: data from the send
    /// buffer, then the FIN once it is empty and asked for. Each goes
    /// into `out` as (sequence, flags, payload); `snd_nxt` moves past it.
    /// A copy the heap refuses stops the preparation: the rest leaves
    /// later.
    fn prepare(&mut self, now: u64, free: usize, out: &mut Vec<Outgoing>) {
        if !matches!(self.state, State::Established | State::CloseWait | State::FinWait1 | State::LastAck) || self.reset {
            return;
        }
        let window = if self.snd_wnd == 0 && self.in_flight() == 0 {
            // Zero window: one byte now and then, so that the reopening
            // is learnt even if its announcement is lost (a probe).
            if now >= self.last_progress + ticks_for(self.rto_ms) { 1 } else { 0 }
        } else {
            self.snd_wnd as usize
        };
        let limit = window.min(self.cwnd);
        while out.len() < BURST && self.unsent() > 0 && self.in_flight() < limit {
            let offset = self.in_flight();
            let length = MSS.min(self.unsent()).min(limit - self.in_flight());
            let mut payload = Vec::new();
            if payload.try_reserve_exact(length).is_err() {
                return;
            }
            // Copied by slices of the ring, as `recv` does.
            let (front, back) = self.outgoing.as_slices();
            let end = offset + length;
            if offset < front.len() {
                payload.extend_from_slice(&front[offset..end.min(front.len())]);
            }
            if end > front.len() {
                payload.extend_from_slice(&back[offset.max(front.len()) - front.len()..end - front.len()]);
            }
            if self.in_flight() == 0 {
                self.last_progress = now;
            }
            let flags = if self.unsent() == length { PSH | ACK } else { ACK };
            out.push(Outgoing { sequence: self.snd_nxt, acknowledgement: self.rcv_nxt, flags, payload, window: self.window_field(free) });
            self.snd_nxt = self.snd_nxt.wrapping_add(length as u32);
            self.segments_out += 1;
        }
        if self.closing && self.fin_seq.is_none() && self.unsent() == 0 && out.len() < BURST {
            if self.in_flight() == 0 {
                self.last_progress = now;
            }
            out.push(Outgoing { sequence: self.snd_nxt, acknowledgement: self.rcv_nxt, flags: FIN | ACK, payload: Vec::new(), window: self.window_field(free) });
            self.fin_seq = Some(self.snd_nxt);
            self.snd_nxt = self.snd_nxt.wrapping_add(1);
            self.state = if self.state == State::CloseWait { State::LastAck } else { State::FinWait1 };
            self.segments_out += 1;
        }
    }

    /// Everything unacknowledged goes again, from `snd_una` (go-back-N):
    /// the timer expired, or three duplicate acknowledgements said a
    /// segment is missing.
    fn rewind(&mut self) {
        self.snd_nxt = self.snd_una;
        if let Some(fin) = self.fin_seq
            && fin == self.snd_una
        {
            // The FIN itself was not acknowledged: it goes again too.
            self.fin_seq = None;
        }
        self.retransmits += 1;
    }

    /// An acknowledgement that moves `snd_una` to `ack`.
    fn acknowledged(&mut self, ack: u32, now: u64) {
        let acked = ack.wrapping_sub(self.snd_una) as usize;
        let data = acked.min(self.outgoing.len());
        self.outgoing.drain(..data);
        if self.outgoing.is_empty() && self.outgoing.capacity() > IDLE_CAPACITY {
            self.outgoing = VecDeque::new();
        }
        self.snd_una = ack;
        self.dup_acks = 0;
        self.retries = 0;
        self.rto_ms = RETRY_MS;
        self.last_progress = now;
        // Slow start, then one segment per window (RFC 5681).
        if self.cwnd < self.ssthresh {
            self.cwnd += acked.min(MSS);
        } else {
            self.cwnd += (MSS * MSS / self.cwnd).max(1);
        }
        self.cwnd = self.cwnd.min(SEND_BUFFER * 2);
        if let Some(fin) = self.fin_seq
            && !after(fin.wrapping_add(1), ack)
        {
            // Our FIN is acknowledged.
            self.state = match self.state {
                State::FinWait1 if self.peer_finished => State::TimeWait,
                State::FinWait1 => State::FinWait2,
                State::LastAck => State::Closed,
                other => other,
            };
        }
    }

    /// A lost segment: the congestion window halves (RFC 5681).
    fn congestion(&mut self, timeout: bool) {
        self.ssthresh = (self.in_flight() / 2).max(2 * MSS);
        self.cwnd = if timeout { MSS } else { self.ssthresh };
    }

    /// The window to announce, before scaling: the room left here, and in
    /// the whole table (`free`, from `table_free`) plus what this queue
    /// has already reserved and not filled -- bytes that land there cost
    /// the heap nothing more.
    fn window_bytes(&self, free: usize) -> usize {
        let spare = self.incoming.capacity() - self.incoming.len();
        RECV_BUFFER.saturating_sub(self.held()).min(free + spare)
    }

    /// The 16-bit field that announces it.
    fn window_field(&self, free: usize) -> u16 {
        (self.window_bytes(free) >> self.rcv_scale).min(u16::MAX as usize) as u16
    }

    /// Moves the out-of-order segments that now follow `rcv_nxt` into the
    /// in-order buffer. `free` is the table's room (`table_free`).
    fn absorb(&mut self, free: usize) {
        loop {
            // Looked for by modular comparison, not by the map's order:
            // numbers wrap, and near the wrap the smallest key is not the
            // first in sequence.
            let Some(sequence) = self.ahead.keys().copied().find(|key| !after(*key, self.rcv_nxt)) else {
                return;
            };
            let data = self.ahead.remove(&sequence).unwrap_or_default();
            self.ahead_bytes -= data.len();
            // Overlap with what is already in order is skipped.
            let skip = self.rcv_nxt.wrapping_sub(sequence) as usize;
            if skip < data.len() {
                // No room in the heap (see `accept_bytes`): the segment
                // goes back where it was, for a later poll. Its own bytes
                // leave the budget as they move, hence the allowance.
                if accept_bytes(&mut self.incoming, data.len() - skip, free + data.len()) == 0 {
                    self.ahead_bytes += data.len();
                    self.ahead.insert(sequence, data);
                    return;
                }
                self.incoming.extend(&data[skip..]);
                self.rcv_nxt = self.rcv_nxt.wrapping_add((data.len() - skip) as u32);
            }
        }
    }
}

/// How many of `count` received bytes the queue can take: all of them,
/// or none when the kernel heap cannot grow it.
///
/// The queue lives in the kernel heap, shared by the whole system. It
/// grows by doubling, but never past `RECV_BUFFER` nor past what the
/// table has left (`free`, from `table_free`): a doubling is itself an
/// allocation, and an unbounded one filled the heap. The reservation is
/// fallible: during a large download a doubling that found no room used
/// to stop the kernel (`alloc_error`). Refused, the segment is simply
/// not acknowledged, and the sender sends it again -- a loss TCP was
/// made for.
fn accept_bytes(queue: &mut VecDeque<u8>, count: usize, free: usize) -> usize {
    let needed = queue.len() + count;
    if needed <= queue.capacity() {
        return count;
    }
    let target = (queue.capacity() * 2)
        .min(RECV_BUFFER)
        .min(queue.capacity() + free)
        .max(needed);
    if queue.try_reserve_exact(target - queue.len()).is_ok() { count } else { 0 }
}

static CONNECTIONS: SpinLock<[Connection; MAX_CONNECTIONS]> =
    SpinLock::new([const { Connection::new() }; MAX_CONNECTIONS]);

/// A segment prepared under the table's lock, emitted once it is released.
struct Outgoing {
    sequence: u32,
    acknowledgement: u32,
    flags: u8,
    payload: Vec<u8>,
    window: u16,
}

/// Heap the send buffers occupy over the whole table.
fn send_memory(connections: &[Connection; MAX_CONNECTIONS]) -> usize {
    connections.iter().filter(|c| c.used).map(|c| c.outgoing.capacity()).sum()
}

/// Emits prepared segments for one connection, lock released. `cached`:
/// from the handling of a received packet, which must not wait for an
/// address resolution. Returns the sequence number of the first segment
/// that could not leave (a full transmit ring), for `rewind_to`.
fn transmit(remote: Ipv4, local_port: u16, remote_port: u16, segments: &[Outgoing], cached: bool) -> Option<u32> {
    let source = super::interface().ip;
    for segment in segments {
        let bytes = build(
            source,
            remote,
            local_port,
            remote_port,
            segment.sequence,
            segment.acknowledgement,
            segment.flags,
            &segment.payload,
            false,
            segment.window,
        );
        let sent = if cached {
            ip::send_cached(ip::PROTOCOL_TCP, remote, &bytes)
        } else {
            ip::send(ip::PROTOCOL_TCP, remote, &bytes)
        };
        if sent.is_err() {
            return Some(segment.sequence);
        }
    }
    None
}

/// A segment that could not leave: `snd_nxt` goes back to it, and the
/// next acknowledgement or tick sends it again.
fn rewind_to(handle_slot: usize, generation: u32, sequence: u32) {
    let mut connections = CONNECTIONS.lock();
    let connection = &mut connections[handle_slot];
    if connection.used && connection.generation == generation && after(connection.snd_nxt, sequence) && !after(connection.snd_una, sequence) {
        connection.snd_nxt = sequence;
        if connection.fin_seq.is_some_and(|fin| !after(sequence, fin)) {
            connection.fin_seq = None;
        }
    }
}

/// Room left under `TOTAL_BUFFER`, over the whole table.
fn table_free(connections: &[Connection; MAX_CONNECTIONS]) -> usize {
    let occupied: usize = connections.iter().filter(|c| c.used).map(|c| c.memory()).sum();
    TOTAL_BUFFER.saturating_sub(occupied)
}

/// Reference to a connection: an entry of the table and the generation it
/// was opened under.
///
/// The entry alone is not enough. A connection the other end resets
/// frees its entry at once (`input`), while the program still holds the
/// reference; a later `connect` takes the entry again, and the old
/// reference then named somebody else's connection -- its `close` closed
/// it. A reference whose generation is no longer the entry's reads as a
/// closed connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handle {
    slot: usize,
    generation: u32,
}

/// Bits of `Handle::index` that carry the entry; the generation sits above.
const SLOT_BITS: u32 = 16;

impl Handle {
    /// Identifier handed to ring 3: the generation and the entry.
    pub fn index(&self) -> usize {
        ((self.generation as usize) << SLOT_BITS) | self.slot
    }

    /// Rebuilds a reference from an identifier handed to ring 3. Validity
    /// is checked at every use: a forged identifier names a closed
    /// connection, never that of somebody else.
    pub fn from_index(index: usize) -> Option<Handle> {
        let slot = index & ((1 << SLOT_BITS) - 1);
        let generation = u32::try_from(index >> SLOT_BITS).ok()?;
        (slot < MAX_CONNECTIONS).then_some(Handle { slot, generation })
    }
}

/// The connection a reference names, if it is still the one it was.
fn named(connections: &mut [Connection; MAX_CONNECTIONS], handle: Handle) -> Option<&mut Connection> {
    let connection = &mut connections[handle.slot];
    (connection.used && !connection.orphan && connection.generation == handle.generation).then_some(connection)
}

/// Is `a` strictly after `b` in the space of sequence numbers? Signed
/// thirty-two-bit comparison (RFC 9293, 3.4).
fn after(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}

fn allocate_port(taken: &[Connection; MAX_CONNECTIONS]) -> u16 {
    let span = (EPHEMERAL_LAST - EPHEMERAL_FIRST + 1) as u64;
    for _ in 0..MAX_CONNECTIONS * 4 {
        let value = NEXT_EPHEMERAL.fetch_add(1, Ordering::Relaxed);
        let port = EPHEMERAL_FIRST + (value % span) as u16;
        if !taken.iter().any(|c| c.used && c.local_port == port) {
            return port;
        }
    }
    EPHEMERAL_FIRST
}

/// Initial sequence number: unpredictable (RFC 6528), from `random`.
fn initial_sequence() -> u32 {
    crate::random::u64() as u32
}

/// One step of a wait: poll the card, spin a little, then sleep a tick.
///
/// ## Why the spin is bounded by activity and not only by laps
///
/// Sleeping costs a whole tick, ten milliseconds, and during it only the
/// driver task empties the reception ring -- once per tick. A 256-frame
/// ring then carries 370 KiB per tick, 37 MB/s, whatever the card could
/// do. The first version slept after `SPIN_LAPS` empty laps; under KVM
/// those laps last a millisecond or two, and a host that paused slightly
/// longer between two bursts sent the reader to sleep in the middle of a
/// transfer. Measured: 233 MB/s on one run, 26 on the next. So while
/// frames arrived in the last two ticks the waiter keeps polling -- an
/// idle connection still sleeps after two ticks.
fn wait_step(spins: &mut u32) {
    if crate::drivers::nic::poll() > 0 {
        *spins = 0;
        return;
    }
    if crate::drivers::nic::recently_active() {
        // Mid-transfer: the core is given up only when the slice is spent.
        // `yield_now` on every lap handed it to any ready task of the same
        // core for that task's whole slice, and the ring filled meanwhile.
        crate::scheduler::preempt_if_needed();
        // A short pause between two empty polls: a few microseconds,
        // against the 370 KiB the ring holds -- two milliseconds at
        // 200 MB/s. Polling back to back kept the card's and the table's
        // locks, which mask interrupts, busy for the other cores.
        for _ in 0..IDLE_PAUSES {
            core::hint::spin_loop();
        }
        return;
    }
    if *spins < SPIN_LAPS {
        *spins += 1;
        crate::scheduler::yield_now();
        core::hint::spin_loop();
        return;
    }
    *spins = 0;
    let per_tick = 1000 / crate::TIMER_HZ as u64;
    let _ = crate::syscall::invoke(crate::syscall::domain_time::SYS_TIME_SLEEP_MS, [per_tick, 0, 0, 0, 0, 0]);
}

/// Builds a complete segment, checksum included.
#[allow(clippy::too_many_arguments)]
fn build(
    source: Ipv4,
    destination: Ipv4,
    local_port: u16,
    remote_port: u16,
    sequence: u32,
    acknowledgement: u32,
    flags: u8,
    payload: &[u8],
    syn_options: bool,
    window: u16,
) -> Vec<u8> {
    // MSS (4 bytes), then NOP and window scale (4 bytes).
    let options: usize = if syn_options { 8 } else { 0 };
    let header = HEADER + options;
    let mut segment = Vec::with_capacity(header + payload.len());

    segment.extend_from_slice(&local_port.to_be_bytes());
    segment.extend_from_slice(&remote_port.to_be_bytes());
    segment.extend_from_slice(&sequence.to_be_bytes());
    segment.extend_from_slice(&acknowledgement.to_be_bytes());
    segment.push(((header / 4) as u8) << 4);
    segment.push(flags);
    segment.extend_from_slice(&window.to_be_bytes());
    segment.extend_from_slice(&[0, 0]);
    segment.extend_from_slice(&[0, 0]);

    if syn_options {
        segment.extend_from_slice(&[2, 4]);
        segment.extend_from_slice(&(MSS as u16).to_be_bytes());
        segment.extend_from_slice(&[1, 3, 3, WINDOW_SCALE]);
    }
    segment.extend_from_slice(payload);

    let pseudo = ip::pseudo_header(source, destination, ip::PROTOCOL_TCP, segment.len());
    let sum = checksum(&[&pseudo, &segment]);
    segment[16..18].copy_from_slice(&sum.to_be_bytes());
    segment
}

/// The window field for a connection, looked up by its addresses.
fn window_for(remote: Ipv4, local_port: u16, remote_port: u16) -> u16 {
    let connections = CONNECTIONS.lock();
    let free = table_free(&connections);
    connections
        .iter()
        .find(|c| c.used && c.local_port == local_port && c.remote_port == remote_port && c.remote == remote)
        .map(|c| c.window_field(free))
        .unwrap_or(u16::MAX)
}

/// Sends a segment from the handling of a received packet: never waits
/// for an ARP resolution (the caller may be the only one able to receive
/// the answer). A lost acknowledgement is resent by the other end.
fn emit_cached(remote: Ipv4, local_port: u16, remote_port: u16, sequence: u32, acknowledgement: u32, flags: u8, window: u16) {
    let source = super::interface().ip;
    let segment = build(source, remote, local_port, remote_port, sequence, acknowledgement, flags, &[], false, window);
    let _ = ip::send_cached(ip::PROTOCOL_TCP, remote, &segment);
}

/// Sends a segment for a connection, with the lock released.
#[allow(clippy::too_many_arguments)]
fn emit(
    remote: Ipv4,
    local_port: u16,
    remote_port: u16,
    sequence: u32,
    acknowledgement: u32,
    flags: u8,
    payload: &[u8],
    syn_options: bool,
) -> Result<(), &'static str> {
    let source = super::interface().ip;
    let window = if syn_options { u16::MAX } else { window_for(remote, local_port, remote_port) };
    let segment = build(
        source,
        remote,
        local_port,
        remote_port,
        sequence,
        acknowledgement,
        flags,
        payload,
        syn_options,
        window,
    );
    ip::send(ip::PROTOCOL_TCP, remote, &segment)
}

/// Public state of a connection.
pub fn state(handle: Handle) -> State {
    named(&mut CONNECTIONS.lock(), handle).map_or(State::Closed, |connection| connection.state)
}

fn ticks_for(milliseconds: u64) -> u64 {
    let per_tick = 1000 / crate::TIMER_HZ as u64;
    milliseconds.div_ceil(per_tick).max(1)
}

/// Opens a connection to an address and a port.
pub fn connect(remote: Ipv4, remote_port: u16, timeout_ms: u64) -> Result<Handle, &'static str> {
    if !super::interface().configured() {
        return Err("interface sans adresse IP");
    }
    if remote.is_unspecified() || remote_port == 0 {
        return Err("destination invalide");
    }

    let taken = {
        let mut connections = CONNECTIONS.lock();
        let local_port = allocate_port(&connections);
        let iss = initial_sequence();

        match connections.iter().position(|c| !c.used) {
            None => None,
            Some(index) => {
                let connection = &mut connections[index];
                // Zero is never a live generation: an identifier of zero, or one
                // forged from nothing, names no connection.
                let generation = connection.generation.wrapping_add(1).max(1);
                *connection = Connection::new();
                connection.used = true;
                connection.generation = generation;
                connection.state = State::SynSent;
                connection.local_port = local_port;
                connection.remote = remote;
                connection.remote_port = remote_port;
                connection.snd_una = iss;
                connection.snd_nxt = iss.wrapping_add(1);

                Some((Handle { slot: index, generation }, local_port, iss))
            }
        }
    };
    // A full table looks, from a program, like a network that cut out:
    // said on the serial console, once the table's lock is released.
    let Some((handle, local_port, iss)) = taken else {
        crate::serial_println!("[tcp] table pleine : {} connexions, connect refuse", MAX_CONNECTIONS);
        return Err("plus de connexion disponible");
    };

    let deadline = crate::idt::ticks() + ticks_for(timeout_ms);
    let mut next_attempt = 0u64;
    let mut attempts = 0;
    let mut spins = 0;

    loop {
        match state(handle) {
            State::Established => return Ok(handle),
            State::Closed => {
                close(handle);
                return Err("connexion refusee");
            }
            _ => {}
        }

        if crate::idt::ticks() >= next_attempt {
            if attempts >= RETRIES {
                close(handle);
                return Err("aucune reponse a la demande de connexion");
            }
            attempts += 1;
            let _ = emit(remote, local_port, remote_port, iss, 0, SYN, &[], true);
            count_out(handle, attempts > 1);
            next_attempt = crate::idt::ticks() + ticks_for(RETRY_MS);
        }

        if crate::idt::ticks() >= deadline {
            close(handle);
            return Err("delai d'ouverture depasse");
        }
        wait_step(&mut spins);
    }
}

fn count_out(handle: Handle, retransmit: bool) {
    if let Some(connection) = named(&mut CONNECTIONS.lock(), handle) {
        connection.segments_out += 1;
        if retransmit {
            connection.retransmits += 1;
        }
    }
}

/// Copies what fits of `data` into the send buffer and sends what the
/// windows allow; returns at once. `Ok(0)`: the buffer is full, the
/// caller waits for acknowledgements (`send_room`). `Err`: the connection
/// cannot send any more (reset, closed, never established).
pub fn write(handle: Handle, data: &[u8]) -> Result<usize, &'static str> {
    let now = crate::idt::ticks();
    let mut segments = Vec::new();
    let (accepted, remote, local_port, remote_port) = {
        let mut connections = CONNECTIONS.lock();
        let free = table_free(&connections);
        let send_free = TOTAL_SEND.saturating_sub(send_memory(&connections));
        let Some(connection) = named(&mut connections, handle) else {
            return Err("connexion fermee");
        };
        if connection.reset {
            return Err("connexion reinitialisee");
        }
        if !connection.writable() {
            return Err("connexion non etablie");
        }
        let count = data.len().min(connection.send_room());
        // Grown like the reception queue: doubling, bounded by the
        // buffer and by what the table has left, and fallibly.
        let needed = connection.outgoing.len() + count;
        let accepted = if needed <= connection.outgoing.capacity() {
            count
        } else {
            let target = (connection.outgoing.capacity() * 2)
                .max(64 * 1024)
                .min(SEND_BUFFER)
                .min(connection.outgoing.capacity() + send_free)
                .max(needed.min(connection.outgoing.capacity() + send_free));
            let room = target.saturating_sub(connection.outgoing.len()).min(count);
            if room == 0 || connection.outgoing.try_reserve_exact(target - connection.outgoing.len()).is_err() {
                0
            } else {
                room
            }
        };
        connection.outgoing.extend(&data[..accepted]);
        connection.prepare(now, free, &mut segments);
        (accepted, connection.remote, connection.local_port, connection.remote_port)
    };
    if let Some(sequence) = transmit(remote, local_port, remote_port, &segments, false) {
        rewind_to(handle.slot, handle.generation, sequence);
    }
    Ok(accepted)
}

/// Room left in the send buffer: what `write` would take now. Zero for a
/// connection that cannot send.
pub fn send_room(handle: Handle) -> usize {
    named(&mut CONNECTIONS.lock(), handle).map_or(0, |connection| {
        if connection.writable() { connection.send_room() } else { 0 }
    })
}

/// Sends data and waits until the other end acknowledged all of it -- the
/// kernel's own clients (packages, settings, the native browser) want to
/// know it arrived. Programs go through `write`, which does not wait.
pub fn send(handle: Handle, data: &[u8]) -> Result<usize, &'static str> {
    let mut spins = 0;
    let mut done = 0;
    while done < data.len() {
        let accepted = write(handle, &data[done..])?;
        done += accepted;
        if accepted == 0 {
            wait_step(&mut spins);
        }
    }
    loop {
        {
            let mut connections = CONNECTIONS.lock();
            let Some(connection) = named(&mut connections, handle) else {
                return Err("connexion fermee");
            };
            if connection.reset {
                return Err("donnees non acquittees");
            }
            if connection.outgoing.is_empty() {
                return Ok(data.len());
            }
        }
        wait_step(&mut spins);
    }
}

/// Reads what has arrived, at most `max` bytes, waiting if needed. Empty
/// at the end of the stream.
pub fn recv(handle: Handle, max: usize, timeout_ms: u64) -> Result<Vec<u8>, &'static str> {
    let deadline = crate::idt::ticks() + ticks_for(timeout_ms);
    let mut spins = 0;

    loop {
        {
            let mut connections = CONNECTIONS.lock();
            let free = table_free(&connections);
            let Some(connection) = named(&mut connections, handle) else {
                return Err("connexion fermee");
            };
            if connection.reset {
                return Err("connexion reinitialisee");
            }
            if !connection.incoming.is_empty() {
                let before = connection.window_bytes(free);
                let count = max.min(connection.incoming.len());
                // Copied by slices: collecting the drain byte by byte was
                // a loop of a quarter of a million turns per call.
                let mut data = Vec::with_capacity(count);
                let (front, back) = connection.incoming.as_slices();
                let first = front.len().min(count);
                data.extend_from_slice(&front[..first]);
                data.extend_from_slice(&back[..count - first]);
                connection.incoming.drain(..count);
                let released = connection.release_unused();
                // A window that was nearly closed opens again: say so, or
                // the other end waits for its retransmission timer.
                let reopened = before < 16 * MSS && connection.window_bytes(free + released) >= 16 * MSS;
                let details = (
                    connection.remote,
                    connection.local_port,
                    connection.remote_port,
                    connection.snd_nxt,
                    connection.rcv_nxt,
                );
                drop(connections);
                if reopened {
                    let (remote, local_port, remote_port, sequence, acknowledgement) = details;
                    let _ = emit(remote, local_port, remote_port, sequence, acknowledgement, ACK, &[], false);
                }
                return Ok(data);
            }
            // Nothing left to read and the other end has closed: an end of
            // stream, not an error.
            if connection.peer_finished {
                return Ok(Vec::new());
            }
        }

        if crate::idt::ticks() >= deadline {
            return Err("delai de lecture depasse");
        }
        wait_step(&mut spins);
    }
}

/// Readiness of a connection, without waiting: (bytes readable, the
/// other end has finished, the connection was reset, state). For the
/// Linux sockets of phase 11.5, which poll instead of blocking.
pub fn readiness(handle: Handle) -> (usize, bool, bool, State) {
    let mut connections = CONNECTIONS.lock();
    let Some(connection) = named(&mut connections, handle) else {
        return (0, true, false, State::Closed);
    };
    (connection.incoming.len(), connection.peer_finished, connection.reset, connection.state)
}

/// Reads what has already arrived, without waiting. Empty if nothing is
/// there -- the caller distinguishes the end of stream with `readiness`.
pub fn take_available(handle: Handle, max: usize) -> Result<Vec<u8>, &'static str> {
    if readiness(handle).0 == 0 {
        return Ok(Vec::new());
    }
    recv(handle, max, 0)
}

/// Closes a connection. What is still in the send buffer is delivered,
/// then the FIN, as the program expects of `close`: the entry lingers,
/// nobody's (`orphan`), until the FIN is acknowledged or `LINGER_MS`
/// passes. Unread received data is dropped. A connection still opening
/// is reset.
pub fn close(handle: Handle) {
    let now = crate::idt::ticks();
    let mut segments = Vec::new();
    let reset_details = {
        let mut connections = CONNECTIONS.lock();
        let free = table_free(&connections);
        let Some(connection) = named(&mut connections, handle) else {
            return;
        };
        match connection.state {
            State::Established | State::CloseWait if !connection.reset => {
                connection.orphan = true;
                connection.closing = true;
                connection.linger_until = now + ticks_for(LINGER_MS);
                connection.incoming = VecDeque::new();
                connection.ahead.clear();
                connection.ahead_bytes = 0;
                connection.prepare(now, free, &mut segments);
                None
            }
            state => {
                let details = (connection.remote, connection.local_port, connection.remote_port, connection.snd_nxt, connection.rcv_nxt);
                let generation = connection.generation;
                *connection = Connection::new();
                connection.generation = generation;
                (state == State::SynSent).then_some(details)
            }
        }
    };
    // Segments leave with the lock released: `ip::send` may resolve a
    // hardware address, hence sleep.
    if let Some((remote, local_port, remote_port, sequence, acknowledgement)) = reset_details {
        let _ = emit(remote, local_port, remote_port, sequence, acknowledgement, RST, &[], false);
    }
    if !segments.is_empty() {
        let (remote, local_port, remote_port) = {
            let connections = CONNECTIONS.lock();
            let c = &connections[handle.slot];
            (c.remote, c.local_port, c.remote_port)
        };
        if let Some(sequence) = transmit(remote, local_port, remote_port, &segments, false) {
            rewind_to(handle.slot, handle.generation, sequence);
        }
    }
}

/// The retransmission timer and the lingering of closed connections. Run
/// by the network driver task at every lap (at least every tick).
pub fn tick() {
    let now = crate::idt::ticks();
    for slot in 0..MAX_CONNECTIONS {
        let mut segments = Vec::new();
        let target = {
            let mut connections = CONNECTIONS.lock();
            let free = table_free(&connections);
            let connection = &mut connections[slot];
            if !connection.used {
                continue;
            }
            let finished = connection.state == State::Closed
                || connection.state == State::TimeWait
                || connection.state == State::FinWait2
                || connection.reset;
            if connection.orphan && (finished || now >= connection.linger_until) {
                let generation = connection.generation;
                *connection = Connection::new();
                connection.generation = generation;
                continue;
            }
            // Only a connection that carries data: the SYN of one being
            // opened is `connect`'s to resend, and rewinding it would make
            // the SYN-ACK acknowledge a number no longer sent.
            let carrying = matches!(connection.state, State::Established | State::CloseWait | State::FinWait1 | State::LastAck);
            if carrying && connection.in_flight() > 0 && now >= connection.last_progress + ticks_for(connection.rto_ms) {
                connection.retries += 1;
                if connection.retries > RETRIES {
                    // The other end is gone: the program learns it at its
                    // next read or write.
                    connection.reset = true;
                    connection.outgoing = VecDeque::new();
                    continue;
                }
                connection.congestion(true);
                connection.rewind();
                connection.rto_ms = (connection.rto_ms * 2).min(RETRY_MS << 4);
                connection.last_progress = now;
            }
            connection.prepare(now, free, &mut segments);
            if segments.is_empty() {
                continue;
            }
            (connection.remote, connection.local_port, connection.remote_port, connection.generation)
        };
        let (remote, local_port, remote_port, generation) = target;
        if let Some(sequence) = transmit(remote, local_port, remote_port, &segments, true) {
            rewind_to(slot, generation, sequence);
        }
    }
}

/// The window scale a SYN-ACK offers, if any.
fn scale_option(options: &[u8]) -> Option<u8> {
    let mut index = 0;
    while index < options.len() {
        match options[index] {
            0 => return None,
            1 => index += 1,
            kind => {
                let length = *options.get(index + 1)? as usize;
                if length < 2 {
                    return None;
                }
                if kind == 3 && length == 3 {
                    return options.get(index + 2).map(|shift| (*shift).min(14));
                }
                index += length;
            }
        }
    }
    None
}

/// Handles a received segment, in the task that polled the card, then
/// tells the Linux sockets that something may have changed.
pub fn input(source: Ipv4, destination: Ipv4, segment: &[u8]) {
    input_segment(source, destination, segment);
    crate::posix::net::notify();
}

fn input_segment(source: Ipv4, destination: Ipv4, segment: &[u8]) {
    if segment.len() < HEADER {
        super::discard();
        return;
    }

    let pseudo = ip::pseudo_header(source, destination, ip::PROTOCOL_TCP, segment.len());
    if checksum(&[&pseudo, segment]) != 0 {
        super::discard();
        return;
    }

    let source_port = u16::from_be_bytes([segment[0], segment[1]]);
    let destination_port = u16::from_be_bytes([segment[2], segment[3]]);
    let sequence = u32::from_be_bytes([segment[4], segment[5], segment[6], segment[7]]);
    let acknowledgement = u32::from_be_bytes([segment[8], segment[9], segment[10], segment[11]]);
    let data_offset = ((segment[12] >> 4) as usize) * 4;
    let flags = segment[13];
    let window = u16::from_be_bytes([segment[14], segment[15]]);

    if data_offset < HEADER || data_offset > segment.len() {
        super::discard();
        return;
    }
    let options = &segment[HEADER..data_offset];
    let payload = &segment[data_offset..];

    // Answer built under the lock, sent once it is released; so are the
    // segments an acknowledgement lets go (`prepare`).
    let mut reply: Option<(Ipv4, u16, u16, u32, u32, u8, u16)> = None;
    let mut segments = Vec::new();
    let mut sender: Option<(Ipv4, u16, u16, usize, u32)> = None;

    {
        let mut connections = CONNECTIONS.lock();
        let free = table_free(&connections);
        let found = connections.iter().position(|c| {
            c.used && c.local_port == destination_port && c.remote_port == source_port && c.remote == source
        });

        let Some(slot) = found else {
            super::discard();
            return;
        };
        let connection = &mut connections[slot];
        connection.segments_in += 1;

        if flags & RST != 0 {
            connection.reset = true;
            connection.state = State::Closed;
            connection.used = false;
            connection.outgoing = VecDeque::new();
            connection.incoming = VecDeque::new();
            connection.ahead.clear();
            connection.ahead_bytes = 0;
            return;
        }

        match connection.state {
            State::SynSent => {
                if flags & SYN != 0 && flags & ACK != 0 {
                    if acknowledgement != connection.snd_nxt {
                        // An acknowledgement of a number we did not send:
                        // a segment of an earlier connection.
                        return;
                    }
                    // Scaling applies both ways only if both offered it.
                    if let Some(shift) = scale_option(options) {
                        connection.snd_scale = shift;
                        connection.rcv_scale = WINDOW_SCALE;
                    }
                    // The window of a SYN is never scaled.
                    connection.snd_wnd = window as u32;
                    connection.rcv_nxt = sequence.wrapping_add(1);
                    connection.snd_una = acknowledgement;
                    connection.state = State::Established;
                    reply = Some((
                        connection.remote,
                        connection.local_port,
                        connection.remote_port,
                        connection.snd_nxt,
                        connection.rcv_nxt,
                        ACK,
                        connection.window_field(free),
                    ));
                }
            }
            _ => {
                let announced = (window as u32) << connection.snd_scale;
                let now = crate::idt::ticks();
                if flags & ACK != 0 && after(acknowledgement, connection.snd_una) && !after(acknowledgement, connection.snd_nxt) {
                    connection.acknowledged(acknowledgement, now);
                } else if flags & (ACK | SYN | FIN) == ACK
                    && payload.is_empty()
                    && acknowledgement == connection.snd_una
                    && connection.in_flight() > 0
                    && announced == connection.snd_wnd
                {
                    // A duplicate acknowledgement: the other end received
                    // something past a hole. The third says a segment is
                    // lost -- sent again now, not at the timer (RFC 5681).
                    connection.dup_acks = connection.dup_acks.saturating_add(1);
                    if connection.dup_acks == 3 {
                        connection.congestion(false);
                        connection.rewind();
                        connection.last_progress = now;
                    }
                }
                connection.snd_wnd = announced;

                if !payload.is_empty() && connection.orphan {
                    // Nobody will read it: acknowledged, not kept.
                    if sequence == connection.rcv_nxt {
                        connection.rcv_nxt = connection.rcv_nxt.wrapping_add(payload.len() as u32);
                    }
                    reply = Some((
                        connection.remote,
                        connection.local_port,
                        connection.remote_port,
                        connection.snd_nxt,
                        connection.rcv_nxt,
                        ACK,
                        connection.window_field(free),
                    ));
                } else if !payload.is_empty() {
                    let room = connection.window_bytes(free);
                    let in_order = sequence == connection.rcv_nxt;
                    if in_order {
                        let count = accept_bytes(&mut connection.incoming, room.min(payload.len()), free);
                        connection.incoming.extend(&payload[..count]);
                        connection.rcv_nxt = connection.rcv_nxt.wrapping_add(count as u32);
                        connection.absorb(free);
                    } else if after(sequence, connection.rcv_nxt) {
                        // Ahead of a missing segment: kept, if it fits,
                        // and the acknowledgement below names the gap.
                        let distance = sequence.wrapping_sub(connection.rcv_nxt) as usize;
                        // The copy may fail like any reception buffer: the
                        // segment is then dropped, and sent again.
                        let mut copy = Vec::new();
                        if distance + payload.len() <= room
                            && payload.len() + AHEAD_OVERHEAD <= free
                            && !connection.ahead.contains_key(&sequence)
                            && copy.try_reserve_exact(payload.len()).is_ok()
                        {
                            copy.extend_from_slice(payload);
                            connection.ahead_bytes += payload.len();
                            connection.ahead.insert(sequence, copy);
                            connection.out_of_order += 1;
                        }
                    } else {
                        // Already received, in part: what follows is new.
                        let end = sequence.wrapping_add(payload.len() as u32);
                        if after(end, connection.rcv_nxt) {
                            let skip = connection.rcv_nxt.wrapping_sub(sequence) as usize;
                            let count = accept_bytes(&mut connection.incoming, room.min(payload.len() - skip), free);
                            connection.incoming.extend(&payload[skip..skip + count]);
                            connection.rcv_nxt = connection.rcv_nxt.wrapping_add(count as u32);
                            connection.absorb(free);
                        }
                    }
                    // An in-order segment with no gap behind it waits for
                    // the end of the poll (`flush_acks`), or for
                    // `ACK_EVERY` others. Anything else -- a segment out of
                    // order, a duplicate -- is acknowledged at once: those
                    // duplicate ACKs are what the sender's fast
                    // retransmission counts.
                    connection.ack_pending = connection.ack_pending.saturating_add(1);
                    let deferred = in_order && connection.ahead.is_empty() && connection.ack_pending < ACK_EVERY;
                    if !deferred {
                        connection.ack_pending = 0;
                        reply = Some((
                            connection.remote,
                            connection.local_port,
                            connection.remote_port,
                            connection.snd_nxt,
                            connection.rcv_nxt,
                            ACK,
                            connection.window_field(free),
                        ));
                    }
                }

                if flags & FIN != 0 && sequence.wrapping_add(payload.len() as u32) == connection.rcv_nxt {
                    connection.rcv_nxt = connection.rcv_nxt.wrapping_add(1);
                    connection.peer_finished = true;
                    connection.ack_pending = 0;
                    connection.state = match connection.state {
                        State::FinWait1 | State::FinWait2 => State::TimeWait,
                        _ => State::CloseWait,
                    };
                    reply = Some((
                        connection.remote,
                        connection.local_port,
                        connection.remote_port,
                        connection.snd_nxt,
                        connection.rcv_nxt,
                        ACK,
                        connection.window_field(free),
                    ));
                }

                // What the acknowledgement freed, or the window opened,
                // leaves now, from this task.
                connection.prepare(now, free, &mut segments);
                if !segments.is_empty() {
                    sender = Some((connection.remote, connection.local_port, connection.remote_port, slot, connection.generation));
                }
            }
        }
    }

    if let Some((remote, local_port, remote_port, sequence, acknowledgement, flags, window)) = reply {
        emit_cached(remote, local_port, remote_port, sequence, acknowledgement, flags, window);
    }
    if let Some((remote, local_port, remote_port, slot, generation)) = sender
        && let Some(sequence) = transmit(remote, local_port, remote_port, &segments, true)
    {
        rewind_to(slot, generation, sequence);
    }
}

/// Sends the acknowledgements `input` deferred.
///
/// ## Why acknowledgements are grouped
///
/// Every ACK is a frame, and every frame a notification of the card --
/// on virtio-net, an exit of the virtual machine. One ACK per segment was
/// 370 000 of them for 512 MiB, about as many as the segments received.
/// So in-order segments are acknowledged together, by at most
/// `ACK_EVERY`, and `nic::poll` calls this at the end of **every** batch
/// it hands to the stack: an ACK is never deferred past the poll that
/// received its segment, so no timer is needed. RFC 1122 asks for an ACK
/// at least every second full segment; a batch of the card is the same
/// trade Linux makes with GRO, where a batch is also acknowledged as one.
pub fn flush_acks() {
    // Collected by groups into a small array on the stack: a table-sized
    // one would be 8 KiB of a kernel stack with no guard page. Every
    // group is sent before the next is taken, until none waits -- an ACK
    // left for "the next poll" would wait for a frame that may not come.
    const ACK_BATCH: usize = 32;
    loop {
        let mut pending: [Option<(Ipv4, u16, u16, u32, u32, u16)>; ACK_BATCH] = [None; ACK_BATCH];
        {
            let mut connections = CONNECTIONS.lock();
            let free = table_free(&connections);
            let waiting = connections.iter_mut().filter(|c| c.used && c.ack_pending != 0);
            for (slot, connection) in pending.iter_mut().zip(waiting) {
                connection.ack_pending = 0;
                *slot = Some((
                    connection.remote,
                    connection.local_port,
                    connection.remote_port,
                    connection.snd_nxt,
                    connection.rcv_nxt,
                    connection.window_field(free),
                ));
            }
        }
        if pending[0].is_none() {
            return;
        }
        for (remote, local_port, remote_port, sequence, acknowledgement, window) in pending.into_iter().flatten() {
            emit_cached(remote, local_port, remote_port, sequence, acknowledgement, ACK, window);
        }
    }
}

/// Counters of a connection: (segments received, sent, retransmitted).
pub fn counters(handle: Handle) -> (u64, u64, u64) {
    named(&mut CONNECTIONS.lock(), handle)
        .map_or((0, 0, 0), |connection| (connection.segments_in, connection.segments_out, connection.retransmits))
}

pub fn dump_state() {
    // The local address is read **before** taking the table: two locks
    // nested one way here and the other way elsewhere build a deadlock.
    let local = super::interface().ip.text();

    // The lines are made under the table's lock and printed after it:
    // the table is taken for every segment received, and every serial
    // byte is slow.
    let lines: Vec<alloc::string::String> = CONNECTIONS
        .lock()
        .iter()
        .filter(|c| c.used)
        .map(|connection| {
            alloc::format!(
                "[tcp]   {}:{} -> {}:{} - {} - {} recus / {} emis / {} retransmis / {} hors ordre - fenetre {} o, congestion {} o, a envoyer {} o",
                local,
                connection.local_port,
                connection.remote.text(),
                connection.remote_port,
                connection.state.as_str(),
                connection.segments_in,
                connection.segments_out,
                connection.retransmits,
                connection.out_of_order,
                connection.snd_wnd,
                connection.cwnd,
                connection.outgoing.len()
            )
        })
        .collect();
    if lines.is_empty() {
        return;
    }
    crate::serial_println!("[tcp] {} connexion(s)", lines.len());
    for line in lines {
        crate::serial_println!("{}", line);
    }
}

//! UDP -- datagrams (RFC 768).
//!
//! Eight header bytes, no guarantee of delivery or order. It is the
//! transport of DHCP and DNS, and it is for them that it exists here.
//!
//! ## The checksum is optional, and we compute it anyway
//!
//! In IPv4, a sender may leave the field at zero to say "I did not
//! compute one". We always compute it: the cost is negligible and the
//! absence of a check would make a corruption in a DNS transfer
//! undetectable, and that answer dictates who we will talk to next.
//!
//! A subtlety of the format: a computed checksum that **is zero** must be
//! transmitted as 0xFFFF, the two being the same thing in ones'
//! complement. Emitting the literal zero would mean "no checksum", and
//! would disable the check in exactly the case where the computation had
//! produced that value.
//!
//! ## Ephemeral ports
//!
//! A client that does not state its port receives one in the high range
//! recommended by the IANA, 49152 to 65535, drawn from a counter. Taking
//! it low would make an answer collide with a service.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::{checksum, ip, Ipv4};
use crate::vga::SpinLock;

/// Length of the UDP header.
pub const HEADER: usize = 8;

/// Number of simultaneous sockets, for the whole system.
///
/// Sixteen was the first value, and the browser alone came within one of
/// it: every name Firefox resolves opens a socket for the query, and every
/// HTTP/3 connection (QUIC: Google, YouTube, speedtest.net) keeps one for
/// its whole life. Measured: 15 open during a speed test, 14 over nine
/// sites. The seventeenth `bind` failed, the resolver got `EADDRINUSE`,
/// and the page said "we're having trouble finding that site" -- the
/// network seemed to cut out, with the network fine.
const MAX_SOCKETS: usize = 256;

/// Bytes a socket may keep unread before the oldest datagram is dropped.
/// A count of eight datagrams was the first bound: a QUIC flight is a
/// dozen full packets, and a socket of the speed test lost 69 of them.
const QUEUE_BYTES: usize = 256 * 1024;

/// Bytes all sockets together may keep unread: the queues live in the
/// kernel heap (64 MiB), where an allocation that fails stops the machine.
const TOTAL_BYTES: usize = 4 * 1024 * 1024;

/// Heap a queued datagram costs beyond its bytes, roughly.
const DATAGRAM_OVERHEAD: usize = 48;

/// Range of the ephemeral ports (IANA).
const EPHEMERAL_FIRST: u16 = 49152;
const EPHEMERAL_LAST: u16 = 65535;

static NEXT_EPHEMERAL: AtomicU64 = AtomicU64::new(EPHEMERAL_FIRST as u64);

/// Received datagram, with its origin.
#[derive(Clone)]
pub struct Datagram {
    pub source: Ipv4,
    pub port: u16,
    pub payload: Vec<u8>,
}

struct Socket {
    used: bool,
    /// Incremented each time the entry is given to a new socket: a
    /// `Handle` names one generation, never the socket that succeeds it.
    generation: u32,
    port: u16,
    queue: VecDeque<Datagram>,
    /// Heap the queue occupies (`DATAGRAM_OVERHEAD` included).
    queued_bytes: usize,
    dropped: u64,
}

impl Socket {
    const fn new() -> Self {
        Socket {
            used: false,
            generation: 0,
            port: 0,
            queue: VecDeque::new(),
            queued_bytes: 0,
            dropped: 0,
        }
    }

    /// Empties the queue, giving its bytes back to the global budget.
    fn clear(&mut self) {
        QUEUED.fetch_sub(self.queued_bytes, Ordering::Relaxed);
        self.queued_bytes = 0;
        self.queue = VecDeque::new();
    }

    fn pop(&mut self) -> Option<Datagram> {
        let datagram = self.queue.pop_front()?;
        let cost = datagram.payload.capacity() + DATAGRAM_OVERHEAD;
        self.queued_bytes -= cost;
        QUEUED.fetch_sub(cost, Ordering::Relaxed);
        Some(datagram)
    }
}

/// Bytes queued over every socket (`TOTAL_BYTES`).
static QUEUED: AtomicUsize = AtomicUsize::new(0);

static SOCKETS: SpinLock<[Socket; MAX_SOCKETS]> =
    SpinLock::new([const { Socket::new() }; MAX_SOCKETS]);

/// Reference to an open socket: an entry of the table and the generation
/// it was opened under. A reference kept after its socket closed names a
/// closed socket, never the one that took the entry next (see the same
/// reasoning in `tcp::Handle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handle {
    slot: usize,
    generation: u32,
}

impl Handle {
    pub fn index(&self) -> usize {
        self.slot
    }
}

/// The socket a reference names, if it is still the one it was.
fn named(sockets: &mut [Socket; MAX_SOCKETS], handle: Handle) -> Option<&mut Socket> {
    let socket = &mut sockets[handle.slot];
    (socket.used && socket.generation == handle.generation).then_some(socket)
}

fn allocate_port() -> u16 {
    let value = NEXT_EPHEMERAL.fetch_add(1, Ordering::Relaxed);
    let span = (EPHEMERAL_LAST - EPHEMERAL_FIRST + 1) as u64;
    EPHEMERAL_FIRST + (value % span) as u16
}

/// Opens a socket. A null port gets an ephemeral one assigned.
pub fn bind(port: u16) -> Result<(Handle, u16), &'static str> {
    let mut sockets = SOCKETS.lock();

    let port = if port == 0 {
        // The port drawn must be free: the range is vast, but a
        // long-running service may have kept its own.
        let mut candidate = allocate_port();
        for _ in 0..MAX_SOCKETS {
            if !sockets.iter().any(|s| s.used && s.port == candidate) {
                break;
            }
            candidate = allocate_port();
        }
        candidate
    } else {
        if sockets.iter().any(|s| s.used && s.port == port) {
            return Err("port deja ouvert");
        }
        port
    };

    for (index, socket) in sockets.iter_mut().enumerate() {
        if !socket.used {
            socket.clear();
            socket.used = true;
            socket.generation = socket.generation.wrapping_add(1).max(1);
            socket.port = port;
            socket.dropped = 0;
            return Ok((Handle { slot: index, generation: socket.generation }, port));
        }
    }
    // Said on the serial console, the table's lock released first: a full
    // table looks, from a program, like a network that cut out (the
    // resolver cannot even ask).
    drop(sockets);
    crate::serial_println!("[udp] table pleine : {} sockets ouvertes, bind refuse", MAX_SOCKETS);
    Err("plus de socket disponible")
}

/// Closes a socket and throws away what it has not read.
pub fn close(handle: Handle) {
    let mut sockets = SOCKETS.lock();
    let Some(socket) = named(&mut sockets, handle) else {
        return;
    };
    socket.clear();
    socket.used = false;
    socket.port = 0;
}

/// Builds a datagram, checksum included.
fn build(source: Ipv4, destination: Ipv4, from: u16, to: u16, payload: &[u8]) -> Vec<u8> {
    let length = HEADER + payload.len();
    let mut datagram = Vec::with_capacity(length);

    datagram.extend_from_slice(&from.to_be_bytes());
    datagram.extend_from_slice(&to.to_be_bytes());
    datagram.extend_from_slice(&(length as u16).to_be_bytes());
    datagram.extend_from_slice(&[0, 0]);
    datagram.extend_from_slice(payload);

    let pseudo = ip::pseudo_header(source, destination, ip::PROTOCOL_UDP, length);
    let mut sum = checksum(&[&pseudo, &datagram]);
    if sum == 0 {
        // Zero means "no checksum": the equivalent value in ones'
        // complement is 0xFFFF, and that is the one to emit.
        sum = 0xFFFF;
    }
    datagram[6..8].copy_from_slice(&sum.to_be_bytes());
    datagram
}

/// Sends a datagram from an open socket.
pub fn send_to(
    handle: Handle,
    destination: Ipv4,
    port: u16,
    payload: &[u8],
) -> Result<usize, &'static str> {
    let source_port = {
        let mut sockets = SOCKETS.lock();
        named(&mut sockets, handle).ok_or("socket fermee")?.port
    };

    let interface = super::interface();
    let datagram = build(interface.ip, destination, source_port, port, payload);
    ip::send(ip::PROTOCOL_UDP, destination, &datagram)?;
    Ok(payload.len())
}

/// Sends a datagram forcing the hardware source and destination.
///
/// DHCP needs it: it talks before having an address, and its server is
/// not in the ARP cache yet.
pub fn send_broadcast(
    source_port: u16,
    destination_port: u16,
    payload: &[u8],
) -> Result<(), &'static str> {
    let datagram = build(
        Ipv4::UNSPECIFIED,
        Ipv4::BROADCAST,
        source_port,
        destination_port,
        payload,
    );
    ip::send_raw(
        ip::PROTOCOL_UDP,
        Ipv4::UNSPECIFIED,
        Ipv4::BROADCAST,
        super::MAC_BROADCAST,
        &datagram,
    )
}

/// Datagrams waiting on a socket (Linux sockets poll this).
pub fn pending(handle: Handle) -> usize {
    named(&mut SOCKETS.lock(), handle).map_or(0, |socket| socket.queue.len())
}

/// Removes a datagram from the queue, without waiting.
pub fn try_recv(handle: Handle) -> Option<Datagram> {
    named(&mut SOCKETS.lock(), handle)?.pop()
}

/// Waits for a datagram, for at most `timeout_ms` milliseconds.
pub fn recv(handle: Handle, timeout_ms: u64) -> Option<Datagram> {
    let per_tick = 1000 / crate::TIMER_HZ as u64;
    let deadline = crate::idt::ticks() + timeout_ms.div_ceil(per_tick).max(1);

    loop {
        if let Some(datagram) = try_recv(handle) {
            return Some(datagram);
        }
        if crate::idt::ticks() >= deadline {
            return None;
        }
        let _ = crate::syscall::invoke(
            crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
            [per_tick, 0, 0, 0, 0, 0],
        );
    }
}

/// Handles a received datagram.
pub fn input(source: Ipv4, destination: Ipv4, datagram: &[u8]) {
    input_datagram(source, destination, datagram);
    crate::posix::net::notify();
}

fn input_datagram(source: Ipv4, destination: Ipv4, datagram: &[u8]) {
    if datagram.len() < HEADER {
        super::discard();
        return;
    }

    let source_port = u16::from_be_bytes([datagram[0], datagram[1]]);
    let destination_port = u16::from_be_bytes([datagram[2], datagram[3]]);
    let length = u16::from_be_bytes([datagram[4], datagram[5]]) as usize;
    if length < HEADER || length > datagram.len() {
        super::discard();
        return;
    }

    // A null checksum means the sender computed none: the format allows
    // it in IPv4, and refusing those datagrams would cut communication
    // with perfectly conforming stacks.
    let declared = u16::from_be_bytes([datagram[6], datagram[7]]);
    if declared != 0 {
        let pseudo = ip::pseudo_header(source, destination, ip::PROTOCOL_UDP, length);
        if checksum(&[&pseudo, &datagram[..length]]) != 0 {
            super::discard();
            return;
        }
    }

    // The copy may fail: on a full heap the datagram is lost, as UDP
    // allows, instead of the kernel stopping.
    let mut payload = Vec::new();
    if payload.try_reserve_exact(length - HEADER).is_err() {
        super::discard();
        return;
    }
    payload.extend_from_slice(&datagram[HEADER..length]);
    let cost = payload.capacity() + DATAGRAM_OVERHEAD;

    let mut sockets = SOCKETS.lock();
    match sockets
        .iter_mut()
        .find(|s| s.used && s.port == destination_port)
    {
        Some(socket) => {
            // Room made by dropping the oldest, as a full queue does; a
            // datagram larger than the budgets is not kept at all.
            while !socket.queue.is_empty()
                && (socket.queued_bytes + cost > QUEUE_BYTES || QUEUED.load(Ordering::Relaxed) + cost > TOTAL_BYTES)
            {
                socket.pop();
                socket.dropped += 1;
            }
            if socket.queued_bytes + cost > QUEUE_BYTES
                || QUEUED.load(Ordering::Relaxed) + cost > TOTAL_BYTES
                || socket.queue.try_reserve(1).is_err()
            {
                socket.dropped += 1;
                return;
            }
            socket.queued_bytes += cost;
            QUEUED.fetch_add(cost, Ordering::Relaxed);
            socket.queue.push_back(Datagram {
                source,
                port: source_port,
                payload,
            });
        }
        // No recipient: the RFC wants an ICMP port unreachable. We drop
        // silently -- emitting an error from a host that serves nothing
        // makes it a scan amplifier.
        None => super::discard(),
    }
}

pub fn dump_state() {
    // Copied, then printed: the table is taken for every datagram.
    let sockets: alloc::vec::Vec<(u16, usize, u64)> = SOCKETS
        .lock()
        .iter()
        .filter(|s| s.used)
        .map(|s| (s.port, s.queue.len(), s.dropped as u64))
        .collect();
    if sockets.is_empty() {
        return;
    }
    crate::serial_println!("[udp] {} socket(s) ouverte(s)", sockets.len());
    for (port, waiting, dropped) in sockets {
        crate::serial_println!("[udp]   port {} - {} en attente, {} perdus", port, waiting, dropped);
    }
}

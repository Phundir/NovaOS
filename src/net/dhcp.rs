//! 6.2.3 -- DHCP client (RFC 2131).
//!
//! Four messages: DISCOVER as a broadcast, OFFER from the server, REQUEST
//! accepting the offer, ACK confirming it. The REQUEST is indispensable
//! even when a single offer arrives: several servers may answer, and it
//! is what says which one was kept -- without it, the others keep the
//! address reserved for nothing.
//!
//! ## The broadcast flag
//!
//! The client has no address yet: the server cannot answer it in unicast
//! without assuming its stack accepts a datagram addressed to an IP it
//! does not carry yet. Bit 15 of the `flags` field explicitly asks for a
//! broadcast answer, and that is what makes the exchange reliable
//! whatever the implementation opposite.
//!
//! ## The transaction identifier
//!
//! It pairs answer with request. An answer carrying another identifier is
//! meant for another client -- on a shared link everybody receives
//! everything -- and ignoring it is not a precaution but a condition of
//! correctness.
//!
//! ## What is not done
//!
//! The lease is not renewed. Its duration is recorded and displayed;
//! reaching half the lease should trigger a renewal REQUEST. Under QEMU
//! the lease lasts a day, which is enough for any test session, but a
//! real system would lose its address.

use alloc::vec::Vec;

use super::{udp, Ipv4};

const CLIENT_PORT: u16 = 68;
const SERVER_PORT: u16 = 67;

/// Length of the fixed part of a BOOTP message.
const FIXED: usize = 236;

/// Marker that precedes the DHCP options.
const MAGIC: [u8; 4] = [0x63, 0x82, 0x53, 0x63];

const OP_REQUEST: u8 = 1;
const OP_REPLY: u8 = 2;
const HTYPE_ETHERNET: u8 = 1;

/// Bit 15 of `flags`: answer as a broadcast.
const FLAG_BROADCAST: u16 = 0x8000;

const OPTION_SUBNET: u8 = 1;
const OPTION_ROUTER: u8 = 3;
const OPTION_DNS: u8 = 6;
const OPTION_REQUESTED_IP: u8 = 50;
const OPTION_LEASE: u8 = 51;
const OPTION_MESSAGE_TYPE: u8 = 53;
const OPTION_SERVER_ID: u8 = 54;
const OPTION_PARAMETERS: u8 = 55;
const OPTION_END: u8 = 255;

const DISCOVER: u8 = 1;
const OFFER: u8 = 2;
const REQUEST: u8 = 3;
const ACK: u8 = 5;
const NAK: u8 = 6;

/// Wait for an answer, in milliseconds.
const REPLY_MS: u64 = 3000;

/// Complete attempts before giving up.
const ATTEMPTS: u32 = 3;

/// Result of a successful negotiation.
#[derive(Debug, Clone, Copy)]
pub struct Lease {
    pub ip: Ipv4,
    pub netmask: Ipv4,
    pub gateway: Ipv4,
    pub dns: Ipv4,
    pub server: Ipv4,
    pub seconds: u32,
}

fn build(kind: u8, xid: u32, requested: Option<Ipv4>, server: Option<Ipv4>) -> Vec<u8> {
    let mac = super::interface().mac;
    let mut message = alloc::vec![0u8; FIXED];

    message[0] = OP_REQUEST;
    message[1] = HTYPE_ETHERNET;
    message[2] = 6;
    message[3] = 0;
    message[4..8].copy_from_slice(&xid.to_be_bytes());
    message[10..12].copy_from_slice(&FLAG_BROADCAST.to_be_bytes());
    message[28..34].copy_from_slice(&mac);

    message.extend_from_slice(&MAGIC);
    message.extend_from_slice(&[OPTION_MESSAGE_TYPE, 1, kind]);

    if let Some(ip) = requested {
        message.push(OPTION_REQUESTED_IP);
        message.push(4);
        message.extend_from_slice(&ip.0);
    }
    if let Some(ip) = server {
        message.push(OPTION_SERVER_ID);
        message.push(4);
        message.extend_from_slice(&ip.0);
    }

    message.extend_from_slice(&[
        OPTION_PARAMETERS,
        3,
        OPTION_SUBNET,
        OPTION_ROUTER,
        OPTION_DNS,
    ]);
    message.push(OPTION_END);
    message
}

/// Decoded answer.
struct Reply {
    kind: u8,
    yiaddr: Ipv4,
    netmask: Ipv4,
    gateway: Ipv4,
    dns: Ipv4,
    server: Ipv4,
    lease: u32,
}

/// Parses a received message. Returns `None` if it is not a DHCP answer
/// that concerns us.
fn parse(message: &[u8], xid: u32) -> Option<Reply> {
    if message.len() < FIXED + 4 {
        return None;
    }
    if message[0] != OP_REPLY {
        return None;
    }
    if u32::from_be_bytes(message[4..8].try_into().ok()?) != xid {
        return None;
    }
    if message[FIXED..FIXED + 4] != MAGIC {
        return None;
    }

    let mut reply = Reply {
        kind: 0,
        yiaddr: Ipv4([message[16], message[17], message[18], message[19]]),
        netmask: Ipv4::UNSPECIFIED,
        gateway: Ipv4::UNSPECIFIED,
        dns: Ipv4::UNSPECIFIED,
        server: Ipv4::UNSPECIFIED,
        lease: 0,
    };

    // The options are a type-length-value sequence. A walk assuming a
    // fixed length would go off the rails at the first unknown option --
    // and there is always one.
    let mut index = FIXED + 4;
    while index < message.len() {
        let code = message[index];
        if code == OPTION_END {
            break;
        }
        if code == 0 {
            // Padding: a single byte, no length.
            index += 1;
            continue;
        }
        if index + 1 >= message.len() {
            break;
        }
        let length = message[index + 1] as usize;
        let start = index + 2;
        if start + length > message.len() {
            break;
        }
        let value = &message[start..start + length];

        match (code, length) {
            (OPTION_MESSAGE_TYPE, 1) => reply.kind = value[0],
            (OPTION_SUBNET, 4) => reply.netmask = Ipv4([value[0], value[1], value[2], value[3]]),
            (OPTION_ROUTER, l) if l >= 4 => {
                reply.gateway = Ipv4([value[0], value[1], value[2], value[3]])
            }
            (OPTION_DNS, l) if l >= 4 => reply.dns = Ipv4([value[0], value[1], value[2], value[3]]),
            (OPTION_SERVER_ID, 4) => reply.server = Ipv4([value[0], value[1], value[2], value[3]]),
            (OPTION_LEASE, 4) => {
                reply.lease = u32::from_be_bytes([value[0], value[1], value[2], value[3]])
            }
            _ => {}
        }
        index = start + length;
    }

    Some(reply)
}

/// Negotiates a lease. Applies the configuration obtained to the interface.
pub fn acquire() -> Result<Lease, &'static str> {
    if !super::interface().attached {
        return Err("aucune carte reseau");
    }

    let (socket, _) = udp::bind(CLIENT_PORT)?;
    let outcome = negotiate(socket);
    udp::close(socket);
    outcome
}

fn negotiate(socket: udp::Handle) -> Result<Lease, &'static str> {
    // The transaction identifier must differ from one attempt to the
    // next: reusing the same one would accept the previous offer, which
    // may already have expired on the server.
    for attempt in 0..ATTEMPTS {
        let xid = (crate::idt::ticks() as u32)
            .wrapping_mul(2_654_435_761)
            .wrapping_add(attempt);

        udp::send_broadcast(CLIENT_PORT, SERVER_PORT, &build(DISCOVER, xid, None, None))?;

        let Some(offer) = wait_for(socket, xid, OFFER) else {
            continue;
        };

        udp::send_broadcast(
            CLIENT_PORT,
            SERVER_PORT,
            &build(REQUEST, xid, Some(offer.yiaddr), Some(offer.server)),
        )?;

        let Some(ack) = wait_for(socket, xid, ACK) else {
            continue;
        };

        // A server may not return the mask: without it, nothing tells the
        // local link from the rest, and every send would go to the
        // gateway. The fallback value follows the class of the address,
        // which is what IP did before CIDR -- approximate, but always
        // better than zero.
        let netmask = if ack.netmask.is_unspecified() {
            match ack.yiaddr.0[0] {
                0..=127 => Ipv4([255, 0, 0, 0]),
                128..=191 => Ipv4([255, 255, 0, 0]),
                _ => Ipv4([255, 255, 255, 0]),
            }
        } else {
            ack.netmask
        };

        let lease = Lease {
            ip: ack.yiaddr,
            netmask,
            gateway: ack.gateway,
            dns: ack.dns,
            server: ack.server,
            seconds: ack.lease,
        };

        // The lease's name server is not taken: the system's (Cloudflare,
        // or one chosen by hand) stays. See `net::DNS_CLOUDFLARE`.
        let dns = super::interface().dns;
        super::configure(lease.ip, lease.netmask, lease.gateway, dns, true, lease.seconds);
        return Ok(lease);
    }

    Err("aucun serveur DHCP n'a repondu")
}

/// Waits for an answer of the wanted type, ignoring those of others.
fn wait_for(socket: udp::Handle, xid: u32, kind: u8) -> Option<Reply> {
    let deadline = crate::idt::ticks() + (REPLY_MS / 10).max(1);

    while crate::idt::ticks() < deadline {
        let remaining = (deadline.saturating_sub(crate::idt::ticks())) * 10;
        let datagram = udp::recv(socket, remaining.max(10))?;
        if let Some(reply) = parse(&datagram.payload, xid) {
            if reply.kind == kind {
                return Some(reply);
            }
            if reply.kind == NAK {
                return None;
            }
        }
    }
    None
}

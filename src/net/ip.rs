//! IPv4 -- encapsulation, checking and routing (RFC 791).
//!
//! ## Variable-length header
//!
//! The IHL field gives the header length in four-byte words: it is
//! twenty bytes in the vast majority of cases, but a packet carrying
//! options counts more. Taking twenty for granted makes the payload be
//! read at an offset -- a TCP packet whose IP options would be read as
//! the start of the TCP header. So the offset is always computed.
//!
//! ## Checksum
//!
//! It covers **the header only**, not the payload -- protecting theirs is
//! up to the transport protocols, and that is why UDP and TCP each have
//! their own, computed over a pseudo-header that takes back the IP
//! addresses. A packet whose header is correct but whose payload is
//! corrupt therefore passes here unhindered, and will be rejected higher up.
//!
//! ## Fragmentation
//!
//! Not supported. A packet carrying a fragment offset or the "more
//! follow" flag is discarded. Reassembling requires a fragment table with
//! its deadlines and its overlap policy -- historically one of the most
//! fertile attack surfaces of the IP stacks. The path exercised here
//! never fragments: DHCP fits in one frame, DNS too, and TCP announces an
//! MSS that avoids it by construction.

use alloc::vec::Vec;

use super::{arp, checksum, eth, Ipv4, Mac};

/// Length of a header with no options.
pub const HEADER: usize = 20;

pub const PROTOCOL_ICMP: u8 = 1;
pub const PROTOCOL_TCP: u8 = 6;
pub const PROTOCOL_UDP: u8 = 17;

/// Initial time to live. Sixty-four hops is the usual value: enough to
/// cross the Internet, few enough that a routing loop dies out.
const TTL: u8 = 64;

/// Bit 0 of the fragmentation field: more fragments follow.
const FLAG_MORE_FRAGMENTS: u16 = 1 << 13;
/// Bit 1: do not fragment.
const FLAG_DONT_FRAGMENT: u16 = 1 << 14;

/// Datagram identifier counter.
static NEXT_ID: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);

/// Builds a complete datagram.
pub fn build(protocol: u8, source: Ipv4, destination: Ipv4, payload: &[u8]) -> Vec<u8> {
    let total = HEADER + payload.len();
    let mut packet = Vec::with_capacity(total);

    let id = NEXT_ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed) as u16;

    packet.push(0x45); // version 4, header of five words
    packet.push(0x00); // no differentiated service
    packet.extend_from_slice(&(total as u16).to_be_bytes());
    packet.extend_from_slice(&id.to_be_bytes());
    packet.extend_from_slice(&FLAG_DONT_FRAGMENT.to_be_bytes());
    packet.push(TTL);
    packet.push(protocol);
    packet.extend_from_slice(&[0, 0]); // checksum, computed afterwards
    packet.extend_from_slice(&source.0);
    packet.extend_from_slice(&destination.0);

    let sum = checksum(&[&packet[..HEADER]]);
    packet[10..12].copy_from_slice(&sum.to_be_bytes());

    packet.extend_from_slice(payload);
    packet
}

/// Sends a datagram to a destination.
///
/// The source address is that of the interface, unless it has none yet:
/// DHCP sends from 0.0.0.0, which is the case the RFC provides for and
/// the only one where an unspecified source is legitimate.
pub fn send(protocol: u8, destination: Ipv4, payload: &[u8]) -> Result<(), &'static str> {
    let interface = super::interface();
    if !interface.attached {
        return Err("aucune carte reseau");
    }
    if HEADER + payload.len() > super::MTU {
        return Err("datagramme au-dela du MTU, fragmentation non prise en charge");
    }

    let destination_mac: Mac =
        arp::next_hop(destination).ok_or("adresse materielle du prochain saut inconnue")?;

    let packet = build(protocol, interface.ip, destination, payload);
    eth::send(destination_mac, eth::TYPE_IPV4, &packet)
}

/// Sends a datagram without ever waiting for an address resolution.
///
/// **The driver task must not sleep in its own receive path.** `send`
/// may trigger an ARP resolution, which waits for an answer -- which will
/// only arrive through the driver task, busy waiting. The block is
/// bounded, so it is not a deadlock, but reception is stopped for the
/// whole resolution and the wait is lost in advance.
///
/// The answers emitted *from* the handling of a received packet -- a TCP
/// acknowledgement, an ICMP echo answer -- therefore go through here: if
/// the hardware address is not already known, the packet is dropped. That
/// is of no consequence, the other end will retransmit, and that is
/// exactly what TCP retransmits for.
pub fn send_cached(protocol: u8, destination: Ipv4, payload: &[u8]) -> Result<(), &'static str> {
    let interface = super::interface();
    if !interface.attached {
        return Err("aucune carte reseau");
    }
    if HEADER + payload.len() > super::MTU {
        return Err("datagramme au-dela du MTU");
    }

    let destination_mac = arp::next_hop_cached(destination)
        .ok_or("adresse materielle inconnue du cache - paquet abandonne")?;

    let packet = build(protocol, interface.ip, destination, payload);
    eth::send(destination_mac, eth::TYPE_IPV4, &packet)
}

/// Sends a datagram forcing the source address.
///
/// Reserved to DHCP, which must send from 0.0.0.0 to the broadcast before
/// the interface has any address at all.
pub fn send_raw(
    protocol: u8,
    source: Ipv4,
    destination: Ipv4,
    destination_mac: Mac,
    payload: &[u8],
) -> Result<(), &'static str> {
    if HEADER + payload.len() > super::MTU {
        return Err("datagramme au-dela du MTU");
    }
    let packet = build(protocol, source, destination, payload);
    eth::send(destination_mac, eth::TYPE_IPV4, &packet)
}

/// Handles a received datagram.
pub fn input(_source_mac: Mac, packet: &[u8]) {
    if packet.len() < HEADER {
        super::discard();
        return;
    }

    let version = packet[0] >> 4;
    let header_length = ((packet[0] & 0x0F) as usize) * 4;
    if version != 4 || header_length < HEADER || packet.len() < header_length {
        super::discard();
        return;
    }

    // The checksum covers the complete header, options included, and must
    // be zero once the checksum already present is included in the
    // computation.
    if checksum(&[&packet[..header_length]]) != 0 {
        super::discard();
        return;
    }

    let total_length = u16::from_be_bytes([packet[2], packet[3]]) as usize;
    if total_length < header_length || total_length > packet.len() {
        super::discard();
        return;
    }

    let fragment = u16::from_be_bytes([packet[6], packet[7]]);
    if fragment & FLAG_MORE_FRAGMENTS != 0 || fragment & 0x1FFF != 0 {
        super::discard();
        return;
    }

    let protocol = packet[9];
    let source = Ipv4([packet[12], packet[13], packet[14], packet[15]]);
    let destination = Ipv4([packet[16], packet[17], packet[18], packet[19]]);

    // A datagram addressed to somebody else is not for us. The broadcast
    // and 0.0.0.0 are accepted: that is how a DHCP offer arrives, before
    // we have an address.
    let interface = super::interface();
    let for_us = destination == interface.ip
        || destination == Ipv4::BROADCAST
        || interface.ip.is_unspecified();
    if !for_us {
        super::discard();
        return;
    }

    let payload = &packet[header_length..total_length];

    match protocol {
        PROTOCOL_ICMP => super::icmp::input(source, payload),
        PROTOCOL_UDP => super::udp::input(source, destination, payload),
        PROTOCOL_TCP => super::tcp::input(source, destination, payload),
        _ => super::discard(),
    }
}

/// Pseudo-header of the transport checksums.
///
/// UDP and TCP compute their checksum over twelve bytes that never
/// travel: source and destination addresses, protocol, length. That is
/// what binds the segment to the IP header that carried it -- without it,
/// a datagram redirected to another machine would be accepted there as-is.
pub fn pseudo_header(source: Ipv4, destination: Ipv4, protocol: u8, length: usize) -> [u8; 12] {
    let mut header = [0u8; 12];
    header[0..4].copy_from_slice(&source.0);
    header[4..8].copy_from_slice(&destination.0);
    header[8] = 0;
    header[9] = protocol;
    header[10..12].copy_from_slice(&(length as u16).to_be_bytes());
    header
}

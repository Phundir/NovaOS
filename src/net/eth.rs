//! Ethernet II -- encapsulation and demultiplexing.
//!
//! Fourteen header bytes: destination, source, and the type of what
//! follows. Nothing more -- no length. That is what tells Ethernet II
//! from the 802.3 format, where the same field carries a length; the
//! convention is that a value above 1500 names a type. Every modern
//! stack emits Ethernet II only, and it is the only format decoded here.
//!
//! Padding to sixty bytes minimum is done by the card: the e1000 driver
//! sets `PSP` in `TCTL`. Emitting a shorter frame without that bit would
//! produce an invalid frame on the wire.

use alloc::vec::Vec;

use super::{Mac, MAC_BROADCAST};

/// Length of an Ethernet II header.
pub const HEADER: usize = 14;

/// Payload types recognised.
pub const TYPE_IPV4: u16 = 0x0800;
pub const TYPE_ARP: u16 = 0x0806;

/// Builds a complete frame.
pub fn build(destination: Mac, ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(HEADER + payload.len());
    let source = super::interface().mac;

    frame.extend_from_slice(&destination);
    frame.extend_from_slice(&source);
    frame.extend_from_slice(&ethertype.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// Sends a payload to a hardware address.
pub fn send(destination: Mac, ethertype: u16, payload: &[u8]) -> Result<(), &'static str> {
    super::output(&build(destination, ethertype, payload))
}

/// Demultiplexes a received frame.
pub fn input(frame: &[u8]) {
    if frame.len() < HEADER {
        super::discard();
        return;
    }

    let destination: Mac = match frame[0..6].try_into() {
        Ok(mac) => mac,
        Err(_) => return,
    };
    let source: Mac = match frame[6..12].try_into() {
        Ok(mac) => mac,
        Err(_) => return,
    };
    let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    let payload = &frame[HEADER..];

    // The hardware filter of the card already lets our address and the
    // broadcast through; this software check is the second lock. It will
    // count the day promiscuous mode serves to diagnose a network: the
    // stack must not start answering for others.
    let interface = super::interface();
    if destination != MAC_BROADCAST && destination != interface.mac {
        super::discard();
        return;
    }

    match ethertype {
        TYPE_ARP => super::arp::input(payload),
        TYPE_IPV4 => super::ip::input(source, payload),
        _ => super::discard(),
    }
}

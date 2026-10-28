//! ICMP -- echo, and nothing else (RFC 792).
//!
//! Answering an echo is the only ICMP function a system needs to be
//! diagnosable from the outside: it is what makes `ping` useful. The
//! error messages -- destination unreachable, time exceeded -- are
//! received and counted, never emitted: emitting them correctly requires
//! copying the header of the offending packet and keeping a budget so as
//! not to amplify a storm.
//!
//! The answer takes back **the identifier and the sequence number** of
//! the request, along with its whole payload. That is what lets the
//! sender pair answer with request; returning an empty payload produces a
//! `ping` that seems to work and whose times are wrong.

use alloc::vec::Vec;

use super::{checksum, ip, Ipv4};
use core::sync::atomic::{AtomicU64, Ordering};

const TYPE_ECHO_REPLY: u8 = 0;
const TYPE_ECHO_REQUEST: u8 = 8;

/// ICMP header: type, code, checksum, identifier, sequence.
const HEADER: usize = 8;

static ECHOS_IN: AtomicU64 = AtomicU64::new(0);
static ECHOS_OUT: AtomicU64 = AtomicU64::new(0);

/// Echoes received and answers emitted.
pub fn counters() -> (u64, u64) {
    (ECHOS_IN.load(Ordering::Relaxed), ECHOS_OUT.load(Ordering::Relaxed))
}

fn build(kind: u8, identifier: u16, sequence: u16, payload: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(HEADER + payload.len());
    message.push(kind);
    message.push(0);
    message.extend_from_slice(&[0, 0]);
    message.extend_from_slice(&identifier.to_be_bytes());
    message.extend_from_slice(&sequence.to_be_bytes());
    message.extend_from_slice(payload);

    // The ICMP checksum covers the whole message, with no pseudo-header:
    // it is the only one of the three that does not involve the IP addresses.
    let sum = checksum(&[&message]);
    message[2..4].copy_from_slice(&sum.to_be_bytes());
    message
}

/// Emits an echo request.
pub fn echo(destination: Ipv4, identifier: u16, sequence: u16, payload: &[u8]) -> Result<(), &'static str> {
    let message = build(TYPE_ECHO_REQUEST, identifier, sequence, payload);
    ip::send(ip::PROTOCOL_ICMP, destination, &message)
}

/// Handles a received ICMP message.
pub fn input(source: Ipv4, message: &[u8]) {
    if message.len() < HEADER || checksum(&[message]) != 0 {
        super::discard();
        return;
    }

    let kind = message[0];
    if kind != TYPE_ECHO_REQUEST {
        // Echo answer or error message: counted, not processed.
        return;
    }

    ECHOS_IN.fetch_add(1, Ordering::Relaxed);

    let identifier = u16::from_be_bytes([message[4], message[5]]);
    let sequence = u16::from_be_bytes([message[6], message[7]]);
    let reply = build(TYPE_ECHO_REPLY, identifier, sequence, &message[HEADER..]);

    // `send_cached` and not `send`: this code runs in the driver task,
    // which cannot wait for an ARP answer it would itself be in charge of
    // receiving. A `ping` whose hardware address is not yet known goes
    // unanswered once -- the ARP request from the other side has filled
    // our cache anyway before its first echo arrives.
    if ip::send_cached(ip::PROTOCOL_ICMP, source, &reply).is_ok() {
        ECHOS_OUT.fetch_add(1, Ordering::Relaxed);
    }
}

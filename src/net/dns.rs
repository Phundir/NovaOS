//! 6.2.3 -- Name resolution (RFC 1035).
//!
//! One question, one A type, one answer. That is what is needed to reach
//! a host by its name, and nothing more is done: no cache, no local
//! recursive search, no AAAA.
//!
//! ## Name compression
//!
//! A DNS answer rarely rewrites a name in full: it places a two-byte
//! **pointer** to an earlier occurrence in the same message, recognisable
//! by its two high bits being set. A parser ignoring it reads an absurd
//! label length and goes off the rails. A parser following it without
//! care can loop forever on a forged message whose pointer names itself.
//! Both traps are closed here: pointers are followed, and the number of
//! jumps is bounded. It is the same precaution a fragment reassembly
//! would demand, and that is why the latter is not done while this one
//! is: there is no way to speak DNS without decoding the compression.
//!
//! ## What is not checked
//!
//! The origin of the answer is compared against the identifier of the
//! request and the server queried -- that is the minimum. The identifier
//! is drawn from `random`; the source port still comes from a counter,
//! and there is no DNSSEC. Not to be presented as a safe resolution:
//! HTTPS, which checks the certificate, is what protects a page.

use alloc::string::String;
use alloc::vec::Vec;

use super::{udp, Ipv4};

const PORT: u16 = 53;

/// DNS header: identifier, flags, four counters.
const HEADER: usize = 12;

const TYPE_A: u16 = 1;
const CLASS_IN: u16 = 1;

/// Recursion desired bit.
const FLAG_RECURSION: u16 = 1 << 8;
/// Response bit.
const FLAG_RESPONSE: u16 = 1 << 15;

/// Compression jumps tolerated before concluding the message is forged.
const MAX_JUMPS: usize = 16;

/// Wait for an answer, in milliseconds.
const REPLY_MS: u64 = 2000;

/// Attempts before giving up.
const ATTEMPTS: u32 = 2;

/// Maximum length of a name, and of a label.
const NAME_MAX: usize = 253;
const LABEL_MAX: usize = 63;

/// Encodes a name as a sequence of length-value labels, zero terminated.
fn encode(name: &str) -> Option<Vec<u8>> {
    if name.is_empty() || name.len() > NAME_MAX {
        return None;
    }
    let mut encoded = Vec::with_capacity(name.len() + 2);
    for label in name.split('.') {
        if label.is_empty() || label.len() > LABEL_MAX {
            return None;
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return None;
        }
        encoded.push(label.len() as u8);
        encoded.extend_from_slice(label.as_bytes());
    }
    encoded.push(0);
    Some(encoded)
}

/// Skips a name, pointers included, and returns the position just after.
fn skip_name(message: &[u8], mut index: usize) -> Option<usize> {
    let mut jumps = 0;
    loop {
        let length = *message.get(index)?;
        if length & 0xC0 == 0xC0 {
            // A pointer closes the name: what follows in the message
            // resumes two bytes further on, whatever the length of the
            // name pointed at.
            return Some(index + 2);
        }
        if length == 0 {
            return Some(index + 1);
        }
        index += 1 + length as usize;
        jumps += 1;
        if jumps > MAX_JUMPS || index > message.len() {
            return None;
        }
    }
}

/// Queries the DNS servers of the interface for an A record.
pub fn resolve(name: &str) -> Result<Ipv4, &'static str> {
    // A name that is already an address has nothing to resolve: the
    // detour through the server would fail on a machine with no DNS
    // configured, when the answer was in the question.
    if let Some(ip) = Ipv4::parse(name) {
        return Ok(ip);
    }

    let interface = super::interface();
    if !interface.configured() {
        return Err("interface sans adresse IP");
    }

    let question = encode(name).ok_or("nom de domaine invalide")?;
    let (socket, _) = udp::bind(0)?;
    let mut outcome = Err("aucun serveur DNS configure");
    // The interface's server, then Cloudflare's second address: one
    // resolver that stops answering must not take every name with it.
    for server in super::dns_servers().into_iter().flatten() {
        outcome = query(socket, server, &question);
        if outcome.is_ok() {
            break;
        }
    }
    udp::close(socket);
    outcome
}

fn query(socket: udp::Handle, server: Ipv4, question: &[u8]) -> Result<Ipv4, &'static str> {
    for attempt in 0..ATTEMPTS {
        // Unpredictable since `random` exists: a guessable identifier is
        // what lets a third party answer in the server's place.
        let identifier = (crate::random::u64() as u16).wrapping_add(attempt as u16);

        let mut message = Vec::with_capacity(HEADER + question.len() + 4);
        message.extend_from_slice(&identifier.to_be_bytes());
        message.extend_from_slice(&FLAG_RECURSION.to_be_bytes());
        message.extend_from_slice(&1u16.to_be_bytes()); // one question
        message.extend_from_slice(&0u16.to_be_bytes()); // no answer
        message.extend_from_slice(&0u16.to_be_bytes());
        message.extend_from_slice(&0u16.to_be_bytes());
        message.extend_from_slice(question);
        message.extend_from_slice(&TYPE_A.to_be_bytes());
        message.extend_from_slice(&CLASS_IN.to_be_bytes());

        udp::send_to(socket, server, PORT, &message)?;

        let Some(datagram) = udp::recv(socket, REPLY_MS) else {
            continue;
        };
        if datagram.source != server {
            continue;
        }
        if let Some(address) = parse(&datagram.payload, identifier) {
            return Ok(address);
        }
    }
    Err("aucune reponse du serveur DNS")
}

/// Extracts the first A address of an answer.
fn parse(message: &[u8], identifier: u16) -> Option<Ipv4> {
    if message.len() < HEADER {
        return None;
    }
    if u16::from_be_bytes([message[0], message[1]]) != identifier {
        return None;
    }
    let flags = u16::from_be_bytes([message[2], message[3]]);
    if flags & FLAG_RESPONSE == 0 {
        return None;
    }
    // The four low bits of the flags carry the return code; any non-null
    // value is a refusal, not an empty answer.
    if flags & 0x000F != 0 {
        return None;
    }

    let questions = u16::from_be_bytes([message[4], message[5]]) as usize;
    let answers = u16::from_be_bytes([message[6], message[7]]) as usize;

    let mut index = HEADER;
    for _ in 0..questions {
        index = skip_name(message, index)?;
        index += 4; // type and class
        if index > message.len() {
            return None;
        }
    }

    for _ in 0..answers {
        index = skip_name(message, index)?;
        if index + 10 > message.len() {
            return None;
        }
        let kind = u16::from_be_bytes([message[index], message[index + 1]]);
        let class = u16::from_be_bytes([message[index + 2], message[index + 3]]);
        let length = u16::from_be_bytes([message[index + 8], message[index + 9]]) as usize;
        index += 10;
        if index + length > message.len() {
            return None;
        }

        // An answer may start with CNAMEs: we advance to the first A
        // record, instead of concluding a failure.
        if kind == TYPE_A && class == CLASS_IN && length == 4 {
            return Some(Ipv4([
                message[index],
                message[index + 1],
                message[index + 2],
                message[index + 3],
            ]));
        }
        index += length;
    }

    None
}

/// Readable name of an address, for the logs. No reverse resolution is
/// done: the dotted notation is the name.
pub fn describe(address: Ipv4) -> String {
    address.text()
}

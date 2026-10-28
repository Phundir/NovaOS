//! ARP -- hardware address resolution (RFC 826).
//!
//! ## Why a busy wait and not a block
//!
//! Resolving an address means sending a request and waiting for an answer
//! that will arrive in the **driver task**, not in the one waiting.
//! Putting the caller to sleep on a condition would assume a rendezvous
//! point per address being resolved; the stack sends a request, sleeps
//! ten milliseconds, looks at the cache, and starts again. Three
//! attempts, one second in total: beyond that the address is unreachable
//! and saying so is more useful than carrying on.
//!
//! ## The cache is also fed by the requests
//!
//! A host asking us for our address has just given us its own. Recording
//! it avoids a symmetric resolution right afterwards, and it is the
//! behaviour the RFC describes -- the one ARP optimisation that is both
//! free and correct.
//!
//! ## What is not done
//!
//! No entry expiry. A real ARP cache ages its entries within a few
//! minutes, failing which a card change on the same IP address stays
//! invisible. The table is bounded and the oldest entry is replaced when
//! it overflows, which limits the damage without removing it.

use super::{eth, Ipv4, Mac, MAC_BROADCAST};
use crate::vga::SpinLock;

/// Length of an ARP packet for IPv4 over Ethernet.
const PACKET: usize = 28;

const HTYPE_ETHERNET: u16 = 1;
const PTYPE_IPV4: u16 = 0x0800;
const OPER_REQUEST: u16 = 1;
const OPER_REPLY: u16 = 2;

/// Number of cache entries.
const CACHE_LEN: usize = 16;

/// Resolution attempts before giving up.
const ATTEMPTS: u32 = 3;

/// Wait between two attempts, in milliseconds.
const RETRY_MS: u64 = 300;

#[derive(Clone, Copy)]
struct CacheEntry {
    ip: Ipv4,
    mac: Mac,
    /// Tick of the last update -- used to choose the victim.
    stamp: u64,
    used: bool,
}

static CACHE: SpinLock<[CacheEntry; CACHE_LEN]> = SpinLock::new(
    [CacheEntry {
        ip: Ipv4::UNSPECIFIED,
        mac: [0; 6],
        stamp: 0,
        used: false,
    }; CACHE_LEN],
);

/// Records an association.
fn remember(ip: Ipv4, mac: Mac) {
    if ip.is_unspecified() || mac == [0; 6] {
        return;
    }
    let now = crate::idt::ticks();
    let mut cache = CACHE.lock();

    if let Some(entry) = cache.iter_mut().find(|e| e.used && e.ip == ip) {
        entry.mac = mac;
        entry.stamp = now;
        return;
    }
    if let Some(entry) = cache.iter_mut().find(|e| !e.used) {
        *entry = CacheEntry { ip, mac, stamp: now, used: true };
        return;
    }

    // Table full: the oldest gives up its place.
    let mut oldest = 0;
    for index in 1..CACHE_LEN {
        if cache[index].stamp < cache[oldest].stamp {
            oldest = index;
        }
    }
    cache[oldest] = CacheEntry { ip, mac, stamp: now, used: true };
}

/// Looks for an already known association.
pub fn lookup(ip: Ipv4) -> Option<Mac> {
    let cache = CACHE.lock();
    cache
        .iter()
        .find(|e| e.used && e.ip == ip)
        .map(|e| e.mac)
}

/// Builds and sends an ARP packet.
fn emit(operation: u16, target_ip: Ipv4, target_mac: Mac, destination: Mac) {
    let interface = super::interface();
    let mut packet = [0u8; PACKET];

    packet[0..2].copy_from_slice(&HTYPE_ETHERNET.to_be_bytes());
    packet[2..4].copy_from_slice(&PTYPE_IPV4.to_be_bytes());
    packet[4] = 6;
    packet[5] = 4;
    packet[6..8].copy_from_slice(&operation.to_be_bytes());
    packet[8..14].copy_from_slice(&interface.mac);
    packet[14..18].copy_from_slice(&interface.ip.0);
    packet[18..24].copy_from_slice(&target_mac);
    packet[24..28].copy_from_slice(&target_ip.0);

    let _ = eth::send(destination, eth::TYPE_ARP, &packet);
}

/// Handles a received ARP packet.
pub fn input(packet: &[u8]) {
    if packet.len() < PACKET {
        super::discard();
        return;
    }
    if u16::from_be_bytes([packet[0], packet[1]]) != HTYPE_ETHERNET
        || u16::from_be_bytes([packet[2], packet[3]]) != PTYPE_IPV4
        || packet[4] != 6
        || packet[5] != 4
    {
        super::discard();
        return;
    }

    let operation = u16::from_be_bytes([packet[6], packet[7]]);
    let sender_mac: Mac = match packet[8..14].try_into() {
        Ok(mac) => mac,
        Err(_) => return,
    };
    let sender_ip = Ipv4([packet[14], packet[15], packet[16], packet[17]]);
    let target_ip = Ipv4([packet[24], packet[25], packet[26], packet[27]]);

    remember(sender_ip, sender_mac);

    let interface = super::interface();
    if operation == OPER_REQUEST && interface.configured() && target_ip == interface.ip {
        emit(OPER_REPLY, sender_ip, sender_mac, sender_mac);
    }
}

/// Does the address name everybody?
///
/// Two forms: the general broadcast 255.255.255.255, and the **directed**
/// broadcast -- the link address whose host bits are all one, that is
/// 10.0.2.255 on a /24. Both resolve to an Ethernet broadcast; sending an
/// ARP request for them would have no recipient able to answer.
fn is_broadcast(ip: Ipv4, interface: &super::Interface) -> bool {
    if ip == Ipv4::BROADCAST {
        return true;
    }
    if interface.netmask.is_unspecified() || !interface.on_link(ip) {
        return false;
    }
    let host_bits = !interface.netmask.to_u32();
    (ip.to_u32() & host_bits) == host_bits
}

/// Resolves an IPv4 address into a hardware address, sending if needed.
pub fn resolve(ip: Ipv4) -> Option<Mac> {
    let interface = super::interface();
    if is_broadcast(ip, &interface) {
        return Some(MAC_BROADCAST);
    }

    if let Some(mac) = lookup(ip) {
        return Some(mac);
    }
    if !interface.configured() {
        return None;
    }

    for _ in 0..ATTEMPTS {
        emit(OPER_REQUEST, ip, [0; 6], MAC_BROADCAST);
        let _ = crate::syscall::invoke(
            crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
            [RETRY_MS, 0, 0, 0, 0, 0],
        );
        if let Some(mac) = lookup(ip) {
            return Some(mac);
        }
    }
    None
}

/// Hardware address of the next hop towards a destination.
///
/// On the link: the destination itself. Elsewhere: the gateway. With no
/// gateway configured the destination is unreachable -- and saying so is
/// worth more than sending to an address nobody will relay.
pub fn next_hop(destination: Ipv4) -> Option<Mac> {
    match hop_address(destination) {
        Some(target) => resolve(target),
        None => None,
    }
}

/// Same decision, but never sending a request nor waiting.
///
/// That is what everything answering from the driver task takes: see
/// `ip::send_cached`.
pub fn next_hop_cached(destination: Ipv4) -> Option<Mac> {
    let interface = super::interface();
    let target = hop_address(destination)?;
    if is_broadcast(target, &interface) {
        return Some(MAC_BROADCAST);
    }
    lookup(target)
}

/// Address whose hardware address must be known to reach the
/// destination: itself on the link, the gateway otherwise.
fn hop_address(destination: Ipv4) -> Option<Ipv4> {
    let interface = super::interface();
    if interface.on_link(destination) || destination == Ipv4::BROADCAST {
        Some(destination)
    } else if !interface.gateway.is_unspecified() {
        Some(interface.gateway)
    } else {
        None
    }
}

pub fn dump_state() {
    let cache = *CACHE.lock();
    let count = cache.iter().filter(|e| e.used).count();
    crate::serial_println!("[arp] {} entree(s) en cache", count);
    for entry in cache.iter().filter(|e| e.used) {
        crate::serial_println!(
            "[arp]   {} -> {}",
            entry.ip.text(),
            super::mac_text(&entry.mac)
        );
    }
}

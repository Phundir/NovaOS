//! Phase 6.2 -- TCP/IP stack.
//!
//! ## Where it lives
//!
//! Above the network card (`drivers::nic`), of which it knows only two things:
//! `transmit` to send, and the fact that it hands over every received
//! frame through `net::input`. No call in the other direction, no
//! register touched here. Replacing the card with another model would not
//! ask for a single line of this stack.
//!
//! ## What is implemented
//!
//! Ethernet II, ARP, IPv4, ICMP echo, UDP, TCP client, DHCP and DNS. It
//! is the minimal set that lets a new machine obtain an address, resolve
//! a name and open a connection -- in other words do something useful
//! with no manual configuration.
//!
//! ## What is not, and why
//!
//! * **No IP fragment reassembly.** A fragmented datagram is dropped. The
//!   path that counts -- DHCP, DNS, TCP with an announced MSS -- never
//!   fragments. Reassembling demands a fragment table with its deadlines,
//!   hence an attack surface, for a case nothing exercises.
//! * **No IPv6.** 6.6 does not mention it, and a second address family
//!   would double every table.
//! * **No TCP server.** `SYS_NET_CONNECT` is a client call. Listening
//!   would demand a book of half-open connections and the policy that
//!   goes with it.
//! * **One interface only.** The table is written to hold several --
//!   `IFACE_LIST` enumerates as many as the PCI bus shows -- but only one
//!   is configured and driven.
//!
//! ## Byte order
//!
//! Everything travelling on the network is big-endian, everything the
//! processor computes is little-endian. The conversion happens at
//! **serialisation**, never at storage: an address filed in a structure
//! is always in network order, as four bytes. Mixing the two conventions
//! produces bugs that only show on certain values -- a palindromic
//! address works.

pub mod arp;
pub mod bench;
pub mod dhcp;
pub mod dns;
pub mod eth;
pub mod icmp;
pub mod ip;
pub mod tcp;
pub mod sntp;
pub mod udp;

use alloc::format;
use alloc::string::String;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::fs::perm::Mode;
use crate::vga::SpinLock;

/// IPv4 address, always in network order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ipv4(pub [u8; 4]);

impl Ipv4 {
    pub const UNSPECIFIED: Ipv4 = Ipv4([0, 0, 0, 0]);
    pub const BROADCAST: Ipv4 = Ipv4([255, 255, 255, 255]);

    pub const fn from_u32(value: u32) -> Self {
        Ipv4(value.to_be_bytes())
    }

    pub const fn to_u32(self) -> u32 {
        u32::from_be_bytes(self.0)
    }

    pub fn is_unspecified(&self) -> bool {
        self.0 == [0, 0, 0, 0]
    }

    /// Dotted notation, in printable ASCII.
    pub fn text(&self) -> String {
        format!("{}.{}.{}.{}", self.0[0], self.0[1], self.0[2], self.0[3])
    }

    /// Parses a dotted notation. Returns `None` on any form that is not
    /// exactly four numbers from 0 to 255.
    pub fn parse(text: &str) -> Option<Ipv4> {
        let mut octets = [0u8; 4];
        let mut count = 0;
        for part in text.trim().split('.') {
            if count == 4 {
                return None;
            }
            octets[count] = part.parse::<u8>().ok()?;
            count += 1;
        }
        if count == 4 { Some(Ipv4(octets)) } else { None }
    }
}

/// Ethernet hardware address.
pub type Mac = [u8; 6];

/// Ethernet broadcast address.
pub const MAC_BROADCAST: Mac = [0xFF; 6];

/// Formats a hardware address.
pub fn mac_text(mac: &Mac) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

/// Maximum transmission unit of an Ethernet link.
pub const MTU: usize = 1500;

/// The system's name server: Cloudflare's public resolver.
///
/// Chosen over the one DHCP hands out (under QEMU, the emulator's relay
/// `10.0.2.3`): it answers the same everywhere, fast, and does not depend
/// on the network the machine happens to be on. A server set by hand in
/// the network settings still wins (`Interface::dns_chosen`).
pub const DNS_CLOUDFLARE: Ipv4 = Ipv4([1, 1, 1, 1]);
/// Cloudflare's second address, asked when the first does not answer.
pub const DNS_CLOUDFLARE_SECONDARY: Ipv4 = Ipv4([1, 0, 0, 1]);

/// Interface configuration.
#[derive(Debug, Clone, Copy)]
pub struct Interface {
    pub mac: Mac,
    pub ip: Ipv4,
    pub netmask: Ipv4,
    pub gateway: Ipv4,
    pub dns: Ipv4,
    /// Was `dns` set by hand? Then neither DHCP nor the default replaces it.
    pub dns_chosen: bool,
    /// Has the driver handed over a card?
    pub attached: bool,
    /// Does the address come from a DHCP lease?
    pub dynamic: bool,
    /// Lease duration, in seconds. Zero if the address is static.
    pub lease: u32,
}

impl Interface {
    const fn new() -> Self {
        Interface {
            mac: [0; 6],
            ip: Ipv4::UNSPECIFIED,
            netmask: Ipv4::UNSPECIFIED,
            gateway: Ipv4::UNSPECIFIED,
            dns: DNS_CLOUDFLARE,
            dns_chosen: false,
            attached: false,
            dynamic: false,
            lease: 0,
        }
    }

    /// Is the interface usable to send a datagram?
    pub fn configured(&self) -> bool {
        self.attached && !self.ip.is_unspecified()
    }

    /// Is an address on the same link as us?
    ///
    /// That is what decides whether the hardware address of the recipient
    /// or that of the gateway is resolved. Getting it wrong produces a
    /// system that reaches its neighbour and nothing else, or the reverse.
    pub fn on_link(&self, target: Ipv4) -> bool {
        let mask = self.netmask.to_u32();
        mask != 0 && (target.to_u32() & mask) == (self.ip.to_u32() & mask)
    }
}

static INTERFACE: SpinLock<Interface> = SpinLock::new(Interface::new());

static FRAMES_IN: AtomicU64 = AtomicU64::new(0);
static FRAMES_OUT: AtomicU64 = AtomicU64::new(0);
static BYTES_IN: AtomicU64 = AtomicU64::new(0);
static BYTES_OUT: AtomicU64 = AtomicU64::new(0);
static DISCARDED: AtomicU64 = AtomicU64::new(0);

/// Snapshot of the configuration.
pub fn interface() -> Interface {
    *INTERFACE.lock()
}

/// The driver announces a card. Called once, at boot.
pub fn attach(mac: Mac) {
    let mut interface = INTERFACE.lock();
    interface.mac = mac;
    interface.attached = true;
}

/// Sets a configuration. An unspecified `dns` means the system's default.
pub fn configure(ip: Ipv4, netmask: Ipv4, gateway: Ipv4, dns: Ipv4, dynamic: bool, lease: u32) {
    let mut interface = INTERFACE.lock();
    interface.ip = ip;
    interface.netmask = netmask;
    interface.gateway = gateway;
    interface.dns = if dns.is_unspecified() { DNS_CLOUDFLARE } else { dns };
    interface.dynamic = dynamic;
    interface.lease = lease;
}

/// Sets the name server by hand. Unspecified goes back to the default,
/// and to DHCP no longer having a say either way.
pub fn choose_dns(dns: Ipv4) {
    let mut interface = INTERFACE.lock();
    interface.dns_chosen = !dns.is_unspecified();
    interface.dns = if dns.is_unspecified() { DNS_CLOUDFLARE } else { dns };
}

/// The name servers to ask, in order: the interface's, then Cloudflare's
/// second address when it is not already the one.
pub fn dns_servers() -> [Option<Ipv4>; 2] {
    let first = interface().dns;
    let second = (first != DNS_CLOUDFLARE_SECONDARY).then_some(DNS_CLOUDFLARE_SECONDARY);
    [Some(first), second]
}

/// Counters: (frames in, out, bytes in, out, frames dropped).
/// trames ecartees).
pub fn stats() -> (u64, u64, u64, u64, u64) {
    (
        FRAMES_IN.load(Ordering::Relaxed),
        FRAMES_OUT.load(Ordering::Relaxed),
        BYTES_IN.load(Ordering::Relaxed),
        BYTES_OUT.load(Ordering::Relaxed),
        DISCARDED.load(Ordering::Relaxed),
    )
}

/// Counts a dropped frame -- invalid header, unknown protocol.
pub(crate) fn discard() {
    DISCARDED.fetch_add(1, Ordering::Relaxed);
}

/// Entry point of the received frames, called by the driver task.
pub fn input(frame: &[u8]) {
    FRAMES_IN.fetch_add(1, Ordering::Relaxed);
    BYTES_IN.fetch_add(frame.len() as u64, Ordering::Relaxed);
    eth::input(frame);
}

/// Sends a complete Ethernet frame.
pub(crate) fn output(frame: &[u8]) -> Result<(), &'static str> {
    crate::drivers::nic::transmit(frame)?;
    FRAMES_OUT.fetch_add(1, Ordering::Relaxed);
    BYTES_OUT.fetch_add(frame.len() as u64, Ordering::Relaxed);
    Ok(())
}

/// Internet checksum (RFC 1071).
///
/// Sum of the sixteen-bit big-endian words, with carry wrapped around,
/// then ones' complement. An odd trailing byte is completed by a zero **on
/// the right**: completing it on the left gives a checksum wrong by a
/// factor of 256, which goes unnoticed as long as the length is even --
/// that is to say during every easy test.
pub fn checksum(parts: &[&[u8]]) -> u16 {
    let mut sum: u32 = 0;
    let mut pending: Option<u8> = None;

    for part in parts {
        let mut bytes = *part;
        if let Some(high) = pending.take() {
            if let Some((first, rest)) = bytes.split_first() {
                sum += u16::from_be_bytes([high, *first]) as u32;
                bytes = rest;
            } else {
                pending = Some(high);
                continue;
            }
        }
        let mut index = 0;
        while index + 1 < bytes.len() {
            sum += u16::from_be_bytes([bytes[index], bytes[index + 1]]) as u32;
            index += 2;
        }
        if index < bytes.len() {
            pending = Some(bytes[index]);
        }
    }

    if let Some(high) = pending {
        sum += u16::from_be_bytes([high, 0]) as u32;
    }

    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

// ============================================================
//  6.2.2 -- Persistent configuration
// ============================================================

/// Interface configuration file (spec 17: `/net/`).
pub const CONFIG_PATH: &str = "/net/interface.cfg";

/// Writes the current configuration into `/net/`.
///
/// The format is one line per field, `key=value`, in ASCII: it must stay
/// readable and fixable by hand from a rescue shell, before any
/// configuration tool exists at all.
pub fn save_config() -> Result<(), crate::fs::FsError> {
    if !crate::fs::is_ready() {
        return Err(crate::fs::FsError::NotMounted);
    }
    let interface = interface();
    let content = format!(
        "# configuration reseau - ecrite par la pile 6.2\n\
         mac={}\n\
         ip={}\n\
         netmask={}\n\
         gateway={}\n\
         dns={}\n\
         dns-choisi={}\n\
         source={}\n\
         lease={}\n",
        mac_text(&interface.mac),
        interface.ip.text(),
        interface.netmask.text(),
        interface.gateway.text(),
        interface.dns.text(),
        if interface.dns_chosen { "oui" } else { "non" },
        if interface.dynamic { "dhcp" } else { "statique" },
        interface.lease
    );

    let who = crate::fs::perm::Credentials::kernel();
    let now = crate::idt::ticks();
    // The file may already exist: the write replaces, it does not
    // duplicate.
    let _ = crate::fs::delete(CONFIG_PATH, who, now);
    // Private file: the network configuration names the gateway and the
    // DNS server, which describe the network the machine is on.
    crate::fs::write_file(CONFIG_PATH, content.as_bytes(), who, Mode::private_file(), now)
        .map(|_| ())
}

/// Reads back a static configuration left in `/net/`.
///
/// A name server chosen by hand (`dns-choisi=oui`) is taken back in every
/// case, DHCP or not. One written without that mark is the one an earlier
/// DHCP lease gave, and is ignored: the default applies.
///
/// Returns `true` if an address was there. A static configuration wins
/// over DHCP: it is an explicit choice by the administrator, and a lease
/// obtained afterwards would overwrite it without being asked.
pub fn load_config() -> bool {
    if !crate::fs::is_ready() {
        return false;
    }
    let who = crate::fs::perm::Credentials::kernel();
    let Ok(bytes) = crate::fs::read_file(CONFIG_PATH, who) else {
        return false;
    };
    let Ok(text) = core::str::from_utf8(&bytes) else {
        return false;
    };

    let mut ip = Ipv4::UNSPECIFIED;
    let mut netmask = Ipv4([255, 255, 255, 0]);
    let mut gateway = Ipv4::UNSPECIFIED;
    let mut dns = Ipv4::UNSPECIFIED;
    let mut dns_chosen = false;
    let mut dynamic = true;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "ip" => ip = Ipv4::parse(value).unwrap_or(Ipv4::UNSPECIFIED),
            "netmask" => netmask = Ipv4::parse(value).unwrap_or(netmask),
            "gateway" => gateway = Ipv4::parse(value).unwrap_or(Ipv4::UNSPECIFIED),
            "dns" => dns = Ipv4::parse(value).unwrap_or(Ipv4::UNSPECIFIED),
            "dns-choisi" => dns_chosen = value.trim() == "oui",
            "source" => dynamic = value.trim() != "statique",
            _ => {}
        }
    }

    if dns_chosen {
        choose_dns(dns);
    }
    if ip.is_unspecified() || dynamic {
        return false;
    }
    configure(ip, netmask, gateway, if dns_chosen { dns } else { Ipv4::UNSPECIFIED }, false, 0);
    true
}

// ============================================================
//  Diagnostics
// ============================================================

pub fn dump_state() {
    let interface = interface();
    let (frames_in, frames_out, bytes_in, bytes_out, discarded) = stats();

    if !interface.attached {
        crate::serial_println!("[net] aucune carte attachee");
        return;
    }

    crate::serial_println!(
        "[net] {} - {} / masque {} - passerelle {} - DNS {}{} ({})",
        mac_text(&interface.mac),
        interface.ip.text(),
        interface.netmask.text(),
        interface.gateway.text(),
        interface.dns.text(),
        if interface.dns_chosen { " choisi" } else { "" },
        if interface.dynamic { "DHCP" } else { "statique" }
    );
    crate::serial_println!(
        "[net] {} trames recues ({} o), {} emises ({} o), {} ecartees",
        frames_in,
        bytes_in,
        frames_out,
        bytes_out,
        discarded
    );
    arp::dump_state();
    udp::dump_state();
    tcp::dump_state();
}

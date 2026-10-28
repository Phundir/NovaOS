//! 5.6.4 and 6.2 -- Domain 0x08, network (spec 6.6).
//!
//! ## Two steps, one domain
//!
//! Phase 5 had laid down the table and the access rule: a binary only
//! reaches the network if it has **declared it in its header**
//! (`flags::NETWORK`). The transport calls existed and refused with
//! `UNSUPPORTED`, for want of a stack.
//!
//! Phase 6.2 supplied the stack. These calls now really join it:
//! `CONNECT` opens a TCP connection, `RESOLVE` queries the DNS server
//! obtained by DHCP, `STATUS` says whether the interface is configured.
//! The access rule has not moved a line -- and that is the sign it was in
//! the right place.
//!
//! ## Order of the checks
//!
//! The header declaration is checked **before** the state of the stack: an
//! undeclared program must receive `PERMISSION_DENIED`, not
//! `UNSUPPORTED`. The two diagnoses do not call for the same fix --
//! change the header, or plug in a cable.
//!
//! ## `CLOSE`, outside the specification
//!
//! 6.6 lists no close. Without it, an open connection would never be
//! given back: the table counts eight, and a program looping on `CONNECT`
//! would exhaust them in eight laps. The spec describes an interface, not
//! a resource management; the addition follows the same logic as
//! `SYS_PROC_EXIT`.

use alloc::vec::Vec;

use super::Call;
use super::error::{self, ErrorCode, SysError, SysResult};
use super::table::{Entry, entry};
use crate::binfmt::header::flags;
use crate::net::{self, Ipv4, dns, tcp};

pub const SYS_NET_STATUS: u16 = 0x0801;
pub const SYS_NET_IFACE_LIST: u16 = 0x0802;
pub const SYS_NET_CONNECT: u16 = 0x0803;
pub const SYS_NET_SEND: u16 = 0x0804;
pub const SYS_NET_RECV: u16 = 0x0805;
pub const SYS_NET_RESOLVE: u16 = 0x0806;
pub const SYS_NET_CLOSE: u16 = 0x0807;

pub static TABLE: &[Entry] = &[
    entry!(SYS_NET_STATUS, "STATUS", Sync, status),
    entry!(SYS_NET_IFACE_LIST, "IFACE_LIST", Sync, iface_list),
    entry!(SYS_NET_CONNECT, "CONNECT", Async, connect),
    entry!(SYS_NET_SEND, "SEND", Async, send),
    entry!(SYS_NET_RECV, "RECV", Async, recv),
    entry!(SYS_NET_RESOLVE, "RESOLVE", Async, resolve),
    entry!(SYS_NET_CLOSE, "CLOSE", Sync, close),
];

/// PCI class of a network controller.
const PCI_CLASS_NETWORK: u8 = 0x02;

/// Maximum length of a host name accepted.
const HOST_MAX: u64 = 253;

/// Default deadline of an operation, in milliseconds.
const DEFAULT_TIMEOUT_MS: u64 = 5_000;

/// Maximum deadline accepted: one minute.
///
/// With no cap, a program would ask for a wait of several hours and its
/// core would serve nothing but waiting for it.
const MAX_TIMEOUT_MS: u64 = 60_000;

/// Check of the header declaration.
///
/// The check bears on the flags of the binary, read at launch and kept by
/// the scheduler. A program therefore cannot declare itself a network
/// user while running: the declaration was made once and for all, in a
/// signed file.
fn declared(call: &Call) -> SysResult<()> {
    if call.origin == super::Origin::Kernel {
        return Ok(());
    }
    match crate::scheduler::task_info(call.pid) {
        Some(info) if info.app_flags & flags::NETWORK != 0 => Ok(()),
        Some(_) => Err(error::denied(
            "le binaire n'a pas declare l'acces reseau dans son en-tete",
        )),
        None => Err(error::missing("tache appelante")),
    }
}

/// Network cards seen by the PCI scan.
fn adapters() -> Vec<crate::pci::Device> {
    crate::pci::enumerate()
        .into_iter()
        .filter(|device| device.class == PCI_CLASS_NETWORK)
        .collect()
}

fn timeout_of(raw: u64) -> u64 {
    if raw == 0 {
        DEFAULT_TIMEOUT_MS
    } else {
        raw.min(MAX_TIMEOUT_MS)
    }
}

/// Layout of the network state.
///
/// ```text
///   0x00  4  interfaces detected
///   0x04  1  network stack operational
///   0x05  1  the caller declared the network
///   0x06  1  the physical link is established
///   0x07  1  the address comes from a DHCP lease
///   0x08  8  bytes sent
///   0x10  8  bytes received
///   0x18  4  IPv4 address
///   0x1C  4  mask
///   0x20  4  gateway
///   0x24  4  DNS server
/// ```
pub const NET_STATUS_SIZE: usize = 0x28;

fn status(call: &mut Call) -> SysResult<u64> {
    // `STATUS` is the only call of the domain that answers with no
    // declaration: a program must be able to establish that it has no
    // right, and a settings manager must be able to display the state of
    // the network without declaring itself a network client.
    let count = adapters().len();
    let allowed = declared(call).is_ok();
    let interface = net::interface();
    let (_, _, bytes_in, bytes_out, _) = net::stats();

    let mut record = [0u8; NET_STATUS_SIZE];
    record[0x00..0x04].copy_from_slice(&(count as u32).to_le_bytes());
    record[0x04] = u8::from(interface.configured());
    record[0x05] = u8::from(allowed);
    record[0x06] = u8::from(crate::drivers::nic::link_up());
    record[0x07] = u8::from(interface.dynamic);
    record[0x08..0x10].copy_from_slice(&bytes_out.to_le_bytes());
    record[0x10..0x18].copy_from_slice(&bytes_in.to_le_bytes());
    record[0x18..0x1C].copy_from_slice(&interface.ip.0);
    record[0x1C..0x20].copy_from_slice(&interface.netmask.0);
    record[0x20..0x24].copy_from_slice(&interface.gateway.0);
    record[0x24..0x28].copy_from_slice(&interface.dns.0);

    call.write_bytes(call.arg(0), call.arg(1), &record)?;
    Ok(count as u64)
}

/// Layout of an interface descriptor.
///
/// ```text
///   0x00  2  vendor identifier
///   0x02  2  product identifier
///   0x04  1  bus
///   0x05  1  device
///   0x06  1  function
///   0x07  1  driver loaded
///   0x08  6  hardware address, when it is known
///   0x0E  2  reserved
/// ```
pub const IFACE_ENTRY_SIZE: usize = 0x10;

fn iface_list(call: &mut Call) -> SysResult<u64> {
    declared(call)?;

    let devices = adapters();
    let capacity = (call.arg(1) as usize) / IFACE_ENTRY_SIZE;
    let count = devices.len().min(capacity);
    let driven = crate::drivers::nic::mac();

    let mut buffer = Vec::with_capacity(count * IFACE_ENTRY_SIZE);
    for device in devices.iter().take(count) {
        let mut record = [0u8; IFACE_ENTRY_SIZE];
        record[0x00..0x02].copy_from_slice(&device.vendor_id.to_le_bytes());
        record[0x02..0x04].copy_from_slice(&device.device_id.to_le_bytes());
        record[0x04] = device.address.bus;
        record[0x05] = device.address.device;
        record[0x06] = device.address.function;
        // Only one card is driven: the others are seen and not served,
        // and the field says so rather than letting it be believed.
        if let Some(mac) = driven {
            record[0x07] = 1;
            record[0x08..0x0E].copy_from_slice(&mac);
        }
        buffer.extend_from_slice(&record);
    }

    call.write_bytes(call.arg(0), call.arg(1), &buffer)?;
    Ok(devices.len() as u64)
}

/// Translates a stack error into a system error.
///
/// A string is more precise than a code: `TIMED_OUT` does not exist in
/// the 6.1 table, and making one code per network failure cause would
/// inflate it for no gain. The code says the category, the context says
/// the cause.
fn from_stack(reason: &'static str) -> SysError {
    SysError::with(ErrorCode::Unsupported, reason)
}

/// `CONNECT(name, length, port, deadline)` -- opens a TCP connection.
///
/// The name may be a dotted address or a domain name: the distinction is
/// made by the resolution, which returns a literal address immediately
/// without querying anybody.
fn connect(call: &mut Call) -> SysResult<u64> {
    declared(call)?;
    call.require_async("CONNECT")?;

    let host = call.read_str(call.arg(0), call.arg(1).min(HOST_MAX))?;
    let port = call.arg(2);
    if port == 0 || port > u16::MAX as u64 {
        return Err(error::invalid("numero de port"));
    }
    let timeout = timeout_of(call.arg(3));

    if !net::interface().configured() {
        return Err(from_stack("interface sans adresse IP"));
    }

    let address = dns::resolve(&host).map_err(from_stack)?;
    super::count_block();
    let handle = tcp::connect(address, port as u16, timeout).map_err(from_stack)?;
    Ok(handle.index() as u64)
}

/// Finds a connection from the identifier handed to ring 3.
///
/// A forged identifier names at worst a closed connection: the table is
/// a fixed-size array, and a free entry returns `Closed`. So there is no
/// way to reach the connection of another task through a number chosen at
/// random -- but there is no owner per connection either, which remains
/// to be done the day several ring 3 applications open connections.
fn connection(value: u64) -> SysResult<tcp::Handle> {
    tcp::Handle::from_index(value as usize).ok_or_else(|| error::invalid("identifiant de connexion"))
}

/// `SEND(connection, buffer, length)`.
fn send(call: &mut Call) -> SysResult<u64> {
    declared(call)?;
    call.require_async("SEND")?;

    let handle = connection(call.arg(0))?;
    let payload = call.read_bytes(call.arg(1), call.arg(2))?;
    if payload.is_empty() {
        return Ok(0);
    }
    if tcp::state(handle) == tcp::State::Closed {
        return Err(error::missing("connexion"));
    }

    super::count_block();
    let written = tcp::send(handle, &payload).map_err(from_stack)?;
    Ok(written as u64)
}

/// `RECV(connection, buffer, size, deadline)`.
fn recv(call: &mut Call) -> SysResult<u64> {
    declared(call)?;
    call.require_async("RECV")?;

    let handle = connection(call.arg(0))?;
    let capacity = call.arg(2).min(super::TRANSFER_MAX) as usize;
    if capacity == 0 {
        return Err(error::invalid("taille de tampon"));
    }
    if tcp::state(handle) == tcp::State::Closed {
        return Err(error::missing("connexion"));
    }

    super::count_block();
    let data = tcp::recv(handle, capacity, timeout_of(call.arg(3))).map_err(from_stack)?;
    call.write_bytes(call.arg(1), call.arg(2), &data)
}

/// `RESOLVE(name, length, buffer, size)` -- four bytes on output.
fn resolve(call: &mut Call) -> SysResult<u64> {
    declared(call)?;
    call.require_async("RESOLVE")?;

    let name = call.read_str(call.arg(0), call.arg(1).min(HOST_MAX))?;
    super::count_block();
    let address: Ipv4 = dns::resolve(&name).map_err(from_stack)?;
    call.write_bytes(call.arg(2), call.arg(3), &address.0)?;
    Ok(address.to_u32() as u64)
}

/// `CLOSE(connection)` -- outside the spec, see the module header.
fn close(call: &mut Call) -> SysResult<u64> {
    declared(call)?;
    let handle = connection(call.arg(0))?;
    tcp::close(handle);
    Ok(0)
}

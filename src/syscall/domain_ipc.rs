//! 5.5 -- Domain 0x04, inter-process communication (spec 6.5).
//!
//! ## A message is not a run of bytes
//!
//! The phase 2 mechanism returned only the payload to ring 3: the
//! recipient received bytes without knowing from whom, or why. Every call
//! of this domain therefore carries a sixteen-byte header in front of the
//! payload -- sender, label, nature -- because a request and answer
//! protocol is impossible to write without knowing whom to answer.
//!
//! ## Blocking reception
//!
//! `RECV` accepts a wait flag. It is the second blocking call of the
//! system, with `SYS_PROC_WAIT`, and the only one whose caller chooses
//! the behaviour: without the flag, an empty queue returns `WOULD_BLOCK`;
//! with it, the task goes to sleep and the sender wakes it. Both forms
//! are useful -- a service loop wants to wait, a render loop wants to
//! know at once.
//!
//! ## Request and answer (6.5, `QUERY` / `RESPOND`)
//!
//! A `.dsa` exposing an API opens a named channel; a client resolves it
//! by that name and addresses a request to it carrying an identifier made
//! by the kernel. The server answers reusing that identifier as a label.
//! The kernel knows nothing of the content: it only guarantees that two
//! simultaneous requests do not get confused, which neither the sender
//! nor the recipient can guarantee alone.
//!
//! ## Internal ports (12.2, step 6.4.1)
//!
//! Four calls outside the 6.5 table, because 12.2 describes a mechanism
//! without giving it numbers. They are filed after the domain they belong
//! to: an internal port is IPC, with a negotiation in front.
//! The negotiation is what tells them from `SEND`: `SEND` fails at once
//! between two different groups, `PORT_CONNECT` asks -- the user first
//! (12.5), the receiving application next -- and the accepted link
//! becomes the authorisation `SEND` will consult.

use core::sync::atomic::{AtomicU64, Ordering};

use super::error::{self, ErrorCode, SysError, SysResult};
use super::table::{Entry, entry};
use super::Call;
use crate::ipc::{self, Message, MessageKind};

pub const SYS_IPC_SEND: u16 = 0x0401;
pub const SYS_IPC_RECV: u16 = 0x0402;
pub const SYS_IPC_OPEN: u16 = 0x0403;
pub const SYS_IPC_CLOSE: u16 = 0x0404;
pub const SYS_IPC_SUBSCRIBE: u16 = 0x0405;
pub const SYS_IPC_PUBLISH: u16 = 0x0406;
pub const SYS_IPC_QUERY: u16 = 0x0407;
pub const SYS_IPC_RESPOND: u16 = 0x0408;
pub const SYS_IPC_PORT_BIND: u16 = 0x0409;
pub const SYS_IPC_PORT_CONNECT: u16 = 0x040A;
pub const SYS_IPC_PORT_ACCEPT: u16 = 0x040B;
pub const SYS_IPC_PORT_CLOSE: u16 = 0x040C;

pub static TABLE: &[Entry] = &[
    entry!(SYS_IPC_SEND, "SEND", Async, send),
    entry!(SYS_IPC_RECV, "RECV", Async, recv),
    entry!(SYS_IPC_OPEN, "OPEN", Sync, open),
    entry!(SYS_IPC_CLOSE, "CLOSE", Sync, close),
    entry!(SYS_IPC_SUBSCRIBE, "SUBSCRIBE", Sync, subscribe),
    entry!(SYS_IPC_PUBLISH, "PUBLISH", Async, publish),
    entry!(SYS_IPC_QUERY, "QUERY", Sync, query),
    entry!(SYS_IPC_RESPOND, "RESPOND", Async, respond),
    entry!(SYS_IPC_PORT_BIND, "PORT_BIND", Sync, port_bind),
    entry!(SYS_IPC_PORT_CONNECT, "PORT_CONNECT", Async, port_connect),
    entry!(SYS_IPC_PORT_ACCEPT, "PORT_ACCEPT", Sync, port_accept),
    entry!(SYS_IPC_PORT_CLOSE, "PORT_CLOSE", Sync, port_close),
];

/// Length of the header placed in front of every payload returned.
///
/// ```text
///   0x00  8  PID of the sender
///   0x08  4  label
///   0x0C  1  nature: 0 data, 1 request, 2 answer, 3 event, 4 signal
///   0x0D  1  reserved
///   0x0E  2  length of the payload
/// ```
pub const MESSAGE_HEADER: usize = 16;

/// Maximum length of a channel name.
const NAME_MAX: u64 = 64;

/// Flag of `RECV`: wait rather than return `WOULD_BLOCK`.
const RECV_BLOCKING: u64 = 1 << 0;

/// Counter of request identifiers.
///
/// Global and monotonic: two different clients must never make the same
/// label, failing which a server would answer the wrong party. A counter
/// per client would not be enough.
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

fn kind_code(kind: MessageKind) -> u8 {
    match kind {
        MessageKind::Data => 0,
        MessageKind::Request => 1,
        MessageKind::Response => 2,
        MessageKind::Event => 3,
        MessageKind::Signal => 4,
    }
}

/// Copies a message into the space of the caller, header included.
fn deliver(call: &Call, message: &Message) -> SysResult<u64> {
    let mut buffer = alloc::vec![0u8; MESSAGE_HEADER + message.payload.len()];
    buffer[0x00..0x08].copy_from_slice(&message.sender.to_le_bytes());
    buffer[0x08..0x0C].copy_from_slice(&message.tag.to_le_bytes());
    buffer[0x0C] = kind_code(message.kind);
    buffer[0x0E..0x10].copy_from_slice(&(message.payload.len() as u16).to_le_bytes());
    buffer[MESSAGE_HEADER..].copy_from_slice(&message.payload);

    call.write_bytes(call.arg(0), call.arg(1), &buffer)
}

// ============================================================
//  5.5.1 -- SEND / RECV
// ============================================================

/// `SEND(pid, buffer, length, label)`.
fn send(call: &mut Call) -> SysResult<u64> {
    let target = call.arg(0);
    let payload = call.read_bytes(call.arg(1), call.arg(2))?;
    if payload.len() > ipc::MAX_PAYLOAD {
        return Err(SysError::with(ErrorCode::TooLarge, "charge utile"));
    }

    let message = Message::new(call.pid, MessageKind::Data, call.arg(3) as u32, &payload);
    ipc::send(target, message).map_err(|e| error::from_ipc(e, "SEND"))?;
    Ok(payload.len() as u64)
}

/// `RECV(buffer, size, flags)`.
///
/// The buffer is validated **before** anything is removed from the queue:
/// a faulty pointer must not cost a message.
fn recv(call: &mut Call) -> SysResult<u64> {
    let size = call.arg(1);
    if size < MESSAGE_HEADER as u64 {
        return Err(error::invalid("tampon trop court pour l'en-tete"));
    }
    // A dry-run validation on the header is enough to discard an invalid
    // pointer; the rest will be checked by the real write.
    call.write_bytes(call.arg(0), 0, &[])?;

    let blocking = call.arg(2) & RECV_BLOCKING != 0;

    if let Some(message) = ipc::try_recv() {
        return deliver(call, &message);
    }

    if !blocking {
        return Err(SysError::with(ErrorCode::WouldBlock, "boite vide"));
    }

    call.require_async("RECV bloquant")?;
    super::count_block();

    // `ipc::recv` puts the task to sleep and only hands back with a message.
    let message = ipc::recv();
    deliver(call, &message)
}

// ============================================================
//  5.5.2 -- OPEN / CLOSE
// ============================================================

/// `OPEN(name, length)` -- creates a named channel, returns its identifier.
///
/// It is what closes the limit opened by phase 3: a ring 3 program had
/// only PIDs, whose order depended on the number of cores and of binaries
/// loaded. A name, on the other hand, does not move.
fn open(call: &mut Call) -> SysResult<u64> {
    let name = call.read_str(call.arg(0), call.arg(1).min(NAME_MAX))?;
    if name.is_empty() {
        return Err(error::invalid("nom de canal vide"));
    }
    ipc::open_channel(&name).map_err(|e| error::from_ipc(e, &name))
}

/// `CLOSE(id)` -- reserved to the owner of the channel.
fn close(call: &mut Call) -> SysResult<u64> {
    ipc::close_channel(call.arg(0))
        .map_err(|e| error::from_ipc(e, "CLOSE"))
        .map(|_| 0)
}

// ============================================================
//  5.5.3 -- SUBSCRIBE / PUBLISH
// ============================================================

/// `SUBSCRIBE(name, length)` -- subscribes by name, returns the identifier.
///
/// By name and not by identifier: a subscriber has no reason to know the
/// number a channel opened by somebody else received.
fn subscribe(call: &mut Call) -> SysResult<u64> {
    let name = call.read_str(call.arg(0), call.arg(1).min(NAME_MAX))?;
    let id = ipc::find_channel(&name).ok_or_else(|| error::missing(&name))?;
    ipc::subscribe(id).map_err(|e| error::from_ipc(e, &name))?;
    Ok(id)
}

/// `PUBLISH(id, buffer, length, label)` -- returns the number of
/// subscribers served.
fn publish(call: &mut Call) -> SysResult<u64> {
    let payload = call.read_bytes(call.arg(1), call.arg(2))?;
    if payload.len() > ipc::MAX_PAYLOAD {
        return Err(SysError::with(ErrorCode::TooLarge, "charge utile"));
    }
    let message = Message::new(call.pid, MessageKind::Event, call.arg(3) as u32, &payload);
    ipc::publish(call.arg(0), message)
        .map(|count| count as u64)
        .map_err(|e| error::from_ipc(e, "PUBLISH"))
}

// ============================================================
//  5.5.4 -- QUERY / RESPOND
// ============================================================

/// `QUERY(name, length, buffer, length)` -- queries an exposed API.
///
/// Returns the request identifier, which the client will find again as
/// the label of the answer. The call is **synchronous** in the sense of
/// the spec: it drops the request and hands back. Waiting for the answer
/// is a `RECV`, which leaves the client free to do something else
/// meanwhile -- a call that waited by default would forbid any concurrent
/// request.
fn query(call: &mut Call) -> SysResult<u64> {
    let name = call.read_str(call.arg(0), call.arg(1).min(NAME_MAX))?;
    let payload = call.read_bytes(call.arg(2), call.arg(3))?;
    if payload.len() > ipc::MAX_PAYLOAD {
        return Err(SysError::with(ErrorCode::TooLarge, "charge utile"));
    }

    let owner = ipc::channel_owner(&name).ok_or_else(|| error::missing(&name))?;
    if owner == call.pid {
        return Err(error::invalid("une API ne s'interroge pas elle-meme"));
    }

    let request = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
    let message = Message::new(call.pid, MessageKind::Request, request as u32, &payload);
    ipc::send(owner, message).map_err(|e| error::from_ipc(e, &name))?;
    Ok(request)
}

/// `RESPOND(pid, buffer, length, identifier)`.
fn respond(call: &mut Call) -> SysResult<u64> {
    let payload = call.read_bytes(call.arg(1), call.arg(2))?;
    if payload.len() > ipc::MAX_PAYLOAD {
        return Err(SysError::with(ErrorCode::TooLarge, "charge utile"));
    }
    let message = Message::new(
        call.pid,
        MessageKind::Response,
        call.arg(3) as u32,
        &payload,
    );
    ipc::send(call.arg(0), message).map_err(|e| error::from_ipc(e, "RESPOND"))?;
    Ok(payload.len() as u64)
}

// ============================================================
//  6.4.1 -- Internal ports (spec 12.2)
// ============================================================

/// Maximum length of an application identifier.
const APP_ID_MAX: u64 = 64;

/// Translates a port error into a system error.
///
/// The refusals fall into three categories the caller must be able to
/// tell apart: what does not exist, what is already taken, and what is
/// refused to it. Merging them into one code would leave a client unable
/// to know whether to correct its call or give up.
fn from_port(error: crate::ports::PortError) -> SysError {
    use crate::ports::PortError;

    let code = match error {
        PortError::AlreadyBound => ErrorCode::AlreadyExists,
        PortError::Full => ErrorCode::QueueFull,
        PortError::NoSuchApp | PortError::NoSuchPort | PortError::NoSuchLink => ErrorCode::NotFound,
        PortError::Blocked | PortError::DeniedByUser | PortError::DeniedByApp => {
            ErrorCode::PermissionDenied
        }
        PortError::Timeout => ErrorCode::WouldBlock,
        PortError::NotAccepted => ErrorCode::Busy,
    };
    SysError::with(code, error.as_str())
}

/// `PORT_BIND(number)` -- declares a port for the caller.
fn port_bind(call: &mut Call) -> SysResult<u64> {
    let number = call.arg(0);
    if number == 0 || number > u16::MAX as u64 {
        return Err(error::invalid("numero de port"));
    }
    crate::ports::bind(number as u16).map_err(from_port)?;
    Ok(number)
}

/// `PORT_CONNECT(identifier, length, number)` -- opens a link.
///
/// Asynchronous: the negotiation waits for the user then for the
/// receiving application, which may take several seconds.
fn port_connect(call: &mut Call) -> SysResult<u64> {
    call.require_async("PORT_CONNECT")?;

    let app_id = call.read_str(call.arg(0), call.arg(1).min(APP_ID_MAX))?;
    let number = call.arg(2);
    if number == 0 || number > u16::MAX as u64 {
        return Err(error::invalid("numero de port"));
    }

    super::count_block();
    let link = crate::ports::connect(&app_id, number as u16).map_err(from_port)?;
    Ok(link)
}

/// `PORT_ACCEPT(link, decision)` -- the server makes up its mind.
///
/// A null decision refuses. It is the sense that protects: a caller
/// getting its argument wrong refuses the connection instead of granting it.
fn port_accept(call: &mut Call) -> SysResult<u64> {
    let link = call.arg(0);
    let accept = call.arg(1) != 0;

    if accept {
        crate::ports::accept(link).map_err(from_port)?;
    } else {
        crate::ports::refuse(link).map_err(from_port)?;
    }
    Ok(u64::from(accept))
}

/// `PORT_CLOSE(link)` -- closes a link on both sides.
fn port_close(call: &mut Call) -> SysResult<u64> {
    let link = call.arg(0);
    if crate::ports::state(link).is_none() {
        return Err(error::missing("lien"));
    }
    crate::ports::close(link);
    Ok(0)
}

//! 6.4.1 -- Internal application ports (spec 12.2).
//!
//! ## What 12.2 describes
//!
//! "Internal ports are channels isolated from the OS network, dedicated
//! to one app. Two apps may use the same number without conflict --
//! never mapped outwards. The receiving app receives the request + the
//! complete AppIdentity and decides to accept or refuse."
//!
//! Three properties follow, and each translates into a structural
//! decision:
//!
//! 1. **Two apps may carry the same number.** A port is therefore not a
//!    global key: it only makes sense paired with the application holding
//!    it. The table is indexed by (owner PID, number), and a connection
//!    request names *one application by its identifier*, not a number
//!    floating in a common space. That is exactly the opposite of a TCP
//!    port.
//! 2. **Never mapped outwards.** Nothing in this module touches `net`:
//!    there is no path, however indirect, between an internal port and a
//!    frame. This is not a run-time precaution, it is an absence of code.
//! 3. **The receiving app decides.** A connection is born `EnAttente` and
//!    carries nothing until the server has accepted it. Refusal is a
//!    state, not an absence of answer: a client must be able to tell
//!    "refused" from "has not answered yet".
//!
//! ## The prompt-then-application order
//!
//! 12.1 demands, for two different groups, "receiving app
//! accepts/refuses + user prompt". The prompt comes **first**:
//! disturbing the user for a connection the application would have
//! refused anyway is pointless, but the reverse is worse -- waking an
//! application for a request the user is going to reject teaches it who
//! is trying to talk to it, which is already a leak. A remembered choice
//! makes the prompt free, which removes all cost from this order.
//!
//! ## What stays open
//!
//! No rate limit per link. An accepted client can saturate the server's
//! mailbox -- the IPC queue protects it by refusing, but nothing
//! penalises the sender. A per-link quota will come with resource
//! accounting.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::ipc::{Message, MessageKind};
use crate::process::Pid;
use crate::vga::SpinLock;

/// Number of ports declared at the same time.
const MAX_PORTS: usize = 16;

/// Number of links, established or pending.
const MAX_LINKS: usize = 16;

/// Time left to the receiving application to make up its mind.
const ACCEPT_TIMEOUT_MS: u64 = 5_000;

/// State of a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    /// The server has not answered yet.
    Pending,
    Accepted,
    Refused,
    Closed,
}

impl LinkState {
    pub fn as_str(&self) -> &'static str {
        match self {
            LinkState::Pending => "en attente",
            LinkState::Accepted => "accepte",
            LinkState::Refused => "refuse",
            LinkState::Closed => "ferme",
        }
    }
}

#[derive(Clone, Copy)]
struct Port {
    used: bool,
    owner: Pid,
    number: u16,
}

#[derive(Clone)]
struct Link {
    used: bool,
    id: u64,
    client: Pid,
    server: Pid,
    port: u16,
    state: LinkState,
    /// Identifier of the client application, as the server sees it.
    client_app: String,
    messages: u64,
}

struct Registry {
    ports: [Port; MAX_PORTS],
    links: Vec<Link>,
    next_id: u64,
}

static REGISTRY: SpinLock<Registry> = SpinLock::new(Registry {
    ports: [Port {
        used: false,
        owner: 0,
        number: 0,
    }; MAX_PORTS],
    links: Vec::new(),
    next_id: 1,
});

static REFUSED: AtomicU64 = AtomicU64::new(0);
static ACCEPTED: AtomicU64 = AtomicU64::new(0);

/// Failure causes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortError {
    /// The number is already taken by this same application.
    AlreadyBound,
    /// No room left in the table.
    Full,
    /// No application carries that identifier.
    NoSuchApp,
    /// The target application does not listen on that number.
    NoSuchPort,
    /// 12.1 forbids all communication between those two.
    Blocked,
    /// The user refused.
    DeniedByUser,
    /// The receiving application refused.
    DeniedByApp,
    /// The server did not answer in time.
    Timeout,
    /// The link does not exist, or no longer does.
    NoSuchLink,
    /// The link is not accepted: nothing can travel through it.
    NotAccepted,
}

impl PortError {
    pub fn as_str(&self) -> &'static str {
        match self {
            PortError::AlreadyBound => "numero de port deja declare par cette application",
            PortError::Full => "table des ports pleine",
            PortError::NoSuchApp => "aucune application sous cet identifiant",
            PortError::NoSuchPort => "l'application n'ecoute pas ce port",
            PortError::Blocked => "communication interdite par la spec 12.1",
            PortError::DeniedByUser => "l'utilisateur a refuse la connexion",
            PortError::DeniedByApp => "l'application receptrice a refuse",
            PortError::Timeout => "l'application receptrice n'a pas repondu",
            PortError::NoSuchLink => "lien inconnu",
            PortError::NotAccepted => "lien non accepte",
        }
    }
}

/// Declares a port for the current task.
pub fn bind(number: u16) -> Result<(), PortError> {
    let owner = crate::scheduler::current_pid();
    let mut registry = REGISTRY.lock();

    if registry
        .ports
        .iter()
        .any(|p| p.used && p.owner == owner && p.number == number)
    {
        return Err(PortError::AlreadyBound);
    }

    let slot = registry
        .ports
        .iter_mut()
        .find(|p| !p.used)
        .ok_or(PortError::Full)?;
    *slot = Port {
        used: true,
        owner,
        number,
    };
    Ok(())
}

/// Removes a port and closes the links attached to it.
pub fn unbind(number: u16) {
    let owner = crate::scheduler::current_pid();
    let mut registry = REGISTRY.lock();

    for port in registry.ports.iter_mut() {
        if port.used && port.owner == owner && port.number == number {
            port.used = false;
        }
    }
    for link in registry.links.iter_mut() {
        if link.used && link.server == owner && link.port == number {
            link.state = LinkState::Closed;
            link.used = false;
        }
    }
}

/// Forgets everything that belonged to a dead task.
pub fn forget(pid: Pid) {
    let mut registry = REGISTRY.lock();
    for port in registry.ports.iter_mut() {
        if port.used && port.owner == pid {
            port.used = false;
        }
    }
    for link in registry.links.iter_mut() {
        if link.used && (link.client == pid || link.server == pid) {
            link.state = LinkState::Closed;
            link.used = false;
        }
    }
}

/// Does an accepted link join these two tasks?
///
/// That is what `ipc::send` consults when 12.1 imposes going through an
/// internal port: the link **is** the authorisation.
pub fn authorized(from: Pid, to: Pid) -> bool {
    let registry = REGISTRY.lock();
    registry.links.iter().any(|link| {
        link.used
            && link.state == LinkState::Accepted
            && ((link.client == from && link.server == to)
                || (link.server == from && link.client == to))
    })
}

/// Request as described to the server.
#[derive(Debug, Clone)]
pub struct Request {
    pub link: u64,
    pub client: Pid,
    pub client_app: String,
    pub port: u16,
}

/// Requests pending for the current task.
pub fn pending() -> Vec<Request> {
    let server = crate::scheduler::current_pid();
    let registry = REGISTRY.lock();
    registry
        .links
        .iter()
        .filter(|l| l.used && l.server == server && l.state == LinkState::Pending)
        .map(|l| Request {
            link: l.id,
            client: l.client,
            client_app: l.client_app.clone(),
            port: l.port,
        })
        .collect()
}

/// State of a link.
pub fn state(link: u64) -> Option<LinkState> {
    let registry = REGISTRY.lock();
    registry
        .links
        .iter()
        .find(|l| l.used && l.id == link)
        .map(|l| l.state)
}

fn settle(link: u64, decision: LinkState) -> Result<(), PortError> {
    let server = crate::scheduler::current_pid();
    let client = {
        let mut registry = REGISTRY.lock();
        let entry = registry
            .links
            .iter_mut()
            .find(|l| l.used && l.id == link)
            .ok_or(PortError::NoSuchLink)?;
        if entry.server != server {
            return Err(PortError::NoSuchLink);
        }
        entry.state = decision;
        entry.client
    };

    match decision {
        LinkState::Accepted => ACCEPTED.fetch_add(1, Ordering::Relaxed),
        _ => REFUSED.fetch_add(1, Ordering::Relaxed),
    };

    // The client is waiting: waking it avoids its sitting until the
    // deadline when the answer is already there.
    crate::scheduler::wake(client);
    Ok(())
}

/// The server accepts a request.
pub fn accept(link: u64) -> Result<(), PortError> {
    settle(link, LinkState::Accepted)
}

/// The server refuses a request.
pub fn refuse(link: u64) -> Result<(), PortError> {
    settle(link, LinkState::Refused)
}

/// Closes a link, on both sides.
pub fn close(link: u64) {
    let mut registry = REGISTRY.lock();
    if let Some(entry) = registry.links.iter_mut().find(|l| l.used && l.id == link) {
        entry.state = LinkState::Closed;
        entry.used = false;
    }
}

/// Opens a link towards a port of another application.
pub fn connect(app_id: &str, number: u16) -> Result<u64, PortError> {
    let client = crate::scheduler::current_pid();
    let server = crate::apps::find_by_app_id(app_id).ok_or(PortError::NoSuchApp)?;

    {
        let registry = REGISTRY.lock();
        if !registry
            .ports
            .iter()
            .any(|p| p.used && p.owner == server && p.number == number)
        {
            return Err(PortError::NoSuchPort);
        }
    }

    use crate::binfmt::identity::Communication;
    let mode = crate::apps::communication(client, server);
    if mode == Communication::Blocked {
        return Err(PortError::Blocked);
    }

    // 12.5: the user's agreement, when the two applications do not belong
    // to the same group. The resource is named in a stable way -- target
    // application and number -- so that a remembered choice bears on this
    // connection and not on all of them.
    if mode == Communication::InternalPort {
        let client_identity = crate::apps::identity_of(client);
        let (client_app, display) = match &client_identity {
            Some(identity) => (identity.app_id.clone(), identity.display_name.clone()),
            None => (String::from("systeme"), crate::apps::display_name(client)),
        };
        let resource = alloc::format!("port:{}:{}", app_id, number);
        let question = alloc::format!(
            "autoriser la connexion vers l'application \"{}\" ?",
            app_id
        );
        let decision = crate::prompt::ask(&client_app, &display, &resource, &question);
        if !decision.allowed() {
            REFUSED.fetch_add(1, Ordering::Relaxed);
            return Err(PortError::DeniedByUser);
        }
    }

    let client_app = crate::apps::identity_of(client)
        .map(|identity| identity.app_id)
        .unwrap_or_else(|| String::from("systeme"));

    let id = {
        let mut registry = REGISTRY.lock();
        if registry.links.len() >= MAX_LINKS {
            registry.links.retain(|l| l.used);
        }
        if registry.links.len() >= MAX_LINKS {
            return Err(PortError::Full);
        }
        let id = registry.next_id;
        registry.next_id += 1;
        registry.links.push(Link {
            used: true,
            id,
            client,
            server,
            port: number,
            state: LinkState::Pending,
            client_app: client_app.clone(),
            messages: 0,
        });
        id
    };

    // 12.2: the server receives the request **with the complete identity
    // of the client**. It travels in the clear in the payload: the server
    // decides on what it reads, not on a PID that teaches it nothing.
    let payload = describe(id, number, &client_app);
    let message = Message::new(client, MessageKind::Request, id as u32, &payload);
    let _ = crate::ipc::send_unchecked(server, message);

    // Waiting for the decision.
    let per_tick = 1000 / crate::TIMER_HZ as u64;
    let deadline = crate::idt::ticks() + ACCEPT_TIMEOUT_MS.div_ceil(per_tick).max(1);

    loop {
        match state(id) {
            Some(LinkState::Accepted) => return Ok(id),
            Some(LinkState::Refused) => {
                close(id);
                return Err(PortError::DeniedByApp);
            }
            Some(LinkState::Pending) => {}
            _ => return Err(PortError::NoSuchLink),
        }
        if crate::idt::ticks() >= deadline {
            close(id);
            REFUSED.fetch_add(1, Ordering::Relaxed);
            return Err(PortError::Timeout);
        }
        let _ = crate::syscall::invoke(
            crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
            [per_tick, 0, 0, 0, 0, 0],
        );
    }
}

/// Payload of a connection request.
///
/// ```text
///   0x00  8  link identifier
///   0x08  2  port number
///   0x0A  2  length of the application identifier
///   0x0C  n  application identifier, in ASCII
/// ```
pub const REQUEST_HEADER: usize = 12;

fn describe(link: u64, port: u16, app_id: &str) -> Vec<u8> {
    let name = app_id.as_bytes();
    let count = name.len().min(crate::ipc::MAX_PAYLOAD - REQUEST_HEADER);

    let mut payload = Vec::with_capacity(REQUEST_HEADER + count);
    payload.extend_from_slice(&link.to_le_bytes());
    payload.extend_from_slice(&port.to_le_bytes());
    payload.extend_from_slice(&(count as u16).to_le_bytes());
    payload.extend_from_slice(&name[..count]);
    payload
}

/// Passes a message through an accepted link.
pub fn send(link: u64, payload: &[u8]) -> Result<(), PortError> {
    let me = crate::scheduler::current_pid();
    let target = {
        let mut registry = REGISTRY.lock();
        let entry = registry
            .links
            .iter_mut()
            .find(|l| l.used && l.id == link)
            .ok_or(PortError::NoSuchLink)?;
        if entry.state != LinkState::Accepted {
            return Err(PortError::NotAccepted);
        }
        entry.messages += 1;
        if entry.client == me {
            entry.server
        } else if entry.server == me {
            entry.client
        } else {
            return Err(PortError::NoSuchLink);
        }
    };

    // Through `send` and not `send_unchecked`: the accepted link **is**
    // the authorisation, and `ipc::send` consults it. Routing a link's
    // messages through the back door would take from 12.1 its single
    // point of passage, and a link closed in the meantime would keep
    // carrying.
    let message = Message::new(me, MessageKind::Data, link as u32, payload);
    crate::ipc::send(target, message).map_err(|_| PortError::NotAccepted)
}

/// Counters: (links accepted, refused).
pub fn counters() -> (u64, u64) {
    (
        ACCEPTED.load(Ordering::Relaxed),
        REFUSED.load(Ordering::Relaxed),
    )
}

pub fn dump_state() {
    // The lines are made under the registry's lock and printed after it:
    // every serial byte is slow, and the lock masks interrupts.
    let (ports, links, lines) = {
        let registry = REGISTRY.lock();
        let ports = registry.ports.iter().filter(|p| p.used).count();
        let links = registry.links.iter().filter(|l| l.used).count();
        let mut lines = alloc::vec::Vec::new();
        for port in registry.ports.iter().filter(|p| p.used) {
            lines.push(alloc::format!("[ports]   PID {} ecoute le port {}", port.owner, port.number));
        }
        for link in registry.links.iter().filter(|l| l.used) {
            lines.push(alloc::format!(
                "[ports]   lien {} : \"{}\" (PID {}) -> PID {} port {} - {} - {} messages",
                link.id,
                link.client_app,
                link.client,
                link.server,
                link.port,
                link.state.as_str(),
                link.messages
            ));
        }
        (ports, links, lines)
    };
    if ports == 0 && links == 0 {
        return;
    }
    crate::serial_println!("[ports] {} port(s) declares, {} lien(s)", ports, links);
    for line in lines {
        crate::serial_println!("{}", line);
    }
}

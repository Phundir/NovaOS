//! Inter-process communication (spec 6.5).
//!
//! Two complementary mechanisms:
//!   - **mailboxes** -- each process owns a named message queue,
//!     addressed by PID (SYS_IPC_SEND / SYS_IPC_RECV);
//!   - **named channels** -- shared rendezvous points, named by a string,
//!     which several processes can subscribe to (SYS_IPC_OPEN /
//!     SYS_IPC_CLOSE / SYS_IPC_PUBLISH).
//!
//! Reception is blocking: a process with no message goes SLEEPING and
//! stops consuming CPU until a send wakes it. That is what makes the
//! scheduling realistic -- without blocking, the low priority tasks would
//! only run through anti-starvation.
//!
//! The isolation rules of spec 12.1 have been enforced since step 6.4.1,
//! in `allowed_by_isolation`: a `.ras` communicates with nobody, two
//! applications of different groups only talk through an accepted
//! internal port (12.2). A task with no application identity -- driver,
//! service, kernel -- is not concerned.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::process::Pid;
use crate::scheduler;
use crate::vga::SpinLock;

/// Cap on messages pending per recipient. Beyond it, the send fails
/// rather than consume the heap indefinitely.
///
/// Public because it is also the right bound for a reader that wants to
/// empty its box in one pass: `gui::modules::run` drains up to this many
/// events per wake, so that a backlog can never survive a wake. A literal
/// 64 there would drift the day this number changes.
pub const MAX_QUEUE_LEN: usize = 64;

/// Maximum payload size carried by a message.
pub const MAX_PAYLOAD: usize = 256;

pub type ChannelId = u64;

// ============================================================
//  Message
// ============================================================

/// Nature of the message -- determines its handling by the recipient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    /// Raw data.
    Data,
    /// System event notification (SYS_IPC_PUBLISH).
    Event,
    /// Request awaiting an answer (SYS_IPC_QUERY).
    Request,
    /// Answer to a request (SYS_IPC_RESPOND).
    Response,
    /// Control signal with no payload.
    Signal,
}

impl MessageKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            MessageKind::Data => "DATA",
            MessageKind::Event => "EVENT",
            MessageKind::Request => "REQUEST",
            MessageKind::Response => "RESPONSE",
            MessageKind::Signal => "SIGNAL",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Message {
    pub sender: Pid,
    pub kind: MessageKind,
    /// Free application identifier -- serves as an opcode.
    pub tag: u32,
    pub payload: Vec<u8>,
    /// Timer tick at the moment of the send.
    pub timestamp: u64,
}

impl Message {
    pub fn new(sender: Pid, kind: MessageKind, tag: u32, payload: &[u8]) -> Self {
        Message {
            sender,
            kind,
            tag,
            payload: payload.to_vec(),
            timestamp: crate::idt::ticks(),
        }
    }

    /// Builds a message carrying text.
    pub fn text(sender: Pid, kind: MessageKind, tag: u32, text: &str) -> Self {
        Message::new(sender, kind, tag, text.as_bytes())
    }

    /// Interprets the payload as text, if it is valid.
    pub fn as_str(&self) -> Option<&str> {
        core::str::from_utf8(&self.payload).ok()
    }
}

/// Failure causes of an IPC operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcError {
    /// The target PID has no registered mailbox.
    NoSuchProcess,
    /// The named channel does not exist.
    NoSuchChannel,
    /// Queue full -- the recipient does not consume fast enough.
    QueueFull,
    /// Payload too large.
    PayloadTooLarge,
    /// The channel already exists under that name.
    ChannelExists,
    /// Spec 12.1 forbids all communication between these two apps --
    /// that is the case of a `.ras`, isolated without exception.
    Forbidden,
    /// Communication allowed, but only through an explicitly accepted
    /// internal port (spec 12.2).
    NeedsPort,
}

impl IpcError {
    pub fn as_str(&self) -> &'static str {
        match self {
            IpcError::NoSuchProcess => "processus inconnu",
            IpcError::NoSuchChannel => "canal inconnu",
            IpcError::QueueFull => "file pleine",
            IpcError::PayloadTooLarge => "charge utile trop grande",
            IpcError::ChannelExists => "canal deja existant",
            IpcError::Forbidden => "communication interdite entre ces applications",
            IpcError::NeedsPort => "passage par un port interne exige",
        }
    }
}

// ============================================================
//  2.6.2 -- Named channels
// ============================================================

struct Channel {
    id: ChannelId,
    name: String,
    owner: Pid,
    queue: VecDeque<Message>,
    /// Subscribed processes -- all woken on a publication.
    subscribers: Vec<Pid>,
}

// ============================================================
//  Global registry
// ============================================================

struct IpcRegistry {
    /// 2.6.3 -- one queue per process.
    mailboxes: BTreeMap<Pid, VecDeque<Message>>,
    channels: BTreeMap<ChannelId, Channel>,
    names: BTreeMap<String, ChannelId>,
    next_channel_id: ChannelId,

    // --- statistics ---
    sent: u64,
    received: u64,
    dropped: u64,
    blocks: u64,
}

impl IpcRegistry {
    const fn new() -> Self {
        IpcRegistry {
            mailboxes: BTreeMap::new(),
            channels: BTreeMap::new(),
            names: BTreeMap::new(),
            next_channel_id: 1,
            sent: 0,
            received: 0,
            dropped: 0,
            blocks: 0,
        }
    }
}

static REGISTRY: SpinLock<IpcRegistry> = SpinLock::new(IpcRegistry::new());

// ============================================================
//  Mailboxes
// ============================================================

/// Creates the mailbox of a process. Called at its creation.
pub fn register(pid: Pid) {
    REGISTRY.lock().mailboxes.insert(pid, VecDeque::new());
}

/// Removes the mailbox and takes the process out of every channel.
///
/// The channels it owned are closed: leaving them open would let a name
/// resolve to a dead task, and `SYS_IPC_QUERY` would address its requests
/// to nobody.
pub fn unregister(pid: Pid) {
    {
        let mut reg = REGISTRY.lock();
        reg.mailboxes.remove(&pid);
        for channel in reg.channels.values_mut() {
            channel.subscribers.retain(|&p| p != pid);
        }

        let orphans: Vec<(ChannelId, String)> = reg
            .channels
            .values()
            .filter(|channel| channel.owner == pid)
            .map(|channel| (channel.id, channel.name.clone()))
            .collect();
        for (id, name) in orphans {
            reg.channels.remove(&id);
            reg.names.remove(&name);
        }
    }

    // Outside the IPC lock: those two registries have their own, and
    // nesting them would invite deadlock.
    crate::apps::forget(pid);
    crate::ports::forget(pid);
}

/// 6.4.1 -- Isolation check of spec 12.1.
///
/// The 12.1 table gives three verdicts, and all three are expressed here:
/// direct IPC, internal port, or blocking. The blocking of a `.ras` is
/// **absolute** -- 12.3 puts it on the deny-list, with no exception and no
/// possible waiver by header declaration.
///
/// The "internal port" verdict is not a definitive refusal: it points to
/// `ports::connect`, whose acceptance by the recipient *and* the user's
/// agreement amount to authorisation. The established link is consulted
/// here, which makes it the only legitimate door between two distinct
/// groups.
///
/// Two tasks of which at least one has no application identity -- a
/// driver, a service, the scheduler -- are not concerned: these rules
/// govern applications between themselves.
fn allowed_by_isolation(from: Pid, to: Pid) -> Result<(), IpcError> {
    use crate::binfmt::identity::Communication;

    match crate::apps::communication(from, to) {
        Communication::DirectIpc => Ok(()),
        Communication::Blocked => Err(IpcError::Forbidden),
        Communication::InternalPort => {
            if crate::ports::authorized(from, to) {
                Ok(())
            } else {
                Err(IpcError::NeedsPort)
            }
        }
    }
}

/// 2.6.1 -- Drops a message in the recipient's box and wakes it.
///
/// Non-blocking: if the queue is full, the send fails immediately.
///
/// Since step 6.4.1, sending is subject to the isolation rules of 12.1:
/// it is the single point of passage of every named message, hence the
/// only place where the rule can be enforced without a hole.
pub fn send(to: Pid, message: Message) -> Result<(), IpcError> {
    allowed_by_isolation(message.sender, to)?;
    send_unchecked(to, message)
}

/// Drops a message without applying the rules of 12.1.
///
/// Reserved to the kernel and to the internal port mechanism: the
/// connection request itself must reach the recipient, failing which no
/// inter-group connection could ever be negotiated. It is the only
/// exception, and it carries the request alone.
pub fn send_unchecked(to: Pid, message: Message) -> Result<(), IpcError> {
    if message.payload.len() > MAX_PAYLOAD {
        return Err(IpcError::PayloadTooLarge);
    }

    {
        let mut reg = REGISTRY.lock();

        let queue = reg
            .mailboxes
            .get_mut(&to)
            .ok_or(IpcError::NoSuchProcess)?;

        if queue.len() >= MAX_QUEUE_LEN {
            reg.dropped += 1;
            return Err(IpcError::QueueFull);
        }

        queue.push_back(message);
        reg.sent += 1;
    }

    // Wake outside the IPC lock: wake takes the scheduler lock, and
    // nesting the two would invite deadlock.
    scheduler::wake(to);
    Ok(())
}

/// Textual send -- a common shortcut.
pub fn send_text(to: Pid, kind: MessageKind, tag: u32, text: &str) -> Result<(), IpcError> {
    let from = scheduler::current_pid();
    send(to, Message::text(from, kind, tag, text))
}

/// Removes a message from one's own box, without waiting.
pub fn try_recv() -> Option<Message> {
    let pid = scheduler::current_pid();
    let mut reg = REGISTRY.lock();

    let message = reg.mailboxes.get_mut(&pid)?.pop_front();
    if message.is_some() {
        reg.received += 1;
    }
    message
}

/// 2.6.1 -- **Blocking** reception. The process goes SLEEPING until a
/// message arrives.
///
/// The check/sleep sequence must be atomic. Two mechanisms see to it, and
/// they do not cover the same thing:
///
/// - **masking interrupts** prevents a preemption from separating the
///   test from the sleep on this core;
/// - **arming the sleep before the test** prevents a sender working on
///   *another* core from delivering its message and calling `wake` on a
///   task that is still RUNNING -- a wake with no effect, followed by a
///   permanent sleep despite the pending message.
///
/// The second point cannot be seen on a single core, and that is what
/// makes it dangerous: the code looked correct as long as it was tested
/// on a uniprocessor only.
pub fn recv() -> Message {
    loop {
        crate::idt::disable_interrupts();

        scheduler::arm_sleep();

        if let Some(message) = try_recv() {
            scheduler::cancel_sleep();
            unsafe { crate::idt::enable_interrupts() };
            return message;
        }

        REGISTRY.lock().blocks += 1;

        // Yield the CPU. On waking, execution resumes here and the loop
        // tries again.
        scheduler::yield_now();

        unsafe { crate::idt::enable_interrupts() };
    }
}

/// Number of messages pending for the current process.
pub fn pending() -> usize {
    let pid = scheduler::current_pid();
    REGISTRY
        .lock()
        .mailboxes
        .get(&pid)
        .map(|q| q.len())
        .unwrap_or(0)
}

// ============================================================
//  2.6.2 -- Named channels
// ============================================================

/// Creates a named channel. The creator becomes its owner and first
/// subscriber (SYS_IPC_OPEN).
pub fn open_channel(name: &str) -> Result<ChannelId, IpcError> {
    let owner = scheduler::current_pid();
    let mut reg = REGISTRY.lock();

    if reg.names.contains_key(name) {
        return Err(IpcError::ChannelExists);
    }

    let id = reg.next_channel_id;
    reg.next_channel_id += 1;

    reg.channels.insert(
        id,
        Channel {
            id,
            name: name.to_string(),
            owner,
            queue: VecDeque::new(),
            subscribers: alloc::vec![owner],
        },
    );
    reg.names.insert(name.to_string(), id);

    Ok(id)
}

/// Resolves a channel name into an identifier.
pub fn find_channel(name: &str) -> Option<ChannelId> {
    REGISTRY.lock().names.get(name).copied()
}

/// 5.5.4 -- Process exposing the API carried by a channel.
///
/// `SYS_IPC_QUERY` needs it: a request is addressed to the **owner** of
/// the channel, not to the channel itself. Publishing on the channel
/// would reach every subscriber, which is not what a request wants.
pub fn channel_owner(name: &str) -> Option<Pid> {
    let reg = REGISTRY.lock();
    let id = reg.names.get(name)?;
    reg.channels.get(id).map(|channel| channel.owner)
}

/// Open channels: (name, identifier, owner, subscribers).
pub fn channel_list() -> Vec<(String, ChannelId, Pid, usize)> {
    let reg = REGISTRY.lock();
    reg.channels
        .values()
        .map(|channel| {
            (
                channel.name.clone(),
                channel.id,
                channel.owner,
                channel.subscribers.len(),
            )
        })
        .collect()
}

/// Adds the current process to the subscribers of a channel (SYS_IPC_SUBSCRIBE).
pub fn subscribe(id: ChannelId) -> Result<(), IpcError> {
    let pid = scheduler::current_pid();
    let mut reg = REGISTRY.lock();

    let channel = reg.channels.get_mut(&id).ok_or(IpcError::NoSuchChannel)?;
    if !channel.subscribers.contains(&pid) {
        channel.subscribers.push(pid);
    }
    Ok(())
}

/// Broadcasts a message to every subscriber of a channel (SYS_IPC_PUBLISH).
///
/// Each subscriber receives a copy in its own box, which allows a uniform
/// reception through `recv`.
pub fn publish(id: ChannelId, message: Message) -> Result<usize, IpcError> {
    let (subscribers, sender) = {
        let mut reg = REGISTRY.lock();
        let channel = reg.channels.get_mut(&id).ok_or(IpcError::NoSuchChannel)?;
        channel.queue.push_back(message.clone());
        if channel.queue.len() > MAX_QUEUE_LEN {
            channel.queue.pop_front();
        }
        (channel.subscribers.clone(), message.sender)
    };

    let mut delivered = 0;
    for pid in subscribers {
        // The sender does not receive its own publication.
        if pid == sender {
            continue;
        }
        if send(pid, message.clone()).is_ok() {
            delivered += 1;
        }
    }

    Ok(delivered)
}

/// Publishes on a channel named by its string.
pub fn publish_text(name: &str, tag: u32, text: &str) -> Result<usize, IpcError> {
    let id = find_channel(name).ok_or(IpcError::NoSuchChannel)?;
    let from = scheduler::current_pid();
    publish(id, Message::text(from, MessageKind::Event, tag, text))
}

/// Closes a channel (SYS_IPC_CLOSE). Only its owner may do so.
pub fn close_channel(id: ChannelId) -> Result<(), IpcError> {
    let pid = scheduler::current_pid();
    let mut reg = REGISTRY.lock();

    let channel = reg.channels.get(&id).ok_or(IpcError::NoSuchChannel)?;
    if channel.owner != pid {
        return Err(IpcError::NoSuchChannel);
    }

    let name = channel.name.clone();
    reg.channels.remove(&id);
    reg.names.remove(&name);
    Ok(())
}

// ============================================================
//  Diagnostics
// ============================================================

/// Statistics: (sent, received, lost, blocks).
pub fn stats() -> (u64, u64, u64, u64) {
    let reg = REGISTRY.lock();
    (reg.sent, reg.received, reg.dropped, reg.blocks)
}

pub fn dump_state() {
    let reg = REGISTRY.lock();

    crate::serial_println!(
        "[ipc] {} envoyes - {} recus - {} perdus - {} blocages",
        reg.sent,
        reg.received,
        reg.dropped,
        reg.blocks
    );

    for (pid, queue) in reg.mailboxes.iter() {
        if !queue.is_empty() {
            crate::serial_println!("[ipc]   boite PID {} : {} en attente", pid, queue.len());
        }
    }

    for channel in reg.channels.values() {
        crate::serial_println!(
            "[ipc]   canal {} \"{}\" - proprietaire {} - {} abonnes",
            channel.id,
            channel.name,
            channel.owner,
            channel.subscribers.len()
        );
    }
}

//! Sockets: unix-domain, and IPv4 TCP and UDP over the Nova stack.
//!
//! ## Unix-domain sockets
//!
//! Firefox talks to itself through them even in a single process: its
//! IPC channels are `socketpair`s, and descriptors travel over them in
//! `SCM_RIGHTS` messages. Both ends live in this kernel, so a socket pair
//! is two queues of messages, one per direction. A message carries bytes
//! and, possibly, descriptions of open files; passing one installs a new
//! descriptor on the same description in the receiving table -- the
//! whole of `SCM_RIGHTS` when sender and receiver are the same process.
//!
//! Stream sockets read across message boundaries, but never across a
//! message carrying descriptors, as Linux does. Datagram and seqpacket
//! sockets deliver one message per read.
//!
//! Named sockets (`bind` to a path or an abstract name, `listen`,
//! `accept`, `connect`) are kept in a table by name rather than as nodes
//! of the tree: nothing in a single-process browser lists them.
//!
//! ## IPv4
//!
//! The Nova TCP stack is a client with blocking calls. A Linux socket
//! wraps a connection handle: `connect` runs in a helper task so that a
//! non-blocking socket can return `EINPROGRESS` and report completion
//! through `poll`, reads take only what has already arrived, and every
//! segment or datagram the stack receives wakes the waiters of `NET`.
//! IPv6 is refused (`EAFNOSUPPORT`): programs then fall back to IPv4.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use super::errno::*;
use super::fd::{Description, FileOps, POLLERR, POLLHUP, POLLIN, POLLOUT, POLLRDHUP};
use super::vfs::{Kind, S_IFSOCK, Stat};
use super::wait::{self, WaitQueue};
use super::Thread;
use crate::net::Ipv4;
use crate::vga::SpinLock;

pub const AF_UNIX: u64 = 1;
pub const AF_INET: u64 = 2;
pub const AF_INET6: u64 = 10;
pub const AF_NETLINK: u64 = 16;
pub const SOCK_STREAM: u64 = 1;
pub const SOCK_DGRAM: u64 = 2;
pub const SOCK_SEQPACKET: u64 = 5;
pub const SOCK_NONBLOCK: u64 = 0o4000;
pub const SOCK_CLOEXEC: u64 = 0o2000000;

const SOL_SOCKET: u64 = 1;
const SCM_RIGHTS: u32 = 1;
const MSG_PEEK: u64 = 2;
const MSG_DONTWAIT: u64 = 0x40;
const MSG_NOSIGNAL: u64 = 0x4000;
const MSG_CMSG_CLOEXEC: u64 = 0x4000_0000;
const MSG_CTRUNC: u32 = 8;
const MSG_TRUNC: u32 = 0x20;

/// Every segment and datagram the Nova stack receives wakes this queue.
static NET: WaitQueue = WaitQueue::new();

/// Called by `net::tcp::input` and `net::udp::input`.
pub fn notify() {
    if !NET.is_empty() {
        NET.notify_all();
    }
}

static NEXT_INO: AtomicU64 = AtomicU64::new(5_000_000);

fn wait_err(e: wait::WaitError) -> i64 {
    match e {
        wait::WaitError::Interrupted => EINTR,
        wait::WaitError::TimedOut => EAGAIN,
    }
}

// ============================================================
//  Unix-domain sockets
// ============================================================

struct Message {
    data: Vec<u8>,
    /// Bytes of `data` already read (stream sockets read in pieces).
    read: usize,
    files: Vec<Arc<Description>>,
}

/// One direction of a connection.
struct Channel {
    messages: SpinLock<VecDeque<Message>>,
    bytes: AtomicU64,
    /// The writing end is gone, or shut down for writing.
    closed: AtomicBool,
}

impl Channel {
    fn new() -> Arc<Channel> {
        Arc::new(Channel { messages: SpinLock::new(VecDeque::new()), bytes: AtomicU64::new(0), closed: AtomicBool::new(false) })
    }
}

const UNIX_BUFFER: u64 = 256 * 1024;

enum UnixState {
    Unbound,
    Listening { backlog: VecDeque<Arc<UnixSocket>> },
    Connected { rx: Arc<Channel>, tx: Arc<Channel> },
}

pub struct UnixSocket {
    ino: u64,
    kind: u64,
    state: SpinLock<UnixState>,
    name: SpinLock<Option<String>>,
    /// Shared by both ends of a pair: any change on either side wakes
    /// the waiters of both.
    queue: Arc<WaitQueue>,
    peer_pid: AtomicU64,
}

/// Named sockets waiting for `connect`, by path or abstract name.
static LISTENERS: SpinLock<BTreeMap<String, Weak<UnixSocket>>> = SpinLock::new(BTreeMap::new());

impl UnixSocket {
    fn new(kind: u64, queue: Arc<WaitQueue>) -> Arc<UnixSocket> {
        Arc::new(UnixSocket {
            ino: NEXT_INO.fetch_add(1, Ordering::Relaxed),
            kind,
            state: SpinLock::new(UnixState::Unbound),
            name: SpinLock::new(None),
            queue,
            peer_pid: AtomicU64::new(0),
        })
    }

    fn channels(&self) -> Option<(Arc<Channel>, Arc<Channel>)> {
        match &*self.state.lock() {
            UnixState::Connected { rx, tx } => Some((rx.clone(), tx.clone())),
            _ => None,
        }
    }

    /// Sends one message. Stream sockets may take part of it.
    fn send_message(&self, data: &[u8], files: Vec<Arc<Description>>, nonblock: bool) -> LxResult<usize> {
        let (_, tx) = self.channels().ok_or(ENOTCONN)?;
        let stream = self.kind == SOCK_STREAM;
        let mut files = Some(files);
        let put = || -> Option<LxResult<usize>> {
            if tx.closed.load(Ordering::Acquire) {
                return Some(Err(EPIPE));
            }
            let used = tx.bytes.load(Ordering::Acquire);
            let room = UNIX_BUFFER.saturating_sub(used) as usize;
            let needed = if stream { 1.min(data.len()) } else { data.len() };
            if room >= needed.max(1) || data.is_empty() {
                let n = if stream { room.min(data.len()) } else { data.len() };
                tx.bytes.fetch_add(n as u64, Ordering::AcqRel);
                tx.messages.lock().push_back(Message {
                    data: data[..n].to_vec(),
                    read: 0,
                    files: files.take().unwrap_or_default(),
                });
                return Some(Ok(n));
            }
            if nonblock {
                return Some(Err(EAGAIN));
            }
            None
        };
        let n = wait::wait_until(&[&self.queue], None, put).map_err(wait_err)??;
        self.queue.notify_all();
        Ok(n)
    }

    /// Receives into `buf`. Returns (bytes, descriptions passed, the
    /// message was longer than `buf`).
    fn recv_message(&self, buf: &mut [u8], nonblock: bool, peek: bool) -> LxResult<(usize, Vec<Arc<Description>>, bool)> {
        let (rx, _) = self.channels().ok_or(ENOTCONN)?;
        let stream = self.kind == SOCK_STREAM;
        let take = || -> Option<LxResult<(usize, Vec<Arc<Description>>, bool)>> {
            let mut messages = rx.messages.lock();
            if messages.is_empty() {
                if rx.closed.load(Ordering::Acquire) {
                    return Some(Ok((0, Vec::new(), false)));
                }
                if nonblock {
                    return Some(Err(EAGAIN));
                }
                return None;
            }
            if !stream {
                let m = if peek { messages.front().map(|m| (m.data.clone(), Vec::new())) } else { messages.pop_front().map(|m| (m.data, m.files)) };
                let (data, files) = m.unwrap();
                if !peek {
                    rx.bytes.fetch_sub(data.len() as u64, Ordering::AcqRel);
                }
                let n = data.len().min(buf.len());
                buf[..n].copy_from_slice(&data[..n]);
                return Some(Ok((n, files, data.len() > buf.len())));
            }
            // Stream: read across messages, stopping before one that
            // carries descriptors (unless it is the first).
            let mut done = 0;
            let mut files = Vec::new();
            let mut index = 0;
            while done < buf.len() && index < messages.len() {
                if index > 0 && !messages[index].files.is_empty() {
                    break;
                }
                let m = &mut messages[index];
                let available = m.data.len() - m.read;
                let n = available.min(buf.len() - done);
                buf[done..done + n].copy_from_slice(&m.data[m.read..m.read + n]);
                done += n;
                if !peek {
                    m.read += n;
                    if files.is_empty() {
                        files = core::mem::take(&mut m.files);
                    }
                }
                if m.read < m.data.len() || peek {
                    if peek {
                        index += 1;
                        continue;
                    }
                    break;
                }
                index += 1;
            }
            if !peek {
                while messages.front().is_some_and(|m| m.read >= m.data.len() && m.files.is_empty()) {
                    messages.pop_front();
                }
                rx.bytes.fetch_sub(done as u64, Ordering::AcqRel);
            }
            Some(Ok((done, files, false)))
        };
        let result = wait::wait_until(&[&self.queue], None, take).map_err(wait_err)??;
        self.queue.notify_all();
        Ok(result)
    }
}

impl Drop for UnixSocket {
    fn drop(&mut self) {
        if let UnixState::Connected { tx, rx, .. } = &*self.state.lock() {
            tx.closed.store(true, Ordering::Release);
            // Descriptions in flight towards us are released with us.
            rx.messages.lock().clear();
        }
        if let Some(name) = self.name.lock().take() {
            LISTENERS.lock().remove(&name);
        }
        self.queue.notify_all();
    }
}

impl FileOps for UnixSocket {
    fn read(&self, buf: &mut [u8], nonblock: bool) -> LxResult<usize> {
        self.recv_message(buf, nonblock, false).map(|(n, _, _)| n)
    }
    fn write(&self, buf: &[u8], nonblock: bool) -> LxResult<usize> {
        let r = self.send_message(buf, Vec::new(), nonblock);
        if r == Err(EPIPE) {
            super::raise_sigpipe();
        }
        r
    }
    fn stat(&self) -> LxResult<Stat> {
        Ok(Stat { dev: 0x8, ino: self.ino, mode: S_IFSOCK | 0o777, ..Default::default() })
    }
    fn kind(&self) -> Kind {
        Kind::Socket
    }
    fn poll(&self) -> u32 {
        match &*self.state.lock() {
            UnixState::Unbound => POLLOUT,
            UnixState::Listening { backlog } => {
                if backlog.is_empty() {
                    0
                } else {
                    POLLIN
                }
            }
            UnixState::Connected { rx, tx } => {
                let mut r = 0;
                if !rx.messages.lock().is_empty() {
                    r |= POLLIN;
                }
                if rx.closed.load(Ordering::Acquire) {
                    r |= POLLIN | POLLRDHUP | POLLHUP;
                }
                if tx.closed.load(Ordering::Acquire) {
                    r |= POLLOUT | POLLERR;
                } else if tx.bytes.load(Ordering::Acquire) < UNIX_BUFFER {
                    r |= POLLOUT;
                }
                r
            }
        }
    }
    fn wait_queue(&self) -> Option<&WaitQueue> {
        Some(&self.queue)
    }
    fn ioctl(&self, cmd: u64, arg: u64) -> LxResult<u64> {
        const FIONREAD: u64 = 0x541B;
        if cmd == FIONREAD {
            let n = self.channels().map_or(0, |(rx, _)| rx.bytes.load(Ordering::Acquire)) as u32;
            crate::vm::write_user(arg, &n).map_err(|_| EFAULT)?;
            return Ok(0);
        }
        Err(ENOTTY)
    }
    fn describe(&self) -> String {
        alloc::format!("socket:[{}]", self.ino)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn connect_pair(a: &Arc<UnixSocket>, b: &Arc<UnixSocket>) {
    let ab = Channel::new();
    let ba = Channel::new();
    *a.state.lock() = UnixState::Connected { rx: ba.clone(), tx: ab.clone() };
    *b.state.lock() = UnixState::Connected { rx: ab, tx: ba };
    let pid = crate::scheduler::current_pid();
    a.peer_pid.store(pid, Ordering::Relaxed);
    b.peer_pid.store(pid, Ordering::Relaxed);
}

// ============================================================
//  IPv4
// ============================================================

#[derive(Clone, Copy, PartialEq, Eq)]
enum TcpPhase {
    Fresh,
    Connecting,
    Connected(crate::net::tcp::Handle),
    Failed(i64),
}

pub struct InetSocket {
    ino: u64,
    kind: u64,
    tcp: SpinLock<TcpPhase>,
    udp: SpinLock<Option<crate::net::udp::Handle>>,
    local_port: AtomicU32,
    peer: SpinLock<Option<(Ipv4, u16)>>,
    /// Pending SO_ERROR, read (and cleared) by getsockopt.
    error: SpinLock<i64>,
    shut_write: AtomicBool,
}

impl InetSocket {
    fn new(kind: u64) -> Arc<InetSocket> {
        Arc::new(InetSocket {
            ino: NEXT_INO.fetch_add(1, Ordering::Relaxed),
            kind,
            tcp: SpinLock::new(TcpPhase::Fresh),
            udp: SpinLock::new(None),
            local_port: AtomicU32::new(0),
            peer: SpinLock::new(None),
            error: SpinLock::new(0),
            shut_write: AtomicBool::new(false),
        })
    }

    fn udp_handle(&self) -> LxResult<crate::net::udp::Handle> {
        let mut udp = self.udp.lock();
        if let Some(h) = *udp {
            return Ok(h);
        }
        let (h, port) = crate::net::udp::bind(0).map_err(|_| EADDRINUSE)?;
        self.local_port.store(port as u32, Ordering::Relaxed);
        *udp = Some(h);
        Ok(h)
    }
}

impl Drop for InetSocket {
    fn drop(&mut self) {
        if let TcpPhase::Connected(h) = *self.tcp.lock() {
            crate::net::tcp::close(h);
        }
        if let Some(h) = self.udp.lock().take() {
            crate::net::udp::close(h);
        }
    }
}

/// Connections in progress, handed to their helper tasks.
static CONNECTS: SpinLock<Vec<(Arc<InetSocket>, Ipv4, u16)>> = SpinLock::new(Vec::new());

fn connect_helper() -> ! {
    let job = CONNECTS.lock().pop();
    if let Some((socket, address, port)) = job {
        let result = crate::net::tcp::connect(address, port, 15_000);
        let phase = match result {
            Ok(h) => TcpPhase::Connected(h),
            Err(reason) => {
                let errno = if reason.contains("refusee") { ECONNREFUSED } else if reason.contains("delai") || reason.contains("reponse") { ETIMEDOUT } else { ENETUNREACH };
                *socket.error.lock() = errno;
                TcpPhase::Failed(errno)
            }
        };
        *socket.tcp.lock() = phase;
        NET.notify_all();
    }
    crate::scheduler::exit_current_with(0)
}

impl FileOps for InetSocket {
    fn read(&self, buf: &mut [u8], nonblock: bool) -> LxResult<usize> {
        if self.kind == SOCK_DGRAM {
            return udp_recv(self, buf, nonblock).map(|(n, _)| n);
        }
        let take = || -> Option<LxResult<usize>> {
            let phase = *self.tcp.lock();
            let h = match phase {
                TcpPhase::Connected(h) => h,
                TcpPhase::Failed(e) => return Some(Err(e)),
                _ => return Some(Err(ENOTCONN)),
            };
            let (available, finished, reset, _) = crate::net::tcp::readiness(h);
            if reset {
                return Some(Err(ECONNRESET));
            }
            if available > 0 {
                return Some(match crate::net::tcp::take_available(h, buf.len()) {
                    Ok(data) => {
                        buf[..data.len()].copy_from_slice(&data);
                        Ok(data.len())
                    }
                    Err(_) => Err(ECONNRESET),
                });
            }
            if finished {
                return Some(Ok(0));
            }
            if nonblock {
                return Some(Err(EAGAIN));
            }
            None
        };
        wait::wait_until(&[&NET], None, take).map_err(wait_err)?
    }
    /// TCP: the bytes go into the connection's send buffer and the call
    /// returns -- it does not wait for them to be acknowledged. A full
    /// buffer is `EAGAIN` for a non-blocking socket; a blocking one waits
    /// for room until everything is queued, as POSIX has it.
    fn write(&self, buf: &[u8], nonblock: bool) -> LxResult<usize> {
        if self.kind == SOCK_DGRAM {
            let peer = self.peer.lock().ok_or(EDESTADDRREQ)?;
            return udp_send(self, buf, peer);
        }
        if self.shut_write.load(Ordering::Acquire) {
            return Err(EPIPE);
        }
        let phase = *self.tcp.lock();
        let h = match phase {
            TcpPhase::Connected(h) => h,
            TcpPhase::Connecting => return Err(EAGAIN),
            TcpPhase::Failed(e) => return Err(e),
            _ => return Err(ENOTCONN),
        };
        let broken = || {
            super::raise_sigpipe();
            EPIPE
        };
        let mut done = crate::net::tcp::write(h, buf).map_err(|_| broken())?;
        while done < buf.len() {
            if nonblock {
                return if done > 0 { Ok(done) } else { Err(EAGAIN) };
            }
            let room = || -> Option<()> {
                let (_, _, reset, _) = crate::net::tcp::readiness(h);
                (reset || crate::net::tcp::send_room(h) > 0).then_some(())
            };
            wait::wait_until(&[&NET], None, room).map_err(wait_err)?;
            done += crate::net::tcp::write(h, &buf[done..]).map_err(|_| broken())?;
        }
        Ok(done)
    }
    fn stat(&self) -> LxResult<Stat> {
        Ok(Stat { dev: 0x8, ino: self.ino, mode: S_IFSOCK | 0o777, ..Default::default() })
    }
    fn kind(&self) -> Kind {
        Kind::Socket
    }
    fn poll(&self) -> u32 {
        if self.kind == SOCK_DGRAM {
            let mut r = POLLOUT;
            if let Some(h) = *self.udp.lock()
                && crate::net::udp::pending(h) > 0
            {
                r |= POLLIN;
            }
            return r;
        }
        match *self.tcp.lock() {
            TcpPhase::Fresh => POLLOUT | POLLHUP,
            TcpPhase::Connecting => 0,
            TcpPhase::Failed(_) => POLLERR | POLLHUP | POLLOUT | POLLIN,
            TcpPhase::Connected(h) => {
                let (available, finished, reset, _) = crate::net::tcp::readiness(h);
                // Writable while the send buffer has room (`tcp::write`).
                let mut r = if crate::net::tcp::send_room(h) > 0 { POLLOUT } else { 0 };
                if available > 0 || finished {
                    r |= POLLIN;
                }
                if finished {
                    r |= POLLRDHUP;
                }
                if reset {
                    r |= POLLERR | POLLHUP;
                }
                r
            }
        }
    }
    fn wait_queue(&self) -> Option<&WaitQueue> {
        Some(&NET)
    }
    fn ioctl(&self, cmd: u64, arg: u64) -> LxResult<u64> {
        const FIONREAD: u64 = 0x541B;
        if cmd == FIONREAD {
            let n = match *self.tcp.lock() {
                TcpPhase::Connected(h) => crate::net::tcp::readiness(h).0 as u32,
                _ => 0,
            };
            crate::vm::write_user(arg, &n).map_err(|_| EFAULT)?;
            return Ok(0);
        }
        Err(ENOTTY)
    }
    fn describe(&self) -> String {
        alloc::format!("socket:[{}]", self.ino)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn udp_send(s: &InetSocket, data: &[u8], to: (Ipv4, u16)) -> LxResult<usize> {
    let h = s.udp_handle()?;
    crate::net::udp::send_to(h, to.0, to.1, data).map_err(|_| ENETUNREACH)
}

fn udp_recv(s: &InetSocket, buf: &mut [u8], nonblock: bool) -> LxResult<(usize, (Ipv4, u16))> {
    let h = s.udp_handle()?;
    let take = || -> Option<LxResult<(usize, (Ipv4, u16))>> {
        match crate::net::udp::try_recv(h) {
            Some(d) => {
                let n = d.payload.len().min(buf.len());
                buf[..n].copy_from_slice(&d.payload[..n]);
                Some(Ok((n, (d.source, d.port))))
            }
            None if nonblock => Some(Err(EAGAIN)),
            None => None,
        }
    };
    wait::wait_until(&[&NET], None, take).map_err(wait_err)?
}

// ============================================================
//  Addresses
// ============================================================

enum Address {
    Inet(Ipv4, u16),
    Unix(String),
}

fn read_address(addr: u64, len: u64) -> LxResult<Address> {
    if !(2..=128).contains(&len) {
        return Err(EINVAL);
    }
    let mut raw = alloc::vec![0u8; len as usize];
    crate::vm::copy_from_user(&mut raw, addr).map_err(|_| EFAULT)?;
    let family = u16::from_le_bytes([raw[0], raw[1]]) as u64;
    match family {
        AF_INET if len >= 8 => Ok(Address::Inet(Ipv4([raw[4], raw[5], raw[6], raw[7]]), u16::from_be_bytes([raw[2], raw[3]]))),
        AF_UNIX => {
            let path = &raw[2..];
            let name = if path.first() == Some(&0) {
                // Abstract namespace: the whole length counts, NULs included.
                let mut n = String::from("@");
                n.push_str(&String::from_utf8_lossy(&path[1..]));
                n
            } else {
                let end = path.iter().position(|b| *b == 0).unwrap_or(path.len());
                String::from_utf8_lossy(&path[..end]).into()
            };
            Ok(Address::Unix(name))
        }
        AF_INET6 => Err(EAFNOSUPPORT),
        _ => Err(EAFNOSUPPORT),
    }
}

fn inet_bytes(ip: Ipv4, port: u16) -> Vec<u8> {
    let mut v = alloc::vec![0u8; 16];
    v[0..2].copy_from_slice(&(AF_INET as u16).to_le_bytes());
    v[2..4].copy_from_slice(&port.to_be_bytes());
    v[4..8].copy_from_slice(&ip.0);
    v
}

fn unix_bytes(name: Option<&str>) -> Vec<u8> {
    let mut v = (AF_UNIX as u16).to_le_bytes().to_vec();
    if let Some(n) = name {
        if let Some(abstract_name) = n.strip_prefix('@') {
            v.push(0);
            v.extend_from_slice(abstract_name.as_bytes());
        } else {
            v.extend_from_slice(n.as_bytes());
            v.push(0);
        }
    }
    v
}

/// Writes an address back: `addr` gets at most `*len_addr` bytes, and
/// `*len_addr` the full length.
fn write_address(addr: u64, len_addr: u64, bytes: &[u8]) -> LxResult<()> {
    if addr == 0 || len_addr == 0 {
        return Ok(());
    }
    let room: u32 = crate::vm::read_user(len_addr).map_err(|_| EFAULT)?;
    let n = (room as usize).min(bytes.len());
    crate::vm::copy_to_user(addr, &bytes[..n]).map_err(|_| EFAULT)?;
    crate::vm::write_user(len_addr, &(bytes.len() as u32)).map_err(|_| EFAULT)?;
    Ok(())
}

// ============================================================
//  Calls
// ============================================================

fn install(t: &Arc<Thread>, file: Arc<dyn FileOps>, flags: u64) -> LxResult<u64> {
    let status = if flags & SOCK_NONBLOCK != 0 { super::fd::O_NONBLOCK } else { 0 } | super::fd::O_RDWR;
    let desc = Description::new(file, status);
    t.process.files.lock().insert(desc, flags & SOCK_CLOEXEC != 0).map(|fd| fd as u64)
}

fn socket_of(t: &Arc<Thread>, fd: i64) -> LxResult<(Arc<dyn FileOps>, Arc<Description>)> {
    let entry = t.process.files.lock().get(fd)?;
    let file = entry.desc.file.clone();
    if file.kind() != Kind::Socket {
        return Err(ENOTSOCK);
    }
    Ok((file, entry.desc))
}

pub fn socket(t: &Arc<Thread>, family: u64, kind: u64, _protocol: u64) -> LxResult<u64> {
    let base = kind & 0xF;
    match family {
        AF_UNIX => {
            if !matches!(base, SOCK_STREAM | SOCK_DGRAM | SOCK_SEQPACKET) {
                return Err(EINVAL);
            }
            let s = UnixSocket::new(base, Arc::new(WaitQueue::new()));
            install(t, s, kind)
        }
        AF_INET => {
            if !matches!(base, SOCK_STREAM | SOCK_DGRAM) {
                return Err(EPROTONOSUPPORT);
            }
            install(t, InetSocket::new(base), kind)
        }
        _ => Err(EAFNOSUPPORT),
    }
}

pub fn socketpair(t: &Arc<Thread>, family: u64, kind: u64, _protocol: u64, out: u64) -> LxResult<u64> {
    if family != AF_UNIX {
        return Err(EOPNOTSUPP);
    }
    let base = kind & 0xF;
    if !matches!(base, SOCK_STREAM | SOCK_DGRAM | SOCK_SEQPACKET) {
        return Err(EINVAL);
    }
    let queue = Arc::new(WaitQueue::new());
    let a = UnixSocket::new(base, queue.clone());
    let b = UnixSocket::new(base, queue);
    connect_pair(&a, &b);
    let fa = install(t, a, kind)?;
    let fb = match install(t, b, kind) {
        Ok(fd) => fd,
        Err(e) => {
            let _ = t.process.files.lock().remove(fa as i64);
            return Err(e);
        }
    };
    crate::vm::write_user(out, &[fa as i32, fb as i32]).map_err(|_| EFAULT)?;
    Ok(0)
}

pub fn bind(t: &Arc<Thread>, fd: i64, addr: u64, len: u64) -> LxResult<u64> {
    let (file, _) = socket_of(t, fd)?;
    match read_address(addr, len)? {
        Address::Unix(name) => {
            let s = file.as_any().downcast_ref::<UnixSocket>().ok_or(EINVAL)?;
            let listeners = LISTENERS.lock();
            if listeners.get(&name).is_some_and(|w| w.strong_count() > 0) {
                return Err(EADDRINUSE);
            }
            *s.name.lock() = Some(name);
            Ok(0)
        }
        Address::Inet(_, port) => {
            let s = file.as_any().downcast_ref::<InetSocket>().ok_or(EINVAL)?;
            if s.kind == SOCK_DGRAM {
                let (h, p) = crate::net::udp::bind(port).map_err(|_| EADDRINUSE)?;
                *s.udp.lock() = Some(h);
                s.local_port.store(p as u32, Ordering::Relaxed);
            } else {
                s.local_port.store(port as u32, Ordering::Relaxed);
            }
            Ok(0)
        }
    }
}

pub fn listen(t: &Arc<Thread>, fd: i64, _backlog: u64) -> LxResult<u64> {
    let (file, _) = socket_of(t, fd)?;
    let s = file.as_any().downcast_ref::<UnixSocket>().ok_or(EOPNOTSUPP)?;
    let name = s.name.lock().clone().ok_or(EINVAL)?;
    *s.state.lock() = UnixState::Listening { backlog: VecDeque::new() };
    // The table holds a weak reference: a listener closed is forgotten.
    let entry = t.process.files.lock().get(fd)?;
    let strong: Arc<dyn FileOps> = entry.desc.file.clone();
    let any: &dyn Any = strong.as_any();
    if any.downcast_ref::<UnixSocket>().is_some() {
        // Rebuild an Arc<UnixSocket> from the description's file.
        let raw = Arc::into_raw(strong) as *const UnixSocket;
        // SAFETY: the downcast above proved the concrete type.
        let socket = unsafe { Arc::from_raw(raw) };
        LISTENERS.lock().insert(name, Arc::downgrade(&socket));
    }
    Ok(0)
}

pub fn accept(t: &Arc<Thread>, fd: i64, addr: u64, len_addr: u64, flags: u64) -> LxResult<u64> {
    let (file, desc) = socket_of(t, fd)?;
    let s = file.as_any().downcast_ref::<UnixSocket>().ok_or(EOPNOTSUPP)?;
    let nonblock = desc.nonblock();
    let take = || -> Option<LxResult<Arc<UnixSocket>>> {
        match &mut *s.state.lock() {
            UnixState::Listening { backlog } => match backlog.pop_front() {
                Some(c) => Some(Ok(c)),
                None if nonblock => Some(Err(EAGAIN)),
                None => None,
            },
            _ => Some(Err(EINVAL)),
        }
    };
    let conn = wait::wait_until(&[&s.queue], None, take).map_err(wait_err)??;
    write_address(addr, len_addr, &unix_bytes(None))?;
    install(t, conn, flags)
}

pub fn connect(t: &Arc<Thread>, fd: i64, addr: u64, len: u64) -> LxResult<u64> {
    let (file, desc) = socket_of(t, fd)?;
    match read_address(addr, len)? {
        Address::Unix(name) => {
            let s = file.as_any().downcast_ref::<UnixSocket>().ok_or(EINVAL)?;
            let listener = LISTENERS.lock().get(&name).and_then(|w| w.upgrade()).ok_or(ENOENT)?;
            // The server's end of the connection, queued for accept. Both
            // ends share a queue, as a socketpair does.
            let server_end = UnixSocket::new(s.kind, Arc::new(WaitQueue::new()));
            // Rebuild our own Arc from the description.
            let raw = Arc::into_raw(file.clone()) as *const UnixSocket;
            // SAFETY: downcast checked above.
            let client = unsafe { Arc::from_raw(raw) };
            let client_end = client;
            // Link: messages flow through two fresh channels.
            let ab = Channel::new();
            let ba = Channel::new();
            *client_end.state.lock() = UnixState::Connected { rx: ba.clone(), tx: ab.clone() };
            *server_end.state.lock() = UnixState::Connected { rx: ab, tx: ba };
            match &mut *listener.state.lock() {
                UnixState::Listening { backlog } => backlog.push_back(server_end),
                _ => return Err(ECONNREFUSED),
            }
            listener.queue.notify_all();
            Ok(0)
        }
        Address::Inet(ip, port) => {
            let s = file.as_any().downcast_ref::<InetSocket>().ok_or(EINVAL)?;
            if s.kind == SOCK_DGRAM {
                *s.peer.lock() = Some((ip, port));
                s.udp_handle()?;
                return Ok(0);
            }
            {
                let mut phase = s.tcp.lock();
                match *phase {
                    TcpPhase::Fresh => *phase = TcpPhase::Connecting,
                    TcpPhase::Connecting => return Err(EALREADY),
                    TcpPhase::Connected(_) => return Err(EISCONN),
                    _ => return Err(EINVAL),
                }
            }
            *s.peer.lock() = Some((ip, port));
            let raw = Arc::into_raw(file.clone()) as *const InetSocket;
            // SAFETY: downcast checked above.
            let socket = unsafe { Arc::from_raw(raw) };
            CONNECTS.lock().push((socket, ip, port));
            crate::scheduler::spawn("linux-connect", crate::process::BinaryType::Cus, connect_helper);
            if desc.nonblock() {
                return Err(EINPROGRESS);
            }
            let done = || -> Option<LxResult<u64>> {
                match *s.tcp.lock() {
                    TcpPhase::Connected(_) => Some(Ok(0)),
                    TcpPhase::Failed(e) => Some(Err(e)),
                    _ => None,
                }
            };
            wait::wait_until(&[&NET], None, done).map_err(wait_err)?
        }
    }
}

pub fn shutdown(t: &Arc<Thread>, fd: i64, how: u64) -> LxResult<u64> {
    let (file, _) = socket_of(t, fd)?;
    if let Some(s) = file.as_any().downcast_ref::<UnixSocket>() {
        if let Some((rx, tx)) = s.channels() {
            if how == 1 || how == 2 {
                tx.closed.store(true, Ordering::Release);
            }
            if how == 0 || how == 2 {
                rx.closed.store(true, Ordering::Release);
            }
            s.queue.notify_all();
        }
        return Ok(0);
    }
    if let Some(s) = file.as_any().downcast_ref::<InetSocket>()
        && (how == 1 || how == 2)
    {
        s.shut_write.store(true, Ordering::Release);
    }
    Ok(0)
}

pub fn getsockname(t: &Arc<Thread>, fd: i64, addr: u64, len: u64) -> LxResult<u64> {
    let (file, _) = socket_of(t, fd)?;
    let bytes = if let Some(s) = file.as_any().downcast_ref::<UnixSocket>() {
        unix_bytes(s.name.lock().as_deref())
    } else if let Some(s) = file.as_any().downcast_ref::<InetSocket>() {
        inet_bytes(crate::net::interface().ip, s.local_port.load(Ordering::Relaxed) as u16)
    } else {
        return Err(ENOTSOCK);
    };
    write_address(addr, len, &bytes)?;
    Ok(0)
}

pub fn getpeername(t: &Arc<Thread>, fd: i64, addr: u64, len: u64) -> LxResult<u64> {
    let (file, _) = socket_of(t, fd)?;
    let bytes = if let Some(s) = file.as_any().downcast_ref::<UnixSocket>() {
        if s.channels().is_none() {
            return Err(ENOTCONN);
        }
        unix_bytes(None)
    } else if let Some(s) = file.as_any().downcast_ref::<InetSocket>() {
        let (ip, port) = s.peer.lock().ok_or(ENOTCONN)?;
        inet_bytes(ip, port)
    } else {
        return Err(ENOTSOCK);
    };
    write_address(addr, len, &bytes)?;
    Ok(0)
}

pub fn getsockopt(t: &Arc<Thread>, fd: i64, level: u64, name: u64, value: u64, len_addr: u64) -> LxResult<u64> {
    const SO_TYPE: u64 = 3;
    const SO_ERROR: u64 = 4;
    const SO_SNDBUF: u64 = 7;
    const SO_RCVBUF: u64 = 8;
    const SO_PEERCRED: u64 = 17;
    const SO_ACCEPTCONN: u64 = 30;
    const SO_DOMAIN: u64 = 39;
    let (file, _) = socket_of(t, fd)?;
    let unix = file.as_any().downcast_ref::<UnixSocket>();
    let inet = file.as_any().downcast_ref::<InetSocket>();
    let bytes: Vec<u8> = match (level, name) {
        (SOL_SOCKET, SO_TYPE) => (unix.map(|s| s.kind).or(inet.map(|s| s.kind)).unwrap_or(1) as u32).to_le_bytes().to_vec(),
        (SOL_SOCKET, SO_ERROR) => {
            let e = inet.map(|s| core::mem::take(&mut *s.error.lock())).unwrap_or(0);
            (e as u32).to_le_bytes().to_vec()
        }
        (SOL_SOCKET, SO_SNDBUF | SO_RCVBUF) => 212_992u32.to_le_bytes().to_vec(),
        (SOL_SOCKET, SO_DOMAIN) => (if unix.is_some() { AF_UNIX } else { AF_INET } as u32).to_le_bytes().to_vec(),
        (SOL_SOCKET, SO_ACCEPTCONN) => {
            let listening = unix.is_some_and(|s| matches!(&*s.state.lock(), UnixState::Listening { .. }));
            (listening as u32).to_le_bytes().to_vec()
        }
        (SOL_SOCKET, SO_PEERCRED) => {
            let pid = unix.map(|s| s.peer_pid.load(Ordering::Relaxed)).unwrap_or(0) as u32;
            let tgid = super::thread_by_tid(pid as u64).map_or(pid, |th| th.process.tgid as u32);
            let mut v = tgid.to_le_bytes().to_vec();
            v.extend_from_slice(&0u32.to_le_bytes());
            v.extend_from_slice(&0u32.to_le_bytes());
            v
        }
        _ => 0u32.to_le_bytes().to_vec(),
    };
    write_address(value, len_addr, &bytes)?;
    Ok(0)
}

pub fn setsockopt(t: &Arc<Thread>, fd: i64, _level: u64, _name: u64, _value: u64, _len: u64) -> LxResult<u64> {
    // Buffer sizes, keepalive, TCP_NODELAY, SO_REUSEADDR, SO_PASSCRED:
    // accepted; the stack has one behaviour for each.
    socket_of(t, fd)?;
    Ok(0)
}

// ============================================================
//  sendmsg / recvmsg / sendto / recvfrom
// ============================================================

struct MsgHeader {
    name: u64,
    name_len: u32,
    iov: u64,
    iov_len: u64,
    control: u64,
    control_len: u64,
}

fn read_msghdr(addr: u64) -> LxResult<MsgHeader> {
    let raw: [u64; 7] = crate::vm::read_user(addr).map_err(|_| EFAULT)?;
    Ok(MsgHeader { name: raw[0], name_len: raw[1] as u32, iov: raw[2], iov_len: raw[3], control: raw[4], control_len: raw[5] })
}

fn read_iov(addr: u64, count: u64) -> LxResult<Vec<(u64, u64)>> {
    if count > 1024 {
        return Err(EINVAL);
    }
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let pair: [u64; 2] = crate::vm::read_user(addr + i * 16).map_err(|_| EFAULT)?;
        out.push((pair[0], pair[1]));
    }
    Ok(out)
}

fn scatter(iov: &[(u64, u64)], data: &[u8]) -> LxResult<()> {
    let mut done = 0usize;
    for (base, len) in iov {
        if done >= data.len() {
            break;
        }
        let n = (*len as usize).min(data.len() - done);
        crate::vm::copy_to_user(*base, &data[done..done + n]).map_err(|_| EFAULT)?;
        done += n;
    }
    Ok(())
}

pub fn sendmsg(t: &Arc<Thread>, fd: i64, msg: u64, flags: u64) -> LxResult<u64> {
    let (file, desc) = socket_of(t, fd)?;
    let h = read_msghdr(msg)?;
    let iov = read_iov(h.iov, h.iov_len)?;
    let nonblock = desc.nonblock() || flags & MSG_DONTWAIT != 0;

    // Descriptors to pass (SCM_RIGHTS).
    let mut files = Vec::new();
    if h.control != 0 && h.control_len >= 16 {
        let mut control = alloc::vec![0u8; h.control_len.min(4096) as usize];
        crate::vm::copy_from_user(&mut control, h.control).map_err(|_| EFAULT)?;
        let mut at = 0;
        while at + 16 <= control.len() {
            let len = u64::from_le_bytes(control[at..at + 8].try_into().unwrap()) as usize;
            let level = u32::from_le_bytes(control[at + 8..at + 12].try_into().unwrap());
            let kind = u32::from_le_bytes(control[at + 12..at + 16].try_into().unwrap());
            if len < 16 || at + len > control.len() {
                break;
            }
            if level as u64 == SOL_SOCKET && kind == SCM_RIGHTS {
                let table = t.process.files.lock();
                for chunk in control[at + 16..at + len].chunks(4) {
                    if chunk.len() == 4 {
                        let passed = i32::from_le_bytes(chunk.try_into().unwrap());
                        files.push(table.get(passed as i64)?.desc);
                    }
                }
            }
            at += (len + 7) & !7;
        }
    }

    let to = match file.as_any().downcast_ref::<InetSocket>() {
        Some(s) if s.kind == SOCK_DGRAM && h.name != 0 => match read_address(h.name, h.name_len as u64)? {
            Address::Inet(ip, port) => Some((ip, port)),
            _ => return Err(EAFNOSUPPORT),
        },
        _ => None,
    };
    send_iov(&file, &iov, files, nonblock, to, flags)
}

/// Largest payload of a UDP datagram (65 535 - 8 - 20).
const UDP_MAX: usize = 65_507;

/// Sends what `iov` describes, a piece at a time (see
/// `files::user_buffer`): a message is never gathered whole in the kernel
/// heap, where a 5.6 MiB IPC message of the browser once stopped the
/// kernel. A stream takes pieces of its buffer size until all is sent (or,
/// non-blocking, until it is full); a datagram must fit whole, as on
/// Linux (`EMSGSIZE`). `files` go with the first piece.
fn send_iov(
    file: &Arc<dyn FileOps>,
    iov: &[(u64, u64)],
    files: Vec<Arc<Description>>,
    nonblock: bool,
    to: Option<(Ipv4, u16)>,
    flags: u64,
) -> LxResult<u64> {
    let total = super::files::iov_total(iov);
    let unix = file.as_any().downcast_ref::<UnixSocket>();
    let inet = file.as_any().downcast_ref::<InetSocket>();
    let (stream, piece) = match (unix, inet) {
        (Some(s), _) => (s.kind == SOCK_STREAM, UNIX_BUFFER as usize),
        (None, Some(s)) => (s.kind != SOCK_DGRAM, CHUNK),
        (None, None) => return Err(ENOTSOCK),
    };
    if !stream {
        let limit = if unix.is_some() { UNIX_BUFFER as usize } else { UDP_MAX };
        if total > limit as u64 {
            return Err(EMSGSIZE);
        }
    }
    let send = |data: &[u8], files: Vec<Arc<Description>>| -> LxResult<usize> {
        let result = match (unix, inet, to) {
            (Some(s), _, _) => s.send_message(data, files, nonblock),
            (None, Some(s), Some(to)) => udp_send(s, data, to),
            _ => file.write(data, nonblock),
        };
        if result == Err(EPIPE) && flags & MSG_NOSIGNAL == 0 && unix.is_some() {
            super::raise_sigpipe();
        }
        result
    };
    if !stream {
        let data = super::files::gather_range(iov, 0, total as usize)?;
        return send(&data, files).map(|n| n as u64);
    }
    let mut files = Some(files);
    let mut done = 0u64;
    loop {
        let data = super::files::gather_range(iov, done, piece)?;
        match send(&data, files.take().unwrap_or_default()) {
            Ok(n) => {
                done += n as u64;
                if done >= total || n < data.len() && nonblock || n == 0 {
                    break;
                }
            }
            Err(e) if done == 0 => return Err(e),
            Err(_) => break,
        }
    }
    Ok(done)
}

/// Largest piece a stream socket copies through the kernel at once.
const CHUNK: usize = 256 << 10;

/// What one receive may copy through the kernel: all a Unix socket can
/// hold, a whole UDP datagram, a piece of a TCP stream.
fn receive_limit(file: &Arc<dyn FileOps>) -> usize {
    if file.as_any().downcast_ref::<UnixSocket>().is_some() {
        return UNIX_BUFFER as usize;
    }
    match file.as_any().downcast_ref::<InetSocket>() {
        Some(s) if s.kind == SOCK_DGRAM => 65_536,
        _ => CHUNK,
    }
}

pub fn recvmsg(t: &Arc<Thread>, fd: i64, msg: u64, flags: u64) -> LxResult<u64> {
    let (file, desc) = socket_of(t, fd)?;
    let h = read_msghdr(msg)?;
    let iov = read_iov(h.iov, h.iov_len)?;
    let total = super::files::iov_total(&iov).min(receive_limit(&file) as u64);
    let mut buf = super::files::user_buffer(total as usize)?;
    let nonblock = desc.nonblock() || flags & MSG_DONTWAIT != 0;
    let peek = flags & MSG_PEEK != 0;
    let mut out_flags = 0u32;

    let (n, files, from) = if let Some(s) = file.as_any().downcast_ref::<UnixSocket>() {
        let (n, files, truncated) = s.recv_message(&mut buf, nonblock, peek)?;
        if truncated {
            out_flags |= MSG_TRUNC;
        }
        (n, files, None)
    } else if let Some(s) = file.as_any().downcast_ref::<InetSocket>() {
        if s.kind == SOCK_DGRAM {
            let (n, from) = udp_recv(s, &mut buf, nonblock)?;
            (n, Vec::new(), Some(from))
        } else {
            (file.read(&mut buf, nonblock)?, Vec::new(), None)
        }
    } else {
        return Err(ENOTSOCK);
    };
    scatter(&iov, &buf[..n])?;

    // Passed descriptors become new descriptors here.
    let mut control_used = 0u64;
    if !files.is_empty() {
        let room = h.control_len.saturating_sub(16) / 4;
        let cloexec = flags & MSG_CMSG_CLOEXEC != 0;
        let mut fds: Vec<i32> = Vec::new();
        {
            let mut table = t.process.files.lock();
            for (i, d) in files.into_iter().enumerate() {
                if (i as u64) < room {
                    fds.push(table.insert(d, cloexec)? as i32);
                } else {
                    out_flags |= MSG_CTRUNC;
                }
            }
        }
        let len = 16 + fds.len() * 4;
        let mut control = Vec::with_capacity((len + 7) & !7);
        control.extend_from_slice(&(len as u64).to_le_bytes());
        control.extend_from_slice(&(SOL_SOCKET as u32).to_le_bytes());
        control.extend_from_slice(&SCM_RIGHTS.to_le_bytes());
        for fd in &fds {
            control.extend_from_slice(&fd.to_le_bytes());
        }
        control.resize((len + 7) & !7, 0);
        crate::vm::copy_to_user(h.control, &control).map_err(|_| EFAULT)?;
        control_used = control.len() as u64;
    }
    // msg_controllen, msg_flags
    crate::vm::write_user(msg + 40, &control_used).map_err(|_| EFAULT)?;
    crate::vm::write_user(msg + 48, &out_flags).map_err(|_| EFAULT)?;
    if h.name != 0 {
        let bytes = match from {
            Some((ip, port)) => inet_bytes(ip, port),
            None => unix_bytes(None),
        };
        let n = (h.name_len as usize).min(bytes.len());
        crate::vm::copy_to_user(h.name, &bytes[..n]).map_err(|_| EFAULT)?;
        crate::vm::write_user(msg + 8, &(bytes.len() as u32)).map_err(|_| EFAULT)?;
    }
    Ok(n as u64)
}

pub fn sendto(t: &Arc<Thread>, fd: i64, buf: u64, len: u64, flags: u64, addr: u64, addr_len: u64) -> LxResult<u64> {
    let (file, desc) = socket_of(t, fd)?;
    let nonblock = desc.nonblock() || flags & MSG_DONTWAIT != 0;
    let to = match file.as_any().downcast_ref::<InetSocket>() {
        Some(s) if s.kind == SOCK_DGRAM && addr != 0 => match read_address(addr, addr_len)? {
            Address::Inet(ip, port) => Some((ip, port)),
            _ => return Err(EAFNOSUPPORT),
        },
        _ => None,
    };
    send_iov(&file, &[(buf, len)], Vec::new(), nonblock, to, flags)
}

pub fn recvfrom(t: &Arc<Thread>, fd: i64, buf: u64, len: u64, flags: u64, addr: u64, addr_len: u64) -> LxResult<u64> {
    let (file, desc) = socket_of(t, fd)?;
    let mut data = super::files::user_buffer(len.min(receive_limit(&file) as u64) as usize)?;
    let nonblock = desc.nonblock() || flags & MSG_DONTWAIT != 0;
    let (n, from) = if let Some(s) = file.as_any().downcast_ref::<InetSocket>()
        && s.kind == SOCK_DGRAM
    {
        let (n, from) = udp_recv(s, &mut data, nonblock)?;
        (n, Some(from))
    } else if let Some(s) = file.as_any().downcast_ref::<UnixSocket>() {
        (s.recv_message(&mut data, nonblock, flags & MSG_PEEK != 0)?.0, None)
    } else {
        (file.read(&mut data, nonblock)?, None)
    };
    crate::vm::copy_to_user(buf, &data[..n]).map_err(|_| EFAULT)?;
    if let Some((ip, port)) = from {
        write_address(addr, addr_len, &inet_bytes(ip, port))?;
    }
    Ok(n as u64)
}

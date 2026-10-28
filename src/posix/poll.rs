//! Waiting on several descriptors: `poll`, `select`, `epoll`.
//!
//! All three come down to `wait::wait_until` over the wait queues of the
//! objects concerned, with a readiness test that asks each object its
//! `FileOps::poll` mask. What differs is only how the interest is given
//! and how the answer is written back.
//!
//! ## Edge-triggered epoll
//!
//! `EPOLLET` means "tell me when something happens", not "while it is
//! true". A socket is writable almost all the time: reported level-
//! triggered, an event loop registered for `EPOLLOUT | EPOLLET` would spin.
//! Each wait queue counts its notifications (`WaitQueue::generation`); an
//! edge-triggered interest is reported again only once that count has
//! moved since its last report.

use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;

use super::errno::*;
use super::fd::{self, FileOps, POLLERR, POLLHUP, POLLIN, POLLNVAL, POLLOUT, POLLPRI};
use super::vfs::{Kind, Stat};
use super::wait::{self, WaitQueue};
use super::Thread;
use crate::vga::SpinLock;

fn deadline_from_ms(ms: i64) -> Option<u64> {
    (ms >= 0).then(|| super::time::deadline_after_ns(ms as u64 * 1_000_000))
}

fn deadline_from_timespec(addr: u64) -> LxResult<Option<u64>> {
    if addr == 0 {
        return Ok(None);
    }
    let ts: super::time::Timespec = crate::vm::read_user(addr).map_err(|_| EFAULT)?;
    let ns = ts.to_ns().ok_or(EINVAL)?;
    Ok(Some(super::time::deadline_after_ns(ns)))
}

/// Runs `body` with the signal mask temporarily replaced (`ppoll`,
/// `pselect6`, `epoll_pwait` with a mask argument).
fn with_mask<T>(t: &Arc<Thread>, mask_addr: u64, body: impl FnOnce() -> T) -> LxResult<T> {
    use core::sync::atomic::Ordering;
    if mask_addr == 0 {
        return Ok(body());
    }
    let mask: u64 = crate::vm::read_user(mask_addr).map_err(|_| EFAULT)?;
    let old = t.sigmask.swap(mask & !((1 << 8) | (1 << 18)), Ordering::AcqRel);
    let result = body();
    t.sigmask.store(old, Ordering::Release);
    Ok(result)
}

fn wait_error(e: wait::WaitError) -> i64 {
    match e {
        wait::WaitError::Interrupted => EINTR,
        wait::WaitError::TimedOut => 0,
    }
}

// ============================================================
//  poll / ppoll
// ============================================================

struct Polled {
    file: Option<Arc<dyn FileOps>>,
    events: u32,
}

fn do_poll(t: &Arc<Thread>, fds_addr: u64, nfds: u64, deadline: Option<u64>) -> LxResult<u64> {
    if nfds > 65536 {
        return Err(EINVAL);
    }
    let mut raw = alloc::vec![0u8; nfds as usize * 8];
    crate::vm::copy_from_user(&mut raw, fds_addr).map_err(|_| EFAULT)?;
    let entries: Vec<(i32, u32)> = raw
        .chunks(8)
        .map(|c| (i32::from_le_bytes(c[0..4].try_into().unwrap()), u16::from_le_bytes(c[4..6].try_into().unwrap()) as u32))
        .collect();
    let polled: Vec<Polled> = {
        let files = t.process.files.lock();
        entries
            .iter()
            .map(|(fd, events)| Polled {
                file: if *fd < 0 { None } else { files.get(*fd as i64).ok().map(|e| e.desc.file.clone()) },
                events: *events,
            })
            .collect()
    };
    let queues: Vec<&WaitQueue> = polled.iter().filter_map(|p| p.file.as_ref().and_then(|f| f.wait_queue())).collect();
    let mut revents = alloc::vec![0u32; polled.len()];
    let scan = |revents: &mut Vec<u32>| -> usize {
        let mut ready = 0;
        for (i, p) in polled.iter().enumerate() {
            let r = match (&p.file, entries[i].0) {
                (_, fd) if fd < 0 => 0,
                (None, _) => POLLNVAL,
                (Some(f), _) => f.poll() & (p.events | POLLERR | POLLHUP),
            };
            revents[i] = r;
            if r != 0 {
                ready += 1;
            }
        }
        ready
    };
    let result = wait::wait_until(&queues, deadline, || {
        let n = scan(&mut revents);
        (n > 0).then_some(n)
    });
    let count = match result {
        Ok(n) => n,
        Err(wait::WaitError::TimedOut) => {
            scan(&mut revents);
            0
        }
        Err(e) => return Err(wait_error(e)),
    };
    for (i, r) in revents.iter().enumerate() {
        raw[i * 8 + 6..i * 8 + 8].copy_from_slice(&(*r as u16).to_le_bytes());
    }
    crate::vm::copy_to_user(fds_addr, &raw).map_err(|_| EFAULT)?;
    Ok(count as u64)
}

pub fn poll(t: &Arc<Thread>, fds: u64, nfds: u64, timeout_ms: i64) -> LxResult<u64> {
    do_poll(t, fds, nfds, deadline_from_ms(timeout_ms))
}

pub fn ppoll(t: &Arc<Thread>, fds: u64, nfds: u64, timeout: u64, mask: u64) -> LxResult<u64> {
    let deadline = deadline_from_timespec(timeout)?;
    with_mask(t, mask, || do_poll(t, fds, nfds, deadline))?
}

// ============================================================
//  select / pselect6
// ============================================================

fn do_select(t: &Arc<Thread>, nfds: u64, sets: [u64; 3], deadline: Option<u64>) -> LxResult<u64> {
    if nfds > 1024 {
        return Err(EINVAL);
    }
    let bytes = nfds.div_ceil(64) as usize * 8;
    let mut input = [alloc::vec![0u8; bytes], alloc::vec![0u8; bytes], alloc::vec![0u8; bytes]];
    for (k, addr) in sets.iter().enumerate() {
        if *addr != 0 {
            crate::vm::copy_from_user(&mut input[k], *addr).map_err(|_| EFAULT)?;
        }
    }
    let is_set = |set: &[u8], fd: usize| set[fd / 8] & (1 << (fd % 8)) != 0;
    let mut watched: Vec<(usize, Arc<dyn FileOps>, u32)> = Vec::new();
    {
        let files = t.process.files.lock();
        for fd in 0..nfds as usize {
            let mut events = 0;
            if is_set(&input[0], fd) {
                events |= POLLIN;
            }
            if is_set(&input[1], fd) {
                events |= POLLOUT;
            }
            if is_set(&input[2], fd) {
                events |= POLLPRI;
            }
            if events != 0 {
                let file = files.get(fd as i64).map_err(|_| EBADF)?.desc.file.clone();
                watched.push((fd, file, events));
            }
        }
    }
    let queues: Vec<&WaitQueue> = watched.iter().filter_map(|(_, f, _)| f.wait_queue()).collect();
    let mut output = [alloc::vec![0u8; bytes], alloc::vec![0u8; bytes], alloc::vec![0u8; bytes]];
    let scan = |output: &mut [Vec<u8>; 3]| -> usize {
        for o in output.iter_mut() {
            o.fill(0);
        }
        let mut count = 0;
        for (fd, file, events) in &watched {
            let r = file.poll();
            if events & POLLIN != 0 && r & (POLLIN | POLLHUP | POLLERR) != 0 {
                output[0][fd / 8] |= 1 << (fd % 8);
                count += 1;
            }
            if events & POLLOUT != 0 && r & (POLLOUT | POLLERR) != 0 {
                output[1][fd / 8] |= 1 << (fd % 8);
                count += 1;
            }
            if events & POLLPRI != 0 && r & POLLPRI != 0 {
                output[2][fd / 8] |= 1 << (fd % 8);
                count += 1;
            }
        }
        count
    };
    let result = wait::wait_until(&queues, deadline, || {
        let n = scan(&mut output);
        (n > 0).then_some(n)
    });
    let count = match result {
        Ok(n) => n,
        Err(wait::WaitError::TimedOut) => {
            for o in output.iter_mut() {
                o.fill(0);
            }
            0
        }
        Err(e) => return Err(wait_error(e)),
    };
    for (k, addr) in sets.iter().enumerate() {
        if *addr != 0 {
            crate::vm::copy_to_user(*addr, &output[k]).map_err(|_| EFAULT)?;
        }
    }
    Ok(count as u64)
}

pub fn select(t: &Arc<Thread>, nfds: u64, r: u64, w: u64, e: u64, timeval: u64) -> LxResult<u64> {
    let deadline = if timeval == 0 {
        None
    } else {
        let tv: [i64; 2] = crate::vm::read_user(timeval).map_err(|_| EFAULT)?;
        if tv[0] < 0 || tv[1] < 0 {
            return Err(EINVAL);
        }
        Some(super::time::deadline_after_ns(tv[0] as u64 * 1_000_000_000 + tv[1] as u64 * 1000))
    };
    do_select(t, nfds, [r, w, e], deadline)
}

pub fn pselect6(t: &Arc<Thread>, nfds: u64, r: u64, w: u64, e: u64, timeout: u64, sigdata: u64) -> LxResult<u64> {
    let deadline = deadline_from_timespec(timeout)?;
    // sigdata points at { const sigset_t *ss; size_t len; }.
    let mask = if sigdata != 0 {
        let d: [u64; 2] = crate::vm::read_user(sigdata).map_err(|_| EFAULT)?;
        d[0]
    } else {
        0
    };
    with_mask(t, mask, || do_select(t, nfds, [r, w, e], deadline))?
}

// ============================================================
//  epoll
// ============================================================

pub const EPOLLIN: u32 = 0x001;
pub const EPOLLOUT: u32 = 0x004;
pub const EPOLLERR: u32 = 0x008;
pub const EPOLLHUP: u32 = 0x010;
pub const EPOLLRDHUP: u32 = 0x2000;
pub const EPOLLEXCLUSIVE: u32 = 1 << 28;
pub const EPOLLWAKEUP: u32 = 1 << 29;
pub const EPOLLONESHOT: u32 = 1 << 30;
pub const EPOLLET: u32 = 1 << 31;

struct Interest {
    file: Weak<dyn FileOps>,
    events: u32,
    data: u64,
    /// Generation of the object's queue at the last report (EPOLLET).
    reported: u64,
    disabled: bool,
}

pub struct Epoll {
    interests: SpinLock<BTreeMap<i64, Interest>>,
    /// Woken on `epoll_ctl`, so a waiter picks up new interests.
    queue: WaitQueue,
}

pub fn new_epoll() -> Arc<Epoll> {
    Arc::new(Epoll { interests: SpinLock::new(BTreeMap::new()), queue: WaitQueue::new() })
}

impl FileOps for Epoll {
    fn stat(&self) -> LxResult<Stat> {
        Ok(Stat { dev: 0xb, mode: 0o600, ..Default::default() })
    }
    fn kind(&self) -> Kind {
        Kind::File
    }
    fn poll(&self) -> u32 {
        // An epoll set is readable when one of its interests is ready.
        let interests = self.interests.lock();
        for i in interests.values() {
            if let Some(f) = i.file.upgrade()
                && !i.disabled
                && f.poll() & (i.events | POLLERR | POLLHUP) != 0
            {
                return POLLIN;
            }
        }
        0
    }
    fn wait_queue(&self) -> Option<&WaitQueue> {
        Some(&self.queue)
    }
    fn describe(&self) -> alloc::string::String {
        alloc::string::String::from("anon_inode:[eventpoll]")
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn epoll_of(t: &Arc<Thread>, epfd: i64) -> LxResult<Arc<dyn FileOps>> {
    let file = t.process.files.lock().get(epfd)?.desc.file.clone();
    if file.as_any().downcast_ref::<Epoll>().is_none() {
        return Err(EINVAL);
    }
    Ok(file)
}

pub fn epoll_ctl(t: &Arc<Thread>, epfd: i64, op: u64, fd: i64, event: u64) -> LxResult<u64> {
    const ADD: u64 = 1;
    const DEL: u64 = 2;
    const MOD: u64 = 3;
    let holder = epoll_of(t, epfd)?;
    let ep = holder.as_any().downcast_ref::<Epoll>().unwrap();
    let target = t.process.files.lock().get(fd)?.desc.file.clone();
    if fd == epfd {
        return Err(EINVAL);
    }
    if target.kind() == Kind::File && target.wait_queue().is_none() && target.as_any().downcast_ref::<fd::OpenFile>().is_some() {
        // Regular files are always ready; Linux refuses them outright.
        return Err(EPERM);
    }
    let (events, data) = if op != DEL {
        let raw: [u8; 12] = {
            let mut b = [0u8; 12];
            crate::vm::copy_from_user(&mut b, event).map_err(|_| EFAULT)?;
            b
        };
        (u32::from_le_bytes(raw[0..4].try_into().unwrap()), u64::from_le_bytes(raw[4..12].try_into().unwrap()))
    } else {
        (0, 0)
    };
    {
        let mut interests = ep.interests.lock();
        match op {
            ADD => {
                if interests.contains_key(&fd) {
                    return Err(EEXIST);
                }
                interests.insert(fd, Interest { file: Arc::downgrade(&target), events, data, reported: u64::MAX, disabled: false });
            }
            MOD => {
                let i = interests.get_mut(&fd).ok_or(ENOENT)?;
                i.events = events;
                i.data = data;
                i.disabled = false;
                i.reported = u64::MAX;
            }
            DEL => {
                interests.remove(&fd).ok_or(ENOENT)?;
            }
            _ => return Err(EINVAL),
        }
    }
    ep.queue.notify_all();
    Ok(0)
}

fn do_epoll_wait(t: &Arc<Thread>, epfd: i64, out: u64, max: i64, deadline: Option<u64>) -> LxResult<u64> {
    if max <= 0 || max > 65536 {
        return Err(EINVAL);
    }
    let holder = epoll_of(t, epfd)?;
    let ep = holder.as_any().downcast_ref::<Epoll>().unwrap();

    loop {
        // The files watched, strong for the duration of this round.
        let files: Vec<(i64, Arc<dyn FileOps>)> = {
            let mut interests = ep.interests.lock();
            let mut dead = Vec::new();
            let mut live = Vec::new();
            for (fd, i) in interests.iter() {
                match i.file.upgrade() {
                    Some(f) => live.push((*fd, f)),
                    None => dead.push(*fd),
                }
            }
            for fd in dead {
                interests.remove(&fd);
            }
            live
        };
        let mut queues: Vec<&WaitQueue> = files.iter().filter_map(|(_, f)| f.wait_queue()).collect();
        queues.push(&ep.queue);
        let ctl_generation = ep.queue.generation();

        let mut ready: Vec<(u32, u64)> = Vec::new();
        let result = wait::wait_until(&queues, deadline, || {
            ready.clear();
            let mut interests = ep.interests.lock();
            for (fd, file) in &files {
                let Some(i) = interests.get_mut(fd) else { continue };
                if i.disabled {
                    continue;
                }
                let r = file.poll() & (i.events | POLLERR | POLLHUP);
                if r == 0 {
                    continue;
                }
                if i.events & EPOLLET != 0 {
                    let generation = file.wait_queue().map_or(0, |q| q.generation());
                    if generation == i.reported {
                        continue;
                    }
                    i.reported = generation;
                }
                if i.events & EPOLLONESHOT != 0 {
                    i.disabled = true;
                }
                ready.push((r, i.data));
                if ready.len() >= max as usize {
                    break;
                }
            }
            if !ready.is_empty() {
                return Some(true);
            }
            // The interest list changed: start over with the new files.
            (ep.queue.generation() != ctl_generation).then_some(false)
        });
        match result {
            Ok(true) => {
                let mut bytes = Vec::with_capacity(ready.len() * 12);
                for (events, data) in &ready {
                    bytes.extend_from_slice(&events.to_le_bytes());
                    bytes.extend_from_slice(&data.to_le_bytes());
                }
                crate::vm::copy_to_user(out, &bytes).map_err(|_| EFAULT)?;
                return Ok(ready.len() as u64);
            }
            Ok(false) => continue,
            Err(wait::WaitError::TimedOut) => return Ok(0),
            Err(e) => return Err(wait_error(e)),
        }
    }
}

pub fn epoll_wait(t: &Arc<Thread>, epfd: i64, out: u64, max: i64, timeout_ms: i64) -> LxResult<u64> {
    do_epoll_wait(t, epfd, out, max, deadline_from_ms(timeout_ms))
}

pub fn epoll_pwait(t: &Arc<Thread>, epfd: i64, out: u64, max: i64, timeout_ms: i64, mask: u64) -> LxResult<u64> {
    let deadline = deadline_from_ms(timeout_ms);
    with_mask(t, mask, || do_epoll_wait(t, epfd, out, max, deadline))?
}

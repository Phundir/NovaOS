//! Pipes and eventfds: the two objects event loops wake each other with.
//!
//! A pipe is a byte ring shared by a read end and a write end. Each end is
//! a separate object, so that closing the last write end is what makes a
//! reader see end of file, and closing the last read end what makes a
//! writer get `EPIPE` -- the counts are the `Arc` counts of the ends.
//!
//! An eventfd is a 64-bit counter: `write` adds, `read` takes it all (or
//! one, in semaphore mode) and blocks at zero. Firefox's thread wake-ups
//! go through one of the two, depending on the library.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::errno::*;
use super::fd::{FileOps, POLLERR, POLLHUP, POLLIN, POLLOUT};
use super::vfs::{Kind, S_IFIFO, Stat};
use super::wait::{self, WaitQueue};
use crate::vga::SpinLock;

/// Linux's default capacity.
const CAPACITY: usize = 64 * 1024;
/// Writes up to this size are atomic (PIPE_BUF).
const PIPE_BUF: usize = 4096;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub struct Pipe {
    id: u64,
    buffer: SpinLock<VecDeque<u8>>,
    readers: AtomicUsize,
    writers: AtomicUsize,
    /// Readers and writers wait on the same queue: every change wakes
    /// both sides, and each re-tests its own condition.
    queue: WaitQueue,
}

pub struct ReadEnd(Arc<Pipe>);
pub struct WriteEnd(Arc<Pipe>);

pub fn new_pipe() -> (Arc<ReadEnd>, Arc<WriteEnd>) {
    let pipe = Arc::new(Pipe {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        buffer: SpinLock::new(VecDeque::new()),
        readers: AtomicUsize::new(1),
        writers: AtomicUsize::new(1),
        queue: WaitQueue::new(),
    });
    (Arc::new(ReadEnd(pipe.clone())), Arc::new(WriteEnd(pipe)))
}

impl Drop for ReadEnd {
    fn drop(&mut self) {
        self.0.readers.fetch_sub(1, Ordering::AcqRel);
        self.0.queue.notify_all();
    }
}

impl Drop for WriteEnd {
    fn drop(&mut self) {
        self.0.writers.fetch_sub(1, Ordering::AcqRel);
        self.0.queue.notify_all();
    }
}

fn wait_result(e: wait::WaitError) -> i64 {
    match e {
        wait::WaitError::Interrupted => EINTR,
        wait::WaitError::TimedOut => EAGAIN,
    }
}

fn fifo_stat(id: u64) -> Stat {
    Stat { dev: 0x9, ino: id, mode: S_IFIFO | 0o600, ..Default::default() }
}

impl FileOps for ReadEnd {
    fn read(&self, buf: &mut [u8], nonblock: bool) -> LxResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let pipe = &self.0;
        let take = || -> Option<LxResult<usize>> {
            let mut data = pipe.buffer.lock();
            if !data.is_empty() {
                let n = data.len().min(buf.len());
                for (slot, b) in buf.iter_mut().zip(data.drain(..n)) {
                    *slot = b;
                }
                return Some(Ok(n));
            }
            if pipe.writers.load(Ordering::Acquire) == 0 {
                return Some(Ok(0));
            }
            if nonblock {
                return Some(Err(EAGAIN));
            }
            None
        };
        let n = wait::wait_until(&[&pipe.queue], None, take).map_err(wait_result)??;
        pipe.queue.notify_all();
        Ok(n)
    }
    fn stat(&self) -> LxResult<Stat> {
        Ok(fifo_stat(self.0.id))
    }
    fn kind(&self) -> Kind {
        Kind::Fifo
    }
    fn poll(&self) -> u32 {
        let mut r = 0;
        if !self.0.buffer.lock().is_empty() {
            r |= POLLIN;
        }
        if self.0.writers.load(Ordering::Acquire) == 0 {
            r |= POLLHUP;
        }
        r
    }
    fn wait_queue(&self) -> Option<&WaitQueue> {
        Some(&self.0.queue)
    }
    fn ioctl(&self, cmd: u64, arg: u64) -> LxResult<u64> {
        const FIONREAD: u64 = 0x541B;
        if cmd == FIONREAD {
            let n = self.0.buffer.lock().len() as u32;
            crate::vm::write_user(arg, &n).map_err(|_| EFAULT)?;
            return Ok(0);
        }
        Err(ENOTTY)
    }
    fn describe(&self) -> String {
        alloc::format!("pipe:[{}]", self.0.id)
    }
}

impl FileOps for WriteEnd {
    fn write(&self, buf: &[u8], nonblock: bool) -> LxResult<usize> {
        let pipe = &self.0;
        let mut written = 0;
        while written < buf.len() {
            let chunk = &buf[written..];
            let put = || -> Option<LxResult<usize>> {
                if pipe.readers.load(Ordering::Acquire) == 0 {
                    return Some(Err(EPIPE));
                }
                let mut data = pipe.buffer.lock();
                let room = CAPACITY - data.len();
                // Small writes go in one piece or not at all.
                let needed = if chunk.len() <= PIPE_BUF { chunk.len() } else { 1 };
                if room >= needed {
                    let n = room.min(chunk.len());
                    data.extend(&chunk[..n]);
                    return Some(Ok(n));
                }
                if nonblock {
                    return Some(Err(EAGAIN));
                }
                None
            };
            match wait::wait_until(&[&pipe.queue], None, put) {
                Ok(Ok(n)) => {
                    written += n;
                    pipe.queue.notify_all();
                }
                Ok(Err(EAGAIN)) if written > 0 => break,
                Ok(Err(e)) => {
                    if e == EPIPE {
                        super::raise_sigpipe();
                    }
                    return if written > 0 { Ok(written) } else { Err(e) };
                }
                Err(e) => return if written > 0 { Ok(written) } else { Err(wait_result(e)) },
            }
            if nonblock {
                break;
            }
        }
        Ok(written)
    }
    fn stat(&self) -> LxResult<Stat> {
        Ok(fifo_stat(self.0.id))
    }
    fn kind(&self) -> Kind {
        Kind::Fifo
    }
    fn poll(&self) -> u32 {
        if self.0.readers.load(Ordering::Acquire) == 0 {
            return POLLOUT | POLLERR;
        }
        if self.0.buffer.lock().len() < CAPACITY {
            POLLOUT
        } else {
            0
        }
    }
    fn wait_queue(&self) -> Option<&WaitQueue> {
        Some(&self.0.queue)
    }
    fn describe(&self) -> String {
        alloc::format!("pipe:[{}]", self.0.id)
    }
}

// ============================================================
//  eventfd
// ============================================================

pub struct EventFd {
    counter: SpinLock<u64>,
    semaphore: bool,
    queue: WaitQueue,
}

pub fn new_eventfd(initial: u64, semaphore: bool) -> Arc<EventFd> {
    Arc::new(EventFd { counter: SpinLock::new(initial), semaphore, queue: WaitQueue::new() })
}

impl FileOps for EventFd {
    fn read(&self, buf: &mut [u8], nonblock: bool) -> LxResult<usize> {
        if buf.len() < 8 {
            return Err(EINVAL);
        }
        let take = || -> Option<LxResult<u64>> {
            let mut c = self.counter.lock();
            if *c > 0 {
                let v = if self.semaphore { 1 } else { *c };
                *c -= v;
                return Some(Ok(v));
            }
            if nonblock {
                return Some(Err(EAGAIN));
            }
            None
        };
        let v = wait::wait_until(&[&self.queue], None, take).map_err(wait_result)??;
        buf[..8].copy_from_slice(&v.to_le_bytes());
        self.queue.notify_all();
        Ok(8)
    }
    fn write(&self, buf: &[u8], nonblock: bool) -> LxResult<usize> {
        if buf.len() < 8 {
            return Err(EINVAL);
        }
        let add = u64::from_le_bytes(buf[..8].try_into().unwrap());
        if add == u64::MAX {
            return Err(EINVAL);
        }
        let put = || -> Option<LxResult<()>> {
            let mut c = self.counter.lock();
            if u64::MAX - 1 - *c >= add {
                *c += add;
                return Some(Ok(()));
            }
            if nonblock {
                return Some(Err(EAGAIN));
            }
            None
        };
        wait::wait_until(&[&self.queue], None, put).map_err(wait_result)??;
        self.queue.notify_all();
        Ok(8)
    }
    fn stat(&self) -> LxResult<Stat> {
        Ok(Stat { dev: 0xa, mode: 0o600, ..Default::default() })
    }
    fn kind(&self) -> Kind {
        Kind::File
    }
    fn poll(&self) -> u32 {
        let c = *self.counter.lock();
        let mut r = 0;
        if c > 0 {
            r |= POLLIN;
        }
        if c < u64::MAX - 1 {
            r |= POLLOUT;
        }
        r
    }
    fn wait_queue(&self) -> Option<&WaitQueue> {
        Some(&self.queue)
    }
    fn describe(&self) -> String {
        String::from("anon_inode:[eventfd]")
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

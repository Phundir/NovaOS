//! `/dev/nova-audio`: the sound of a Linux program, into Nova's mixer.
//!
//! ## Why a device of our own
//!
//! The Web Browser plays sound through ALSA (`libasound`, loaded by its
//! `cubeb` library). Real ALSA speaks to the kernel through a hundred
//! ioctls on `/dev/snd/*`; alsa-lib also accepts *plugins*, a shared
//! library implementing a PCM in user space. So the kernel side is the
//! smallest thing a plugin can drive, and the plugin
//! (`tools/ports/alsa-nova/pcm_nova.c`) does ALSA's bookkeeping.
//!
//! ## The ABI
//!
//! | Call | Effect |
//! |---|---|
//! | `open` | a stream of the mixer (`audio::open`), ENODEV without a sound card |
//! | `write` | interleaved stereo, signed 16-bit little endian, 48 kHz; what fits |
//! | `poll` | `POLLOUT` once the queue is down to the threshold (a quarter free without one) |
//! | `ioctl(PENDING)` | frames queued and not yet mixed |
//! | `ioctl(FLUSH)` | throws the queue away (stop, seek) |
//! | `ioctl(THRESHOLD, n)` | `POLLOUT` only while at most `n` frames are queued |
//! | `ioctl(DELAY)` | frames queued plus those in the card's ring: when a frame written now is heard |
//!
//! ## Why a threshold
//!
//! The queue holds half a second; a player keeps far less in it (Firefox
//! asks ALSA for 40 to 100 ms). With `POLLOUT` meaning "a quarter free",
//! the descriptor was **always** writable, and cubeb's event loop -- poll,
//! find no room in ALSA's own accounting, poll again -- spun: 14 000
//! `poll` a second, a whole core taken from the video decoder for as long
//! as sound played. The plugin now says at what depth it wants to be
//! woken (its buffer minus one period, in device frames), which is what a
//! real sound card's interrupt per period gives ALSA.
//!
//! ## Why a delay
//!
//! A video is timed on its sound: the image shown is the one whose time
//! matches what the loudspeaker plays. The mixer's queue shrinks a whole
//! buffer (1024 frames, 21 ms) at a time, so a clock read from it moves in
//! steps of 21 ms, and images timed on it come in bursts. The card's
//! position inside its buffer moves sample by sample: added to the
//! queue, it gives a clock as smooth as the hardware's.
//!
//! The format is the mixer's own: conversions (Firefox's float samples,
//! its 44.1 kHz) are alsa-lib's `plug` layer, in user space, where
//! floating point is allowed.
//!
//! ## Waking the writer
//!
//! The mixer is drained by the AC'97 driver, which calls `tick` each time
//! it has taken a buffer: a writer waiting for room looks again at the
//! moment room is made. The timer tick calls it too while a stream is
//! open, every 10 ms, as a safety net.

use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicUsize, Ordering};

use super::errno::*;
use super::fd::{FileOps, POLLOUT};
use super::vfs::{Kind, S_IFCHR, Stat};
use super::wait::WaitQueue;
use crate::audio::{self, Handle};

pub const IOCTL_PENDING: u64 = 0x4E41;
pub const IOCTL_FLUSH: u64 = 0x4E42;
pub const IOCTL_THRESHOLD: u64 = 0x4E43;
pub const IOCTL_DELAY: u64 = 0x4E44;

/// No threshold set: the historical "a quarter of the queue free".
const NO_THRESHOLD: usize = usize::MAX;

/// Bytes of one frame: two channels of 16 bits.
const FRAME: usize = 4;
/// Samples a stream queues at most (the mixer's capacity).
const CAPACITY: usize = audio::SAMPLE_RATE as usize / 2 * audio::CHANNELS;

static QUEUE: WaitQueue = WaitQueue::new();
static OPEN: AtomicUsize = AtomicUsize::new(0);

pub struct Device {
    stream: Handle,
    /// Frames accepted, for the log line of `drop`.
    frames: AtomicUsize,
    /// `poll` calls, for the same line: a player whose event loop spins
    /// on a descriptor always ready shows here in the hundreds of
    /// thousands.
    polls: AtomicUsize,
    /// DMA halts of the card seen while this stream was open.
    starved_at_open: u64,
    /// Queue depth, in frames, at or below which the stream is writable.
    threshold: AtomicUsize,
}

pub fn open() -> LxResult<Arc<dyn FileOps>> {
    if !crate::drivers::audio::is_present() {
        crate::serial_println!("[nova-audio] ouverture refusee : aucune carte son");
        return Err(ENODEV);
    }
    let stream = audio::open("web-browser").map_err(|_| EBUSY)?;
    let open = OPEN.fetch_add(1, Ordering::AcqRel) + 1;
    crate::serial_println!(
        "[nova-audio] flux {} ouvert ({} ouvert(s)) - tache {}",
        stream.index(),
        open,
        crate::scheduler::current_pid()
    );
    Ok(Arc::new(Device {
        stream,
        frames: AtomicUsize::new(0),
        polls: AtomicUsize::new(0),
        starved_at_open: crate::drivers::audio::starved(),
        threshold: AtomicUsize::new(NO_THRESHOLD),
    }))
}

/// Timer tick: writers waiting for room look again.
pub fn tick() {
    if OPEN.load(Ordering::Relaxed) != 0 {
        QUEUE.notify_all();
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        let (missing, holes, refused) = audio::stream_counters(self.stream);
        audio::close(self.stream);
        OPEN.fetch_sub(1, Ordering::AcqRel);
        crate::serial_println!(
            "[nova-audio] flux {} ferme - {} trame(s) recue(s) - {} trou(s) ({} echantillons), {} refuses, {} poll, {} arret(s) du DMA",
            self.stream.index(),
            self.frames.load(Ordering::Relaxed),
            holes,
            missing,
            refused,
            self.polls.load(Ordering::Relaxed),
            crate::drivers::audio::starved() - self.starved_at_open
        );
    }
}

impl Device {
    fn room(&self) -> usize {
        CAPACITY.saturating_sub(audio::pending(self.stream))
    }

    fn writable(&self) -> bool {
        let pending = audio::pending(self.stream);
        match self.threshold.load(Ordering::Relaxed) {
            NO_THRESHOLD => CAPACITY.saturating_sub(pending) >= CAPACITY / 4,
            frames => pending / audio::CHANNELS <= frames && pending < CAPACITY,
        }
    }
}

impl FileOps for Device {
    fn write(&self, buf: &[u8], nonblock: bool) -> LxResult<usize> {
        let frames = buf.len() / FRAME;
        if frames == 0 {
            return Ok(0);
        }
        let take = || -> Option<LxResult<usize>> {
            let room = self.room() / audio::CHANNELS;
            if room == 0 {
                return if nonblock { Some(Err(EAGAIN)) } else { None };
            }
            let count = room.min(frames);
            let samples: alloc::vec::Vec<i16> = buf[..count * FRAME]
                .chunks_exact(2)
                .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            Some(match audio::write(self.stream, &samples) {
                Ok(accepted) => {
                    self.frames.fetch_add(accepted / audio::CHANNELS, Ordering::Relaxed);
                    Ok(accepted * 2)
                }
                Err(_) => Err(EIO),
            })
        };
        super::wait::wait_until(&[&QUEUE], None, take).map_err(|_| EINTR)?
    }

    fn poll(&self) -> u32 {
        self.polls.fetch_add(1, Ordering::Relaxed);
        if self.writable() { POLLOUT } else { 0 }
    }

    fn wait_queue(&self) -> Option<&WaitQueue> {
        Some(&QUEUE)
    }

    fn ioctl(&self, cmd: u64, arg: u64) -> LxResult<u64> {
        match cmd {
            IOCTL_PENDING => Ok((audio::pending(self.stream) / audio::CHANNELS) as u64),
            IOCTL_THRESHOLD => {
                let frames = (arg as usize).min(CAPACITY / audio::CHANNELS);
                if self.threshold.swap(frames, Ordering::Relaxed) != frames {
                    crate::serial_println!(
                        "[nova-audio] flux {} : reveil sous {} trames ({} ms)",
                        self.stream.index(),
                        frames,
                        frames * 1000 / audio::SAMPLE_RATE as usize
                    );
                }
                QUEUE.notify_all();
                Ok(0)
            }
            IOCTL_DELAY => {
                let queued = (audio::pending(self.stream) / audio::CHANNELS) as u64;
                Ok(queued + crate::drivers::audio::output_delay_frames() as u64)
            }
            IOCTL_FLUSH => {
                audio::flush(self.stream);
                QUEUE.notify_all();
                Ok(0)
            }
            _ => Err(ENOTTY),
        }
    }

    fn stat(&self) -> LxResult<Stat> {
        Ok(Stat { mode: S_IFCHR | 0o666, rdev: 0xE001, ..Default::default() })
    }

    fn kind(&self) -> Kind {
        Kind::CharDev
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

//! `/dev/nova-gui`: a Nova window for a Linux program (phase 11.8).
//!
//! ## Why a device
//!
//! The Web Browser port draws its pages itself (Gecko's software renderer)
//! and needs three things from the system: a rectangle of pixels on the
//! screen, the keyboard and mouse events aimed at it, and the size it
//! got. A Linux program reaches the kernel through files, so the window is
//! one: opened, configured by `ioctl`, its pixels mapped with `mmap`, its
//! events read with `read` and waited for with `poll`/`epoll` like any
//! other descriptor of an event loop.
//!
//! ## The ABI
//!
//! | Call | Argument | Effect |
//! |---|---|---|
//! | `ioctl(CREATE)` | `{u32 w, u32 h, char title[128]}` | opens the window |
//! | `mmap(fd, w*h*4, MAP_SHARED)` | | the pixels, BGRA, stride `w*4` |
//! | `ioctl(PRESENT)` | `{i32 x, y, w, h}` or NULL | "the pixels changed" |
//! | `ioctl(TITLE)` | `char title[128]` | window title |
//! | `ioctl(CURSOR)` | `u32` shape: 0 arrow, 1 hand, 2 text | pointer shape |
//! | `ioctl(INFO)` | out `{u32 w, h, screen_w, screen_h}` | sizes |
//! | `read` | 32-byte `Event` records | input, resize, close |
//!
//! ## On the Nova side
//!
//! `CREATE` starts a kernel task that runs the ordinary window loop of
//! native applications (`apps::window::run`) with a `Bridge` as its
//! application: painting copies the mapped pixels into a canvas,
//! input becomes `Event` records in a queue the Linux side reads. The
//! pixel buffer is a tmpfs file, so the program's mapping and the
//! bridge's copy are the same frames.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::errno::*;
use super::fd::{FileOps, POLLIN, POLLOUT};
use super::vfs::tmpfs::{TmpFs, TmpNode};
use super::vfs::{Inode, Kind, S_IFCHR, Stat};
use super::wait::WaitQueue;
use crate::apps::window::{App, Press, Ui};
use crate::gui::canvas::Canvas;
use crate::gui::protocol::{CursorShape, KeyEvent};
use crate::vga::SpinLock;

pub const IOCTL_CREATE: u64 = 0x4E01;
pub const IOCTL_PRESENT: u64 = 0x4E02;
pub const IOCTL_TITLE: u64 = 0x4E03;
pub const IOCTL_CURSOR: u64 = 0x4E04;
pub const IOCTL_INFO: u64 = 0x4E05;

pub const EV_MOTION: u32 = 1;
pub const EV_BUTTON_DOWN: u32 = 2;
pub const EV_BUTTON_UP: u32 = 3;
pub const EV_WHEEL: u32 = 4;
pub const EV_KEY_DOWN: u32 = 5;
pub const EV_KEY_UP: u32 = 6;
pub const EV_RESIZE: u32 = 7;
pub const EV_CLOSE: u32 = 8;
pub const EV_FOCUS: u32 = 9;

/// One input record, 32 bytes, little endian:
/// type, x, y, code (button or scancode), modifiers, character, value, 0.
#[derive(Clone, Copy, Default)]
pub struct Event {
    pub kind: u32,
    pub x: i32,
    pub y: i32,
    pub code: u32,
    pub modifiers: u32,
    pub character: u32,
    pub value: i32,
}

impl Event {
    fn bytes(&self) -> [u8; 32] {
        let mut b = [0u8; 32];
        let words = [self.kind, self.x as u32, self.y as u32, self.code, self.modifiers, self.character, self.value as u32, 0];
        for (i, w) in words.iter().enumerate() {
            b[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
        }
        b
    }
}

/// What the Linux side and the bridge task share.
pub struct Window {
    width: AtomicU32,
    height: AtomicU32,
    pixels: Arc<TmpNode>,
    title: SpinLock<String>,
    events: SpinLock<VecDeque<Event>>,
    queue: WaitQueue,
    dirty: AtomicBool,
    cursor: AtomicU32,
    /// The program closed its descriptor: the bridge closes the window.
    released: AtomicBool,
    /// The user closed the window: the program hears EV_CLOSE.
    closed: AtomicBool,
    /// The bridge task, woken by PRESENT: it no longer waits for the next
    /// lap of its 10 ms loop to show a frame.
    bridge: core::sync::atomic::AtomicU64,
    /// Area changed since the bridge's last copy, (x0, y0, x1, y1)
    /// exclusive; the whole window when PRESENT names no rectangle.
    damage: SpinLock<Option<[u32; 4]>>,
}

impl Window {
    fn push(&self, event: Event) {
        if event.kind == EV_KEY_DOWN || event.kind == EV_BUTTON_DOWN {
            let now = super::time::monotonic_ns();
            LAST_INPUT_NS.store(now, Ordering::Relaxed);
            INPUT_PENDING.store(now, Ordering::Relaxed);
            *CPU_AT_INPUT.lock() = linux_cpu();
            *CALLS_AT_INPUT.lock() = Some(counters());
        }
        let mut events = self.events.lock();
        // Motion collapses: only the latest position matters.
        if event.kind == EV_MOTION
            && let Some(last) = events.back_mut()
            && last.kind == EV_MOTION
        {
            *last = event;
        } else if events.len() < 1024 {
            events.push_back(event);
        }
        drop(events);
        self.queue.notify_all();
    }
}

pub struct Device {
    window: SpinLock<Option<Arc<Window>>>,
}

/// The window behind descriptor `fd` of the calling process -- what
/// `SYS_GPU_PRESENT` (spec 6.8) shows a GPU image in.
pub fn window_of_descriptor(fd: i64) -> Option<Arc<Window>> {
    let thread = super::current_thread()?;
    let entry = thread.process.files.lock().get(fd).ok()?;
    let device = entry.desc.file.as_any().downcast_ref::<Device>()?;
    device.window.lock().clone()
}

impl Window {
    /// Width and height, in pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.width.load(Ordering::Relaxed), self.height.load(Ordering::Relaxed))
    }

    /// Writes pixels (B, G, R, X) at the start of row `y`.
    pub fn write_row(&self, y: u32, row: &[u8]) -> LxResult<()> {
        let (w, h) = self.size();
        if y >= h || row.len() > w as usize * 4 {
            return Err(EINVAL);
        }
        self.pixels.write_at(y as u64 * w as u64 * 4, row).map(|_| ())
    }

    /// "The pixels changed", all of them: what `ioctl(PRESENT)` with no
    /// rectangle does.
    pub fn present_all(&self) {
        let (w, h) = self.size();
        *self.damage.lock() = Some([0, 0, w, h]);
        self.dirty.store(true, Ordering::Release);
        let bridge = self.bridge.load(Ordering::Acquire);
        if bridge != 0 {
            crate::scheduler::wake(bridge);
        }
    }
}

pub fn open() -> Arc<dyn FileOps> {
    Arc::new(Device { window: SpinLock::new(None) })
}

impl Drop for Device {
    fn drop(&mut self) {
        if let Some(w) = self.window.lock().take() {
            w.released.store(true, Ordering::Release);
        }
    }
}

/// Instant of the last key press or click, for the latency trace.
static LAST_INPUT_NS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
/// Same instant, until the first image after it is published.
static INPUT_PENDING: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Processor time of every Linux thread at the last key press or click:
/// the base of the breakdown printed when a page's title arrives.
static CPU_AT_INPUT: SpinLock<Vec<(crate::process::Pid, u64)>> = SpinLock::new(Vec::new());
/// System calls, page faults and TLB shootdowns at the same instant.
static CALLS_AT_INPUT: SpinLock<Option<Counters>> = SpinLock::new(None);

struct Counters {
    calls: [u64; crate::linux::syscall::COUNTED],
    /// The same for the calls of the ported programs (`syscall::ported`).
    nova: [u64; crate::syscall::ported::COUNTED],
    faults: u64,
    shootdowns: u64,
}

fn counters() -> Counters {
    Counters {
        calls: crate::linux::syscall::counts(),
        nova: crate::syscall::ported::counts(),
        faults: crate::vm::fault_stats().0,
        shootdowns: crate::memory::tlb_stats().0,
    }
}

/// (thread, processor time in TSC cycles) of the Linux threads alive.
fn linux_cpu() -> Vec<(crate::process::Pid, u64)> {
    crate::scheduler::task_list()
        .into_iter()
        .filter(|t| t.linux)
        .map(|t| (t.pid, t.run_cycles))
        .collect()
}

/// Where the time from the input to the page went: the processor time
/// each Linux thread consumed since the input, the busiest first. When
/// their sum is far below the delay, the page waited -- for the network,
/// a timer, or a core -- rather than computed.
fn cpu_breakdown(elapsed_ms: u64) {
    let base = CPU_AT_INPUT.lock().clone();
    let per_ms = super::time::tsc_per_ms().max(1);
    let mut used: Vec<(u64, crate::scheduler::TaskInfo)> = crate::scheduler::task_list()
        .into_iter()
        .filter(|t| t.linux)
        .filter_map(|t| {
            let before = base.iter().find(|(pid, _)| *pid == t.pid).map_or(0, |(_, c)| *c);
            let us = t.run_cycles.saturating_sub(before) * 1000 / per_ms;
            (us > 0).then_some((us, t))
        })
        .collect();
    used.sort_by(|a, b| b.0.cmp(&a.0));
    let total: u64 = used.iter().map(|(us, _)| us).sum();
    let mut line = alloc::format!("[profil] {} ms apres l'entree : {} us de calcul Linux -", elapsed_ms, total);
    for (us, t) in used.iter().take(6) {
        line.push_str(&alloc::format!(" {} {} ({}) {} us,", t.pid, t.name, t.cpu, us));
    }
    crate::serial_println!("{}", line.trim_end_matches(','));

    // What the kernel was asked meanwhile: the busiest system calls, and
    // the faults and TLB shootdowns they caused.
    let Some(before) = CALLS_AT_INPUT.lock().as_ref().map(|c| {
        let now = counters();
        let linux = (0..crate::linux::syscall::COUNTED)
            .map(|n| (now.calls[n].saturating_sub(c.calls[n]), crate::linux::syscall::name(n as u64)));
        let nova = (0..crate::syscall::ported::COUNTED).map(|n| {
            let number = crate::syscall::ported::number_of(n);
            (now.nova[n].saturating_sub(c.nova[n]), crate::syscall::ported::name(number))
        });
        let mut calls: Vec<(u64, &'static str)> = linux.chain(nova).filter(|(count, _)| *count > 0).collect();
        calls.sort_by(|a, b| b.0.cmp(&a.0));
        (calls, now.faults - c.faults, now.shootdowns - c.shootdowns)
    }) else {
        return;
    };
    let (calls, faults, shootdowns) = before;
    let total: u64 = calls.iter().map(|(count, _)| count).sum();
    let mut line = alloc::format!(
        "[profil]   {} appels systeme, {} fautes de page, {} invalidations de TLB -",
        total, faults, shootdowns
    );
    for (count, name) in calls.iter().take(8) {
        line.push_str(&alloc::format!(" {} {},", name, count));
    }
    crate::serial_println!("{}", line.trim_end_matches(','));
}

/// Windows waiting for their bridge task to pick them up.
static PENDING: SpinLock<Vec<Arc<Window>>> = SpinLock::new(Vec::new());

fn bridge_task() -> ! {
    let window = PENDING.lock().pop();
    let Some(window) = window else {
        crate::scheduler::exit_current_with(-1);
    };
    let title = window.title.lock().clone();
    let (w, h) = (window.width.load(Ordering::Relaxed) as i32, window.height.load(Ordering::Relaxed) as i32);
    crate::serial_println!("[nova-gui] fenetre ouverte {} ms apres le lancement", super::ms_since_browser_launch());
    window.bridge.store(crate::scheduler::current_pid(), Ordering::Release);
    let mut bridge = Bridge {
        frames: 0,
        window,
        canvas: None,
        width: w,
        height: h,
        cursor: CursorShape::Arrow,
        previous: None,
        stats_frames: 0,
        stats_ns: 0,
        stats_since: super::time::monotonic_ns(),
    };
    crate::apps::window::run("nova-gui", &title, w, h, &mut bridge)
}

struct Bridge {
    frames: u64,
    window: Arc<Window>,
    canvas: Option<Canvas>,
    width: i32,
    height: i32,
    cursor: CursorShape,
    /// Area copied into the other buffer of the canvas at the previous
    /// frame: `publish` swaps the two buffers, so the one being filled
    /// now last received the frame before that, and needs both areas.
    previous: Option<[u32; 4]>,
    /// Images and copy time since `stats_since`, for the rate line.
    stats_frames: u64,
    stats_ns: u64,
    stats_since: u64,
}

impl Bridge {
    /// Copies what changed of the program's pixels into the canvas and
    /// publishes it.
    ///
    /// Only the damaged rectangle, united with the previous one (the
    /// canvas is double-buffered), and straight into the canvas: one copy
    /// per row, from the frames of the shared file, then the alpha byte
    /// cleared -- BGRA in memory is 0xAARRGGBB as a little-endian word,
    /// the canvas wants 0x00RRGGBB.
    fn refresh(&mut self) {
        let started = super::time::monotonic_ns();
        let (w, h) = (self.width as usize, self.height as usize);
        let stride = self.window.width.load(Ordering::Acquire) as usize;
        let rows = h.min(self.window.height.load(Ordering::Acquire) as usize);
        let cols = w.min(stride);
        let fresh = self.canvas.is_none();
        let canvas = self.canvas.get_or_insert_with(|| Canvas::new(w, h));
        let full = [0, 0, cols as u32, rows as u32];
        let damage = self.window.damage.lock().take().unwrap_or(full);
        let area = match (fresh, self.previous) {
            (false, Some(p)) => [damage[0].min(p[0]), damage[1].min(p[1]), damage[2].max(p[2]), damage[3].max(p[3])],
            _ => full,
        };
        self.previous = Some(damage);
        let (x0, y0) = (area[0] as usize, area[1] as usize);
        let (x1, y1) = ((area[2] as usize).min(cols), (area[3] as usize).min(rows));
        if x0 < x1 {
            for y in y0..y1 {
                let dst = &mut canvas.pixels[y * w + x0..y * w + x1];
                // SAFETY: a slice of u32 viewed as its bytes, same length.
                let bytes = unsafe { core::slice::from_raw_parts_mut(dst.as_mut_ptr() as *mut u8, dst.len() * 4) };
                if self.window.pixels.read_at(((y * stride + x0) * 4) as u64, bytes).is_err() {
                    break;
                }
                for px in dst.iter_mut() {
                    *px &= 0x00FF_FFFF;
                }
            }
        }
        canvas.publish();
        self.frames += 1;
        // Latency trace: the first image after a key press or a click,
        // with its delay -- what "the browser is slow to answer" means in
        // milliseconds. One line per input: every serial byte is a trap
        // to the hypervisor, and a line per image would slow down what it
        // measures. The page's arrival is traced with its title (TITLE).
        let input = INPUT_PENDING.swap(0, Ordering::Relaxed);
        if input != 0 {
            let elapsed = super::time::monotonic_ns().saturating_sub(input) / 1_000_000;
            if elapsed <= 5000 {
                crate::serial_println!("[nova-gui] image {} ms apres l'entree", elapsed);
            }
        }
        if self.frames == 1 {
            crate::serial_println!("[nova-gui] premiere image {} ms apres le lancement", super::ms_since_browser_launch());
            profile();
        }
        // Rate line, every five seconds while images flow.
        let now = super::time::monotonic_ns();
        self.stats_frames += 1;
        self.stats_ns += now - started;
        if now - self.stats_since >= 5_000_000_000 {
            let seconds = (now - self.stats_since) / 1_000_000;
            crate::serial_println!(
                "[nova-gui] {} images/s - copie moyenne {} us",
                self.stats_frames * 1000 / seconds.max(1),
                self.stats_ns / self.stats_frames.max(1) / 1000
            );
            // Where the images went after the copy: delay to the screen and
            // regularity (see `canvas::Pacing`). The Linux threads' wait
            // for a core once woken is the monitor's line.
            if let Some(canvas) = self.canvas.as_mut() {
                let p = canvas.take_pacing();
                let b = crate::gui::canvas::INTERVAL_BUCKETS_MS;
                crate::serial_println!(
                    "[nova-gui] ecran : {} images, delai moyen {} us max {} us - ecarts <{} {} / <{} {} / <{} {} / <{} {} / <{} {} / plus {} - max {} ms",
                    p.frames,
                    p.latency_sum_ns / p.frames.max(1) / 1000,
                    p.latency_max_ns / 1000,
                    b[0], p.intervals[0], b[1], p.intervals[1], b[2], p.intervals[2],
                    b[3], p.intervals[3], b[4], p.intervals[4], p.intervals[5],
                    p.interval_max_ns / 1_000_000
                );
            }

            self.stats_frames = 0;
            self.stats_ns = 0;
            self.stats_since = now;
        }
    }
}

/// Where the time to the first image went: CPU consumed per task (ticks
/// of 10 ms, counted by the timer on the core running it) and the tar
/// image's disk reads.
fn profile() {
    let mut tasks = crate::scheduler::task_list();
    tasks.sort_by(|a, b| b.ticks_used.cmp(&a.ticks_used));
    for task in tasks.iter().filter(|t| t.ticks_used >= 5).take(16) {
        crate::serial_println!("[profil]   {:>5} tics  coeur {}  {}", task.ticks_used, task.cpu, task.name);
    }
    let (pages, ms, cached, hits, shared) = super::vfs::novafs::stats();
    crate::serial_println!(
        "[profil] racine Linux : {} pages lues sur le disque en {} ms - {} pages en cache, {} succes, {} pages partagees",
        pages, ms, cached, hits, shared
    );
}

fn press_code(press: Press) -> Option<u32> {
    if press.pressed {
        Some(EV_BUTTON_DOWN)
    } else if press.released {
        Some(EV_BUTTON_UP)
    } else {
        None
    }
}

impl App for Bridge {
    fn size(&mut self, width: i32, height: i32) {
        self.width = width;
        self.height = height;
        let (w, h) = (self.window.width.load(Ordering::Relaxed) as i32, self.window.height.load(Ordering::Relaxed) as i32);
        if (width, height) != (w, h) {
            // The screen was smaller than asked: the program redraws at
            // the size the window really has.
            self.window.push(Event { kind: EV_RESIZE, x: width, y: height, ..Default::default() });
        }
    }

    fn paint(&self, ui: &mut Ui) {
        if let Some(canvas) = &self.canvas {
            ui.client.canvas(ui.surface, canvas.id, 0, 0);
        }
    }

    fn key(&mut self, key: &KeyEvent) -> bool {
        self.window.push(Event {
            kind: if key.pressed { EV_KEY_DOWN } else { EV_KEY_UP },
            code: key.scancode as u32,
            modifiers: key.modifiers as u32,
            character: key.codepoint,
            ..Default::default()
        });
        false
    }

    fn pointer(&mut self, x: i32, y: i32, press: Press) -> (bool, CursorShape) {
        self.window.push(Event { kind: EV_MOTION, x, y, ..Default::default() });
        if let Some(kind) = press_code(press) {
            self.window.push(Event { kind, x, y, code: 1, ..Default::default() });
        }
        self.cursor = match self.window.cursor.load(Ordering::Relaxed) {
            1 => CursorShape::Hand,
            2 => CursorShape::Text,
            _ => CursorShape::Arrow,
        };
        (false, self.cursor)
    }

    fn wheel(&mut self, x: i32, y: i32, notches: i32) -> bool {
        self.window.push(Event { kind: EV_WHEEL, x, y, value: notches, ..Default::default() });
        false
    }

    fn tick(&mut self) -> bool {
        if self.window.dirty.swap(false, Ordering::AcqRel) {
            self.refresh();
            return true;
        }
        false
    }

    fn quit_requested(&self) -> bool {
        self.window.released.load(Ordering::Acquire)
    }

    fn handles_close_key(&self) -> bool {
        true
    }

    fn title(&self) -> Option<String> {
        Some(self.window.title.lock().clone())
    }

    fn close(&mut self) {
        self.window.closed.store(true, Ordering::Release);
        self.window.push(Event { kind: EV_CLOSE, ..Default::default() });
    }
}

fn read_title(addr: u64) -> String {
    let mut raw = [0u8; 128];
    if crate::vm::copy_from_user(&mut raw, addr).is_err() {
        return String::from("Web Browser");
    }
    let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
    let mut title = String::new();
    for c in String::from_utf8_lossy(&raw[..end]).chars() {
        match c {
            ' '..='~' => title.push(c),
            _ => {
                // The title bar draws ASCII only (a byte outside 0x20-0x7E
                // is a filled square): letters lose their accent rather
                // than disappear -- "Wikipedia", not "Wikipdia".
                if let Some(plain) = fold(c) {
                    title.push_str(plain);
                }
            }
        }
    }
    title
}

/// ASCII stand-in for the characters page titles commonly carry.
fn fold(c: char) -> Option<&'static str> {
    Some(match c {
        '\u{E0}' | '\u{E1}' | '\u{E2}' | '\u{E3}' | '\u{E4}' | '\u{E5}' => "a",
        '\u{C0}' | '\u{C1}' | '\u{C2}' | '\u{C3}' | '\u{C4}' | '\u{C5}' => "A",
        '\u{E7}' => "c",
        '\u{C7}' => "C",
        '\u{E8}' | '\u{E9}' | '\u{EA}' | '\u{EB}' => "e",
        '\u{C8}' | '\u{C9}' | '\u{CA}' | '\u{CB}' => "E",
        '\u{EC}' | '\u{ED}' | '\u{EE}' | '\u{EF}' => "i",
        '\u{CC}' | '\u{CD}' | '\u{CE}' | '\u{CF}' => "I",
        '\u{F1}' => "n",
        '\u{D1}' => "N",
        '\u{F2}' | '\u{F3}' | '\u{F4}' | '\u{F5}' | '\u{F6}' | '\u{F8}' => "o",
        '\u{D2}' | '\u{D3}' | '\u{D4}' | '\u{D5}' | '\u{D6}' | '\u{D8}' => "O",
        '\u{F9}' | '\u{FA}' | '\u{FB}' | '\u{FC}' => "u",
        '\u{D9}' | '\u{DA}' | '\u{DB}' | '\u{DC}' => "U",
        '\u{FD}' | '\u{FF}' => "y",
        '\u{153}' => "oe",
        '\u{152}' => "OE",
        '\u{E6}' => "ae",
        '\u{C6}' => "AE",
        '\u{DF}' => "ss",
        '\u{2013}' | '\u{2014}' | '\u{2212}' => "-",
        '\u{2018}' | '\u{2019}' => "'",
        '\u{201C}' | '\u{201D}' | '\u{AB}' | '\u{BB}' => "\"",
        '\u{2026}' => "...",
        '\u{A0}' | '\u{202F}' => " ",
        '\u{B7}' | '\u{2022}' => "-",
        _ => return None,
    })
}

impl FileOps for Device {
    fn read(&self, buf: &mut [u8], nonblock: bool) -> LxResult<usize> {
        let window = self.window.lock().clone().ok_or(EINVAL)?;
        if buf.len() < 32 {
            return Err(EINVAL);
        }
        let take = || -> Option<LxResult<usize>> {
            let mut events = window.events.lock();
            if events.is_empty() {
                return if nonblock { Some(Err(EAGAIN)) } else { None };
            }
            let mut n = 0;
            while n + 32 <= buf.len() {
                let Some(e) = events.pop_front() else { break };
                buf[n..n + 32].copy_from_slice(&e.bytes());
                n += 32;
            }
            Some(Ok(n))
        };
        super::wait::wait_until(&[&window.queue], None, take).map_err(|_| EINTR)?
    }

    fn stat(&self) -> LxResult<Stat> {
        Ok(Stat { mode: S_IFCHR | 0o666, rdev: 0xE000, ..Default::default() })
    }

    fn kind(&self) -> Kind {
        Kind::CharDev
    }

    fn poll(&self) -> u32 {
        match &*self.window.lock() {
            Some(w) if !w.events.lock().is_empty() => POLLIN | POLLOUT,
            _ => POLLOUT,
        }
    }

    fn wait_queue(&self) -> Option<&WaitQueue> {
        // The queue lives in the window, which lives as long as `self`
        // once created. Before CREATE there is nothing to wait for.
        let guard = self.window.lock();
        let ptr = guard.as_ref().map(|w| &w.queue as *const WaitQueue);
        drop(guard);
        // SAFETY: the window is never replaced once set, and `self` holds it.
        ptr.map(|p| unsafe { &*p })
    }

    fn ioctl(&self, cmd: u64, arg: u64) -> LxResult<u64> {
        match cmd {
            IOCTL_CREATE => {
                if self.window.lock().is_some() {
                    return Err(EBUSY);
                }
                let dims: [u32; 2] = crate::vm::read_user(arg).map_err(|_| EFAULT)?;
                let (w, h) = (dims[0].clamp(64, 4096), dims[1].clamp(64, 4096));
                let pixels = TmpFs::anonymous_file();
                pixels.truncate(w as u64 * h as u64 * 4)?;
                let window = Arc::new(Window {
                    width: AtomicU32::new(w),
                    height: AtomicU32::new(h),
                    pixels,
                    title: SpinLock::new(read_title(arg + 8)),
                    events: SpinLock::new(VecDeque::new()),
                    queue: WaitQueue::new(),
                    dirty: AtomicBool::new(false),
                    cursor: AtomicU32::new(0),
                    released: AtomicBool::new(false),
                    closed: AtomicBool::new(false),
                    bridge: core::sync::atomic::AtomicU64::new(0),
                    damage: SpinLock::new(None),
                });
                *self.window.lock() = Some(window.clone());
                PENDING.lock().push(window);
                crate::scheduler::spawn("nova-gui", crate::process::BinaryType::Cus, bridge_task);
                Ok(0)
            }
            IOCTL_PRESENT => {
                let window = self.window.lock().clone().ok_or(EINVAL)?;
                let (w, h) = (window.width.load(Ordering::Relaxed), window.height.load(Ordering::Relaxed));
                // The rectangle the program says it changed, or all of it.
                let rect = if arg == 0 {
                    [0, 0, w, h]
                } else {
                    let r: [i32; 4] = crate::vm::read_user(arg).map_err(|_| EFAULT)?;
                    let x0 = r[0].clamp(0, w as i32) as u32;
                    let y0 = r[1].clamp(0, h as i32) as u32;
                    let x1 = r[0].saturating_add(r[2]).clamp(0, w as i32) as u32;
                    let y1 = r[1].saturating_add(r[3]).clamp(0, h as i32) as u32;
                    [x0, y0, x1, y1]
                };
                if rect[0] < rect[2] && rect[1] < rect[3] {
                    let mut damage = window.damage.lock();
                    *damage = Some(match *damage {
                        Some(d) => [d[0].min(rect[0]), d[1].min(rect[1]), d[2].max(rect[2]), d[3].max(rect[3])],
                        None => rect,
                    });
                }
                window.dirty.store(true, Ordering::Release);
                // Now, not on the bridge's next lap: that lap was up to two
                // ticks away, and with the compositor's own, a frame could
                // wait 30 ms -- what made video stutter.
                let bridge = window.bridge.load(Ordering::Acquire);
                if bridge != 0 {
                    crate::scheduler::wake(bridge);
                }
                Ok(0)
            }
            IOCTL_TITLE => {
                let window = self.window.lock().clone().ok_or(EINVAL)?;
                let title = read_title(arg);
                // The page's title arrives with its <head>: the latency
                // trace's "page there" marker.
                let input = LAST_INPUT_NS.load(Ordering::Relaxed);
                if input != 0 && *window.title.lock() != title {
                    let elapsed = super::time::monotonic_ns().saturating_sub(input) / 1_000_000;
                    if elapsed <= 30_000 {
                        crate::serial_println!("[nova-gui] titre \"{}\" {} ms apres l'entree", title, elapsed);
                        // A page load, not a page that retitles itself
                        // every second (the video test page does).
                        if elapsed <= 5_000 {
                            cpu_breakdown(elapsed);
                        }
                    }
                }
                *window.title.lock() = title;
                Ok(0)
            }
            IOCTL_CURSOR => {
                let window = self.window.lock().clone().ok_or(EINVAL)?;
                window.cursor.store(arg as u32, Ordering::Relaxed);
                Ok(0)
            }
            IOCTL_INFO => {
                let window = self.window.lock().clone();
                let (w, h) = window.map_or((0, 0), |w| (w.width.load(Ordering::Relaxed), w.height.load(Ordering::Relaxed)));
                let (sw, sh) = crate::gui::screen_size().unwrap_or((1024, 768));
                crate::vm::write_user(arg, &[w, h, sw, sh]).map_err(|_| EFAULT)?;
                Ok(0)
            }
            _ => Err(ENOTTY),
        }
    }

    /// The pixels, for `mmap`.
    fn inode(&self) -> Option<Arc<dyn Inode>> {
        self.window.lock().as_ref().map(|w| w.pixels.clone() as Arc<dyn Inode>)
    }

    fn describe(&self) -> String {
        String::from("/dev/nova-gui")
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

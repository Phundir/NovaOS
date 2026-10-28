//! Phase 7 -- Graphics system (spec 8, 9, 2.3-2.4, 16).
//!
//! ## Map of the subsystem
//!
//! | Module | Step | Role |
//! |---|---|---|
//! | `protocol` | 7.4 | operation codes, encoding, events |
//! | `client` | 7.4 | caller side of the protocol, local queue |
//! | `surface` | 7.2.3 | surfaces, layers, z-order, display lists |
//! | `painter` | 7.2.2 | execution of a list, with clipping |
//! | `compositor` | 7.2.1-7.2.2 | state, requests, render, input routing |
//! | `module` | 7.3.1-7.3.4 | `.mod` format, sweep, signatures, pause |
//! | `modules/` | 7.3.5 | the six native modules of 8.2 |
//! | `theme` | 7.5 | `theme.cfg`, user priority |
//! | `notify` | 7.6 | the five levels of 9 |
//! | `splash` | 7.7.3 | modular boot screen |
//! | `login` | 7.7.1-7.7.2 | accounts, passwords, anti-bruteforce, lock screen |
//! | `session` | hors phase | lock, log out, reboot, power off |
//!
//! ## What phase 7 does not invent
//!
//! No new system call. 8.1 says applications "send IPC instructions": the
//! protocol lives entirely above `ipc`, on the `sys.gui` channel. Routing
//! the drawing through the kernel would have been simpler to write and
//! wrong with respect to the spec.
//!
//! ## Lock order
//!
//! `COMPOSITOR` -> `REGISTRY` -> the screen lock of `graphics`. Never the
//! reverse. The render takes the screen lock while already holding that
//! of the compositor; the module loader takes `REGISTRY` without ever
//! taking `COMPOSITOR` again. An inversion would be paid for with a
//! deadlock between the render loop and a module sweep.

pub mod canvas;
pub mod client;
pub mod compositor;
pub mod display_settings;
pub mod gpu;
pub mod login;
pub mod module;
pub mod modules;
pub mod notify;
pub mod painter;
pub mod protocol;
pub mod selftest;
pub mod session;
pub mod splash;
pub mod surface;
pub mod theme;

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::binfmt::header::SignatureType;
use crate::ipc::{self, MessageKind};
use crate::process::{BinaryType, Pid};
use crate::vga::SpinLock;

use compositor::Compositor;
use module::{Origin, Slot, State};

// ============================================================
//  Global state
// ============================================================

static COMPOSITOR: SpinLock<Option<Compositor>> = SpinLock::new(None);

/// Has the compositor taken over the screen?
///
/// An atomic separate from the lock: `vga::_print` must be able to ask
/// it on every line without ever waiting for a render in progress.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Tick at which the compositor started - used by the log.
static STARTED_AT: AtomicU64 = AtomicU64::new(0);

pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Does a surface carrying this title exist?
///
/// Used by the self-tests that launch an application: observing that a
/// program *started* says nothing, since a live task may never have
/// obtained a surface. Observing that its window exists does. The
/// question bears on the title and not on the PID: it is what the
/// compositor knows of the application, and it is also what stays
/// checkable after its death.
pub fn surface_named(title: &str) -> bool {
    with(|comp| comp.stack.iter().any(|surface| surface.title == title)).unwrap_or(false)
}

/// Where a surface currently sits on screen.
///
/// Asked for rather than remembered by the application, and that is the
/// point: a window is moved by **the compositor**, which drags it by its
/// title bar without telling its owner. An application that cached its own
/// origin would be wrong from the first drag, and a click meant for a
/// letter of text would land several characters away -- the further the
/// window had been moved, the further off.
pub fn surface_origin(id: surface::SurfaceId) -> Option<(i32, i32)> {
    with(|comp| comp.stack.get(id).map(|surface| (surface.rect.x, surface.rect.y))).flatten()
}

/// Size of the screen the compositor draws, if it runs.
pub fn screen_size() -> Option<(u32, u32)> {
    with(|comp| (comp.width as u32, comp.height as u32))
}

/// Account of the open session, if there is one.
///
/// Read from the compositor rather than kept beside it: the login screen
/// of 7.7 writes it there, and a second copy would be a second truth --
/// the one that gets out of date being, inevitably, the one a security
/// check reads.
pub fn session_user() -> Option<String> {
    with(|comp| comp.session_user.clone()).flatten()
}

/// Accesses the state of the compositor for the duration of a closure.
///
/// Same discipline as `graphics::with_surface`: the lock is taken and
/// released by the function, hence impossible to hold across a task
/// switch.
pub fn with<R>(action: impl FnOnce(&mut Compositor) -> R) -> Option<R> {
    let mut guard = COMPOSITOR.lock();
    guard.as_mut().map(action)
}

// ============================================================
//  7.3 -- Module registry
// ============================================================

/// A module kept by the loader.
#[derive(Debug, Clone)]
pub struct LoadedModule {
    pub slot: Slot,
    pub app_id: String,
    pub display_name: String,
    pub origin: Origin,
    pub state: State,
    pub signature: SignatureType,
    pub version: String,
    pub path: String,
    /// Task running the module, or zero if there is none.
    pub pid: Pid,
    /// Explained refusal, when there is one.
    pub error: Option<module::ModuleError>,
}

struct Registry {
    modules: Vec<LoadedModule>,
    scans: u64,
    started: u64,
    stopped: u64,
    rejected: u64,
}

static REGISTRY: SpinLock<Registry> = SpinLock::new(Registry {
    modules: Vec::new(),
    scans: 0,
    started: 0,
    stopped: 0,
    rejected: 0,
});

/// Internal label: asks a module to withdraw cleanly.
///
/// A paused module must **deregister**, says 8.3: destroy its surface and
/// give back its slot. Killing it outright would leave its surface on
/// screen, since the compositor replays display lists that survive their
/// author very well.
pub const TAG_MODULE_STOP: u32 = 0xFFFF_0001;

/// Snapshot for `OP_MODULE_LIST`: (slot, active).
pub fn registry_snapshot() -> Vec<(u8, bool)> {
    REGISTRY
        .lock()
        .modules
        .iter()
        .map(|module| (module.slot as u8, module.state == State::Active))
        .collect()
}

pub fn loaded_modules() -> Vec<LoadedModule> {
    REGISTRY.lock().modules.clone()
}

/// The module occupying this slot, if it is active.
pub fn module_in(slot: Slot) -> Option<LoadedModule> {
    REGISTRY
        .lock()
        .modules
        .iter()
        .find(|module| module.slot == slot && module.state == State::Active)
        .cloned()
}

// ============================================================
//  7.3.5 -- Table of the native implementations
// ============================================================

/// The native modules of 8.2, plus the splash of 2.3.
///
/// A `.mod` describes **who** occupies a slot; this table says **which
/// code** runs it. The two are separate on purpose: the day a module is a
/// real ring 3 binary, it is this table that will disappear, not the
/// format nor the loader.
type ModuleEntry = (&'static str, Slot, &'static str, fn() -> !);

pub static NATIVE_MODULES: &[ModuleEntry] = &[
    (
        "os.nova.wallpaper",
        Slot::Wallpaper,
        "Fond d'ecran",
        modules::wallpaper::task,
    ),
    (
        "os.nova.winmanager",
        Slot::WindowManager,
        "Gestionnaire fenetres",
        modules::winmanager::task,
    ),
    (
        "os.nova.taskbar",
        Slot::Taskbar,
        "Barre des taches",
        modules::taskbar::task,
    ),
    (
        "os.nova.launcher",
        Slot::Launcher,
        "Lanceur",
        modules::launcher::task,
    ),
    (
        "os.nova.notifs",
        Slot::Notifications,
        "Notifications",
        modules::notifs::task,
    ),
    (
        "os.nova.cursor",
        Slot::Overlay,
        "Curseur",
        modules::cursor::task,
    ),
    (
        "os.nova.splash",
        Slot::Splash,
        "Splash de demarrage",
        splash::task,
    ),
    // 16.2 -- The five accessibility modules. They sit in the same table
    // as the rest because they are modules like the rest: same `.mod`
    // header, same sweep, same pause by rename, same replaceability.
    // What distinguishes them is where their file lives -- the
    // `accessibility/` subdirectory of 17 -- and the ACCESSIBILITY flag,
    // which is what `install_native_modules` reads to place them.
    (
        "os.nova.accessibility",
        Slot::Accessibility,
        "Accessibilite",
        crate::access::coordinator_task,
    ),
    (
        "os.nova.screenreader",
        Slot::ScreenReader,
        "Lecteur d'ecran",
        crate::access::reader::task,
    ),
    (
        "os.nova.zoom",
        Slot::Zoom,
        "Zoom",
        crate::access::zoom_task,
    ),
    (
        "os.nova.contrast",
        Slot::Contrast,
        "Contraste",
        crate::access::contrast_task,
    ),
    (
        "os.nova.keyboardnav",
        Slot::KeyboardNav,
        "Navigation clavier",
        crate::access::keynav_task,
    ),
];

fn native_for(app_id: &str) -> Option<&'static ModuleEntry> {
    NATIVE_MODULES.iter().find(|entry| entry.0 == app_id)
}

// ============================================================
//  7.3.1 -- Installation of the .mod files
// ============================================================

/// Writes the native `.mod` into `/sys/gui/modules/`.
///
/// What the system image built by `dev package` (9.1.5) would do. Until
/// then, the kernel drops them itself on the first boot -- and
/// **registers them in the trust store**, failing which their SYSTEM
/// signature would be refused by `module::authorize`. It is the same
/// mechanism phase 3 uses for its self-test binaries, and it carries the
/// same reservation: a digest table filled at run time proves integrity,
/// not provenance.
pub fn install_native_modules(now: u64) -> (usize, usize) {
    use crate::fs::perm::{Credentials, Mode};

    if !crate::fs::is_ready() {
        return (0, 0);
    }
    let who = Credentials::kernel();
    let mut written = 0;
    let mut present = 0;

    for (app_id, slot, display_name, _) in NATIVE_MODULES.iter() {
        let short = app_id.rsplit('.').next().unwrap_or(app_id);
        let directory = match slot {
            Slot::Splash => module::BOOT_DIR,
            // 17 gives the accessibility modules their own directory, and
            // 16.4 wants them working from the login screen -- so they
            // must be found by the sweep before any session exists, which
            // is exactly what a separate directory scanned at boot buys.
            Slot::Accessibility
            | Slot::ScreenReader
            | Slot::Zoom
            | Slot::Contrast
            | Slot::KeyboardNav => module::ACCESSIBILITY_DIR,
            _ => module::SYSTEM_DIR,
        };
        let path = alloc::format!("{}/{}.mod", directory, short);

        // The payload describes the module. It holds no code: it is the
        // table of the natives that supplies that. The field exists and is
        // covered by the digest, so it cannot be altered in silence --
        // that is what counts for 7.3.2.
        let payload = alloc::format!(
            "module={}\nslot={}\nnative=1\n",
            app_id,
            slot.as_str()
        );
        let file = module::build(&module::BuildSpec {
            slot: *slot,
            signature: SignatureType::System,
            app_id,
            display_name,
            author: "Nova OS",
            version: [1, 0, 0, 1],
            extra_flags: match slot {
                Slot::Splash => module::flags::BOOT,
                // BOOT as well as ACCESSIBILITY: 16.4 says the shortcuts
                // work "from the login screen", so these five cannot wait
                // for a session.
                Slot::Accessibility
                | Slot::ScreenReader
                | Slot::Zoom
                | Slot::Contrast
                | Slot::KeyboardNav => module::flags::BOOT | module::flags::ACCESSIBILITY,
                _ => 0,
            },
            payload: payload.as_bytes(),
        });

        if let Some(digest) = module::payload_digest(&file) {
            crate::binfmt::signature::register_system_digest(digest);
        }

        // A file already present is not rewritten: the user may have
        // paused it, and reinstalling it on every boot would cancel their
        // choice.
        let paused = alloc::format!("{}/{}.pause", directory, short);
        if crate::fs::stat(&path, who).is_ok() || crate::fs::stat(&paused, who).is_ok() {
            present += 1;
            continue;
        }

        if crate::fs::write_file(&path, &file, who, Mode::system_file(), now).is_ok() {
            written += 1;
        }
    }

    (written, present)
}

/// Writes the reference `theme.cfg` if it does not exist (7.5.2).
pub fn install_default_theme(now: u64) -> bool {
    use crate::fs::perm::{Credentials, Mode};

    if !crate::fs::is_ready() {
        return false;
    }
    let who = Credentials::kernel();
    if crate::fs::stat(theme::SYSTEM_THEME, who).is_ok() {
        return false;
    }
    let content = theme::Theme::dark().to_cfg();
    crate::fs::write_file(
        theme::SYSTEM_THEME,
        content.as_bytes(),
        who,
        Mode::system_file(),
        now,
    )
    .is_ok()
}

// ============================================================
//  7.3.1 and 7.3.4 -- Sweep and life cycle
// ============================================================

/// Sweeps the directories, starts what must be started, stops the rest.
///
/// Returns the number of active modules. Callable hot: it is what gives
/// meaning to the `.mod` -> `.pause` rename, which would be a mere
/// convention if nothing read it back.
pub fn rescan_modules(user: Option<&str>) -> usize {
    let found = module::resolve(user);

    // What must start and what must stop is decided under the lock; the
    // task creations and the IPC sends happen afterwards, with the lock
    // released. Launching a task while holding a lock that task might want
    // is the shortest path to a block.
    let mut to_start: Vec<(&'static str, fn() -> !)> = Vec::new();
    let mut to_stop: Vec<Pid> = Vec::new();
    let active;

    {
        let mut registry = REGISTRY.lock();
        registry.scans += 1;

        let mut next: Vec<LoadedModule> = Vec::new();

        for item in found.iter() {
            let Some(header) = item.header.as_ref() else {
                registry.rejected += 1;
                next.push(LoadedModule {
                    slot: Slot::Core,
                    app_id: String::from(item.file_name.as_str()),
                    display_name: String::from("?"),
                    origin: item.origin,
                    state: State::Rejected,
                    signature: SignatureType::None,
                    version: String::from("-"),
                    path: item.path.clone(),
                    pid: 0,
                    error: item.error,
                });
                continue;
            };

            // A module already in service keeps its task: the re-sweep
            // must not restart what has not moved, otherwise the slightest
            // re-read would make the whole screen flicker.
            let running = registry
                .modules
                .iter()
                .find(|existing| {
                    existing.app_id == header.app_id && existing.state == State::Active
                })
                .map(|existing| existing.pid)
                .unwrap_or(0);

            let mut entry = LoadedModule {
                slot: header.slot,
                app_id: header.app_id.clone(),
                display_name: header.display_name.clone(),
                origin: item.origin,
                state: item.state,
                signature: header.signature,
                version: header.version_string(),
                path: item.path.clone(),
                pid: running,
                error: item.error,
            };

            if item.state == State::Active {
                if running == 0 && let Some((_, _, _, task)) = native_for(&header.app_id) {
                    to_start.push((
                        NATIVE_MODULES
                            .iter()
                            .find(|candidate| candidate.0 == header.app_id)
                            .map(|candidate| candidate.0)
                            .unwrap_or("module"),
                        *task,
                    ));
                }
            } else if running != 0 {
                to_stop.push(running);
                entry.pid = 0;
            }
            if item.state == State::Rejected {
                registry.rejected += 1;
            }

            next.push(entry);
        }

        // A module that has disappeared from the disk stops too: 8.3 only
        // speaks of renaming, but deleting the file cannot leave the
        // module running.
        for existing in registry.modules.iter() {
            if existing.state == State::Active
                && existing.pid != 0
                && !next.iter().any(|module| module.app_id == existing.app_id)
            {
                to_stop.push(existing.pid);
            }
        }

        active = next
            .iter()
            .filter(|module| module.state == State::Active)
            .count();
        registry.started += to_start.len() as u64;
        registry.stopped += to_stop.len() as u64;
        registry.modules = next;
    }

    for pid in to_stop {
        let message = crate::ipc::Message::new(
            crate::scheduler::current_pid(),
            MessageKind::Signal,
            TAG_MODULE_STOP,
            &[],
        );
        let _ = ipc::send_unchecked(pid, message);
    }

    for (name, task) in to_start {
        let short = name.rsplit('.').next().unwrap_or(name);
        let pid = crate::scheduler::spawn(short, BinaryType::Ss, task);
        let mut registry = REGISTRY.lock();
        if let Some(module) = registry
            .modules
            .iter_mut()
            .find(|module| module.app_id == name)
        {
            module.pid = pid;
        }
    }

    active
}

/// Must a module stop? Consulted by its loop on every lap.
pub fn stop_requested(message: &crate::ipc::Message) -> bool {
    message.kind == MessageKind::Signal && message.tag == TAG_MODULE_STOP
}

// ============================================================
//  7.2.1 -- The compositor task
// ============================================================

/// Period of the render loop, in milliseconds.
///
/// Ten milliseconds, that is one tick: it is the resolution of the clock,
/// and going lower would only occupy the core with nothing able to happen
/// between two wake-ups.
const FRAME_PERIOD_MS: u64 = 10;

/// Frames between two polls of the host screen size: a tenth of a second.
/// With the two steady polls `poll_resize` waits for, a resize lands a
/// fifth of a second after the host window stops moving.
const RESIZE_POLL_FRAMES: u32 = 10;

/// Time the compositor spends per frame, on each path: everything from
/// the damage taken to the image sent, the GPU's work included when the
/// GPU composes (the task waits for it).
#[derive(Clone, Copy, Default)]
pub struct FrameTime {
    pub processor_frames: u64,
    pub processor_ns: u64,
    pub gpu_frames: u64,
    pub gpu_ns: u64,
}

impl FrameTime {
    fn record(&mut self, ns: u64, gpu: bool) {
        if gpu {
            self.gpu_frames += 1;
            self.gpu_ns += ns;
        } else {
            self.processor_frames += 1;
            self.processor_ns += ns;
        }
    }
}

pub static FRAME_TIME: SpinLock<FrameTime> = SpinLock::new(FrameTime {
    processor_frames: 0,
    processor_ns: 0,
    gpu_frames: 0,
    gpu_ns: 0,
});

/// Frames between two collections of orphan surfaces.
///
/// Twenty frames, that is a fifth of a second at the rate above.
const DEAD_SWEEP_FRAMES: u32 = 20;

/// `compositor.ss` -- the only component that writes on the framebuffer.
pub fn task() -> ! {
    if !crate::graphics::is_active() {
        crate::serial_println!("[gui] pas de framebuffer - compositeur non demarre");
        crate::scheduler::exit_current_with(0);
    }

    if ipc::open_channel(compositor::CHANNEL).is_err() {
        crate::serial_println!("[gui] canal \"{}\" deja ouvert", compositor::CHANNEL);
        crate::scheduler::exit_current_with(-1);
    }

    let (width, height) = crate::graphics::resolution().unwrap_or((640, 480));
    let (theme, origin) = theme::load_for(None);

    *COMPOSITOR.lock() = Some(Compositor::new(width as i32, height as i32, theme));
    ACTIVE.store(true, Ordering::Release);
    STARTED_AT.store(crate::idt::ticks(), Ordering::Relaxed);

    // 8.1: from here on, the compositor is the only one to write on the
    // framebuffer. The kernel console falls silent on screen and carries
    // on over serial, which receives everything.
    crate::graphics::release_console();

    crate::serial_println!(
        "[gui] compositeur en ligne - canal \"{}\", {}x{}, theme {} cles systeme",
        compositor::CHANNEL,
        width,
        height,
        origin.system_keys
    );

    let self_pid = crate::scheduler::current_pid();
    let mut sweep: u32 = 0;
    let mut resize_poll: u32 = 0;
    let mut windows_seen: u64 = 0;
    let mut gpu_driver = gpu::drive::Driver::new();

    loop {
        // The input events arrive by subscription to the driver channels:
        // the compositor polls no hardware, which is the boundary phase 6
        // laid down.
        //
        // The subscription is retried on every lap until it succeeds: the
        // compositor starts in phase 5 of 2.1, the drivers in phase 6.
        // Subscribing once and for all at boot would amount to never
        // having any input.
        subscribe_input();

        let backlog = drain_mailbox(self_pid);

        // A session action accepted by `handle` runs here, with the lock
        // released: it spawns, kills and writes to the disk.
        if let Some(action) = with(|comp| comp.session_request.take()).flatten() {
            session::perform(action);
        }

        expire_notifications();

        // The collection of the orphan surfaces is not done on every
        // frame. It takes the scheduler lock **once per surface** to ask
        // whether its owner is still alive: at a hundred frames per second
        // and seven surfaces, that is seven hundred lock acquisitions per
        // second to answer "yes" seven hundred times.
        //
        // A window whose application has just died therefore stays
        // displayed for up to a fifth of a second longer. Nobody perceives
        // that delay, and the compositor gets back the time it spent
        // there.
        sweep += 1;
        if sweep >= DEAD_SWEEP_FRAMES {
            sweep = 0;
            collect_dead_owners();
        }

        // Not while the GPU is busy with a late frame: asking the device
        // for the host's size waits in QEMU until the GPU is free.
        resize_poll += 1;
        if resize_poll >= RESIZE_POLL_FRAMES && !gpu_driver.suspended() {
            resize_poll = 0;
            follow_host_size(self_pid);
        }

        // The task bar is told when the windows change, rather than
        // finding out on its next one-second repaint: a window opened or
        // focused showed in the bar up to a second late, or under the
        // wrong name until then.
        let changed = with(|comp| {
            let signature = compositor::window_signature(comp);
            if signature == windows_seen {
                return None;
            }
            compositor::announce_windows(comp);
            Some((signature, core::mem::take(&mut comp.pending)))
        })
        .flatten();
        if let Some((signature, pushes)) = changed {
            windows_seen = signature;
            for routed in pushes.iter() {
                compositor::post(routed, self_pid);
            }
        }

        // The GPU composes once its core is up (phase 13.8); until then,
        // and for good if it fails, the processor paints.
        gpu_driver.poll();
        let before = with(|comp| comp.frames).unwrap_or(0);
        let started = crate::posix::time::monotonic_ns();
        if !gpu_driver.frame() {
            let _ = with(compositor::render);
        }
        let after = with(|comp| comp.frames).unwrap_or(0);
        if after != before {
            FRAME_TIME.lock().record(crate::posix::time::monotonic_ns() - started, gpu_driver.active());
        }

        // A full lap means the clients have more waiting: take it now, not
        // a frame later. Sleeping here was paying ten milliseconds per 128
        // messages while the clients sat blocked on a full box. A yield
        // still lets every other task run.
        if backlog {
            crate::scheduler::yield_now();
        } else {
            let _ = crate::syscall::invoke(
                crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
                [FRAME_PERIOD_MS, 0, 0, 0, 0, 0],
            );
        }
    }
}

/// Follows the size of the host window, when virtio-gpu reports one.
///
/// The order is the one every consumer of the size needs: the framebuffer
/// first (`set_mode`), so that nothing is ever painted at a size the
/// screen does not have; then the pointer and the compositor; then the
/// clients, who place their surfaces again from the event.
///
/// Runs on the compositor task with no lock held, which is what
/// `set_mode` demands.
fn follow_host_size(self_pid: Pid) {
    // A size chosen in the settings first; then the host, only if the
    // screen follows it. The host is not polled while following is off:
    // the event would be read and dropped, and turning following back on
    // would then wait for the next resize instead of catching up.
    let wanted = match display_settings::take_request() {
        Some(size) => Some(size),
        None if display_settings::follows_host() => crate::graphics::virtio_gpu::poll_resize(),
        None => None,
    };
    let Some((width, height)) = wanted else {
        return;
    };
    if let Err(reason) = crate::graphics::virtio_gpu::set_mode(width, height) {
        crate::serial_println!(
            "[gui] redimensionnement en {}x{} refuse : {}",
            width,
            height,
            reason
        );
        return;
    }
    let Some((width, height)) = crate::graphics::resolution() else {
        return;
    };
    let (width, height) = (width as i32, height as i32);
    crate::drivers::mouse::set_surface(width, height);
    let routed = with(|comp| compositor::resize(comp, width, height)).unwrap_or_default();
    for target in routed.iter() {
        compositor::post(target, self_pid);
    }
    crate::serial_println!(
        "[gui] ecran redimensionne en {}x{} - {} client(s) prevenu(s)",
        width,
        height,
        routed.len()
    );
}

/// One bit per input channel: 1 = keyboard, 2 = mouse.
static SUBSCRIBED: AtomicU64 = AtomicU64::new(0);

fn subscribe_input() {
    const CHANNELS: [&str; 2] = [
        crate::drivers::keyboard::CHANNEL,
        crate::drivers::mouse::CHANNEL,
    ];

    let mut done = SUBSCRIBED.load(Ordering::Relaxed);
    if done == 0b11 {
        return;
    }

    for (index, channel) in CHANNELS.iter().enumerate() {
        let bit = 1u64 << index;
        if done & bit != 0 {
            continue;
        }
        if let Some(id) = ipc::find_channel(channel)
            && ipc::subscribe(id).is_ok()
        {
            done |= bit;
            crate::serial_println!("[gui] abonne a \"{}\"", channel);
        }
    }
    SUBSCRIBED.store(done, Ordering::Relaxed);
}

/// Empties the mailbox: requests, input events, signals.
/// Messages handled per lap before painting.
const DRAIN_PER_LAP: usize = 128;

/// Empties the mailbox. Returns `true` if the bound was reached, hence if
/// more is waiting.
fn drain_mailbox(self_pid: Pid) -> bool {
    // Bounded: a client flooding the compositor must not stop it painting.
    // The rest is taken on the next lap -- at once, not a frame later:
    // see the loop in `task`.
    for _ in 0..DRAIN_PER_LAP {
        let Some(message) = ipc::try_recv() else {
            return false;
        };

        match message.kind {
            MessageKind::Request => {
                let quiet = message.tag & protocol::NO_REPLY != 0;
                let op = message.tag & !protocol::NO_REPLY;
                let reply = with(|comp| {
                    compositor::handle(comp, message.sender, op, &message.payload)
                })
                .flatten()
                .filter(|_| !quiet);
                // The answer goes out **outside the lock**: it is the rule
                // of docs/DECISIONS.md on sends under a lock, and it holds
                // here as much as elsewhere.
                if let Some(reply) = reply {
                    let response = crate::ipc::Message::new(
                        self_pid,
                        MessageKind::Response,
                        op,
                        &reply.payload,
                    );
                    let _ = ipc::send_unchecked(message.sender, response);
                }
            }

            // The drivers publish their events as Event on their channel:
            // it is what the compositor subscribed to.
            MessageKind::Event | MessageKind::Data => {
                route_input(self_pid, &message);
            }

            _ => {}
        }

        // Whatever the message was, it may have left events for a third
        // party in the queue -- a menu toggle for the shell modules, a
        // close request for the owner of a window whose cross was clicked.
        //
        // Flushed for **every** kind and not only after a request: the
        // close button is driven by a pointer event, and a queue emptied
        // only on the next request would hold it until some module happened
        // to draw. That would have worked, by accident, and the cause of a
        // stray delay would have been three functions away.
        //
        // The sends leave the lock behind them, like the reply above and
        // for the same reason.
        let pushes = with(|comp| core::mem::take(&mut comp.pending)).unwrap_or_default();
        for routed in pushes.iter() {
            compositor::post(routed, self_pid);
        }
    }
    true
}

fn route_input(self_pid: Pid, message: &crate::ipc::Message) {
    use protocol::{KeyEvent, PointerEvent};

    // A person's timing is entropy nobody can replay: every input event
    // is stirred into the generator TLS draws its keys from.
    crate::random::mix_event(message.payload.len() as u64 ^ message.tag as u64);

    if let Some(event) = KeyEvent::decode(&message.payload) {
        if login::intercept(event) {
            return;
        }
        let routed = with(|comp| compositor::route_key(comp, event)).unwrap_or_default();
        // The sends happen **outside the lock**: `post` may fail on a full
        // box, and doing it under the compositor lock would block the
        // render as much as the reception.
        for target in routed.iter() {
            compositor::post(target, self_pid);
        }
        return;
    }

    if let Some(event) = PointerEvent::decode(&message.payload) {
        let routed = with(|comp| compositor::route_pointer(comp, event)).unwrap_or_default();
        for target in routed.iter() {
            compositor::post(target, self_pid);
        }
    }
}

fn expire_notifications() {
    let now = crate::idt::ticks();
    with(|comp| {
        if comp.center.expire(now) != 0 {
            comp.damage_all();
        }
    });
}

/// Removes the surfaces of dead processes.
///
/// A display list survives its author: without this sweep, the window of
/// a terminated application would stay painted forever. 8.3 demands it
/// for the modules; the reason holds for every surface.
fn collect_dead_owners() {
    let owners: Vec<Pid> = with(|comp| {
        comp.stack
            .iter()
            .map(|surface| surface.owner)
            .collect::<Vec<_>>()
    })
    .unwrap_or_default();

    for owner in owners {
        if crate::scheduler::task_info(owner).is_some() {
            continue;
        }
        with(|comp| {
            if comp.stack.destroy_owned_by(owner) != 0 {
                comp.center.forget(owner);
                comp.damage_all();
            }
        });
        // Outside the compositor lock: the mixer has its own.
        crate::audio::forget_owner(owner);
        canvas::forget_owner(owner);
    }
}

// ============================================================
//  Startup of the subsystem
// ============================================================

/// Launches the compositor, installs the files and loads the modules.
///
/// Called by `init.dsa`: 2.1 places the compositor in phase 5, after the
/// core services and before the login screen.
pub fn start(now: u64) {
    // The passwords are laid down before everything else: the phase 7
    // self-test also writes into that file, and letting it go first would
    // make `install_default_credentials` believe the work is done.
    login::install_default_credentials(now);

    let (written, present) = install_native_modules(now);
    let theme_written = install_default_theme(now);
    crate::serial_println!(
        "[gui] modules installes : {} ecrits, {} deja presents - theme systeme {}",
        written,
        present,
        if theme_written { "ecrit" } else { "deja present" }
    );

    // The applications of `/apps`, which the launcher lists. Installed here
    // rather than by the launcher: the menu must be able to list what is
    // there without being the thing that puts it there, otherwise a menu
    // that never opens would mean an empty `/apps`.
    let apps = crate::apps::install_native(now);

    // The tabs of `parametres.ss`: each `.ss` that has settings registers
    // its own. Then the defaults every tab gives before anybody logs in,
    // so that what the settings would show and what the system does agree
    // from boot.
    display_settings::register();
    crate::apps::register_settings();
    crate::config::reset();
    crate::serial_println!(
        "[gui] applications installees : {} ecrite(s), {} au catalogue",
        apps,
        crate::apps::NATIVE_APPS.len()
    );

    // virtio-gpu before the compositor: the compositor reads the
    // resolution once when it starts, and everything it creates -- the
    // task bar at the bottom edge, the splash at the centre -- is placed
    // from it. Switching afterwards would work, through the resize path,
    // but would show every module at the wrong place for a frame.
    match crate::graphics::virtio_gpu::init() {
        Ok((width, height)) => {
            crate::drivers::mouse::set_surface(width as i32, height as i32);
            crate::serial_println!(
                "[gui] ecran virtio-gpu {}x{} - suit la taille de la fenetre hote",
                width,
                height
            );
        }
        Err(reason) => {
            crate::serial_println!("[gui] ecran de taille fixe - {}", reason);
        }
    }

    crate::scheduler::spawn("compositor.ss", BinaryType::Ss, task);

    // The compositor must have opened its channel before a module tries
    // to connect to it: the modules wait, but making them wait for
    // nothing costs scheduling laps.
    for _ in 0..200 {
        if is_active() {
            break;
        }
        crate::scheduler::yield_now();
    }

    // With no compositor, the modules have nobody to talk to: each would
    // wait five seconds before giving up, for seven tasks created and
    // destroyed to no purpose. The sweep still happens -- the registry
    // and the diagnostics stay accurate -- but no task is launched.
    // lancee.
    if !is_active() {
        let found = module::resolve(None).len();
        crate::serial_println!(
            "[gui] pas de compositeur - {} module(s) recense(s), aucun demarre",
            found
        );
        return;
    }

    let active = rescan_modules(None);
    crate::serial_println!("[gui] {} module(s) actif(s) apres balayage", active);
}

// ============================================================
//  Diagnostics
// ============================================================

pub fn dump_state() {
    if !is_active() {
        crate::serial_println!("[gui] compositeur inactif");
        return;
    }
    let gpu_stats = *gpu::STATS.lock();
    match gpu_stats {
        Some((stats, (textures, icons))) => crate::serial_println!(
            "[gui] rendu GPU - {} images, {} instances, processeur {} us/image, attente GPU {} us/image, {} Kio copies, {} toile(s), {} icone(s), {} image(s) transformee(s)",
            stats.frames,
            stats.instances,
            stats.cpu_ns / stats.frames.max(1) / 1000,
            stats.wait_ns / stats.frames.max(1) / 1000,
            stats.uploaded / 1024,
            textures,
            icons,
            stats.transformed
        ),
        None => crate::serial_println!("[gui] rendu processeur"),
    }

    with(|comp| {
        crate::serial_println!(
            "[gui] {}x{} - {} surface(s), focus {:?} - {} images, {} requetes, {} refus",
            comp.width,
            comp.height,
            comp.stack.len(),
            comp.stack.focus(),
            comp.frames,
            comp.requests,
            comp.refusals
        );
        crate::serial_println!(
            "[gui] pointeur ({}, {}) boutons 0x{:02x} - {} evenements routes",
            comp.pointer_x,
            comp.pointer_y,
            comp.pointer_buttons,
            comp.events_routed
        );

        crate::serial_println!(
            "[gui]  ID  COUCHE         Z  VISIBLE  CMD  GEOMETRIE          TITRE"
        );
        for id in comp.stack.painting_order() {
            let Some(surface) = comp.stack.get(id) else {
                continue;
            };
            crate::serial_println!(
                "[gui]  {:2}  {:<12} {:3}  {:<7}  {:3}  {:4}x{:<4} +{:4},{:<4} {}",
                surface.id,
                surface.layer.as_str(),
                surface.z,
                if surface.visible { "oui" } else { "non" },
                surface.display_list().len(),
                surface.rect.width,
                surface.rect.height,
                surface.rect.x,
                surface.rect.y,
                surface.title
            );
        }

        let counts = comp.center.counts();
        crate::serial_println!(
            "[gui] notifications : {} au centre ({} PRIO, {} URG, {} HAUTE, {} NORM, {} BASSE)",
            comp.center.len(),
            counts[0],
            counts[1],
            counts[2],
            counts[3],
            counts[4]
        );
        crate::serial_println!(
            "[gui] emises {} - refusees {} - expirees {} - fermees {} - NDD {}",
            comp.center.posted,
            comp.center.refused,
            comp.center.expired,
            comp.center.dismissed,
            if comp.center.do_not_disturb { "actif" } else { "inactif" }
        );
    });

    dump_modules();
}

pub fn dump_modules() {
    let registry = REGISTRY.lock();
    crate::serial_println!(
        "[gui] modules : {} balayage(s), {} demarre(s), {} arrete(s), {} refuse(s)",
        registry.scans,
        registry.started,
        registry.stopped,
        registry.rejected
    );
    crate::serial_println!("[gui]  EMPLACEMENT     ETAT      SRC          SIG         PID  MODULE");
    for module in registry.modules.iter() {
        crate::serial_println!(
            "[gui]  {:<14}  {:<8}  {:<11}  {:<10}  {:3}  {}",
            module.slot.as_str(),
            module.state.as_str(),
            module.origin.as_str(),
            module.signature.as_str(),
            module.pid,
            module.display_name
        );
        if let Some(error) = module.error {
            crate::serial_println!("[gui]      refuse : {} ({})", error.message(), module.path);
        }
    }
}

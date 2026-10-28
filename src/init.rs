//! Phase 6.5 -- Boot services (spec 2.1 and 2.5).
//!
//! ## What `init.dsa` is
//!
//! Phase 4 of the 2.1 table: "First process, orchestrates every
//! service". Concretely, four responsibilities:
//!
//! 1. launch the three fundamental services -- `fs`, `ipc`, `hw` -- and
//!    **wait** for them to declare themselves ready;
//! 2. return an error code from 2.5 if one fails to answer;
//! 3. then launch the secondary services -- drivers, network, audio --
//!    whose failure does not prevent booting;
//! 4. **adopt the orphans**: reap the dead tasks nobody is waiting for.
//!
//! The fourth point closes a limit left open by phase 5:
//! `SYS_PROC_WAIT` did return stack and image, but a task nobody was
//! waiting for stayed a tombstone, its stack tied up forever. That is the
//! historic role of an `init` process, and there was nobody to fill it.
//!
//! ## Adopting only the orphans
//!
//! A task whose parent is waiting for the exit code must **not** be
//! reaped by init: the parent would find "unknown task" where it expected
//! a code. The check therefore bears on two conditions -- nobody is
//! waiting for it, and its parent no longer exists -- and that is exactly
//! the definition of an orphan.
//!
//! ## Why the core services are tasks and not calls
//!
//! Nothing would force `fs.dsa` to be a task: the filesystem is already
//! there, in the kernel, reachable by system call. What the task brings
//! is the **named rendezvous point**: a program wanting the filesystem
//! resolves `sys.fs` and talks to whoever answers, without knowing
//! whether the service is in the kernel, in a binary, or on another
//! machine. The day storage really moves to user space, no client will
//! change by a line.
//!
//! ## What is not done
//!
//! No automatic restart of a dead service. 2.5 gives error codes for the
//! crashes of `fs.dsa`, `ipc.dsa` and `hw.dsa` -- we emit them -- but
//! nothing relaunches the service. A restart policy assumes being able to
//! tell a transient failure from an outright one, failing which one
//! loops.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::ipc::{self, Message, MessageKind};
use crate::process::BinaryType;
use crate::vga::Color;

/// Boot error codes (spec 2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootError {
    /// Not enough memory.
    OutOfMemory,
    /// `init.dsa` missing or corrupted.
    NoInit,
    /// Crash of the filesystem service.
    FsCrash,
    /// Crash of the IPC service.
    IpcCrash,
    /// Crash of the hardware service.
    HwCrash,
    /// The compositor failed before the splash.
    CompositorCrash,
    /// A core service did not answer within thirty seconds.
    ServiceTimeout,
}

impl BootError {
    /// Numeric code as 2.5 names it.
    pub fn code(&self) -> u16 {
        match self {
            BootError::OutOfMemory => 0x0001,
            BootError::NoInit => 0x0010,
            BootError::FsCrash => 0x0020,
            BootError::IpcCrash => 0x0021,
            BootError::HwCrash => 0x0022,
            BootError::CompositorCrash => 0x0030,
            BootError::ServiceTimeout => 0x0040,
        }
    }

    pub fn message(&self) -> &'static str {
        match self {
            BootError::OutOfMemory => "memoire insuffisante",
            BootError::NoInit => "init.dsa introuvable ou corrompu",
            BootError::FsCrash => "le service fs.dsa a echoue",
            BootError::IpcCrash => "le service ipc.dsa a echoue",
            BootError::HwCrash => "le service hw.dsa a echoue",
            BootError::CompositorCrash => "le compositeur a echoue avant le splash",
            BootError::ServiceTimeout => "delai depasse sur un service core",
        }
    }
}

/// Reports a boot error on screen and on the serial console.
///
/// The display carries the code **as the spec writes it**: it is that
/// number the user will copy out to look for help, and translating it
/// into prose alone would make the search impossible.
pub fn report(error: BootError) {
    crate::vga::set_color(Color::White, Color::Red);
    crate::kprintln!(" BOOT_ERR_0x{:04X} - {} ", error.code(), error.message());
    crate::vga::set_color(Color::LightGray, Color::Black);
    crate::serial_println!(
        "[init] BOOT_ERR_0x{:04X} - {}",
        error.code(),
        error.message()
    );
}

/// Time granted to the core services, in milliseconds (spec 2.5).
const CORE_TIMEOUT_MS: u64 = 30_000;

// ============================================================
//  Service registry
// ============================================================

/// Fundamental services, in launch order.
///
/// The order is not arbitrary: `fs` first, because the other two may
/// want to read a configuration; `ipc` next, because `hw` publishes; `hw`
/// last.
pub const CORE_SERVICES: [(&str, &str, BootError); 3] = [
    ("fs.dsa", "sys.fs", BootError::FsCrash),
    ("ipc.dsa", "sys.ipc", BootError::IpcCrash),
    ("hw.dsa", "sys.hw", BootError::HwCrash),
];

/// Mask of the services declared ready, one bit per service.
static READY: AtomicU64 = AtomicU64::new(0);
/// Mask of the services that failed.
static FAILED: AtomicU64 = AtomicU64::new(0);
/// Tasks reaped by adoption.
static ADOPTED: AtomicU64 = AtomicU64::new(0);

fn mark(index: usize, ready: bool) {
    let bit = 1u64 << index;
    if ready {
        READY.fetch_or(bit, Ordering::SeqCst);
    } else {
        FAILED.fetch_or(bit, Ordering::SeqCst);
    }
}

/// State of the core services: (ready, failed).
pub fn service_state() -> (usize, usize) {
    (
        READY.load(Ordering::SeqCst).count_ones() as usize,
        FAILED.load(Ordering::SeqCst).count_ones() as usize,
    )
}

/// Number of orphan tasks reaped.
pub fn adopted() -> u64 {
    ADOPTED.load(Ordering::Relaxed)
}

/// Are all the core services in service?
pub fn core_ready() -> bool {
    READY.load(Ordering::SeqCst).count_ones() as usize == CORE_SERVICES.len()
}

// ============================================================
//  6.5.2 -- Core services
// ============================================================

/// Opens the service channel and declares itself ready.
///
/// Returns `false` if the channel is already taken: two services under
/// the same name would make resolution ambiguous, so the second withdraws.
fn announce(index: usize) -> bool {
    let (name, channel, _) = CORE_SERVICES[index];
    match ipc::open_channel(channel) {
        Ok(id) => {
            crate::serial_println!(
                "[init] service {} pret - canal \"{}\" (id {})",
                name,
                channel,
                id
            );
            mark(index, true);
            true
        }
        Err(error) => {
            crate::serial_println!(
                "[init] service {} : ouverture de \"{}\" refusee - {}",
                name,
                channel,
                error.as_str()
            );
            mark(index, false);
            false
        }
    }
}

/// Answers a request and counts the exchange.
fn respond(to: crate::process::Pid, tag: u32, payload: &[u8]) {
    let message = Message::new(
        crate::scheduler::current_pid(),
        MessageKind::Response,
        tag,
        payload,
    );
    let _ = ipc::send_unchecked(to, message);
}

/// `fs.dsa` -- answers questions about the filesystem.
///
/// Operations, in the message label:
/// 1 = number of volumes mounted, 2 = health (`fs::check_all`).
pub fn service_fs() -> ! {
    if !announce(0) {
        crate::scheduler::exit_current_with(-1);
    }

    loop {
        let request = ipc::recv();
        if request.kind != MessageKind::Request {
            continue;
        }
        let answer: alloc::vec::Vec<u8> = match request.tag {
            1 => (crate::fs::disk_list().len() as u32).to_le_bytes().to_vec(),
            2 => match crate::fs::check_all() {
                Ok((nodes, blocks)) => {
                    let mut payload = alloc::vec::Vec::with_capacity(9);
                    payload.push(1);
                    payload.extend_from_slice(&(nodes as u32).to_le_bytes());
                    payload.extend_from_slice(&(blocks as u32).to_le_bytes());
                    payload
                }
                Err(_) => alloc::vec![0u8],
            },
            _ => alloc::vec::Vec::new(),
        };
        respond(request.sender, request.tag, &answer);
    }
}

/// `ipc.dsa` -- describes the open channels.
///
/// 1 = number of channels, 2 = does a channel name resolve (payload: the
/// name).
pub fn service_ipc() -> ! {
    if !announce(1) {
        crate::scheduler::exit_current_with(-1);
    }

    loop {
        let request = ipc::recv();
        if request.kind != MessageKind::Request {
            continue;
        }
        let answer: alloc::vec::Vec<u8> = match request.tag {
            1 => (ipc::channel_list().len() as u32).to_le_bytes().to_vec(),
            2 => {
                let name = core::str::from_utf8(&request.payload).unwrap_or("");
                match ipc::find_channel(name) {
                    Some(id) => id.to_le_bytes().to_vec(),
                    None => alloc::vec::Vec::new(),
                }
            }
            _ => alloc::vec::Vec::new(),
        };
        respond(request.sender, request.tag, &answer);
    }
}

/// `hw.dsa` -- describes the hardware and the drivers.
///
/// 1 = number of drivers in service, 2 = number of PCI functions seen.
pub fn service_hw() -> ! {
    if !announce(2) {
        crate::scheduler::exit_current_with(-1);
    }

    loop {
        let request = ipc::recv();
        if request.kind != MessageKind::Request {
            continue;
        }
        let answer: alloc::vec::Vec<u8> = match request.tag {
            1 => {
                let ready = crate::drivers::list()
                    .iter()
                    .filter(|d| d.state == crate::drivers::DriverState::Ready)
                    .count();
                (ready as u32).to_le_bytes().to_vec()
            }
            2 => (crate::pci::enumerate().len() as u32).to_le_bytes().to_vec(),
            _ => alloc::vec::Vec::new(),
        };
        respond(request.sender, request.tag, &answer);
    }
}

// ============================================================
//  6.5.1 -- The init process
// ============================================================

/// The `init.dsa` task.
pub fn task() -> ! {
    crate::serial_println!("[init] demarrage - orchestration des services (spec 2.1)");

    // --- 10.2 and 10.1: locale and accessibility ---
    //
    // Before everything else that displays anything. 14.1 wants the
    // settings to apply with no restart, and 16.4 wants the accessibility
    // shortcuts working "from the login screen": both must therefore be
    // in place before the compositor paints its first frame, not after.
    //
    // Neither needs a driver, a network or a session -- only the
    // filesystem, which `kernel_main` mounted before creating this task.
    crate::i18n::start(crate::idt::ticks());
    crate::access::start(crate::idt::ticks());

    // --- 2.1 phase 5: splash screen ---
    //
    // The compositor starts **before** the core services, contrary to the
    // order of table 2.1. The reason is practical and defensible: the
    // splash exists to show boot progress, and a splash appearing only
    // once everything has started shows nothing. The compositor depends
    // on no core service -- it needs the framebuffer and the filesystem,
    // both ready.
    crate::gui::start(crate::idt::ticks());
    crate::gui::splash::set_progress(0, 10);

    // --- 6.5.2: core services ---
    crate::scheduler::spawn("fs.dsa", BinaryType::Dsa, service_fs);
    crate::scheduler::spawn("ipc.dsa", BinaryType::Dsa, service_ipc);
    crate::scheduler::spawn("hw.dsa", BinaryType::Dsa, service_hw);

    // --- 6.5.3: deadline and error codes ---
    if !wait_for_core(CORE_TIMEOUT_MS) {
        // A service that explicitly failed is named; otherwise it is the
        // deadline that spoke. The two diagnoses do not call for the same
        // fix: repair a service, or look for what is holding it up.
        let failed = FAILED.load(Ordering::SeqCst);
        let mut named = false;
        for (index, (_, _, error)) in CORE_SERVICES.iter().enumerate() {
            if failed & (1 << index) != 0 {
                report(*error);
                named = true;
            }
        }
        if !named {
            report(BootError::ServiceTimeout);
        }
    } else {
        crate::vga::set_color(Color::LightGreen, Color::Black);
        crate::kprintln!("[init] {} services core en ligne", CORE_SERVICES.len());
        crate::vga::set_color(Color::LightGray, Color::Black);
    }

    crate::gui::splash::set_progress(1, 40);

    // --- 6.5.4: secondary services ---
    //
    // Their failure does not compromise the boot: a machine with no
    // network card must reach the login screen. They declare themselves
    // absent, and the system carries on.
    crate::drivers::start_all();
    if !crate::drivers::wait_settled(5, 10_000) {
        crate::serial_println!("[init] un pilote au moins n'a pas rendu son verdict a temps");
    }
    crate::drivers::dump_state();
    // The GPU (spec 8.5) with the drivers: its backend opens a first Venus
    // session to learn the host's card, a tenth of a second. The compositor
    // started earlier in software, and moves to the GPU when it is ready.
    crate::gpu::start();
    crate::gui::splash::set_progress(2, 70);

    crate::scheduler::spawn("reseau", BinaryType::Dsa, network_task);

    // --- 8.4 and 8.5: package manager and updates ---
    //
    // After the drivers, for two distinct reasons: the update service
    // needs the network, and the boot validation of 11.3 must open once
    // the core services have spoken, not before -- it would otherwise
    // measure the time of its own launch.
    crate::spm::start(crate::idt::ticks());

    // --- 9: SDK ---
    //
    // After SPM, and for the same reason that put SPM after the drivers:
    // the SDK writes into `/sys/tools/` and `/sys/runtimes/`, so it needs
    // the volume, which is in place here and was at no earlier moment.
    crate::sdk::start(crate::idt::ticks());

    // --- 2.1 phase 7: login screen ---
    crate::gui::splash::set_progress(3, 90);
    start_session();

    // --- 6.5.1: adoption of the orphans ---
    reaper()
}

/// 2.1 phase 7 -- Removes the splash and shows the login screen.
///
/// The order matters: the splash occupies the `Modal` layer, the very one
/// the login screen is going to settle in. Showing both would give two
/// superimposed modal surfaces, whose order would depend on their
/// creation date -- behaviour that is correct by accident.
fn start_session() {
    wait_for_selftests();
    crate::gui::splash::set_progress(5, 100);
    // Lets the splash show its last state: without this delay, the bar
    // would go from 90 to nothing at all, and nobody would see it reach
    // the end.
    let _ = crate::syscall::invoke(
        crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
        [400, 0, 0, 0, 0, 0],
    );
    crate::gui::splash::finish();

    if crate::gui::is_active() {
        crate::scheduler::spawn("login.ss", BinaryType::Ss, crate::gui::login::task);
        crate::serial_println!("[init] ecran de connexion lance (spec 2.4)");
    } else {
        crate::serial_println!("[init] pas de compositeur - connexion graphique ignoree");
    }
}

/// Longest wait for the boot self-tests before the login screen opens
/// anyway. They take seconds; a self-test stuck for good must not keep
/// the machine on its splash for ever.
const SELFTEST_WAIT_MS: u64 = 180_000;

/// Keeps the splash up until the boot self-tests are done.
///
/// They run as `.dsa`, above everything a session runs, and they used to
/// run **under** the first session: the login screen appeared, and for
/// the next half-minute every click waited behind them. Opening the
/// session once they are done costs the same seconds, spent where nobody
/// is trying to use the machine yet, and the splash says so.
fn wait_for_selftests() {
    let per_tick = 1000 / crate::TIMER_HZ as u64;
    let deadline = crate::idt::ticks() + SELFTEST_WAIT_MS.div_ceil(per_tick).max(1);
    loop {
        let (running, created) = crate::selftests_pending();
        if running == 0 {
            break;
        }
        // Six self-test tasks at most; the bar goes from 90 to 99 as they
        // finish. `created` grows while the chain runs, so the bar can
        // step back by a hair -- truer than a bar that lies.
        let done = created.saturating_sub(running);
        crate::gui::splash::set_progress(4, 90 + 9 * done / created.max(6));
        if crate::idt::ticks() >= deadline {
            crate::serial_println!(
                "[init] {} autotest(s) encore en cours apres {} s - session ouverte malgre tout",
                running,
                SELFTEST_WAIT_MS / 1000
            );
            return;
        }
        let _ = crate::syscall::invoke(
            crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
            [100, 0, 0, 0, 0, 0],
        );
    }
    crate::serial_println!("[init] autotests termines - ouverture de la session");
}

/// Waits for the core services to declare themselves, or for the deadline.
fn wait_for_core(timeout_ms: u64) -> bool {
    let per_tick = 1000 / crate::TIMER_HZ as u64;
    let deadline = crate::idt::ticks() + timeout_ms.div_ceil(per_tick).max(1);

    loop {
        if core_ready() {
            return true;
        }
        let (ready, failed) = service_state();
        if ready + failed >= CORE_SERVICES.len() {
            return false;
        }
        if crate::idt::ticks() >= deadline {
            return false;
        }
        let _ = crate::syscall::invoke(
            crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
            [10, 0, 0, 0, 0, 0],
        );
    }
}

/// Configures the network: static configuration, otherwise DHCP.
fn network_task() -> ! {
    // The driver must have had time to speak: with no card there is
    // nothing to configure, and saying so at once avoids three seconds of
    // pointless DHCP wait.
    for _ in 0..20 {
        if crate::net::interface().attached {
            break;
        }
        let _ = crate::syscall::invoke(
            crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
            [50, 0, 0, 0, 0, 0],
        );
    }

    if !crate::net::interface().attached {
        crate::serial_println!("[reseau] aucune carte - configuration ignoree");
        crate::scheduler::exit_current_with(0);
    }

    if crate::net::load_config() {
        let interface = crate::net::interface();
        crate::vga::set_color(Color::LightCyan, Color::Black);
        crate::kprintln!("[net] {} (configuration statique)", interface.ip.text());
        crate::vga::set_color(Color::LightGray, Color::Black);
    } else {
        match crate::net::dhcp::acquire() {
            Ok(lease) => {
                crate::vga::set_color(Color::LightGreen, Color::Black);
                crate::kprintln!(
                    "[net] {} par DHCP - passerelle {}, DNS {}",
                    lease.ip.text(),
                    lease.gateway.text(),
                    crate::net::interface().dns.text()
                );
                crate::vga::set_color(Color::LightGray, Color::Black);
                crate::serial_println!(
                    "[net] bail de {} s obtenu du serveur {} - DNS du bail {} non retenu",
                    lease.seconds,
                    lease.server.text(),
                    lease.dns.text()
                );
                if let Err(error) = crate::net::save_config() {
                    crate::serial_println!(
                        "[net] configuration non enregistree dans /net/ : {}",
                        error.message()
                    );
                } else {
                    crate::serial_println!("[net] configuration ecrite dans {}", crate::net::CONFIG_PATH);
                }
            }
            Err(reason) => {
                crate::vga::set_color(Color::Yellow, Color::Black);
                crate::kprintln!("[net] pas d'adresse : {}", reason);
                crate::vga::set_color(Color::LightGray, Color::Black);
            }
        }
    }

    crate::net::dump_state();
    // Network time, now that there may be a network to ask. The service
    // retries by itself if the address came up empty.
    crate::net::sntp::start();
    // Only when the host asked for it (`make bench-net`).
    crate::net::bench::run_if_requested();
    crate::scheduler::exit_current_with(0)
}

/// Adoption loop: reaps the dead tasks nobody is waiting for.
fn reaper() -> ! {
    use crate::process::State;

    loop {
        let orphans: alloc::vec::Vec<crate::process::Pid> = crate::scheduler::task_list()
            .into_iter()
            .filter(|task| task.state == State::Dead)
            .filter(|task| !crate::scheduler::is_awaited(task.pid))
            .filter(|task| task.parent == 0 || !crate::scheduler::exists(task.parent))
            .map(|task| task.pid)
            .collect();

        for pid in orphans {
            // The mailbox has already disappeared at termination:
            // `try_reap` only returns what survives death, stack and
            // image.
            if crate::scheduler::try_reap(pid).is_some() {
                ADOPTED.fetch_add(1, Ordering::Relaxed);
                crate::serial_println!("[init] tache orpheline {} adoptee et recuperee", pid);
            }
        }

        // Half a second: short enough that a stack does not stay tied up
        // for long, long enough that the loop weighs nothing.
        let _ = crate::syscall::invoke(
            crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
            [500, 0, 0, 0, 0, 0],
        );
    }
}

pub fn dump_state() {
    let (ready, failed) = service_state();
    crate::serial_println!(
        "[init] services core : {} prets, {} en echec - {} orphelines adoptees",
        ready,
        failed,
        adopted()
    );
}

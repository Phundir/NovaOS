#![no_std]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]

extern crate alloc;

pub mod access;
pub mod acpi;
pub mod ahci;
pub mod alloc_test;
pub mod apic;
pub mod apps;
pub mod audio;
pub mod binfmt;
pub mod compat;
pub mod config;
pub mod crypto;
pub mod drivers;
pub mod fpu;
pub mod frame;
pub mod fwcfg;
pub mod fs;
pub mod gdt;
pub mod gpu;
pub mod graphics;
pub mod i18n;
pub mod gui;
pub mod heap;
pub mod idt;
pub mod init;
pub mod ioapic;
pub mod ipc;
pub mod json;
pub mod linux;
pub mod posix;
pub mod memory;
pub mod multiboot;
pub mod net;
pub mod pci;
pub mod pic;
pub mod port;
pub mod ports;
pub mod power;
pub mod process;
pub mod prompt;
pub mod random;
pub mod rtc;
pub mod scheduler;
pub mod security;
pub mod sdk;
pub mod shell;
pub mod serial;
pub mod smp;
pub mod spm;
pub mod syscall;
pub mod textbuf;
pub mod usb;
pub mod user;
pub mod version;
pub mod vga;
pub mod vm;
pub mod web;

use core::alloc::Layout;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU64, Ordering};

use alloc::format;
use ipc::MessageKind;
use multiboot::{BootInfo, BootMode};
use process::BinaryType;
use vga::Color;

/// APIC timer frequency: 100 Hz, that is one tick every 10 ms. Each core
/// arms its own at that same rate.
pub const TIMER_HZ: u32 = 100;

/// Frequency of core 0's timer: a thousand interrupts a second, of which
/// one in ten is the system tick above (`idt::ticks`, slices, Nova's
/// sleeps). The other nine serve only the deadlines of Linux programs'
/// waits (`idt::fine_ticks`, one per millisecond): at 100 Hz their timers
/// were rounded up to the next tick plus one, a 33 ms video frame came
/// out at 40 or 50 ms and a 16.7 ms refresh at 20 or 30 -- the browser's
/// stutter. The other cores keep 100 Hz: they check no deadline.
pub const FINE_HZ: u32 = 1000;

/// Name of the system logging channel.
const LOG_CHANNEL: &str = "sys.log";

/// Number of cores online, filled in after the SMP startup.
static CORE_COUNT: AtomicU64 = AtomicU64::new(1);

/// PIDs of the test tasks, filled in at creation.
/// Boot self-tests created and not yet finished.
///
/// The login screen waits for this to reach zero: the self-tests run as
/// `.dsa`, the highest priority, and on one core they took the machine
/// from under the first session -- a desktop that answered a click a
/// second late for the first half-minute. The splash stays up instead,
/// and says what it is waiting for.
static SELFTESTS_RUNNING: AtomicU64 = AtomicU64::new(0);

/// Self-tests created since boot, for the splash's progress.
static SELFTESTS_CREATED: AtomicU64 = AtomicU64::new(0);

/// Creates a self-test task and counts it until `selftest_done`.
fn spawn_selftest(name: &str, entry: fn() -> !) {
    SELFTESTS_RUNNING.fetch_add(1, Ordering::SeqCst);
    SELFTESTS_CREATED.fetch_add(1, Ordering::SeqCst);
    scheduler::spawn(name, BinaryType::Dsa, entry);
}

/// Same, on a stack of its own size: the JavaScript engine is a tree
/// walker and keeps 96 KiB of margin below it (`web::js::interp`).
fn spawn_selftest_with_stack(name: &str, entry: fn() -> !, stack: usize) {
    SELFTESTS_RUNNING.fetch_add(1, Ordering::SeqCst);
    SELFTESTS_CREATED.fetch_add(1, Ordering::SeqCst);
    scheduler::spawn_with_stack(name, BinaryType::Dsa, entry, stack);
}

/// Last call of a self-test task.
fn selftest_done() -> ! {
    SELFTESTS_RUNNING.fetch_sub(1, Ordering::SeqCst);
    scheduler::exit_current_with(0)
}

/// Boot self-tests: (still running, created so far).
pub fn selftests_pending() -> (u64, u64) {
    (
        SELFTESTS_RUNNING.load(Ordering::SeqCst),
        SELFTESTS_CREATED.load(Ordering::SeqCst),
    )
}

static PID_PING: AtomicU64 = AtomicU64::new(0);
static PID_PONG: AtomicU64 = AtomicU64::new(0);

pub fn smp_core_count() -> usize {
    CORE_COUNT.load(Ordering::Relaxed) as usize
}

/// Rust entry point of the kernel, run by the boot core alone.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(multiboot_phys: u64) -> ! {
    serial::init();

    let boot = unsafe { multiboot::parse(multiboot_phys) };
    multiboot::set_boot_mode(boot.mode);

    // 7.1.1 -- The display switches before the first line of the kernel.
    //
    // GRUB has programmed a graphics mode at the request of the
    // Multiboot2 header: the text buffer at 0xb8000 is no longer
    // displayed. Writing the banner there then switching would make
    // everything before the switch disappear. `graphics::init` asks for
    // neither frame nor page table, precisely so that it can come here
    // -- and with no graphics mode it does nothing, the VGA writer stays.
    unsafe { graphics::init(&boot) };
    vga::clear_screen();
    banner(&boot);

    gdt::init();
    idt::init();
    vga::set_color(Color::LightGreen, Color::Black);
    kprintln!("[gdt] chargee - ring 0 et ring 3, TSS 0x30");
    vga::set_color(Color::LightGray, Color::Black);

    unsafe { frame::init(&boot.memory_map, boot.phys_addr, boot.total_size) };
    let (_, _, free) = frame::stats();
    kprintln!(
        "[frame] {} frames libres ({} Mio) - fenetre physique de {} Gio",
        free,
        free as u64 * frame::FRAME_SIZE / (1024 * 1024),
        memory::window_end() >> 30
    );

    match unsafe { heap::init() } {
        Ok(()) => kprintln!("[heap] {} Kio mappes", heap::HEAP_SIZE / 1024),
        Err(e) => kprintln!("[heap] ECHEC : {}", e),
    }

    // W^X: without EFER.NXE, bit 63 of a table entry is reserved and the
    // loader cannot make a data page non-executable. To be armed on every
    // core -- EFER is a per-processor register.
    if unsafe { memory::enable_nx() } {
        vga::set_color(Color::LightGreen, Color::Black);
        kprintln!("[mem] EFER.NXE arme - pages de donnees non executables");
    } else {
        vga::set_color(Color::Yellow, Color::Black);
        kprintln!("[mem] NX absent du CPU - W^X degrade en W seul");
    }
    vga::set_color(Color::LightGray, Color::Black);

    // 11.2 -- SSE for ring 3, before the first context switch: the switch
    // saves and restores it. Per processor, like EFER.
    if unsafe { fpu::enable() } {
        kprintln!("[fpu] SSE arme - etat x87/SSE sauve a chaque commutation (FXSAVE)");
    } else {
        panic!("processeur sans FXSR/SSE/SSE2 : la commutation de contexte les exige");
    }

    // 11.5.6 -- Ring 0 respects read-only pages: copy-on-write depends
    // on it. Per core, like the rest of CR0.
    unsafe { memory::enable_write_protect() };

    // 11.3 -- Every upper PML4 entry filled now, before the graphics, the
    // drivers and the secondary cores add mappings: private address
    // spaces copy the upper half once and must never miss a later entry.
    unsafe { vm::init() };
    kprintln!(
        "[vm] moitie haute figee - {} entree(s) PML4 creees, espaces prives prets",
        vm::upper_entries_created()
    );

    // 7.1.2 -- The double buffer comes after the allocator, which it
    // depends on for its frames, and after EFER.NXE, which it depends on
    // to mark the framebuffer non-executable. The self-test follows at
    // once: it clears the screen, which costs the less as few lines have
    // been written there at this stage. The full detail goes to serial.
    init_graphics();

    init_interrupts();

    unsafe { syscall::init(0) };
    // The dispatch tables are static: validating them once, before the
    // first call, avoids a badly sorted table showing up as an
    // unexplained `INVALID_SYSCALL` on a call that does exist.
    match syscall::check_tables() {
        Ok(()) => {
            vga::set_color(Color::LightGreen, Color::Black);
            kprintln!(
                "[syscall] syscall/sysret arme - {} appels sur 9 domaines",
                syscall::table::count()
            );
        }
        Err(reason) => {
            vga::set_color(Color::LightRed, Color::Black);
            kprintln!("[syscall] ECHEC : {}", reason);
        }
    }
    vga::set_color(Color::LightGray, Color::Black);

    // The wall clock is read once the timer is calibrated: the current
    // time is then derived from that reading and the tick counter.
    unsafe { rtc::init() };

    let acpi = unsafe { acpi::parse(boot.rsdp_addr) };
    acpi::dump_state(&acpi);
    power::init(acpi.power);

    let cores = unsafe { smp::start_all(&acpi) };
    CORE_COUNT.store(cores as u64, Ordering::Relaxed);

    vga::set_color(Color::LightCyan, Color::Black);
    kprintln!("[smp] {} coeurs en ligne - repartition selon la spec 7.2", cores);
    vga::set_color(Color::LightGray, Color::Black);
    smp::dump_state();


    if boot.verbose {
        serial_println!();
        serial_println!("--- traces verbeuses (mode debug) ---");
        unsafe { multiboot::dump_tags(multiboot_phys) };
        multiboot::dump_memory_map(&boot.memory_map);
        unsafe { memory::dump_level4() };
        frame::dump_state();
        heap::dump_state();
        apic::dump_state();
        ioapic::dump_state();
        syscall::dump_state();
        fs::dump_state();
        fs::dump_tree("/", fs::perm::Credentials::kernel(), 2);
        serial_println!("--- fin des traces ---");
        serial_println!();
    }

    spawn_tasks(cores);

    // --- phase 4: filesystem ---
    // Before phase 3: the loader will read its binaries from the disk in
    // phase 5, and the boot order must already be the right one. The PCI
    // scan and any formatting take a few tens of milliseconds, with locks
    // held hence interrupts masked -- of no consequence, the scheduler
    // has not started yet.
    pci::dump_state();
    match unsafe { fs::init(idt::ticks()) } {
        Ok(()) => {
            let disks = fs::disk_list();
            vga::set_color(Color::LightGreen, Color::Black);
            kprintln!(
                "[fs] {} volume(s) monte(s) - {} Mio, arborescence spec 17 en place",
                disks.len(),
                disks.iter().map(|d| d.capacity_mib()).sum::<u64>()
            );
            vga::set_color(Color::LightGray, Color::Black);
            // 15.0.3 -- the ENTERPRISE authorities an administrator
            // trusts, before anything signed is loaded.
            let authorities = binfmt::signature::load_authorities();
            kprintln!(
                "[sign] {} autorite(s) ENTERPRISE de confiance ({})",
                authorities,
                binfmt::signature::AUTHORITIES_PATH
            );
        }
        Err(error) => {
            vga::set_color(Color::LightRed, Color::Black);
            kprintln!("[fs] ECHEC : {}", error.message());
            vga::set_color(Color::LightGray, Color::Black);
        }
    }

    // 11.5, 12 -- the POSIX objects of ported and Linux programs: their
    // file tree is mounted from the Nova volume now, before the phase 4
    // self-test writes to the removable media.
    posix::init();

    // --- phase 3: binary formats, signatures and loader ---
    // After the creation of the demonstration tasks: the loader creates
    // some too, and making them born last avoids shifting the PIDs of the
    // phase 2 test set. The full detail goes to the serial console; the
    // screen receives only the verdict, for want of room.
    // --- 2.2: recovery mode ---
    //
    // "kernel + fs.dsa + shell CLI only": no self-test, no compositor, no
    // secondary service -- what a machine that does not reach its desktop
    // still needs to be repaired. `init.dsa` reads the mode and stops
    // after the core services; the shell listens on the serial console.
    scheduler::spawn("shell-serie", BinaryType::Dsa, shell::serial::task);
    if multiboot::is_recovery() {
        kprintln!("[recovery] autotests omis - noyau, services core et shell serie (spec 2.2)");
        scheduler::spawn("init.dsa", BinaryType::Dsa, init::task);
    } else {
        let (mut passed, mut failed) = binfmt::selftest();
        let (loaded, refused, rejected) = binfmt::loader::stats();

        if fs::is_ready() {
            let (p, f) = fs::selftest::run(idt::ticks());
            passed += p;
            failed += f;
        }

        // --- phase 5: system API ---
        //
        // Its self-test does not run here but in a **scheduled task** (see
        // `task_api_selftest`). That is not an organisational detail: domain
        // 0x02 returns the identity of the caller, domain 0x04 drops into its
        // mailbox, and domain 0x05 reads its account. But `kernel_main` is
        // not a task -- `current_pid` is zero there, no mailbox exists, and
        // half the API would have nothing to describe. Exercising it from a
        // task means exercising it in the conditions where it will really be
        // used.
        spawn_selftest("api-autotest", task_api_selftest);

        // --- phase 6: OS layer ---
        //
        // `init.dsa` is phase 4 of table 2.1: the first process, the one that
        // launches and watches everything else. It is created last because
        // what it orchestrates -- drivers, network, adoption of the orphans
        // -- assumes the filesystem and the system API are already in place.
        // The real `init.dsa` will be a binary loaded from /sysapp/; this one
        // is the same orchestration, written as a kernel task for want of an
        // SDK to compile it otherwise.
        scheduler::spawn("init.dsa", BinaryType::Dsa, init::task);
        spawn_selftest("phase6-autotest", task_phase6_selftest);

        // --- phase 7: graphics system ---
        //
        // Same reason as for phase 5: the self-test must run inside a task,
        // because it talks to the compositor by IPC and `kernel_main` has no
        // mailbox.
        spawn_selftest("phase7-autotest", task_phase7_selftest);

        // --- phase 8: SPM and updates ---
        //
        // Same reason again: the self-test installs and uninstalls for real,
        // which assumes a context where `current_credentials` means something
        // -- 10.4 makes installation depend on the role of the caller, and
        // `kernel_main` has none.
        spawn_selftest("phase8-autotest", task_phase8_selftest);

        // --- phase 13: GPU stack ---
        //
        // Waits for `gpu::start`, which `init.dsa` calls after the compositor:
        // the half of the self-test that draws needs the backend up.
        spawn_selftest("phase13-autotest", task_phase13_selftest);

        if failed == 0 {
            vga::set_color(Color::LightGreen, Color::Black);
            kprintln!(
                "[test] phases 3 et 4 validees - {} verifications conformes aux specs 3 a 5",
                passed
            );
        } else {
            vga::set_color(Color::LightRed, Color::Black);
            kprintln!("[test] ECHEC - {} cas incorrects sur {}", failed, passed + failed);
        }
        vga::set_color(Color::LightGray, Color::Black);
        kprintln!(
            "[load] {} images installees - {} refusees, {} rejetees",
            loaded,
            refused,
            rejected
        );
    }

    vga::set_color(Color::LightGreen, Color::Black);
    kprintln!("[sched] {} taches creees - demarrage", scheduler::stats().0);
    vga::set_color(Color::LightGray, Color::Black);
    kprintln!();
    scheduler::dump_tasks();

    smp::release_scheduler();

    unsafe { scheduler::start() }
}

/// Creates the task set, relying on affinity for the distribution.
fn spawn_tasks(cores: usize) {
    for cpu in 0..cores {
        let name = format!("idle-{}", cpu);
        scheduler::spawn_pinned(&name, BinaryType::Idle, task_idle, cpu as u8);
    }

    scheduler::spawn("journal", BinaryType::Dsa, task_logger);
    scheduler::spawn("fs-validation", BinaryType::Dsa, fs::commit_task);

    let ping = scheduler::spawn("ping", BinaryType::Ss, task_ping);
    let pong = scheduler::spawn("pong", BinaryType::Ss, task_pong);
    PID_PING.store(ping, Ordering::Relaxed);
    PID_PONG.store(pong, Ordering::Relaxed);

    scheduler::spawn("moniteur", BinaryType::Cus, task_monitor);
    scheduler::spawn("calcul-a", BinaryType::Cus, task_worker_a);
    scheduler::spawn("calcul-b", BinaryType::Cus, task_worker_b);

    scheduler::spawn("userland", BinaryType::Cus, task_userland);
}

/// Phase 5 self-test, run as a task in its own right.
///
/// It waits a few ticks before starting: the demonstration tasks must
/// have been elected at least once for the suspend/resume cycle to bear
/// on a really active task.
fn task_api_selftest() -> ! {
    let start = idt::ticks();
    while idt::ticks() < start + 5 {
        scheduler::yield_now();
    }

    let (passed, failed) = syscall::selftest::run();

    if failed == 0 {
        vga::set_color(Color::LightGreen, Color::Black);
        kprintln!(
            "[test] phase 5 validee - {} verifications de l'API systeme (spec 6)",
            passed
        );
    } else {
        vga::set_color(Color::LightRed, Color::Black);
        kprintln!(
            "[test] phase 5 ECHEC - {} cas incorrects sur {}",
            failed,
            passed + failed
        );
    }
    vga::set_color(Color::LightGray, Color::Black);

    syscall::dump_state();
    selftest_done()
}

/// Phase 6 self-test, run as a task.
///
/// It waits for `init.dsa` to have finished launching the drivers:
/// checking a service before it opens would test nothing but the order
/// in which the tasks were created.
fn task_phase6_selftest() -> ! {
    let (passed, failed) = drivers_selftest();

    if failed == 0 {
        vga::set_color(Color::LightGreen, Color::Black);
        kprintln!(
            "[test] phase 6 validee - {} verifications de la couche OS",
            passed
        );
    } else {
        vga::set_color(Color::LightRed, Color::Black);
        kprintln!(
            "[test] phase 6 ECHEC - {} cas incorrects sur {}",
            failed,
            passed + failed
        );
    }
    vga::set_color(Color::LightGray, Color::Black);
    selftest_done()
}

fn drivers_selftest() -> (usize, usize) {
    crate::drivers::selftest::run()
}

/// Phase 13 self-test (GPU stack), run as a task.
///
/// Runs last: after `gpu::start` -- opening the first Venus session loads
/// the host's Vulkan driver -- and after the other self-tests. The GPU
/// path allocates frames (the control queue's payloads, the page tables of
/// the shared region's window), and the self-test of phase 5 counts the
/// free frames of the whole machine around its launch cycle: run side by
/// side, the two made each other fail.
fn task_phase13_selftest() -> ! {
    let deadline = idt::ticks() + (120_000 / (1000 / TIMER_HZ as u64)).max(1);
    while (!gpu::started() || SELFTESTS_RUNNING.load(Ordering::SeqCst) > 1) && idt::ticks() < deadline {
        let _ = syscall::invoke(syscall::domain_time::SYS_TIME_SLEEP_MS, [50, 0, 0, 0, 0, 0]);
    }
    let (passed, failed, live) = gpu::selftest::run();
    let what = if live { "sur le GPU" } else { "sans GPU (absent)" };
    if failed == 0 {
        serial_println!("[test] phase 13 validee - {} verifications de la pile graphique, {}", passed, what);
    } else {
        serial_println!("[test] phase 13 ECHEC - {} cas incorrects sur {}, {}", failed, passed + failed, what);
    }
    gpu::dump_state();
    selftest_done()
}

/// Phase 7 self-test, run as a task.
///
/// The verdict goes to the serial console and not to the screen: as soon
/// as it runs, the compositor owns the framebuffer and the kernel console
/// has fallen silent there. That is the wanted behaviour, not a regression.
fn task_phase7_selftest() -> ! {
    let (passed, failed) = gui::selftest::run();

    if failed == 0 {
        serial_println!(
            "[test] phase 7 validee - {} verifications du systeme graphique",
            passed
        );
    } else {
        serial_println!(
            "[test] phase 7 ECHEC - {} cas incorrects sur {}",
            failed,
            passed + failed
        );
    }

    gui::dump_state();
    gui::login::dump_state();
    selftest_done()
}

/// Phase 8 self-test, run as a task.
///
/// It starts after the phase 7 one: both write on the volume, and two
/// self-tests purging their trees at the same time would tread on each
/// other. It also waits for `spm::start` to have declared the local
/// repository, failing which half the cases would bear on a repository
/// that does not exist.
fn task_phase8_selftest() -> ! {
    let deadline = idt::ticks() + (10_000 / (1000 / TIMER_HZ as u64)).max(1);
    while !spm::repo::load()
        .iter()
        .any(|repo| repo.name == spm::LOCAL_REPO_NAME)
        && idt::ticks() < deadline
    {
        let _ = syscall::invoke(
            syscall::domain_time::SYS_TIME_SLEEP_MS,
            [50, 0, 0, 0, 0, 0],
        );
    }

    let (passed, failed) = spm::selftest::run();

    if failed == 0 {
        serial_println!(
            "[test] phase 8 validee - {} verifications de SPM et des mises a jour (spec 10 et 11)",
            passed
        );
    } else {
        serial_println!(
            "[test] phase 8 ECHEC - {} cas incorrects sur {}",
            failed,
            passed + failed
        );
    }

    spm::dump_state();

    // --- phase 9: SDK ---
    //
    // Created here and not in `kernel_main`: both self-tests write on the
    // volume, and the phase 9 one measures the free frames around its
    // allocations. Running them in parallel would make that count depend
    // on what the other is doing -- a check that fails one time in three
    // is worse than no check at all.
    spawn_selftest("phase9-autotest", task_phase9_selftest);

    selftest_done()
}

/// Phase 9 self-test, run as a task.
///
/// It is created by the phase 8 one, at the end of its own work: see the
/// reason above. It additionally waits for `sdk::start` to have laid down
/// the directory tree, failing which the runtime resolution would bear on
/// directories that do not exist.
///
/// The gate is `sdk::is_started`, not `runtime::resolve`. The layout
/// lives on a persistent volume, so resolving succeeds immediately on
/// every boot but the first: gating on it let the self-test overtake
/// `sdk::start`, whose stale-sandbox purge then deleted the overlay of a
/// sandbox the self-test was using. The deadline stays as a floor, for
/// the case where `start` never runs at all.
fn task_phase9_selftest() -> ! {
    let deadline = idt::ticks() + (10_000 / (1000 / TIMER_HZ as u64)).max(1);
    while !sdk::is_started() && idt::ticks() < deadline {
        let _ = syscall::invoke(
            syscall::domain_time::SYS_TIME_SLEEP_MS,
            [50, 0, 0, 0, 0, 0],
        );
    }

    let (passed, failed) = sdk::selftest::run();

    if failed == 0 {
        serial_println!(
            "[test] phase 9 validee - {} verifications du SDK (spec 13 et 15)",
            passed
        );
    } else {
        serial_println!(
            "[test] phase 9 ECHEC - {} cas incorrects sur {}",
            failed,
            passed + failed
        );
    }

    sdk::dump_state();

    // --- phase 10: finishing touches ---
    //
    // Created by the phase 9 self-test, at the end of its own work, for
    // the reason phases 8 and 9 already established: all three write on
    // the volume, and the phase 9 one measures free frames around its
    // allocations. Running them in parallel would make that count depend
    // on what the other is doing.
    spawn_selftest("phase10-autotest", task_phase10_selftest);

    selftest_done()
}

/// Waits for a session, then opens the browser of a test image or runs
/// the root image's startup script.
fn task_linux_autostart() -> ! {
    while gui::session_user().is_none() {
        let _ = syscall::invoke(syscall::domain_time::SYS_TIME_SLEEP_MS, [200, 0, 0, 0, 0, 0]);
    }
    let status = linux::autostart(u64::MAX / 2);
    scheduler::exit_current_with(status.unwrap_or(0))
}

/// Phase 10 self-test, run as a task.
///
/// One task for the three sections rather than three: 10.1, 10.2 and 10.3
/// share state -- the reader announces through the locale, the security
/// prompt draws through the compositor, and both read the session
/// account. Three tasks would interleave, and a failure would then depend
/// on which one got there first.
fn task_phase10_selftest() -> ! {
    let (mut passed, mut failed) = access::selftest::run();

    let (p, f) = i18n::selftest::run();
    passed += p;
    failed += f;

    let (p, f) = security::selftest::run();
    passed += p;
    failed += f;

    let (p, f) = compat::selftest::run();
    passed += p;
    failed += f;

    // The web client: published crypto vectors, then addresses, cookies,
    // cache, HTML, CSS and layout -- no network, a boot must not need it.
    // Its own line, the browser being no step of phase 10.
    let (crypto_passed, crypto_failed) = crypto::selftest::run();
    let (web_passed, web_failed) = web::selftest::run();
    if crypto_failed + web_failed == 0 {
        serial_println!(
            "[test] navigateur valide - {} vecteurs cryptographiques, {} verifications web",
            crypto_passed,
            web_passed
        );
    } else {
        serial_println!(
            "[test] navigateur ECHEC - {} vecteur(s) et {} verification(s) web en echec",
            crypto_failed,
            web_failed
        );
    }

    // Phase 11.2: x87/SSE state saved per task. Its own line too.
    let (fpu_passed, fpu_failed) = fpu::selftest();
    if fpu_failed == 0 {
        serial_println!("[test] phase 11.2 validee - {} verifications de l'etat SSE par tache", fpu_passed);
    } else {
        serial_println!("[test] phase 11.2 ECHEC - {} cas incorrects sur {}", fpu_failed, fpu_passed + fpu_failed);
    }

    // Phase 11.3: private address spaces, demand paging, preemption.
    let (vm_passed, vm_failed) = vm::selftest::run();
    if vm_failed == 0 {
        serial_println!("[test] phase 11.3 validee - {} verifications des espaces d'adressage prives", vm_passed);
    } else {
        serial_println!("[test] phase 11.3 ECHEC - {} cas incorrects sur {}", vm_failed, vm_passed + vm_failed);
    }

    // Phase 11.5: Linux programs of the root image.
    let (lx_passed, lx_failed, lx_absent) = linux::selftest::run();
    if lx_passed + lx_failed == 0 {
        serial_println!("[test] phase 11.5 non eprouvee - aucune image racine Linux ({} programme(s) absent(s))", lx_absent);
    } else if lx_failed == 0 {
        serial_println!("[test] phase 11.5 validee - {} programme(s) Linux conformes, {} absent(s)", lx_passed, lx_absent);
    } else {
        serial_println!("[test] phase 11.5 ECHEC - {} programme(s) en echec sur {}", lx_failed, lx_passed + lx_failed);
    }

    // Phase 12: the same programs through the Nova interface (spec 6.7),
    // with the Nova C library -- and not one Linux call meanwhile.
    let (nv_passed, nv_failed, nv_absent, nv_linux) = posix::selftest::run_native();
    if nv_passed + nv_failed == 0 {
        serial_println!("[test] phase 12 non eprouvee - aucun programme natif ({} absent(s))", nv_absent);
    } else if nv_failed == 0 && nv_linux == 0 {
        serial_println!(
            "[test] phase 12 validee - {} programme(s) natifs conformes, {} absent(s), aucun appel Linux",
            nv_passed,
            nv_absent
        );
    } else {
        serial_println!(
            "[test] phase 12 ECHEC - {} programme(s) en echec sur {}, {} appel(s) Linux",
            nv_failed,
            nv_passed + nv_failed,
            nv_linux
        );
    }

    // The root image's own startup script, if it carries one: the Web
    // Browser. Started once a session is open -- its window would sit
    // under the boot splash and the login screen otherwise -- by a task of
    // its own, so the boot does not wait for it.
    scheduler::spawn("linux-autostart", process::BinaryType::Cus, task_linux_autostart);

    if failed == 0 {
        serial_println!(
            "[test] phase 10 validee - {} verifications des finitions (spec 12, 14 et 16)",
            passed
        );
    } else {
        serial_println!(
            "[test] phase 10 ECHEC - {} cas incorrects sur {}",
            failed,
            passed + failed
        );
    }

    access::dump_state();
    i18n::dump_state();
    security::dump_state();
    compat::dump_state();

    // --- phase 15: atomic updates of applications ---
    //
    // Chained after phase 10 for the reason phases 8 to 10 already
    // follow: it installs, launches and deletes on the volume, and the
    // phase 9 and 10 self-tests measure frames around their own work.
    spawn_selftest("phase15-autotest", task_phase15_selftest);
    selftest_done()
}

/// Self-test of phase 15, in a task: it launches the versions it
/// installs, and asks the update service as an application would.
fn task_phase15_selftest() -> ! {
    let (passed, failed) = spm::selftest_atomic::run();
    if failed == 0 {
        serial_println!(
            "[test] phase 15 validee - {} verifications des mises a jour atomiques (spec 10.5)",
            passed
        );
    } else {
        serial_println!("[test] phase 15 ECHEC - {} cas incorrects sur {}", failed, passed + failed);
    }
    // --- phase 16: languages and tools of the developer ---
    spawn_selftest("phase16-autotest", task_phase16_selftest);
    selftest_done()
}

/// Self-test of phase 16: the shell and the terminal (16.0).
fn task_phase16_selftest() -> ! {
    spawn_selftest_with_stack("js-autotest", task_js_selftest, 1024 * 1024);
    let (passed, failed) = shell::selftest::run();
    if failed == 0 {
        serial_println!(
            "[test] phase 16.0 validee - {} verifications du shell et du terminal (spec 1.1, 2.2)",
            passed
        );
    } else {
        serial_println!("[test] phase 16.0 ECHEC - {} cas incorrects sur {}", failed, passed + failed);
    }
    selftest_done()
}

/// Self-test of the JavaScript engine (16.1): the corpus of
/// `web::js::selftest`, one realm per case.
fn task_js_selftest() -> ! {
    let started = idt::ticks();
    let (passed, failed) = web::js::selftest::run(&scheduler::stack_headroom);
    let elapsed_ms = (idt::ticks() - started) * 1000 / TIMER_HZ as u64;
    if failed == 0 {
        serial_println!(
            "[test] phase 16.1 validee - {} cas du moteur JavaScript ({} ms)",
            passed,
            elapsed_ms
        );
    } else {
        serial_println!("[test] phase 16.1 ECHEC - {} cas incorrects sur {}", failed, passed + failed);
    }
    selftest_done()
}

/// How long an idle core watches for work before it halts, in
/// microseconds.
///
/// Under a hypervisor `hlt` hands the virtual processor back to the host,
/// and the IPI of the next wake-up must have the host schedule it again
/// -- out of a deep idle state of the physical core, as often as not. A
/// thread woken on an idle core waited 30 to 40 us on average before it
/// ran, where the host's own threads wait a few. A browser is a chain of
/// such hand-overs (input thread, main thread, content process,
/// compositor, each waking the next), and most of them come within
/// microseconds of the core going idle. So the idle task watches its
/// rescheduling flag for a short while before halting, as Linux guests do
/// (`cpuidle-haltpoll`): a wake-up within the window costs a cache line,
/// not a trip through the host. Only after the core has just come back
/// from work: a core woken by its own timer tick with nothing to do halts
/// at once, so an idle machine does not keep the host's processors busy.
/// Measured on a pipe round trip between two threads: 7.8 us without the
/// window, 6.0 with it (3 on the host).
const IDLE_POLL_US: u64 = 50;

fn task_idle() -> ! {
    let mut seen = scheduler::idle_elections();
    loop {
        scheduler::preempt_if_needed();
        let per_us = posix::time::tsc_per_ms() / 1000;
        let elections = scheduler::idle_elections();
        let back_from_work = elections != seen;
        seen = elections;
        if per_us != 0 && back_from_work {
            let start = random::rdtsc();
            while !scheduler::reschedule_pending() && random::rdtsc().wrapping_sub(start) < IDLE_POLL_US * per_us {
                core::hint::spin_loop();
            }
            if scheduler::reschedule_pending() {
                continue;
            }
        }
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
}

fn task_logger() -> ! {
    match ipc::open_channel(LOG_CHANNEL) {
        Ok(id) => serial_println!("[journal] canal \"{}\" ouvert (id {})", LOG_CHANNEL, id),
        Err(e) => serial_println!("[journal] ECHEC ouverture : {}", e.as_str()),
    }

    loop {
        let message = ipc::recv();

        vga::set_color(Color::LightGreen, Color::Black);
        kprintln!(
            "[journal] {} de PID {} : {}",
            message.kind.as_str(),
            message.sender,
            message.as_str().unwrap_or("<binaire>")
        );
        vga::set_color(Color::LightGray, Color::Black);
    }
}

/// Number of IPC demonstration round trips (phase 2.6).
///
/// The loop was infinite. It proved what it had to prove -- the IPC
/// rendezvous, the switch, the distribution over the cores -- and then
/// went on proving it for eternity: **73 % of the serial output**,
/// 200 000 messages, and as many heap allocations, since `Message::new`
/// copies its payload into a `Vec`. Two tasks running flat out took from
/// the compositor the CPU it needs to follow the mouse.
///
/// Bounded, the demonstration keeps all its value: if the round trip
/// works five hundred times, it works. If it fails, it fails early.
const DEMO_ROUNDS: u64 = 500;

/// Iterations of the compute tasks before handing back for good.
const DEMO_ITERATIONS: u64 = 20_000_000;

fn task_ping() -> ! {
    let pong = PID_PONG.load(Ordering::Relaxed);
    let mut round: u64 = 0;
    let mut failures: u64 = 0;

    for _ in 0..3 {
        scheduler::yield_now();
    }

    // The payload no longer depends on the round: reformatting it on
    // every round trip allocated one string per message for a text nobody
    // reads. The label already carries the round number.
    const PAYLOAD: &[u8] = b"ping";

    let started = idt::ticks();

    while round < DEMO_ROUNDS {
        round += 1;

        let message = ipc::Message::new(
            scheduler::current_pid(),
            MessageKind::Request,
            round as u32,
            PAYLOAD,
        );
        if ipc::send(pong, message).is_err() {
            failures += 1;
            break;
        }

        let reply = ipc::recv();
        if reply.tag != round as u32 {
            failures += 1;
        }
    }

    let elapsed = idt::ticks().saturating_sub(started);
    if failures == 0 {
        serial_println!(
            "[ping] {} allers-retours IPC en {} tics (coeur {}) - demonstration 2.6 terminee",
            round,
            elapsed,
            smp::current_cpu_id()
        );
    } else {
        serial_println!(
            "[ping] ECHEC - {} anomalie(s) sur {} allers-retours",
            failures,
            round
        );
    }

    // 2.6: `pong` is waiting for a message that will not come. Telling it
    // rather than leaving it blocked forever on its box -- a sleeping
    // task no wake-up awaits is a tombstone, and the `reaper` of
    // `init.dsa` only reaps tasks that are already dead.
    let farewell = ipc::Message::new(
        scheduler::current_pid(),
        MessageKind::Signal,
        0,
        PAYLOAD,
    );
    let _ = ipc::send(pong, farewell);

    scheduler::exit_current_with(0)
}

fn task_pong() -> ! {
    let ping = PID_PING.load(Ordering::Relaxed);
    let mut answered: u64 = 0;

    loop {
        let request = ipc::recv();

        // The end signal: `ping` has finished its demonstration.
        if request.kind == MessageKind::Signal {
            serial_println!("[pong] {} reponses - fin de la demonstration 2.6", answered);
            scheduler::exit_current_with(0);
        }

        let reply = ipc::Message::new(
            scheduler::current_pid(),
            MessageKind::Response,
            request.tag,
            b"pong",
        );
        if ipc::send(ping, reply).is_err() {
            serial_println!("[pong] echec reponse au tour {}", request.tag);
            scheduler::exit_current_with(-1);
        }
        answered += 1;
    }
}

/// 2.7 -- Compute load, to observe the distribution over the cores.
///
/// Bounded for the same reason as `task_ping`: two infinite tight loops
/// left the compositor a fraction of the CPU, and a cursor advancing one
/// pixel per frame is described very exactly as "the mouse does not
/// move".
fn demo_workload(label: &str) -> ! {
    let started = idt::ticks();
    let mut count: u64 = 0;

    while count < DEMO_ITERATIONS {
        count += 1;
        // `black_box` stops the compiler from noticing that the loop
        // produces nothing and removing it entirely: without it, the
        // distribution demonstration would measure no work at all.
        core::hint::black_box(count);
        if count % 1_000_000 == 0 {
            scheduler::preempt_if_needed();
        }
    }

    serial_println!(
        "[{}] {} iterations en {} tics (coeur {}) - demonstration 2.7 terminee",
        label,
        count,
        idt::ticks().saturating_sub(started),
        smp::current_cpu_id()
    );
    scheduler::exit_current_with(0)
}

fn task_worker_a() -> ! {
    demo_workload("calcul-a")
}

fn task_worker_b() -> ! {
    demo_workload("calcul-b")
}

fn task_userland() -> ! {
    for _ in 0..5 {
        scheduler::yield_now();
    }
    unsafe { user::launch() }
}

fn task_monitor() -> ! {
    let mut last_report = 0;

    if let Some(id) = ipc::find_channel(LOG_CHANNEL) {
        let _ = ipc::subscribe(id);
    }

    loop {
        let seconds = idt::ticks() / TIMER_HZ as u64;

        if seconds != last_report && seconds > 0 && seconds % 5 == 0 {
            last_report = seconds;

            let (tasks, switches, rescues) = scheduler::stats();
            let (sent, received, dropped, blocks) = ipc::stats();
            let (calls, errors, _) = syscall::stats();
            let (allocated, free_heap, allocs, frees) = heap::stats();
            let (_, _, free_frames) = frame::stats();

            serial_println!(
                "[moniteur] {} s (coeur {}) - {} taches - {} commutations - {} anti-famine",
                seconds,
                smp::current_cpu_id(),
                tasks,
                switches,
                rescues
            );
            serial_println!(
                "[moniteur] IPC : {} envoyes / {} recus / {} perdus / {} blocages",
                sent,
                received,
                dropped,
                blocks
            );
            // The ported programs (the Web Browser) and the Linux
            // subsystem, apart: a native session leaves the Linux count
            // where the boot's self-tests left it.
            serial_println!(
                "[moniteur] syscalls : {} appels, {} erreurs - programmes portes {}, sous-systeme Linux {}",
                calls,
                errors,
                syscall::ported::call_count(),
                linux::syscall::call_count()
            );
            // How long the Linux threads waited for a core once woken
            // (see `scheduler::wake_latency`), over the last report.
            let (count, mean, max, pid, over) = scheduler::wake_latency::take();
            if count != 0 {
                let who = scheduler::task_info(pid);
                serial_println!(
                    "[moniteur] reveils Linux : {} - attente moyenne {} us, max {} us ({} {} coeur {}) - plus de 1 ms {}, 5 ms {}, 20 ms {}",
                    count,
                    mean,
                    max,
                    pid,
                    who.as_ref().map_or("?", |t| t.name.as_str()),
                    who.as_ref().map_or(0, |t| t.cpu),
                    over[0],
                    over[1],
                    over[2]
                );
            }
            // Invariants of docs/DECISIONS.md: the heap counters must
            // meet at rest, the frames must not melt away for no reason.
            serial_println!(
                "[moniteur] heap : {} o alloues / {} libres - {} allocations, {} liberations",
                allocated,
                free_heap,
                allocs,
                frees
            );
            serial_println!("[moniteur] frames libres : {}", free_frames);

            // The counters go out on every report: they are the ones
            // carrying the invariants of docs/DECISIONS.md, and an
            // invariant reported once a minute detects nothing any more.
            //
            // The **tables**, on the other hand, are spaced out.
            // `dump_tasks` alone writes one line per task, that is some
            // forty every five seconds: half the boot log described a
            // state that had not changed. Reading them every thirty
            // seconds is amply enough to follow a drift.
            if seconds % 30 == 0 {
                // Between two tables the core is handed back to whoever
                // needs it: the monitor runs on a core of the `.cus` range,
                // the browser's, and is not preempted (see `dump_tasks`).
                scheduler::dump_tasks();
                scheduler::preempt_if_needed();
                smp::dump_state();
                scheduler::preempt_if_needed();
                drivers::dump_state();
                scheduler::preempt_if_needed();
                net::dump_state();
                scheduler::preempt_if_needed();
                apps::dump_state();
                ports::dump_state();
                init::dump_state();
            }
        }

        // Sleep until the next report rather than poll the clock.
        //
        // The loop called `preempt_if_needed` continuously: it therefore
        // consumed all the time the scheduler was willing to give it, to
        // do nothing but read `ticks()` again. A sleeping task takes
        // nothing at all, and the monitor has strictly nothing to do
        // between two reports.
        let _ = syscall::invoke(
            syscall::domain_time::SYS_TIME_SLEEP_MS,
            [500, 0, 0, 0, 0, 0],
        );
    }
}

/// 7.1.2 -- Completes the graphics setup, then puts it through its paces.
///
/// Does not hand back on a failure: a kernel booting without a
/// framebuffer is still a kernel that boots. The serial console carries
/// the full diagnosis, the screen receives only the verdict.
fn init_graphics() {
    if graphics::mode().is_some_and(|mode| mode.is_graphic()) {
        let capabilities = unsafe { graphics::enable_double_buffer() };

        // The mode, the pixel format and the cache type go to serial on
        // every boot, as ACPI, PCI and SMP do: they are the three things
        // to read back when the colours are wrong or the display slow,
        // and asking for them again assumes knowing one can.
        graphics::dump_state();

        if graphics::is_active() {
            let (width, height) = graphics::resolution().unwrap_or((0, 0));
            let (columns, rows) = graphics::console_size().unwrap_or((0, 0));

            let (passed, failed) = graphics::selftest::run();

            // The scroll self-test flattens the screen: the console
            // starts again from a blank page, top left.
            vga::clear_screen();

            vga::set_color(Color::LightGreen, Color::Black);
            kprintln!(
                "[gfx] {}x{} - console {}x{} - {}",
                width,
                height,
                columns,
                rows,
                capabilities
            );
            if failed == 0 {
                kprintln!("[test] phase 7.1 validee - {} verifications du framebuffer", passed);
            } else {
                vga::set_color(Color::LightRed, Color::Black);
                kprintln!(
                    "[test] phase 7.1 ECHEC - {} cas incorrects sur {}",
                    failed,
                    passed + failed
                );
            }
            vga::set_color(Color::LightGray, Color::Black);
            return;
        }
    }

    vga::set_color(Color::Yellow, Color::Black);
    kprintln!("[gfx] pas de mode graphique - affichage en mode texte VGA 80x25");
    vga::set_color(Color::LightGray, Color::Black);
    graphics::dump_state();
}

fn init_interrupts() {
    unsafe { pic::disable() };
    let (m1, m2) = pic::read_masks();
    vga::set_color(Color::LightCyan, Color::Black);
    kprintln!("[pic] 8259 remappe et masque (0x{:02x}/0x{:02x})", m1, m2);

    match unsafe { apic::init() } {
        Ok(()) => kprintln!(
            "[apic] LAPIC id {} actif - mode {}",
            apic::id(),
            if apic::is_x2apic() { "x2APIC" } else { "xAPIC" }
        ),
        Err(e) => {
            vga::set_color(Color::LightRed, Color::Black);
            kprintln!("[apic] ECHEC : {}", e);
            vga::set_color(Color::LightGray, Color::Black);
            return;
        }
    }

    unsafe { ioapic::init() };
    kprintln!(
        "[ioapic] id {} - {} lignes disponibles",
        ioapic::id(),
        ioapic::max_redirection_entries()
    );

    let frequency = unsafe { apic::init_timer(FINE_HZ) };
    kprintln!(
        "[timer] {} Hz - coeur 0 a {} Hz pour les echeances fines - bus APIC mesure a {} MHz",
        TIMER_HZ,
        FINE_HZ,
        frequency / 1_000_000
    );

    ioapic::set_irq(1, apic::VECTOR_KEYBOARD, apic::id() as u8);
    ioapic::unmask(1);

    unsafe { idt::enable_interrupts() };
    vga::set_color(Color::LightGreen, Color::Black);
    kprintln!("[irq] interruptions materielles actives");
    vga::set_color(Color::LightGray, Color::Black);
}

fn banner(boot: &BootInfo) {
    let (label, color) = match boot.mode {
        BootMode::Normal => ("", Color::LightGreen),
        BootMode::Recovery => (" - RECOVERY", Color::Yellow),
        BootMode::Debug => (" - DEBUG", Color::LightCyan),
    };

    vga::set_color(color, Color::Black);
    kprintln!(
        "Nova OS {} ({}) booted{}",
        version::VERSION,
        version::COMMIT,
        label
    );

    vga::set_color(Color::LightGray, Color::Black);
    kprintln!(
        "  {} - mode {} - {} Mio de RAM",
        boot.bootloader,
        boot.mode.as_str(),
        boot.memory_map.total_usable() / (1024 * 1024)
    );
    kprintln!();
}

#[alloc_error_handler]
fn alloc_error(layout: Layout) -> ! {
    graphics::reclaim_console();
    vga::set_color(Color::White, Color::Red);
    kprintln!();
    // Who asked: the PID and the core are read without a lock (the one
    // this task may hold could be any), so the line is always printed.
    kprintln!(
        "ALLOCATION ECHOUEE : {} octets, alignement {} - tache {} sur le coeur {}",
        layout.size(),
        layout.align(),
        scheduler::current_pid(),
        smp::current_cpu_id()
    );
    serial_println!(
        "ALLOCATION ECHOUEE : {} octets, alignement {} - tache {} sur le coeur {}",
        layout.size(),
        layout.align(),
        scheduler::current_pid(),
        smp::current_cpu_id()
    );
    halt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    idt::disable_interrupts();
    // 8.1 admits an exception here: a kernel that stops must be able to
    // say so on screen, even if the compositor held it. At this stage,
    // there is no compositor left to spare.
    graphics::reclaim_console();
    vga::set_color(Color::White, Color::Red);
    println!();
    println!("KERNEL PANIC (coeur {}): {}", smp::current_cpu_id(), info);
    serial_println!();
    // The task too: read without a lock, like the core, so the line is
    // always printed -- and it says which of the sixty tasks to look at.
    serial_println!("KERNEL PANIC (coeur {}, tache {}): {}", smp::current_cpu_id(), scheduler::current_pid(), info);
    #[cfg(nova_pile)]
    print_call_chain();
    halt_loop();
}

/// The return addresses of the frames above the panic, for
/// `llvm-addr2line -f -C -i -e build/nova-os.bin <address>`.
///
/// Only in a kernel built with frame pointers (`make PILE=1`): without
/// them `rbp` is an ordinary register, and following it would read
/// wherever it points. The walk stops at the first frame outside the
/// kernel's half, or misaligned, and after twenty-four frames.
#[cfg(nova_pile)]
fn print_call_chain() {
    let mut rbp: u64;
    // SAFETY: reads a register.
    unsafe { core::arch::asm!("mov {}, rbp", out(reg) rbp) };
    for _ in 0..24 {
        if rbp < 0xFFFF_8000_0000_0000 || rbp % 8 != 0 {
            break;
        }
        // SAFETY: with frame pointers, `rbp` is the saved frame: the
        // previous `rbp` at `[rbp]`, the return address at `[rbp + 8]`.
        let ret = unsafe { ((rbp + 8) as *const u64).read_volatile() };
        serial_println!("  pile : {:#x}", ret);
        rbp = unsafe { (rbp as *const u64).read_volatile() };
    }
}

/// Puts the CPU to sleep until the next interrupt, indefinitely.
pub fn halt_loop() -> ! {
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) }
    }
}

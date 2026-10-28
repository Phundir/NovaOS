//! The GPU from programs: the native C program and the Rust one, both
//! launched as a program would be, and the compositor on the GPU.

use super::*;

/// The native program of 13.7.1: the same triangle, from ring 3, through
/// `nova_gfx.h` and the Nova C library. It ends with 0 when every check
/// held; its standard output goes to the serial console.
pub(super) fn native_program(count: &mut Count) {
    const PATH: &str = "/nova-tests/natif/gpu";
    if crate::posix::vfs::walk("/", PATH, true).is_err() {
        crate::serial_println!("[gpu]   {} absent (make webroot) - essai natif ignore", PATH);
        return;
    }
    let linux_before = crate::linux::syscall::call_count();
    let started = crate::idt::ticks();
    let envp = ["PATH=/usr/bin:/bin", "HOME=/home/nova", "LANG=C.UTF-8"];
    // The watchdog trial only when asked at boot (`essai-gpu=garde`).
    let trial = crate::multiboot::GPU_WATCHDOG_TRIAL.load(core::sync::atomic::Ordering::Relaxed);
    let argv: &[&str] = if trial { &[PATH, "garde"] } else { &[PATH] };
    let outcome = crate::posix::spawn_native(PATH, argv, &envp).map(|pid| crate::posix::wait_exit(pid, Some(6000)));
    let elapsed = crate::idt::ticks() - started;
    let linux_calls = crate::linux::syscall::call_count() - linux_before;
    match outcome {
        Ok(Some(0)) => crate::serial_println!("[gpu]   programme natif {} : code 0 en {} tics", PATH, elapsed),
        other => crate::serial_println!("[gpu]   programme natif {} : {:?}", PATH, other),
    }
    let what = if trial { "le delai de garde coupe un dessin trop long, un autre contexte repond ensuite" } else { "le programme natif dessine et verifie son triangle" };
    count.check(what, matches!(outcome, Ok(Some(0))));
    count.check("le programme natif ne fait aucun appel Linux", linux_calls == 0);
    // Its child died holding a context: the sweep takes it back.
    let deadline = crate::idt::fine_ticks() + 2 * crate::gpu::SWEEP_MS + 500;
    while crate::gpu::orphan_count() != 0 && crate::idt::fine_ticks() < deadline {
        let _ = crate::syscall::invoke(crate::syscall::domain_time::SYS_TIME_SLEEP_MS, [20, 0, 0, 0, 0, 0]);
    }
    count.check("les contextes d'un processus mort sont repris", crate::gpu::orphan_count() == 0);
}

/// The same triangle from Rust (13.6.3): `nova-gfx`, the crate, in a
/// `no_std` program linked with the Nova C library.
pub(super) fn rust_program(count: &mut Count) {
    const PATH: &str = "/nova-tests/natif/gpu-rust";
    if crate::posix::vfs::walk("/", PATH, true).is_err() {
        crate::serial_println!("[gpu]   {} absent (make webroot) - essai en Rust ignore", PATH);
        return;
    }
    let linux_before = crate::linux::syscall::call_count();
    let started = crate::idt::ticks();
    let envp = ["PATH=/usr/bin:/bin", "HOME=/home/nova", "LANG=C.UTF-8"];
    let outcome = crate::posix::spawn_native(PATH, &[PATH], &envp).map(|pid| crate::posix::wait_exit(pid, Some(6000)));
    let elapsed = crate::idt::ticks() - started;
    match outcome {
        Ok(Some(0)) => crate::serial_println!("[gpu]   programme Rust {} : code 0 en {} tics", PATH, elapsed),
        other => crate::serial_println!("[gpu]   programme Rust {} : {:?}", PATH, other),
    }
    count.check("un programme Rust dessine et verifie son triangle par nova-gfx", matches!(outcome, Ok(Some(0))));
    count.check("le programme Rust ne fait aucun appel Linux", crate::linux::syscall::call_count() == linux_before);
}

/// The compositor on the GPU (13.4.4, 13.8): the same pixels as the
/// processor's, and the screen really taken.
pub(super) fn compositor(count: &mut Count) {
    for (label, passed) in crate::gui::gpu::selftest::run() {
        count.check(label, passed);
    }
    // The compositor switches on its own lap once the core is up -- or
    // back, a second after the watchdog trial took its context; the test
    // waits for it, a few seconds at most.
    let deadline = crate::idt::fine_ticks() + 5000;
    let composing = || crate::gui::gpu::STATS.lock().is_some_and(|(stats, _)| stats.frames > 0) && crate::gpu::screen::shown();
    while !composing() && crate::idt::fine_ticks() < deadline {
        let _ = crate::syscall::invoke(crate::syscall::domain_time::SYS_TIME_SLEEP_MS, [20, 0, 0, 0, 0, 0]);
    }
    if crate::gui::is_active() {
        count.check("le compositeur compose l'ecran sur le GPU", composing());
    }
    crate::gui::gpu::selftest::measure();
    let plane = crate::gui::with(|comp| comp.cursor_plane).unwrap_or(false);
    if plane {
        // Asked for by the compositor, and shown by the host: its answer
        // to the image's transfer is what puts the image on the plane.
        let deadline = crate::idt::fine_ticks() + 2000;
        while !crate::gpu::screen::cursor_shown() && crate::idt::fine_ticks() < deadline {
            let _ = crate::syscall::invoke(crate::syscall::domain_time::SYS_TIME_SLEEP_MS, [20, 0, 0, 0, 0, 0]);
        }
        count.check("le pointeur est sur le plan curseur de l'hote", crate::gpu::screen::cursor_shown());
    }
    crate::serial_println!(
        "[gpu]   ecran au coeur GPU : {} - plan curseur : {}",
        if crate::gpu::screen::shown() { "oui" } else { "non" },
        if plane { "oui" } else if crate::gpu::screen::cursor_plane() { "disponible, pas de pointeur a montrer" } else { "absent" }
    );
}

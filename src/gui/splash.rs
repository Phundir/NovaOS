//! 7.7.3 -- Modular splash screen (spec 2.3).
//!
//! ## What 2.3 imposes
//!
//! "The splash screen and the progress bar are replaceable `.mod`
//! modules. Stored in `/sys/gui/modules/boot/`, they can be overridden
//! per user in `/users/{user}/settings/gui/modules/boot/`."
//!
//! Three requirements, and all three go through the same loader as the
//! other modules: `module::resolve` sweeps `boot/` as it sweeps the rest,
//! and the priority rule of 8.4 applies there with no special code. The
//! splash occupies the `Slot::Splash` slot, replaceable like every other
//! but `Core`.
//!
//! ## Why it does not block the boot
//!
//! The splash displays the progress, it does not drive it. Its surface is
//! destroyed when `init` announces the services are ready; if the module
//! is paused or refused, nothing changes in the boot, only the screen
//! stays on the kernel log. A splash on the critical path would be one
//! more point of failure for a purely decorative component.

use core::sync::atomic::{AtomicU64, Ordering};

use super::client::{Client, Event};
use super::surface::Layer;

/// Boot progress, in hundredths. Written by `init`, read by the module: an
/// atomic integer is enough, and avoids one more IPC channel to carry a
/// percentage.
static PROGRESS: AtomicU64 = AtomicU64::new(0);

/// Current step, as an index into `STEPS`.
static STEP: AtomicU64 = AtomicU64::new(0);

/// Must the splash withdraw?
static FINISHED: AtomicU64 = AtomicU64::new(0);

/// The phases of the 2.1 table the splash reports.
pub const STEPS: &[&str] = &[
    "Initialisation du noyau",
    "Services systeme",
    "Pilotes materiels",
    "Interface graphique",
    "Verifications du systeme",
    "Session utilisateur",
];

/// Publishes the progress. Called by `init.dsa`.
pub fn set_progress(step: usize, percent: u64) {
    STEP.store(step.min(STEPS.len() - 1) as u64, Ordering::Relaxed);
    PROGRESS.store(percent.min(100), Ordering::Relaxed);
}

/// Announces the boot is finished: the splash withdraws.
pub fn finish() {
    PROGRESS.store(100, Ordering::Relaxed);
    FINISHED.store(1, Ordering::Relaxed);
}

pub fn is_finished() -> bool {
    FINISHED.load(Ordering::Relaxed) != 0
}

pub fn progress() -> (usize, u64) {
    (
        STEP.load(Ordering::Relaxed) as usize,
        PROGRESS.load(Ordering::Relaxed),
    )
}

/// Task of the `splash.mod` module.
///
/// Does not use the common canvas of `modules::run`: the splash has a
/// life cycle of its own -- it withdraws by itself when the boot is
/// finished -- and the common canvas knows only the stop on request.
pub fn task() -> ! {
    let Some(mut client) = Client::connect(5_000) else {
        crate::serial_println!("[mod-splash] compositeur injoignable");
        crate::scheduler::exit_current_with(-1);
    };

    let Some((width, height, _, _)) = client.screen_info() else {
        crate::scheduler::exit_current_with(-1);
    };
    let (width, height) = (width as i32, height as i32);
    let theme = client.theme().unwrap_or_default();

    let Some(surface) = client.create_surface(Layer::Modal, 0, 0, width, height, "splash") else {
        crate::serial_println!("[mod-splash] creation de surface refusee");
        crate::scheduler::exit_current_with(-1);
    };

    crate::serial_println!("[mod-splash] affiche - surface {}", surface);

    loop {
        if is_finished() {
            client.destroy_surface(surface);
            crate::serial_println!("[mod-splash] demarrage termine - surface rendue");
            crate::scheduler::exit_current_with(0);
        }

        // A stop asked by the loader takes precedence over everything.
        while let Some(event) = client.poll_event() {
            if let Event::Other(message) = &event
                && super::stop_requested(message)
            {
                client.destroy_surface(surface);
                crate::scheduler::exit_current_with(0);
            }
        }

        let (step, percent) = progress();
        draw(&mut client, surface, width, height, &theme, step, percent);

        let _ = crate::syscall::invoke(
            crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
            [100, 0, 0, 0, 0, 0],
        );
    }
}

fn draw(
    client: &mut Client,
    surface: super::surface::SurfaceId,
    width: i32,
    height: i32,
    theme: &super::theme::Theme,
    step: usize,
    percent: u64,
) {
    let glyph_width = crate::graphics::font::GLYPH_WIDTH as i32;
    let glyph_height = crate::graphics::font::GLYPH_HEIGHT as i32;

    client.reset(surface);
    client.fill(surface, theme.background);

    // --- title ---
    let title = "Nova OS";
    let title_x = (width - glyph_width * title.len() as i32) / 2;
    client.text(
        surface,
        title_x,
        height / 2 - 80,
        title,
        theme.text,
        None,
    );

    // --- progress bar ---
    let bar_width = (width / 3).max(200);
    let bar_x = (width - bar_width) / 2;
    let bar_y = height / 2;
    let bar_height = 6;

    client.rect(
        surface,
        bar_x,
        bar_y,
        bar_width,
        bar_height,
        theme.surface_alt,
    );
    let filled = (bar_width as u64 * percent / 100) as i32;
    if filled > 0 {
        client.rect(surface, bar_x, bar_y, filled, bar_height, theme.accent);
    }
    client.frame(
        surface,
        bar_x,
        bar_y,
        bar_width,
        bar_height,
        theme.border,
        1,
    );

    // --- current step ---
    let label = STEPS.get(step).copied().unwrap_or("");
    let label_x = (width - glyph_width * label.len() as i32) / 2;
    client.text(
        surface,
        label_x,
        bar_y + 20,
        label,
        theme.text_dim,
        None,
    );

    let percent_text = alloc::format!("{} %", percent);
    let percent_x = (width - glyph_width * percent_text.len() as i32) / 2;
    client.text(
        surface,
        percent_x,
        bar_y + 20 + glyph_height + 4,
        &percent_text,
        theme.text_dim,
        None,
    );

    client.commit(surface);
}

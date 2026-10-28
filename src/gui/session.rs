//! Session and power actions: lock, log out, reboot, power off.
//!
//! ## Who asks, who acts
//!
//! The buttons are on `taskbar.mod`, which asks with `OP_SESSION_ACTION`.
//! The compositor checks the request under its lock (shell module, session
//! open, not locked), stores it, answers, and hands it to `perform` from
//! its own task **once the lock is released**. Every action here spawns a
//! task, kills tasks, sends IPC or writes to the disk; none of that may
//! happen inside `compositor::handle` (entry 47 of DECISIONS.md).
//!
//! Performed by the compositor task rather than by the task bar itself:
//! 8.2 makes the bar replaceable, and a replacement bar would then have to
//! reimplement the end of a session -- or, worse, get it slightly wrong.
//!
//! ## Lock versus log out
//!
//! * **Lock** leaves everything running and covers the screen with the
//!   login panel, restricted to the account in session. The keyboard is
//!   taken **before** the panel task is spawned: between the click and
//!   the first paint, a keystroke must not reach a window.
//! * **Log out** ends the applications of the session, gives the locale,
//!   the accessibility settings, the theme and the modules back to their
//!   system defaults, then shows the full login screen.
//!
//! ## What log out does not do
//!
//! It ends the tasks that own a **window**. An application with no window
//! survives the session: tasks carry no session identity today, so
//! nothing else designates them. And the GUEST account of 5.2 is not
//! wiped -- "session temporaire, effacee a la deconnexion" is not
//! implemented. Both are named in ROADMAP.md.

use alloc::vec::Vec;

use crate::process::{BinaryType, Pid};

use super::notify::Level;
use super::protocol::SessionAction;
use super::surface::Layer;

/// Runs an action the compositor accepted. Called with no lock held.
pub fn perform(action: SessionAction) {
    crate::serial_println!("[session] {} demande(e)", action.as_str());
    match action {
        SessionAction::Lock => lock(),
        SessionAction::Logout => logout(),
        // No notice on screen first: `perform` runs on the compositor
        // task, so nothing would be painted before the machine goes down.
        SessionAction::Reboot => {
            crate::power::prepare();
            crate::power::reboot();
        }
        SessionAction::PowerOff => {
            crate::power::prepare();
            let reason = crate::power::power_off();
            crate::serial_println!("[session] extinction impossible : {}", reason);
            super::with(|comp| {
                super::compositor::post_notification(
                    comp,
                    Level::Haute,
                    "Extinction impossible",
                    reason,
                );
                comp.damage_all();
            });
        }
    }
}

fn lock() {
    let Some(user) = super::session_user() else {
        return;
    };
    if !super::login::begin(super::login::ScreenMode::Unlock(user.clone())) {
        crate::serial_println!("[session] verrouillage refuse - compte \"{}\" introuvable", user);
        return;
    }
    super::with(|comp| comp.session_locked = true);
    crate::scheduler::spawn(
        "login.ss",
        BinaryType::Ss,
        super::login::resume_task,
    );
    crate::serial_println!("[session] session de \"{}\" verrouillee", user);
}

fn logout() {
    let Some(user) = super::session_user() else {
        return;
    };

    // The keyboard first, for the reason `lock` gives.
    super::login::begin(super::login::ScreenMode::Login);

    let ended = end_applications();

    // The applications are gone, so what they handed to the filesystem is
    // all there will ever be: write it out before the next account can
    // log in and power the machine off without asking.
    crate::power::prepare();

    let (theme, _) = super::theme::load_for(None);
    super::with(|comp| {
        comp.session_user = None;
        comp.session_locked = false;
        comp.center.session_active = false;
        comp.theme = theme;
        comp.damage_all();
    });
    crate::i18n::reload(None);
    crate::access::load(None);
    crate::config::reset();

    // The modules last: they read the theme and the locale when they
    // start, the same ordering `login::open_session` follows the other
    // way round.
    let active = super::rescan_modules(None);

    crate::scheduler::spawn(
        "login.ss",
        BinaryType::Ss,
        super::login::resume_task,
    );
    crate::serial_println!(
        "[session] \"{}\" deconnecte(e) - {} application(s) terminee(s), {} module(s) actif(s)",
        user,
        ended,
        active
    );
}

/// Ends the applications that own a window. Returns how many.
///
/// Killed, not asked to close. `EV_CLOSE` would let an application refuse
/// or prompt, and a prompt behind the login screen is one nobody can
/// answer. The task bar asks for a confirmation before sending the request
/// precisely because this loses unsaved work.
///
/// Application formats are ended, and so are the native applications the
/// launcher starts on the account's behalf whatever their format: the
/// explorer is a `.ss` and works with the rights of the account in
/// session, so leaving it open would hand the next person to log in a
/// window holding the previous account's rights. Any other `.ss` or `.dsa`
/// owning a window -- the consent dialog of 12.5 -- belongs to the system,
/// not to the session.
fn end_applications() -> usize {
    let owners: Vec<Pid> =
        super::with(|comp| comp.stack.owners_in(Layer::Window)).unwrap_or_default();

    let mut ended = 0;
    for pid in owners {
        let Some(info) = crate::scheduler::task_info(pid) else {
            continue;
        };
        let application = matches!(
            info.binary_type,
            BinaryType::Cus | BinaryType::Gs | BinaryType::Ras
        );
        let launched = crate::apps::native_for(&info.name).is_some();
        if !application && !launched {
            continue;
        }
        if crate::scheduler::kill(pid, -1) {
            ended += 1;
            // A killed task never closes its audio stream nor gives the
            // microphone back: done for it, or the next session could not
            // record.
            crate::audio::forget_owner(pid);
            crate::serial_println!("[session]   \"{}\" (PID {}) terminee", info.name, pid);
        }
        // The surfaces go at once rather than at the next collection of
        // dead owners: the login screen is about to be painted, and a
        // window of the previous session must not show through a frame.
        super::with(|comp| {
            if comp.stack.destroy_owned_by(pid) != 0 {
                comp.center.forget(pid);
                comp.damage_all();
            }
        });
    }
    ended
}

//! 15.5-15.6 -- `spm.dsa`: the one process that writes applications.
//!
//! ## The rule of the proposal (section 2)
//!
//! An application may **look** for an update of itself, and tell its
//! user; it never writes to the disk. If the user accepts, it asks the
//! system's manager by IPC, and the manager -- alone with the right to
//! write the application directories (spec 12.4 already forbids `/apps/`
//! to applications) -- downloads, checks and installs.
//!
//! ## The channel `spm.update`
//!
//! A `Request` message whose payload is text:
//!
//! ```text
//!   verifier <app_id>    -> "a-jour <v>" | "disponible <v> <code>" | "erreur ..."
//!   installer <app_id>   -> "installe <v>" | "a-jour <v>" | "erreur ..."
//! ```
//!
//! The answer comes back as a `Response` with the request's tag.
//!
//! **An application asks only for itself.** The identity is not what the
//! message claims: it is the application root the sender (or one of its
//! ancestors) was launched from (`versions::version_of`). A request
//! naming another application is refused, and so is a sender SPM did not
//! launch.
//!
//! ## What else the service does
//!
//! At start, the purge of 15.5.2 (before any application can run); then,
//! every ten seconds, the deferred deletions of 15.5.1 -- versions whose
//! last process ended.

use alloc::string::String;

use crate::ipc::{self, Message, MessageKind};

use super::db::Scope;
use super::versions::{self, Outcome};

/// Name of the channel of requests.
pub const CHANNEL: &str = "spm.update";

/// The application a process was launched as, if SPM launched it.
pub fn app_of(pid: crate::process::Pid) -> Option<String> {
    let (root, _) = versions::version_of(pid)?;
    root.rsplit('/').next().map(String::from)
}

/// Answers one request. Public for the self-test, which calls it as the
/// service would, with the sender it wants to be.
pub fn answer(sender: crate::process::Pid, text: &str, now: u64) -> String {
    let mut words = text.split_whitespace();
    let (Some(verb), Some(app)) = (words.next(), words.next()) else {
        return String::from("erreur : requete illisible (verifier|installer <app_id>)");
    };
    match app_of(sender) {
        Some(own) if own == app => {}
        Some(own) => {
            crate::serial_println!("[spm.dsa] processus {} ({}) demande pour {} - refuse", sender, own, app);
            return String::from("erreur : une application ne demande que pour elle-meme");
        }
        None => return String::from("erreur : processus non lance par SPM"),
    }
    match verb {
        "verifier" => match super::registry::resolve(app) {
            Ok((_, _, channel)) => {
                let installed = super::db::find(&Scope::Global, app).map(|entry| entry.version);
                if installed.is_some_and(|version| version >= channel.numbers) {
                    alloc::format!("a-jour {}", channel.version)
                } else {
                    alloc::format!("disponible {} {}", channel.version, channel.code.code())
                }
            }
            Err(error) => alloc::format!("erreur : {}", error.message()),
        },
        "installer" => match versions::update_from_registry(app, Scope::Global, false, now) {
            Ok(Outcome::Installed(done)) => alloc::format!("installe {}", done.version),
            Ok(Outcome::UpToDate(version)) => alloc::format!("a-jour {}", version),
            Err(error) => alloc::format!("erreur : {}", error.message()),
        },
        _ => String::from("erreur : verbe inconnu (verifier|installer)"),
    }
}

/// The `spm.dsa` task.
pub fn task() -> ! {
    let pid = crate::scheduler::current_pid();
    ipc::register(pid);
    let channel = ipc::open_channel(CHANNEL).is_ok();

    let (roots, removed) = versions::purge_at_boot(crate::idt::ticks());
    crate::serial_println!(
        "[spm.dsa] pret - canal {} {}, {} application(s) versionnee(s), {} version(s) purgee(s)",
        CHANNEL,
        if channel { "ouvert" } else { "indisponible" },
        roots,
        removed
    );

    let mut last_tick = crate::idt::ticks();
    loop {
        while let Some(message) = ipc::try_recv() {
            if message.kind != MessageKind::Request {
                continue;
            }
            let text = String::from(message.as_str().unwrap_or(""));
            let reply = answer(message.sender, &text, crate::idt::ticks());
            let _ = ipc::send(message.sender, Message::text(pid, MessageKind::Response, message.tag, &reply));
        }
        let now = crate::idt::ticks();
        if now.saturating_sub(last_tick) >= 10 * crate::TIMER_HZ as u64 {
            versions::tick(now);
            last_tick = now;
        }
        let _ = crate::syscall::invoke(crate::syscall::domain_time::SYS_TIME_SLEEP_MS, [200, 0, 0, 0, 0, 0]);
    }
}

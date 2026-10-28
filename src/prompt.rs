//! 6.4.2 -- Authorisation prompt (spec 12.5).
//!
//! ## What the spec asks for, and what can be held to today
//!
//! 12.5 wants "a real-time prompt displaying the identity of the app, the
//! resource requested and a *Remember this choice* box", the remembered
//! choice being filed in
//! `/users/{user}/settings/permissions/{app_id}.cfg` and revocable.
//!
//! Three of the four elements are feasible now: the display, reading an
//! answer, and persistence in the intended file -- the phase 4 filesystem
//! can write, and the phase 6.1.1 keyboard driver can read. The fourth,
//! the **window**, assumes the compositor of 8.1, which is phase 7. So
//! the prompt shows on the text screen and the serial console.
//!
//! ## Refuse by default
//!
//! A prompt with no answer expires, and expiry **refuses**. That is the
//! only defensible default: granting by silence would make every
//! unattended machine a permissive one, and a program would only have to
//! ask at the right moment. 12.5 does not say so explicitly -- it
//! describes an interface, not a policy -- but the hybrid model of 12.3,
//! where the undeclared is refused, leaves no other coherent reading.
//!
//! ## A remembered choice short-circuits the display
//!
//! And it must do so **before** disturbing the user, otherwise the
//! "remember" box would serve no purpose. It is also what makes the
//! mechanism testable with no human intervention: write the file, then
//! observe that the prompt does not show and returns the recorded answer.

use alloc::format;
use alloc::string::String;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::fs::perm::{Credentials, Mode};
use crate::vga::Color;

/// Answer to an authorisation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

impl Decision {
    pub fn as_str(&self) -> &'static str {
        match self {
            Decision::Allow => "accorde",
            Decision::Deny => "refuse",
        }
    }

    fn keyword(&self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::Deny => "deny",
        }
    }

    pub fn allowed(&self) -> bool {
        *self == Decision::Allow
    }
}

/// Time left to the user, in milliseconds.
const TIMEOUT_MS: u64 = 10_000;

static ASKED: AtomicU64 = AtomicU64::new(0);
static REMEMBERED: AtomicU64 = AtomicU64::new(0);
static EXPIRED: AtomicU64 = AtomicU64::new(0);

/// Counters: (prompts raised, answers found in memory, expiries).
pub fn counters() -> (u64, u64, u64) {
    (
        ASKED.load(Ordering::Relaxed),
        REMEMBERED.load(Ordering::Relaxed),
        EXPIRED.load(Ordering::Relaxed),
    )
}

/// Path of the remembered-choice file of an application.
///
/// 12.5 places it under the user's account. An account with no home
/// directory -- the system account -- files its own in `/sys/config/`,
/// failing which remembering would silently fail for everything running
/// as the system.
pub fn decision_path(app_id: &str) -> String {
    let who = crate::scheduler::current_credentials();
    match crate::fs::account_of(who.uid) {
        Some((name, _, _)) if name != "root" => {
            format!("/users/{}/settings/permissions/{}.cfg", name, app_id)
        }
        _ => format!("/sys/config/permissions-{}.cfg", app_id),
    }
}

/// Reads a choice already recorded for a resource.
pub fn remembered(app_id: &str, resource: &str) -> Option<Decision> {
    if !crate::fs::is_ready() {
        return None;
    }
    let path = decision_path(app_id);
    let bytes = crate::fs::read_file(&path, Credentials::kernel()).ok()?;
    let text = core::str::from_utf8(&bytes).ok()?;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != resource {
            continue;
        }
        return match value.trim() {
            "allow" => Some(Decision::Allow),
            "deny" => Some(Decision::Deny),
            _ => None,
        };
    }
    None
}

/// Records a choice, keeping those bearing on other resources.
/// ressources.
/// Rewriting the whole file rather than appending a line: two
/// contradictory lines on the same resource would make the result depend
/// on the reading order, and a revocation from the settings would then
/// have to remove every occurrence instead of one.
pub fn remember(app_id: &str, resource: &str, decision: Decision) -> Result<(), crate::fs::FsError> {
    if !crate::fs::is_ready() {
        return Err(crate::fs::FsError::NotMounted);
    }
    let path = decision_path(app_id);
    let who = Credentials::kernel();
    let now = crate::idt::ticks();

    let mut content = String::from("# choix memorises - spec 12.5, revocables depuis les parametres\n");
    if let Ok(bytes) = crate::fs::read_file(&path, who)
        && let Ok(text) = core::str::from_utf8(&bytes)
    {
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            match trimmed.split_once('=') {
                Some((key, _)) if key.trim() == resource => continue,
                _ => {
                    content.push_str(trimmed);
                    content.push('\n');
                }
            }
        }
    }
    content.push_str(&format!("{}={}\n", resource, decision.keyword()));

    let _ = crate::fs::delete(&path, who, now);
    crate::fs::write_file(&path, content.as_bytes(), who, Mode::private_file(), now).map(|_| ())
}

/// Removes a remembered choice -- this is the revocation of 12.5.
pub fn revoke(app_id: &str, resource: &str) -> Result<(), crate::fs::FsError> {
    // Recording a refusal is not a revocation: revocation must return the
    // system to the "never asked" state, where the prompt will show
    // again.
    if !crate::fs::is_ready() {
        return Err(crate::fs::FsError::NotMounted);
    }
    let path = decision_path(app_id);
    let who = Credentials::kernel();
    let now = crate::idt::ticks();

    let Ok(bytes) = crate::fs::read_file(&path, who) else {
        return Ok(());
    };
    let text = core::str::from_utf8(&bytes).unwrap_or("");

    let mut content = String::from("# choix memorises - spec 12.5, revocables depuis les parametres\n");
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        match trimmed.split_once('=') {
            Some((key, _)) if key.trim() == resource => continue,
            _ => {
                content.push_str(trimmed);
                content.push('\n');
            }
        }
    }

    let _ = crate::fs::delete(&path, who, now);
    crate::fs::write_file(&path, content.as_bytes(), who, Mode::private_file(), now).map(|_| ())
}

/// Puts the question to the user, or returns the already remembered choice.
///
/// `app_id` identifies the requester, `display` is what is shown of it,
/// `resource` names what is asked -- it is the key of the remembered
/// choice, so it must be stable and contain neither space nor equals sign.
pub fn ask(app_id: &str, display: &str, resource: &str, question: &str) -> Decision {
    if let Some(decision) = remembered(app_id, resource) {
        REMEMBERED.fetch_add(1, Ordering::Relaxed);
        crate::serial_println!(
            "[prompt] \"{}\" demande {} - choix memorise : {}",
            display,
            resource,
            decision.as_str()
        );
        return decision;
    }

    ASKED.fetch_add(1, Ordering::Relaxed);

    crate::vga::set_color(Color::Black, Color::LightGray);
    crate::kprintln!(" AUTORISATION DEMANDEE ");
    crate::vga::set_color(Color::Yellow, Color::Black);
    crate::kprintln!("  Application : {} ({})", display, app_id);
    crate::kprintln!("  Ressource   : {}", resource);
    crate::kprintln!("  {}", question);
    crate::kprintln!("  O = accorder, N = refuser, M = accorder et memoriser");
    crate::vga::set_color(Color::LightGray, Color::Black);

    crate::serial_println!(
        "[prompt] \"{}\" ({}) demande {} - {} [O/N/M, {} s]",
        display,
        app_id,
        resource,
        question,
        TIMEOUT_MS / 1000
    );

    let decision = read_answer(app_id, resource);

    crate::vga::set_color(
        if decision.allowed() {
            Color::LightGreen
        } else {
            Color::LightRed
        },
        Color::Black,
    );
    crate::kprintln!("  -> {}", decision.as_str());
    crate::vga::set_color(Color::LightGray, Color::Black);
    decision
}

/// Waits for a keystroke, for at most `TIMEOUT_MS`.
fn read_answer(app_id: &str, resource: &str) -> Decision {
    use crate::drivers::keyboard::Key;

    // Same rule as the graphical prompt: what was typed before the
    // question was asked does not answer it. See
    // `drivers::keyboard::drain`.
    crate::drivers::keyboard::drain();

    let per_tick = 1000 / crate::TIMER_HZ as u64;
    let deadline = crate::idt::ticks() + TIMEOUT_MS.div_ceil(per_tick).max(1);

    loop {
        while let Some(key) = crate::drivers::keyboard::read_key() {
            if let Key::Char(character) = key {
                match character.to_ascii_lowercase() {
                    'o' => return Decision::Allow,
                    'n' => return Decision::Deny,
                    'm' => {
                        let _ = remember(app_id, resource, Decision::Allow);
                        return Decision::Allow;
                    }
                    _ => {}
                }
            }
        }

        if crate::idt::ticks() >= deadline {
            EXPIRED.fetch_add(1, Ordering::Relaxed);
            crate::serial_println!("[prompt] delai depasse - refus par defaut");
            return Decision::Deny;
        }

        let _ = crate::syscall::invoke(
            crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
            [per_tick, 0, 0, 0, 0, 0],
        );
    }
}

pub fn dump_state() {
    let (asked, remembered, expired) = counters();
    if asked == 0 && remembered == 0 {
        return;
    }
    crate::serial_println!(
        "[prompt] {} invites posees, {} reponses memorisees, {} expirations",
        asked,
        remembered,
        expired
    );
}

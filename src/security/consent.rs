//! 10.3.5 -- Authorisation prompt, graphical (spec 12.5).
//!
//! ## What phase 6.4.2 could not do
//!
//! `prompt::ask` already showed the identity, the resource and a
//! question, already remembered the answer in
//! `/users/{user}/settings/permissions/{app_id}.cfg`, and already
//! refused by default on expiry. Its own header named what was missing:
//! *"the fourth [element], the **window**, assumes the compositor of 8.1,
//! which is phase 7"*. Phase 7 exists now, so the window can.
//!
//! ## The tick box is a state, not a third key
//!
//! 12.5 asks for a box "Remember this choice", and a box has a state one
//! can see before answering. The phase 6 prompt approximated it with a
//! third key -- `M` for "allow and remember" -- which works but cannot
//! remember a **refusal**, and shows nothing until it is over. Here Space
//! toggles the box, `O` and `N` answer, and what is remembered is the
//! answer given with the box ticked. That is four combinations from three
//! keys, and it is the only shape that lets a user record "never".
//!
//! The old keys still work: `M` is "allow and remember". It costs one
//! line and keeps the serial transcripts of phase 6 readable.
//!
//! ## Why the requesting task draws it itself
//!
//! A broker task would be the usual answer, and it is what the compositor
//! is for everything else. It is the wrong shape here: the prompt is
//! **synchronous** -- the system call that triggered it is waiting -- so a
//! broker would need a reply channel, a correlation identifier and a
//! timeout of its own, all to serialise prompts that are already
//! serialised by the fact that only one person is looking at the screen.
//!
//! The requester connects, draws in the `Modal` layer, reads the answer,
//! and destroys its surface. If it dies mid-prompt the surface goes with
//! it, which is exactly what the compositor's dead-owner collection is
//! for.
//!
//! ## The keys are read from the driver, not from the compositor
//!
//! Same reason phase 6.4.2 did it: the answer is a modal one, nothing
//! else is entitled to those keystrokes while it is up, and going through
//! the compositor would mean owning the focus -- which would take it away
//! from the very window whose call is blocked.

use alloc::string::String;

use crate::gui::client::Client;
use crate::gui::surface::Layer;
use crate::prompt::Decision;

use super::policy::{Context, Resource};

/// Time left to the user, in milliseconds.
///
/// Longer than the ten seconds of the text prompt: a graphical dialog is
/// read, not reacted to, and a deadline shorter than the reading makes
/// the default answer the usual one.
const TIMEOUT_MS: u64 = 20_000;

/// Width and height of the dialog, in pixels.
const WIDTH: i32 = 520;
const HEIGHT: i32 = 210;

/// What the user answered, and whether to keep it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Answer {
    pub decision: Decision,
    pub remember: bool,
}

/// Asks about one resource for one application.
///
/// Returns the remembered choice without showing anything when there is
/// one -- that is what makes the box worth ticking, and it is also what
/// makes the mechanism testable with no human: write the file, then
/// observe that nothing is displayed and the recorded answer comes back.
pub fn ask(context: &Context, resource: &Resource) -> Decision {
    let app_id = context.identity.app_id.clone();
    let display = context.identity.display_name.clone();
    let key = resource.key();

    if let Some(decision) = crate::prompt::remembered(&app_id, &key) {
        crate::serial_println!(
            "[secu] \"{}\" demande {} - choix memorise : {}",
            display,
            key,
            decision.as_str()
        );
        return decision;
    }

    let question = alloc::format!(
        "{} ({}) demande : {}",
        display,
        context.identity.format.extension(),
        resource.describe()
    );

    // No compositor: the text prompt of 6.4.2 is not a degraded mode, it
    // is the same prompt on the only screen there is. A machine booted
    // with `-vga none` must still be able to refuse.
    if !crate::gui::is_active() {
        return crate::prompt::ask(&app_id, &display, &key, &question);
    }

    let answer = show(context, resource, &question);

    if answer.remember && crate::prompt::remember(&app_id, &key, answer.decision).is_ok() {
        crate::serial_println!("[secu] choix memorise pour \"{}\" : {}", key, answer.decision.as_str());
    }

    answer.decision
}

/// Draws the dialog and waits for the answer.
fn show(context: &Context, resource: &Resource, question: &str) -> Answer {
    let Some(mut client) = Client::connect(2_000) else {
        // The compositor said it was active and does not answer. Refusing
        // is the only safe reading: granting because the display failed
        // would make a broken compositor a way in.
        crate::serial_println!("[secu] compositeur injoignable - refus par defaut");
        return Answer {
            decision: Decision::Deny,
            remember: false,
        };
    };

    let Some((screen_width, screen_height, _, _)) = client.screen_info() else {
        return Answer {
            decision: Decision::Deny,
            remember: false,
        };
    };
    let x = (screen_width as i32 - WIDTH) / 2;
    let y = (screen_height as i32 - HEIGHT) / 2;
    let theme = client.theme().unwrap_or_default();

    let Some(surface) = client.create_surface(Layer::Modal, x, y, WIDTH, HEIGHT, "Autorisation")
    else {
        return Answer {
            decision: Decision::Deny,
            remember: false,
        };
    };

    // Whatever was typed before this dialog existed must not answer it.
    // See `drivers::keyboard::drain`: the self-test of 10.3 watched a
    // leftover keystroke from the login screen accept an authorisation
    // the moment it was displayed.
    let stale = crate::drivers::keyboard::drain();
    if stale > 0 {
        crate::serial_println!("[secu] {} touche(s) en attente ecartee(s)", stale);
    }

    let mut remember = false;
    paint(&mut client, surface, &theme, context, resource, question, remember);

    crate::serial_println!(
        "[secu] invite graphique surface {} - {} [O/N, espace = memoriser, {} s]",
        surface,
        resource.key(),
        TIMEOUT_MS / 1000
    );

    let per_tick = 1000 / crate::TIMER_HZ as u64;
    let deadline = crate::idt::ticks() + TIMEOUT_MS.div_ceil(per_tick).max(1);
    let mut decision = None;

    'wait: loop {
        while let Some(key) = crate::drivers::keyboard::read_key() {
            use crate::drivers::keyboard::Key;
            match key {
                Key::Char(' ') => {
                    remember = !remember;
                    paint(
                        &mut client, surface, &theme, context, resource, question, remember,
                    );
                }
                Key::Char(character) => match character.to_ascii_lowercase() {
                    'o' => {
                        decision = Some(Decision::Allow);
                        break 'wait;
                    }
                    'n' => {
                        decision = Some(Decision::Deny);
                        break 'wait;
                    }
                    // Kept from 6.4.2: "allow and remember" in one key.
                    'm' => {
                        remember = true;
                        decision = Some(Decision::Allow);
                        break 'wait;
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        if crate::idt::ticks() >= deadline {
            crate::serial_println!("[secu] delai depasse - refus par defaut (12.5)");
            break;
        }

        let _ = crate::syscall::invoke(
            crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
            [per_tick, 0, 0, 0, 0, 0],
        );
    }

    // The surface goes before the answer is returned: leaving it up while
    // the caller resumes would put a dead dialog on the screen, which is
    // the defect 8.3 asks modules to avoid and applies here word for
    // word.
    client.destroy_surface(surface);

    // An expiry refuses, and does not remember. Recording a refusal
    // nobody made would silently turn a moment of inattention into a
    // permanent "no".
    match decision {
        Some(decision) => Answer { decision, remember },
        None => Answer {
            decision: Decision::Deny,
            remember: false,
        },
    }
}

/// Complete redraw of the dialog.
fn paint(
    client: &mut Client,
    surface: crate::gui::surface::SurfaceId,
    theme: &crate::gui::theme::Theme,
    context: &Context,
    resource: &Resource,
    question: &str,
    remember: bool,
) {
    let padding = theme.padding as i32;
    let line = 16;

    client.reset(surface);
    client.fill(surface, theme.surface);
    client.frame(surface, 0, 0, WIDTH, HEIGHT, theme.warning, 2);

    // Title bar of the dialog, in the warning colour: this is not an
    // ordinary window, and it must not look like one.
    client.rect(surface, 2, 2, WIDTH - 4, 24, theme.warning);
    client.text(
        surface,
        padding,
        8,
        "AUTORISATION DEMANDEE",
        theme.background,
        None,
    );

    let mut cursor = 24 + padding;
    for (label, value) in [
        ("Application", context.identity.display_name.as_str()),
        ("Identifiant", context.identity.app_id.as_str()),
        ("Groupe", context.identity.group.as_str()),
        ("Type", context.identity.format.extension()),
    ] {
        client.text(surface, padding, cursor, label, theme.text_dim, None);
        client.text(surface, padding + 110, cursor, value, theme.text, None);
        cursor += line;
    }

    let requested = resource.describe();
    client.text(surface, padding, cursor, "Ressource", theme.text_dim, None);
    client.text(
        surface,
        padding + 110,
        cursor,
        &crate::gui::client::ellipsize(&requested, 48),
        theme.accent,
        None,
    );
    cursor += line + 4;

    client.text(
        surface,
        padding,
        cursor,
        &crate::gui::client::ellipsize(question, 62),
        theme.text,
        None,
    );
    cursor += line + 6;

    // The tick box of 12.5. Drawn as a box and a mark rather than written
    // as text, because what the specification asks for is something the
    // user can *see the state of* before answering.
    let box_size = 12;
    client.frame(surface, padding, cursor, box_size, box_size, theme.border, 1);
    if remember {
        client.line(
            surface,
            padding + 2,
            cursor + 2,
            padding + box_size - 3,
            cursor + box_size - 3,
            theme.success,
        );
        client.line(
            surface,
            padding + box_size - 3,
            cursor + 2,
            padding + 2,
            cursor + box_size - 3,
            theme.success,
        );
    }
    client.text(
        surface,
        padding + box_size + 8,
        cursor,
        "Se souvenir de ce choix (espace)",
        theme.text,
        None,
    );
    cursor += line + 8;

    client.text(
        surface,
        padding,
        cursor,
        "O = accorder     N = refuser     sans reponse = refus",
        theme.text_dim,
        None,
    );

    client.commit(surface);
}

/// Revocation of 12.5, exposed here so the settings have one entry point.
pub fn revoke(app_id: &str, resource: &str) -> Result<(), crate::fs::FsError> {
    crate::prompt::revoke(app_id, resource)
}

/// Choices currently recorded for an application.
///
/// Returned as pairs so a settings panel can list and revoke them; 12.5
/// requires the choice to be revocable, and a choice one cannot enumerate
/// is not revocable in practice.
pub fn recorded(app_id: &str) -> alloc::vec::Vec<(String, Decision)> {
    let mut out = alloc::vec::Vec::new();
    if !crate::fs::is_ready() {
        return out;
    }
    let path = crate::prompt::decision_path(app_id);
    let Ok(bytes) = crate::fs::read_file(&path, crate::fs::perm::Credentials::kernel()) else {
        return out;
    };
    let Ok(text) = core::str::from_utf8(&bytes) else {
        return out;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let decision = match value.trim() {
            "allow" => Decision::Allow,
            "deny" => Decision::Deny,
            _ => continue,
        };
        out.push((String::from(key.trim()), decision));
    }
    out
}

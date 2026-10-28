//! 10.3.4 -- The per-type sandbox, enforced (spec 12.4).
//!
//! ## The limit this closes
//!
//! The roadmap carried, since phase 4: *"the path sandbox of 12.4 is
//! described, not enforced. `tree::sandbox_roots` gives the table;
//! nothing makes it hold at run time."* A table nobody consults is
//! documentation, not a rule, and calling it a sandbox was the part that
//! was wrong.
//!
//! This module is the enforcement point. Every filesystem access made
//! **on behalf of an application** passes through `check`, which answers
//! from the roots of 12.4 and from the account of the session.
//!
//! ## Why the account is not in the table
//!
//! 12.4 grants `.gs` and `.cus` `/users/{user}/home/` on top of their own
//! directory. `{user}` is not known when the binary is loaded -- the
//! session may not even be open yet -- so it cannot be one of the static
//! roots. It is added here, from the session, and it moves when the
//! session changes. That is the difference between a root that comes from
//! the **format** and one that comes from the **context**.
//!
//! ## What "outside" leads to
//!
//! Not a refusal: a prompt (12.5). A user asking an editor to open a file
//! they picked themselves is the ordinary case, and refusing it would
//! make the sandbox a cage rather than a boundary. The prompt is what
//! turns "the program decided" into "the person decided", which is the
//! whole content of 12.5.
//!
//! ## Normalisation comes first
//!
//! `/apps/x/../../sys/config` is inside `/sys/`, whatever it looks like.
//! Every path is normalised before it is compared -- `fs::normalize`
//! already resolves `.` and `..` -- because a check on the written form
//! is a check that a single `..` defeats.

use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::header::BinaryFormat;

use super::policy::{under, Context};

/// Result of a sandbox check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    /// Inside the sandbox: allowed with no question.
    Inside(String),
    /// Outside: 12.5 says to ask.
    Outside,
}

impl Access {
    pub fn inside(&self) -> bool {
        matches!(self, Access::Inside(_))
    }
}

/// The complete set of roots an application may reach, session included.
///
/// The order matters for the message only: the first root is the one a
/// refusal names, and naming the application's own directory is more
/// useful than naming a shared one.
pub fn roots_for(context: &Context, user: Option<&str>) -> Vec<String> {
    let mut roots = context.roots.clone();

    // 12.4 grants the user's home to `.gs` and `.cus`. It is a root of
    // the context, not of the format: it does not exist before a session
    // is open, and it changes with the session.
    if matches!(context.identity.format, BinaryFormat::Gs | BinaryFormat::Cus)
        && let Some(name) = user
    {
        roots.push(alloc::format!("/users/{}/home", name));
    }

    roots
}

/// Is this path inside the sandbox of this application?
///
/// The path is normalised first: a check on the written form is a check
/// a single `..` defeats.
pub fn check(context: &Context, user: Option<&str>, path: &str) -> Access {
    let normalized = crate::fs::normalize(path);
    let roots = roots_for(context, user);

    match roots.iter().find(|root| under(&normalized, root)) {
        Some(root) => Access::Inside(root.clone()),
        None => Access::Outside,
    }
}

/// The 12.4 table, as text, for the state dump.
///
/// Written out rather than derived from the code: the point of showing it
/// is to compare it with the specification, and a table generated from
/// the very code it is meant to verify would agree with itself whatever
/// the code says.
pub fn table() -> [(&'static str, &'static str, &'static str, &'static str); 5] {
    [
        (".dsa", "Confiance totale", "Acces complet", "Libre"),
        (
            ".ss",
            "Isolee des apps user",
            "Etendu selon declaration",
            "Libre avec .ss et .dsa",
        ),
        (
            ".gs",
            "Isolee",
            "/games/{app}/ + /users/{user}/home/",
            "Groupe + port interne",
        ),
        (
            ".cus",
            "Isolee",
            "/apps/{app}/ + /users/{user}/home/",
            "Groupe + port + prompt",
        ),
        (".ras", "Isolee", "/ras/{app}/ uniquement", "Aucune"),
    ]
}

/// Prints the state of the enforcement.
pub fn dump_state() {
    crate::serial_println!("[secu] bac a sable par type (12.4) :");
    for (format, memory, files, ipc) in table() {
        crate::serial_println!(
            "[secu]   {:<5} {:<22} {:<38} {}",
            format,
            memory,
            files,
            ipc
        );
    }
}

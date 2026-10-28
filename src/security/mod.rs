//! Phase 10.3 -- Advanced security (spec 12).
//!
//! | Module | Covers |
//! |---|---|
//! | `policy` | 10.3.3 -- the hybrid deny-list / allow-list model (12.3) |
//! | `sandbox` | 10.3.4 -- the per-type sandbox, enforced (12.4) |
//! | `consent` | 10.3.5 -- the graphical prompt and the remembered choice (12.5) |
//!
//! 10.3.1 and 10.3.2 were built in phase 3 -- `AppIdentity` and the
//! communication table of 12.1 -- and what this phase adds to them is
//! **enforcement**, not structure:
//!
//! * `apps::register` now **refuses** to change the identity of a live
//!   task. 12.1 wants it "immutable at run time" and `DECISIONS.md` lists
//!   that among the points that are not settings; overwriting it, which
//!   is what the phase 6 code did, was that mutation.
//! * `tree::sandbox_roots` had no reader. The roadmap said so from phase
//!   4 on: *"the path sandbox of 12.4 is described, not enforced"*.
//!   `security::check_path` is the reader.
//!
//! ## One entry point, one decision
//!
//! Everything goes through `authorize`. The point is not tidiness: a rule
//! enforced at two places is enforced at neither, because the two drift.
//! The order of the checks lives in `policy::evaluate` and nowhere else,
//! and the self-test verifies the **order**, not just the branches.
//!
//! ## What a task with no identity is
//!
//! A kernel service: scheduler, driver, self-test. It is subject to no
//! rule of 12, and `authorize` grants it everything. That is not a hole,
//! it is what `apps` already documented -- the rules of 12 govern
//! applications between themselves, not the services they rest on. The
//! day a driver is a real ring 3 binary it will have an identity, and it
//! will fall into line with everything else.

pub mod consent;
pub mod policy;
pub mod sandbox;
pub mod selftest;

use core::sync::atomic::{AtomicU64, Ordering};

use crate::process::Pid;
use crate::prompt::Decision;

use policy::{Context, DenyReason, Resource, Verdict};

/// Accesses allowed, refused, and sent to the user.
static ALLOWED: AtomicU64 = AtomicU64::new(0);
static DENIED: AtomicU64 = AtomicU64::new(0);
static PROMPTED: AtomicU64 = AtomicU64::new(0);

/// Counters: (allowed, refused, prompted).
pub fn counters() -> (u64, u64, u64) {
    (
        ALLOWED.load(Ordering::Relaxed),
        DENIED.load(Ordering::Relaxed),
        PROMPTED.load(Ordering::Relaxed),
    )
}

/// Outcome of an authorisation, once the user has had their say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Allowed,
    Denied(DenyReason),
}

impl Outcome {
    pub fn allowed(&self) -> bool {
        matches!(self, Outcome::Allowed)
    }

    pub fn reason(&self) -> Option<DenyReason> {
        match self {
            Outcome::Denied(reason) => Some(*reason),
            Outcome::Allowed => None,
        }
    }
}

/// Decides on one access by one task.
///
/// A task with no identity is a kernel service and is granted everything:
/// see the module header.
///
/// The prompt is raised **here** and not by the caller, so that the
/// remembered choice of 12.5 short-circuits it before anybody is
/// disturbed -- which is also what makes the whole path testable without
/// a human.
pub fn authorize(pid: Pid, resource: Resource) -> Outcome {
    let Some(context) = Context::of(pid) else {
        ALLOWED.fetch_add(1, Ordering::Relaxed);
        return Outcome::Allowed;
    };
    authorize_in(&context, resource)
}

/// Same decision, on a context built by the caller.
///
/// Exposed because the loader and the self-test hold a context before any
/// task exists: an application must be checkable before it is launched,
/// or every refusal would arrive after the fact.
pub fn authorize_in(context: &Context, resource: Resource) -> Outcome {
    match policy::evaluate(context, &resource) {
        Verdict::Allow(_) => {
            ALLOWED.fetch_add(1, Ordering::Relaxed);
            Outcome::Allowed
        }
        Verdict::Deny(reason) => {
            DENIED.fetch_add(1, Ordering::Relaxed);
            crate::serial_println!(
                "[secu] REFUS {} pour \"{}\" : {}",
                resource.describe(),
                context.identity.app_id,
                reason.as_str()
            );
            Outcome::Denied(reason)
        }
        Verdict::Prompt(_) => {
            PROMPTED.fetch_add(1, Ordering::Relaxed);
            match consent::ask(context, &resource) {
                Decision::Allow => {
                    ALLOWED.fetch_add(1, Ordering::Relaxed);
                    Outcome::Allowed
                }
                Decision::Deny => {
                    DENIED.fetch_add(1, Ordering::Relaxed);
                    Outcome::Denied(DenyReason::UserRefused)
                }
            }
        }
    }
}

/// Verdict of the policy with no side effect: no prompt, no counter.
///
/// This is what a self-test and a settings panel need -- both want to
/// know what *would* happen, and neither should make it happen.
pub fn verdict(context: &Context, resource: Resource) -> Verdict {
    policy::evaluate(context, &resource)
}

/// 10.3.4 -- Checks a path against the sandbox of a task.
///
/// The session account is read here rather than passed in: it is the
/// authority on which home directory 12.4 opens, and letting a caller
/// supply it would make the check depend on what the caller claims.
///
/// The session home is folded into the **context** rather than tested
/// separately, so `policy::evaluate` sees the same roots this function
/// does. Two root lists -- one for the sandbox test, one for the policy
/// -- is the shape in which they drift apart, and the drift shows up as
/// an access allowed by one and refused by the other.
pub fn check_path(pid: Pid, path: &str, write: bool) -> Outcome {
    let Some(mut context) = Context::of(pid) else {
        ALLOWED.fetch_add(1, Ordering::Relaxed);
        return Outcome::Allowed;
    };
    let session = crate::gui::session_user();
    context.roots = sandbox::roots_for(&context, session.as_deref());

    // Normalised before anything is compared: `/apps/x/../../sys/config`
    // is inside `/sys/` whatever it looks like, and a check on the
    // written form is a check a single `..` defeats.
    let normalized = crate::fs::normalize(path);
    let resource = if write {
        Resource::FileWrite(&normalized)
    } else {
        Resource::FileRead(&normalized)
    };

    authorize_in(&context, resource)
}

/// Prints the state of the subsystem.
pub fn dump_state() {
    let (allowed, denied, prompted) = counters();
    crate::serial_println!(
        "[secu] {} acces autorises, {} refuses, {} soumis a l'utilisateur",
        allowed,
        denied,
        prompted
    );
    sandbox::dump_state();
    crate::prompt::dump_state();
}

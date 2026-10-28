//! 10.3.3 -- The hybrid permission model (spec 12.3).
//!
//! ## The two lists are not symmetric
//!
//! 12.3 puts two lists side by side, and it is tempting to treat them as
//! one table with a sign. They do not work the same way at all:
//!
//! * The **deny-list** is absolute. `.ras` may not talk to another app,
//!   no process reads another's memory, no app writes another's binary.
//!   Nothing lifts these -- not a header, not a user, not a prompt. Ask
//!   the user and the rule stops being a rule.
//! * The **allow-list** is declarative. Internet, microphone, webcam,
//!   `/sys/`, `SYS_HW_*`, user management: declared in the header or
//!   refused. Not promptable either, and for a different reason -- a
//!   header is fixed at compile time (12.1), so there is nothing a "yes"
//!   could write into. Prompting would be theatre.
//!
//! ## Then what is the prompt of 12.5 for?
//!
//! For the third case, the one 12.4 opens: a **path outside the
//! sandbox**. A `.cus` lives in `/apps/{id}/` and the user's home; asking
//! it to open a file the user just picked elsewhere is legitimate, cannot
//! be foreseen at compile time, and the only authority able to settle it
//! is the person in front of the screen. That is exactly what 12.5
//! describes, and it is the only branch that reaches a prompt.
//!
//! So the order of the decision is fixed, and it is not an
//! implementation detail:
//!
//! ```text
//!   1. deny-list        -> refused, definitively
//!   2. allow-list       -> refused if undeclared
//!   3. sandbox (12.4)   -> allowed inside, prompt outside
//!   4. otherwise        -> allowed
//! ```
//!
//! Putting the allow-list before the deny-list would let a `.ras`
//! declaring `IPC_PUBLIC` talk to other apps. Putting the sandbox before
//! the allow-list would let a `.cus` reach `/sys/` by answering a prompt.
//! Neither is a hypothesis: both are what the wrong order produces, and
//! the self-test checks the order rather than the branches.

use alloc::string::String;

use crate::binfmt::header::BinaryFormat;
use crate::binfmt::identity::{AppIdentity, Permissions};
use crate::process::Pid;

// ============================================================
//  What is being asked for
// ============================================================

/// A resource an application may want to reach.
///
/// Paths are borrowed rather than owned: the check happens on the call
/// path of a system call, and allocating a string to refuse an access is
/// exactly the wrong place to allocate.
#[derive(Debug, Clone, Copy)]
pub enum Resource<'a> {
    /// One of the declared permission bits of 12.3.
    Declared(u64),
    /// Reading a file or a directory.
    FileRead(&'a str),
    /// Writing, creating, deleting.
    FileWrite(&'a str),
    /// Direct IPC towards another task.
    IpcTo(Pid),
    /// Opening an internal port towards another application (12.2).
    PortTo(Pid),
    /// Reading or writing the memory of another task -- deny-list.
    ForeignMemory(Pid),
    /// Writing over the binary of another application -- deny-list.
    ForeignBinary(&'a str),
}

impl Resource<'_> {
    /// Label shown in a prompt or a log line.
    pub fn describe(&self) -> String {
        match self {
            Resource::Declared(bit) => String::from(permission_label(*bit)),
            Resource::FileRead(path) => alloc::format!("lecture de {}", path),
            Resource::FileWrite(path) => alloc::format!("ecriture de {}", path),
            Resource::IpcTo(pid) => alloc::format!("IPC vers {}", crate::apps::display_name(*pid)),
            Resource::PortTo(pid) => {
                alloc::format!("port interne vers {}", crate::apps::display_name(*pid))
            }
            Resource::ForeignMemory(pid) => alloc::format!("memoire du PID {}", pid),
            Resource::ForeignBinary(app) => alloc::format!("binaire de {}", app),
        }
    }

    /// Stable key under which a remembered choice is filed (12.5).
    ///
    /// It must not carry a PID: a PID changes at every launch, and a
    /// choice remembered against one would be forgotten at the next boot
    /// while looking as though it had been kept.
    pub fn key(&self) -> String {
        match self {
            Resource::Declared(bit) => alloc::format!("perm:{}", permission_label(*bit)),
            Resource::FileRead(path) => alloc::format!("lecture:{}", path),
            Resource::FileWrite(path) => alloc::format!("ecriture:{}", path),
            Resource::IpcTo(pid) | Resource::PortTo(pid) => {
                match crate::apps::identity_of(*pid) {
                    Some(identity) => alloc::format!("ipc:{}", identity.app_id),
                    None => String::from("ipc:systeme"),
                }
            }
            Resource::ForeignMemory(_) => String::from("memoire:externe"),
            Resource::ForeignBinary(app) => alloc::format!("binaire:{}", app),
        }
    }
}

/// Name of a permission bit, for a message.
pub fn permission_label(bit: u64) -> &'static str {
    match bit {
        Permissions::NETWORK_INTERNET => "reseau internet",
        Permissions::NETWORK_LOCAL => "reseau local",
        Permissions::MICROPHONE => "microphone",
        Permissions::WEBCAM => "webcam",
        Permissions::AUDIO_OUTPUT => "sortie audio",
        Permissions::SYS_DIRECTORY => "dossier /sys/",
        Permissions::HARDWARE => "acces materiel",
        Permissions::USER_MANAGEMENT => "gestion des comptes",
        Permissions::KERNEL_SPACE => "espace noyau",
        Permissions::FS_USER_HOME => "dossier personnel",
        Permissions::FS_FULL => "systeme de fichiers complet",
        Permissions::IPC_PUBLIC => "IPC publique",
        Permissions::IPC_INTERNAL_PORTS => "ports internes",
        Permissions::NOTIFICATIONS => "notifications",
        Permissions::FULLSCREEN => "plein ecran",
        Permissions::CLIPBOARD => "presse-papiers",
        _ => "permission inconnue",
    }
}

// ============================================================
//  The verdict
// ============================================================

/// Why an access was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// Deny-list of 12.3: a `.ras` has no communication at all.
    RasIsolated,
    /// Deny-list of 12.3: the memory of another process.
    ForeignMemory,
    /// Deny-list of 12.3: the binaries of another application.
    ForeignBinary,
    /// Allow-list of 12.3: the permission is not in the header.
    NotDeclared,
    /// 12.4: the permission is above the ceiling of the format.
    AboveCeiling,
    /// 12.1: the two applications must go through an internal port.
    NeedsPort,
    /// The user refused, or a remembered choice refuses.
    UserRefused,
}

impl DenyReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            DenyReason::RasIsolated => "un .ras est totalement isole (12.3)",
            DenyReason::ForeignMemory => "la memoire d'un autre processus est interdite (12.3)",
            DenyReason::ForeignBinary => "le binaire d'une autre app est interdit (12.3)",
            DenyReason::NotDeclared => "permission non declaree dans l'en-tete (12.3)",
            DenyReason::AboveCeiling => "permission au-dessus du plafond du format (12.4)",
            DenyReason::NeedsPort => "groupes differents : passer par un port interne (12.1)",
            DenyReason::UserRefused => "refuse par l'utilisateur (12.5)",
        }
    }

    /// Can this refusal be lifted by asking the user?
    ///
    /// Only `UserRefused` can, and only because the user may change their
    /// mind. Everything else is structural: asking would suggest an
    /// answer exists, and there is none.
    pub fn promptable(&self) -> bool {
        matches!(self, DenyReason::UserRefused)
    }
}

/// Outcome of a policy evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Allowed, with the rule that allowed it.
    Allow(&'static str),
    /// Refused, definitively.
    Deny(DenyReason),
    /// Outside the sandbox: 12.5 says to ask. The string is the sandbox
    /// root the request left.
    Prompt(String),
}

impl Verdict {
    pub fn allowed(&self) -> bool {
        matches!(self, Verdict::Allow(_))
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Allow(_) => "autorise",
            Verdict::Deny(_) => "refuse",
            Verdict::Prompt(_) => "demande a l'utilisateur",
        }
    }
}

// ============================================================
//  The deny-list (12.3)
// ============================================================

/// The three absolute rules, evaluated before anything else.
///
/// Returns `None` when the deny-list has nothing to say -- which is the
/// common case, and the reason this function is cheap.
pub fn denylist(identity: &AppIdentity, resource: &Resource) -> Option<DenyReason> {
    match resource {
        // "`.ras` cannot communicate with other apps" -- 12.3, without an
        // exception clause, so there is none here either.
        Resource::IpcTo(_) | Resource::PortTo(_) if identity.format == BinaryFormat::Ras => {
            Some(DenyReason::RasIsolated)
        }
        // "inter-process memory access". No format lifts it, `.dsa`
        // included: a debugger is a supervisor protocol, not a read of
        // someone else's pages.
        Resource::ForeignMemory(_) => Some(DenyReason::ForeignMemory),
        // "modification of another app's binaries".
        Resource::ForeignBinary(_) => Some(DenyReason::ForeignBinary),
        // A write whose path lands inside another application's install
        // directory is the same rule, written on a path rather than on an
        // identifier. Without this line the rule would only cover the
        // callers that thought to name it.
        Resource::FileWrite(path) => foreign_install(identity, path).then_some(DenyReason::ForeignBinary),
        _ => None,
    }
}

/// Does this path belong to the install directory of *another*
/// application?
///
/// The comparison is on a whole path segment. `/apps/mon` must not match
/// `/apps/monautre` -- the same trap phase 8 met on package prefixes, and
/// the reason it is written out rather than left to `starts_with`.
fn foreign_install(identity: &AppIdentity, path: &str) -> bool {
    for root in ["/apps/", "/games/", "/ras/", "/sysapp/"] {
        let Some(rest) = path.strip_prefix(root) else {
            continue;
        };
        let owner = rest.split('/').next().unwrap_or("");
        if owner.is_empty() {
            continue;
        }
        if owner != identity.app_id {
            return true;
        }
    }
    false
}

// ============================================================
//  The allow-list (12.3)
// ============================================================

/// Bit of 12.3 a resource requires, when it requires one.
fn required_permission(resource: &Resource) -> Option<u64> {
    match resource {
        Resource::Declared(bit) => Some(*bit),
        // `/sys/` is named by 12.3 among the sensitive accesses. Reading
        // it counts: a configuration file is read, not written, and it is
        // its content that is sensitive.
        Resource::FileRead(path) | Resource::FileWrite(path) if under(path, "/sys") => {
            Some(Permissions::SYS_DIRECTORY)
        }
        _ => None,
    }
}

/// Is this path inside this root, on a segment boundary?
pub fn under(path: &str, root: &str) -> bool {
    if root == "/" {
        return path.starts_with('/');
    }
    let root = root.trim_end_matches('/');
    match path.strip_prefix(root) {
        Some("") => true,
        Some(rest) => rest.starts_with('/'),
        None => false,
    }
}

// ============================================================
//  Evaluation
// ============================================================

/// Everything the policy needs to know about one application.
///
/// Identity and declaration travel together because they come from the
/// same header and have the same life span. Separating them would allow
/// a context with an identity and no declaration, which the engine would
/// read as "declares nothing" -- a refusal that looks like a rule and is
/// really a missing record.
#[derive(Debug, Clone)]
pub struct Context {
    pub identity: AppIdentity,
    pub permissions: Permissions,
    /// Sandbox roots of 12.4, as `fs::tree::sandbox_roots` gives them.
    ///
    /// Carried rather than recomputed at each check: building them
    /// allocates, and refusing an access should not.
    pub roots: alloc::vec::Vec<String>,
}

impl Context {
    /// Builds the context of a running task.
    ///
    /// `None` for a task with no identity: a kernel service, which no
    /// rule of 12 governs. See the header of `apps`.
    pub fn of(pid: Pid) -> Option<Self> {
        let identity = crate::apps::identity_of(pid)?;
        let permissions = crate::apps::permissions_of(pid).unwrap_or(Permissions::NONE);
        let roots = crate::fs::tree::sandbox_roots(identity.format, &identity.app_id);
        Some(Context {
            identity,
            permissions,
            roots,
        })
    }
}

/// Applies the four steps to one request.
pub fn evaluate(context: &Context, resource: &Resource) -> Verdict {
    let identity = &context.identity;
    let roots = &context.roots;
    // --- 1. deny-list ---
    if let Some(reason) = denylist(identity, resource) {
        return Verdict::Deny(reason);
    }

    // --- 2. allow-list ---
    if let Some(bit) = required_permission(resource) {
        // The ceiling of 12.4 comes first: a header may claim a bit its
        // format cannot carry, and `binfmt` refuses such a binary at
        // load. Checking it again here is not redundant -- the identity
        // may come from a registry, and a rule that is only enforced at
        // one point is enforced nowhere.
        if bit & Permissions::ceiling(identity.format) == 0 {
            return Verdict::Deny(DenyReason::AboveCeiling);
        }
        if !declared(identity, context.permissions, bit) {
            return Verdict::Deny(DenyReason::NotDeclared);
        }
    }

    // --- 3. communication rules of 12.1 ---
    if let Resource::IpcTo(pid) = resource {
        if let Some(other) = crate::apps::identity_of(*pid) {
            use crate::binfmt::identity::Communication;
            match identity.communication_with(&other) {
                Communication::DirectIpc => return Verdict::Allow("12.1 : IPC directe"),
                Communication::InternalPort => return Verdict::Deny(DenyReason::NeedsPort),
                Communication::Blocked => return Verdict::Deny(DenyReason::RasIsolated),
            }
        }
        // No identity on the far side: a system service. See `apps`.
        return Verdict::Allow("12.1 : destinataire systeme");
    }

    // --- 4. sandbox of 12.4 ---
    if let Resource::FileRead(path) | Resource::FileWrite(path) = resource {
        if roots.iter().any(|root| under(path, root)) {
            return Verdict::Allow("12.4 : dans le bac a sable");
        }
        // The user's home is open to every format 12.4 grants it to, and
        // it is not one of the roots because the account is not known
        // until the session opens.
        return Verdict::Prompt(roots.first().cloned().unwrap_or_default());
    }

    Verdict::Allow("aucune regle ne s'y oppose")
}

/// Is the permission declared in the header?
///
/// A `.dsa` is "total trust" by 12.4 and declares nothing: refusing it
/// for want of a declaration would make the system apps unable to run.
/// The trust is placed in the **signature**, which `binfmt` requires to
/// be SYSTEM for a `.dsa` -- not in the declaration.
fn declared(identity: &AppIdentity, permissions: Permissions, bit: u64) -> bool {
    if identity.format == BinaryFormat::Dsa {
        return true;
    }
    permissions.contains(bit)
}

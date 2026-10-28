//! Registry of the application identities (spec 12.1).
//!
//! ## The missing link
//!
//! `AppIdentity` has existed since phase 3: it is read from the header of
//! a binary, immutable, and can say which communication mode 12.1 allows
//! between two applications. What was missing is the link between a
//! **running task** and the identity of the binary it came from. Without
//! it, `ipc::send` could check nothing: it saw two PIDs, and a PID says
//! nothing of the group nor of the format.
//!
//! This module holds that link. The loader deposits the identity there
//! when it creates the task; IPC consults it on every send.
//!
//! ## Tasks with no identity are the system
//!
//! A kernel task -- the scheduler, a driver, the self-test -- has no
//! binary header and therefore no identity. It is subject to no rule of
//! 12.1, and that is correct: those rules govern **applications** between
//! themselves, not the services they depend on. A keyboard driver unable
//! to publish to an application of another group would be useless.
//!
//! The consequence to keep in mind: as long as the drivers run in ring 0,
//! "no identity" means "trusted". The day a driver is a real binary, it
//! will have one, and will fall into line.

pub mod browser;
pub mod explorer;
pub mod network_settings;
pub mod notepad;
pub mod clock_settings;
pub mod parametres;
pub mod player;
pub mod playback;
pub mod recorder;
pub mod sound_settings;
pub mod widgets;
pub mod window;

use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::identity::{AppIdentity, Communication, Permissions};
use crate::process::Pid;
use crate::vga::SpinLock;

/// Number of applications identified at the same time.
const MAX_APPS: usize = 32;

struct Slot {
    used: bool,
    pid: Pid,
    identity: Option<AppIdentity>,
    /// Allow-list of 12.3, read from the header of the same binary.
    ///
    /// Held here rather than beside it because it has exactly the same
    /// life span and the same source: separating them would let a task
    /// exist with an identity and no declaration, which the policy engine
    /// of 10.3.3 would then read as "declares nothing" -- a refusal that
    /// looks like a rule and is really a missing record.
    permissions: Permissions,
}

impl Slot {
    const fn empty() -> Self {
        Slot {
            used: false,
            pid: 0,
            identity: None,
            permissions: Permissions::NONE,
        }
    }
}

static REGISTRY: SpinLock<[Slot; MAX_APPS]> = SpinLock::new([const { Slot::empty() }; MAX_APPS]);

/// Attaches an identity to a task.
///
/// Called by the loader, never by the application: 12.1 wants an identity
/// "defined at compile time and immutable at run time", which no system
/// call must be able to contradict.
pub fn register(pid: Pid, identity: AppIdentity, permissions: Permissions) {
    let mut registry = REGISTRY.lock();

    if let Some(slot) = registry.iter_mut().find(|s| s.used && s.pid == pid) {
        // 12.1 makes the identity "immutable at run time", and
        // `DECISIONS.md` lists that among the points that are not
        // settings. An overwrite here would be exactly the mutation the
        // rule forbids, so it is refused rather than performed.
        //
        // Re-registering the *same* identity is not a mutation and stays
        // silent: the loader may legitimately be replayed on a slot it
        // already filled, and turning that into a warning would train the
        // reader to ignore the message.
        let same = slot
            .identity
            .as_ref()
            .map(|current| {
                current.app_id == identity.app_id
                    && current.group == identity.group
                    && current.format == identity.format
                    && current.signature == identity.signature
            })
            .unwrap_or(false);
        if !same {
            crate::serial_println!(
                "[apps] REFUS : identite du PID {} deja fixee - 12.1 la veut immuable",
                pid
            );
            return;
        }
        slot.permissions = permissions;
        return;
    }
    if let Some(slot) = registry.iter_mut().find(|s| !s.used) {
        slot.used = true;
        slot.pid = pid;
        slot.identity = Some(identity);
        slot.permissions = permissions;
        return;
    }

    // Table full: recording nothing would mean "system task", that is to
    // say trusted. Refusing outright is worth more than a silence that
    // would grant rights.
    crate::serial_println!(
        "[apps] table pleine - l'identite du PID {} n'est pas enregistree",
        pid
    );
}

/// Forgets a dead task.
pub fn forget(pid: Pid) {
    let mut registry = REGISTRY.lock();
    if let Some(slot) = registry.iter_mut().find(|s| s.used && s.pid == pid) {
        slot.used = false;
        slot.pid = 0;
        slot.identity = None;
        slot.permissions = Permissions::NONE;
    }
}

/// Allow-list declared by the binary a task came from (12.3).
///
/// `None` for a task with no identity -- a kernel service. The policy
/// engine reads that as "not an application", never as "declares
/// nothing": the two lead to opposite decisions.
pub fn permissions_of(pid: Pid) -> Option<Permissions> {
    let registry = REGISTRY.lock();
    registry
        .iter()
        .find(|s| s.used && s.pid == pid)
        .filter(|s| s.identity.is_some())
        .map(|s| s.permissions)
}

/// Identity of a task, if it has one.
pub fn identity_of(pid: Pid) -> Option<AppIdentity> {
    let registry = REGISTRY.lock();
    registry
        .iter()
        .find(|s| s.used && s.pid == pid)
        .and_then(|s| s.identity.clone())
}

/// Task carrying a given application identifier.
pub fn find_by_app_id(app_id: &str) -> Option<Pid> {
    let registry = REGISTRY.lock();
    registry
        .iter()
        .find(|s| {
            s.used
                && s.identity
                    .as_ref()
                    .map(|i| i.app_id == app_id)
                    .unwrap_or(false)
        })
        .map(|s| s.pid)
}

/// Communication mode allowed between two tasks (12.1).
///
/// Two tasks of which at least one has no identity communicate freely:
/// see the module header.
pub fn communication(from: Pid, to: Pid) -> Communication {
    if from == to {
        return Communication::DirectIpc;
    }
    let (Some(source), Some(target)) = (identity_of(from), identity_of(to)) else {
        return Communication::DirectIpc;
    };
    source.communication_with(&target)
}

/// Readable name of a task, for an error message or a prompt.
pub fn display_name(pid: Pid) -> String {
    match identity_of(pid) {
        Some(identity) => identity.display_name.clone(),
        None => match crate::scheduler::task_info(pid) {
            Some(info) => info.name,
            None => alloc::format!("PID {}", pid),
        },
    }
}

/// Registered applications: (PID, identifier, group, format).
pub fn list() -> Vec<(Pid, String, String, &'static str)> {
    let registry = REGISTRY.lock();
    registry
        .iter()
        .filter(|s| s.used)
        .filter_map(|s| {
            let identity = s.identity.as_ref()?;
            Some((
                s.pid,
                identity.app_id.clone(),
                identity.group.clone(),
                identity.format.extension(),
            ))
        })
        .collect()
}

pub fn dump_state() {
    let apps = list();
    if apps.is_empty() {
        return;
    }
    crate::serial_println!("[apps] {} application(s) identifiee(s)", apps.len());
    for (pid, app_id, group, format) in apps {
        crate::serial_println!(
            "[apps]   PID {:<3} {:<28} groupe \"{}\" - {}",
            pid,
            app_id,
            group,
            format
        );
    }
}

// ============================================================
//  Native applications
// ============================================================

/// An application whose code is in the kernel image.
///
/// `(file name, app id, type, entry point)`.
pub type NativeApp = (&'static str, &'static str, crate::process::BinaryType, fn() -> !);

/// Applications the launcher can start.
///
/// ## Why a table, and what becomes of it
///
/// Exactly the same arrangement as `gui::NATIVE_MODULES`, for the same
/// reason and with the same fate. The binary written into `/apps` is a real
/// assembled file -- its header, its signature and its permissions are
/// checked by 3.3 -- but its **code** is not what runs: the loader that
/// would execute a foreign `.cus` in its own address space needs one CR3
/// per process, a limit phase 3 opened and no phase has closed.
///
/// So the file says who the application is and the table says where its
/// code lives. The day the loader can run the file, this table disappears
/// and nothing else changes: neither the format, nor the installation, nor
/// the way the launcher asks.
///
/// Saying it plainly matters because the alternative would read as done: a
/// `.cus` in `/apps` that launches looks exactly like a `.cus` in `/apps`
/// that is executed.
pub static NATIVE_APPS: &[NativeApp] = &[
    (
        "notepad.cus",
        notepad::APP_ID,
        crate::process::BinaryType::Cus,
        notepad::task,
    ),
    (
        "explorer.ss",
        explorer::APP_ID,
        crate::process::BinaryType::Ss,
        explorer::task,
    ),
    (
        player::FILE_NAME,
        player::APP_ID,
        crate::process::BinaryType::Cus,
        player::task,
    ),
    (
        recorder::FILE_NAME,
        recorder::APP_ID,
        crate::process::BinaryType::Cus,
        recorder::task,
    ),
    (
        parametres::FILE_NAME,
        parametres::APP_ID,
        crate::process::BinaryType::Ss,
        parametres::task,
    ),
    (
        sound_settings::FILE_NAME,
        sound_settings::APP_ID,
        crate::process::BinaryType::Ss,
        sound_settings::task,
    ),
    (
        browser::FILE_NAME,
        browser::APP_ID,
        crate::process::BinaryType::Cus,
        browser::task,
    ),
    (
        network_settings::FILE_NAME,
        network_settings::APP_ID,
        crate::process::BinaryType::Ss,
        network_settings::task,
    ),
    (
        clock_settings::FILE_NAME,
        clock_settings::APP_ID,
        crate::process::BinaryType::Ss,
        clock_settings::task,
    ),
];

/// Registers the settings tabs of the native `.ss`. Called once, at boot.
///
/// The tabs are the components' own: listed here only because this is
/// where the native applications are listed. `parametres.ss` reads them
/// from `config` and knows none of them.
pub fn register_settings() {
    sound_settings::register();
    network_settings::register();
    clock_settings::register();
    explorer::settings::register();
}

/// The native application a file name corresponds to, if any.
pub fn native_for(file_name: &str) -> Option<&'static NativeApp> {
    NATIVE_APPS.iter().find(|entry| entry.0 == file_name)
}

/// Installs the binaries of the native applications.
///
/// Returns how many were written. Zero on the second boot is the **correct**
/// answer, not a failure: 17 wants an installation to find what is there
/// rather than recreate it.
pub fn install_native(now: u64) -> usize {
    let mut written = 0;
    if notepad::install(now) {
        written += 1;
    }
    if explorer::install(now) {
        written += 1;
    }
    for installed in [
        player::install(now),
        recorder::install(now),
        sound_settings::install(now),
        parametres::install(now),
        browser::install(now),
        network_settings::install(now),
        clock_settings::install(now),
    ] {
        if installed {
            written += 1;
        }
    }
    // Retired: the demonstration tune older builds installed for the
    // player, and the IDE of the SDK. Deleted, not only no longer
    // written -- a file left on the disk stays in the lists.
    for retired in ["/users/_shared/audio/demo-nova.wav", "/sysapp/devstudio.ss"] {
        let _ = crate::fs::delete(retired, crate::fs::perm::Credentials::kernel(), now);
    }
    written
}

/// What a native application's binary declares, for `install_binary`.
pub struct BinarySpec {
    pub path: &'static str,
    pub title: &'static str,
    pub app_id: &'static str,
    pub display_name: &'static str,
    pub format: crate::binfmt::header::BinaryFormat,
    pub permissions: u64,
}

/// Writes the binary of a native application if it is not there.
///
/// The shape `notepad::install` has: a real assembled file whose header
/// the checks of 3.3 can read, while the code that runs is the native one
/// of `NATIVE_APPS`. Factored for the applications that came after it,
/// rather than copied a third, fourth and fifth time.
pub fn install_binary(spec: &BinarySpec, now: u64) -> bool {
    use crate::binfmt::header::{SignatureType, flags};
    use crate::binfmt::identity::Version;
    use crate::fs::perm::{Credentials, Mode};

    if !crate::fs::is_ready() {
        return false;
    }
    let credentials = Credentials::kernel();
    if let Ok(existing) = crate::fs::read_file(spec.path, credentials) {
        // Re-registered on every boot: the file survives, the table of
        // official digests does not.
        let _ = crate::binfmt::sample::register_as_official(&existing);
        return false;
    }

    let source = alloc::format!(
        ".text\n_start:\n    mov rax, 0x0001\n    lea rdi, [rip + titre]\n    mov rsi, titre_fin - titre\n    syscall\n    mov rax, 0x020B\n    mov rdi, 0\n    syscall\n    hlt\n.data\ntitre:\n    db \"{}\"\ntitre_fin:\n",
        spec.title
    );
    let Ok(object) = crate::sdk::asm::assemble(&source) else {
        return false;
    };
    let emit = crate::sdk::emit::EmitSpec {
        format: spec.format,
        signature: SignatureType::System,
        app_id: spec.app_id,
        author: "Nova OS",
        group: "os.nova",
        display_name: spec.display_name,
        version: Version::new(1, 0, 0, 1),
        permissions: spec.permissions,
        flags: flags::GUI,
        ports: &[],
        stack_size: 64 * 1024,
        authority: None,
    };
    let Ok(image) = crate::sdk::emit::emit(&object, &emit) else {
        return false;
    };
    let _ = crate::binfmt::sample::register_as_official(&image);
    crate::fs::write_file(spec.path, &image, credentials, Mode::system_binary(), now).is_ok()
}

/// Asks the policy of 12.3 whether a native application may use a
/// declared permission -- the microphone, the loudspeaker.
///
/// ## Why from the binary on disk
///
/// A native application is spawned from `NATIVE_APPS`, not by the loader,
/// so no identity is registered for its task and `security::authorize`
/// would grant it everything as a kernel service. The header of its
/// installed binary is what it **declares**; building the context from it
/// is what makes the allow-list of 12.3 bite: remove `MICROPHONE` from the
/// header and the recorder is refused, by the same code that refuses any
/// other application.
pub fn authorize_native(binary: &str, permission: u64) -> Result<(), alloc::string::String> {
    use crate::security::policy::{Context, Resource};

    let bytes = crate::fs::read_file(binary, crate::fs::perm::Credentials::kernel())
        .map_err(|error| alloc::format!("{} illisible : {}", binary, error.message()))?;
    let header = crate::binfmt::parse(&bytes)
        .map_err(|_| alloc::format!("{} : en-tete invalide", binary))?;
    let roots = crate::fs::tree::sandbox_roots(header.identity.format, &header.identity.app_id);
    let context = Context {
        identity: header.identity,
        permissions: header.permissions,
        roots,
    };
    match crate::security::authorize_in(&context, Resource::Declared(permission)) {
        crate::security::Outcome::Allowed => Ok(()),
        crate::security::Outcome::Denied(reason) => Err(alloc::string::String::from(reason.as_str())),
    }
}

/// A directory and a name, joined by exactly one slash.
pub fn join_path(directory: &str, name: &str) -> alloc::string::String {
    if directory.ends_with('/') {
        alloc::format!("{}{}", directory, name)
    } else {
        alloc::format!("{}/{}", directory, name)
    }
}

/// The account in session and its rights, for an application that works
/// in the user's name. `None` with no session open.
pub fn session_credentials() -> Option<(alloc::string::String, crate::fs::perm::Credentials)> {
    let user = crate::gui::session_user()?;
    let credentials = crate::fs::credentials_of(&user)?;
    Some((user, credentials))
}

/// Starts a native application by its file name.
///
/// Returns its PID. `None` when the name is not one of ours -- which is the
/// answer the launcher shows rather than a silence, because a menu entry
/// that does nothing when clicked is indistinguishable from a broken one.
pub fn launch(file_name: &str) -> Option<crate::process::Pid> {
    // Phase 11: the browser entry opens the Web Browser (the Firefox
    // fork, through the Linux subsystem) when the root image carries it;
    // the native browser of before stays for a system without that image.
    if file_name == browser::FILE_NAME
        && let Some(pid) = crate::posix::launch_web_browser()
    {
        crate::serial_println!("[apps] lancement du Web Browser - PID {}", pid);
        return Some(pid);
    }
    let (_, app_id, kind, entry) = native_for(file_name)?;
    // The browser interprets the scripts of pages: its recursion goes as
    // deep as theirs, and it gets a stack to match.
    let pid = if file_name == browser::FILE_NAME {
        crate::scheduler::spawn_with_stack(file_name, *kind, *entry, browser::STACK_SIZE)
    } else {
        crate::scheduler::spawn(file_name, *kind, *entry)
    };
    crate::serial_println!(
        "[apps] lancement de \"{}\" ({}) - PID {}",
        file_name,
        app_id,
        pid
    );
    Some(pid)
}

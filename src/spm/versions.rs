//! 15.3 to 15.5 -- Atomic installation of applications, the active
//! pointer, the launch by the pointer, the deferred cleanup.
//!
//! ## Layout (decisions of 15.1, points 4 and 5)
//!
//! ```text
//!   {root}/{app_id}/
//!       versions/1.2.0/          one directory per installed version
//!       versions/1.3.0.tmp/      a version being laid down
//!       current                  "1.3.0": the active version
//!       previous                 "1.2.0": the one a rollback returns to
//! ```
//!
//! `{root}` is the one spec 4.3 gives the format (`/apps`, `/sysapp`,
//! `/games`, `/ras`) or `/users/{user}/apps` for a personal installation.
//!
//! ## The order of an installation
//!
//! ```text
//!   1  accept       parse, signature, consent, dependencies (manager)
//!   2  lay down     into versions/{v}.tmp, scripts confined to it
//!   3  seal         versions/{v}.tmp renamed versions/{v}
//!   4  switch       current.nouveau written, then put in place of
//!                   `current` in one step (fs::retarget)
//!   5  record       previous, database, notification
//!   6  clean        versions neither active, nor previous, nor running
//! ```
//!
//! A power cut before step 4 leaves the old version active and a `.tmp`
//! (or a sealed but unpointed version) that the boot purge removes; after
//! it, the new one. Never an application without a version.
//!
//! ## Processes keep their version
//!
//! The pointer is read **at launch**: a process receives the absolute
//! path of its own version directory. A version 1 process that opens more
//! of its files after the switch -- Firefox loads its libraries as it goes
//! -- keeps reading version 1. That is why the pointer is a file and not a
//! directory link (ROADMAP, 15.1 point 5).
//!
//! A version is in use while a process launched from it, or any of its
//! descendants, lives (point 6: Firefox starts a dozen). Its deletion
//! waits; `tick` takes it up later.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::binfmt::header::BinaryFormat;
use crate::fs::perm::{Credentials, Mode};
use crate::process::Pid;
use crate::vga::SpinLock;

use super::db::{self, Entry, Scope, Status};
use super::manager::{self, SpmError};

/// Name of the pointer file, of its replacement being written, and of the
/// rollback target.
pub const CURRENT: &str = "current";
const CURRENT_NEXT: &str = "current.nouveau";
pub const PREVIOUS: &str = "previous";
pub const VERSIONS: &str = "versions";
const TEMP_SUFFIX: &str = ".tmp";

/// An installation is laying down a `.tmp`: the cleanup leaves them.
static INSTALLING: AtomicBool = AtomicBool::new(false);

/// Processes launched from a version: (root PID, app root, version).
static RUNNING: SpinLock<Vec<(Pid, String, String)>> = SpinLock::new(Vec::new());

/// App roots with a version waiting for its processes to end.
static PENDING: SpinLock<Vec<String>> = SpinLock::new(Vec::new());

fn kernel() -> Credentials {
    Credentials::kernel()
}

/// The text form of a package version: three numbers, the fourth only
/// when it is not zero (`1.2.0`, `1.2.0.7`).
pub fn version_text(version: &[u16; 4]) -> String {
    if version[3] == 0 {
        alloc::format!("{}.{}.{}", version[0], version[1], version[2])
    } else {
        alloc::format!("{}.{}.{}.{}", version[0], version[1], version[2], version[3])
    }
}

/// A version name read from the disk is trusted only if it is one: digits
/// and dots, short. It becomes a path component.
fn valid_version(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 32
        && text.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && !text.starts_with('.')
        && !text.contains("..")
}

/// The root of an application, from its scope, format and app_id.
pub fn app_root(scope: &Scope, format: BinaryFormat, app_id: &str) -> String {
    alloc::format!("{}/{}", scope.install_root(format), app_id)
}

pub fn version_dir(root: &str, version: &str) -> String {
    alloc::format!("{}/{}/{}", root, VERSIONS, version)
}

fn read_small(path: &str) -> Option<String> {
    let bytes = crate::fs::read_file(path, kernel()).ok()?;
    if bytes.len() > 64 {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    let text = String::from(text.trim());
    valid_version(&text).then_some(text)
}

/// The active version of an application root, if it is versioned.
pub fn active(root: &str) -> Option<String> {
    read_small(&alloc::format!("{}/{}", root, CURRENT))
}

/// The version a rollback would return to.
pub fn previous(root: &str) -> Option<String> {
    read_small(&alloc::format!("{}/{}", root, PREVIOUS))
}

/// Is this application root versioned (phase 15) rather than flat (8)?
pub fn is_versioned(root: &str) -> bool {
    active(root).is_some()
}

/// Deletes a tree, depth first. Returns the entries removed. Bounded by
/// the stack: task stacks have no guard page (DECISIONS, invariants).
pub fn remove_tree(path: &str, now: u64) -> usize {
    let mut removed = 0;
    if crate::scheduler::stack_headroom() < 16 * 1024 {
        return 0;
    }
    if let Ok(entries) = crate::fs::list(path, kernel()) {
        for entry in entries {
            let child = alloc::format!("{}/{}", path, entry.name);
            removed += remove_tree(&child, now);
        }
    }
    if crate::fs::delete(path, kernel(), now).is_ok() {
        removed += 1;
    }
    removed
}

// ============================================================
//  15.3.2 -- The switch
// ============================================================

/// Makes `version` the active one of `root`, in one step of the
/// filesystem. Records the version replaced as the previous one.
pub fn switch(root: &str, version: &str, now: u64) -> Result<Option<String>, SpmError> {
    let old = active(root);
    let next = alloc::format!("{}/{}", root, CURRENT_NEXT);
    let pointer = alloc::format!("{}/{}", root, CURRENT);
    let text = alloc::format!("{}\n", version);

    // The replacement is written whole, aside, and reaches the disk
    // before the entry is switched to it.
    let _ = crate::fs::delete(&next, kernel(), now);
    crate::fs::write_file(&next, text.as_bytes(), kernel(), Mode::system_file(), now)
        .map_err(|_| SpmError::WriteFailed(next.clone()))?;
    let _ = crate::fs::flush_all();
    crate::fs::retarget(&pointer, &next, kernel(), now).map_err(|_| SpmError::WriteFailed(pointer.clone()))?;
    let _ = crate::fs::delete(&next, kernel(), now);

    if let Some(old) = &old
        && old != version
    {
        let previous = alloc::format!("{}/{}", root, PREVIOUS);
        let _ = crate::fs::delete(&previous, kernel(), now);
        let _ = crate::fs::write_file(&previous, alloc::format!("{}\n", old).as_bytes(), kernel(), Mode::system_file(), now);
    }
    let _ = crate::fs::flush_all();
    Ok(old)
}

// ============================================================
//  15.3.1 -- Installation into a version of its own
// ============================================================

/// What a versioned installation produced.
#[derive(Debug, Clone)]
pub struct Installed {
    pub app_id: String,
    pub version: String,
    pub root: String,
    /// The version that was active before, if any.
    pub replaced: Option<String>,
    pub status: Status,
    pub verdict: crate::binfmt::signature::Verdict,
}

/// Installs a package as a new version of its application, and makes it
/// the active one. The previous version stays on the disk -- running
/// processes keep it, a rollback returns to it.
pub fn install(bytes: &[u8], scope: Scope, origin: &str, now: u64) -> Result<Installed, SpmError> {
    let accepted = manager::accept(bytes, &scope)?;
    let header = &accepted.header;
    let root = if accepted.manifest.prefix.is_empty() {
        app_root(&scope, header.format, &header.app_id)
    } else {
        accepted.manifest.prefix.clone()
    };
    let version = version_text(&header.package_version);
    if active(&root).as_deref() == Some(version.as_str()) {
        return Err(SpmError::AlreadyInstalled(alloc::format!("{} {}", header.app_id, version)));
    }

    // A flat installation of phase 8 under the same root is not mixed
    // with versions: it is uninstalled first.
    if db::find(&scope, &header.app_id).is_some() && !is_versioned(&root) {
        let _ = manager::remove(&header.app_id, scope.clone(), now);
    }

    for dir in [root.clone(), alloc::format!("{}/{}", root, VERSIONS)] {
        if crate::fs::stat(&dir, kernel()).is_err() {
            crate::fs::create_directory(&dir, kernel(), Mode::system_dir(), now)
                .map_err(|_| SpmError::WriteFailed(dir.clone()))?;
        }
    }

    INSTALLING.store(true, Ordering::SeqCst);
    let outcome = lay_and_switch(&accepted, bytes, &root, &version, now);
    INSTALLING.store(false, Ordering::SeqCst);
    let (replaced, status) = outcome?;

    let entry = Entry {
        app_id: header.app_id.clone(),
        version: header.package_version,
        format: header.format,
        signature: header.signature,
        update_code: header.update_code,
        prefix: root.clone(),
        origin: String::from(origin),
        installed_at: super::rollback::epoch_now(),
        status,
    };
    db::upsert(&scope, entry, now).map_err(|_| SpmError::WriteFailed(scope.database()))?;
    // The whole root is the package's: removing it removes every version.
    db::record_files(&scope, &header.app_id, core::slice::from_ref(&root), now)
        .map_err(|_| SpmError::WriteFailed(scope.files_dir()))?;

    manager::notify_installed(header, &version_dir(&root, &version));
    crate::serial_println!(
        "[spm] {} {} installe dans {} - version active, precedente {}",
        header.app_id,
        version,
        version_dir(&root, &version),
        replaced.as_deref().unwrap_or("aucune")
    );

    clean(&root, now);
    Ok(Installed {
        app_id: header.app_id.clone(),
        version,
        root,
        replaced,
        status,
        verdict: accepted.verdict,
    })
}

fn lay_and_switch(
    accepted: &manager::Accepted,
    bytes: &[u8],
    root: &str,
    version: &str,
    now: u64,
) -> Result<(Option<String>, Status), SpmError> {
    let temp = alloc::format!("{}{}", version_dir(root, version), TEMP_SUFFIX);
    // A `.tmp` of an earlier, interrupted attempt is not resumed: its
    // content is of unknown completeness.
    remove_tree(&temp, now);
    let (_, status) = manager::lay_down(accepted, bytes, &temp, now)?;
    let _ = crate::fs::flush_all();

    // A sealed version of the same number that is not active (an earlier
    // attempt cut between seal and switch) gives way to this one.
    let sealed = version_dir(root, version);
    remove_tree(&sealed, now);
    crate::fs::rename(&temp, version, kernel(), now).map_err(|_| SpmError::WriteFailed(sealed.clone()))?;
    let _ = crate::fs::flush_all();

    let replaced = switch(root, version, now)?;
    Ok((replaced, status))
}

/// `spm rollback {app}`: the previous version becomes the active one.
pub fn rollback(root: &str, now: u64) -> Result<String, SpmError> {
    let target = previous(root).ok_or_else(|| SpmError::NotFound(alloc::format!("{} : aucune version precedente", root)))?;
    if crate::fs::stat(&version_dir(root, &target), kernel()).is_err() {
        return Err(SpmError::NotFound(version_dir(root, &target)));
    }
    switch(root, &target, now)?;
    crate::serial_println!("[spm] {} revenu a la version {}", root, target);
    Ok(target)
}

// ============================================================
//  15.4 -- Launching through the pointer
// ============================================================

/// The binary of a version directory: the one its manifest names, else
/// the first file carrying a Nova extension.
pub fn binary_of(dir: &str) -> Option<String> {
    if let Ok(manifest) = crate::fs::read_file(&alloc::format!("{}/manifeste.cfg", dir), kernel()) {
        let text = String::from_utf8_lossy(&manifest);
        for line in text.lines() {
            if let Some(name) = line.trim().strip_prefix("binary=").or_else(|| line.trim().strip_prefix("binaire=")) {
                let name = name.trim();
                if !name.is_empty() && !name.contains('/') {
                    return Some(alloc::format!("{}/{}", dir, name));
                }
            }
        }
    }
    let entries = crate::fs::list(dir, kernel()).ok()?;
    entries
        .iter()
        .find(|entry| {
            ["dsa", "ss", "gs", "cus", "ras"]
                .iter()
                .any(|ext| entry.name.ends_with(&alloc::format!(".{}", ext)))
        })
        .map(|entry| alloc::format!("{}/{}", dir, entry.name))
}

/// Starts the binary at `path`: a linked image (spec 3.5) through the
/// Nova C library, any other Nova binary through the loader of phase 3.
pub fn launch_binary(path: &str) -> Result<Pid, String> {
    let data = crate::fs::read_file(path, kernel()).map_err(|_| alloc::format!("illisible : {}", path))?;
    let header = crate::binfmt::parse(&data).map_err(|e| alloc::format!("{} : {:?}", path, e))?;
    if header.flags & crate::binfmt::header::flags::LINKED_IMAGE != 0 {
        let name = path.rsplit('/').next().unwrap_or(path);
        return crate::posix::spawn_native(path, &[name], &[]).map_err(|errno| alloc::format!("{} : errno {}", path, errno));
    }
    match crate::binfmt::loader::load_and_spawn(&data) {
        Ok((image, pid)) => {
            crate::binfmt::loader::keep(pid, image);
            Ok(pid)
        }
        Err(error) => Err(alloc::format!("{} : {}", path, error.message())),
    }
}

/// Launches the active version of the application at `root`; the process
/// is recorded with its version.
pub fn launch(root: &str) -> Result<Pid, String> {
    let version = active(root).ok_or_else(|| alloc::format!("{} : pas de version active", root))?;
    let dir = version_dir(root, &version);
    let binary = binary_of(&dir).ok_or_else(|| alloc::format!("{} : aucun binaire", dir))?;
    let pid = launch_binary(&binary)?;
    note_launch(pid, root, &version);
    crate::serial_println!("[spm] {} {} lance - processus {}", root, version, pid);
    Ok(pid)
}

/// Launches an installed application named as the launcher lists it --
/// an entry of `/apps`, `/sysapp`, `/games` or the session's own apps.
/// A versioned root (phase 15) starts its active version; a flat one
/// (phase 8) the binary it holds. `None` when no such directory exists.
pub fn launch_by_name(name: &str) -> Option<Result<Pid, String>> {
    if name.is_empty() || name.contains('/') {
        return None;
    }
    let mut parents: Vec<String> = ["/apps", "/sysapp", "/games"].iter().map(|s| String::from(*s)).collect();
    if let Some(user) = crate::gui::session_user() {
        parents.push(alloc::format!("/users/{}/apps", user));
    }
    for parent in parents {
        let root = alloc::format!("{}/{}", parent, name);
        if is_versioned(&root) {
            return Some(launch(&root));
        }
        if crate::fs::stat(&root, kernel()).is_ok_and(|node| node.is_directory()) {
            if let Some(binary) = binary_of(&root) {
                return Some(launch_binary(&binary));
            }
        }
    }
    None
}

/// Records that `pid` runs `version` of `root`.
pub fn note_launch(pid: Pid, root: &str, version: &str) {
    RUNNING.lock().push((pid, String::from(root), String::from(version)));
}

/// The version a process (or one of its ancestors) was launched from.
pub fn version_of(pid: Pid) -> Option<(String, String)> {
    let tasks = crate::scheduler::task_list();
    let running = RUNNING.lock().clone();
    let mut at = pid;
    for _ in 0..64 {
        if let Some((_, root, version)) = running.iter().find(|(p, _, _)| *p == at) {
            return Some((root.clone(), version.clone()));
        }
        at = tasks.iter().find(|t| t.pid == at)?.parent;
        if at == 0 {
            return None;
        }
    }
    None
}

/// Does any living process run `version` of `root`? A process counts if it
/// was launched from it or descends from one that was.
pub fn in_use(root: &str, version: &str) -> bool {
    let tasks = crate::scheduler::task_list();
    let roots: Vec<Pid> = RUNNING
        .lock()
        .iter()
        .filter(|(_, r, v)| r == root && v == version)
        .map(|(pid, _, _)| *pid)
        .collect();
    if roots.is_empty() {
        return false;
    }
    tasks.iter().filter(|t| t.state != crate::process::State::Dead).any(|task| {
        let mut at = task.pid;
        for _ in 0..64 {
            if roots.contains(&at) {
                return true;
            }
            match tasks.iter().find(|t| t.pid == at) {
                Some(info) if info.parent != 0 && info.parent != at => at = info.parent,
                _ => return roots.contains(&at),
            }
        }
        false
    })
}

/// Forgets the launches whose whole tree has ended.
fn prune() {
    let snapshot = RUNNING.lock().clone();
    let alive: Vec<(Pid, String, String)> = snapshot
        .into_iter()
        .filter(|(_, root, version)| in_use(root, version))
        .collect();
    *RUNNING.lock() = alive;
}

// ============================================================
//  15.5 -- Cleanup
// ============================================================

/// Removes from `root` what is neither active, nor previous, nor in use,
/// and the `.tmp` of interrupted installations. A version still running
/// is kept and `root` noted for `tick`. Returns the versions removed.
pub fn clean(root: &str, now: u64) -> usize {
    let active = active(root);
    let previous = previous(root);
    let Ok(entries) = crate::fs::list(&alloc::format!("{}/{}", root, VERSIONS), kernel()) else {
        return 0;
    };
    let mut removed = 0;
    let mut deferred = false;
    for entry in entries {
        let name = entry.name.as_str();
        let path = version_dir(root, name);
        if name.ends_with(TEMP_SUFFIX) {
            if !INSTALLING.load(Ordering::SeqCst) {
                remove_tree(&path, now);
                removed += 1;
            }
            continue;
        }
        if Some(name) == active.as_deref() || Some(name) == previous.as_deref() {
            continue;
        }
        if in_use(root, name) {
            deferred = true;
            crate::serial_println!("[spm] {} {} encore en cours d'execution - suppression differee", root, name);
            continue;
        }
        remove_tree(&path, now);
        removed += 1;
        crate::serial_println!("[spm] {} {} supprimee - plus active ni precedente, plus executee", root, name);
    }
    // Pointer files of a cut switch.
    let _ = crate::fs::delete(&alloc::format!("{}/{}", root, CURRENT_NEXT), kernel(), now);

    let mut pending = PENDING.lock();
    pending.retain(|r| r != root);
    if deferred {
        pending.push(String::from(root));
    }
    removed
}

/// The deferred deletions: called periodically by the updater.
pub fn tick(now: u64) -> usize {
    prune();
    let roots = PENDING.lock().clone();
    roots.iter().map(|root| clean(root, now)).sum()
}

/// Versions waiting for their processes.
pub fn pending() -> usize {
    PENDING.lock().len()
}

/// Every versioned application root of the installation roots, all
/// scopes: the global ones, then every account's.
pub fn all_roots() -> Vec<String> {
    let mut parents: Vec<String> = ["/apps", "/sysapp", "/games", "/ras"].iter().map(|s| String::from(*s)).collect();
    for (name, _, _) in crate::fs::account_names() {
        parents.push(alloc::format!("/users/{}/apps", name));
    }
    let mut roots = Vec::new();
    for parent in parents {
        let Ok(entries) = crate::fs::list(&parent, kernel()) else {
            continue;
        };
        for entry in entries {
            let root = alloc::format!("{}/{}", parent, entry.name);
            if is_versioned(&root) {
                roots.push(root);
            }
        }
    }
    roots
}

/// 15.5.2 -- The purge at boot: before any application starts, nothing
/// can be in use, and everything neither active nor previous goes.
pub fn purge_at_boot(now: u64) -> (usize, usize) {
    let roots = all_roots();
    let removed = roots.iter().map(|root| clean(root, now)).sum();
    (roots.len(), removed)
}

// ============================================================
//  15.2-15.3 -- From the registry to the active version
// ============================================================

/// What `update_from_registry` did.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// The active version is already the channel's.
    UpToDate(String),
    Installed(Installed),
}

/// Where a download in progress is kept, resumable after a cut: under the
/// application's root, out of `versions/` (it is not a version).
fn download_path(root: &str, version: &str) -> String {
    alloc::format!("{}/telechargement-{}.spm", root, version)
}

/// Checks the registries for `app_id` and installs the version of its
/// channel when it differs from the active one: download (resumed),
/// checksum, then the atomic installation. `allow_downgrade`: a channel
/// changed to an older one moves back only on request (15.1, point 7).
pub fn update_from_registry(
    app_id: &str,
    scope: Scope,
    allow_downgrade: bool,
    now: u64,
) -> Result<Outcome, SpmError> {
    use super::registry::RegistryError;
    let (registry, _manifest, channel) = super::registry::resolve(app_id).map_err(|error| match error {
        RegistryError::NotFound | RegistryError::NoChannel(_) => SpmError::NotFound(alloc::format!("{} : {}", app_id, error.message())),
        other => SpmError::Registry(alloc::format!("{} : {}", app_id, other.message())),
    })?;

    let installed = db::find(&scope, app_id);
    if let Some(entry) = &installed {
        let current = active(&entry.prefix);
        if current.as_deref() == Some(version_text(&channel.numbers).as_str()) || entry.version == channel.numbers {
            return Ok(Outcome::UpToDate(channel.version.clone()));
        }
        if channel.numbers < entry.version && !allow_downgrade {
            return Ok(Outcome::UpToDate(version_string_of(&entry.version)));
        }
    }

    // The root is known only once the package is read; until then the
    // download goes to the system's spool.
    let spool = alloc::format!("{}/{}-{}.spm", super::update::UPDATES_DIR, app_id, channel.version);
    let target = match &installed {
        Some(entry) if is_versioned(&entry.prefix) => download_path(&entry.prefix, &channel.version),
        _ => spool,
    };
    crate::serial_println!(
        "[spm] {} {} (canal {}) depuis le registre \"{}\" - {}",
        app_id,
        channel.version,
        channel.name,
        registry.name,
        registry.resolve(&channel.url)
    );
    super::registry::download(&registry, &channel, &target, now)
        .map_err(|error| SpmError::Registry(alloc::format!("{} : {}", app_id, error.message())))?;

    let bytes = crate::fs::read_file(&target, kernel()).map_err(|_| SpmError::NotFound(target.clone()))?;
    let result = install(&bytes, scope, &registry.name, now);
    // Installed or refused, the download has served: a refused package
    // must leave nothing (15.7.2).
    let _ = crate::fs::delete(&target, kernel(), now);
    result.map(Outcome::Installed)
}

fn version_string_of(version: &[u16; 4]) -> String {
    version_text(version)
}

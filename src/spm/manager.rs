//! 8.2 -- Package manager (spec 10.3).
//!
//! ## The commands are parsed as the spec writes them
//!
//! 10.3 does not describe an API, it gives seven command lines. Parsing
//! them literally rather than exposing seven functions is not an
//! affectation: it is what makes the transcription **checkable**. The
//! self-test puts the seven strings of the table through and observes
//! what they produce -- a discrepancy between spec and code then shows in
//! the test, not in a re-reading.
//!
//! The functions exist too, and they are the ones the SDK will call:
//! `execute` is only a parser placed in front.
//!
//! ## The order of the operations of an installation
//!
//! ```text
//!   1  read the package       local file or repository
//!   2  parse                  format, sections, resource table
//!   3  verify                 integrity, signature, 3.4 combination
//!   4  qualify                warning level 10.4
//!   5  consent                admin prompt, or blocking red refusal
//!   6  resolve                dependencies declared by the manifest
//!   7  pre script             cancels everything if it fails
//!   8  write                  binary, then resources
//!   9  post script            does not cancel, marks "partial"
//!  10  register               database, file list, SYSTEM store
//!  11  notify                 according to the 10.2 code
//! ```
//!
//! Steps 3 to 6 are all possible refusals, and all happen **before** the
//! first write. That is the invariant that counts: a refused installation
//! leaves nothing behind it. An installation failing after step 8, on the
//! other hand, leaves files -- and that is why the file list is kept as
//! it goes and not at the end.
//!
//! ## What "remove" cleans, and what it does not touch
//!
//! The files the package laid down, in the reverse order of their
//! creation -- a directory is only deleted once empty. **Not** the data
//! the application produced afterwards: it does not belong to the
//! package, and 10.3 says "uninstalls + cleans", not "erases everything
//! carrying that name".

use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::header::BinaryFormat;
use crate::binfmt::signature::Verdict;
use crate::fs::perm::{Credentials, Mode, Role};

use super::db::{self, Entry, Scope, Status};
use super::package::{self, PackageHeader, UpdateCode, WarningLevel};
use super::repo;
use super::script;

/// Failure reason of an operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpmError {
    /// The file or the package cannot be found.
    NotFound(String),
    /// The package is malformed.
    Malformed(package::PackageError),
    /// The signature check failed.
    Rejected(Verdict),
    /// The user refused, or did not answer.
    ConsentDenied(WarningLevel),
    /// The current role does not allow this installation.
    AdminRequired,
    /// A declared dependency is not installed.
    MissingDependency(String),
    /// The package is already installed in this scope.
    AlreadyInstalled(String),
    /// The pre-installation script failed.
    PreScriptFailed(script::Failure),
    /// Writing impossible.
    WriteFailed(String),
    /// Repository error.
    Repo(repo::RepoError),
    /// Command line not understood.
    BadCommand,
    /// The filesystem is not mounted.
    NotReady,
}

impl SpmError {
    pub fn message(&self) -> String {
        match self {
            SpmError::NotFound(what) => alloc::format!("introuvable : {}", what),
            SpmError::Malformed(error) => alloc::format!("paquet invalide : {}", error.message()),
            SpmError::Rejected(verdict) => {
                alloc::format!("signature refusee : {}", verdict.as_str())
            }
            SpmError::ConsentDenied(level) => {
                alloc::format!("installation refusee : {} - {}", level.as_str(), level.action())
            }
            SpmError::AdminRequired => String::from("role administrateur requis (spec 10.4)"),
            SpmError::MissingDependency(app) => alloc::format!("dependance absente : {}", app),
            SpmError::AlreadyInstalled(app) => alloc::format!("deja installe : {}", app),
            SpmError::PreScriptFailed(failure) => alloc::format!(
                "script pre-installation ligne {} ({}) : {}",
                failure.line,
                failure.verb,
                failure.error.message()
            ),
            SpmError::WriteFailed(path) => alloc::format!("ecriture impossible : {}", path),
            SpmError::Repo(error) => alloc::format!("depot : {}", error.message()),
            SpmError::BadCommand => String::from("commande spm non reconnue (spec 10.3)"),
            SpmError::NotReady => String::from("systeme de fichiers non monte"),
        }
    }
}

/// What a successful installation produced.
#[derive(Debug, Clone)]
pub struct Installed {
    pub app_id: String,
    pub version: [u16; 4],
    pub prefix: String,
    pub scope: Scope,
    pub warning: WarningLevel,
    pub verdict: Verdict,
    pub files: Vec<String>,
    pub status: Status,
}

// ============================================================
//  Identity of the caller
// ============================================================

/// Name of the current account, or `None` for a system context.
pub fn current_user() -> Option<String> {
    let who = crate::scheduler::current_credentials();
    match crate::fs::account_of(who.uid) {
        Some((name, _, _)) if name != "root" => Some(name),
        _ => None,
    }
}

/// Does the current role allow installing a system component?
///
/// `Root` passes because it is the context of the kernel itself --
/// refusing it would prevent installing the packages shipped with the OS.
/// `Service` too: the system `.dsa` install on behalf of the system.
pub fn is_admin() -> bool {
    matches!(
        crate::scheduler::current_credentials().role,
        Role::Root | Role::Admin | Role::Service
    )
}

// ============================================================
//  10.4 -- Consent
// ============================================================

/// Applies the line of 10.4 matching the package.
///
/// Three outcomes: nothing to do, a warning displayed, a question asked.
/// The question goes through `prompt::ask`, whose refusal by default on
/// expiry is exactly the policy wanted here: an installation nobody
/// confirmed must not happen.
fn require_consent(header: &PackageHeader, level: WarningLevel) -> Result<(), SpmError> {
    if !level.warns() {
        crate::serial_println!(
            "[spm] {} - {} : installation sans avertissement",
            header.app_id,
            level.as_str()
        );
        return Ok(());
    }

    if level.requires_admin() && !is_admin() {
        return Err(SpmError::AdminRequired);
    }

    if !level.requires_consent() {
        // Yellow warning: displayed, non-blocking (10.4). `kprintln!`
        // already echoes on the serial console -- adding a
        // `serial_println!` would write the line twice.
        crate::vga::set_color(crate::vga::Color::Yellow, crate::vga::Color::Black);
        crate::kprintln!(
            "[spm] {} : {} - {}",
            header.app_id,
            level.as_str(),
            level.action()
        );
        crate::vga::set_color(crate::vga::Color::LightGray, crate::vga::Color::Black);
        return Ok(());
    }

    if level == WarningLevel::Dangerous {
        crate::vga::set_color(crate::vga::Color::White, crate::vga::Color::Red);
        crate::kprintln!(
            " {} : {} - {} ",
            header.app_id,
            level.as_str(),
            level.action()
        );
        crate::vga::set_color(crate::vga::Color::LightGray, crate::vga::Color::Black);
    }

    let question = alloc::format!(
        "Installer \"{}\" ({}) ? {}",
        header.display_name,
        header.format.extension(),
        level.action()
    );
    let decision = crate::prompt::ask(
        &header.app_id,
        &header.display_name,
        "spm.install",
        &question,
    );

    if decision.allowed() {
        Ok(())
    } else {
        Err(SpmError::ConsentDenied(level))
    }
}

// ============================================================
//  8.2.1 -- Installation
// ============================================================

/// Effective installation prefix.
///
/// The manifest may impose it, which 15.3 hints at for the applications
/// shipping a tree. Failing that, it follows from the scope and the
/// format, as `tree::create_sandbox` already does for a loaded binary:
/// two ways of filing the same thing would be one too many.
fn install_prefix(
    header: &PackageHeader,
    manifest: &package::Manifest,
    scope: &Scope,
) -> String {
    if !manifest.prefix.is_empty() {
        return manifest.prefix.clone();
    }
    alloc::format!("{}/{}", scope.install_root(header.format), header.app_id)
}

/// Name of the binary file laid down in the prefix.
fn binary_name(header: &PackageHeader, manifest: &package::Manifest) -> String {
    if !manifest.binary.is_empty() {
        return manifest.binary.clone();
    }
    alloc::format!("{}.{}", header.app_id, header.format.extension())
}

/// Installs a package already in memory.
///
/// `origin` names the provenance -- a repository, or `local`. It is
/// recorded in the database because `spm update` must know where to go
/// looking for what comes next.
pub fn install_bytes(
    bytes: &[u8],
    scope: Scope,
    origin: &str,
    now: u64,
) -> Result<Installed, SpmError> {
    if !crate::fs::is_ready() {
        return Err(SpmError::NotReady);
    }

    // --- 2: parsing ---
    let header = package::parse(bytes).map_err(SpmError::Malformed)?;
    let manifest = package::manifest(&header, bytes);

    // --- 3: verification ---
    let verdict = package::verify(&header, bytes);
    package::dump(&header, bytes, verdict);
    if !verdict.accepted() {
        return Err(SpmError::Rejected(verdict));
    }

    // --- 4 and 5: qualification and consent ---
    let level = WarningLevel::of(header.signature, header.format);
    require_consent(&header, level)?;

    // --- 6: dependencies ---
    //
    // Looked for in the scope requested **and** in the global one: a
    // personal application may depend on a library installed for
    // everybody, the reverse would make no sense.
    for dependency in &manifest.depends {
        let found = db::find(&scope, dependency).is_some()
            || db::find(&Scope::Global, dependency).is_some();
        if !found {
            return Err(SpmError::MissingDependency(dependency.clone()));
        }
    }

    if db::find(&scope, &header.app_id).is_some() && !header.has_flag(package::flags::UPGRADE) {
        return Err(SpmError::AlreadyInstalled(header.app_id.clone()));
    }

    let prefix = install_prefix(&header, &manifest, &scope);
    let who = Credentials::kernel();
    let mut files: Vec<String> = Vec::new();

    // The directory of the package is created before the pre-installation
    // script: that one is confined to the prefix, and a non-existent
    // prefix would make its first instruction fail for a reason unrelated
    // to what it asks.
    if crate::fs::stat(&prefix, who).is_err() {
        crate::fs::create_directory(&prefix, who, Mode::system_dir(), now)
            .map_err(|_| SpmError::WriteFailed(prefix.clone()))?;
        files.push(prefix.clone());
    }

    let roots = alloc::vec![prefix.clone()];

    // --- 7: pre-installation script ---
    let pre = core::str::from_utf8(package::pre_script(&header, bytes)).unwrap_or("");
    if !pre.trim().is_empty() {
        let outcome = script::execute(pre, &roots, now);
        files.extend(outcome.created.iter().cloned());
        if let Some(failure) = outcome.failures.first() {
            // Nothing useful is laid down yet: what the script created is
            // undone, and the installation did not happen.
            cleanup(&files, now);
            return Err(SpmError::PreScriptFailed(failure.clone()));
        }
    }

    // --- 8: binary and resources ---
    let binary = package::binary(&header, bytes);
    if !binary.is_empty() {
        let path = alloc::format!("{}/{}", prefix, binary_name(&header, &manifest));
        let mode = if header.format == BinaryFormat::Ras {
            Mode::private_file()
        } else {
            Mode::system_binary()
        };
        crate::fs::write_file(&path, binary, who, mode, now)
            .map_err(|_| SpmError::WriteFailed(path.clone()))?;
        files.push(path);
    }

    if let Some(resources) = package::read_resources(&header, bytes) {
        for resource in &resources {
            let path = alloc::format!("{}/{}", prefix, resource.name);
            let content = package::resource_bytes(&header, bytes, resource);
            crate::fs::write_file(&path, content, who, Mode::system_file(), now)
                .map_err(|_| SpmError::WriteFailed(path.clone()))?;
            files.push(path);
        }
    }

    // The manifest is kept beside the binary: it is what anyone wanting
    // to know what a directory holds without consulting the database will
    // read back, and it is the only copy surviving the loss of that one.
    let manifest_path = alloc::format!("{}/manifeste.cfg", prefix);
    if crate::fs::write_file(
        &manifest_path,
        package::manifest_bytes(&header, bytes),
        who,
        Mode::system_file(),
        now,
    )
    .is_ok()
    {
        files.push(manifest_path);
    }

    // --- 9: post-installation script ---
    //
    // Its failure does not cancel: the files are laid down and working.
    // The package is marked "partial", which `spm update` will read back.
    let mut status = Status::Installed;
    let post = core::str::from_utf8(package::post_script(&header, bytes)).unwrap_or("");
    if !post.trim().is_empty() {
        let outcome = script::execute(post, &roots, now);
        files.extend(outcome.created.iter().cloned());
        if let Some(failure) = outcome.failures.first() {
            status = Status::Partial;
            crate::serial_println!(
                "[spm] script post-installation ligne {} ({}) : {} - paquet marque partiel",
                failure.line,
                failure.verb,
                failure.error.message()
            );
        }
    }

    // --- 10: registration ---
    //
    // A verified SYSTEM package sees its digest enter the store: without
    // that, the binary it has just laid down would be refused at load,
    // even though the package carrying it was recognised. It is the same
    // gesture `install_native_modules` makes for the `.mod`.
    if verdict == Verdict::System {
        let digest = crate::binfmt::sha256::digest(binary);
        crate::binfmt::signature::register_system_digest(digest);
    }

    let entry = Entry {
        app_id: header.app_id.clone(),
        version: header.package_version,
        format: header.format,
        signature: header.signature,
        update_code: header.update_code,
        prefix: prefix.clone(),
        origin: String::from(origin),
        installed_at: super::rollback::epoch_now(),
        status,
    };
    db::upsert(&scope, entry, now).map_err(|_| SpmError::WriteFailed(scope.database()))?;
    db::record_files(&scope, &header.app_id, &files, now)
        .map_err(|_| SpmError::WriteFailed(scope.files_dir()))?;

    // --- 11: notification (10.2) ---
    notify_installed(&header, &prefix);

    crate::serial_println!(
        "[spm] {} v{} installe dans {} - portee {}, {} fichier(s), {}",
        header.app_id,
        header.version_string(),
        prefix,
        scope.as_str(),
        files.len(),
        status.as_str()
    );

    Ok(Installed {
        app_id: header.app_id.clone(),
        version: header.package_version,
        prefix,
        scope,
        warning: level,
        verdict,
        files,
        status,
    })
}

/// Undoes what an aborted installation had laid down.
///
/// Backwards: a directory is only deleted once its content is gone.
fn cleanup(files: &[String], now: u64) {
    let who = Credentials::kernel();
    for path in files.iter().rev() {
        let _ = crate::fs::delete(path, who, now);
    }
}

/// 8.2.1 -- `spm install ./monApp.spm`: from a local file.
pub fn install_file(path: &str, scope: Scope, now: u64) -> Result<Installed, SpmError> {
    let bytes = crate::fs::read_file(path, Credentials::kernel())
        .map_err(|_| SpmError::NotFound(String::from(path)))?;
    install_bytes(&bytes, scope, "local", now)
}

/// 8.2.1 -- `spm install firefox`: from a repository.
pub fn install_from_repo(app_id: &str, scope: Scope, now: u64) -> Result<Installed, SpmError> {
    let (repo, entry) =
        repo::resolve(app_id).ok_or_else(|| SpmError::NotFound(String::from(app_id)))?;
    let bytes = repo::fetch_package(&repo, &entry).map_err(SpmError::Repo)?;
    install_bytes(&bytes, scope, &repo.name, now)
}

/// Notification at the end of an installation, per the 10.2 code.
fn notify_installed(header: &PackageHeader, prefix: &str) {
    use crate::gui::notify::Level;

    let code = header.update_code;
    if !code.notifies() {
        crate::serial_println!(
            "[spm] code {} ({}) - installation sans notification",
            code.code(),
            code.as_str()
        );
        return;
    }

    let level = match code {
        UpdateCode::Mandatory => Level::Prioritaire,
        UpdateCode::Standard => Level::Normale,
        UpdateCode::Optional => Level::Basse,
        UpdateCode::Silent => return,
    };

    let title = alloc::format!("{} installe", header.display_name);
    let body = alloc::format!("Version {} dans {}", header.version_string(), prefix);

    // PRIORITY demands a button (9.1) and `post_notification` lays none:
    // the notification would be refused. Code -1 of 10.2 asks for a
    // "button that cannot be dismissed", which only the application path
    // `OP_NOTIFY_POST` can build -- see the recorded limit.
    let effective = if level == Level::Prioritaire {
        Level::Haute
    } else {
        level
    };

    let posted = crate::gui::with(|comp| {
        crate::gui::compositor::post_notification(comp, effective, &title, &body)
    });
    if posted.is_none() {
        crate::serial_println!("[spm] pas de compositeur - notification non affichee");
    }
}

// ============================================================
//  8.2.2 -- Uninstallation
// ============================================================

/// What an uninstallation undid.
#[derive(Debug, Clone)]
pub struct Removed {
    pub app_id: String,
    pub removed_files: usize,
    pub kept_files: usize,
}

/// 8.2.2 -- `spm remove firefox`.
pub fn remove(app_id: &str, scope: Scope, now: u64) -> Result<Removed, SpmError> {
    if !crate::fs::is_ready() {
        return Err(SpmError::NotReady);
    }
    let entry =
        db::find(&scope, app_id).ok_or_else(|| SpmError::NotFound(String::from(app_id)))?;

    let who = Credentials::kernel();
    let files = db::files_of(&scope, app_id);
    let mut removed = 0;
    let mut kept = 0;

    // Backwards: the files first, the directories next, because a
    // directory is only deleted once empty. A file the user added
    // afterwards therefore makes the deletion of the directory holding it
    // fail -- and that is the wanted behaviour: better to leave a
    // directory than to carry off data.
    for path in files.iter().rev() {
        if crate::fs::delete(path, who, now).is_ok() {
            removed += 1;
        } else {
            kept += 1;
        }
    }

    // The prefix itself, if the package had not recorded it.
    if !files.contains(&entry.prefix) && crate::fs::delete(&entry.prefix, who, now).is_ok() {
        removed += 1;
    }

    db::remove(&scope, app_id, now).map_err(|_| SpmError::WriteFailed(scope.database()))?;
    db::forget_files(&scope, app_id, now);

    crate::serial_println!(
        "[spm] {} desinstalle - {} fichier(s) supprime(s), {} conserve(s)",
        app_id,
        removed,
        kept
    );

    Ok(Removed {
        app_id: String::from(app_id),
        removed_files: removed,
        kept_files: kept,
    })
}

// ============================================================
//  8.2.3 -- Update and search
// ============================================================

/// An update offered for an installed package.
#[derive(Debug, Clone)]
pub struct Upgrade {
    pub app_id: String,
    pub installed: [u16; 4],
    pub available: [u16; 4],
    pub repo: String,
    pub code: UpdateCode,
}

/// 8.2.3 -- `spm update`: what the repositories offer that is newer.
///
/// Lays nothing down: the spec says "checks all updates". Applying
/// without asking would contradict 10.2, three of whose four codes demand
/// a notification and two a confirmation.
pub fn check_updates(scope: &Scope) -> Vec<Upgrade> {
    let mut upgrades = Vec::new();

    for entry in db::load(scope) {
        let Some((repo, candidate)) = repo::resolve(&entry.app_id) else {
            continue;
        };
        // A package marked partial is offered again at equal version: its
        // post-installation script did not complete, and reinstalling it
        // is the way to take it up again.
        let newer = candidate.version > entry.version
            || (candidate.version == entry.version && entry.status == Status::Partial);
        if !newer {
            continue;
        }
        upgrades.push(Upgrade {
            app_id: entry.app_id.clone(),
            installed: entry.version,
            available: candidate.version,
            repo: repo.name.clone(),
            code: candidate.update_code,
        });
    }

    upgrades
}

/// Applies an offered update.
pub fn apply_upgrade(upgrade: &Upgrade, scope: Scope, now: u64) -> Result<Installed, SpmError> {
    let (repo, entry) = repo::resolve(&upgrade.app_id)
        .ok_or_else(|| SpmError::NotFound(upgrade.app_id.clone()))?;
    let bytes = repo::fetch_package(&repo, &entry).map_err(SpmError::Repo)?;

    // The package is reinstalled over the top: the old one is uninstalled
    // first, failing which its orphaned files would stay in the list and a
    // later uninstallation would miss them.
    let _ = remove(&upgrade.app_id, scope.clone(), now);
    install_bytes(&bytes, scope, &repo.name, now)
}

/// A search result.
#[derive(Debug, Clone)]
pub struct Found {
    pub repo: String,
    pub app_id: String,
    pub version: [u16; 4],
    pub format: BinaryFormat,
    pub installed: bool,
}

/// 8.2.3 -- `spm search firefox`.
pub fn search(pattern: &str, scope: &Scope) -> Vec<Found> {
    repo::search(pattern)
        .into_iter()
        .map(|(repo, entry)| Found {
            repo,
            installed: db::find(scope, &entry.app_id).is_some(),
            app_id: entry.app_id,
            version: entry.version,
            format: entry.format,
        })
        .collect()
}

/// Packages installed, all scopes together for one account.
pub fn installed(user: Option<&str>) -> Vec<(Scope, Entry)> {
    let mut list = Vec::new();
    for scope in db::scopes_for(user) {
        for entry in db::load(&scope) {
            list.push((scope.clone(), entry));
        }
    }
    list
}

// ============================================================
//  10.3 -- Parsing of the command lines
// ============================================================

/// What a command produced.
#[derive(Debug, Clone)]
pub enum Output {
    Installed(Installed),
    Removed(Removed),
    Updates(Vec<Upgrade>),
    OsUpdates(usize),
    Results(Vec<Found>),
    RepoAdded(String),
    RepoExists(String),
}

/// Parses and runs a line of 10.3.
///
/// The seven forms of the spec, literally:
///
/// ```text
///   spm install firefox
///   spm install firefox --user
///   spm install ./monApp.spm
///   spm remove firefox
///   spm update            (+ --os for the manual check of 11.2)
///   spm search firefox
///   spm repo add <url>
/// ```
pub fn execute(line: &str, now: u64) -> Result<Output, SpmError> {
    let mut words = line.split_whitespace();
    if words.next() != Some("spm") {
        return Err(SpmError::BadCommand);
    }
    let verb = words.next().ok_or(SpmError::BadCommand)?;
    let arguments: Vec<&str> = words.collect();

    let user_scope = arguments.iter().any(|word| *word == "--user");
    let positional: Vec<&str> = arguments
        .iter()
        .copied()
        .filter(|word| !word.starts_with("--"))
        .collect();

    // `--user` with no logged-in account has no scope to point at:
    // refusing it is worth more than silently falling back on the global
    // one, which would install for everybody what was asked for one
    // account.
    let scope = if user_scope {
        match current_user() {
            Some(name) => Scope::User(name),
            None => return Err(SpmError::BadCommand),
        }
    } else {
        Scope::Global
    };

    match verb {
        "install" => {
            let target = positional.first().ok_or(SpmError::BadCommand)?;
            // A path, or a package name. The distinction is made on the
            // extension and not on the first character: `./` is only a
            // writing convention, `.spm` is what counts.
            if target.ends_with(".spm") {
                let path = normalize_local(target);
                install_file(&path, scope, now).map(Output::Installed)
            } else {
                install_from_repo(target, scope, now).map(Output::Installed)
            }
        }
        "remove" => {
            let target = positional.first().ok_or(SpmError::BadCommand)?;
            remove(target, scope, now).map(Output::Removed)
        }
        "update" => {
            if arguments.iter().any(|word| *word == "--os") {
                // 11.2 -- manual check of the system updates.
                let found = super::update::check_now(now);
                for update in &found {
                    super::update::notify(update);
                    super::rollback::stage_update(update, now);
                }
                Ok(Output::OsUpdates(found.len()))
            } else {
                Ok(Output::Updates(check_updates(&scope)))
            }
        }
        "search" => {
            let pattern = positional.first().ok_or(SpmError::BadCommand)?;
            Ok(Output::Results(search(pattern, &scope)))
        }
        "repo" => {
            if positional.first() != Some(&"add") {
                return Err(SpmError::BadCommand);
            }
            let url = positional.get(1).ok_or(SpmError::BadCommand)?;
            // The name of the repository is the last segment of the URL:
            // the spec asks only for a URL, and a repository with no name
            // would be impossible to remove.
            let name = url
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .filter(|segment| !segment.is_empty())
                .unwrap_or("depot");
            let anchor = positional.get(2).and_then(|text| repo::unhex(text));
            match repo::add(name, url, anchor, now) {
                Ok(true) => Ok(Output::RepoAdded(String::from(name))),
                Ok(false) => Ok(Output::RepoExists(String::from(name))),
                Err(_) => Err(SpmError::WriteFailed(String::from(repo::CONFIG_PATH))),
            }
        }
        _ => Err(SpmError::BadCommand),
    }
}

/// `./monApp.spm` names the current directory, which does not exist here:
/// there is no notion of working directory. The path is resolved from the
/// home of the user, or from `/apps/` failing that.
fn normalize_local(target: &str) -> String {
    if target.starts_with('/') || target.contains(':') {
        return String::from(target);
    }
    let name = target.trim_start_matches("./");
    match current_user() {
        Some(user) => alloc::format!("/users/{}/home/downloads/{}", user, name),
        None => alloc::format!("/sys/updates/{}", name),
    }
}

pub fn dump_state() {
    let user = current_user();
    let list = installed(user.as_deref());
    if list.is_empty() {
        crate::serial_println!("[spm] aucun paquet installe");
        return;
    }
    for (scope, entry) in list {
        crate::serial_println!(
            "[spm] {} v{} - {} ({}), depot \"{}\", {}",
            entry.app_id,
            entry.version_string(),
            entry.prefix,
            scope.as_str(),
            entry.origin,
            entry.status.as_str()
        );
    }
}

//! Phase 8 -- Package manager and updates (spec 10 and 11).
//!
//! ## What this phase adds to the system
//!
//! Until now, everything Nova OS executes has been laid down by the
//! kernel itself: the GUI modules by `install_native_modules`, the sample
//! binaries by `binfmt::sample`, the tree by `fs::tree`. Phase 8 opens
//! the first path by which **outside** content enters the system, and
//! that makes it the most sensitive point written so far.
//!
//! Three barriers, in this order, and none optional:
//!
//! 1. **The form** -- `package::parse` refuses a file whose sections
//!    overflow or overlap, before any of them is read;
//! 2. **Integrity and origin** -- `package::verify` takes back the
//!    verdicts of phase 3, including the table of format/signature
//!    combinations of 3.4: a package cannot deliver a self-signed `.dsa`
//!    on the pretext that its wrapper is in order;
//! 3. **Consent** -- the 10.4 table, two of whose five lines demand a
//!    human answer.
//!
//! Only after all three does the first write happen. A refused
//! installation leaves nothing behind it, and that is the invariant the
//! self-test checks by counting the files before and after.
//!
//! ## Layout
//!
//! | Module | Covers |
//! |---|---|
//! | `package` | 8.1 -- `.spm` format, manifest, 10.2 codes, 10.4 warnings |
//! | `script`  | 8.1.3 -- pre/post scripts, confined to the prefix |
//! | `db`      | database of installed packages, file list per package |
//! | `manager` | 8.2 -- the seven commands of 10.3 |
//! | `repo`    | 8.3 -- repository index, transport, checking on receipt |
//! | `fetch`   | 15.0.4 -- `file:`, `http://`, `https://`, to memory or disk |
//! | `registry`| 15.2 -- signed JSON manifests, channels, checked downloads |
//! | `versions`| 15.3-15.5 -- versions apart, atomic pointer, launch, cleanup |
//! | `service` | 15.6 -- `spm.dsa`: requests of the applications, cleanup |
//! | `update`  | 8.4 -- 11.1 types, `updater.dsa` service |
//! | `rollback`| 8.5 -- slots, boot validation, snapshots |
//!
//! ## What phase 8 does not do
//!
//! The manager has no interface: `spm` is a function, not a binary, and
//! there is no terminal to type its commands in. 10.3 describes a command
//! line, and it is literally the string `manager::execute` parses -- but
//! nobody types it yet. The terminal will come with the SDK (9.4), and
//! `execute` will not change by a line: it already expects a string.

pub mod db;
pub mod fetch;
pub mod manager;
pub mod package;
pub mod registry;
pub mod repo;
pub mod rollback;
pub mod script;
pub mod selftest;
pub mod selftest_atomic;
pub mod service;
pub mod update;
pub mod versions;

use crate::fs::perm::{Credentials, Mode};

/// Local repository shipped with the system.
///
/// A `file:` repository laid on the boot volume. It is not a test
/// artifice: it is the form a repository takes on an installation medium,
/// and it is also what makes the complete chain -- index, fetch,
/// verification, laying down -- exercisable with no network.
pub const LOCAL_REPO_DIR: &str = "/sys/tools/spm/depot";

/// Name of the local repository.
pub const LOCAL_REPO_NAME: &str = "systeme";

/// Lays down the tree of the manager.
///
/// Idempotent: a second boot must recreate nothing and overwrite nothing,
/// failing which a repository added by the user would disappear on every
/// restart.
pub fn install_layout(now: u64) -> usize {
    if !crate::fs::is_ready() {
        return 0;
    }
    let who = Credentials::kernel();
    let mut created = 0;

    for path in [
        db::SPM_DIR,
        repo::CACHE_DIR,
        LOCAL_REPO_DIR,
        update::UPDATES_DIR,
        rollback::ROLLBACK_DIR,
    ] {
        if crate::fs::stat(path, who).is_err()
            && crate::fs::create_directory(path, who, Mode::system_dir(), now).is_ok()
        {
            created += 1;
        }
    }

    // The service configuration is written along the way: `config`
    // creates it if missing, and creating it here spares the first lap of
    // the service loop from writing into `/sys/config/`.
    let _ = update::config(now);

    // The update state exists from the first boot: without it,
    // `rollback::state()` would return a default on every call and the
    // current version would be recorded nowhere.
    if crate::fs::stat(rollback::STATE_PATH, who).is_err() {
        let _ = rollback::save_state(&rollback::State::default(), now);
        created += 1;
    }

    created
}

/// Declares the local repository, if it is not already.
///
/// The anchor is the digest of the repository name. It is a secret
/// everybody can recompute, so it is not one -- the anchor attests here
/// that an index has not been modified *by accident*, not that it comes
/// from somebody. The distinction is the same as in phase 3, and it will
/// disappear the day an asymmetric signature exists.
pub fn register_local_repo(now: u64) -> bool {
    if !crate::fs::is_ready() {
        return false;
    }
    let anchor = crate::binfmt::sha256::digest(LOCAL_REPO_NAME.as_bytes());
    let url = alloc::format!("file:{}", LOCAL_REPO_DIR);
    matches!(repo::add(LOCAL_REPO_NAME, &url, Some(anchor), now), Ok(true))
}

/// Anchor of the local repository, for whoever wants to publish its index.
pub fn local_anchor() -> [u8; 32] {
    crate::binfmt::sha256::digest(LOCAL_REPO_NAME.as_bytes())
}

/// Starts phase 8.
///
/// Called by `init.dsa` after the core services: the manager writes into
/// `/sys/`, which assumes the filesystem is mounted, and the update
/// service talks to the network, which does not exist before the drivers.
pub fn start(now: u64) {
    if !crate::fs::is_ready() {
        crate::serial_println!("[spm] systeme de fichiers absent - phase 8 inactive");
        return;
    }

    let created = install_layout(now);
    let repo_added = register_local_repo(now);

    crate::serial_println!(
        "[spm] arborescence prete - {} dossier(s) cree(s), depot local {}",
        created,
        if repo_added {
            "declare"
        } else {
            "deja declare"
        }
    );

    // 8.5.2 -- the validation of a boot must start before anything else
    // can fail for a reason unrelated to the update.
    if rollback::on_boot(now) {
        crate::serial_println!("[rollback] validation de demarrage engagee (spec 11.3)");
    }

    crate::scheduler::spawn(
        "updater.dsa",
        crate::process::BinaryType::Dsa,
        update::task,
    );

    // 15.5-15.6 -- the one writer of the applications: requests of the
    // applications, purge at boot, deferred deletions.
    crate::scheduler::spawn("spm.dsa", crate::process::BinaryType::Dsa, service::task);
}

pub fn dump_state() {
    repo::dump_state();
    manager::dump_state();
    update::dump_state();
    rollback::dump_state();
}

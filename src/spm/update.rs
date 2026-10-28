//! 8.4 -- OS updates (spec 11.1 and 11.2).
//!
//! ## Two tables, the same numbers, two meanings
//!
//! 10.2 numbers **notification behaviours**: -1 forced, 0 standard,
//! 1 optional, 2 silent. 11.1 numbers **update types**: 0 PATCH,
//! 1 UPDATE, 2 MAJOR, -1 CRITICAL. The numbers coincide, the meanings do
//! not -- code 2 means "no notification" on one side and "new partition,
//! mandatory reboot" on the other.
//!
//! Confusing them would silently apply the heaviest update of the system.
//! The two tables are therefore two distinct types, and the translation
//! from one to the other is written once, here, in `notification_code`.
//!
//! ## What "silent download" means (11.2)
//!
//! "Silent download in the background before notification": the file is
//! fetched **before** anybody is told, and dropped in `/sys/updates/`.
//! The notification that follows therefore announces an update already
//! available, not an update to download. The difference shows when the
//! user accepts: they do not wait.
//!
//! Silent does not mean invisible: the serial console logs the download.
//! What is kept quiet is the interruption of the user.
//!
//! ## The interval is configurable (11.2)
//!
//! Twenty-four hours by default, read back from `/sys/config/updater.cfg`
//! on every lap of the loop and not once at boot: a service that never
//! re-reads its configuration forces a restart to change it, and an
//! update service is precisely the one nobody wants to restart.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::fs::perm::{Credentials, Mode};

use super::db::version_string;
use super::package;

/// Directory of the pending updates (spec 17).
pub const UPDATES_DIR: &str = "/sys/updates";

/// Configuration of the service.
pub const CONFIG_PATH: &str = "/sys/config/updater.cfg";

/// Default check interval, in hours (spec 11.2).
pub const DEFAULT_INTERVAL_HOURS: u64 = 24;

// ============================================================
//  11.1 -- Update types
// ============================================================

/// The four lines of the 11.1 table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i8)]
pub enum OsUpdateKind {
    /// 0 -- incremental, modified files only, no reboot.
    Patch = 0,
    /// 1 -- incremental, several components, no reboot.
    Update = 1,
    /// 2 -- complete, new partition, mandatory reboot.
    Major = 2,
    /// -1 -- incremental or complete depending on impact.
    Critical = -1,
}

impl OsUpdateKind {
    pub fn from_code(code: i8) -> Option<Self> {
        match code {
            0 => Some(OsUpdateKind::Patch),
            1 => Some(OsUpdateKind::Update),
            2 => Some(OsUpdateKind::Major),
            -1 => Some(OsUpdateKind::Critical),
            _ => None,
        }
    }

    pub fn code(&self) -> i8 {
        *self as i8
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            OsUpdateKind::Patch => "PATCH",
            OsUpdateKind::Update => "UPDATE",
            OsUpdateKind::Major => "MAJOR",
            OsUpdateKind::Critical => "CRITICAL",
        }
    }

    /// "Format" column.
    pub fn payload(&self) -> &'static str {
        match self {
            OsUpdateKind::Patch => "incrementale - fichiers modifies uniquement",
            OsUpdateKind::Update => "incrementale - plusieurs composants",
            OsUpdateKind::Major => "complete - nouvelle partition",
            OsUpdateKind::Critical => "incrementale ou complete selon impact",
        }
    }

    /// "Reboot" column.
    ///
    /// CRITICAL is noted "depending on impact": the answer therefore
    /// cannot be read from the type alone, and it is the package that
    /// carries it, through the `NEEDS_REBOOT` flag. Returning it here
    /// would mean deciding in the package's stead.
    pub fn reboot(&self, package_asks_reboot: bool) -> bool {
        match self {
            OsUpdateKind::Major => true,
            OsUpdateKind::Critical => package_asks_reboot,
            OsUpdateKind::Patch | OsUpdateKind::Update => false,
        }
    }

    /// Does the update occupy a new slot (11.3)?
    ///
    /// Only MAJOR does. The incremental ones make do with a snapshot,
    /// which is the reason the second paragraph of 11.3 exists.
    pub fn uses_new_slot(&self) -> bool {
        *self == OsUpdateKind::Major
    }

    /// Translation to the 10.2 table, for the notification.
    ///
    /// Written once only, and readable line by line:
    ///
    /// * CRITICAL is **mandatory** -- that is the meaning of the word;
    /// * MAJOR asks for a confirmation, because it reboots the machine;
    /// * UPDATE asks for a confirmation, it touches several components;
    /// * PATCH is optional: a few files, no reboot.
    ///
    /// No type of OS update is **silent**. 11.2 promises a silent
    /// download, never a silent application: modifying the system without
    /// saying so is something this table does not allow.
    pub fn notification_code(&self) -> package::UpdateCode {
        match self {
            OsUpdateKind::Critical => package::UpdateCode::Mandatory,
            OsUpdateKind::Major | OsUpdateKind::Update => package::UpdateCode::Standard,
            OsUpdateKind::Patch => package::UpdateCode::Optional,
        }
    }

    /// Matching notification level (spec 9.1).
    pub fn level(&self) -> crate::gui::notify::Level {
        use crate::gui::notify::Level;
        match self {
            OsUpdateKind::Critical => Level::Prioritaire,
            OsUpdateKind::Major => Level::Haute,
            OsUpdateKind::Update => Level::Normale,
            OsUpdateKind::Patch => Level::Basse,
        }
    }
}

// ============================================================
//  Current version of the system
// ============================================================

/// Version of the OS produced by this build.
///
/// Taken from `Cargo.toml` through `crate::version`, the field the
/// release workflow tags. It is what decides whether a repository offers
/// "better": a wrong value would either miss every update, or offer them
/// again without end.
pub const OS_VERSION: [u16; 4] = crate::version::NUMBERS;

/// Name of the file a fetched update is dropped under.
pub fn staging_name(version: &[u16; 4]) -> String {
    alloc::format!("nova-{}.spm", version_string(version))
}

// ============================================================
//  Configuration
// ============================================================

/// Settings of the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub interval_hours: u64,
    /// Automatic check enabled.
    pub automatic: bool,
    /// Background download before notification (11.2).
    pub background_download: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            interval_hours: DEFAULT_INTERVAL_HOURS,
            automatic: true,
            background_download: true,
        }
    }
}

impl Config {
    pub fn to_cfg(&self) -> String {
        alloc::format!(
            "# service updater.dsa - spec 11.2\nintervalle_heures={}\nautomatique={}\ntelechargement_arriere_plan={}\n",
            self.interval_hours,
            if self.automatic { 1 } else { 0 },
            if self.background_download { 1 } else { 0 }
        )
    }

    pub fn parse(text: &str) -> Config {
        let mut config = Config::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                // A null interval would make the loop turn with no rest
                // and saturate the network: bounded, not refused.
                "intervalle_heures" => {
                    if let Ok(hours) = value.parse::<u64>() {
                        config.interval_hours = hours.clamp(1, 24 * 30);
                    }
                }
                "automatique" => config.automatic = value != "0",
                "telechargement_arriere_plan" => config.background_download = value != "0",
                _ => {}
            }
        }
        config
    }
}

/// Reads the configuration, writing it if it does not exist.
pub fn config(now: u64) -> Config {
    let who = Credentials::kernel();
    if let Ok(bytes) = crate::fs::read_file(CONFIG_PATH, who)
        && let Ok(text) = core::str::from_utf8(&bytes)
    {
        return Config::parse(text);
    }

    let config = Config::default();
    let _ = crate::fs::write_file(
        CONFIG_PATH,
        config.to_cfg().as_bytes(),
        who,
        Mode::system_file(),
        now,
    );
    config
}

// ============================================================
//  Detection and download
// ============================================================

/// An available update, already downloaded.
#[derive(Debug, Clone)]
pub struct Available {
    pub version: [u16; 4],
    pub kind: OsUpdateKind,
    pub repo: String,
    /// Path of the file dropped in `/sys/updates/`.
    pub path: String,
    pub size: usize,
    pub needs_reboot: bool,
}

static CHECKS: AtomicU64 = AtomicU64::new(0);
static FOUND: AtomicU64 = AtomicU64::new(0);
static DOWNLOADED: AtomicU64 = AtomicU64::new(0);

/// Counters: (checks, found, downloaded).
pub fn counters() -> (u64, u64, u64) {
    (
        CHECKS.load(Ordering::Relaxed),
        FOUND.load(Ordering::Relaxed),
        DOWNLOADED.load(Ordering::Relaxed),
    )
}

/// 8.4.3 -- Queries the repositories, silently downloads what is newer
/// than the current version.
///
/// Returns what was actually dropped. An unreachable repository is
/// ignored: the next check will try again, and failing the whole check
/// because a third-party repository is offline would deprive the user of
/// the updates of the others.
pub fn check_now(now: u64) -> Vec<Available> {
    CHECKS.fetch_add(1, Ordering::Relaxed);

    let settings = config(now);
    let mut available = Vec::new();

    for repo in super::repo::load() {
        let index = match super::repo::fetch_index(&repo) {
            Ok(index) => index,
            Err(error) => {
                crate::serial_println!(
                    "[updater] depot \"{}\" ignore - {}",
                    repo.name,
                    error.message()
                );
                continue;
            }
        };

        for entry in index.os {
            if entry.version <= OS_VERSION {
                continue;
            }
            FOUND.fetch_add(1, Ordering::Relaxed);
            crate::serial_println!(
                "[updater] {} {} proposee par \"{}\" - {}",
                entry.kind.as_str(),
                version_string(&entry.version),
                repo.name,
                entry.kind.payload()
            );

            if !settings.background_download {
                continue;
            }

            let bytes = match super::repo::fetch_os_update(&repo, &entry) {
                Ok(bytes) => bytes,
                Err(error) => {
                    crate::serial_println!(
                        "[updater] recuperation refusee - {}",
                        error.message()
                    );
                    continue;
                }
            };

            // A file arriving intact but that is not a package, or whose
            // signature is refused, must not be dropped in
            // `/sys/updates/`: that directory is read back at the next boot.
            let Ok(header) = package::parse(&bytes) else {
                crate::serial_println!("[updater] fichier recu illisible - ignore");
                continue;
            };
            let verdict = package::verify(&header, &bytes);
            if !verdict.accepted() {
                crate::serial_println!(
                    "[updater] paquet refuse - {}",
                    verdict.as_str()
                );
                continue;
            }

            match stage(&entry.version, &bytes, now) {
                Ok(path) => {
                    DOWNLOADED.fetch_add(1, Ordering::Relaxed);
                    crate::serial_println!(
                        "[updater] {} octets deposes dans {} - en attente",
                        bytes.len(),
                        path
                    );
                    available.push(Available {
                        version: entry.version,
                        kind: entry.kind,
                        repo: repo.name.clone(),
                        path,
                        size: bytes.len(),
                        needs_reboot: entry
                            .kind
                            .reboot(header.has_flag(package::flags::NEEDS_REBOOT)),
                    });
                }
                Err(error) => {
                    crate::serial_println!(
                        "[updater] depot local impossible - {}",
                        error.message()
                    );
                }
            }
        }
    }

    available
}

/// Writes a fetched update into `/sys/updates/`.
pub fn stage(
    version: &[u16; 4],
    bytes: &[u8],
    now: u64,
) -> Result<String, crate::fs::FsError> {
    let who = Credentials::kernel();
    if crate::fs::stat(UPDATES_DIR, who).is_err() {
        crate::fs::create_directory(UPDATES_DIR, who, Mode::system_dir(), now)?;
    }
    let path = alloc::format!("{}/{}", UPDATES_DIR, staging_name(version));
    let _ = crate::fs::delete(&path, who, now);
    crate::fs::write_file(&path, bytes, who, Mode::system_file(), now)?;
    Ok(path)
}

/// Updates already dropped and not yet applied.
///
/// Reads the directory back rather than a registry: it is the directory
/// that is authoritative, and a registry describing it could diverge from
/// it after a cut at the wrong moment.
pub fn pending() -> Vec<(String, [u16; 4])> {
    let who = Credentials::kernel();
    let Ok(links) = crate::fs::list(UPDATES_DIR, who) else {
        return Vec::new();
    };

    let mut found = Vec::new();
    for link in links {
        let Some(rest) = link.name.strip_prefix("nova-") else {
            continue;
        };
        let Some(version) = rest.strip_suffix(".spm") else {
            continue;
        };
        let Some(version) = super::db::parse_version(version) else {
            continue;
        };
        found.push((alloc::format!("{}/{}", UPDATES_DIR, link.name), version));
    }
    found.sort_by(|a, b| a.1.cmp(&b.1));
    found
}

/// Notifies an available update, per 10.2 and 9.1.
///
/// The notification only goes out if the compositor is running: without
/// it there is no screen, and the serial console has already said all.
pub fn notify(update: &Available) {
    let code = update.kind.notification_code();
    if !code.notifies() {
        return;
    }

    let title = alloc::format!("Mise a jour {}", update.kind.as_str());
    let body = alloc::format!(
        "Nova OS {} est disponible ({} Kio). {}",
        version_string(&update.version),
        update.size.div_ceil(1024),
        if update.needs_reboot {
            "Redemarrage necessaire."
        } else {
            "Aucun redemarrage."
        }
    );

    let level = update.kind.level();
    let posted = crate::gui::with(|comp| {
        crate::gui::compositor::post_notification(comp, level, &title, &body)
    });

    match posted {
        Some(Some(id)) => crate::serial_println!(
            "[updater] notification {} emise - niveau {}, code MAJ {} ({})",
            id,
            level.as_str(),
            code.code(),
            code.behaviour()
        ),
        _ => crate::serial_println!(
            "[updater] pas de compositeur - notification {} non affichee",
            level.as_str()
        ),
    }
}

// ============================================================
//  8.4.2 -- The service
// ============================================================

/// The `updater.dsa` task.
///
/// It does not check at boot: the network is not configured yet when
/// `init.dsa` launches it, and an immediate first check would do nothing
/// but log a failure. It waits for the first interval -- unless woken by
/// `spm update --os`, which calls `check_now` directly from the calling
/// task.
pub fn task() -> ! {
    crate::serial_println!("[updater] service demarre (spec 11.2)");

    // Leaves the network time to configure itself. Without this wait, the
    // first check would systematically fall before DHCP.
    let _ = crate::syscall::invoke(
        crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
        [8_000, 0, 0, 0, 0, 0],
    );

    loop {
        let now = crate::idt::ticks();
        let settings = config(now);

        if settings.automatic {
            for update in check_now(now) {
                notify(&update);
                super::rollback::stage_update(&update, now);
            }
        }

        // The interval is read back on every lap: changing it in the file
        // must take effect without restarting the service.
        let sleep_ms = settings.interval_hours.saturating_mul(3_600_000);
        sleep_long(sleep_ms);
    }
}

/// Sleeps a long time, in slices.
///
/// `SYS_TIME_SLEEP_MS` counts in ticks; twenty-four hours make several
/// million of them, and cutting them up lets the loop re-read its
/// configuration if it changes meanwhile -- at worst a minute later.
fn sleep_long(total_ms: u64) {
    const SLICE_MS: u64 = 60_000;
    let mut remaining = total_ms;
    while remaining > 0 {
        let slice = remaining.min(SLICE_MS);
        let _ = crate::syscall::invoke(
            crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
            [slice, 0, 0, 0, 0, 0],
        );
        remaining -= slice;
    }
}

pub fn dump_state() {
    let (checks, found, downloaded) = counters();
    let settings = config(crate::idt::ticks());
    crate::serial_println!(
        "[updater] version {} - intervalle {} h, automatique {}",
        version_string(&OS_VERSION),
        settings.interval_hours,
        if settings.automatic { "oui" } else { "non" }
    );
    crate::serial_println!(
        "[updater] {} verification(s), {} trouvee(s), {} telechargee(s), {} en attente",
        checks,
        found,
        downloaded,
        pending().len()
    );
}

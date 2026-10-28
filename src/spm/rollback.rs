//! 8.5 -- Automatic rollback (spec 11.3).
//!
//! ## What 11.3 asks for
//!
//! Two distinct mechanisms, often confused:
//!
//! 1. **After a MAJOR**: the next boot validates every core service, with
//!    a deadline of sixty seconds. On failure, automatic return to the
//!    previous version, with a notification explaining the reason.
//!
//! 2. **For the incremental ones**: a snapshot kept thirty days in
//!    `/sys/updates/rollback/`.
//!
//! The first protects against a system that no longer boots. The second
//! against a regression only noticed after the fact. They have neither
//! the same trigger, nor the same lifetime.
//!
//! ## The double partition, and what it really is here
//!
//! 11.1 says of a MAJOR that it occupies a "new partition". The idea is
//! that an update never overwrites the running system: it installs
//! **beside** it, and the next boot switches. If that boot fails, the old
//! slot is intact and the switch is cancelled.
//!
//! Two slots, `A` and `B`. Which is active is written in the state, and
//! that is the only thing telling them apart -- a slot is not a property
//! of the disk, it is a recorded choice.
//!
//! **Acknowledged limit**: with no bootloader able to choose between two
//! kernel images, the switch is recorded and honoured by the filesystem,
//! not by GRUB. A `B` slot on a second volume does receive the files, the
//! state does switch, the rollback does restore -- but the image actually
//! booted stays the one GRUB knows. Closing that limit demands one menu
//! entry per slot and a kernel that knows where it runs, which is not
//! phase 8.
//!
//! ## Why the attempt counter is incremented *before* the validation
//!
//! A system crashing during validation would never pass again through
//! the code incrementing the counter, and would reboot forever on the
//! broken version. Increment first, validate next: on the second
//! unsuccessful attempt, the rollback starts with no need to have reached
//! anything at all.

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::perm::{Credentials, Mode};

use super::db::{parse_version, version_string};
use super::update::{Available, OsUpdateKind};

/// Directory of the snapshots (spec 17).
pub const ROLLBACK_DIR: &str = "/sys/updates/rollback";

/// State file.
pub const STATE_PATH: &str = "/sys/updates/state.cfg";

/// Retention time of a snapshot, in seconds (spec 11.3).
pub const RETENTION_SECONDS: u64 = 30 * 24 * 3600;

/// Validation deadline of the core services (spec 11.3).
pub const VALIDATION_TIMEOUT_MS: u64 = 60_000;

/// Number of attempts before rollback.
///
/// Two: the first may fail on a transient cause -- a slow disk, a device
/// absent on the first lap. The second one failing is no longer an
/// accident.
pub const MAX_ATTEMPTS: u32 = 2;

/// The two slots of 11.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    A,
    B,
}

impl Slot {
    pub fn as_str(&self) -> &'static str {
        match self {
            Slot::A => "A",
            Slot::B => "B",
        }
    }

    pub fn other(&self) -> Slot {
        match self {
            Slot::A => Slot::B,
            Slot::B => Slot::A,
        }
    }

    fn from_str(text: &str) -> Slot {
        match text.trim() {
            "B" => Slot::B,
            _ => Slot::A,
        }
    }

    /// Root of the system for this slot.
    ///
    /// `A` is the boot volume. `B` takes the second mounted volume if one
    /// exists -- that is the real double partition -- and otherwise falls
    /// back on a directory of the boot volume. The fallback protects
    /// against the same accident (an update that does not boot), not
    /// against the same failure (an unreadable volume): that is said, it
    /// is not equivalent.
    pub fn root(&self) -> String {
        match self {
            Slot::A => String::from("/"),
            Slot::B => match second_volume() {
                Some(letter) => alloc::format!("{}:/nova", letter),
                None => String::from("/sys/updates/slot-b"),
            },
        }
    }

    /// Does the slot have a volume of its own?
    pub fn is_separate_volume(&self) -> bool {
        match self {
            Slot::A => true,
            Slot::B => second_volume().is_some(),
        }
    }
}

/// Letter of the first volume that is not the boot one.
fn second_volume() -> Option<char> {
    crate::fs::disk_list()
        .into_iter()
        .map(|disk| disk.letter)
        .find(|letter| *letter != crate::fs::BOOT_LETTER)
}

// ============================================================
//  State
// ============================================================

/// Persistent state of the update mechanism.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub current: [u16; 4],
    pub previous: Option<[u16; 4]>,
    pub slot: Slot,
    /// An update has been applied and is waiting to be validated by a
    /// complete boot.
    pub awaiting_validation: bool,
    pub attempts: u32,
    /// Date of application, in seconds since the epoch.
    pub applied_at: u64,
    pub kind: Option<OsUpdateKind>,
    /// File dropped and not yet applied.
    pub staged: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        State {
            current: super::update::OS_VERSION,
            previous: None,
            slot: Slot::A,
            awaiting_validation: false,
            attempts: 0,
            applied_at: 0,
            kind: None,
            staged: None,
        }
    }
}

impl State {
    pub fn to_cfg(&self) -> String {
        let mut text = String::from("# etat des mises a jour - spec 11.3\n");
        text.push_str(&alloc::format!(
            "version_courante={}\n",
            version_string(&self.current)
        ));
        if let Some(previous) = &self.previous {
            text.push_str(&alloc::format!(
                "version_precedente={}\n",
                version_string(previous)
            ));
        }
        text.push_str(&alloc::format!("emplacement={}\n", self.slot.as_str()));
        text.push_str(&alloc::format!(
            "attente_validation={}\n",
            if self.awaiting_validation { 1 } else { 0 }
        ));
        text.push_str(&alloc::format!("essais={}\n", self.attempts));
        text.push_str(&alloc::format!("appliquee_le={}\n", self.applied_at));
        if let Some(kind) = self.kind {
            text.push_str(&alloc::format!("type={}\n", kind.code()));
        }
        if let Some(staged) = &self.staged {
            text.push_str(&alloc::format!("en_attente={}\n", staged));
        }
        text
    }

    pub fn parse(text: &str) -> State {
        let mut state = State::default();
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
                "version_courante" => {
                    if let Some(version) = parse_version(value) {
                        state.current = version;
                    }
                }
                "version_precedente" => state.previous = parse_version(value),
                "emplacement" => state.slot = Slot::from_str(value),
                "attente_validation" => state.awaiting_validation = value != "0",
                "essais" => state.attempts = value.parse().unwrap_or(0),
                "appliquee_le" => state.applied_at = value.parse().unwrap_or(0),
                "type" => {
                    state.kind = value.parse::<i8>().ok().and_then(OsUpdateKind::from_code);
                }
                "en_attente" => {
                    if !value.is_empty() {
                        state.staged = Some(String::from(value));
                    }
                }
                _ => {}
            }
        }
        state
    }
}

/// Reads the state. A missing file is a system never updated, not an
/// error.
pub fn state() -> State {
    let who = Credentials::kernel();
    match crate::fs::read_file(STATE_PATH, who) {
        Ok(bytes) => match core::str::from_utf8(&bytes) {
            Ok(text) => State::parse(text),
            Err(_) => State::default(),
        },
        Err(_) => State::default(),
    }
}

/// Writes the state.
pub fn save_state(state: &State, now: u64) -> Result<(), crate::fs::FsError> {
    let who = Credentials::kernel();
    if crate::fs::stat(super::update::UPDATES_DIR, who).is_err() {
        crate::fs::create_directory(
            super::update::UPDATES_DIR,
            who,
            Mode::system_dir(),
            now,
        )?;
    }
    let _ = crate::fs::delete(STATE_PATH, who, now);
    crate::fs::write_file(
        STATE_PATH,
        state.to_cfg().as_bytes(),
        who,
        Mode::system_file(),
        now,
    )
    .map(|_| ())
}

// ============================================================
//  8.5.4 -- Incremental snapshots
// ============================================================

/// Timestamp in seconds. The RTC when it answers, zero otherwise.
///
/// Zero is recognisable and serves as a marker: a snapshot stamped zero
/// is **never** purged, because its age is unknown. Deleting by default
/// what one cannot date would amount to erasing the only recourse of a
/// system with no CMOS battery.
pub fn epoch_now() -> u64 {
    crate::rtc::now().unwrap_or(0)
}

/// Takes a snapshot of the given files.
///
/// Returns the path of the directory created. "Incremental" in the sense
/// of 11.3: only the files the update is going to touch are copied, not
/// the whole system -- a complete snapshot on every PATCH would fill the
/// volume in a few weeks.
pub fn snapshot(label: &str, files: &[String], now: u64) -> Result<String, crate::fs::FsError> {
    let who = Credentials::kernel();

    if crate::fs::stat(ROLLBACK_DIR, who).is_err() {
        crate::fs::create_directory(ROLLBACK_DIR, who, Mode::system_dir(), now)?;
    }

    let directory = alloc::format!("{}/{}-{}", ROLLBACK_DIR, epoch_now(), label);
    crate::fs::create_directory(&directory, who, Mode::system_dir(), now)?;

    // The manifest keeps the original path of each file: the flattened
    // name would not be enough to know where to put it back, and two
    // files of the same name in two directories would overlap.
    let mut manifest = String::from("# snapshot - chemin d'origine par ligne\n");
    let mut saved = 0;

    for (index, path) in files.iter().enumerate() {
        let Ok(bytes) = crate::fs::read_file(path, who) else {
            continue;
        };
        let name = alloc::format!("{}.bin", index);
        let target = alloc::format!("{}/{}", directory, name);
        if crate::fs::write_file(&target, &bytes, who, Mode::system_file(), now).is_ok() {
            manifest.push_str(&alloc::format!("{}|{}\n", name, path));
            saved += 1;
        }
    }

    crate::fs::write_file(
        &alloc::format!("{}/manifeste.txt", directory),
        manifest.as_bytes(),
        who,
        Mode::system_file(),
        now,
    )?;

    crate::serial_println!(
        "[rollback] snapshot \"{}\" - {} fichier(s) dans {}",
        label,
        saved,
        directory
    );
    Ok(directory)
}

/// One listed snapshot.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub path: String,
    pub label: String,
    /// Creation date in seconds, zero if unknown.
    pub created_at: u64,
}

/// Lists the snapshots, from the oldest to the most recent.
pub fn snapshots() -> Vec<Snapshot> {
    let who = Credentials::kernel();
    let Ok(links) = crate::fs::list(ROLLBACK_DIR, who) else {
        return Vec::new();
    };

    let mut found: Vec<Snapshot> = links
        .into_iter()
        .filter_map(|link| {
            let (stamp, label) = link.name.split_once('-')?;
            Some(Snapshot {
                path: alloc::format!("{}/{}", ROLLBACK_DIR, link.name),
                label: String::from(label),
                created_at: stamp.parse().ok()?,
            })
        })
        .collect();
    found.sort_by_key(|snapshot| snapshot.created_at);
    found
}

/// Restores a snapshot: each file returns to its original path.
pub fn restore(snapshot: &Snapshot, now: u64) -> usize {
    let who = Credentials::kernel();
    let manifest = alloc::format!("{}/manifeste.txt", snapshot.path);
    let Ok(bytes) = crate::fs::read_file(&manifest, who) else {
        return 0;
    };
    let Ok(text) = core::str::from_utf8(&bytes) else {
        return 0;
    };

    let mut restored = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, origin)) = line.split_once('|') else {
            continue;
        };
        let source = alloc::format!("{}/{}", snapshot.path, name);
        let Ok(content) = crate::fs::read_file(&source, who) else {
            continue;
        };
        let _ = crate::fs::delete(origin, who, now);
        if crate::fs::write_file(origin, &content, who, Mode::system_file(), now).is_ok() {
            restored += 1;
        }
    }
    restored
}

/// Deletes a snapshot and its content.
pub fn discard(snapshot: &Snapshot, now: u64) -> bool {
    let who = Credentials::kernel();
    let Ok(links) = crate::fs::list(&snapshot.path, who) else {
        return false;
    };
    for link in links {
        let _ = crate::fs::delete(&alloc::format!("{}/{}", snapshot.path, link.name), who, now);
    }
    crate::fs::delete(&snapshot.path, who, now).is_ok()
}

/// 8.5.4 -- Purges the snapshots older than thirty days.
///
/// Returns the number of snapshots deleted. A snapshot stamped zero is
/// kept: see `epoch_now`.
pub fn prune(now: u64) -> usize {
    let reference = epoch_now();
    if reference == 0 {
        return 0;
    }

    let mut removed = 0;
    for snapshot in snapshots() {
        if snapshot.created_at == 0 {
            continue;
        }
        if reference.saturating_sub(snapshot.created_at) > RETENTION_SECONDS
            && discard(&snapshot, now)
        {
            removed += 1;
            crate::serial_println!(
                "[rollback] snapshot \"{}\" purge - plus de 30 jours",
                snapshot.label
            );
        }
    }
    removed
}

// ============================================================
//  8.5.1 -- Applying an update
// ============================================================

/// Records that an update is downloaded and waiting to be applied.
pub fn stage_update(update: &Available, now: u64) {
    let mut current = state();
    current.staged = Some(update.path.clone());
    current.kind = Some(update.kind);
    if let Err(error) = save_state(&current, now) {
        crate::serial_println!(
            "[rollback] etat non enregistre - {}",
            error.message()
        );
    }
}

/// Applies an already dropped update.
///
/// What "applying" covers depends on the type:
///
/// * incremental (PATCH, UPDATE, CRITICAL with no reboot) -- snapshot of
///   the files touched, then writing;
/// * complete (MAJOR) -- writing into the inactive slot, switch
///   recorded, validation demanded at the next boot.
///
/// Returns `true` if the application took place.
pub fn apply(
    kind: OsUpdateKind,
    version: [u16; 4],
    touched: &[String],
    now: u64,
) -> bool {
    let mut current = state();

    if kind.uses_new_slot() {
        let target = current.slot.other();
        crate::serial_println!(
            "[rollback] {} vers l'emplacement {} ({}) - validation exigee au prochain demarrage",
            kind.as_str(),
            target.as_str(),
            if target.is_separate_volume() {
                "volume distinct"
            } else {
                "repli sur le volume de demarrage"
            }
        );

        let who = Credentials::kernel();
        let root = target.root();
        if crate::fs::stat(&root, who).is_err()
            && crate::fs::create_directory(&root, who, Mode::system_dir(), now).is_err()
        {
            crate::serial_println!("[rollback] emplacement {} inaccessible", target.as_str());
            return false;
        }

        current.previous = Some(current.current);
        current.current = version;
        current.slot = target;
        current.awaiting_validation = true;
        current.attempts = 0;
        current.applied_at = epoch_now();
        current.kind = Some(kind);
        current.staged = None;
    } else {
        // Incremental: the recourse is the snapshot, not the slot.
        if snapshot(&version_string(&version), touched, now).is_err() {
            crate::serial_println!("[rollback] snapshot impossible - application annulee");
            return false;
        }
        current.previous = Some(current.current);
        current.current = version;
        current.awaiting_validation = false;
        current.applied_at = epoch_now();
        current.kind = Some(kind);
        current.staged = None;
    }

    if let Err(error) = save_state(&current, now) {
        crate::serial_println!("[rollback] etat non enregistre - {}", error.message());
        return false;
    }
    true
}

// ============================================================
//  8.5.2 and 8.5.3 -- Boot validation and rollback
// ============================================================

/// Reason for a rollback, as the notification explains it (11.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// The core services did not answer within the sixty seconds.
    CoreTimeout,
    /// A core service declared itself failed.
    CoreFailed,
    /// The number of attempts is exhausted.
    AttemptsExhausted,
}

impl Cause {
    pub fn message(&self) -> &'static str {
        match self {
            Cause::CoreTimeout => "les services core n'ont pas repondu en 60 s",
            Cause::CoreFailed => "un service core a echoue au demarrage",
            Cause::AttemptsExhausted => "deux demarrages consecutifs ont echoue",
        }
    }
}

/// Is there a validation in progress?
pub fn validation_pending() -> bool {
    state().awaiting_validation
}

/// 8.5.2 -- Opens the validation window of a boot.
///
/// Increments the attempt counter **before** any wait: a system freezing
/// during validation would never pass through here again, and would
/// reboot endlessly on the broken version. Returns `false` when the
/// attempts are exhausted, in which case the caller must go back without
/// even trying.
pub fn begin_validation(now: u64) -> bool {
    let mut current = state();
    if !current.awaiting_validation {
        return true;
    }

    current.attempts += 1;
    let attempts = current.attempts;
    let _ = save_state(&current, now);

    crate::serial_println!(
        "[rollback] validation du demarrage - essai {} sur {} (spec 11.3)",
        attempts,
        MAX_ATTEMPTS
    );

    attempts <= MAX_ATTEMPTS
}

/// 8.5.2 -- Marks the boot as valid.
pub fn confirm(now: u64) {
    let mut current = state();
    if !current.awaiting_validation {
        return;
    }
    current.awaiting_validation = false;
    current.attempts = 0;
    let _ = save_state(&current, now);

    crate::serial_println!(
        "[rollback] version {} validee - services core en ligne",
        version_string(&current.current)
    );
}

/// 8.5.3 -- Return to the previous version.
///
/// The notification is emitted **before** the state is written: if the
/// machine goes off between the two, a message whose rollback did not
/// happen is better than a rollback whose reason nobody knew. 11.3
/// explicitly demands that the notification explain the cause.
pub fn rollback(cause: Cause, now: u64) -> bool {
    let mut current = state();
    let Some(previous) = current.previous else {
        crate::serial_println!(
            "[rollback] {} - aucune version precedente, rien a restaurer",
            cause.message()
        );
        return false;
    };

    crate::vga::set_color(crate::vga::Color::Yellow, crate::vga::Color::Black);
    crate::kprintln!(
        "[rollback] retour a Nova OS {} - {}",
        version_string(&previous),
        cause.message()
    );
    crate::vga::set_color(crate::vga::Color::LightGray, crate::vga::Color::Black);

    notify_rollback(&previous, cause);

    let restored = current.slot;
    current.slot = current.slot.other();
    current.current = previous;
    current.previous = None;
    current.awaiting_validation = false;
    current.attempts = 0;
    current.kind = None;

    // An incremental update has no slot to give back: it is its most
    // recent snapshot that undoes it.
    if restored == current.slot {
        if let Some(snapshot) = snapshots().last() {
            let files = restore(snapshot, now);
            crate::serial_println!(
                "[rollback] {} fichier(s) restaure(s) depuis \"{}\"",
                files,
                snapshot.label
            );
        }
    }

    if let Err(error) = save_state(&current, now) {
        crate::serial_println!("[rollback] etat non enregistre - {}", error.message());
        return false;
    }
    true
}

/// Notification of 11.3: it must explain the reason.
fn notify_rollback(version: &[u16; 4], cause: Cause) {
    use crate::gui::notify::Level;

    let title = String::from("Retour a la version precedente");
    let body = alloc::format!(
        "La mise a jour a ete annulee : {}. Nova OS {} est restaure.",
        cause.message(),
        version_string(version)
    );

    let posted = crate::gui::with(|comp| {
        crate::gui::compositor::post_notification(comp, Level::Urgent, &title, &body)
    });
    if posted.is_none() {
        crate::serial_println!("[rollback] pas de compositeur - notification non affichee");
    }
}

/// Validation task, launched at boot when an update is waiting.
///
/// It does one thing only: wait for the core services to be all online,
/// for at most sixty seconds. A separate task rather than a wait inside
/// `init.dsa` -- `init` must carry on launching the secondary services
/// while the window runs, failing which the validation would measure its
/// own wait.
pub fn validation_task() -> ! {
    let now = crate::idt::ticks();

    if !begin_validation(now) {
        rollback(Cause::AttemptsExhausted, now);
        crate::scheduler::exit_current_with(0);
    }

    let per_tick = 1000 / crate::TIMER_HZ as u64;
    let deadline = crate::idt::ticks() + VALIDATION_TIMEOUT_MS.div_ceil(per_tick).max(1);

    loop {
        if crate::init::core_ready() {
            confirm(crate::idt::ticks());
            prune(crate::idt::ticks());
            crate::scheduler::exit_current_with(0);
        }

        let (_, failed) = crate::init::service_state();
        if failed > 0 {
            rollback(Cause::CoreFailed, crate::idt::ticks());
            crate::scheduler::exit_current_with(0);
        }

        if crate::idt::ticks() >= deadline {
            rollback(Cause::CoreTimeout, crate::idt::ticks());
            crate::scheduler::exit_current_with(0);
        }

        let _ = crate::syscall::invoke(
            crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
            [250, 0, 0, 0, 0, 0],
        );
    }
}

/// Called by `init.dsa` on every boot.
///
/// Returns `true` if a validation has been engaged -- in which case a
/// task follows it. With no pending update, the snapshot purge is done
/// along the way: it is the only moment in the life of the system when
/// nobody is in a hurry.
pub fn on_boot(now: u64) -> bool {
    if !validation_pending() {
        let removed = prune(now);
        if removed > 0 {
            crate::serial_println!("[rollback] {} snapshot(s) purge(s)", removed);
        }
        return false;
    }

    crate::scheduler::spawn(
        "validation.dsa",
        crate::process::BinaryType::Dsa,
        validation_task,
    );
    true
}

pub fn dump_state() {
    let current = state();
    crate::serial_println!(
        "[rollback] version {} - emplacement {} ({}), {} snapshot(s)",
        version_string(&current.current),
        current.slot.as_str(),
        if current.slot.is_separate_volume() {
            "volume distinct"
        } else {
            "repli"
        },
        snapshots().len()
    );
    if current.awaiting_validation {
        crate::serial_println!(
            "[rollback] validation en attente - essai {} sur {}",
            current.attempts,
            MAX_ATTEMPTS
        );
    }
    if let Some(previous) = current.previous {
        crate::serial_println!(
            "[rollback] version precedente conservee : {}",
            version_string(&previous)
        );
    }
}

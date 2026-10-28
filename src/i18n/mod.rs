//! Phase 10.2 -- Internationalisation (spec 14).
//!
//! | Module | Covers |
//! |---|---|
//! | `units` | 10.2.3 -- dates, times, numbers, currency, the four unit families |
//! | `lang` | 10.2.6 -- the `.lang` format and `{name}` substitution |
//! | `keyboard` | 10.2.4 -- layouts, primary/secondary, switch key |
//! | `tz` | 10.2.5 -- zones, UTC storage, conversion at display |
//!
//! ## Priority (14.1)
//!
//! **Application > User > System.** Three files, read from the least
//! specific to the most: each one applies the keys it declares over what
//! the previous left. A setting absent from the user file therefore keeps
//! its system value instead of falling back to a compiled default -- that
//! is the difference between "overriding" and "replacing", and only the
//! first matches what 14.1 describes.
//!
//! ```text
//!   /sys/config/i18n.cfg                system
//!   /users/{user}/settings/i18n.cfg     user
//!   /apps/{app}/i18n/settings.cfg       application
//! ```
//!
//! ## Hot reload (14.1)
//!
//! "Every change is immediate -- the compositor reloads the translations
//! through `SYS_IPC_PUBLISH("i18n.changed")` with no restart." So
//! `reload` re-reads the files, swaps the active settings under the lock,
//! then publishes. It does not ask anyone to restart, and it does not
//! hold the lock while publishing: a subscriber that woke up and
//! immediately asked for the settings would deadlock against the very
//! call that woke it.
//!
//! ## What the active settings are, and are not
//!
//! One global set, that of the session. An application asking for its own
//! (14.1 allows it to override) gets them through `settings_for_app`,
//! which layers its file on top -- it does not change what anybody else
//! sees. A per-process setting table would need a per-process descriptor
//! table, which the kernel does not have; that limit is named in the
//! roadmap rather than hidden behind an API that looks per-process.

pub mod keyboard;
pub mod lang;
pub mod selftest;
pub mod tz;
pub mod units;

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::fs::perm::{Credentials, Mode};
use crate::vga::SpinLock;

use keyboard::{Layout, SwitchKey};
use units::{
    ClockFormat, CurrencyPosition, DateFormat, Distance, Separator, Speed, Temperature, Weight,
};

/// Channel on which a change is published (14.1).
pub const CHANGE_CHANNEL: &str = "i18n.changed";

/// System-wide settings file.
pub const SYSTEM_CONFIG: &str = "/sys/config/i18n.cfg";
/// User settings, relative to `/users/{user}/`.
pub const USER_CONFIG: &str = "settings/i18n.cfg";
/// Root of the system translations (spec 17).
pub const SYSTEM_I18N: &str = "/sys/i18n";

/// Number of reloads since boot, and number of publications.
static RELOADS: AtomicU64 = AtomicU64::new(0);
static PUBLICATIONS: AtomicU64 = AtomicU64::new(0);

// ============================================================
//  Settings
// ============================================================

/// The complete 14.2 table, in one structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Main language, e.g. `fr-FR`.
    pub language: String,
    /// Language used when a key is missing from the main one.
    pub fallback: String,
    pub date: DateFormat,
    pub clock: ClockFormat,
    pub thousands: Separator,
    pub currency: String,
    pub currency_position: CurrencyPosition,
    pub distance: Distance,
    pub weight: Weight,
    pub temperature: Temperature,
    pub speed: Speed,
    pub keyboard_primary: Layout,
    pub keyboard_secondary: Layout,
    pub keyboard_switch: SwitchKey,
    /// Time zone identifier; the offset is looked up in `tz::ZONES`.
    pub timezone: String,
}

impl Default for Settings {
    /// The reference locale of the system.
    ///
    /// French, because every string the system displays is written in
    /// French and a default that did not match them would show a
    /// half-translated interface on a machine nobody has configured.
    fn default() -> Self {
        Settings {
            language: String::from("fr-FR"),
            fallback: String::from("en-US"),
            date: DateFormat::Dmy,
            clock: ClockFormat::H24,
            thousands: Separator::Space,
            currency: String::from("EUR"),
            currency_position: CurrencyPosition::After,
            distance: Distance::Km,
            weight: Weight::Kg,
            temperature: Temperature::Celsius,
            speed: Speed::Kmh,
            keyboard_primary: Layout::Azerty,
            keyboard_secondary: Layout::Qwerty,
            keyboard_switch: SwitchKey::RightAlt,
            timezone: String::from("UTC"),
        }
    }
}

impl Settings {
    /// Applies one `key=value` line. Returns `false` if nothing matched.
    ///
    /// An unknown key is **not** an error: a configuration written by a
    /// later version must not make an older kernel refuse the whole file.
    /// A *known* key with an invalid value is refused, though -- silently
    /// keeping the old value there would hide a typo forever.
    pub fn apply(&mut self, line: &str) -> bool {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return false;
        }
        let Some((key, value)) = line.split_once('=') else {
            return false;
        };
        let key = key.trim();
        let value = value.trim();

        match key {
            "langue" | "language" => {
                if value.is_empty() {
                    return false;
                }
                self.language = String::from(value);
            }
            "repli" | "fallback" => {
                if value.is_empty() {
                    return false;
                }
                self.fallback = String::from(value);
            }
            "date" => match DateFormat::parse(value) {
                Some(format) => self.date = format,
                None => return false,
            },
            "heure" | "time" => match ClockFormat::parse(value) {
                Some(format) => self.clock = format,
                None => return false,
            },
            "milliers" | "thousands" => match Separator::parse(value) {
                Some(separator) => self.thousands = separator,
                None => return false,
            },
            "monnaie" | "currency" => {
                // A currency code is three letters. Refusing anything
                // else stops `EUR ` and `euros` from both appearing in
                // the same interface.
                if value.len() != 3 || !value.bytes().all(|byte| byte.is_ascii_alphabetic()) {
                    return false;
                }
                self.currency = value.to_ascii_uppercase();
            }
            "monnaie_position" | "currency_position" => match CurrencyPosition::parse(value) {
                Some(position) => self.currency_position = position,
                None => return false,
            },
            "distance" => match Distance::parse(value) {
                Some(unit) => self.distance = unit,
                None => return false,
            },
            "poids" | "weight" => match Weight::parse(value) {
                Some(unit) => self.weight = unit,
                None => return false,
            },
            "temperature" => match Temperature::parse(value) {
                Some(unit) => self.temperature = unit,
                None => return false,
            },
            "vitesse" | "speed" => match Speed::parse(value) {
                Some(unit) => self.speed = unit,
                None => return false,
            },
            "clavier" | "keyboard" => match Layout::parse(value) {
                Some(layout) => self.keyboard_primary = layout,
                None => return false,
            },
            "clavier_secondaire" | "keyboard_secondary" => match Layout::parse(value) {
                Some(layout) => self.keyboard_secondary = layout,
                None => return false,
            },
            "clavier_bascule" | "keyboard_switch" => match SwitchKey::parse(value) {
                Some(key) => self.keyboard_switch = key,
                None => return false,
            },
            "fuseau" | "timezone" => {
                // Refused if unknown: silently falling back to UTC would
                // shift every timestamp with nothing to say why.
                if tz::find(value).is_none() {
                    return false;
                }
                self.timezone = String::from(value);
            }
            _ => return false,
        }
        true
    }

    /// Merges a whole file. Returns how many settings were applied.
    pub fn merge(&mut self, text: &str) -> usize {
        text.lines().filter(|line| self.apply(line)).count()
    }

    /// Serialises back to the configuration format.
    pub fn to_cfg(&self) -> String {
        let mut out = String::new();
        out.push_str("# Nova OS - parametres regionaux (spec 14.2)\n");
        out.push_str(&alloc::format!("langue = {}\n", self.language));
        out.push_str(&alloc::format!("repli = {}\n", self.fallback));
        out.push_str(&alloc::format!("date = {}\n", self.date.as_str()));
        out.push_str(&alloc::format!("heure = {}\n", self.clock.as_str()));
        out.push_str(&alloc::format!("milliers = {}\n", self.thousands.as_str()));
        out.push_str(&alloc::format!("monnaie = {}\n", self.currency));
        out.push_str(&alloc::format!(
            "monnaie_position = {}\n",
            self.currency_position.as_str()
        ));
        out.push_str(&alloc::format!("distance = {}\n", self.distance.as_str()));
        out.push_str(&alloc::format!("poids = {}\n", self.weight.as_str()));
        out.push_str(&alloc::format!(
            "temperature = {}\n",
            self.temperature.as_str()
        ));
        out.push_str(&alloc::format!("vitesse = {}\n", self.speed.as_str()));
        out.push_str(&alloc::format!(
            "clavier = {}\n",
            self.keyboard_primary.as_str()
        ));
        out.push_str(&alloc::format!(
            "clavier_secondaire = {}\n",
            self.keyboard_secondary.as_str()
        ));
        out.push_str(&alloc::format!(
            "clavier_bascule = {}\n",
            self.keyboard_switch.as_str()
        ));
        out.push_str(&alloc::format!("fuseau = {}\n", self.timezone));
        out
    }

    /// The zone named by `timezone`, or UTC if it has gone missing.
    pub fn zone(&self) -> &'static tz::Zone {
        tz::find(&self.timezone).unwrap_or_else(tz::default_zone)
    }

    // --- the formatting entry points of 14.2 ---

    /// Formats a UTC instant as a local date.
    pub fn format_date(&self, epoch_utc: u64) -> String {
        let local = crate::rtc::DateTime::from_epoch(self.zone().to_local(epoch_utc));
        units::format_date(local.year, local.month, local.day, self.date)
    }

    /// Formats a UTC instant as a local time of day.
    pub fn format_time(&self, epoch_utc: u64) -> String {
        let local = crate::rtc::DateTime::from_epoch(self.zone().to_local(epoch_utc));
        units::format_time(local.hour, local.minute, local.second, self.clock)
    }

    /// Date then time, in that order whatever the format: it is the order
    /// that sorts, and the one every log of this repository already uses.
    pub fn format_datetime(&self, epoch_utc: u64) -> String {
        alloc::format!(
            "{} {}",
            self.format_date(epoch_utc),
            self.format_time(epoch_utc)
        )
    }

    pub fn format_integer(&self, value: i64) -> String {
        units::format_integer(value, self.thousands)
    }

    pub fn format_amount(&self, cents: i64) -> String {
        units::format_currency(cents, &self.currency, self.currency_position, self.thousands)
    }

    /// Distance given in millimetres, rendered in the chosen unit.
    pub fn format_distance(&self, millimetres: i64) -> String {
        let value = self.distance.from_millimetres(millimetres);
        alloc::format!(
            "{} {}",
            units::format_milli(value, 2, self.thousands),
            self.distance.symbol()
        )
    }

    /// Weight given in milligrams.
    pub fn format_weight(&self, milligrams: i64) -> String {
        let value = self.weight.from_milligrams(milligrams);
        alloc::format!(
            "{} {}",
            units::format_milli(value, 2, self.thousands),
            self.weight.symbol()
        )
    }

    /// Temperature given in milli-Celsius.
    pub fn format_temperature(&self, milli_celsius: i64) -> String {
        let value = self.temperature.from_milli_celsius(milli_celsius);
        alloc::format!(
            "{} {}",
            units::format_milli(value, 1, self.thousands),
            self.temperature.symbol()
        )
    }

    /// Speed given in millimetres per second.
    pub fn format_speed(&self, millimetres_per_second: i64) -> String {
        let value = self
            .speed
            .from_millimetres_per_second(millimetres_per_second);
        alloc::format!(
            "{} {}",
            units::format_milli(value, 1, self.thousands),
            self.speed.symbol()
        )
    }
}

// ============================================================
//  Active state
// ============================================================

struct State {
    settings: Settings,
    /// Catalogue of the main language.
    main: lang::Catalogue,
    /// Catalogue of the fallback language.
    fallback: lang::Catalogue,
    /// Account whose settings are loaded, if any.
    user: Option<String>,
    /// Is the secondary keyboard layout the active one?
    secondary_layout: bool,
}

static STATE: SpinLock<Option<State>> = SpinLock::new(None);

/// Snapshot of the active settings.
///
/// Returns a clone rather than a guard: a caller holding the lock while
/// formatting a string would hold it across an allocation, and the
/// allocator takes its own lock. That is the ordering the whole
/// repository avoids.
pub fn settings() -> Settings {
    STATE
        .lock()
        .as_ref()
        .map(|state| state.settings.clone())
        .unwrap_or_default()
}

/// Is the subsystem loaded?
pub fn is_ready() -> bool {
    STATE.lock().is_some()
}

/// The account whose settings are active.
pub fn current_user() -> Option<String> {
    STATE.lock().as_ref().and_then(|state| state.user.clone())
}

/// Translates a key, with `{name}` substitution.
///
/// Order: main catalogue, then fallback, then the key itself. Returning
/// the key is deliberate -- see `lang`.
pub fn translate(key: &str, variables: &[(&str, &str)]) -> String {
    let template = {
        let guard = STATE.lock();
        match guard.as_ref() {
            Some(state) => state
                .main
                .get(key)
                .or_else(|| state.fallback.get(key))
                .map(String::from),
            None => None,
        }
    };
    match template {
        Some(text) => lang::substitute(&text, variables),
        None => String::from(key),
    }
}

/// Is this key present in one of the two active catalogues?
pub fn has_key(key: &str) -> bool {
    let guard = STATE.lock();
    match guard.as_ref() {
        Some(state) => state.main.get(key).is_some() || state.fallback.get(key).is_some(),
        None => false,
    }
}

/// Sizes of the two catalogues.
pub fn catalogue_sizes() -> (usize, usize) {
    let guard = STATE.lock();
    match guard.as_ref() {
        Some(state) => (state.main.len(), state.fallback.len()),
        None => (0, 0),
    }
}

/// The keyboard layout in force, secondary included.
pub fn active_layout() -> Layout {
    let guard = STATE.lock();
    match guard.as_ref() {
        Some(state) if state.secondary_layout => state.settings.keyboard_secondary,
        Some(state) => state.settings.keyboard_primary,
        None => Layout::Azerty,
    }
}

/// Flips between primary and secondary. Returns the new layout.
///
/// Called by the keyboard driver when it sees the switch key. It does not
/// publish on `i18n.changed`: the setting has not changed, only which of
/// the two declared layouts is in use, and a subscriber reloading its
/// translations on every Alt press would be absurd.
pub fn toggle_layout() -> Layout {
    let mut guard = STATE.lock();
    match guard.as_mut() {
        Some(state) => {
            state.secondary_layout = !state.secondary_layout;
            if state.secondary_layout {
                state.settings.keyboard_secondary
            } else {
                state.settings.keyboard_primary
            }
        }
        None => Layout::Azerty,
    }
}

/// Does this scancode trigger the layout switch?
pub fn is_switch_key(scancode: u8, extended: bool) -> bool {
    let guard = STATE.lock();
    match guard.as_ref() {
        Some(state) => state
            .settings
            .keyboard_switch
            .triggered_by(scancode, extended),
        None => false,
    }
}

// ============================================================
//  Loading (14.1)
// ============================================================

/// Reads a configuration file, ignoring a missing one.
fn read_config(path: &str) -> Option<String> {
    let who = Credentials::kernel();
    let bytes = crate::fs::read_file(path, who).ok()?;
    String::from_utf8(bytes).ok()
}

/// Builds the settings by layering system, then user, then application.
///
/// This is 14.1 written out: each layer overrides the keys it declares
/// and leaves the others alone.
pub fn resolve(user: Option<&str>, app: Option<&str>) -> Settings {
    let mut settings = Settings::default();

    if let Some(text) = read_config(SYSTEM_CONFIG) {
        settings.merge(&text);
    }
    if let Some(name) = user
        && let Some(text) = read_config(&alloc::format!("/users/{}/{}", name, USER_CONFIG))
    {
        settings.merge(&text);
    }
    if let Some(id) = app
        && let Some(text) = read_config(&alloc::format!("/apps/{}/i18n/settings.cfg", id))
    {
        settings.merge(&text);
    }

    settings
}

/// Settings an application sees: the session ones with its own file over.
pub fn settings_for_app(app_id: &str) -> Settings {
    let mut settings = settings();
    if let Some(text) = read_config(&alloc::format!("/apps/{}/i18n/settings.cfg", app_id)) {
        settings.merge(&text);
    }
    settings
}

/// Loads the catalogue of a locale.
///
/// Every `.lang` of `/sys/i18n/{locale}/` is merged, in the order the
/// directory hands them back. Merging rather than picking one file: 14.3
/// puts the system translations in a directory, so a subsystem may drop
/// its own catalogue there without touching anybody else's.
pub fn load_catalogue(locale: &str) -> lang::Catalogue {
    let who = Credentials::kernel();
    let mut catalogue = lang::Catalogue::new(locale);
    let directory = alloc::format!("{}/{}", SYSTEM_I18N, locale);

    let Ok(entries) = crate::fs::list(&directory, who) else {
        return catalogue;
    };
    for entry in entries {
        if !entry.name.ends_with(".lang") {
            continue;
        }
        let path = alloc::format!("{}/{}", directory, entry.name);
        if let Ok(bytes) = crate::fs::read_file(&path, who)
            && let Ok(text) = String::from_utf8(bytes)
        {
            catalogue.merge(&text);
        }
    }
    catalogue
}

/// Loads the catalogue an application ships (14.3).
pub fn load_app_catalogue(app_id: &str, locale: &str) -> lang::Catalogue {
    let who = Credentials::kernel();
    let mut catalogue = lang::Catalogue::new(locale);
    let path = alloc::format!("/apps/{}/i18n/{}.lang", app_id, locale);
    if let Ok(bytes) = crate::fs::read_file(&path, who)
        && let Ok(text) = String::from_utf8(bytes)
    {
        catalogue.merge(&text);
    }
    catalogue
}

/// Re-reads everything and publishes the change (14.1).
///
/// Returns the number of subscribers reached. The publication happens
/// **outside** the lock: `ipc::publish` wakes the subscribers, and one of
/// them asking for the settings straight away would wait on a lock this
/// very call holds.
pub fn reload(user: Option<&str>) -> usize {
    let settings = resolve(user, None);
    let main = load_catalogue(&settings.language);
    let fallback = if settings.fallback == settings.language {
        lang::Catalogue::new(&settings.fallback)
    } else {
        load_catalogue(&settings.fallback)
    };

    {
        let mut guard = STATE.lock();
        *guard = Some(State {
            settings,
            main,
            fallback,
            user: user.map(String::from),
            // A reload restores the primary layout: the settings may have
            // renamed it, and staying on "the secondary" would then mean
            // a layout the user never chose.
            secondary_layout: false,
        });
    }

    RELOADS.fetch_add(1, Ordering::Relaxed);

    let reached = crate::ipc::publish_text(CHANGE_CHANNEL, 0, "i18n").unwrap_or(0);
    if reached > 0 {
        PUBLICATIONS.fetch_add(1, Ordering::Relaxed);
    }
    reached
}

/// Counters shown by `dump_state`.
pub fn counters() -> (u64, u64) {
    (
        RELOADS.load(Ordering::Relaxed),
        PUBLICATIONS.load(Ordering::Relaxed),
    )
}

// ============================================================
//  Installation of the tree
// ============================================================

/// The reference French catalogue.
///
/// Written here rather than shipped as a file for the same reason the GUI
/// modules are: nothing produces `/sys/i18n/` yet but the kernel. The keys
/// are the ones the login screen, the shell modules and the accessibility
/// reader actually ask for -- a catalogue of invented keys would prove
/// nothing.
const FR_LANG: &str = "\
# Nova OS - catalogue systeme francais
systeme.nom = Nova OS
login.titre = Connexion
login.utilisateur = Utilisateur
login.motdepasse = Mot de passe
login.erreur = Identifiants incorrects
login.verrouille = Compte verrouille pendant {secondes} secondes
login.bienvenue = Bienvenue {user}
shell.lanceur = Lanceur
shell.taches = Barre des taches
shell.notifications = Notifications
access.lecteur = Lecteur d'ecran
access.zoom = Zoom
access.contraste = Contraste eleve
access.clavier = Navigation clavier
access.actif = active
access.inactif = desactive
access.focus = {role} {label}
unite.distance = Distance
unite.poids = Poids
unite.temperature = Temperature
unite.vitesse = Vitesse
";

/// The English fallback catalogue.
const EN_LANG: &str = "\
# Nova OS - system catalogue, English
systeme.nom = Nova OS
login.titre = Sign in
login.utilisateur = User
login.motdepasse = Password
login.erreur = Wrong credentials
login.verrouille = Account locked for {secondes} seconds
login.bienvenue = Welcome {user}
shell.lanceur = Launcher
shell.taches = Task bar
shell.notifications = Notifications
access.lecteur = Screen reader
access.zoom = Zoom
access.contraste = High contrast
access.clavier = Keyboard navigation
access.actif = on
access.inactif = off
access.focus = {role} {label}
unite.distance = Distance
unite.poids = Weight
unite.temperature = Temperature
unite.vitesse = Speed
";

/// Lays down `/sys/i18n/` and the reference configuration.
///
/// Idempotent, like every other `install_layout` of the repository: a
/// second boot must find its files, not rewrite them, or a locale the
/// user chose would go back to French on every restart.
pub fn install_layout(now: u64) -> usize {
    if !crate::fs::is_ready() {
        return 0;
    }
    let who = Credentials::kernel();
    let mut created = 0;

    for path in [
        SYSTEM_I18N,
        "/sys/i18n/fr-FR",
        "/sys/i18n/en-US",
    ] {
        if crate::fs::stat(path, who).is_err()
            && crate::fs::create_directory(path, who, Mode::system_dir(), now).is_ok()
        {
            created += 1;
        }
    }

    for (path, content) in [
        ("/sys/i18n/fr-FR/systeme.lang", FR_LANG),
        ("/sys/i18n/en-US/systeme.lang", EN_LANG),
    ] {
        if crate::fs::stat(path, who).is_err()
            && crate::fs::write_file(path, content.as_bytes(), who, Mode::system_file(), now)
                .is_ok()
        {
            created += 1;
        }
    }

    if crate::fs::stat(SYSTEM_CONFIG, who).is_err() {
        let content = Settings::default().to_cfg();
        if crate::fs::write_file(
            SYSTEM_CONFIG,
            content.as_bytes(),
            who,
            Mode::system_file(),
            now,
        )
        .is_ok()
        {
            created += 1;
        }
    }

    created
}

/// Starts phase 10.2.
///
/// Called by `init.dsa` **before** the login screen: 16.4 wants
/// accessibility working from the login screen, and the login screen
/// wants its strings.
pub fn start(now: u64) {
    let created = install_layout(now);
    let reached = reload(None);
    let settings = settings();
    let (main, fallback) = catalogue_sizes();

    crate::serial_println!(
        "[i18n] {} entree(s) creee(s) - langue {} (repli {}), {} + {} cle(s), {} abonne(s)",
        created,
        settings.language,
        settings.fallback,
        main,
        fallback,
        reached
    );
}

/// Sets an account's time zone (the "Date et heure" tab of the settings)
/// and, if the account is the session's, applies it at once.
///
/// Only the `fuseau` line of the account's file changes: the file holds
/// the account's overrides of the system layer, and rewriting it from a
/// full `Settings` would turn every inherited value into an override.
pub fn set_user_timezone(user: &str, id: &str) -> Result<(), &'static str> {
    if tz::find(id).is_none() {
        return Err("fuseau horaire inconnu");
    }
    let path = alloc::format!("/users/{}/{}", user, USER_CONFIG);
    let who = Credentials::kernel();
    let old = read_config(&path).unwrap_or_default();
    let mut text = String::new();
    for line in old.lines() {
        let key = line.split_once('=').map(|(key, _)| key.trim()).unwrap_or("");
        if key != "fuseau" && key != "timezone" {
            text.push_str(line);
            text.push('\n');
        }
    }
    text.push_str(&alloc::format!("fuseau = {}\n", id));
    let now = crate::idt::ticks();
    let _ = crate::fs::create_directory(&alloc::format!("/users/{}/settings", user), who, Mode::private_dir(), now);
    crate::fs::write_file(&path, text.as_bytes(), who, Mode::private_file(), now).map_err(|error| error.message())?;
    if current_user().as_deref() == Some(user) {
        reload(Some(user));
    }
    Ok(())
}

/// Reloads for a given account, at session opening.
pub fn adopt_user(user: &str) {
    let reached = reload(Some(user));
    let settings = settings();
    crate::serial_println!(
        "[i18n] session {} - langue {}, fuseau {} ({}), clavier {}, {} abonne(s) prevenu(s)",
        user,
        settings.language,
        settings.timezone,
        settings.zone().offset_text(),
        settings.keyboard_primary.as_str(),
        reached
    );
}

pub fn dump_state() {
    let settings = settings();
    let (main, fallback) = catalogue_sizes();
    let (reloads, publications) = counters();

    crate::serial_println!(
        "[i18n] langue {} / repli {} - {} + {} cle(s)",
        settings.language,
        settings.fallback,
        main,
        fallback
    );
    crate::serial_println!(
        "[i18n] date {} - heure {} - milliers {} - monnaie {} {}",
        settings.date.as_str(),
        settings.clock.as_str(),
        settings.thousands.as_str(),
        settings.currency,
        settings.currency_position.as_str()
    );
    crate::serial_println!(
        "[i18n] unites : {} / {} / {} / {}",
        settings.distance.as_str(),
        settings.weight.as_str(),
        settings.temperature.as_str(),
        settings.speed.as_str()
    );
    crate::serial_println!(
        "[i18n] clavier {} + {} (bascule {}) - actif {}",
        settings.keyboard_primary.as_str(),
        settings.keyboard_secondary.as_str(),
        settings.keyboard_switch.as_str(),
        active_layout().as_str()
    );
    crate::serial_println!(
        "[i18n] fuseau {} {} - {} rechargement(s), {} publication(s)",
        settings.timezone,
        settings.zone().offset_text(),
        reloads,
        publications
    );
}

/// Locales that have a catalogue on disk.
pub fn available_locales() -> Vec<String> {
    let who = Credentials::kernel();
    let mut locales = Vec::new();
    if let Ok(entries) = crate::fs::list(SYSTEM_I18N, who) {
        for entry in entries {
            locales.push(entry.name);
        }
    }
    locales
}

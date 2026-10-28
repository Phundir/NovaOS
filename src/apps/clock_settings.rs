//! `horloge.ss` -- the "Date et heure" tab of `parametres.ss`.
//!
//! Two decisions and what follows from them:
//!
//! * **the time zone**, per account: it is a display preference (14.2:
//!   dates are stored in UTC and converted at display), kept with the
//!   locale in the account's `i18n.cfg` and applied at once to the
//!   session -- the taskbar clock, the file dates, and the Linux
//!   programs' `/etc/localtime` (the Web Browser);
//! * **the network synchronisation**, for the machine: there is one
//!   clock, whoever is logged in. On, the `net::sntp` service sets it at
//!   boot and every six hours; the button asks for it now.
//!
//! The date itself cannot be typed in. With the synchronisation on it
//! would be overwritten within hours, and with it off the CMOS already
//! holds whatever the machine's owner set in its firmware.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::binfmt::identity::Permissions;
use crate::config::{self, Choice, Kind, Provider, Setting, Value};
use crate::i18n::tz;
use crate::net::sntp;

pub const APP_ID: &str = "os.nova.horloge";
pub const FILE_NAME: &str = "horloge.ss";
pub const BINARY: &str = "/sysapp/horloge.ss";

pub struct ClockTab;

pub static TAB: ClockTab = ClockTab;

/// The account's zone as the settings resolve it: the session's when
/// the account is the session's, its files otherwise.
fn zone_of(user: &str) -> &'static tz::Zone {
    let settings = crate::i18n::resolve(Some(user), None);
    tz::find(&settings.timezone).unwrap_or_else(tz::default_zone)
}

fn now_text(user: &str) -> String {
    let zone = zone_of(user);
    match crate::rtc::now() {
        Some(epoch) => {
            let local = crate::rtc::DateTime::from_epoch(zone.to_local(epoch)).format();
            format!("{} - {}", core::str::from_utf8(&local).unwrap_or("?"), zone.offset_text())
        }
        None => String::from("horloge indisponible"),
    }
}

fn sync_text() -> String {
    if !sntp::enabled() {
        return String::from("desactivee - l'heure est celle de l'horloge CMOS");
    }
    match (sntp::last(), sntp::last_error()) {
        (Some(report), None) => {
            let date = crate::rtc::DateTime::from_epoch(report.epoch_ms / 1000).format();
            format!(
                "reglee sur {} a {} UTC (correction {} ms)",
                report.server,
                core::str::from_utf8(&date).unwrap_or("?"),
                report.correction_ms
            )
        }
        (_, Some(reason)) => format!("echec : {} - nouvel essai dans une minute", reason),
        (None, None) => String::from("en attente du reseau"),
    }
}

impl Provider for ClockTab {
    fn file(&self) -> &'static str {
        FILE_NAME
    }

    fn tab(&self) -> &'static str {
        "Date et heure"
    }

    fn settings(&self, _user: &str) -> Vec<Setting> {
        let zones: Vec<Choice> = tz::ZONES
            .iter()
            .map(|zone| Choice::new(zone.id, &format!("{} - {}", zone.offset_text(), zone.label)))
            .collect();
        vec![
            Setting::new("heure", "Date et heure", "Heure locale du fuseau choisi", Kind::Info),
            Setting::new(
                "fuseau",
                "Fuseau horaire",
                "Heure affichee par Nova et par les programmes (navigateur compris)",
                Kind::Select(zones),
            ),
            Setting::new(
                "synchro",
                "Regler l'heure par Internet",
                "Au demarrage puis toutes les six heures, si le reseau est la",
                Kind::Toggle,
            ),
            Setting::new("etat", "Derniere synchronisation", "", Kind::Info),
            Setting::new("maintenant", "Synchroniser", "Demande l'heure a un serveur de temps", Kind::Action("Synchroniser maintenant")),
        ]
    }

    fn get(&self, user: &str, key: &str) -> Value {
        match key {
            "heure" => Value::Text(now_text(user)),
            "fuseau" => Value::Text(String::from(zone_of(user).id)),
            "synchro" => Value::Bool(sntp::enabled()),
            "etat" => Value::Text(sync_text()),
            _ => Value::None,
        }
    }

    fn set(&self, user: &str, key: &str, value: &Value) -> Result<(), String> {
        match (key, value) {
            ("fuseau", Value::Text(id)) => crate::i18n::set_user_timezone(user, id).map_err(String::from),
            ("synchro", Value::Bool(on)) => {
                sntp::set_enabled(*on).map_err(|error| format!("reglage non enregistre : {}", error.message()))
            }
            _ => Err(String::from("valeur refusee")),
        }
    }

    fn act(&self, _user: &str, key: &str) -> Result<String, String> {
        match key {
            "maintenant" => {
                if !crate::net::interface().configured() {
                    return Err(String::from("pas de connexion reseau"));
                }
                // The service does it: a DNS query and an NTP exchange
                // wait for answers, and the settings window must not
                // freeze meanwhile.
                sntp::request();
                Ok(String::from("demande envoyee - l'heure se met a jour dans quelques secondes"))
            }
            _ => Err(String::from("aucune action")),
        }
    }
}

/// Registers the tab. Called at boot.
pub fn register() {
    config::register(&TAB);
}

/// `horloge.ss` from the menu: the settings, opened on this tab.
pub fn task() -> ! {
    super::parametres::run_on(Some(FILE_NAME))
}

pub fn install(now: u64) -> bool {
    super::install_binary(
        &super::BinarySpec {
            path: BINARY,
            title: FILE_NAME,
            app_id: APP_ID,
            display_name: "Date et heure",
            format: crate::binfmt::header::BinaryFormat::Ss,
            permissions: Permissions::NETWORK_INTERNET,
        },
        now,
    )
}

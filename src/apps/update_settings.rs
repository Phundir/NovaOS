//! `mises-a-jour.ss` -- the "Mises a jour" tab of `parametres.ss` (15.6.2).
//!
//! What the user decides about the updates of the applications (spec
//! 10.5) and of the system (11.2):
//!
//! * the **channel** followed by default -- `release`, `beta`, `test` --
//!   for every application without a channel of its own (an application
//!   keeps its own, set by `spm canal` or `spm install --canal`);
//! * whether the **automatic** checks run (`updater.dsa`, every 24 h);
//! * what is installed: the versioned applications, their active and
//!   previous versions, and the registries declared.
//!
//! The settings are the machine's, whoever is logged in: one set of
//! applications is installed for everybody, and only an administrator
//! changes them (the tab says so rather than failing silently).
//!
//! "Verifier maintenant" hands the check to a task of its own: manifests
//! come from the network, and the settings window must not freeze while
//! they do.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::binfmt::identity::Permissions;
use crate::config::{self, Choice, Kind, Provider, Setting, Value};
use crate::spm::{registry, update, versions};

pub const APP_ID: &str = "os.nova.mises-a-jour";
pub const FILE_NAME: &str = "mises-a-jour.ss";
pub const BINARY: &str = "/sysapp/mises-a-jour.ss";

pub struct UpdateTab;

pub static TAB: UpdateTab = UpdateTab;

/// Applications offered by the last check started from the tab.
static LAST_FOUND: AtomicU64 = AtomicU64::new(u64::MAX);

fn is_admin(user: &str) -> bool {
    crate::fs::credentials_of(user).is_some_and(|who| {
        matches!(who.role, crate::fs::perm::Role::Root | crate::fs::perm::Role::Admin)
    })
}

fn installed_text() -> String {
    let roots = versions::all_roots();
    if roots.is_empty() {
        return String::from("aucune application installee par un registre");
    }
    let mut parts = Vec::new();
    for root in roots.iter().take(4) {
        let name = root.rsplit('/').next().unwrap_or(root);
        let active = versions::active(root).unwrap_or_default();
        match versions::previous(root) {
            Some(previous) => parts.push(format!("{} {} (precedente {})", name, active, previous)),
            None => parts.push(format!("{} {}", name, active)),
        }
    }
    if roots.len() > 4 {
        parts.push(format!("et {} autre(s)", roots.len() - 4));
    }
    parts.join(", ")
}

fn registries_text() -> String {
    let registries = registry::load();
    match registries.len() {
        0 => String::from("aucun registre declare (spm registry add)"),
        _ => registries.iter().map(|r| format!("{} ({})", r.name, r.url)).collect::<Vec<_>>().join(", "),
    }
}

fn check_task() -> ! {
    let found = update::check_applications(crate::idt::ticks());
    LAST_FOUND.store(found as u64, Ordering::Relaxed);
    crate::scheduler::exit_current_with(0)
}

impl Provider for UpdateTab {
    fn file(&self) -> &'static str {
        FILE_NAME
    }

    fn tab(&self) -> &'static str {
        "Mises a jour"
    }

    fn settings(&self, _user: &str) -> Vec<Setting> {
        let channels = vec![
            Choice::new("release", "release - versions publiees"),
            Choice::new("beta", "beta - versions d'essai"),
            Choice::new("test", "test - versions de developpement"),
        ];
        vec![
            Setting::new(
                "canal",
                "Canal par defaut",
                "Suivi par les applications qui n'ont pas de canal a elles",
                Kind::Select(channels),
            ),
            Setting::new(
                "auto",
                "Verifier automatiquement",
                "Le systeme et les applications, toutes les 24 heures",
                Kind::Toggle,
            ),
            Setting::new("applications", "Applications", "Version active et version de retour", Kind::Info),
            Setting::new("registres", "Registres", "Ou les manifestes sont lus", Kind::Info),
            Setting::new("etat", "Derniere verification", "", Kind::Info),
            Setting::new(
                "verifier",
                "Verifier",
                "Lit les manifestes des registres maintenant",
                Kind::Action("Verifier maintenant"),
            ),
        ]
    }

    fn get(&self, _user: &str, key: &str) -> Value {
        match key {
            "canal" => Value::Text(registry::channel_of("defaut")),
            "auto" => Value::Bool(update::config(crate::idt::ticks()).automatic),
            "applications" => Value::Text(installed_text()),
            "registres" => Value::Text(registries_text()),
            "etat" => Value::Text(match LAST_FOUND.load(Ordering::Relaxed) {
                u64::MAX => String::from("aucune depuis l'ouverture de session"),
                0 => String::from("tout est a jour"),
                found => format!("{} mise(s) a jour proposee(s) - voir les notifications", found),
            }),
            _ => Value::None,
        }
    }

    fn set(&self, user: &str, key: &str, value: &Value) -> Result<(), String> {
        if !is_admin(user) {
            return Err(String::from("reserve a un administrateur (spec 5.2)"));
        }
        let now = crate::idt::ticks();
        match (key, value) {
            ("canal", Value::Text(channel)) => {
                if registry::set_channel("defaut", channel, now) {
                    Ok(())
                } else {
                    Err(String::from("canal inconnu"))
                }
            }
            ("auto", Value::Bool(on)) => update::set_automatic(*on, now).map_err(|_| String::from("reglage non enregistre")),
            _ => Err(String::from("valeur refusee")),
        }
    }

    fn act(&self, _user: &str, key: &str) -> Result<String, String> {
        match key {
            "verifier" => {
                if registry::load().is_empty() {
                    return Err(String::from("aucun registre declare"));
                }
                crate::scheduler::spawn("maj-verification", crate::process::BinaryType::Dsa, check_task);
                Ok(String::from("verification lancee - les notifications diront ce qui est propose"))
            }
            _ => Err(String::from("aucune action")),
        }
    }
}

/// Registers the tab. Called at boot.
pub fn register() {
    config::register(&TAB);
}

/// `mises-a-jour.ss` from the menu: the settings, opened on this tab.
pub fn task() -> ! {
    super::parametres::run_on(Some(FILE_NAME))
}

pub fn install(now: u64) -> bool {
    super::install_binary(
        &super::BinarySpec {
            path: BINARY,
            title: FILE_NAME,
            app_id: APP_ID,
            display_name: "Mises a jour",
            format: crate::binfmt::header::BinaryFormat::Ss,
            permissions: Permissions::NETWORK_INTERNET,
        },
        now,
    )
}

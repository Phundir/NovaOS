//! The "Explorateur" tab of `parametres.ss`.
//!
//! Stored in `config::store` under `explorer.ss`, read by the explorer
//! when a window opens: a change applies to the next window, not to one
//! already open -- which the help line says, rather than letting a user
//! wonder why the window in front of them did not move.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::config::{self, Choice, Kind, Provider, Setting, Value, store};

use super::ops::SortKey;

const FILE: &str = "explorer.ss";

pub struct ExplorerTab;

pub static TAB: ExplorerTab = ExplorerTab;

/// Where a new window opens, for an account.
pub fn start_folder(user: &str) -> String {
    let home = alloc::format!("/users/{}/home", user);
    match store::get(user, FILE, "depart", "accueil").as_str() {
        "documents" => alloc::format!("{}/documents", home),
        "telechargements" => alloc::format!("{}/downloads", home),
        "racine" => String::from("/"),
        "perso" => store::get(user, FILE, "depart_perso", &home),
        _ => home,
    }
}

/// The sort of a new window.
pub fn default_sort(user: &str) -> SortKey {
    match store::get(user, FILE, "tri", "nom").as_str() {
        "type" => SortKey::Kind,
        "taille" => SortKey::Size,
        _ => SortKey::Name,
    }
}

/// Does a deletion ask first?
pub fn confirms_delete(user: &str) -> bool {
    store::get(user, FILE, "confirmer_suppression", "oui") != "non"
}

fn flag(value: bool) -> &'static str {
    if value { "oui" } else { "non" }
}

impl Provider for ExplorerTab {
    fn file(&self) -> &'static str {
        FILE
    }

    fn tab(&self) -> &'static str {
        "Explorateur"
    }

    fn settings(&self, _user: &str) -> Vec<Setting> {
        vec![
            Setting::new(
                "depart",
                "Dossier a l'ouverture",
                "Ou s'ouvre une nouvelle fenetre de l'explorateur",
                Kind::Select(vec![
                    Choice::new("accueil", "Accueil"),
                    Choice::new("documents", "Documents"),
                    Choice::new("telechargements", "Telechargements"),
                    Choice::new("racine", "Racine /"),
                    Choice::new("perso", "Dossier ci-dessous"),
                ]),
            ),
            Setting::new(
                "depart_perso",
                "Dossier personnalise",
                "Chemin absolu, utilise quand le choix ci-dessus est \"Dossier ci-dessous\"",
                Kind::Field { max: 120 },
            ),
            Setting::new(
                "tri",
                "Tri par defaut",
                "Ordre d'une nouvelle fenetre ; les dossiers restent en tete",
                Kind::Select(vec![
                    Choice::new("nom", "Nom"),
                    Choice::new("type", "Type"),
                    Choice::new("taille", "Taille"),
                ]),
            ),
            Setting::new(
                "confirmer_suppression",
                "Confirmer les suppressions",
                "Sans confirmation, Suppr efface tout de suite - y compris un dossier et son contenu",
                Kind::Toggle,
            ),
            Setting::new(
                "note",
                "",
                "",
                Kind::Info,
            ),
        ]
    }

    fn get(&self, user: &str, key: &str) -> Value {
        match key {
            "depart" => Value::Text(store::get(user, FILE, "depart", "accueil")),
            "depart_perso" => Value::Text(store::get(
                user,
                FILE,
                "depart_perso",
                &alloc::format!("/users/{}/home", user),
            )),
            "tri" => Value::Text(store::get(user, FILE, "tri", "nom")),
            "confirmer_suppression" => Value::Bool(confirms_delete(user)),
            "note" => Value::Text(String::from(
                "Ces reglages s'appliquent aux fenetres ouvertes ensuite.",
            )),
            _ => Value::None,
        }
    }

    fn set(&self, user: &str, key: &str, value: &Value) -> Result<(), String> {
        let text = match (key, value) {
            ("depart", Value::Text(choice))
                if ["accueil", "documents", "telechargements", "racine", "perso"]
                    .contains(&choice.as_str()) =>
            {
                choice.clone()
            }
            ("tri", Value::Text(choice)) if ["nom", "type", "taille"].contains(&choice.as_str()) => {
                choice.clone()
            }
            ("confirmer_suppression", Value::Bool(on)) => String::from(flag(*on)),
            ("depart_perso", Value::Text(path)) => {
                let path = path.trim();
                let Some(canonical) = super::ops::canonical(path) else {
                    return Err(String::from("chemin absolu attendu (/... ou A:/...)"));
                };
                // Checked with the account's rights: a folder it cannot
                // open would give a window that opens on an error.
                let who = crate::fs::credentials_of(user).ok_or("compte introuvable")?;
                match crate::fs::stat(&canonical, who) {
                    Ok(node) if node.kind == crate::fs::format::NodeKind::Directory => canonical,
                    Ok(_) => return Err(alloc::format!("{} n'est pas un dossier", canonical)),
                    Err(error) => return Err(alloc::format!("{} : {}", canonical, error.message())),
                }
            }
            _ => return Err(String::from("valeur refusee")),
        };
        store::set(user, FILE, key, &text).map_err(|error| String::from(error.message()))
    }
}

/// Registers the tab. Called at boot.
pub fn register() {
    config::register(&TAB);
}

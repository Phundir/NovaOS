//! Self-test of the settings framework.
//!
//! The settings application draws what the providers declare and trusts
//! nothing else, so what is checked is the **contract** every provider
//! must keep: unique keys, and a value of the type its kind calls for --
//! a toggle answering a number would be drawn as "off" forever, and a
//! select whose value is none of its choices would show a blank box.

use alloc::string::String;
use alloc::vec::Vec;

use super::{Choice, Kind, Provider, Setting, Value, store};

struct Count {
    passed: usize,
    failed: usize,
}

impl Count {
    fn check(&mut self, label: &str, condition: bool) {
        if condition {
            self.passed += 1;
        } else {
            self.failed += 1;
            crate::serial_println!("[config] ECHEC : {}", label);
        }
    }
}

/// An account that does not exist: every provider must answer its
/// defaults for it rather than fail.
const NOBODY: &str = "autotest-inexistant";

struct NotSs;

impl Provider for NotSs {
    fn file(&self) -> &'static str {
        "intrus.cus"
    }
    fn tab(&self) -> &'static str {
        "Intrus"
    }
    fn settings(&self, _user: &str) -> Vec<Setting> {
        Vec::new()
    }
    fn get(&self, _user: &str, _key: &str) -> Value {
        Value::None
    }
    fn set(&self, _user: &str, _key: &str, _value: &Value) -> Result<(), String> {
        Ok(())
    }
}

static NOT_SS: NotSs = NotSs;

fn fits(kind: &Kind, value: &Value) -> bool {
    match (kind, value) {
        (Kind::Toggle, Value::Bool(_)) => true,
        (Kind::Select(choices), Value::Text(text)) => {
            choices.iter().any(|choice: &Choice| choice.value == *text)
        }
        (Kind::Field { max }, Value::Text(text)) => text.len() <= *max,
        (Kind::Slider { min, max, .. }, Value::Number(number)) => (*min..=*max).contains(number),
        (Kind::Info, Value::Text(_)) => true,
        (Kind::Level | Kind::Action(_), _) => true,
        _ => false,
    }
}

pub fn run() -> (usize, usize) {
    let mut count = Count {
        passed: 0,
        failed: 0,
    };

    count.check("un fournisseur qui n'est pas un .ss est refuse", !super::register(&NOT_SS));
    let providers = super::providers();
    count.check("au moins trois onglets sont inscrits", providers.len() >= 3);
    count.check(
        "seuls des .ss sont inscrits",
        providers.iter().all(|provider| provider.file().ends_with(".ss")),
    );
    let mut files: Vec<&str> = providers.iter().map(|provider| provider.file()).collect();
    files.sort();
    files.dedup();
    count.check("un .ss n'a qu'un onglet", files.len() == providers.len());

    for provider in providers.iter() {
        let settings = provider.settings(NOBODY);
        let mut keys: Vec<&str> = settings.iter().map(|setting| setting.key).collect();
        keys.sort();
        keys.dedup();
        count.check(
            &alloc::format!("{} : cles uniques", provider.file()),
            keys.len() == settings.len() && !settings.is_empty(),
        );
        let wrong: Vec<&str> = settings
            .iter()
            .filter(|setting| !fits(&setting.kind, &provider.get(NOBODY, setting.key)))
            .map(|setting| setting.key)
            .collect();
        if !wrong.is_empty() {
            crate::serial_println!("[config]   {} : valeurs hors de leur genre : {:?}", provider.file(), wrong);
        }
        count.check(
            &alloc::format!("{} : chaque valeur a le type de son controle", provider.file()),
            wrong.is_empty(),
        );
    }

    let pairs = alloc::vec![
        (String::from("depart"), String::from("documents")),
        (String::from("tri"), String::from("taille")),
    ];
    count.check(
        "un fichier de reglages survit a sa relecture",
        store::parse(&store::serialize(&pairs)) == pairs,
    );
    count.check(
        "une ligne sans egal est ignoree",
        store::parse("# note\nsans egal\ncle = valeur\n")
            == alloc::vec![(String::from("cle"), String::from("valeur"))],
    );

    (count.passed, count.failed)
}

//! Settings exposed by the `.ss` components, for `parametres.ss`.
//!
//! ## The settings application contains no setting
//!
//! Every `.ss` that has something to configure registers a `Provider`:
//! the name of its tab, the list of its settings -- each with a kind that
//! says how it is edited -- and how to read and write them. The settings
//! application knows none of them: it lists the providers, draws the
//! controls their kinds call for, and hands the values back. Adding a tab
//! is adding a provider; `parametres.ss` does not change.
//!
//! ## Only `.ss`
//!
//! 3.1 makes `.ss` the system software -- the explorer, the compositor,
//! the task manager. `register` refuses a provider whose file is not a
//! `.ss`: an ordinary application configures itself in its own window,
//! and a panel where any `.cus` could add a tab would be a panel any
//! `.cus` could fill with look-alikes of system settings.
//!
//! ## Why providers are native code
//!
//! A provider must **apply** a change -- the mixer, the codec, the
//! compositor -- not only store it, and the components it speaks for run
//! in the kernel image. A `.ss` loaded from disk would describe its
//! settings in a file next to its binary and receive changes by IPC; the
//! day such a binary exists, it gets a provider that reads that file, and
//! nothing here changes. Said so that nobody believes it exists already.
//!
//! ## Life of a value
//!
//! Per account, like every preference of the system (8.4, 14.1, 16): a
//! provider stores what it likes where it likes -- the audio keeps its
//! `audio.cfg` -- and `store` offers a `key = value` file for those with
//! nothing of their own. At log in, `adopt_user` gives each provider the
//! account; at log out, `reset` gives the defaults back.

pub mod selftest;
pub mod store;

use alloc::string::String;
use alloc::vec::Vec;

use crate::vga::SpinLock;

/// One option of a `Select`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub value: String,
    pub label: String,
}

impl Choice {
    pub fn new(value: &str, label: &str) -> Self {
        Choice {
            value: String::from(value),
            label: String::from(label),
        }
    }
}

/// How a setting is edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// On or off. Value: `Bool`.
    Toggle,
    /// One of a list. Value: `Text`, the `value` of a choice.
    Select(Vec<Choice>),
    /// Free text, checked by the provider. Value: `Text`.
    Field { max: usize },
    /// A number in a range. Value: `Number`.
    Slider {
        min: i64,
        max: i64,
        step: i64,
        unit: &'static str,
    },
    /// A live level, read-only, 0..=32767: `Provider::level`.
    Level,
    /// A button: `Provider::act`. The text is its label.
    Action(&'static str),
    /// A line of information, read-only. Value: `Text`.
    Info,
}

/// A value, as a provider gives and takes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Bool(bool),
    Number(i64),
    Text(String),
    None,
}

impl Value {
    pub fn as_bool(&self) -> bool {
        matches!(self, Value::Bool(true))
    }

    pub fn as_number(&self) -> i64 {
        match self {
            Value::Number(number) => *number,
            _ => 0,
        }
    }

    pub fn as_text(&self) -> &str {
        match self {
            Value::Text(text) => text,
            _ => "",
        }
    }
}

/// One setting of a tab.
#[derive(Debug, Clone)]
pub struct Setting {
    pub key: &'static str,
    pub label: String,
    /// One line shown when the pointer or the focus is on the setting.
    pub help: String,
    pub kind: Kind,
}

impl Setting {
    pub fn new(key: &'static str, label: &str, help: &str, kind: Kind) -> Self {
        Setting {
            key,
            label: String::from(label),
            help: String::from(help),
            kind,
        }
    }
}

/// What a `.ss` implements to have a tab.
///
/// Every method receives the account the change is made for: a setting
/// belongs to an account, and a provider must not guess whose.
pub trait Provider: Sync {
    /// The `.ss` file this tab belongs to, as in `/sysapp`.
    fn file(&self) -> &'static str;
    /// Name of the tab.
    fn tab(&self) -> &'static str;
    /// The settings, in display order. Asked again when the tab opens and
    /// then about once a second, so that a list can follow the hardware.
    fn settings(&self, user: &str) -> Vec<Setting>;
    fn get(&self, user: &str, key: &str) -> Value;
    /// Applies and stores a value. The error is shown as it is.
    fn set(&self, user: &str, key: &str, value: &Value) -> Result<(), String>;
    /// Applies a value **without storing it**: a slider being dragged.
    /// `set` follows when it is let go. The default does nothing, and the
    /// value then applies on release only.
    fn preview(&self, _user: &str, _key: &str, _value: &Value) {}
    /// Current level of a `Level` setting.
    fn level(&self, _key: &str) -> u16 {
        0
    }
    /// Runs an `Action`. The text is shown in the status line.
    fn act(&self, _user: &str, _key: &str) -> Result<String, String> {
        Err(String::from("aucune action"))
    }
    /// The tab is shown: start whatever its live values need. A text
    /// returned is shown as a warning.
    fn shown(&self, _user: &str) -> Option<String> {
        None
    }
    /// The tab is hidden or the settings window closed.
    fn hidden(&self) {}
    /// Called every tick while the tab is shown.
    fn tick(&self) {}
    /// A session opens: apply this account's values.
    fn adopt(&self, _user: &str) {}
    /// The session closed: give the defaults back.
    fn reset(&self) {}
}

static PROVIDERS: SpinLock<Vec<&'static dyn Provider>> = SpinLock::new(Vec::new());

/// Registers a tab. Refuses a file that is not a `.ss`, and a second
/// provider for the same file -- a registration replayed at every boot
/// must not give two tabs.
pub fn register(provider: &'static dyn Provider) -> bool {
    if !provider.file().ends_with(".ss") {
        crate::serial_println!(
            "[config] REFUS : \"{}\" n'est pas un .ss (3.1)",
            provider.file()
        );
        return false;
    }
    let mut providers = PROVIDERS.lock();
    if providers.iter().any(|existing| existing.file() == provider.file()) {
        return false;
    }
    providers.push(provider);
    true
}

/// The tabs, in registration order.
pub fn providers() -> Vec<&'static dyn Provider> {
    PROVIDERS.lock().clone()
}

/// A session opens. Called with no lock held.
pub fn adopt_user(user: &str) {
    for provider in providers() {
        provider.adopt(user);
    }
}

/// The session closed.
pub fn reset() {
    for provider in providers() {
        provider.reset();
    }
}

pub fn dump_state() {
    let providers = providers();
    crate::serial_println!("[config] {} onglet(s) de parametres", providers.len());
    for provider in providers {
        crate::serial_println!("[config]   {:<18} {}", provider.file(), provider.tab());
    }
}

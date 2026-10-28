//! The "Affichage" tab of `parametres.ss`, exposed by `compositor.ss`.
//!
//! Two settings, and a line of information:
//!
//! * **follow the host window** -- what `follow_host_size` does since the
//!   screen became virtio-gpu's. Off, the size stays what it is when the
//!   window is resized, and the host scales or crops the image;
//! * **fixed size**, from a list. Chosen, it turns the following off --
//!   otherwise the next host event would undo it a fifth of a second later.
//!
//! The change is not made here: `set` runs on the settings window's task,
//! and a mode switch must run on the compositor's, lock-free, followed by
//! the resize of every client (`follow_host_size`). So `set` leaves a
//! request, and the compositor takes it on its next poll.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::config::{self, Choice, Kind, Provider, Setting, Value, store};
use crate::vga::SpinLock;

const FILE: &str = "compositor.ss";

/// Sizes offered. The common ones of laptops and monitors, within what
/// `virtio_gpu` accepts.
const SIZES: [(u32, u32); 7] = [
    (1024, 768),
    (1280, 720),
    (1280, 800),
    (1366, 768),
    (1600, 900),
    (1920, 1080),
    (2560, 1440),
];

/// Does the screen follow the host window?
static FOLLOW_HOST: AtomicBool = AtomicBool::new(true);

/// A size asked for from the settings, for the compositor to apply.
static REQUESTED: SpinLock<Option<(u32, u32)>> = SpinLock::new(None);

pub fn follows_host() -> bool {
    FOLLOW_HOST.load(Ordering::Relaxed)
}

/// Takes the size the settings asked for, if any.
pub fn take_request() -> Option<(u32, u32)> {
    REQUESTED.lock().take()
}

fn size_value(width: u32, height: u32) -> String {
    format!("{}x{}", width, height)
}

fn parse_size(text: &str) -> Option<(u32, u32)> {
    let (width, height) = text.split_once('x')?;
    Some((width.parse().ok()?, height.parse().ok()?))
}

pub struct DisplayTab;

pub static TAB: DisplayTab = DisplayTab;

impl Provider for DisplayTab {
    fn file(&self) -> &'static str {
        FILE
    }

    fn tab(&self) -> &'static str {
        "Affichage"
    }

    fn settings(&self, _user: &str) -> Vec<Setting> {
        let virtio = crate::graphics::virtio_gpu::is_active();
        let mut settings = vec![Setting::new("ecran", "Ecran", "", Kind::Info)];
        if virtio {
            settings.push(Setting::new(
                "suivre_hote",
                "Suivre la fenetre de l'hote",
                "L'ecran prend la taille de la fenetre QEMU, ou du moniteur en plein ecran",
                Kind::Toggle,
            ));
            // The current size heads the list when it is not one of the
            // offered ones -- a host window dragged to 1311x877 -- so that
            // the list always shows what the screen is.
            let (width, height) = crate::graphics::resolution()
                .map(|(w, h)| (w as u32, h as u32))
                .unwrap_or((0, 0));
            let mut choices: Vec<Choice> = Vec::new();
            if !SIZES.contains(&(width, height)) {
                let value = size_value(width, height);
                choices.push(Choice::new(&value, &format!("{} (actuelle)", value)));
            }
            choices.extend(SIZES.iter().map(|(w, h)| {
                let value = size_value(*w, *h);
                Choice::new(&value, &value)
            }));
            settings.push(Setting::new(
                "taille",
                "Taille fixe",
                "Choisir une taille desactive le suivi de la fenetre",
                Kind::Select(choices),
            ));
        }
        settings
    }

    fn get(&self, _user: &str, key: &str) -> Value {
        let (width, height) = crate::graphics::resolution()
            .map(|(w, h)| (w as u32, h as u32))
            .unwrap_or((0, 0));
        match key {
            "ecran" => Value::Text(if crate::graphics::virtio_gpu::is_active() {
                format!("{}x{} - virtio-gpu, taille variable", width, height)
            } else if width != 0 {
                format!("{}x{} - mode fixe de GRUB (VBE), non modifiable", width, height)
            } else {
                String::from("aucun ecran graphique")
            }),
            "suivre_hote" => Value::Bool(follows_host()),
            "taille" => Value::Text(size_value(width, height)),
            _ => Value::None,
        }
    }

    fn set(&self, user: &str, key: &str, value: &Value) -> Result<(), String> {
        if !crate::graphics::virtio_gpu::is_active() {
            return Err(String::from("taille fixe : pas de virtio-gpu"));
        }
        match (key, value) {
            ("suivre_hote", Value::Bool(on)) => {
                FOLLOW_HOST.store(*on, Ordering::Relaxed);
                store::set(user, FILE, "suivre_hote", if *on { "oui" } else { "non" })
                    .map_err(|error| String::from(error.message()))
            }
            ("taille", Value::Text(text)) => {
                let size = parse_size(text).ok_or("taille illisible")?;
                FOLLOW_HOST.store(false, Ordering::Relaxed);
                *REQUESTED.lock() = Some(size);
                store::set(user, FILE, "suivre_hote", "non")
                    .and_then(|_| store::set(user, FILE, "taille", text))
                    .map_err(|error| String::from(error.message()))
            }
            _ => Err(String::from("valeur refusee")),
        }
    }

    fn adopt(&self, user: &str) {
        let follow = store::get(user, FILE, "suivre_hote", "oui") != "non";
        FOLLOW_HOST.store(follow, Ordering::Relaxed);
        if !follow
            && let Some(size) = parse_size(&store::get(user, FILE, "taille", ""))
        {
            *REQUESTED.lock() = Some(size);
        }
    }

    fn reset(&self) {
        FOLLOW_HOST.store(true, Ordering::Relaxed);
    }
}

pub fn register() {
    config::register(&TAB);
}

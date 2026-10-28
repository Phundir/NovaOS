//! `son.ss` -- the "Son" tab of `parametres.ss`.
//!
//! Until the settings application existed this was a window of its own,
//! `reglages-audio.ss`. It is now only what the audio exposes: output and
//! input device, volumes, mutes, live levels, a test tone. Launching
//! `son.ss` opens the settings on this tab; there is no second panel.
//!
//! ## Choosing a device
//!
//! The system sees the sound cards on its PCI bus -- under QEMU, one
//! AC'97. The lists show what the driver found, and on that card the
//! input can be the microphone or the line in. The **physical**
//! microphone and loudspeaker behind the virtual card are the host's
//! business: QEMU plugs the card into a PipeWire source and sink, chosen
//! when it starts (`make audio-devices`, then `QEMU_AUDIO_IN` and
//! `QEMU_AUDIO_OUT`). The tab says so in an information line, rather than
//! offering a list of host devices the guest cannot see.
//!
//! ## The input level needs the microphone
//!
//! Showing it **is** listening. `son.ss` declares `MICROPHONE` and asks
//! the policy of 12.3 when the tab opens; it only watches the level
//! (`audio::capture::watch`), so the recorder can record meanwhile.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::audio::settings::{self, Source};
use crate::binfmt::identity::Permissions;
use crate::config::{self, Choice, Kind, Provider, Setting, Value};
use crate::vga::SpinLock;

pub const APP_ID: &str = "os.nova.son";
pub const FILE_NAME: &str = "son.ss";
pub const BINARY: &str = "/sysapp/son.ss";

/// Binary of the window this tab replaced, removed at installation.
const RETIRED: &str = "/sysapp/reglages-audio.ss";

pub struct SoundTab;

pub static TAB: SoundTab = SoundTab;

/// Is the tab watching the input level?
static WATCHING: AtomicBool = AtomicBool::new(false);

/// The test tone's stream, closed once it has played.
static TEST: SpinLock<Option<crate::audio::Handle>> = SpinLock::new(None);

fn output_devices() -> Vec<Choice> {
    if crate::drivers::audio::is_present() {
        vec![Choice::new("ac97", "Intel AC'97 (carte virtuelle)")]
    } else {
        vec![Choice::new("aucun", "Aucun peripherique detecte")]
    }
}

fn input_devices() -> Vec<Choice> {
    if crate::drivers::audio::is_present() {
        vec![
            Choice::new("micro", "Microphone - AC'97"),
            Choice::new("ligne", "Entree ligne - AC'97"),
        ]
    } else {
        vec![Choice::new("aucun", "Aucun peripherique detecte")]
    }
}

/// Half a second of A440, through the master volume -- so that the test
/// sounds like the setting.
fn test_tone() -> Result<String, String> {
    if !crate::drivers::audio::is_present() {
        return Err(String::from("aucun peripherique audio - rien a tester"));
    }
    let mut test = TEST.lock();
    if let Some(stream) = test.take() {
        crate::audio::close(stream);
    }
    let stream = crate::audio::open("test-son").map_err(String::from)?;
    let rate = crate::audio::SAMPLE_RATE as i64;
    let frames = rate / 2;
    let edge = (rate / 100).max(1);
    let mut tone = Vec::with_capacity(frames as usize * 2);
    for index in 0..frames {
        let phase = 2 * (1 << 15) * 440 * index / rate;
        // A short fade at both ends, or the tone clicks.
        let envelope = index.min(frames - index).min(edge) * 256 / edge;
        let value = (crate::audio::synth::sine(phase, 8000) * envelope / 256) as i16;
        tone.push(value);
        tone.push(value);
    }
    let _ = crate::audio::write(stream, &tone);
    *test = Some(stream);
    Ok(String::from("son de test : la 440 Hz, une demi-seconde"))
}

impl Provider for SoundTab {
    fn file(&self) -> &'static str {
        FILE_NAME
    }

    fn tab(&self) -> &'static str {
        "Son"
    }

    fn settings(&self, _user: &str) -> Vec<Setting> {
        vec![
            Setting::new(
                "sortie",
                "Peripherique de sortie",
                "Carte son vue par le systeme",
                Kind::Select(output_devices()),
            ),
            Setting::new(
                "sortie_volume",
                "Volume",
                "Volume general, applique a tous les sons",
                Kind::Slider {
                    min: 0,
                    max: 100,
                    step: 5,
                    unit: "%",
                },
            ),
            Setting::new("sortie_coupee", "Couper le son", "Silence sur toute la sortie", Kind::Toggle),
            Setting::new("sortie_niveau", "Niveau de sortie", "Ce que recoit le haut-parleur", Kind::Level),
            Setting::new(
                "tester",
                "Essai",
                "Joue un la 440 Hz au volume choisi",
                Kind::Action("Tester le son"),
            ),
            Setting::new(
                "entree",
                "Peripherique d'entree",
                "Ce que la carte enregistre",
                Kind::Select(input_devices()),
            ),
            Setting::new(
                "entree_gain",
                "Gain du micro",
                "Amplification de l'entree ; au-dela de 100 %, les cris saturent",
                Kind::Slider {
                    min: 0,
                    max: 400,
                    step: 10,
                    unit: "%",
                },
            ),
            Setting::new("entree_coupee", "Couper le micro", "L'entree ne capte plus que du silence", Kind::Toggle),
            Setting::new(
                "entree_niveau",
                "Niveau du micro",
                "Parlez : la barre doit bouger",
                Kind::Level,
            ),
            Setting::new(
                "hote",
                "",
                "",
                Kind::Info,
            ),
        ]
    }

    fn get(&self, _user: &str, key: &str) -> Value {
        let current = settings::current();
        match key {
            "sortie" => Value::Text(String::from(if crate::drivers::audio::is_present() {
                "ac97"
            } else {
                "aucun"
            })),
            "sortie_volume" => Value::Number(current.output_volume as i64),
            "sortie_coupee" => Value::Bool(current.output_muted),
            "entree" => Value::Text(String::from(if crate::drivers::audio::is_present() {
                current.input_source.as_str()
            } else {
                "aucun"
            })),
            "entree_gain" => Value::Number(current.input_gain as i64),
            "entree_coupee" => Value::Bool(current.input_muted),
            "hote" => Value::Text(String::from(
                "Micro et haut-parleur reels : choisis au lancement (make audio-devices)",
            )),
            _ => Value::None,
        }
    }

    fn set(&self, user: &str, key: &str, value: &Value) -> Result<(), String> {
        let mut next = settings::current();
        match (key, value) {
            ("sortie", Value::Text(device)) => {
                // One card: choosing it is choosing what is already used.
                return if output_devices().iter().any(|choice| choice.value == *device) {
                    Ok(())
                } else {
                    Err(format!("peripherique \"{}\" absent", device))
                };
            }
            ("sortie_volume", Value::Number(volume)) => next.output_volume = (*volume).clamp(0, 100) as u32,
            ("sortie_coupee", Value::Bool(on)) => next.output_muted = *on,
            ("entree", Value::Text(source)) => {
                next.input_source = match source.as_str() {
                    "micro" => Source::Microphone,
                    "ligne" => Source::Line,
                    _ => return Err(String::from("aucune entree a choisir")),
                }
            }
            ("entree_gain", Value::Number(gain)) => next.input_gain = (*gain).clamp(0, 400) as u32,
            ("entree_coupee", Value::Bool(on)) => next.input_muted = *on,
            _ => return Err(String::from("valeur refusee")),
        }
        settings::apply(next);
        settings::save(user, &next).map_err(|error| String::from(error.message()))
    }

    fn preview(&self, _user: &str, key: &str, value: &Value) {
        let mut next = settings::current();
        match (key, value) {
            ("sortie_volume", Value::Number(volume)) => next.output_volume = (*volume).clamp(0, 100) as u32,
            ("entree_gain", Value::Number(gain)) => next.input_gain = (*gain).clamp(0, 400) as u32,
            _ => return,
        }
        // Heard while dragging, written when let go.
        settings::apply(next);
    }

    fn level(&self, key: &str) -> u16 {
        match key {
            "sortie_niveau" => crate::audio::output_level(),
            "entree_niveau" if WATCHING.load(Ordering::Relaxed) => crate::audio::capture::level(),
            _ => 0,
        }
    }

    fn act(&self, _user: &str, key: &str) -> Result<String, String> {
        match key {
            "tester" => test_tone(),
            _ => Err(String::from("aucune action")),
        }
    }

    fn shown(&self, _user: &str) -> Option<String> {
        if WATCHING.load(Ordering::Relaxed) {
            return None;
        }
        match super::authorize_native(BINARY, Permissions::MICROPHONE) {
            Ok(()) => {
                crate::audio::capture::watch();
                WATCHING.store(true, Ordering::Relaxed);
                None
            }
            Err(reason) => Some(format!("niveau du micro indisponible : {}", reason)),
        }
    }

    fn hidden(&self) {
        if WATCHING.swap(false, Ordering::Relaxed) {
            crate::audio::capture::unwatch();
        }
        if let Some(stream) = TEST.lock().take() {
            crate::audio::close(stream);
        }
    }

    fn tick(&self) {
        let mut test = TEST.lock();
        if let Some(stream) = *test
            && crate::audio::pending(stream) == 0
        {
            crate::audio::close(stream);
            *test = None;
        }
    }

    fn adopt(&self, user: &str) {
        settings::adopt_user(user);
    }

    fn reset(&self) {
        settings::reset();
        // The settings window of the session is gone -- killed at log
        // out, its watch and its test stream already released by
        // `audio::forget_owner`. Forgotten here too: a stale handle closed
        // later could be another application's stream by then.
        WATCHING.store(false, Ordering::Relaxed);
        TEST.lock().take();
    }
}

/// Registers the tab. Called at boot.
pub fn register() {
    config::register(&TAB);
}

/// `son.ss` from the menu: the settings, opened on this tab.
pub fn task() -> ! {
    super::parametres::run_on(Some(FILE_NAME))
}

pub fn install(now: u64) -> bool {
    // The window this tab replaced: its binary would stay in the menu and
    // launch nothing, since no native code answers to its name any more.
    let _ = crate::fs::delete(RETIRED, crate::fs::perm::Credentials::kernel(), now);
    super::install_binary(
        &super::BinarySpec {
            path: BINARY,
            title: FILE_NAME,
            app_id: APP_ID,
            display_name: "Son",
            format: crate::binfmt::header::BinaryFormat::Ss,
            permissions: Permissions::MICROPHONE | Permissions::AUDIO_OUTPUT,
        },
        now,
    )
}

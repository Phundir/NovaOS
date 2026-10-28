//! `magneto.cus` -- voice recorder (spec 3.1, Common Utility Software).
//!
//! ```text
//!   +--------------------------------------------------------+
//!   | Enregistreur vocal                                     |
//!   | [ Enregistrer ]   0:12 / 1:06                          |
//!   |                   Microphone [|||||||||        ]       |
//!   | Enregistrements                                        |
//!   | +----------------------------------------------------+ |
//!   | | enregistrement-001.wav                  0:08       | |
//!   | +----------------------------------------------------+ |
//!   | [Ecouter] [Supprimer]                                  |
//!   +--------------------------------------------------------+
//! ```
//!
//! ## 16 kHz mono, and why
//!
//! A file on this disk holds at most `MAX_FILE_SIZE`, a little over 2 MiB.
//! At the 48 kHz stereo the driver captures, that is eleven seconds; at
//! 16 kHz mono, the telephone-and-a-half of speech codecs, it is
//! sixty-six. A voice recorder that stops after eleven seconds is not one,
//! so the capture is reduced on the fly (`audio::resample::Decimator`) and
//! the recording stops by itself when the next second would not fit --
//! with a message, not a truncated file.
//!
//! ## The microphone is a declared permission
//!
//! 12.3 puts the microphone on the allow-list: it must be declared in the
//! header or it is refused. The recorder declares `MICROPHONE` and asks
//! `apps::authorize_native` before opening the input, so that the rule is
//! enforced by the policy engine and not by this file's good manners.
//!
//! ## One recorder at a time
//!
//! The input has one reader (`audio::capture`): a second recorder is told
//! the microphone is taken, rather than each getting half the words.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::audio::resample::{Decimator, VOICE_RATE};
use crate::binfmt::identity::Permissions;
use crate::fs::perm::{Credentials, Mode};
use crate::gui::protocol::{CursorShape, KeyEvent};
use crate::gui::surface::Rect;

use super::playback::Playback;
use super::widgets::{self, rect};
use super::window::{self, App, Press, Ui};

pub const APP_ID: &str = "os.nova.magneto";
pub const FILE_NAME: &str = "magneto.cus";
pub const BINARY: &str = "/apps/magneto.cus";

const WIDTH: i32 = 560;
const HEIGHT: i32 = 420;
const MARGIN: i32 = 12;
const ROW_H: i32 = 20;
const MAX_ROWS: usize = 10;

const SCAN_ENTER: u8 = 0x1C;
const SCAN_SPACE: u8 = 0x39;
const SCAN_UP: u8 = 0x48;
const SCAN_DOWN: u8 = 0x50;
const SCAN_DELETE: u8 = 0x53;

/// Where the recordings go: a directory of their own in the documents,
/// since 4.3 gives the home no audio directory.
pub fn recordings_dir(user: &str) -> String {
    format!("/users/{}/home/documents/enregistrements", user)
}

/// Samples of a recording that still fit in one file.
fn max_samples() -> usize {
    crate::audio::wav::capacity_frames(crate::fs::format::MAX_FILE_SIZE) as usize
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Record,
    Row(usize),
    Listen,
    Delete,
}

struct Layout {
    width: i32,
    height: i32,
}

impl Layout {
    fn record(&self) -> Rect {
        rect(MARGIN, 40, 170, 40)
    }
    fn meter(&self) -> Rect {
        rect(MARGIN + 290, 66, self.width - MARGIN - 290 - MARGIN, 12)
    }
    fn list(&self) -> Rect {
        rect(MARGIN, 120, self.width - 2 * MARGIN, MAX_ROWS as i32 * ROW_H + 4)
    }
    fn row(&self, rank: usize) -> Rect {
        let list = self.list();
        rect(list.x + 2, list.y + 2 + rank as i32 * ROW_H, list.width - 4, ROW_H)
    }
    fn listen(&self) -> Rect {
        rect(MARGIN, self.list().bottom() + 10, 120, 26)
    }
    fn delete(&self) -> Rect {
        rect(MARGIN + 130, self.list().bottom() + 10, 120, 26)
    }
    fn status_y(&self) -> i32 {
        self.height - 44
    }
    fn help_y(&self) -> i32 {
        self.height - 24
    }
}

struct Recording {
    reader: crate::audio::capture::Reader,
    decimator: Decimator,
    samples: Vec<i16>,
}

struct Recorder {
    who: Credentials,
    user: String,
    layout: Layout,
    /// Name and duration of each recording.
    files: Vec<(String, u64)>,
    selected: usize,
    top: usize,
    recording: Option<Recording>,
    playing: Option<Playback>,
    /// The delete button waits for a second click.
    armed: bool,
    message: String,
    error: bool,
    hover: Option<Hit>,
    shown: (u16, u64),
    /// Laps since the list was last read again.
    laps: u32,
}

/// Laps between two readings of the recordings: a second.
const REFRESH_LAPS: u32 = (1000 / window::PERIOD_MS) as u32;

impl Recorder {
    fn inform(&mut self, text: String, error: bool) {
        crate::serial_println!("[magneto] {}", text);
        self.message = text;
        self.error = error;
    }

    fn refresh(&mut self) {
        let dir = recordings_dir(&self.user);
        let mut files: Vec<(String, u64)> = match crate::fs::list(&dir, self.who) {
            Ok(links) => links
                .into_iter()
                .filter(|link| link.name.to_ascii_lowercase().ends_with(".wav"))
                .map(|link| {
                    // The size is enough for a duration: the recorder writes
                    // 16-bit mono at a known rate, and a file it did not
                    // write shows its real duration once played.
                    let size = crate::fs::stat(&super::join_path(&dir, &link.name), self.who)
                        .map(|node| node.size)
                        .unwrap_or(0);
                    let ms = size.saturating_sub(44) / 2 * 1000 / VOICE_RATE as u64;
                    (link.name, ms)
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        files.sort();
        self.files = files;
        if self.selected >= self.files.len() {
            self.selected = self.files.len().saturating_sub(1);
        }
    }

    fn start(&mut self) {
        if !crate::drivers::audio::is_present() {
            self.inform(String::from("aucun peripherique audio - rien a enregistrer"), true);
            return;
        }
        if let Err(reason) = super::authorize_native(BINARY, Permissions::MICROPHONE) {
            self.inform(format!("microphone refuse : {}", reason), true);
            return;
        }
        if let Some(mut playing) = self.playing.take() {
            playing.close();
        }
        match crate::audio::capture::open() {
            Ok(reader) => {
                self.recording = Some(Recording {
                    reader,
                    decimator: Decimator::new(),
                    samples: Vec::with_capacity(VOICE_RATE as usize * 10),
                });
                let source = crate::audio::settings::current().input_source.label();
                self.inform(format!("enregistrement en cours - {}", source), false);
            }
            Err(reason) => self.inform(String::from(reason), true),
        }
    }

    /// Ends the recording and writes it.
    fn stop(&mut self) {
        let Some(recording) = self.recording.take() else {
            return;
        };
        crate::audio::capture::close(recording.reader);
        let samples = recording.samples;
        if samples.is_empty() {
            self.inform(String::from("rien n'a ete capte"), true);
            return;
        }

        let dir = recordings_dir(&self.user);
        let now = crate::idt::ticks();
        let _ = crate::fs::create_directory(&dir, self.who, Mode::private_dir(), now);
        let taken: Vec<String> = self.files.iter().map(|(name, _)| name.clone()).collect();
        let name = (1..1000)
            .map(|rank| format!("enregistrement-{:03}.wav", rank))
            .find(|name| !taken.contains(name))
            .unwrap_or_else(|| String::from("enregistrement.wav"));
        let path = super::join_path(&dir, &name);
        let bytes = crate::audio::wav::encode(VOICE_RATE, 1, &samples);
        let ms = samples.len() as u64 * 1000 / VOICE_RATE as u64;
        match crate::fs::write_file(&path, &bytes, self.who, Mode::private_file(), now) {
            Ok(_) => {
                self.inform(format!("enregistre : {} ({})", name, widgets::clock(ms)), false);
                self.refresh();
                if let Some(index) = self.files.iter().position(|(file, _)| *file == name) {
                    self.select(index);
                }
            }
            Err(error) => {
                self.inform(format!("ecriture impossible : {}", error.message()), true);
            }
        }
    }

    fn toggle_recording(&mut self) {
        if self.recording.is_some() {
            self.stop();
        } else {
            self.start();
        }
    }

    fn listen(&mut self) {
        if let Some(mut playing) = self.playing.take() {
            playing.close();
            self.inform(String::from("ecoute arretee"), false);
            return;
        }
        if self.recording.is_some() {
            return;
        }
        let Some((name, _)) = self.files.get(self.selected).cloned() else {
            return;
        };
        if let Err(reason) = super::authorize_native(BINARY, Permissions::AUDIO_OUTPUT) {
            self.inform(format!("sortie audio refusee : {}", reason), true);
            return;
        }
        let path = super::join_path(&recordings_dir(&self.user), &name);
        let loaded = crate::fs::read_file(&path, self.who)
            .map_err(|error| String::from(error.message()))
            .and_then(|bytes| Playback::load(bytes, "magneto").map_err(String::from));
        match loaded {
            Ok(mut playback) => match playback.play() {
                Ok(()) => {
                    self.playing = Some(playback);
                    self.inform(format!("ecoute de {}", name), false);
                }
                Err(reason) => self.inform(String::from(reason), true),
            },
            Err(reason) => self.inform(format!("{} : {}", name, reason), true),
        }
    }

    /// First press arms, second deletes: a recording is not recoverable.
    fn delete(&mut self) {
        let Some((name, _)) = self.files.get(self.selected).cloned() else {
            return;
        };
        if !self.armed {
            self.armed = true;
            self.message = format!("Supprimer {} ? Cliquez encore pour confirmer", name);
            self.error = true;
            return;
        }
        self.armed = false;
        let path = super::join_path(&recordings_dir(&self.user), &name);
        match crate::fs::delete(&path, self.who, crate::idt::ticks()) {
            Ok(()) => {
                self.inform(format!("{} supprime", name), false);
                self.refresh();
            }
            Err(error) => self.inform(format!("{} : {}", name, error.message()), true),
        }
    }

    fn select(&mut self, index: usize) {
        if index < self.files.len() {
            self.selected = index;
            self.armed = false;
            if index < self.top {
                self.top = index;
            } else if index >= self.top + MAX_ROWS {
                self.top = index + 1 - MAX_ROWS;
            }
        }
    }

    fn elapsed_ms(&self) -> u64 {
        self.recording
            .as_ref()
            .map(|recording| recording.samples.len() as u64 * 1000 / VOICE_RATE as u64)
            .unwrap_or(0)
    }

    fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        let layout = &self.layout;
        if layout.record().contains(x, y) {
            return Some(Hit::Record);
        }
        for rank in 0..MAX_ROWS {
            if layout.row(rank).contains(x, y) && self.top + rank < self.files.len() {
                return Some(Hit::Row(self.top + rank));
            }
        }
        if !self.files.is_empty() {
            if layout.listen().contains(x, y) {
                return Some(Hit::Listen);
            }
            if layout.delete().contains(x, y) {
                return Some(Hit::Delete);
            }
        }
        None
    }
}

impl App for Recorder {
    fn size(&mut self, width: i32, height: i32) {
        self.layout = Layout { width, height };
    }

    fn paint(&self, ui: &mut Ui) {
        let theme = ui.theme;
        let layout = &self.layout;
        ui.text(MARGIN, 10, "Enregistreur vocal", theme.accent);

        let recording = self.recording.is_some();
        widgets::button(
            ui,
            layout.record(),
            if recording { "Arreter" } else { "Enregistrer" },
            true,
            self.hover == Some(Hit::Record),
            recording,
        );
        let max_ms = max_samples() as u64 * 1000 / VOICE_RATE as u64;
        let time = format!("{} / {} max", widgets::clock(self.elapsed_ms()), widgets::clock(max_ms));
        ui.text(MARGIN + 190, 44, &time, if recording { theme.danger } else { theme.text });

        let settings = crate::audio::settings::current();
        let meter = layout.meter();
        ui.text(MARGIN + 190, widgets::text_y(meter) - 2, settings.input_source.label(), theme.text);
        widgets::meter(ui, meter, if recording { crate::audio::capture::level() } else { 0 });

        let list = layout.list();
        ui.text(list.x, list.y - 20, "Enregistrements", theme.text);
        ui.frame(list.x, list.y, list.width, list.height, theme.border);
        if self.files.is_empty() {
            ui.text(list.x + 8, list.y + 8, "(aucun enregistrement)", theme.text_dim);
        }
        for rank in 0..MAX_ROWS {
            let index = self.top + rank;
            let Some((name, ms)) = self.files.get(index) else {
                break;
            };
            let row = layout.row(rank);
            let selected = index == self.selected;
            if selected || self.hover == Some(Hit::Row(index)) {
                ui.rect(
                    row.x,
                    row.y,
                    row.width,
                    row.height,
                    if selected { theme.surface_alt } else { theme.surface },
                );
            }
            ui.text(row.x + 6, widgets::text_y(row), name, if selected { theme.text } else { theme.text_dim });
            let duration = widgets::clock(*ms);
            ui.text(
                row.right() - 8 - widgets::text_width(&duration),
                widgets::text_y(row),
                &duration,
                theme.text_dim,
            );
        }

        if !self.files.is_empty() {
            let playing = self.playing.as_ref().is_some_and(|playback| playback.is_playing());
            widgets::button(
                ui,
                layout.listen(),
                if playing { "Stop" } else { "Ecouter" },
                !recording,
                self.hover == Some(Hit::Listen),
                playing,
            );
            widgets::button(
                ui,
                layout.delete(),
                if self.armed { "Confirmer ?" } else { "Supprimer" },
                !recording,
                self.hover == Some(Hit::Delete),
                self.armed,
            );
        }

        if !self.message.is_empty() {
            ui.text(
                MARGIN,
                layout.status_y(),
                &crate::gui::client::ellipsize(&self.message, ((layout.width - 2 * MARGIN) / 8) as usize),
                if self.error { theme.danger } else { theme.accent },
            );
        }
        ui.text(
            MARGIN,
            layout.help_y(),
            "Espace ou R enregistrer/arreter  Entree ecouter  Suppr supprimer",
            theme.text_dim,
        );
    }

    fn key(&mut self, key: &KeyEvent) -> bool {
        if !key.pressed {
            return false;
        }
        match key.scancode {
            SCAN_SPACE => self.toggle_recording(),
            SCAN_ENTER => self.listen(),
            SCAN_UP => self.select(self.selected.saturating_sub(1)),
            SCAN_DOWN => self.select(self.selected + 1),
            SCAN_DELETE => self.delete(),
            _ => match key.character().map(|c| c.to_ascii_lowercase()) {
                Some('r') => self.toggle_recording(),
                _ => return false,
            },
        }
        true
    }

    fn pointer(&mut self, x: i32, y: i32, press: Press) -> (bool, CursorShape) {
        let hit = self.hit(x, y);
        let mut dirty = hit != self.hover;
        self.hover = hit;
        if press.pressed {
            dirty = true;
            if hit != Some(Hit::Delete) {
                self.armed = false;
            }
            match hit {
                Some(Hit::Record) => self.toggle_recording(),
                Some(Hit::Row(index)) => self.select(index),
                Some(Hit::Listen) if self.recording.is_none() => self.listen(),
                Some(Hit::Delete) if self.recording.is_none() => self.delete(),
                _ => {}
            }
        }
        (dirty, if hit.is_some() { CursorShape::Hand } else { CursorShape::Arrow })
    }

    fn tick(&mut self) -> bool {
        let mut dirty = false;

        // The list follows the folder -- a recording deleted from the
        // explorer, one copied in -- but not under a pending deletion,
        // whose confirmation is about the row on screen.
        self.laps += 1;
        if self.laps >= REFRESH_LAPS && !self.armed {
            self.laps = 0;
            let before = self.files.clone();
            self.refresh();
            dirty |= self.files != before;
        }

        let mut full = false;
        if let Some(recording) = self.recording.as_mut() {
            let mut block = [0i16; 4096];
            loop {
                let count = crate::audio::capture::read(&recording.reader, &mut block);
                if count == 0 {
                    break;
                }
                recording.decimator.push(&block[..count], &mut recording.samples);
            }
            if recording.samples.len() >= max_samples() {
                recording.samples.truncate(max_samples());
                full = true;
            }
        }
        if full {
            self.stop();
            self.message.push_str(" - taille maximale d'un fichier atteinte");
            dirty = true;
        }

        if let Some(playback) = self.playing.as_mut()
            && playback.feed()
        {
            self.playing = None;
            self.inform(String::from("fin de l'ecoute"), false);
            dirty = true;
        }

        let level = if self.recording.is_some() { (widgets::level_to_db(crate::audio::capture::level()).max(-61) + 61) as u16 } else { 0 };
        let quarter = self.elapsed_ms() / 250;
        if (level, quarter) != self.shown {
            self.shown = (level, quarter);
            dirty = true;
        }
        dirty
    }

    fn close(&mut self) {
        // A recording in progress when the window closes is written, not
        // lost: closing is not the same gesture as throwing away.
        if self.recording.is_some() {
            self.stop();
        }
        if let Some(mut playing) = self.playing.take() {
            playing.close();
        }
    }
}

pub fn task() -> ! {
    let Some((user, who)) = super::session_credentials() else {
        crate::serial_println!("[magneto] aucune session ouverte - non demarre");
        crate::scheduler::exit_current_with(-1);
    };
    let mut recorder = Recorder {
        who,
        user,
        layout: Layout {
            width: WIDTH,
            height: HEIGHT,
        },
        files: Vec::new(),
        selected: 0,
        top: 0,
        recording: None,
        playing: None,
        armed: false,
        message: String::new(),
        error: false,
        hover: None,
        shown: (0, 0),
        laps: 0,
    };
    recorder.refresh();
    window::run("magneto", "Enregistreur vocal", WIDTH, HEIGHT, &mut recorder)
}

pub fn install(now: u64) -> bool {
    super::install_binary(
        &super::BinarySpec {
            path: BINARY,
            title: FILE_NAME,
            app_id: APP_ID,
            display_name: "Enregistreur vocal",
            format: crate::binfmt::header::BinaryFormat::Cus,
            // The microphone is on the allow-list of 12.3: declared here,
            // or refused by `authorize_native`.
            permissions: Permissions::MICROPHONE | Permissions::AUDIO_OUTPUT | Permissions::FS_USER_HOME,
        },
        now,
    )
}

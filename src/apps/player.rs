//! `lecteur.cus` -- audio player (spec 3.1, Common Utility Software).
//!
//! ```text
//!   +--------------------------------------------------------------+
//!   | Lecteur audio                                                |
//!   | [Enreg.][Partage][Documents]   message.wav                 |
//!   | +-----------------------+      22050 Hz, mono, 16 bits, 0:03  |
//!   | | message.wav         |      [=========------------------]  |
//!   | | enregistrement-001.wav|      0:01 / 0:03                    |
//!   | |                       |      [<< 5 s][Lecture][Stop][5 s >>]|
//!   | +-----------------------+      Volume [=====o-----]  100 %    |
//!   |                                Sortie [|||||||      ]         |
//!   +--------------------------------------------------------------+
//! ```
//!
//! ## What it plays
//!
//! WAV files, in the encodings `audio::wav` reads, at any rate -- the
//! conversion to the mixer's 48 kHz stereo is `audio::resample`'s. No MP3,
//! no Ogg: a decoder for either is a project of its own, and a list that
//! showed them and then failed would be worse than a list that does not.
//!
//! ## Its rights
//!
//! Those of the account in session, like the explorer: it reads the files
//! the account may read, and a refusal says so. It declares
//! `AUDIO_OUTPUT` in its header and asks the policy of 12.3 before
//! opening a stream.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::identity::Permissions;
use crate::fs::perm::Credentials;
use crate::gui::protocol::{CursorShape, KeyEvent};
use crate::gui::surface::Rect;
use crate::vga::SpinLock;

use super::playback::Playback;
use super::widgets::{self, rect};
use super::window::{self, App, Press, Ui};

pub const APP_ID: &str = "os.nova.lecteur";
pub const FILE_NAME: &str = "lecteur.cus";
pub const BINARY: &str = "/apps/lecteur.cus";

const WIDTH: i32 = 660;
const HEIGHT: i32 = 400;
const MARGIN: i32 = 12;
const LIST_W: i32 = 250;
const ROW_H: i32 = 20;
const MAX_ROWS: usize = 12;
const BUTTON_H: i32 = 26;

/// Seek step of the arrow keys and the two buttons.
const STEP_MS: u64 = 5000;
/// Highest stream volume: twice, the master applies on top.
const MAX_VOLUME: u32 = 200;

const SCAN_ENTER: u8 = 0x1C;
const SCAN_SPACE: u8 = 0x39;
const SCAN_UP: u8 = 0x48;
const SCAN_DOWN: u8 = 0x50;
const SCAN_LEFT: u8 = 0x4B;
const SCAN_RIGHT: u8 = 0x4D;

/// Files handed over by the explorer, like `notepad::open_on_start`.
static TO_OPEN: SpinLock<Vec<String>> = SpinLock::new(Vec::new());

/// Asks the next player that starts to play `path`.
pub fn open_on_start(path: &str) {
    TO_OPEN.lock().push(String::from(path));
}

const FOLDERS: [&str; 3] = ["Enreg.", "Partage", "Documents"];

/// The folder every account can read: sounds shared between sessions.
const SHARED_AUDIO: &str = "/users/_shared/audio";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Folder(usize),
    Row(usize),
    Progress,
    Back,
    PlayPause,
    Stop,
    Forward,
    Volume,
}

struct Layout {
    width: i32,
    height: i32,
}

impl Layout {
    fn folder(&self, index: usize) -> Rect {
        let w = (LIST_W - 8) / 3;
        rect(MARGIN + index as i32 * (w + 4), 34, w, BUTTON_H - 4)
    }
    fn list(&self) -> Rect {
        rect(MARGIN, 64, LIST_W, MAX_ROWS as i32 * ROW_H + 4)
    }
    fn row(&self, rank: usize) -> Rect {
        let list = self.list();
        rect(list.x + 2, list.y + 2 + rank as i32 * ROW_H, list.width - 4, ROW_H)
    }
    fn right_x(&self) -> i32 {
        MARGIN + LIST_W + 2 * MARGIN
    }
    fn right_w(&self) -> i32 {
        self.width - self.right_x() - MARGIN
    }
    fn progress(&self) -> Rect {
        rect(self.right_x(), 110, self.right_w(), 16)
    }
    fn transport(&self, index: usize) -> Rect {
        let w = (self.right_w() - 3 * 6) / 4;
        rect(self.right_x() + index as i32 * (w + 6), 160, w, BUTTON_H)
    }
    fn volume(&self) -> Rect {
        rect(self.right_x() + 70, 212, self.right_w() - 140, 16)
    }
    fn meter(&self) -> Rect {
        rect(self.right_x() + 70, 250, self.right_w() - 70, 12)
    }
    fn status_y(&self) -> i32 {
        self.height - 44
    }
    fn help_y(&self) -> i32 {
        self.height - 24
    }
}

struct Player {
    who: Credentials,
    user: String,
    layout: Layout,
    folder: usize,
    files: Vec<String>,
    selected: usize,
    top: usize,
    /// Path and playback of the loaded file.
    loaded: Option<(String, Playback)>,
    volume: u32,
    message: String,
    error: bool,
    hover: Option<Hit>,
    /// Slider being dragged.
    dragging: Option<Hit>,
    /// Last level and second shown, to repaint only when they change.
    shown: (u16, u64),
    /// Laps since the folder was last read again.
    laps: u32,
}

/// Laps between two readings of the folder: a second at `PERIOD_MS`.
const REFRESH_LAPS: u32 = (1000 / window::PERIOD_MS) as u32;

impl Player {
    fn folder_path(&self, index: usize) -> String {
        match index {
            0 => super::recorder::recordings_dir(&self.user),
            1 => String::from(SHARED_AUDIO),
            _ => format!("/users/{}/home/documents", self.user),
        }
    }

    fn inform(&mut self, text: String, error: bool) {
        if error {
            crate::serial_println!("[lecteur] {}", text);
        }
        self.message = text;
        self.error = error;
    }

    /// Reads the folder again, keeping the selection on the same file.
    /// Returns `true` if the list changed -- a recording saved meanwhile.
    fn refresh_files(&mut self) -> bool {
        let keep = self.files.get(self.selected).cloned();
        let before = core::mem::take(&mut self.files);
        self.files = self.read_folder(self.folder);
        if self.files == before {
            return false;
        }
        self.selected = keep
            .and_then(|name| self.files.iter().position(|file| *file == name))
            .unwrap_or(0);
        self.top = self.top.min(self.files.len().saturating_sub(MAX_ROWS));
        true
    }

    fn read_folder(&self, index: usize) -> Vec<String> {
        let path = self.folder_path(index);
        match crate::fs::list(&path, self.who) {
            Ok(links) => {
                let mut names: Vec<String> = links
                    .into_iter()
                    .map(|link| link.name)
                    .filter(|name| name.to_ascii_lowercase().ends_with(".wav"))
                    .collect();
                names.sort();
                names
            }
            Err(_) => Vec::new(),
        }
    }

    fn open_folder(&mut self, index: usize) {
        self.folder = index;
        self.files = self.read_folder(index);
        self.selected = 0;
        self.top = 0;
    }

    /// Reads a file and starts playing it.
    fn load(&mut self, path: &str) {
        if let Some((_, mut old)) = self.loaded.take() {
            old.close();
        }
        let bytes = match crate::fs::read_file(path, self.who) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.inform(format!("{} : {}", path, error.message()), true);
                return;
            }
        };
        match Playback::load(bytes, "lecteur") {
            Ok(mut playback) => {
                playback.set_volume(self.volume * crate::audio::VOLUME_UNITY / 100);
                let info = *playback.info();
                crate::serial_println!(
                    "[lecteur] \"{}\" - {} Hz, {} voie(s), {} bits, {} ms",
                    path,
                    info.rate,
                    info.channels,
                    info.bits(),
                    info.duration_ms()
                );
                self.loaded = Some((String::from(path), playback));
                self.play();
            }
            Err(reason) => self.inform(format!("{} : {}", file_name(path), reason), true),
        }
    }

    fn play(&mut self) {
        if let Err(reason) = super::authorize_native(BINARY, Permissions::AUDIO_OUTPUT) {
            self.inform(format!("sortie audio refusee : {}", reason), true);
            return;
        }
        let Some((_, playback)) = self.loaded.as_mut() else {
            return;
        };
        match playback.play() {
            Ok(()) => {
                let text = if crate::drivers::audio::is_present() {
                    String::from("lecture")
                } else {
                    // The mixer takes the samples, and nothing drains it:
                    // the position will not move, and that must not look
                    // like a player that hangs.
                    String::from("aucun peripherique audio - rien ne sera entendu")
                };
                let error = !crate::drivers::audio::is_present();
                self.inform(text, error);
            }
            Err(reason) => self.inform(String::from(reason), true),
        }
    }

    fn toggle(&mut self) {
        match self.loaded.as_mut() {
            Some((_, playback)) if playback.is_playing() => {
                playback.pause();
                self.inform(String::from("pause"), false);
            }
            Some(_) => self.play(),
            None => self.open_selected(),
        }
    }

    fn stop(&mut self) {
        if let Some((_, playback)) = self.loaded.as_mut() {
            playback.stop();
            self.inform(String::from("arret"), false);
        }
    }

    fn step(&mut self, forward: bool) {
        if let Some((_, playback)) = self.loaded.as_mut() {
            let now = playback.position_ms();
            let target = if forward {
                (now + STEP_MS).min(playback.duration_ms())
            } else {
                now.saturating_sub(STEP_MS)
            };
            playback.seek_ms(target);
        }
    }

    fn open_selected(&mut self) {
        if let Some(name) = self.files.get(self.selected).cloned() {
            let path = super::join_path(&self.folder_path(self.folder), &name);
            self.load(&path);
        }
    }

    fn select(&mut self, index: usize) {
        if index < self.files.len() {
            self.selected = index;
            if index < self.top {
                self.top = index;
            } else if index >= self.top + MAX_ROWS {
                self.top = index + 1 - MAX_ROWS;
            }
        }
    }

    fn set_volume(&mut self, percent: u32) {
        self.volume = percent.min(MAX_VOLUME);
        if let Some((_, playback)) = self.loaded.as_mut() {
            playback.set_volume(self.volume * crate::audio::VOLUME_UNITY / 100);
        }
    }

    fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        let layout = &self.layout;
        for index in 0..FOLDERS.len() {
            if layout.folder(index).contains(x, y) {
                return Some(Hit::Folder(index));
            }
        }
        for rank in 0..MAX_ROWS {
            if layout.row(rank).contains(x, y) && self.top + rank < self.files.len() {
                return Some(Hit::Row(self.top + rank));
            }
        }
        if self.loaded.is_some() {
            if layout.progress().contains(x, y) {
                return Some(Hit::Progress);
            }
            let transport = [Hit::Back, Hit::PlayPause, Hit::Stop, Hit::Forward];
            for (index, hit) in transport.iter().enumerate() {
                if layout.transport(index).contains(x, y) {
                    return Some(*hit);
                }
            }
        }
        if layout.volume().contains(x, y) {
            return Some(Hit::Volume);
        }
        None
    }

    /// Moves a slider being dragged to `x`.
    fn drag(&mut self, hit: Hit, x: i32) {
        match hit {
            Hit::Volume => {
                let value = widgets::slider_value(self.layout.volume(), x, MAX_VOLUME);
                self.set_volume(value);
            }
            Hit::Progress => {
                let progress = self.layout.progress();
                if let Some((_, playback)) = self.loaded.as_mut() {
                    let total = playback.duration_ms();
                    let target = widgets::slider_value(progress, x, 1000) as u64 * total / 1000;
                    playback.seek_ms(target);
                }
            }
            _ => {}
        }
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

impl App for Player {
    fn size(&mut self, width: i32, height: i32) {
        self.layout = Layout { width, height };
    }

    fn paint(&self, ui: &mut Ui) {
        let theme = ui.theme;
        let layout = &self.layout;
        ui.text(MARGIN, 10, "Lecteur audio", theme.accent);

        for (index, label) in FOLDERS.iter().enumerate() {
            widgets::button(
                ui,
                layout.folder(index),
                label,
                true,
                self.hover == Some(Hit::Folder(index)),
                self.folder == index,
            );
        }

        let list = layout.list();
        ui.frame(list.x, list.y, list.width, list.height, theme.border);
        if self.files.is_empty() {
            ui.text(list.x + 8, list.y + 8, "(aucun fichier .wav)", theme.text_dim);
        }
        let cells = ((list.width - 16) / 8).max(1) as usize;
        for rank in 0..MAX_ROWS {
            let index = self.top + rank;
            let Some(name) = self.files.get(index) else {
                break;
            };
            let row = layout.row(rank);
            let selected = index == self.selected;
            let hovered = self.hover == Some(Hit::Row(index));
            if selected || hovered {
                ui.rect(
                    row.x,
                    row.y,
                    row.width,
                    row.height,
                    if selected { theme.surface_alt } else { theme.surface },
                );
            }
            ui.text(
                row.x + 6,
                widgets::text_y(row),
                &crate::gui::client::ellipsize(name, cells),
                if selected { theme.text } else { theme.text_dim },
            );
        }

        let x = layout.right_x();
        let cells = (layout.right_w() / 8).max(1) as usize;
        match &self.loaded {
            None => ui.text(x, 40, "Choisissez un fichier a gauche", theme.text_dim),
            Some((path, playback)) => {
                let info = playback.info();
                ui.text(x, 40, &crate::gui::client::ellipsize(file_name(path), cells), theme.text);
                let format = format!(
                    "{} Hz, {}, {} bits",
                    info.rate,
                    if info.channels == 1 { String::from("mono") } else { format!("{} voies", info.channels) },
                    info.bits()
                );
                ui.text(x, 62, &format, theme.text_dim);

                let position = playback.position_ms();
                let total = playback.duration_ms();
                widgets::slider(
                    ui,
                    layout.progress(),
                    position.min(total) as u32,
                    total.max(1) as u32,
                    self.hover == Some(Hit::Progress),
                );
                let time = format!("{} / {}", widgets::clock(position), widgets::clock(total));
                ui.text(x, 132, &time, theme.text);

                let labels = [
                    "<< 5 s",
                    if playback.is_playing() { "Pause" } else { "Lecture" },
                    "Stop",
                    "5 s >>",
                ];
                let hits = [Hit::Back, Hit::PlayPause, Hit::Stop, Hit::Forward];
                for (index, label) in labels.iter().enumerate() {
                    widgets::button(
                        ui,
                        layout.transport(index),
                        label,
                        true,
                        self.hover == Some(hits[index]),
                        index == 1 && playback.is_playing(),
                    );
                }
            }
        }

        let volume = layout.volume();
        ui.text(x, widgets::text_y(volume), "Volume", theme.text);
        widgets::slider(ui, volume, self.volume, MAX_VOLUME, self.hover == Some(Hit::Volume));
        ui.text(volume.right() + 12, widgets::text_y(volume), &format!("{} %", self.volume), theme.text);

        let meter = layout.meter();
        ui.text(x, widgets::text_y(meter) - 2, "Sortie", theme.text);
        widgets::meter(ui, meter, crate::audio::output_level());

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
            "Espace lecture/pause  S stop  fleches 5 s  +/- volume  Entree ouvrir",
            theme.text_dim,
        );
    }

    fn key(&mut self, key: &KeyEvent) -> bool {
        if !key.pressed {
            return false;
        }
        match key.scancode {
            SCAN_SPACE => self.toggle(),
            SCAN_ENTER => self.open_selected(),
            SCAN_UP => self.select(self.selected.saturating_sub(1)),
            SCAN_DOWN => self.select(self.selected + 1),
            SCAN_LEFT => self.step(false),
            SCAN_RIGHT => self.step(true),
            _ => match key.character().map(|c| c.to_ascii_lowercase()) {
                Some('s') => self.stop(),
                Some('+') => self.set_volume(self.volume + 10),
                Some('-') => self.set_volume(self.volume.saturating_sub(10)),
                _ => return false,
            },
        }
        true
    }

    fn pointer(&mut self, x: i32, y: i32, press: Press) -> (bool, CursorShape) {
        let hit = self.hit(x, y);
        let mut dirty = hit != self.hover;
        self.hover = hit;

        if let Some(dragged) = self.dragging {
            if press.held {
                self.drag(dragged, x);
                return (true, CursorShape::Hand);
            }
            self.dragging = None;
            dirty = true;
        }

        if press.pressed {
            dirty = true;
            match hit {
                Some(Hit::Folder(index)) => self.open_folder(index),
                Some(Hit::Row(index)) => {
                    // A second click on the selected row plays it: a single
                    // click that played would start a file for every row
                    // the pointer is dragged across.
                    if index == self.selected {
                        self.open_selected();
                    } else {
                        self.select(index);
                    }
                }
                Some(Hit::Back) => self.step(false),
                Some(Hit::Forward) => self.step(true),
                Some(Hit::PlayPause) => self.toggle(),
                Some(Hit::Stop) => self.stop(),
                Some(slider @ (Hit::Volume | Hit::Progress)) => {
                    self.dragging = Some(slider);
                    self.drag(slider, x);
                }
                None => {}
            }
        }
        (
            dirty,
            if hit.is_some() { CursorShape::Hand } else { CursorShape::Arrow },
        )
    }

    fn tick(&mut self) -> bool {
        let mut dirty = false;
        self.laps += 1;
        if self.laps >= REFRESH_LAPS {
            self.laps = 0;
            dirty |= self.refresh_files();
        }
        if let Some((_, playback)) = self.loaded.as_mut()
            && playback.feed()
        {
            self.inform(String::from("fin du fichier"), false);
            dirty = true;
        }
        // Repainted when what is shown changes -- the second on the clock
        // or the level -- and not every 20 ms for nothing.
        let level = (widgets::level_to_db(crate::audio::output_level()).max(-61) + 61) as u16;
        let second = self
            .loaded
            .as_ref()
            .map(|(_, playback)| playback.position_ms() / 250)
            .unwrap_or(0);
        if (level, second) != self.shown {
            self.shown = (level, second);
            dirty = true;
        }
        dirty
    }

    fn close(&mut self) {
        if let Some((_, mut playback)) = self.loaded.take() {
            playback.close();
        }
    }
}

pub fn task() -> ! {
    let Some((user, who)) = super::session_credentials() else {
        crate::serial_println!("[lecteur] aucune session ouverte - non demarre");
        crate::scheduler::exit_current_with(-1);
    };
    let mut player = Player {
        who,
        user,
        layout: Layout {
            width: WIDTH,
            height: HEIGHT,
        },
        folder: 0,
        files: Vec::new(),
        selected: 0,
        top: 0,
        loaded: None,
        volume: 100,
        message: String::new(),
        error: false,
        hover: None,
        dragging: None,
        shown: (0, 0),
        laps: 0,
    };
    // The recordings first: the shared folder no longer ships a sample.
    player.open_folder(0);

    let handed = {
        let mut queue = TO_OPEN.lock();
        if queue.is_empty() { None } else { Some(queue.remove(0)) }
    };
    if let Some(path) = handed {
        player.load(&path);
    }

    window::run("lecteur", "Lecteur audio", WIDTH, HEIGHT, &mut player)
}

pub fn install(now: u64) -> bool {
    super::install_binary(
        &super::BinarySpec {
            path: BINARY,
            title: FILE_NAME,
            app_id: APP_ID,
            display_name: "Lecteur audio",
            format: crate::binfmt::header::BinaryFormat::Cus,
            // The loudspeaker and the account's own files: nothing else.
            permissions: Permissions::AUDIO_OUTPUT | Permissions::FS_USER_HOME,
        },
        now,
    )
}

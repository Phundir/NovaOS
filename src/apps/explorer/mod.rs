//! `explorer.ss` -- file explorer (spec 3.1, System Software).
//!
//! `.ss` and not `.cus`: 3.1 names "l'explorateur" among the applications
//! dedicated to the system. It installs into `/sysapp` (4.3), where the
//! launcher lists it.
//!
//! ## What it does
//!
//! Browse (places, parent, back and forward, a typed path, other disks),
//! sort by name, type or size, open (a directory, a native application, a
//! text file in the notepad), create a directory or an empty file, rename,
//! copy, cut, paste -- directories included, across disks included --,
//! delete with a confirmation, and show the properties of an entry: owner,
//! links, node, the three permission masks of 5.1.
//!
//! ## With whose rights
//!
//! Those of the account in session, looked up by name, and **not**
//! `Credentials::kernel()` as the notepad does. An explorer with the
//! kernel's rights would let any account delete `/sys` with two clicks;
//! with the account's, a refusal is the filesystem's own, and the status
//! line says which permission was missing. With no session the explorer
//! does not start: there is nobody whose rights it could borrow.
//!
//! ## Several entries at once
//!
//! Each row has a box. Actions apply to the marked rows, or to the
//! selected one when none is marked. Not Ctrl+click: pointer events carry
//! no modifiers, and the keyboard driver publishes no event when Ctrl is
//! released, so the explorer cannot know whether Ctrl is held while the
//! mouse clicks. A box is what can be done honestly with that.
//!
//! ## What it does not do
//!
//! No modification date: the nodes store boot ticks, not dates, and a
//! column showing them as dates would be wrong after every reboot. No
//! drag and drop, no thumbnails, no search, no mouse wheel -- the pointer
//! protocol carries none. The clipboard is the explorer's own, not the
//! system's: there is no system clipboard. Named so that nobody looks for
//! them.

mod layout;
pub(crate) mod ops;
mod paint;
pub mod selftest;
pub mod settings;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::perm::{Credentials, Mode};
use crate::gui::client::{Client, Event};
use crate::gui::protocol::{CursorShape, KeyEvent, PointerEvent};
use crate::gui::surface::{Layer, SurfaceId};
use crate::gui::theme::Theme;

use layout::{Action, Hit, Layout};
use ops::{Item, Opening, SortKey};

pub const APP_ID: &str = "os.nova.explorer";
pub const TITLE: &str = "explorer.ss";
/// What the title bar and the task bar show.
pub const WINDOW_TITLE: &str = "Explorateur";
pub const BINARY: &str = "/sysapp/explorer.ss";

const WIDTH: i32 = 820;
const HEIGHT: i32 = 540;

/// Lines of the properties panel, at most.
pub const INFO_LINES: usize = 16;

/// How often the directory shown is read again, in milliseconds.
///
/// Another application writes into it -- the recorder saving, the notepad
/// creating -- and an explorer that only saw it after F5 looked frozen.
/// Once a second, whether the window is in front or not; a read costs a
/// listing and a `stat` per entry, and nothing is repainted unless the
/// listing changed.
const REFRESH_MS: u64 = 1000;

/// Two presses on the same row within this delay open it.
const DOUBLE_CLICK_MS: u64 = 400;

/// Scancodes of set 1: keys with no character.
const SCAN_ESCAPE: u8 = 0x01;
const SCAN_BACKSPACE: u8 = 0x0E;
const SCAN_ENTER: u8 = 0x1C;
const SCAN_SPACE: u8 = 0x39;
const SCAN_F2: u8 = 0x3C;
const SCAN_F5: u8 = 0x3F;
const SCAN_HOME: u8 = 0x47;
const SCAN_UP: u8 = 0x48;
const SCAN_PAGE_UP: u8 = 0x49;
const SCAN_LEFT: u8 = 0x4B;
const SCAN_RIGHT: u8 = 0x4D;
const SCAN_END: u8 = 0x4F;
const SCAN_DOWN: u8 = 0x50;
const SCAN_PAGE_DOWN: u8 = 0x51;
const SCAN_DELETE: u8 = 0x53;

/// What the status line is asking for, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    None,
    /// The path box has the keyboard.
    Path,
    Rename,
    NewFolder,
    NewFile,
    /// Waits for yes or no on `Explorer::pending`.
    ConfirmDelete,
}

impl Prompt {
    fn label(&self) -> &'static str {
        match self {
            Prompt::Rename => "Nouveau nom :",
            Prompt::NewFolder => "Nom du dossier :",
            Prompt::NewFile => "Nom du fichier :",
            _ => "",
        }
    }
}

/// Paths copied or cut, waiting for a paste.
pub struct Clipboard {
    pub paths: Vec<String>,
    pub cut: bool,
}

pub struct Explorer {
    surface: SurfaceId,
    theme: Theme,
    width: i32,
    height: i32,
    who: Credentials,
    user: String,
    cwd: String,
    items: Vec<Item>,
    marked: Vec<bool>,
    selected: Option<usize>,
    /// First listed row shown.
    top: usize,
    sort: SortKey,
    ascending: bool,
    back: Vec<String>,
    forward: Vec<String>,
    clipboard: Option<Clipboard>,
    prompt: Prompt,
    entry: String,
    /// Targets of the pending confirmation.
    pending: Vec<String>,
    message: String,
    /// Is `message` a failure?
    error: bool,
    hover: Hit,
    /// Properties panel, computed when opened.
    info: Option<Vec<String>>,
    /// Row and tick of the last press, for the double click.
    last_click: Option<(usize, u64)>,
    /// `(label, path)` of the side column.
    places: Vec<(String, String)>,
    /// Why the current directory could not be listed.
    listing_error: Option<String>,
    /// Free space of the disk shown, in MiB.
    free_mib: Option<u64>,
    running: bool,
}

impl Explorer {
    fn layout(&self) -> Layout {
        Layout::new(self.width, self.height, &self.theme)
    }

    fn inform(&mut self, text: String) {
        self.message = text;
        self.error = false;
    }

    fn fail(&mut self, text: String) {
        crate::serial_println!("[explorer] {}", text);
        self.message = text;
        self.error = true;
    }

    // --- the side column ---

    fn build_places(&mut self) {
        let home = format!("/users/{}/home", self.user);
        let mut places: Vec<(String, String)> = [
            ("Accueil", home.clone()),
            ("Bureau", format!("{}/desktop", home)),
            ("Documents", format!("{}/documents", home)),
            // `downloads`, the name `fs::tree` creates and SPM writes into
            // -- not the `download/` of 4.3, which no code uses.
            ("Telechargements", format!("{}/downloads", home)),
            ("Images", format!("{}/images", home)),
            ("Videos", format!("{}/videos", home)),
            ("Archives", format!("{}/archive", home)),
            ("Partage", String::from("/users/_shared")),
            ("Applications", String::from("/apps")),
            ("Racine /", String::from("/")),
        ]
        .into_iter()
        .map(|(label, path)| (String::from(label), path))
        .collect();

        // 4.2: every other disk by its letter.
        for disk in crate::fs::disk_list() {
            if disk.letter != crate::fs::BOOT_LETTER {
                places.push((format!("Disque {}:", disk.letter), disk.root()));
            }
        }
        places.truncate(layout::MAX_PLACES);
        self.places = places;
    }

    // --- listing and navigation ---

    /// Reads the directory again if it changed under the window. Returns
    /// `true` when it did, hence when a repaint is due.
    ///
    /// Not while a name is being typed or a deletion confirmed: both are
    /// about the rows as they are on screen, and the row under the prompt
    /// must not move away. The marks and the selection follow their
    /// entries by name, not by row -- a file appearing above them would
    /// otherwise shift the marks onto its neighbours.
    fn auto_refresh(&mut self) -> bool {
        if self.prompt != Prompt::None || self.listing_error.is_some() {
            return false;
        }
        let Ok(fresh) = ops::list(&self.cwd, self.who, self.sort, self.ascending) else {
            return false;
        };
        if fresh == self.items {
            return false;
        }
        let marked: Vec<String> = self
            .items
            .iter()
            .zip(self.marked.iter())
            .filter(|(_, mark)| **mark)
            .map(|(item, _)| item.name.clone())
            .collect();
        let selected = self.selected_item().map(|item| item.name.clone());
        self.items = fresh;
        self.marked = self.items.iter().map(|item| marked.contains(&item.name)).collect();
        self.selected = selected
            .and_then(|name| self.items.iter().position(|item| item.name == name))
            .or(if self.items.is_empty() { None } else { Some(0) });
        if self.info.is_some() {
            self.info = Some(self.properties());
        }
        self.free_mib = crate::fs::disk_mount(ops::disk_of(&self.cwd))
            .ok()
            .map(|disk| disk.free_mib());
        self.follow_selection();
        true
    }

    /// Re-reads the current directory, keeping the selection on the same
    /// name when it is still there.
    fn reload(&mut self) {
        let keep = self.selected_item().map(|item| item.name.clone());
        match ops::list(&self.cwd, self.who, self.sort, self.ascending) {
            Ok(items) => {
                self.items = items;
                self.listing_error = None;
            }
            Err(error) => {
                self.items.clear();
                self.listing_error = Some(String::from(error.message()));
            }
        }
        self.marked = alloc::vec![false; self.items.len()];
        self.selected = keep
            .and_then(|name| self.items.iter().position(|item| item.name == name))
            .or(if self.items.is_empty() { None } else { Some(0) });
        self.free_mib = crate::fs::disk_mount(ops::disk_of(&self.cwd))
            .ok()
            .map(|disk| disk.free_mib());
        self.follow_selection();
    }

    /// Goes to a directory. `record` pushes the one left on the back stack.
    fn go(&mut self, path: &str, record: bool) -> bool {
        let Some(target) = ops::canonical(path) else {
            self.fail(format!("chemin invalide : {}", path));
            return false;
        };
        match crate::fs::stat(&target, self.who) {
            Ok(node) if node.kind == crate::fs::format::NodeKind::Directory => {}
            Ok(_) => {
                self.fail(format!("{} n'est pas un dossier", target));
                return false;
            }
            Err(error) => {
                self.fail(format!("{} : {}", target, error.message()));
                return false;
            }
        }
        if record && target != self.cwd {
            self.back.push(core::mem::replace(&mut self.cwd, target));
            self.forward.clear();
        } else {
            self.cwd = target;
        }
        self.selected = None;
        self.top = 0;
        self.info = None;
        self.message.clear();
        self.reload();
        true
    }

    fn go_back(&mut self) {
        if let Some(previous) = self.back.pop() {
            let here = self.cwd.clone();
            if self.go(&previous, false) {
                self.forward.push(here);
            }
        }
    }

    fn go_forward(&mut self) {
        if let Some(next) = self.forward.pop() {
            let here = self.cwd.clone();
            if self.go(&next, false) {
                self.back.push(here);
            }
        }
    }

    fn go_up(&mut self) {
        let here = String::from(ops::file_name(&self.cwd));
        if let Some(parent) = ops::parent(&self.cwd)
            && self.go(&parent, true)
        {
            // Back in the parent, on the directory just left: the next
            // Enter goes back down, which is what a hand expects.
            if let Some(index) = self.items.iter().position(|item| item.name == here) {
                self.selected = Some(index);
                self.follow_selection();
            }
        }
    }

    // --- selection ---

    fn selected_item(&self) -> Option<&Item> {
        self.selected.and_then(|index| self.items.get(index))
    }

    fn path_of(&self, item: &Item) -> String {
        ops::join(&self.cwd, &item.name)
    }

    /// What the actions apply to: the marked rows, else the selected one.
    fn targets(&self) -> Vec<String> {
        let marked: Vec<String> = self
            .items
            .iter()
            .zip(self.marked.iter())
            .filter(|(_, marked)| **marked)
            .map(|(item, _)| self.path_of(item))
            .collect();
        if !marked.is_empty() {
            return marked;
        }
        self.selected_item()
            .map(|item| alloc::vec![self.path_of(item)])
            .unwrap_or_default()
    }

    fn select(&mut self, index: usize) {
        if index < self.items.len() {
            self.selected = Some(index);
            self.follow_selection();
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let current = self.selected.unwrap_or(0) as isize;
        let last = self.items.len() as isize - 1;
        self.select((current + delta).clamp(0, last) as usize);
    }

    fn follow_selection(&mut self) {
        let rows = self.layout().visible_rows();
        let max_top = self.items.len().saturating_sub(rows);
        if let Some(index) = self.selected {
            if index < self.top {
                self.top = index;
            } else if index >= self.top + rows {
                self.top = index + 1 - rows;
            }
        }
        self.top = self.top.min(max_top);
    }

    fn toggle_mark(&mut self, index: usize) {
        if let Some(mark) = self.marked.get_mut(index) {
            *mark = !*mark;
        }
    }

    fn toggle_all_marks(&mut self) {
        let all = !self.marked.is_empty() && self.marked.iter().all(|mark| *mark);
        for mark in self.marked.iter_mut() {
            *mark = !all;
        }
    }

    /// Left side of the status line.
    fn summary(&self) -> String {
        let mut text = format!("{} element(s)", self.items.len());
        let marked = self.marked.iter().filter(|mark| **mark).count();
        if marked != 0 {
            text.push_str(&format!(", {} marque(s)", marked));
        }
        if let Some(clip) = &self.clipboard {
            text.push_str(&format!(
                " - {} {}",
                clip.paths.len(),
                if clip.cut { "a deplacer" } else { "a copier" }
            ));
        }
        if let Some(free) = self.free_mib {
            text.push_str(&format!(" - {} Mio libres", free));
        }
        text
    }

    // --- actions ---

    /// Is an action possible right now? Drives the look of its button and
    /// whether it answers.
    fn can(&self, action: Action) -> bool {
        let listed = self.listing_error.is_none();
        let has_target = self.selected.is_some() || self.marked.iter().any(|mark| *mark);
        match action {
            Action::NewFolder | Action::NewFile => listed,
            Action::Rename => listed && self.selected.is_some(),
            Action::Copy | Action::Cut | Action::Delete => listed && has_target,
            Action::Paste => listed && self.clipboard.is_some(),
            Action::Info => true,
        }
    }

    fn start_prompt(&mut self, prompt: Prompt, entry: String) {
        self.prompt = prompt;
        self.entry = entry;
        self.info = None;
        self.message.clear();
    }

    fn perform(&mut self, action: Action) {
        if !self.can(action) {
            return;
        }
        match action {
            Action::NewFolder => {
                let taken = self.names();
                self.start_prompt(Prompt::NewFolder, ops::unique_name(&taken, "nouveau dossier"));
            }
            Action::NewFile => {
                let taken = self.names();
                self.start_prompt(Prompt::NewFile, ops::unique_name(&taken, "nouveau fichier.txt"));
            }
            Action::Rename => {
                if let Some(name) = self.selected_item().map(|item| item.name.clone()) {
                    self.start_prompt(Prompt::Rename, name);
                }
            }
            Action::Copy | Action::Cut => {
                let paths = self.targets();
                let cut = action == Action::Cut;
                self.inform(format!(
                    "{} element(s) {} - Coller dans le dossier voulu",
                    paths.len(),
                    if cut { "coupe(s)" } else { "copie(s)" }
                ));
                self.clipboard = Some(Clipboard { paths, cut });
                for mark in self.marked.iter_mut() {
                    *mark = false;
                }
            }
            Action::Paste => self.paste(),
            Action::Delete => {
                self.pending = self.targets();
                self.info = None;
                // The "Explorateur" tab of the settings may turn the
                // question off; read at the moment of deleting, so that the
                // change applies without reopening the window.
                if settings::confirms_delete(&self.user) {
                    self.prompt = Prompt::ConfirmDelete;
                } else {
                    self.delete_pending(crate::idt::ticks());
                }
            }
            Action::Info => {
                self.info = if self.info.is_some() { None } else { Some(self.properties()) };
            }
        }
    }

    fn names(&self) -> Vec<String> {
        self.items.iter().map(|item| item.name.clone()).collect()
    }

    fn confirm_prompt(&mut self, now: u64) {
        let prompt = self.prompt;
        self.prompt = Prompt::None;
        let entry = String::from(self.entry.trim());

        match prompt {
            Prompt::Path => {
                // A relative path is taken from the directory shown.
                let target = if entry.starts_with('/') || entry.as_bytes().get(1) == Some(&b':') {
                    entry
                } else {
                    ops::join(&self.cwd, &entry)
                };
                self.go(&target, true);
            }
            Prompt::Rename => {
                let Some(old) = self.selected_item().map(|item| item.name.clone()) else {
                    return;
                };
                if entry == old {
                    return;
                }
                if let Some(problem) = ops::name_problem(&entry) {
                    self.fail(String::from(problem));
                    return;
                }
                if self.items.iter().any(|item| item.name == entry) {
                    self.fail(format!("{} existe deja", entry));
                    return;
                }
                let path = ops::join(&self.cwd, &old);
                match crate::fs::rename(&path, &entry, self.who, now) {
                    Ok(()) => {
                        crate::serial_println!("[explorer] renomme \"{}\" en \"{}\"", path, entry);
                        self.inform(format!("renomme en {}", entry));
                        self.reload();
                        if let Some(index) = self.items.iter().position(|item| item.name == entry) {
                            self.select(index);
                        }
                    }
                    Err(error) => self.fail(ops::describe(&(path, error))),
                }
            }
            Prompt::NewFolder | Prompt::NewFile => {
                if let Some(problem) = ops::name_problem(&entry) {
                    self.fail(String::from(problem));
                    return;
                }
                // Checked here: `create_directory` accepts an existing
                // directory silently (it creates missing parents), and
                // `write_file` would overwrite an existing file.
                if self.items.iter().any(|item| item.name == entry) {
                    self.fail(format!("{} existe deja", entry));
                    return;
                }
                let path = ops::join(&self.cwd, &entry);
                let result = if prompt == Prompt::NewFolder {
                    crate::fs::create_directory(&path, self.who, Mode::private_dir(), now)
                } else {
                    crate::fs::write_file(&path, &[], self.who, Mode::private_file(), now)
                };
                match result {
                    Ok(_) => {
                        crate::serial_println!("[explorer] cree \"{}\"", path);
                        self.inform(format!("cree : {}", entry));
                        self.reload();
                        if let Some(index) = self.items.iter().position(|item| item.name == entry) {
                            self.select(index);
                        }
                    }
                    Err(error) => self.fail(ops::describe(&(path, error))),
                }
            }
            Prompt::ConfirmDelete => self.delete_pending(now),
            Prompt::None => {}
        }
    }

    fn delete_pending(&mut self, now: u64) {
        let targets = core::mem::take(&mut self.pending);
        let mut deleted = 0;
        for path in targets.iter() {
            match ops::delete_tree(path, self.who, now) {
                Ok(count) => deleted += count,
                Err((count, failure)) => {
                    deleted += count;
                    self.reload();
                    self.fail(format!(
                        "{} element(s) supprime(s), puis arret - {}",
                        deleted,
                        ops::describe(&failure)
                    ));
                    return;
                }
            }
        }
        crate::serial_println!(
            "[explorer] {} element(s) supprime(s) sous \"{}\"",
            deleted,
            self.cwd
        );
        // A deleted path must not stay in the clipboard: pasting it would
        // fail with "introuvable" at a moment unrelated to the deletion.
        if let Some(clip) = &mut self.clipboard {
            clip.paths
                .retain(|path| !targets.iter().any(|gone| ops::is_inside(path, gone)));
            if clip.paths.is_empty() {
                self.clipboard = None;
            }
        }
        self.reload();
        self.inform(format!("{} element(s) supprime(s)", deleted));
    }

    fn paste(&mut self) {
        let Some(clip) = self.clipboard.take() else {
            return;
        };
        let now = crate::idt::ticks();
        let mut taken = self.names();
        let mut done = 0;
        let mut failure = None;

        // A cut on the same disk is only links changing directory (4.1):
        // all of them go to the filesystem at once, and it writes to the
        // disk once. One by one, each move was a flush and a line on the
        // serial console -- a few milliseconds each, seconds for a
        // thousand files.
        let disk = ops::disk_of(&self.cwd);
        let (mut batch, mut singles): (Vec<(String, String)>, Vec<&String>) = (Vec::new(), Vec::new());
        for source in clip.paths.iter() {
            if !clip.cut || ops::disk_of(source) != disk {
                singles.push(source);
                continue;
            }
            if ops::parent(source).as_deref() == Some(self.cwd.as_str()) {
                continue;
            }
            if ops::is_inside(&self.cwd, source) {
                failure = Some(format!("{} : destination dans la source", ops::file_name(source)));
                break;
            }
            let target_name = ops::unique_name(&taken, ops::file_name(source));
            batch.push((source.clone(), ops::join(&self.cwd, &target_name)));
            taken.push(target_name);
        }
        if failure.is_none() && !batch.is_empty() {
            match crate::fs::move_entries(&batch, self.who, now) {
                Ok(moved) => done += moved,
                Err((moved, error)) => {
                    done += moved;
                    let source = batch.get(moved).map(|(from, _)| from.clone()).unwrap_or_default();
                    failure = Some(ops::describe(&(source, error)));
                }
            }
            crate::serial_println!(
                "[explorer] {} element(s) deplace(s) vers \"{}\" - liens reecrits, aucune copie",
                done,
                self.cwd
            );
        }
        let singles: Vec<String> = if failure.is_some() {
            Vec::new()
        } else {
            singles.into_iter().cloned().collect()
        };

        for source in singles.iter() {
            let name = String::from(ops::file_name(source));
            // Cutting and pasting in the same directory is no move at all.
            if clip.cut && ops::parent(source).as_deref() == Some(self.cwd.as_str()) {
                continue;
            }
            if ops::is_inside(&self.cwd, source) {
                failure = Some(format!("{} : destination dans la source", name));
                break;
            }
            let target_name = ops::unique_name(&taken, &name);
            let target = ops::join(&self.cwd, &target_name);
            let result = if clip.cut {
                ops::move_tree(source, &target, self.who, now).map(|_| 1)
            } else {
                ops::copy_tree(source, &target, self.who, now)
            };
            match result {
                Ok(count) => {
                    done += count;
                    taken.push(target_name);
                    crate::serial_println!(
                        "[explorer] {} \"{}\" vers \"{}\"",
                        if clip.cut { "deplace" } else { "copie" },
                        source,
                        target
                    );
                }
                Err(error) => {
                    failure = Some(ops::describe(&error));
                    break;
                }
            }
        }

        // A copy can be pasted again; a cut cannot -- its sources are gone.
        if !clip.cut {
            self.clipboard = Some(clip);
        } else if failure.is_some() {
            // A move that stopped halfway keeps what was not moved, so that
            // the paste can be retried after fixing the cause.
            let remaining: Vec<String> = clip
                .paths
                .into_iter()
                .filter(|path| crate::fs::stat(path, self.who).is_ok())
                .collect();
            if !remaining.is_empty() {
                self.clipboard = Some(Clipboard { paths: remaining, cut: true });
            }
        }

        self.reload();
        match failure {
            Some(text) => self.fail(text),
            None => self.inform(format!("{} element(s) colle(s)", done)),
        }
    }

    /// Lines of the properties panel for the selected entry, or for the
    /// directory shown when nothing is selected.
    fn properties(&self) -> Vec<String> {
        let path = match self.selected_item() {
            Some(item) => self.path_of(item),
            None => self.cwd.clone(),
        };
        let mut lines = alloc::vec![format!("Proprietes de {}", ops::file_name(&path))];
        match crate::fs::stat(&path, self.who) {
            Err(error) => lines.push(format!("illisible : {}", error.message())),
            Ok(node) => {
                let is_dir = node.kind == crate::fs::format::NodeKind::Directory;
                lines.push(format!("Chemin      {}", path));
                lines.push(format!("Type        {}", ops::kind_label(ops::file_name(&path), is_dir)));
                if is_dir {
                    match crate::fs::list(&path, self.who) {
                        Ok(entries) => lines.push(format!("Contenu     {} element(s)", entries.len())),
                        Err(error) => lines.push(format!("Contenu     {}", error.message())),
                    }
                } else {
                    lines.push(format!("Taille      {} ({} octets)", ops::format_size(node.size), node.size));
                }
                let owner = crate::fs::account_of(node.owner)
                    .map(|(name, role, _)| format!("{} ({})", name, role.as_str()))
                    .unwrap_or_else(|| format!("uid {}", node.owner));
                lines.push(format!("Proprietaire {}", owner));
                lines.push(format!("Groupe      gid {}", node.group));
                lines.push(format!("Noeud       {} - {} lien(s)", node.id, node.links));
                lines.push(String::new());
                lines.push(format!("Proprietaire {}", node.perm_owner.compact()));
                lines.push(format!("Groupe       {}", node.perm_group.compact()));
                lines.push(format!("Autres       {}", node.perm_other.compact()));
                lines.push(String::new());
                lines.push(String::from("r lire  w ecrire  x executer  o ecraser  d supprimer"));
                lines.push(String::from("n renommer  a acceder  l lier  m deplacer  c copier"));
                lines.push(String::from("p chmod  q proprietaire  u audit"));
            }
        }
        lines.truncate(INFO_LINES);
        lines
    }

    /// Opens an entry: enters a directory, starts an application, hands
    /// a text to the notepad.
    fn open(&mut self, index: usize) {
        let Some(item) = self.items.get(index).cloned() else {
            return;
        };
        let path = self.path_of(&item);
        match ops::opening(&path, &item, self.who) {
            Ok(Opening::Directory) => {
                self.go(&path, true);
            }
            Ok(Opening::Launch(file_name)) => match crate::apps::launch(file_name) {
                Some(pid) => self.inform(format!("{} lance (PID {})", file_name, pid)),
                None => self.fail(format!("{} : lancement impossible", file_name)),
            },
            Ok(Opening::Text) => {
                crate::apps::notepad::open_on_start(&path);
                match crate::apps::launch(crate::apps::notepad::TITLE) {
                    Some(_) => self.inform(format!("{} ouvert dans le bloc-notes", item.name)),
                    None => self.fail(String::from("bloc-notes introuvable")),
                }
            }
            Ok(Opening::Audio) => {
                crate::apps::player::open_on_start(&path);
                match crate::apps::launch(crate::apps::player::FILE_NAME) {
                    Some(_) => self.inform(format!("{} ouvert dans le lecteur audio", item.name)),
                    None => self.fail(String::from("lecteur audio introuvable")),
                }
            }
            Ok(Opening::Unknown) => {
                self.fail(format!("{} : aucune application ne lit ce format", item.name));
            }
            Err(error) => self.fail(ops::describe(&(path, error))),
        }
    }

    // --- hit test ---

    fn hit(&self, x: i32, y: i32) -> Hit {
        let layout = self.layout();

        if self.prompt == Prompt::ConfirmDelete {
            if layout.answer(true).contains(x, y) {
                return Hit::Yes;
            }
            if layout.answer(false).contains(x, y) {
                return Hit::No;
            }
            return Hit::Nothing;
        }

        // Disabled controls are not zones: a click there does nothing, so
        // they must not look as if it did.
        if layout.back().contains(x, y) {
            return if self.back.is_empty() { Hit::Nothing } else { Hit::Back };
        }
        if layout.forward().contains(x, y) {
            return if self.forward.is_empty() { Hit::Nothing } else { Hit::Forward };
        }
        if layout.up().contains(x, y) {
            return if ops::parent(&self.cwd).is_some() { Hit::Parent } else { Hit::Nothing };
        }
        if layout.path_box().contains(x, y) {
            return Hit::PathBox;
        }
        for (index, action) in Action::ALL.iter().enumerate() {
            if layout.action(index).contains(x, y) {
                return if self.can(*action) { Hit::Action(*action) } else { Hit::Nothing };
            }
        }
        for index in 0..self.places.len() {
            let rect = layout.place(index);
            if rect.bottom() <= layout.sidebar.bottom() && rect.contains(x, y) {
                return Hit::Place(index);
            }
        }
        for key in [SortKey::Name, SortKey::Kind, SortKey::Size] {
            if layout.header_cell(key).contains(x, y) {
                return Hit::Header(key);
            }
        }
        if self.info.is_none() {
            if let Some(thumb) = layout.thumb(self.items.len(), self.top)
                && layout.scrollbar.contains(x, y)
            {
                return if y < thumb.y {
                    Hit::PageUp
                } else if y >= thumb.bottom() {
                    Hit::PageDown
                } else {
                    Hit::Nothing
                };
            }
            for rank in 0..layout.visible_rows() {
                let index = self.top + rank;
                if index >= self.items.len() {
                    break;
                }
                if layout.mark_box(rank).contains(x, y) {
                    return Hit::Mark(index);
                }
                if layout.row(rank).contains(x, y) {
                    return Hit::Row(index);
                }
            }
        }
        Hit::Nothing
    }
}

// ============================================================
//  Keyboard
// ============================================================

fn on_key(ex: &mut Explorer, key: &KeyEvent, now: u64) -> bool {
    if !key.pressed {
        return false;
    }
    let control = key.has(KeyEvent::MOD_CTRL);
    let alt = key.has(KeyEvent::MOD_ALT);
    let shift = key.has(KeyEvent::MOD_SHIFT);
    // Letter shortcuts are read from the **character**, not the scancode:
    // the notepad matches scancodes and so answers Ctrl+W on the key that
    // reads Z on an AZERTY keyboard. Here Ctrl+C is the key marked C,
    // whatever the layout. The driver fills the character with Ctrl held.
    let letter = key.character().map(|character| character.to_ascii_lowercase());

    // --- confirmation ---
    if ex.prompt == Prompt::ConfirmDelete {
        match (key.scancode, letter) {
            (SCAN_ENTER, _) | (_, Some('o')) => ex.confirm_prompt(now),
            (SCAN_ESCAPE, _) | (_, Some('n')) => {
                ex.prompt = Prompt::None;
                ex.pending.clear();
                ex.inform(String::from("suppression annulee"));
            }
            _ => return false,
        }
        return true;
    }

    // --- a text entry has the keyboard ---
    if ex.prompt != Prompt::None {
        match key.scancode {
            SCAN_ESCAPE => {
                ex.prompt = Prompt::None;
                ex.inform(String::from("annule"));
            }
            SCAN_ENTER => ex.confirm_prompt(now),
            SCAN_BACKSPACE => {
                ex.entry.pop();
            }
            _ => match key.character() {
                Some(character) if (0x20..=0x7E).contains(&(character as u32)) => {
                    ex.entry.push(character);
                }
                _ => return false,
            },
        }
        return true;
    }

    if control {
        match letter {
            Some('c') => ex.perform(Action::Copy),
            Some('x') => ex.perform(Action::Cut),
            Some('v') => ex.perform(Action::Paste),
            Some('a') => ex.toggle_all_marks(),
            Some('n') if shift => ex.perform(Action::NewFile),
            Some('n') => ex.perform(Action::NewFolder),
            Some('i') => ex.perform(Action::Info),
            Some('l') => {
                let cwd = ex.cwd.clone();
                ex.start_prompt(Prompt::Path, cwd);
            }
            Some('w') => ex.running = false,
            _ => return false,
        }
        return true;
    }

    if alt {
        match key.scancode {
            SCAN_LEFT => ex.go_back(),
            SCAN_RIGHT => ex.go_forward(),
            SCAN_UP => ex.go_up(),
            _ => return false,
        }
        return true;
    }

    let page = ex.layout().visible_rows() as isize;
    match key.scancode {
        SCAN_UP => ex.move_selection(-1),
        SCAN_DOWN => ex.move_selection(1),
        SCAN_PAGE_UP => ex.move_selection(-page),
        SCAN_PAGE_DOWN => ex.move_selection(page),
        SCAN_HOME => ex.move_selection(isize::MIN / 2),
        SCAN_END => ex.move_selection(isize::MAX / 2),
        SCAN_ENTER => {
            if let Some(index) = ex.selected {
                ex.open(index);
            }
        }
        SCAN_BACKSPACE => ex.go_up(),
        SCAN_SPACE => {
            if let Some(index) = ex.selected {
                ex.toggle_mark(index);
                ex.move_selection(1);
            }
        }
        SCAN_F2 => ex.perform(Action::Rename),
        SCAN_F5 => {
            ex.build_places();
            ex.reload();
            ex.inform(String::from("actualise"));
        }
        SCAN_DELETE => ex.perform(Action::Delete),
        SCAN_ESCAPE => {
            if ex.info.is_some() {
                ex.info = None;
            } else if ex.marked.iter().any(|mark| *mark) {
                for mark in ex.marked.iter_mut() {
                    *mark = false;
                }
            } else {
                ex.message.clear();
            }
        }
        _ => return false,
    }
    true
}

// ============================================================
//  Pointer
// ============================================================

/// Returns whether a repaint is due, and the shape the pointer should take.
fn on_pointer(
    ex: &mut Explorer,
    pointer: &PointerEvent,
    origin: (i32, i32),
    down: bool,
    was_down: bool,
    now: u64,
) -> (bool, Option<CursorShape>) {
    let (x, y) = (pointer.x as i32 - origin.0, pointer.y as i32 - origin.1);
    let hit = ex.hit(x, y);

    let wanted = match hit {
        Hit::Nothing => CursorShape::Arrow,
        Hit::PathBox => CursorShape::Text,
        _ => CursorShape::Hand,
    };
    let shape = (pointer.cursor_shape() != wanted).then_some(wanted);

    // Repainted on the boundary, never on the position (entry 42).
    let mut dirty = hit != ex.hover;
    ex.hover = hit;

    if !down || was_down {
        return (dirty, shape);
    }
    dirty = true;

    // A click outside a field ends its entry. For a name it is also a
    // matter of correctness: the rename applies to the **selected** row,
    // and a click on another row would otherwise change what "Entree"
    // renames while the old name is still in the field.
    match ex.prompt {
        Prompt::Path if hit != Hit::PathBox => ex.prompt = Prompt::None,
        Prompt::Rename | Prompt::NewFolder | Prompt::NewFile => {
            ex.prompt = Prompt::None;
            ex.inform(String::from("annule"));
        }
        _ => {}
    }

    match hit {
        Hit::Nothing => {}
        Hit::Back => ex.go_back(),
        Hit::Forward => ex.go_forward(),
        Hit::Parent => ex.go_up(),
        Hit::PathBox => {
            if ex.prompt == Prompt::None {
                let cwd = ex.cwd.clone();
                ex.start_prompt(Prompt::Path, cwd);
            }
        }
        Hit::Action(action) => ex.perform(action),
        Hit::Place(index) => {
            if let Some((_, path)) = ex.places.get(index).cloned() {
                ex.go(&path, true);
            }
        }
        Hit::Header(key) => {
            if ex.sort == key {
                ex.ascending = !ex.ascending;
            } else {
                ex.sort = key;
                ex.ascending = true;
            }
            let keep = ex.selected_item().map(|item| item.name.clone());
            let marked: Vec<String> = ex
                .items
                .iter()
                .zip(ex.marked.iter())
                .filter(|(_, mark)| **mark)
                .map(|(item, _)| item.name.clone())
                .collect();
            ops::sort(&mut ex.items, ex.sort, ex.ascending);
            // The marks and the selection follow their entries, not their
            // row numbers.
            ex.marked = ex.items.iter().map(|item| marked.contains(&item.name)).collect();
            ex.selected = keep.and_then(|name| ex.items.iter().position(|item| item.name == name));
            ex.follow_selection();
        }
        Hit::Mark(index) => ex.toggle_mark(index),
        Hit::Row(index) => {
            ex.info = None;
            let per_tick = 1000 / crate::TIMER_HZ as u64;
            let double = matches!(ex.last_click, Some((row, at))
                if row == index && now.saturating_sub(at) <= DOUBLE_CLICK_MS.div_ceil(per_tick));
            ex.select(index);
            if double {
                ex.last_click = None;
                ex.open(index);
            } else {
                ex.last_click = Some((index, now));
            }
        }
        Hit::PageUp => {
            let rows = ex.layout().visible_rows();
            ex.top = ex.top.saturating_sub(rows);
        }
        Hit::PageDown => {
            let rows = ex.layout().visible_rows();
            ex.top = (ex.top + rows).min(ex.items.len().saturating_sub(rows));
        }
        Hit::Yes => ex.confirm_prompt(now),
        Hit::No => {
            ex.prompt = Prompt::None;
            ex.pending.clear();
            ex.inform(String::from("suppression annulee"));
        }
    }
    (dirty, shape)
}

// ============================================================
//  The task
// ============================================================

pub fn task() -> ! {
    // Whose rights, before anything is drawn: see the module header.
    let Some(user) = crate::gui::session_user() else {
        crate::serial_println!("[explorer] aucune session ouverte - non demarre");
        crate::scheduler::exit_current_with(-1);
    };
    let Some(who) = crate::fs::credentials_of(&user) else {
        crate::serial_println!("[explorer] compte \"{}\" introuvable - non demarre", user);
        crate::scheduler::exit_current_with(-1);
    };

    let Some(mut client) = Client::connect(5_000) else {
        crate::serial_println!("[explorer] compositeur injoignable");
        crate::scheduler::exit_current_with(-1);
    };
    let Some((screen_w, screen_h, _, _)) = client.screen_info() else {
        crate::scheduler::exit_current_with(-1);
    };
    let theme = client.theme().unwrap_or_default();

    let width = WIDTH.min(screen_w as i32 - 40);
    let height = HEIGHT.min(screen_h as i32 - 80);
    let x = (screen_w as i32 - width) / 2;
    let y = (screen_h as i32 - height) / 2;

    let Some(surface) = client.create_surface(Layer::Window, x, y, width, height, WINDOW_TITLE) else {
        crate::serial_println!("[explorer] creation de surface refusee");
        crate::scheduler::exit_current_with(-1);
    };

    let mut ex = Explorer {
        surface,
        theme,
        width,
        height,
        who,
        user: user.clone(),
        cwd: String::from("/"),
        items: Vec::new(),
        marked: Vec::new(),
        selected: None,
        top: 0,
        sort: settings::default_sort(&user),
        ascending: true,
        back: Vec::new(),
        forward: Vec::new(),
        clipboard: None,
        prompt: Prompt::None,
        entry: String::new(),
        pending: Vec::new(),
        message: String::new(),
        error: false,
        hover: Hit::Nothing,
        info: None,
        last_click: None,
        places: Vec::new(),
        listing_error: None,
        free_mib: None,
        running: true,
    };
    ex.build_places();
    // The home of the account, or the root if the home is not there -- an
    // account created by hand before 4.3's tree existed has none.
    // The folder chosen in the settings, or the home, or the root: an
    // account created by hand before 4.3's tree existed has no home, and a
    // configured folder may since have been deleted.
    let start = settings::start_folder(&user);
    let home = format!("/users/{}/home", user);
    if !ex.go(&start, false) && !ex.go(&home, false) {
        ex.go("/", false);
    }

    crate::serial_println!(
        "[explorer] surface {} - {}x{} - compte \"{}\" (uid {}) - {} ({} element(s))",
        surface,
        width,
        height,
        user,
        who.uid,
        ex.cwd,
        ex.items.len()
    );

    paint::paint(&mut client, &ex);

    let mut pointer_down = false;

    // Woken on a period rather than only on an event: the listing is read
    // again every `REFRESH_MS`, see `auto_refresh`. The whole box is
    // emptied on each wake, the lesson of `gui::modules::run`.
    let per_tick = 1000 / crate::TIMER_HZ as u64;
    let refresh_ticks = REFRESH_MS.div_ceil(per_tick);
    let mut last_refresh = crate::idt::ticks();

    loop {
        let mut dirty = false;
        for _ in 0..crate::ipc::MAX_QUEUE_LEN {
            let Some(event) = client.poll_event() else {
                break;
            };
            let now = crate::idt::ticks();
            dirty |= on_event(&mut ex, &mut client, &event, surface, (x, y), &mut pointer_down, now);
        }

        if !ex.running {
            client.destroy_surface(surface);
            crate::serial_println!("[explorer] ferme - surface rendue");
            crate::scheduler::exit_current_with(0);
        }

        let now = crate::idt::ticks();
        if now.saturating_sub(last_refresh) >= refresh_ticks {
            last_refresh = now;
            dirty |= ex.auto_refresh();
        }

        if dirty {
            paint::paint(&mut client, &ex);
        }

        let _ = crate::syscall::invoke(
            crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
            [super::window::PERIOD_MS, 0, 0, 0, 0, 0],
        );
    }
}

/// One event of the window. Returns `true` if a repaint is due.
fn on_event(
    ex: &mut Explorer,
    client: &mut Client,
    event: &Event,
    surface: SurfaceId,
    fallback: (i32, i32),
    pointer_down: &mut bool,
    now: u64,
) -> bool {
    match event {
        Event::Key(key) => on_key(ex, key, now),
        Event::Pointer(pointer) => {
            // Asked on every event rather than remembered: the window is
            // dragged by the compositor without this task knowing.
            let origin = crate::gui::surface_origin(surface).unwrap_or(fallback);
            let down = pointer.pressed(PointerEvent::BUTTON_LEFT);
            let was_down = *pointer_down;
            *pointer_down = down;
            let (repaint, shape) = on_pointer(ex, pointer, origin, down, was_down, now);
            if let Some(shape) = shape {
                client.set_cursor(shape);
            }
            repaint
        }
        Event::Close(id) if *id == surface => {
            ex.running = false;
            false
        }
        Event::Focus { .. } | Event::Redraw => true,
        Event::Other(message) => {
            if crate::gui::stop_requested(message) {
                ex.running = false;
            }
            false
        }
        _ => false,
    }
}

// ============================================================
//  Installation of the binary
// ============================================================

/// Writes `/sysapp/explorer.ss` if it is not there.
///
/// Same shape as `notepad::install`: a real assembled binary, whose header
/// the checks of 3.3 can read, while the code that runs is the native one
/// of `apps::NATIVE_APPS` -- the limit that section of `apps` names.
pub fn install(now: u64) -> bool {
    use crate::binfmt::header::{BinaryFormat, SignatureType, flags};
    use crate::binfmt::identity::{Permissions, Version};

    if !crate::fs::is_ready() {
        return false;
    }
    let credentials = Credentials::kernel();
    if let Ok(existing) = crate::fs::read_file(BINARY, credentials) {
        // Re-registered on every boot: the file survives, the table of
        // official digests does not (see `notepad::install`).
        let _ = crate::binfmt::sample::register_as_official(&existing);
        return false;
    }

    let source = "\
        .text\n\
        _start:\n\
            mov rax, 0x0001\n\
            lea rdi, [rip + titre]\n\
            mov rsi, titre_fin - titre\n\
            syscall\n\
            mov rax, 0x020B\n\
            mov rdi, 0\n\
            syscall\n\
            hlt\n\
        .data\n\
        titre:\n\
            db \"explorer.ss\"\n\
        titre_fin:\n";

    let Ok(object) = crate::sdk::asm::assemble(source) else {
        return false;
    };
    let spec = crate::sdk::emit::EmitSpec {
        format: BinaryFormat::Ss,
        signature: SignatureType::System,
        app_id: APP_ID,
        author: "Nova OS",
        group: "os.nova",
        display_name: "Explorateur",
        version: Version::new(1, 0, 0, 1),
        // The whole tree: an explorer that could only see its own home
        // would not be one. 12.4 allows FS_FULL to a `.ss`. What it may
        // really touch is then decided node by node by the account's
        // rights -- see the header.
        permissions: Permissions::FS_USER_HOME | Permissions::FS_FULL,
        flags: flags::GUI,
        ports: &[],
        stack_size: 64 * 1024,
        authority: None,
    };
    let Ok(image) = crate::sdk::emit::emit(&object, &spec) else {
        return false;
    };
    let _ = crate::binfmt::sample::register_as_official(&image);

    crate::fs::write_file(BINARY, &image, credentials, Mode::system_binary(), now).is_ok()
}

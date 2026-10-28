//! `notepad.cus` -- notepad (spec 3.1, Common Utility Software).
//!
//! ## An application, not a module
//!
//! The six components of 8.2 are `.mod` files: they occupy a slot and the
//! compositor starts them. This is an **application**: it lives in the
//! `Window` layer, it has a title, it can be moved and closed. So it drives
//! `gui::client::Client` itself with a loop of its own, as the retired
//! `devstudio.ss` did, rather than going through the common canvas of `gui::modules`
//! whose loop is built for modules with neither title nor close.
//!
//! `.cus` and not `.ss`: 3.1 reserves `.ss` for applications dedicated to
//! the system -- the explorer, the task manager -- and makes `.cus`
//! "ordinary applications". A notepad is the definition of an ordinary
//! application.
//!
//! ## The layout
//!
//! ```text
//!   +--------------------------------------------------+
//!   | notes.txt *            Ctrl+S  Ctrl+O  Ctrl+N    |  toolbar
//!   +--------------------------------------------------+
//!   | text, one line per line, a caret                 |
//!   |                                                  |
//!   +--------------------------------------------------+
//!   | l. 3 c. 12          | prompt, when one is open   |  status
//!   +--------------------------------------------------+
//! ```
//!
//! ## Where the caret comes from
//!
//! Two sources, and they do not clamp the same way. A key asks to keep its
//! column across a short line; a **click** asks for the exact spot it
//! pointed at. Both go through `TextBuf`, which is where that difference is
//! written down.
//!
//! A click is converted by dividing by the glyph box, and the division is
//! **rounded to the nearest gap** rather than truncated: clicking the right
//! half of a character means "after it", which is what a hand doing it
//! expects and what truncation gets wrong by one everywhere.
//!
//! ## What it does not do
//!
//! No selection, no copy-paste, no undo, no scroll bar, no word wrap. A
//! line longer than the window is cut at the edge and the view follows the
//! caret horizontally. Those are named so that nobody looks for them.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::perm::{Credentials, Mode};
use crate::vga::SpinLock;
use crate::gui::client::{Client, Event};
use crate::gui::protocol::{CursorShape, KeyEvent, PointerEvent};
use crate::gui::surface::{Layer, Rect, SurfaceId};
use crate::gui::theme::Theme;
use crate::textbuf::{Edit, TextBuf};

pub const APP_ID: &str = "os.nova.notepad";
pub const TITLE: &str = "notepad.cus";
/// What the title bar and the task bar show: the name of the application,
/// not of its file.
pub const WINDOW_TITLE: &str = "Bloc-notes";
pub const BINARY: &str = "/apps/notepad.cus";

const WIDTH: i32 = 620;
const HEIGHT: i32 = 420;

/// Scancodes of set 1 used here.
const SCAN_ESCAPE: u8 = 0x01;
const SCAN_BACKSPACE: u8 = 0x0E;
const SCAN_ENTER: u8 = 0x1C;
const SCAN_LEFT: u8 = 0x4B;
const SCAN_RIGHT: u8 = 0x4D;
const SCAN_UP: u8 = 0x48;
const SCAN_DOWN: u8 = 0x50;
const SCAN_HOME: u8 = 0x47;
const SCAN_END: u8 = 0x4F;
const SCAN_DELETE: u8 = 0x53;
/// `q s o n w` on a QWERTY scancode set -- `a s o n z` on AZERTY.
const SCAN_S: u8 = 0x1F;
const SCAN_O: u8 = 0x18;
const SCAN_N: u8 = 0x31;
const SCAN_W: u8 = 0x11;

/// What the bottom line is asking for, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Prompt {
    /// No prompt: the status line shows the caret position.
    None,
    /// A path to open.
    Open,
    /// A path to write to.
    SaveAs,
}

impl Prompt {
    fn label(&self) -> &'static str {
        match self {
            Prompt::None => "",
            Prompt::Open => "Ouvrir :",
            Prompt::SaveAs => "Enregistrer sous :",
        }
    }
}

struct Notepad {
    surface: SurfaceId,
    theme: Theme,
    width: i32,
    height: i32,
    buffer: TextBuf,
    /// First line displayed, and first column: there is no wrapping.
    top: usize,
    left: usize,
    prompt: Prompt,
    entry: String,
    /// Names of the directory the prompt is pointing into, for the listing.
    suggestions: Vec<String>,
    message: String,
    /// Rank of the suggestion under the pointer, or `usize::MAX`.
    hover: usize,
    running: bool,
}

/// Geometry of the window, computed from its size alone.
///
/// One arithmetic, read by the drawing **and** by the hit test. Two would
/// drift, and here the symptom would be the worst of its kind: a click
/// placing the caret one line away from where it was aimed.
struct Layout {
    glyph_w: i32,
    glyph_h: i32,
    padding: i32,
    toolbar: Rect,
    text: Rect,
    status: Rect,
    /// Height of a text row, glyph height plus the line gap.
    row_h: i32,
}

impl Layout {
    fn new(width: i32, height: i32, theme: &Theme) -> Self {
        let glyph_w = crate::graphics::font::GLYPH_WIDTH as i32;
        let glyph_h = crate::graphics::font::GLYPH_HEIGHT as i32;
        let padding = theme.padding as i32;
        let bar = glyph_h + 2 * padding;
        Layout {
            glyph_w,
            glyph_h,
            padding,
            toolbar: Rect::new(0, 0, width, bar),
            text: Rect::new(0, bar, width, height - 2 * bar),
            status: Rect::new(0, height - bar, width, bar),
            row_h: glyph_h + 2,
        }
    }

    /// How many lines and columns fit in the text area.
    fn visible_rows(&self) -> usize {
        ((self.text.height - 2 * self.padding) / self.row_h).max(1) as usize
    }

    fn visible_columns(&self) -> usize {
        ((self.text.width - 2 * self.padding) / self.glyph_w).max(1) as usize
    }

    /// Top-left pixel of the character at a visible row and column.
    fn cell(&self, row: i32, column: i32) -> (i32, i32) {
        (
            self.text.x + self.padding + column * self.glyph_w,
            self.text.y + self.padding + row * self.row_h,
        )
    }

    /// Row and column a pixel points at, relative to the view.
    ///
    /// The column is rounded to the **nearest gap between characters**, not
    /// truncated: clicking the right half of a glyph means "after it". A
    /// truncating division puts the caret before the character one clicked
    /// on, which is off by one everywhere and feels like the click missed.
    fn cell_at(&self, x: i32, y: i32) -> (usize, usize) {
        let row = ((y - self.text.y - self.padding).max(0) / self.row_h) as usize;
        let offset = (x - self.text.x - self.padding).max(0);
        let column = ((offset + self.glyph_w / 2) / self.glyph_w) as usize;
        (row, column)
    }

    /// Rectangle of a suggestion row of the prompt listing.
    fn suggestion(&self, rank: usize) -> Rect {
        Rect::new(
            self.text.x + self.padding,
            self.text.y + self.padding + rank as i32 * self.row_h,
            self.text.width - 2 * self.padding,
            self.row_h,
        )
    }
}

/// Files handed over by the explorer, waiting for a notepad to start.
///
/// ## Why a queue in memory and not an argument
///
/// `scheduler::spawn` takes an entry point and nothing else: a native
/// application has no command line to read a path from. The explorer
/// therefore leaves the path here before launching, and the notepad takes
/// the oldest one when it starts. Two quick launches get their files in
/// order; a notepad started from the menu with a path still waiting would
/// take it -- the price of standing in for an `argv` the kernel lacks.
static TO_OPEN: SpinLock<Vec<String>> = SpinLock::new(Vec::new());

/// Asks the next notepad that starts to open `path`.
pub fn open_on_start(path: &str) {
    TO_OPEN.lock().push(String::from(path));
}

/// Where a new file goes by default.
///
/// The documents directory of the account that opened the session. With no
/// session -- which happens when the application is started by the
/// self-test before anybody logged in -- `/tmp` is used, because writing
/// into another account's home would be worse than writing nowhere useful.
fn default_directory() -> String {
    match crate::gui::session_user() {
        Some(user) => format!("/users/{}/home/documents", user),
        None => String::from("/tmp"),
    }
}

/// Credentials the application writes with.
///
/// `Credentials::kernel()`, and it is over-privileged: the kernel does not
/// yet attach an account to a process, a limit the roadmap opened in phase
/// 6 and which no phase has closed. The day it does, this becomes the
/// credentials of the session and the notepad loses the right to write
/// outside its own home -- which is what 5.2 wants.
fn who() -> Credentials {
    Credentials::kernel()
}

impl Notepad {
    fn layout(&self) -> Layout {
        Layout::new(self.width, self.height, &self.theme)
    }

    /// Keeps the caret inside the view, horizontally and vertically.
    fn follow_caret(&mut self, layout: &Layout) {
        let rows = layout.visible_rows();
        let columns = layout.visible_columns();
        let row = self.buffer.row();
        let column = self.buffer.column();

        if row < self.top {
            self.top = row;
        } else if row >= self.top + rows {
            self.top = row + 1 - rows;
        }
        if column < self.left {
            self.left = column;
        } else if column >= self.left + columns {
            self.left = column + 1 - columns;
        }
    }

    fn refresh_suggestions(&mut self) {
        self.suggestions.clear();
        self.hover = usize::MAX;
        // The directory the entry points into: everything up to the last
        // slash. Listing the directory being typed into is what makes the
        // prompt usable without a file browser.
        let directory = match self.entry.rfind('/') {
            Some(0) => String::from("/"),
            Some(index) => String::from(&self.entry[..index]),
            None => return,
        };
        let Ok(listing) = crate::fs::list(&directory, who()) else {
            return;
        };
        for entry in listing {
            self.suggestions.push(entry.name);
        }
        self.suggestions.sort();
    }

    fn open_prompt(&mut self, prompt: Prompt) {
        self.prompt = prompt;
        self.entry = match self.buffer.path() {
            Some(path) => String::from(path),
            None => format!("{}/", default_directory()),
        };
        if prompt == Prompt::Open && self.buffer.path().is_none() {
            self.entry = format!("{}/", default_directory());
        }
        self.message.clear();
        self.refresh_suggestions();
    }

    fn confirm_prompt(&mut self, now: u64) {
        let path = String::from(self.entry.trim());
        let prompt = self.prompt;
        self.prompt = Prompt::None;
        self.suggestions.clear();
        self.hover = usize::MAX;

        if path.is_empty() || path.ends_with('/') {
            self.message = String::from("chemin vide - rien fait");
            return;
        }

        match prompt {
            Prompt::Open => {
                if self.buffer.load(&path, who()) {
                    self.top = 0;
                    self.left = 0;
                    self.message = format!("ouvert : {}", path);
                    crate::serial_println!("[notepad] ouvert \"{}\"", path);
                } else {
                    self.message = format!("lecture impossible : {}", path);
                    crate::serial_println!("[notepad] lecture impossible \"{}\"", path);
                }
            }
            Prompt::SaveAs => {
                if self.buffer.save_as(&path, who(), now) {
                    self.message = format!("enregistre : {}", path);
                    crate::serial_println!(
                        "[notepad] enregistre \"{}\" - {} ligne(s), {} caractere(s)",
                        path,
                        self.buffer.line_count(),
                        self.buffer.char_count()
                    );
                } else {
                    self.message = format!("ecriture impossible : {}", path);
                    crate::serial_println!("[notepad] ecriture impossible \"{}\"", path);
                }
            }
            Prompt::None => {}
        }
    }

    fn save(&mut self, now: u64) {
        if self.buffer.path().is_none() {
            // No name yet: ask for one rather than invent one. A file
            // called `sans-nom.txt` appearing in a directory the user never
            // chose is worse than a prompt.
            self.open_prompt(Prompt::SaveAs);
            return;
        }
        if self.buffer.save(who(), now) {
            let path = String::from(self.buffer.path().unwrap_or(""));
            self.message = format!("enregistre : {}", path);
            crate::serial_println!(
                "[notepad] enregistre \"{}\" - {} ligne(s), {} caractere(s)",
                path,
                self.buffer.line_count(),
                self.buffer.char_count()
            );
        } else {
            self.message = String::from("ecriture impossible");
            crate::serial_println!("[notepad] ecriture impossible");
        }
    }
}

// ============================================================
//  Keyboard
// ============================================================

fn on_key(pad: &mut Notepad, key: &KeyEvent, now: u64) -> bool {
    if !key.pressed {
        return false;
    }
    let control = key.has(KeyEvent::MOD_CTRL);

    // --- the prompt has the keyboard while it is open ---
    if pad.prompt != Prompt::None {
        match key.scancode {
            SCAN_ESCAPE => {
                pad.prompt = Prompt::None;
                pad.suggestions.clear();
                pad.hover = usize::MAX;
                pad.message = String::from("annule");
            }
            SCAN_ENTER => pad.confirm_prompt(now),
            SCAN_BACKSPACE => {
                pad.entry.pop();
                pad.refresh_suggestions();
            }
            _ => {
                if let Some(character) = key.character()
                    && (0x20..=0x7E).contains(&(character as u32))
                {
                    pad.entry.push(character);
                    pad.refresh_suggestions();
                } else {
                    return false;
                }
            }
        }
        return true;
    }

    // --- shortcuts ---
    if control {
        match key.scancode {
            SCAN_S => {
                pad.save(now);
                return true;
            }
            SCAN_O => {
                pad.open_prompt(Prompt::Open);
                return true;
            }
            SCAN_N => {
                pad.buffer.clear();
                pad.top = 0;
                pad.left = 0;
                pad.message = String::from("nouveau document");
                return true;
            }
            SCAN_W => {
                // Closing by keyboard, the same path as the cross of the
                // title bar: one refusal rule, not two.
                pad.running = false;
                return true;
            }
            _ => return false,
        }
    }

    let edit = match key.scancode {
        SCAN_LEFT => pad.buffer.left(),
        SCAN_RIGHT => pad.buffer.right(),
        SCAN_UP => pad.buffer.up(),
        SCAN_DOWN => pad.buffer.down(),
        SCAN_HOME => pad.buffer.home(),
        SCAN_END => pad.buffer.end(),
        SCAN_BACKSPACE => pad.buffer.backspace(),
        SCAN_DELETE => pad.buffer.delete(),
        SCAN_ENTER => pad.buffer.newline(),
        SCAN_ESCAPE => {
            pad.message.clear();
            Edit::Moved
        }
        _ => match key.character() {
            Some(character) => pad.buffer.insert(character),
            None => Edit::None,
        },
    };

    if edit.needs_repaint() {
        let layout = pad.layout();
        pad.follow_caret(&layout);
        return true;
    }
    false
}

// ============================================================
//  Pointer
// ============================================================

/// Returns whether a repaint is due, and the shape the pointer should take.
///
/// The shape is **returned** rather than requested here: this function does
/// not own the client, and threading the client through it for one call
/// would put it in every helper's signature. A static would have worked and
/// would have been a second place where the state lives.
fn on_pointer(
    pad: &mut Notepad,
    pointer: &PointerEvent,
    origin: (i32, i32),
    down: bool,
    was_down: bool,
) -> (bool, Option<CursorShape>) {
    let layout = pad.layout();
    let (x, y) = (pointer.x as i32 - origin.0, pointer.y as i32 - origin.1);
    let mut dirty = false;

    // --- cursor shape ---
    //
    // The text area gets the beam, everything else the arrow. Compared
    // against the shape the event carries, so that nothing has to be
    // remembered and the compositor's own reset cannot desynchronise it.
    let wanted = if pad.prompt == Prompt::None && layout.text.contains(x, y) {
        CursorShape::Text
    } else if pad.prompt != Prompt::None && suggestion_at(pad, &layout, x, y).is_some() {
        CursorShape::Hand
    } else {
        CursorShape::Arrow
    };
    let shape = if pointer.cursor_shape() == wanted {
        None
    } else {
        Some(wanted)
    };

    // --- hover on the suggestion list ---
    if pad.prompt != Prompt::None {
        let rank = suggestion_at(pad, &layout, x, y).unwrap_or(usize::MAX);
        if rank != pad.hover {
            pad.hover = rank;
            dirty = true;
        }
    }

    if !down || was_down {
        return (dirty, shape);
    }

    // --- the pressing edge ---
    if pad.prompt != Prompt::None {
        if let Some(rank) = suggestion_at(pad, &layout, x, y)
            && let Some(name) = pad.suggestions.get(rank).cloned()
        {
            // Clicking a listed name completes the entry rather than
            // confirming it: a single click that opened a file would make
            // a mis-aimed click discard unsaved work.
            let directory = match pad.entry.rfind('/') {
                Some(index) => String::from(&pad.entry[..=index]),
                None => String::new(),
            };
            pad.entry = format!("{}{}", directory, name);
            pad.refresh_suggestions();
            return (true, shape);
        }
        return (dirty, shape);
    }

    if layout.text.contains(x, y) {
        let (row, column) = layout.cell_at(x, y);
        // The view offsets are added back: the hit test speaks in visible
        // cells, the buffer in absolute ones.
        let edit = pad.buffer.place(pad.top + row, pad.left + column);
        if edit.needs_repaint() {
            pad.follow_caret(&layout);
            return (true, shape);
        }
    }
    (dirty, shape)
}

/// Which suggestion the pointer is on.
fn suggestion_at(pad: &Notepad, layout: &Layout, x: i32, y: i32) -> Option<usize> {
    let shown = pad.suggestions.len().min(layout.visible_rows());
    (0..shown).find(|rank| layout.suggestion(*rank).contains(x, y))
}

// ============================================================
//  Drawing
// ============================================================

fn paint(client: &mut Client, pad: &Notepad) {
    let layout = pad.layout();
    let theme = &pad.theme;
    let surface = pad.surface;

    client.reset(surface);
    client.fill(surface, theme.background);

    // --- toolbar ---
    client.rect(
        surface,
        layout.toolbar.x,
        layout.toolbar.y,
        layout.toolbar.width,
        layout.toolbar.height,
        theme.surface,
    );
    let name = format!(
        "{}{}",
        pad.buffer.display_name(),
        if pad.buffer.is_dirty() { " *" } else { "" }
    );
    client.text(
        surface,
        layout.padding,
        layout.toolbar.y + layout.padding,
        &name,
        if pad.buffer.is_dirty() {
            theme.warning
        } else {
            theme.text
        },
        None,
    );
    let hints = "Ctrl+S enregistrer  Ctrl+O ouvrir  Ctrl+N nouveau  Ctrl+W fermer";
    let hints_width = layout.glyph_w * hints.len() as i32;
    if hints_width + layout.padding * 2 < layout.toolbar.width - layout.glyph_w * name.len() as i32 {
        client.text(
            surface,
            layout.toolbar.width - hints_width - layout.padding,
            layout.toolbar.y + layout.padding,
            hints,
            theme.text_dim,
            None,
        );
    }
    client.rect(
        surface,
        0,
        layout.toolbar.bottom() - 1,
        layout.toolbar.width,
        1,
        theme.border,
    );

    // --- the prompt takes over the text area ---
    if pad.prompt != Prompt::None {
        paint_suggestions(client, pad, &layout);
    } else {
        paint_text(client, pad, &layout);
    }

    // --- status ---
    client.rect(
        surface,
        layout.status.x,
        layout.status.y,
        layout.status.width,
        layout.status.height,
        theme.surface,
    );
    client.rect(surface, 0, layout.status.y, layout.status.width, 1, theme.border);

    let status = if pad.prompt != Prompt::None {
        format!("{} {}_", pad.prompt.label(), pad.entry)
    } else {
        format!(
            "l. {} c. {}   {} ligne(s)",
            pad.buffer.row() + 1,
            pad.buffer.column() + 1,
            pad.buffer.line_count()
        )
    };
    client.text(
        surface,
        layout.padding,
        layout.status.y + layout.padding,
        &status,
        theme.text,
        None,
    );

    if !pad.message.is_empty() && pad.prompt == Prompt::None {
        let width = layout.glyph_w * pad.message.len() as i32;
        client.text(
            surface,
            layout.status.width - width - layout.padding,
            layout.status.y + layout.padding,
            &pad.message,
            theme.accent,
            None,
        );
    }

    client.commit(surface);
}

fn paint_text(client: &mut Client, pad: &Notepad, layout: &Layout) {
    let theme = &pad.theme;
    let surface = pad.surface;
    let rows = layout.visible_rows();
    let columns = layout.visible_columns();

    for visible in 0..rows {
        let row = pad.top + visible;
        if row >= pad.buffer.line_count() {
            break;
        }
        let line = pad.buffer.line(row);
        // The horizontal view is applied by slicing, not by drawing off
        // screen: a `DRAW_TEXT` whose start is negative would be clipped by
        // the compositor, but its payload would still carry the whole line.
        let shown: String = line.chars().skip(pad.left).take(columns).collect();
        if shown.is_empty() {
            continue;
        }
        let (x, y) = layout.cell(visible as i32, 0);
        client.text(surface, x, y, &shown, theme.text, None);
    }

    // The caret, last, so it is never hidden by a glyph of its own line.
    let row = pad.buffer.row();
    let column = pad.buffer.column();
    if row >= pad.top && row < pad.top + rows && column >= pad.left && column <= pad.left + columns {
        let (x, y) = layout.cell((row - pad.top) as i32, (column - pad.left) as i32);
        // Two pixels wide: one pixel on a caret between two glyphs of eight
        // is hard to find, and the writing bar is the thing a hand looks for
        // first after a click.
        client.rect(surface, x, y, 2, layout.glyph_h, theme.accent);
    }
}

fn paint_suggestions(client: &mut Client, pad: &Notepad, layout: &Layout) {
    let theme = &pad.theme;
    let surface = pad.surface;
    let shown = pad.suggestions.len().min(layout.visible_rows());

    if shown == 0 {
        let (x, y) = layout.cell(0, 0);
        client.text(surface, x, y, "(rien a cet endroit)", theme.text_dim, None);
        return;
    }

    for rank in 0..shown {
        let rect = layout.suggestion(rank);
        let on_it = pad.hover == rank;
        if on_it {
            client.rect(
                surface,
                rect.x,
                rect.y,
                rect.width,
                rect.height,
                crate::graphics::framebuffer::mix(theme.background, theme.surface_alt, 110),
            );
        }
        client.text(
            surface,
            rect.x + layout.padding,
            rect.y,
            &pad.suggestions[rank],
            if on_it { theme.text } else { theme.text_dim },
            None,
        );
    }
}

// ============================================================
//  The task
// ============================================================

pub fn task() -> ! {
    let Some(mut client) = Client::connect(5_000) else {
        crate::serial_println!("[notepad] compositeur injoignable");
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
        crate::serial_println!("[notepad] creation de surface refusee");
        crate::scheduler::exit_current_with(-1);
    };

    let mut pad = Notepad {
        surface,
        theme,
        width,
        height,
        buffer: TextBuf::new(),
        top: 0,
        left: 0,
        prompt: Prompt::None,
        entry: String::new(),
        suggestions: Vec::new(),
        message: String::from("Ctrl+O pour ouvrir un fichier"),
        hover: usize::MAX,
        running: true,
    };

    let handed = {
        let mut queue = TO_OPEN.lock();
        if queue.is_empty() {
            None
        } else {
            Some(queue.remove(0))
        }
    };
    if let Some(path) = handed {
        if pad.buffer.load(&path, who()) {
            pad.message = format!("ouvert : {}", path);
            crate::serial_println!("[notepad] ouvert \"{}\" (depuis l'explorateur)", path);
        } else {
            pad.message = format!("lecture impossible : {}", path);
            crate::serial_println!("[notepad] lecture impossible \"{}\"", path);
        }
    }

    crate::serial_println!(
        "[notepad] surface {} - {}x{} en ({}, {}) - dossier par defaut {}",
        surface,
        width,
        height,
        x,
        y,
        default_directory()
    );

    paint(&mut client, &pad);

    // The window is moved by its title bar, which the compositor handles:
    // the surface's own origin therefore changes without this task being
    // told. It is asked for on every pointer event rather than remembered,
    // because a remembered origin would be wrong from the first drag and a
    // click would then land somewhere else in the text.
    let mut pointer_down = false;

    loop {
        let event = client.next_event();
        let mut dirty = false;

        match &event {
            Event::Key(key) => {
                let now = crate::idt::ticks();
                dirty |= on_key(&mut pad, key, now);
            }

            Event::Pointer(pointer) => {
                let origin = crate::gui::surface_origin(surface).unwrap_or((x, y));
                let down = pointer.pressed(PointerEvent::BUTTON_LEFT);
                let was_down = pointer_down;
                pointer_down = down;
                let (repaint, shape) = on_pointer(&mut pad, pointer, origin, down, was_down);
                dirty |= repaint;
                if let Some(shape) = shape {
                    client.set_cursor(shape);
                }
            }

            // The cross of the title bar, and Alt+F4 the day the window
            // manager sends it here too. The compositor **asks**: it never
            // destroys a surface it does not own, which is what lets an
            // application with unsaved work say something first.
            Event::Close(id) if *id == surface => {
                pad.running = false;
            }

            Event::Focus { .. } | Event::Redraw => dirty = true,

            Event::Other(message) => {
                if crate::gui::stop_requested(message) {
                    pad.running = false;
                }
            }

            _ => {}
        }

        if !pad.running {
            // Unsaved work is reported, not silently dropped. There is
            // nowhere to ask the question yet -- a modal dialog is a
            // feature of its own -- so the serial line is where it goes,
            // and the fact that it is *only* there is a limit, not a
            // design.
            if pad.buffer.is_dirty() {
                crate::serial_println!(
                    "[notepad] fermeture avec {} caractere(s) non enregistre(s)",
                    pad.buffer.char_count()
                );
            }
            client.destroy_surface(surface);
            crate::serial_println!("[notepad] ferme - surface rendue");
            crate::scheduler::exit_current_with(0);
        }

        if dirty {
            paint(&mut client, &pad);
        }
    }
}

// ============================================================
//  Installation of the binary
// ============================================================

/// Writes `/apps/notepad.cus` if it is not there.
///
/// Same shape as every native installer: a real assembled binary rather than
/// a file of `nop`, so that every byte has a reason to be there and the
/// header checks of 3.3 have something true to read.
pub fn install(now: u64) -> bool {
    use crate::binfmt::header::{BinaryFormat, SignatureType, flags};
    use crate::binfmt::identity::{Permissions, Version};

    if !crate::fs::is_ready() {
        return false;
    }
    let credentials = who();
    if let Ok(existing) = crate::fs::read_file(BINARY, credentials) {
        // Re-registering recognises without recreating: the file survives
        // the reboot, the table of official digests does not. Without this,
        // a `.cus` declared SYSTEM would be rejected as `UnknownSystemBinary`
        // on every machine that had already booted once -- the defect
        // the retired `devstudio::install` recorded in the same place.
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
            db \"notepad.cus\"\n\
        titre_fin:\n";

    let Ok(object) = crate::sdk::asm::assemble(source) else {
        return false;
    };
    let spec = crate::sdk::emit::EmitSpec {
        format: BinaryFormat::Cus,
        signature: SignatureType::System,
        app_id: APP_ID,
        author: "Nova OS",
        group: "os.nova",
        display_name: "Bloc-notes",
        version: Version::new(1, 0, 0, 1),
        // Exactly what a notepad needs: its own home to read and write, and
        // nothing else. No network, no clipboard -- there is no copy-paste
        // to serve -- and no system directory.
        permissions: Permissions::FS_USER_HOME,
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

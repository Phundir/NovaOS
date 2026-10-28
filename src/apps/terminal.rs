//! `terminal.ss` -- a window on the shell (16.0.2).
//!
//! The shell (`crate::shell`) is a library; this is its window: the lines
//! it printed, a prompt, the line being typed, a history, scrolling. It
//! runs with the credentials of the session, as any program the user
//! starts: the shell grants nothing the filesystem would refuse.
//!
//! A command runs in the window's task. A `spm install` that downloads
//! freezes the window for the time of the transfer -- said here rather
//! than discovered; a command that lasts will get a task of its own the
//! day one needs it.

use alloc::string::String;
use alloc::vec::Vec;

use super::window::{self, App, Press, Ui};
use crate::binfmt::identity::Permissions;
use crate::gui::protocol::{CursorShape, KeyEvent};
use crate::shell::Shell;

pub const APP_ID: &str = "os.nova.terminal";
pub const FILE_NAME: &str = "terminal.ss";
pub const BINARY: &str = "/sysapp/terminal.ss";

const SCAN_BACKSPACE: u8 = 0x0E;
const SCAN_ENTER: u8 = 0x1C;
const SCAN_LEFT: u8 = 0x4B;
const SCAN_RIGHT: u8 = 0x4D;
const SCAN_UP: u8 = 0x48;
const SCAN_DOWN: u8 = 0x50;
const SCAN_HOME: u8 = 0x47;
const SCAN_END: u8 = 0x4F;
const SCAN_DELETE: u8 = 0x53;
const SCAN_PAGE_UP: u8 = 0x49;
const SCAN_PAGE_DOWN: u8 = 0x51;

const CELL_W: i32 = 8;
const CELL_H: i32 = 16;
const MARGIN: i32 = 6;
/// Lines kept above the screen.
const SCROLLBACK: usize = 2000;

const BACKGROUND: u32 = 0x00_10_14_18;
const TEXT: u32 = 0x00_D8_DC_E0;
const PROMPT: u32 = 0x00_7F_D0_8F;
const ERROR: u32 = 0x00_FF_8A_80;
const DIM: u32 = 0x00_80_88_90;

struct Terminal {
    shell: Shell,
    /// What has been printed: a line and its colour.
    lines: Vec<(String, u32)>,
    input: String,
    cursor: usize,
    /// Position in the history while browsing it; `None`: a new line.
    browsing: Option<usize>,
    /// Lines scrolled up from the bottom.
    scroll: usize,
    cols: usize,
    rows: usize,
    quit: bool,
}

/// What the font can draw: printable ASCII, anything else as `?`.
fn printable(text: &str) -> String {
    text.chars().map(|c| if (' '..='~').contains(&c) { c } else if c == '\t' { ' ' } else { '?' }).collect()
}

impl Terminal {
    fn new() -> Terminal {
        let shell = Shell::for_session();
        let mut terminal = Terminal {
            shell,
            lines: Vec::new(),
            input: String::new(),
            cursor: 0,
            browsing: None,
            scroll: 0,
            cols: 80,
            rows: 24,
            quit: false,
        };
        terminal.push(
            alloc::format!("Nova OS {} - terminal de {}. \"aide\" liste les commandes.", crate::version::VERSION, terminal.shell.user),
            DIM,
        );
        terminal
    }

    /// Adds a line, cut at the width of the window.
    fn push(&mut self, text: String, color: u32) {
        let text = printable(&text);
        let width = self.cols.max(8);
        if text.is_empty() {
            self.lines.push((String::new(), color));
        }
        let mut rest = text.as_str();
        while !rest.is_empty() {
            let cut = rest.len().min(width);
            self.lines.push((String::from(&rest[..cut]), color));
            rest = &rest[cut..];
        }
        if self.lines.len() > SCROLLBACK {
            let excess = self.lines.len() - SCROLLBACK;
            self.lines.drain(..excess);
        }
    }

    fn submit(&mut self) {
        let line = core::mem::take(&mut self.input);
        self.cursor = 0;
        self.browsing = None;
        self.scroll = 0;
        let prompt = self.shell.prompt();
        self.push(alloc::format!("{}{}", prompt, line), PROMPT);
        let output = self.shell.run(&line);
        if output.clear {
            self.lines.clear();
        }
        let color = if output.failed { ERROR } else { TEXT };
        for text in output.lines {
            self.push(text, color);
        }
        if output.exit {
            self.quit = true;
        }
    }

    fn history(&mut self, older: bool) {
        let count = self.shell.history.len();
        if count == 0 {
            return;
        }
        let next = match (self.browsing, older) {
            (None, true) => Some(count - 1),
            (None, false) => None,
            (Some(0), true) => Some(0),
            (Some(i), true) => Some(i - 1),
            (Some(i), false) if i + 1 < count => Some(i + 1),
            (Some(_), false) => None,
        };
        self.browsing = next;
        self.input = next.map(|i| self.shell.history[i].clone()).unwrap_or_default();
        self.cursor = self.input.len();
    }
}

impl App for Terminal {
    fn size(&mut self, width: i32, height: i32) {
        self.cols = ((width - 2 * MARGIN) / CELL_W).max(20) as usize;
        self.rows = ((height - 2 * MARGIN) / CELL_H).max(4) as usize;
    }

    fn paint(&self, ui: &mut Ui) {
        ui.rect(0, 0, ui.width, ui.height, BACKGROUND);
        // The last row is the input line; above, the end of the lines,
        // minus what was scrolled.
        let visible = self.rows.saturating_sub(1);
        let end = self.lines.len().saturating_sub(self.scroll.min(self.lines.len()));
        let start = end.saturating_sub(visible);
        for (row, (text, color)) in self.lines[start..end].iter().enumerate() {
            ui.text(MARGIN, MARGIN + row as i32 * CELL_H, text, *color);
        }
        let prompt = printable(&self.shell.prompt());
        let line_y = MARGIN + (end - start) as i32 * CELL_H;
        let room = self.cols.saturating_sub(prompt.len()).max(1);
        // The input scrolls sideways when it is longer than the window.
        let first = self.cursor.saturating_sub(room - 1).min(self.input.len());
        let shown = &self.input[first..self.input.len().min(first + room)];
        ui.text(MARGIN, line_y, &prompt, PROMPT);
        let x = MARGIN + prompt.len() as i32 * CELL_W;
        ui.text(x, line_y, &printable(shown), TEXT);
        let caret = x + (self.cursor - first) as i32 * CELL_W;
        ui.rect(caret, line_y + CELL_H - 3, CELL_W, 2, PROMPT);
        if self.scroll > 0 {
            let note = alloc::format!("-- {} ligne(s) plus bas --", self.scroll);
            ui.text(ui.width - MARGIN - note.len() as i32 * CELL_W, MARGIN, &note, DIM);
        }
    }

    fn key(&mut self, key: &KeyEvent) -> bool {
        if !key.pressed {
            return false;
        }
        let ctrl = key.has(KeyEvent::MOD_CTRL);
        if ctrl {
            match key.character().map(|c| c.to_ascii_lowercase()) {
                Some('l') => {
                    self.lines.clear();
                    self.scroll = 0;
                }
                Some('c') => {
                    let prompt = self.shell.prompt();
                    self.push(alloc::format!("{}{}^C", prompt, self.input), DIM);
                    self.input.clear();
                    self.cursor = 0;
                }
                Some('u') => {
                    self.input.clear();
                    self.cursor = 0;
                }
                _ => return false,
            }
            return true;
        }
        match key.scancode {
            SCAN_ENTER => self.submit(),
            SCAN_BACKSPACE => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.input.remove(self.cursor);
                }
            }
            SCAN_DELETE => {
                if self.cursor < self.input.len() {
                    self.input.remove(self.cursor);
                }
            }
            SCAN_LEFT => self.cursor = self.cursor.saturating_sub(1),
            SCAN_RIGHT => self.cursor = (self.cursor + 1).min(self.input.len()),
            SCAN_HOME => self.cursor = 0,
            SCAN_END => self.cursor = self.input.len(),
            SCAN_UP => self.history(true),
            SCAN_DOWN => self.history(false),
            SCAN_PAGE_UP => self.scroll = (self.scroll + self.rows / 2).min(self.lines.len()),
            SCAN_PAGE_DOWN => self.scroll = self.scroll.saturating_sub(self.rows / 2),
            _ => match key.character() {
                Some(c) if (' '..='~').contains(&c) && self.input.len() < 512 => {
                    self.input.insert(self.cursor, c);
                    self.cursor += 1;
                    self.scroll = 0;
                }
                _ => return false,
            },
        }
        true
    }

    fn pointer(&mut self, _x: i32, _y: i32, _press: Press) -> (bool, CursorShape) {
        (false, CursorShape::Text)
    }

    fn wheel(&mut self, _x: i32, _y: i32, notches: i32) -> bool {
        if notches > 0 {
            self.scroll = (self.scroll + 3 * notches as usize).min(self.lines.len());
        } else {
            self.scroll = self.scroll.saturating_sub(3 * notches.unsigned_abs() as usize);
        }
        true
    }

    fn tick(&mut self) -> bool {
        false
    }

    fn quit_requested(&self) -> bool {
        self.quit
    }

    fn title(&self) -> Option<String> {
        Some(alloc::format!("Terminal - {}", self.shell.cwd))
    }

    fn close(&mut self) {}
}

/// Drives a terminal with synthetic keys, without a window (16.0.2
/// self-test): typing, editing, history, wrapping, scrolling, Ctrl+C,
/// Ctrl+L, `quitter`. Returns how many checks passed, or the first that
/// did not.
pub fn exercise() -> Result<usize, String> {
    fn key(scancode: u8, c: Option<char>, modifiers: u8) -> KeyEvent {
        KeyEvent { scancode, pressed: true, modifiers, codepoint: c.map_or(0, |c| c as u32) }
    }
    fn typed(t: &mut Terminal, text: &str) {
        for c in text.chars() {
            t.key(&key(0, Some(c), 0));
        }
    }
    let mut passed = 0;
    let mut check = |ok: bool, what: &str| -> Result<(), String> {
        if ok {
            passed += 1;
            Ok(())
        } else {
            Err(String::from(what))
        }
    };
    let mut t = Terminal::new();
    t.size(760, 460);
    check(t.cols == 93 && t.rows == 28, "grille de la fenetre")?;

    typed(&mut t, "echo salut");
    t.key(&key(SCAN_ENTER, None, 0));
    let n = t.lines.len();
    check(n >= 2 && t.lines[n - 1].0 == "salut", "sortie d'une commande")?;
    check(t.lines[n - 2].0.ends_with("$ echo salut") && t.lines[n - 2].1 == PROMPT, "ligne tapee recopiee")?;
    check(t.input.is_empty() && t.cursor == 0, "ligne videe apres Entree")?;

    t.key(&key(SCAN_UP, None, 0));
    check(t.input == "echo salut" && t.cursor == 10, "historique : fleche haut")?;
    for _ in 0..5 {
        t.key(&key(SCAN_LEFT, None, 0));
    }
    t.key(&key(SCAN_BACKSPACE, None, 0));
    check(t.input == "echosalut" && t.cursor == 4, "edition au milieu de la ligne")?;
    t.key(&key(SCAN_HOME, None, 0));
    t.key(&key(SCAN_DELETE, None, 0));
    t.key(&key(SCAN_END, None, 0));
    check(t.input == "chosalut" && t.cursor == 8, "debut, suppression, fin")?;
    t.key(&key(SCAN_DOWN, None, 0));
    check(t.input.is_empty() && t.browsing.is_none(), "historique : retour a une ligne neuve")?;

    typed(&mut t, "abc");
    t.key(&key(0, Some('c'), KeyEvent::MOD_CTRL));
    check(t.input.is_empty() && t.lines.last().is_some_and(|l| l.0.ends_with("abc^C")), "Ctrl+C abandonne la ligne")?;

    let before = t.lines.len();
    typed(&mut t, "echo ");
    typed(&mut t, &"a".repeat(200));
    t.key(&key(SCAN_ENTER, None, 0));
    // The typed line (prompt + 205) and the output (200), cut at 93.
    let prompt = t.shell.prompt().len();
    let expected = (prompt + 205).div_ceil(93) + 200usize.div_ceil(93);
    check(t.lines.len() - before == expected, "lignes longues coupees a la largeur")?;
    check(t.lines.iter().all(|l| l.0.len() <= 93), "aucune ligne plus large que la fenetre")?;

    t.key(&key(SCAN_PAGE_UP, None, 0));
    check(t.scroll > 0 && t.scroll == (t.rows / 2).min(t.lines.len()), "page precedente")?;
    typed(&mut t, "x");
    check(t.scroll == 0, "taper ramene en bas")?;
    t.key(&key(0, Some('u'), KeyEvent::MOD_CTRL));
    check(t.input.is_empty(), "Ctrl+U vide la ligne")?;

    t.key(&key(0, Some('l'), KeyEvent::MOD_CTRL));
    check(t.lines.is_empty(), "Ctrl+L efface")?;
    typed(&mut t, "commande-inconnue");
    t.key(&key(SCAN_ENTER, None, 0));
    check(t.lines.last().is_some_and(|l| l.1 == ERROR), "une erreur s'affiche en couleur d'erreur")?;
    typed(&mut t, "effacer");
    t.key(&key(SCAN_ENTER, None, 0));
    check(t.lines.is_empty(), "effacer vide le terminal")?;
    check(!t.quit_requested(), "pas de fermeture avant quitter")?;
    typed(&mut t, "quitter");
    t.key(&key(SCAN_ENTER, None, 0));
    check(t.quit_requested(), "quitter ferme la fenetre")?;
    Ok(passed)
}

/// The `terminal.ss` task.
pub fn task() -> ! {
    let mut terminal = Terminal::new();
    window::run("terminal", "Terminal", 760, 460, &mut terminal)
}

pub fn install(now: u64) -> bool {
    super::install_binary(
        &super::BinarySpec {
            path: BINARY,
            title: FILE_NAME,
            app_id: APP_ID,
            display_name: "Terminal",
            format: crate::binfmt::header::BinaryFormat::Ss,
            permissions: Permissions::NONE.0,
        },
        now,
    )
}

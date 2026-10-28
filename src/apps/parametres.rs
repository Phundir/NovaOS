//! `parametres.ss` -- the settings application (spec 3.1, System Software).
//!
//! ```text
//!   +--------------------------------------------------------------------+
//!   | Parametres            Son - son.ss                                 |
//!   | +--------------+      Peripherique de sortie   [Intel AC'97     v] |
//!   | | Son          |      Volume                   [=======o---]  80 % |
//!   | | Affichage    |      Couper le son            [ ] Desactive       |
//!   | | Explorateur  |      Niveau de sortie         [|||||        ]     |
//!   | +--------------+      Essai                    [Tester le son]     |
//!   | status or help of the setting under the pointer                    |
//!   +--------------------------------------------------------------------+
//! ```
//!
//! ## It contains nothing
//!
//! Every tab is a `config::Provider` registered by the `.ss` it belongs
//! to. This file draws a control for each **kind** of setting and hands
//! the values back; it knows no key, no label, no tab. See `config` for
//! why only `.ss` may register.
//!
//! ## The controls
//!
//! | Kind | Control | Pointer | Keyboard |
//! |---|---|---|---|
//! | Toggle | check box | click | Space, Left/Right |
//! | Select | drop-down list | click, then a choice | Space opens, arrows, Enter |
//! | Field | text box | click, type, Enter | Enter edits, Enter commits, Esc |
//! | Slider | slider | drag | Left/Right by its step |
//! | Level | meter, live | -- | -- |
//! | Action | button | click | Space |
//! | Info | a line of text | -- | -- |
//!
//! Up and Down move between settings, Page Up and Page Down between tabs.
//! A slider applies while it moves (`Provider::preview`) and is written
//! once, when let go.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::identity::Permissions;
use crate::config::{self, Kind, Provider, Setting, Value};
use crate::gui::client::ellipsize;
use crate::gui::protocol::{CursorShape, KeyEvent};
use crate::gui::surface::Rect;
use crate::vga::SpinLock;

use super::widgets::{self, rect};
use super::window::{self, App, Press, Ui};

pub const APP_ID: &str = "os.nova.parametres";
pub const FILE_NAME: &str = "parametres.ss";
pub const BINARY: &str = "/sysapp/parametres.ss";

const WIDTH: i32 = 820;
const HEIGHT: i32 = 540;
const TABS_W: i32 = 190;
const TOP: i32 = 56;
const PITCH: i32 = 38;
const LABEL_W: i32 = 240;
const OPTION_H: i32 = 22;

/// Laps of the loop between two re-reads of the tab: a second.
const REFRESH_LAPS: u32 = (1000 / window::PERIOD_MS) as u32;
/// Laps a message stays before the help line comes back: four seconds.
const MESSAGE_LAPS: u32 = (4000 / window::PERIOD_MS) as u32;

const SCAN_ESCAPE: u8 = 0x01;
const SCAN_BACKSPACE: u8 = 0x0E;
const SCAN_TAB: u8 = 0x0F;
const SCAN_ENTER: u8 = 0x1C;
const SCAN_SPACE: u8 = 0x39;
const SCAN_UP: u8 = 0x48;
const SCAN_PAGE_UP: u8 = 0x49;
const SCAN_LEFT: u8 = 0x4B;
const SCAN_RIGHT: u8 = 0x4D;
const SCAN_DOWN: u8 = 0x50;
const SCAN_PAGE_DOWN: u8 = 0x51;

/// A tab to open on, left by `son.ss` and the like before starting.
static OPEN_TAB: SpinLock<Option<&'static str>> = SpinLock::new(None);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Tab(usize),
    /// The control of a setting, by index in the tab.
    Control(usize),
    /// A choice of the open drop-down list.
    Choice(usize),
}

struct Layout {
    width: i32,
    height: i32,
}

impl Layout {
    fn tab(&self, index: usize) -> Rect {
        rect(8, TOP - 8 + index as i32 * 30, TABS_W - 16, 26)
    }
    fn content_x(&self) -> i32 {
        TABS_W + 16
    }
    fn control_x(&self) -> i32 {
        self.content_x() + LABEL_W
    }
    fn control_w(&self) -> i32 {
        (self.width - self.control_x() - 90).max(120)
    }
    fn rows(&self) -> usize {
        ((self.height - TOP - 70) / PITCH).max(1) as usize
    }
    fn row_y(&self, rank: usize) -> i32 {
        TOP + rank as i32 * PITCH
    }
    fn control(&self, rank: usize, kind: &Kind) -> Rect {
        let y = self.row_y(rank);
        let x = self.control_x();
        let w = self.control_w();
        match kind {
            Kind::Toggle => rect(x, y + 5, 200, 22),
            Kind::Select(_) | Kind::Field { .. } => rect(x, y + 3, w, 26),
            Kind::Slider { .. } => rect(x, y + 7, w, 18),
            Kind::Level => rect(x, y + 11, w, 12),
            Kind::Action(_) => rect(x, y + 3, 190, 26),
            Kind::Info => rect(self.content_x(), y + 5, self.width - self.content_x() - 16, 22),
        }
    }
    fn choice(&self, rank: usize, index: usize) -> Rect {
        let y = self.row_y(rank) + 3 + 26 + index as i32 * OPTION_H;
        rect(self.control_x(), y, self.control_w(), OPTION_H)
    }
    fn status_y(&self) -> i32 {
        self.height - 46
    }
    fn help_y(&self) -> i32 {
        self.height - 24
    }
}

struct Parametres {
    user: String,
    layout: Layout,
    providers: Vec<&'static dyn Provider>,
    tab: usize,
    settings: Vec<Setting>,
    values: Vec<Value>,
    /// First setting shown, when the tab is longer than the window.
    top: usize,
    focus: usize,
    hover: Option<Hit>,
    /// Open drop-down list: setting index, highlighted choice.
    open: Option<(usize, usize)>,
    /// Field being edited: setting index, text so far.
    editing: Option<(usize, String)>,
    /// Slider being dragged, by setting index.
    dragging: Option<usize>,
    message: String,
    error: bool,
    message_laps: u32,
    laps: u32,
    /// Levels last drawn, to repaint only when one moves.
    levels: Vec<u16>,
}

impl Parametres {
    fn provider(&self) -> Option<&'static dyn Provider> {
        self.providers.get(self.tab).copied()
    }

    fn say(&mut self, text: String, error: bool) {
        if error {
            crate::serial_println!("[parametres] {}", text);
        }
        self.message = text;
        self.error = error;
        self.message_laps = MESSAGE_LAPS;
    }

    /// Re-reads the settings and the values of the tab.
    fn reload(&mut self) {
        let Some(provider) = self.provider() else {
            self.settings.clear();
            self.values.clear();
            return;
        };
        self.settings = provider.settings(&self.user);
        self.values = self
            .settings
            .iter()
            .map(|setting| provider.get(&self.user, setting.key))
            .collect();
        self.focus = self.focus.min(self.settings.len().saturating_sub(1));
    }

    fn switch_tab(&mut self, index: usize) {
        if index >= self.providers.len() || index == self.tab && !self.settings.is_empty() {
            return;
        }
        if let Some(provider) = self.provider() {
            provider.hidden();
        }
        self.tab = index;
        self.top = 0;
        self.focus = 0;
        self.open = None;
        self.editing = None;
        self.dragging = None;
        self.message.clear();
        self.reload();
        if let Some(provider) = self.provider()
            && let Some(warning) = provider.shown(&self.user)
        {
            self.say(warning, true);
        }
    }

    /// Hands a value to the provider.
    fn commit(&mut self, index: usize, value: Value) {
        let (Some(provider), Some(setting)) = (self.provider(), self.settings.get(index)) else {
            return;
        };
        let key = setting.key;
        match provider.set(&self.user, key, &value) {
            Ok(()) => {
                crate::serial_println!(
                    "[parametres] {} : {} = {:?}",
                    provider.file(),
                    key,
                    value
                );
                self.message.clear();
            }
            Err(reason) => self.say(format!("{} : {}", setting.label, reason), true),
        }
        // Read back rather than trusted: the provider may have clamped the
        // value, or refused it.
        self.reload();
    }

    fn act(&mut self, index: usize) {
        let (Some(provider), Some(setting)) = (self.provider(), self.settings.get(index)) else {
            return;
        };
        match provider.act(&self.user, setting.key) {
            Ok(text) => self.say(text, false),
            Err(reason) => self.say(reason, true),
        }
    }

    /// Space or Enter on a setting, or a click on its control.
    fn activate(&mut self, index: usize) {
        let Some(setting) = self.settings.get(index) else {
            return;
        };
        match &setting.kind {
            Kind::Toggle => {
                let on = !self.values[index].as_bool();
                self.commit(index, Value::Bool(on));
            }
            Kind::Select(choices) => {
                let current = self.values[index].as_text();
                let highlighted = choices.iter().position(|choice| choice.value == current).unwrap_or(0);
                self.open = Some((index, highlighted));
            }
            Kind::Field { .. } => {
                self.editing = Some((index, String::from(self.values[index].as_text())));
            }
            Kind::Action(_) => self.act(index),
            Kind::Slider { .. } | Kind::Level | Kind::Info => {}
        }
    }

    /// Left or Right on a setting.
    fn nudge(&mut self, index: usize, up: bool) {
        let Some(setting) = self.settings.get(index) else {
            return;
        };
        match &setting.kind {
            Kind::Slider { min, max, step, .. } => {
                let value = self.values[index].as_number() + if up { *step } else { -*step };
                self.commit(index, Value::Number(value.clamp(*min, *max)));
            }
            Kind::Toggle => self.commit(index, Value::Bool(up)),
            Kind::Select(choices) if !choices.is_empty() => {
                let current = self.values[index].as_text();
                let position = choices.iter().position(|choice| choice.value == current).unwrap_or(0);
                let next = if up {
                    (position + 1).min(choices.len() - 1)
                } else {
                    position.saturating_sub(1)
                };
                let value = Value::Text(choices[next].value.clone());
                self.commit(index, value);
            }
            _ => {}
        }
    }

    /// Settings the focus may stop on: not the read-only ones.
    fn focusable(&self, index: usize) -> bool {
        self.settings
            .get(index)
            .is_some_and(|setting| !matches!(setting.kind, Kind::Info | Kind::Level))
    }

    fn move_focus(&mut self, down: bool) {
        let count = self.settings.len();
        let mut index = self.focus;
        for _ in 0..count {
            index = if down { (index + 1) % count } else { (index + count - 1) % count };
            if self.focusable(index) {
                break;
            }
        }
        self.focus = index;
        let rows = self.layout.rows();
        if self.focus < self.top {
            self.top = self.focus;
        } else if self.focus >= self.top + rows {
            self.top = self.focus + 1 - rows;
        }
    }

    fn slider_value(&self, index: usize, x: i32) -> Option<Value> {
        let setting = self.settings.get(index)?;
        let Kind::Slider { min, max, step, .. } = setting.kind else {
            return None;
        };
        let rank = index.checked_sub(self.top)?;
        let bar = self.layout.control(rank, &setting.kind);
        let span = (max - min).max(1) as u32;
        let raw = min + widgets::slider_value(bar, x, span) as i64;
        // Snapped to the step: a volume of 73 % from a slider stepping by 5
        // would be a value the keyboard can never reach again.
        let snapped = ((raw - min + step / 2) / step.max(1)) * step.max(1) + min;
        Some(Value::Number(snapped.clamp(min, max)))
    }

    fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        if let Some((index, _)) = self.open
            && let Some(Kind::Select(choices)) = self.settings.get(index).map(|s| &s.kind)
        {
            let rank = index.saturating_sub(self.top);
            for choice in 0..choices.len() {
                if self.layout.choice(rank, choice).contains(x, y) {
                    return Some(Hit::Choice(choice));
                }
            }
        }
        for index in 0..self.providers.len() {
            if self.layout.tab(index).contains(x, y) {
                return Some(Hit::Tab(index));
            }
        }
        for rank in 0..self.layout.rows() {
            let index = self.top + rank;
            let Some(setting) = self.settings.get(index) else {
                break;
            };
            if self.focusable(index) && self.layout.control(rank, &setting.kind).contains(x, y) {
                return Some(Hit::Control(index));
            }
        }
        None
    }

    /// The status line: a recent message, else the help of the setting
    /// under the pointer or the focus.
    fn status(&self) -> (String, bool) {
        if !self.message.is_empty() {
            return (self.message.clone(), self.error);
        }
        let index = match self.hover {
            Some(Hit::Control(index)) => index,
            _ => self.focus,
        };
        let help = self
            .settings
            .get(index)
            .map(|setting| setting.help.clone())
            .unwrap_or_default();
        (help, false)
    }
}

// ============================================================
//  Drawing
// ============================================================

fn paint_control(ui: &mut Ui, app: &Parametres, index: usize, rank: usize) {
    let theme = ui.theme;
    let setting = &app.settings[index];
    let value = &app.values[index];
    let control = app.layout.control(rank, &setting.kind);
    let hovered = app.hover == Some(Hit::Control(index)) || app.focus == index;
    let cells = ((control.width - 30) / 8).max(1) as usize;

    match &setting.kind {
        Kind::Toggle => {
            let on = value.as_bool();
            widgets::checkbox(ui, control, if on { "Active" } else { "Desactive" }, on, hovered);
        }
        Kind::Select(choices) => {
            let current = value.as_text();
            let label = choices
                .iter()
                .find(|choice| choice.value == current)
                .map(|choice| choice.label.as_str())
                .unwrap_or(current);
            ui.rect(control.x, control.y, control.width, control.height, theme.background);
            ui.frame(
                control.x,
                control.y,
                control.width,
                control.height,
                if hovered { theme.accent } else { theme.border },
            );
            ui.text(control.x + 8, widgets::text_y(control), &ellipsize(label, cells), theme.text);
            ui.text(control.right() - 16, widgets::text_y(control), "v", theme.text_dim);
        }
        Kind::Field { .. } => {
            let editing = app.editing.as_ref().filter(|(edited, _)| *edited == index);
            ui.rect(control.x, control.y, control.width, control.height, theme.background);
            ui.frame(
                control.x,
                control.y,
                control.width,
                control.height,
                if editing.is_some() || hovered { theme.accent } else { theme.border },
            );
            let text = match editing {
                Some((_, text)) => format!("{}_", text),
                None => String::from(value.as_text()),
            };
            // The end of a long path is the part that matters.
            let shown: String = if text.len() > cells {
                text.chars().skip(text.len() - cells).collect()
            } else {
                text
            };
            ui.text(control.x + 8, widgets::text_y(control), &shown, theme.text);
        }
        Kind::Slider { min, max, unit, .. } => {
            let number = value.as_number();
            widgets::slider(
                ui,
                control,
                (number - min).max(0) as u32,
                (max - min).max(1) as u32,
                hovered || app.dragging == Some(index),
            );
            ui.text(
                control.right() + 14,
                widgets::text_y(control),
                &format!("{} {}", number, unit),
                theme.text,
            );
        }
        Kind::Level => {
            let level = app.provider().map(|provider| provider.level(setting.key)).unwrap_or(0);
            widgets::meter(ui, control, level);
        }
        Kind::Action(label) => widgets::button(ui, control, label, true, hovered, false),
        Kind::Info => {
            // A labelled line starts in the controls' column, beside its
            // label; drawn from the label's column it wrote over it. An
            // unlabelled line keeps the whole width.
            let x = if setting.label.is_empty() { control.x } else { app.layout.control_x() };
            let width = ((control.x + control.width - x) / 8).max(1) as usize;
            ui.text(x, widgets::text_y(control), &ellipsize(value.as_text(), width), theme.text_dim);
        }
    }
}

fn paint_dropdown(ui: &mut Ui, app: &Parametres) {
    let Some((index, highlighted)) = app.open else {
        return;
    };
    let Some(Kind::Select(choices)) = app.settings.get(index).map(|setting| &setting.kind) else {
        return;
    };
    let theme = ui.theme;
    let rank = index.saturating_sub(app.top);
    let first = app.layout.choice(rank, 0);
    let height = choices.len() as i32 * OPTION_H;
    ui.rect(first.x, first.y, first.width, height, theme.surface);
    ui.frame(first.x, first.y, first.width, height, theme.accent);
    let current = app.values[index].as_text();
    let cells = ((first.width - 16) / 8).max(1) as usize;
    for (position, choice) in choices.iter().enumerate() {
        let row = app.layout.choice(rank, position);
        let lit = position == highlighted || app.hover == Some(Hit::Choice(position));
        if lit {
            ui.rect(row.x + 1, row.y, row.width - 2, row.height, theme.surface_alt);
        }
        ui.text(
            row.x + 8,
            widgets::text_y(row),
            &ellipsize(&choice.label, cells),
            if choice.value == current { theme.accent } else { theme.text },
        );
    }
}

impl App for Parametres {
    fn size(&mut self, width: i32, height: i32) {
        self.layout = Layout { width, height };
    }

    fn paint(&self, ui: &mut Ui) {
        let theme = ui.theme;
        let layout = &self.layout;

        ui.text(16, 16, "Parametres", theme.accent);
        ui.rect(TABS_W, 0, 1, layout.height, theme.border);

        if self.providers.is_empty() {
            ui.text(layout.content_x(), TOP, "Aucun .ss n'expose de reglages.", theme.text_dim);
            return;
        }

        for (index, provider) in self.providers.iter().enumerate() {
            let row = layout.tab(index);
            let selected = index == self.tab;
            let hovered = self.hover == Some(Hit::Tab(index));
            if selected || hovered {
                ui.rect(row.x, row.y, row.width, row.height, if selected { theme.surface_alt } else { theme.surface });
            }
            if selected {
                ui.rect(row.x, row.y, 3, row.height, theme.accent);
            }
            ui.text(row.x + 12, widgets::text_y(row), provider.tab(), if selected { theme.text } else { theme.text_dim });
        }

        if let Some(provider) = self.provider() {
            ui.text(layout.content_x(), 16, provider.tab(), theme.text);
            ui.text(
                layout.content_x() + 8 * (provider.tab().len() as i32 + 2),
                16,
                &format!("- {}", provider.file()),
                theme.text_dim,
            );
        }

        for rank in 0..layout.rows() {
            let index = self.top + rank;
            let Some(setting) = self.settings.get(index) else {
                break;
            };
            let y = layout.row_y(rank);
            if index == self.focus && self.focusable(index) {
                ui.rect(layout.content_x() - 8, y + 4, 3, PITCH - 10, theme.accent);
            }
            if !setting.label.is_empty() {
                ui.text(
                    layout.content_x(),
                    y + (PITCH - 16) / 2 - 2,
                    &ellipsize(&setting.label, ((LABEL_W - 12) / 8) as usize),
                    theme.text,
                );
            }
            paint_control(ui, self, index, rank);
        }
        if self.settings.len() > self.top + layout.rows() {
            ui.text(layout.content_x(), layout.status_y() - 20, "... (fleche bas pour la suite)", theme.text_dim);
        }

        // Last, over everything else.
        paint_dropdown(ui, self);

        let (status, error) = self.status();
        if !status.is_empty() {
            ui.text(
                layout.content_x(),
                layout.status_y(),
                &ellipsize(&status, ((layout.width - layout.content_x() - 16) / 8) as usize),
                if error { theme.danger } else { theme.accent },
            );
        }
        ui.text(
            16,
            layout.help_y(),
            "Haut/Bas reglage  PgPrec/PgSuiv onglet  Gauche/Droite modifier  Espace activer",
            theme.text_dim,
        );
    }

    fn key(&mut self, key: &KeyEvent) -> bool {
        if !key.pressed {
            return false;
        }

        // --- a field being edited has the keyboard ---
        if let Some((index, mut text)) = self.editing.take() {
            match key.scancode {
                SCAN_ENTER => self.commit(index, Value::Text(text)),
                SCAN_ESCAPE => self.say(String::from("modification annulee"), false),
                SCAN_BACKSPACE => {
                    text.pop();
                    self.editing = Some((index, text));
                }
                _ => {
                    let max = match self.settings.get(index).map(|setting| &setting.kind) {
                        Some(Kind::Field { max }) => *max,
                        _ => 0,
                    };
                    if let Some(character) = key.character()
                        && (0x20..=0x7E).contains(&(character as u32))
                        && text.len() < max
                    {
                        text.push(character);
                    }
                    self.editing = Some((index, text));
                }
            }
            return true;
        }

        // --- an open list has the keyboard ---
        if let Some((index, highlighted)) = self.open {
            let count = match self.settings.get(index).map(|setting| &setting.kind) {
                Some(Kind::Select(choices)) => choices.len(),
                _ => 0,
            };
            match key.scancode {
                SCAN_UP => self.open = Some((index, highlighted.saturating_sub(1))),
                SCAN_DOWN => self.open = Some((index, (highlighted + 1).min(count.saturating_sub(1)))),
                SCAN_ENTER | SCAN_SPACE => {
                    self.open = None;
                    if let Some(Kind::Select(choices)) = self.settings.get(index).map(|setting| &setting.kind)
                        && let Some(choice) = choices.get(highlighted)
                    {
                        let value = Value::Text(choice.value.clone());
                        self.commit(index, value);
                    }
                }
                SCAN_ESCAPE => self.open = None,
                _ => return false,
            }
            return true;
        }

        match key.scancode {
            SCAN_UP => self.move_focus(false),
            SCAN_DOWN | SCAN_TAB => self.move_focus(true),
            SCAN_PAGE_UP => {
                let previous = self.tab.saturating_sub(1);
                self.switch_tab(previous);
            }
            SCAN_PAGE_DOWN => {
                let next = (self.tab + 1).min(self.providers.len().saturating_sub(1));
                self.switch_tab(next);
            }
            SCAN_LEFT => self.nudge(self.focus, false),
            SCAN_RIGHT => self.nudge(self.focus, true),
            SCAN_SPACE | SCAN_ENTER => self.activate(self.focus),
            _ => return false,
        }
        true
    }

    fn pointer(&mut self, x: i32, y: i32, press: Press) -> (bool, CursorShape) {
        let hit = self.hit(x, y);
        let mut dirty = hit != self.hover;
        self.hover = hit;

        if let Some(index) = self.dragging {
            if let Some(value) = self.slider_value(index, x) {
                if press.held {
                    if let (Some(provider), Some(setting)) = (self.provider(), self.settings.get(index)) {
                        provider.preview(&self.user, setting.key, &value);
                    }
                    self.values[index] = value;
                    return (true, CursorShape::Hand);
                }
                // Let go: written once, now.
                self.dragging = None;
                self.commit(index, value);
                return (true, CursorShape::Hand);
            }
            self.dragging = None;
        }

        if !press.pressed {
            return (dirty, if hit.is_some() { CursorShape::Hand } else { CursorShape::Arrow });
        }
        dirty = true;

        // A click anywhere closes an open list -- on a choice, after
        // taking it -- and ends the editing of a field without keeping it.
        let open = self.open.take();
        if let Some((_, _)) = self.editing.take() {
            self.say(String::from("modification annulee"), false);
        }

        match hit {
            Some(Hit::Choice(position)) => {
                if let Some((index, _)) = open
                    && let Some(Kind::Select(choices)) = self.settings.get(index).map(|setting| &setting.kind)
                    && let Some(choice) = choices.get(position)
                {
                    let value = Value::Text(choice.value.clone());
                    self.commit(index, value);
                }
            }
            Some(Hit::Tab(index)) => self.switch_tab(index),
            Some(Hit::Control(index)) => {
                self.focus = index;
                match self.settings.get(index).map(|setting| &setting.kind) {
                    Some(Kind::Slider { .. }) => {
                        self.dragging = Some(index);
                        if let Some(value) = self.slider_value(index, x) {
                            if let (Some(provider), Some(setting)) = (self.provider(), self.settings.get(index)) {
                                provider.preview(&self.user, setting.key, &value);
                            }
                            self.values[index] = value;
                        }
                    }
                    // Clicking the list that is open closes it.
                    Some(Kind::Select(_)) if open.map(|(open_index, _)| open_index) == Some(index) => {}
                    _ => self.activate(index),
                }
            }
            None => {}
        }
        (dirty, if hit.is_some() { CursorShape::Hand } else { CursorShape::Arrow })
    }

    fn tick(&mut self) -> bool {
        let mut dirty = false;
        let Some(provider) = self.provider() else {
            return false;
        };
        provider.tick();

        if self.message_laps > 0 {
            self.message_laps -= 1;
            if self.message_laps == 0 {
                self.message.clear();
                dirty = true;
            }
        }

        // The tab is read again about once a second -- a device that
        // appeared, a value another window changed -- but never under the
        // hand: not while a list is open, a field edited, a slider held.
        self.laps += 1;
        if self.laps >= REFRESH_LAPS && self.open.is_none() && self.editing.is_none() && self.dragging.is_none() {
            self.laps = 0;
            let before = self.values.clone();
            self.reload();
            dirty |= self.values != before;
        }

        // Compared in decibels, the unit the meter draws in. This was
        // `level / 1024`, linear: everything below -30 dB -- most of
        // speech -- read 0, so the bar only moved on loud peaks and
        // otherwise stayed wherever it was last drawn.
        let levels: Vec<u16> = self
            .settings
            .iter()
            .filter(|setting| setting.kind == Kind::Level)
            .map(|setting| (widgets::level_to_db(provider.level(setting.key)).max(-61) + 61) as u16)
            .collect();
        if levels != self.levels {
            self.levels = levels;
            dirty = true;
        }
        dirty
    }

    fn close(&mut self) {
        if let Some(provider) = self.provider() {
            provider.hidden();
        }
    }
}

/// Runs the settings, on the tab of `file` if given.
pub fn run_on(file: Option<&'static str>) -> ! {
    let Some((user, _)) = super::session_credentials() else {
        crate::serial_println!("[parametres] aucune session ouverte - non demarre");
        crate::scheduler::exit_current_with(-1);
    };
    let providers = config::providers();
    let wanted = file.or_else(|| OPEN_TAB.lock().take());
    let tab = wanted
        .and_then(|file| providers.iter().position(|provider| provider.file() == file))
        .unwrap_or(0);

    let mut app = Parametres {
        user,
        layout: Layout {
            width: WIDTH,
            height: HEIGHT,
        },
        providers,
        tab,
        settings: Vec::new(),
        values: Vec::new(),
        top: 0,
        focus: 0,
        hover: None,
        open: None,
        editing: None,
        dragging: None,
        message: String::new(),
        error: false,
        message_laps: 0,
        laps: 0,
        levels: Vec::new(),
    };
    app.reload();
    if !app.focusable(0) {
        app.move_focus(true);
    }
    if let Some(provider) = app.provider()
        && let Some(warning) = provider.shown(&app.user)
    {
        app.say(warning, true);
    }
    crate::serial_println!(
        "[parametres] {} onglet(s) : {}",
        app.providers.len(),
        app.providers
            .iter()
            .map(|provider| provider.file())
            .collect::<Vec<_>>()
            .join(", ")
    );
    window::run("parametres", "Parametres", WIDTH, HEIGHT, &mut app)
}

pub fn task() -> ! {
    run_on(None)
}

pub fn install(now: u64) -> bool {
    super::install_binary(
        &super::BinarySpec {
            path: BINARY,
            title: FILE_NAME,
            app_id: APP_ID,
            display_name: "Parametres",
            format: crate::binfmt::header::BinaryFormat::Ss,
            // Nothing of its own: every change is made by the provider of
            // the `.ss` it belongs to, under that component's rules.
            permissions: Permissions::FS_USER_HOME,
        },
        now,
    )
}

//! Geometry of the explorer window.
//!
//! One arithmetic, read by the drawing **and** by the hit test -- entry 38
//! of DECISIONS.md, the "14 by 20" defect: two places computing the same
//! rectangle drift apart, and the symptom is a click landing one row away
//! from where it was aimed.
//!
//! ```text
//!   +------------------------------------------------------------+
//!   | < > ^  [ /users/alice/home/documents                     ] |  nav
//!   | +Dossier +Fichier Renommer Copier Couper Coller Suppr Infos |  actions
//!   +-------------+----------------------------------------------+
//!   | Emplacements| [ ] Nom                   Type        Taille  | |  header
//!   |  Accueil    | [x] # images              dossier            | |
//!   |  Documents  | [ ] = notes.txt           texte      1.2 Kio | |  list
//!   |  ...        |                                              | |
//!   +-------------+----------------------------------------------+
//!   | 5 element(s), 1 marque(s)                        message   |  status
//!   +------------------------------------------------------------+
//! ```
//!
//! ## The drawing budget
//!
//! A surface holds at most `surface::MAX_COMMANDS` commands per repaint,
//! and what goes past is dropped by the compositor. The list is where the
//! count grows, so its row count is capped (`MAX_ROWS`) and the whole
//! budget is checked by the self-test: `FIXED_COMMANDS` for everything
//! but the rows, `ROW_COMMANDS` for each row.

use crate::gui::surface::Rect;
use crate::gui::theme::Theme;

use super::ops::SortKey;

/// Width of the places column.
pub const SIDEBAR_W: i32 = 150;
/// Width of the scroll bar.
pub const SCROLL_W: i32 = 10;
/// Columns of the type and size fields, in characters.
pub const TYPE_CHARS: i32 = 20;
pub const SIZE_CHARS: i32 = 9;

/// Rows drawn at most, whatever the window height.
pub const MAX_ROWS: usize = 22;
/// Places listed at most in the side column.
pub const MAX_PLACES: usize = 14;

/// Commands one list row may emit: highlight, mark box, mark, icon, name,
/// type, size.
pub const ROW_COMMANDS: usize = 7;
/// Commands of everything else, counted generously in `paint.rs` order:
/// background 1, nav bar 14, action bar 26, side column 3 + 2 per place,
/// header 6, scroll bar 2, status 10, empty-directory notice 1.
pub const FIXED_COMMANDS: usize = 1 + 14 + 26 + 3 + 2 * MAX_PLACES + 6 + 2 + 10 + 1;

/// Labels of the action bar, in order. `Action::ALL` follows the same order.
pub const ACTION_LABELS: [&str; 8] = [
    "+ Dossier",
    "+ Fichier",
    "Renommer",
    "Copier",
    "Couper",
    "Coller",
    "Supprimer",
    "Infos",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    NewFolder,
    NewFile,
    Rename,
    Copy,
    Cut,
    Paste,
    Delete,
    Info,
}

impl Action {
    pub const ALL: [Action; 8] = [
        Action::NewFolder,
        Action::NewFile,
        Action::Rename,
        Action::Copy,
        Action::Cut,
        Action::Paste,
        Action::Delete,
        Action::Info,
    ];
}

/// What a point of the window designates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Nothing,
    Back,
    Forward,
    Parent,
    PathBox,
    Action(Action),
    Place(usize),
    Header(SortKey),
    /// A list row, by **absolute** index in the listing.
    Row(usize),
    /// The mark box of a row, by absolute index.
    Mark(usize),
    /// The scroll bar track, above or below the thumb.
    PageUp,
    PageDown,
    Yes,
    No,
}

pub struct Layout {
    pub glyph_w: i32,
    pub glyph_h: i32,
    pub padding: i32,
    pub row_h: i32,
    pub width: i32,
    pub nav: Rect,
    pub actions: Rect,
    pub sidebar: Rect,
    pub header: Rect,
    pub list: Rect,
    pub scrollbar: Rect,
    pub status: Rect,
}

impl Layout {
    pub fn new(width: i32, height: i32, theme: &Theme) -> Self {
        let glyph_w = crate::graphics::font::GLYPH_WIDTH as i32;
        let glyph_h = crate::graphics::font::GLYPH_HEIGHT as i32;
        let padding = theme.padding as i32;
        let bar = glyph_h + 2 * padding;
        let row_h = glyph_h + 4;

        let body_y = 2 * bar;
        let status = Rect::new(0, height - bar, width, bar);
        let header = Rect::new(SIDEBAR_W, body_y, width - SIDEBAR_W, row_h + 4);
        let list_h = status.y - header.bottom();
        Layout {
            glyph_w,
            glyph_h,
            padding,
            row_h,
            width,
            nav: Rect::new(0, 0, width, bar),
            actions: Rect::new(0, bar, width, bar),
            sidebar: Rect::new(0, body_y, SIDEBAR_W, status.y - body_y),
            header,
            list: Rect::new(SIDEBAR_W, header.bottom(), width - SIDEBAR_W - SCROLL_W, list_h),
            scrollbar: Rect::new(width - SCROLL_W, header.bottom(), SCROLL_W, list_h),
            status,
        }
    }

    /// Rows that fit in the list, capped by the drawing budget.
    pub fn visible_rows(&self) -> usize {
        ((self.list.height / self.row_h).max(1) as usize).min(MAX_ROWS)
    }

    /// Vertical centring of one line of text in a bar of height `h`.
    pub fn text_y(&self, top: i32, h: i32) -> i32 {
        top + (h - self.glyph_h) / 2
    }

    // --- navigation bar ---

    fn nav_button(&self, rank: i32) -> Rect {
        let size = self.nav.height - 6;
        Rect::new(self.padding / 2 + rank * (size + 2), 3, size, size)
    }

    pub fn back(&self) -> Rect {
        self.nav_button(0)
    }

    pub fn forward(&self) -> Rect {
        self.nav_button(1)
    }

    pub fn up(&self) -> Rect {
        self.nav_button(2)
    }

    pub fn path_box(&self) -> Rect {
        let x = self.up().right() + self.padding;
        Rect::new(x, 3, self.width - x - self.padding / 2, self.nav.height - 6)
    }

    /// Characters of the path that fit in its box.
    pub fn path_chars(&self) -> usize {
        ((self.path_box().width - 2 * self.padding) / self.glyph_w).max(1) as usize
    }

    // --- action bar ---

    pub fn action(&self, index: usize) -> Rect {
        let mut x = self.padding / 2;
        for label in ACTION_LABELS.iter().take(index) {
            x += label.len() as i32 * self.glyph_w + 2 * self.padding + self.padding / 2;
        }
        let width = ACTION_LABELS[index].len() as i32 * self.glyph_w + 2 * self.padding;
        Rect::new(x, self.actions.y + 3, width, self.actions.height - 6)
    }

    // --- side column ---

    /// Row of a place; row 0 of the column is its title.
    pub fn place(&self, index: usize) -> Rect {
        Rect::new(
            self.sidebar.x,
            self.sidebar.y + self.padding / 2 + (index as i32 + 1) * self.row_h,
            self.sidebar.width - 1,
            self.row_h,
        )
    }

    // --- list ---

    pub fn name_x(&self) -> i32 {
        self.list.x + self.padding + 16 + self.padding + 16 + self.padding
    }

    pub fn type_x(&self) -> i32 {
        self.list.right() - self.padding - (SIZE_CHARS + TYPE_CHARS + 1) * self.glyph_w
    }

    /// Right edge of the size column, which is right-aligned.
    pub fn size_right(&self) -> i32 {
        self.list.right() - self.padding
    }

    pub fn name_chars(&self) -> usize {
        ((self.type_x() - self.name_x() - self.padding) / self.glyph_w).max(1) as usize
    }

    /// Header cell of a sort key.
    pub fn header_cell(&self, key: SortKey) -> Rect {
        let (x, right) = match key {
            SortKey::Name => (self.list.x, self.type_x()),
            SortKey::Kind => (self.type_x(), self.size_right() - SIZE_CHARS * self.glyph_w),
            SortKey::Size => (self.size_right() - SIZE_CHARS * self.glyph_w, self.header.right()),
        };
        Rect::new(x, self.header.y, right - x, self.header.height)
    }

    /// Visible row `rank`, counted from the top of the view.
    pub fn row(&self, rank: usize) -> Rect {
        Rect::new(
            self.list.x,
            self.list.y + rank as i32 * self.row_h,
            self.list.width,
            self.row_h,
        )
    }

    pub fn mark_box(&self, rank: usize) -> Rect {
        let row = self.row(rank);
        Rect::new(row.x + self.padding, row.y + (self.row_h - 14) / 2, 14, 14)
    }

    pub fn icon_x(&self) -> i32 {
        self.list.x + self.padding + 16 + self.padding
    }

    /// Thumb of the scroll bar, when the listing is longer than the view.
    pub fn thumb(&self, count: usize, top: usize) -> Option<Rect> {
        let rows = self.visible_rows();
        if count <= rows {
            return None;
        }
        let track = self.scrollbar;
        let height = (track.height * rows as i32 / count as i32).max(12);
        let travel = track.height - height;
        let y = track.y + travel * top as i32 / (count - rows) as i32;
        Some(Rect::new(track.x + 1, y, track.width - 2, height))
    }

    // --- status bar ---

    /// The two answers of a confirmation, at the right of the status bar.
    pub fn answer(&self, yes: bool) -> Rect {
        let width = 5 * self.glyph_w + 2 * self.padding;
        let right = self.status.right() - self.padding / 2 - if yes { width + self.padding } else { 0 };
        Rect::new(right - width, self.status.y + 3, width, self.status.height - 6)
    }
}

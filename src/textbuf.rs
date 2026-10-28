//! Editable text buffer, shared by the applications that edit text.
//!
//! ## Why a module of its own
//!
//! The IDE of phase 9, `devstudio.ss` (since retired), carried a buffer of
//! its own. A second editor copying it would have been two implementations
//! of the same rules, and the day a movement bug was fixed in one it would
//! have stayed in the other. So the logic lives here, once, **with tests**
//! -- which was the difference that mattered: the IDE's buffer had none,
//! and that is why it was never migrated onto this one. Replacing tested
//! code by untested code would have been a downgrade, and replacing
//! untested code by this one without first writing the tests that prove
//! the two agree would have been a
//! change nobody could check. That migration is a step of its own.
//!
//! ## What it holds and what it refuses
//!
//! Lines of printable ASCII, a caret, an optional path. No selection, no
//! copy-paste, no undo. Those are named here so that nobody looks for
//! them: announcing them without writing them would be worse than not
//! having them.
//!
//! ## Why the caret is a row and a column and not an offset
//!
//! An offset into a flat string is cheaper to move and far worse to
//! display: every repaint would have to count the newlines before it to
//! know which line to highlight. A row and a column are what the drawing
//! needs, and the drawing happens sixty times more often than an edit.
//!
//! The column is allowed to sit **past** the end of a line while moving
//! vertically -- that is what makes a caret at column 40 come back to
//! column 40 after crossing a short line, instead of collapsing to its
//! end. Every operation that touches the text clamps it first.

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::perm::{Credentials, Mode};

/// Cap on lines. A buffer is in the heap, and a file read from a volume
/// has no reason to be small.
pub const MAX_LINES: usize = 4096;

/// Cap on the length of a line, in characters.
pub const MAX_LINE: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    /// Nothing changed: the caret was already where it was asked to go, or
    /// the text refused the character.
    None,
    /// The caret moved, the text did not change.
    Moved,
    /// The text changed.
    Changed,
}

impl Edit {
    pub fn touched_text(&self) -> bool {
        matches!(self, Edit::Changed)
    }

    pub fn needs_repaint(&self) -> bool {
        !matches!(self, Edit::None)
    }
}

pub struct TextBuf {
    lines: Vec<String>,
    row: usize,
    column: usize,
    path: Option<String>,
    dirty: bool,
}

impl Default for TextBuf {
    fn default() -> Self {
        Self::new()
    }
}

impl TextBuf {
    pub fn new() -> Self {
        // One empty line and never zero: an empty buffer with no line at
        // all would make `lines[row]` panic on the first keystroke, and
        // every operation would need a guard for a state that has no
        // reason to exist.
        TextBuf {
            lines: alloc::vec![String::new()],
            row: 0,
            column: 0,
            path: None,
            dirty: false,
        }
    }

    // --- reading ---

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, row: usize) -> &str {
        self.lines.get(row).map(String::as_str).unwrap_or("")
    }

    pub fn row(&self) -> usize {
        self.row
    }

    /// Column of the caret, clamped to the line it is on.
    ///
    /// The stored column may be further right; this is the one to draw.
    pub fn column(&self) -> usize {
        self.column.min(self.lines[self.row].len())
    }

    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Name to show in a title bar.
    pub fn display_name(&self) -> &str {
        match self.path.as_deref() {
            Some(path) => path.rsplit('/').next().unwrap_or(path),
            None => "sans nom",
        }
    }

    pub fn set_path(&mut self, path: &str) {
        self.path = Some(String::from(path));
    }

    /// Total number of characters, newlines excluded.
    pub fn char_count(&self) -> usize {
        self.lines.iter().map(String::len).sum()
    }

    // --- files ---

    /// Replaces the contents with those of a file.
    ///
    /// Refuses rather than half-loads: a buffer holding the first half of a
    /// file and claiming its path would overwrite the second half on the
    /// next save.
    pub fn load(&mut self, path: &str, who: Credentials) -> bool {
        let Ok(bytes) = crate::fs::read_file(path, who) else {
            return false;
        };
        let Ok(text) = core::str::from_utf8(&bytes) else {
            return false;
        };
        if text.lines().count() > MAX_LINES {
            return false;
        }
        self.lines = text.lines().map(String::from).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.row = 0;
        self.column = 0;
        self.path = Some(String::from(path));
        self.dirty = false;
        true
    }

    /// Writes to the recorded path. `false` if there is none.
    pub fn save(&mut self, who: Credentials, now: u64) -> bool {
        let Some(path) = self.path.clone() else {
            return false;
        };
        self.save_as(&path, who, now)
    }

    /// Writes to a path, which becomes the buffer's own.
    ///
    /// The file is deleted then rewritten rather than truncated: a shorter
    /// text written over a longer one would otherwise leave the tail of the
    /// old contents behind, and the file would end with text nobody typed.
    pub fn save_as(&mut self, path: &str, who: Credentials, now: u64) -> bool {
        let mut text = self.lines.join("\n");
        // A trailing newline, because `str::lines` does not produce an
        // empty last line for one and a reload must give back what was
        // saved.
        text.push('\n');

        let _ = crate::fs::delete(path, who, now);
        if crate::fs::write_file(path, text.as_bytes(), who, Mode::private_file(), now).is_err() {
            return false;
        }
        self.path = Some(String::from(path));
        self.dirty = false;
        true
    }

    // --- editing ---

    /// Inserts a printable ASCII character at the caret.
    ///
    /// Anything outside 0x20..=0x7E is refused, and that is not a
    /// simplification: the VGA writer paints every other byte as a solid
    /// square, so a buffer that accepted them would save a file whose
    /// display is wrong everywhere in the system.
    pub fn insert(&mut self, character: char) -> Edit {
        if !(0x20..=0x7E).contains(&(character as u32)) {
            return Edit::None;
        }
        let column = self.column();
        let line = &mut self.lines[self.row];
        if line.len() >= MAX_LINE {
            return Edit::None;
        }
        line.insert(column, character);
        self.column = column + 1;
        self.dirty = true;
        Edit::Changed
    }

    /// Splits the line at the caret.
    pub fn newline(&mut self) -> Edit {
        if self.lines.len() >= MAX_LINES {
            return Edit::None;
        }
        let column = self.column();
        let tail = self.lines[self.row].split_off(column);
        self.lines.insert(self.row + 1, tail);
        self.row += 1;
        self.column = 0;
        self.dirty = true;
        Edit::Changed
    }

    /// Deletes the character before the caret, joining lines at column 0.
    pub fn backspace(&mut self) -> Edit {
        let column = self.column();
        if column > 0 {
            self.lines[self.row].remove(column - 1);
            self.column = column - 1;
            self.dirty = true;
            return Edit::Changed;
        }
        if self.row == 0 {
            return Edit::None;
        }
        // Joining: the caret lands where the two lines meet, which is the
        // end of the previous one *before* the join.
        let current = self.lines.remove(self.row);
        self.row -= 1;
        let join = self.lines[self.row].len();
        if join + current.len() <= MAX_LINE {
            self.lines[self.row].push_str(&current);
        } else {
            // The join would overflow a line. Put it back rather than
            // truncate: losing characters to a limit is worse than
            // refusing an operation.
            self.lines.insert(self.row + 1, current);
            self.row += 1;
            return Edit::None;
        }
        self.column = join;
        self.dirty = true;
        Edit::Changed
    }

    /// Deletes the character at the caret, joining lines at end of line.
    pub fn delete(&mut self) -> Edit {
        let column = self.column();
        if column < self.lines[self.row].len() {
            self.lines[self.row].remove(column);
            self.dirty = true;
            return Edit::Changed;
        }
        if self.row + 1 >= self.lines.len() {
            return Edit::None;
        }
        let next = self.lines.remove(self.row + 1);
        if column + next.len() > MAX_LINE {
            self.lines.insert(self.row + 1, next);
            return Edit::None;
        }
        self.lines[self.row].push_str(&next);
        self.column = column;
        self.dirty = true;
        Edit::Changed
    }

    /// Empties the buffer and forgets its path.
    pub fn clear(&mut self) {
        self.lines = alloc::vec![String::new()];
        self.row = 0;
        self.column = 0;
        self.path = None;
        self.dirty = false;
    }

    // --- movements ---

    pub fn left(&mut self) -> Edit {
        let column = self.column();
        if column > 0 {
            self.column = column - 1;
            return Edit::Moved;
        }
        if self.row == 0 {
            return Edit::None;
        }
        self.row -= 1;
        self.column = self.lines[self.row].len();
        Edit::Moved
    }

    pub fn right(&mut self) -> Edit {
        let column = self.column();
        if column < self.lines[self.row].len() {
            self.column = column + 1;
            return Edit::Moved;
        }
        if self.row + 1 >= self.lines.len() {
            return Edit::None;
        }
        self.row += 1;
        self.column = 0;
        Edit::Moved
    }

    /// Up one line, **keeping the stored column**.
    ///
    /// The column is not clamped here on purpose: crossing a short line
    /// must not collapse the caret to its end for good. `column()` clamps
    /// for the drawing, and every edit clamps before touching the text.
    pub fn up(&mut self) -> Edit {
        if self.row == 0 {
            // Already on the first line: go to its start rather than do
            // nothing, which is what every editor does and what a hand
            // pressing Up twice expects.
            if self.column() == 0 {
                return Edit::None;
            }
            self.column = 0;
            return Edit::Moved;
        }
        self.row -= 1;
        Edit::Moved
    }

    pub fn down(&mut self) -> Edit {
        if self.row + 1 >= self.lines.len() {
            let end = self.lines[self.row].len();
            if self.column() == end {
                return Edit::None;
            }
            self.column = end;
            return Edit::Moved;
        }
        self.row += 1;
        Edit::Moved
    }

    pub fn home(&mut self) -> Edit {
        if self.column() == 0 {
            return Edit::None;
        }
        self.column = 0;
        Edit::Moved
    }

    pub fn end(&mut self) -> Edit {
        let end = self.lines[self.row].len();
        if self.column() == end {
            return Edit::None;
        }
        self.column = end;
        Edit::Moved
    }

    /// Places the caret at a row and a column, clamping both.
    ///
    /// This is what a click uses. The column is clamped to the length of
    /// the line **immediately**, unlike a vertical movement: a hand that
    /// pointed past the end of a line asked for its end, whereas a hand
    /// that pressed Down asked to keep its column.
    pub fn place(&mut self, row: usize, column: usize) -> Edit {
        let row = row.min(self.lines.len() - 1);
        let column = column.min(self.lines[row].len());
        if row == self.row && column == self.column() {
            return Edit::None;
        }
        self.row = row;
        self.column = column;
        Edit::Moved
    }
}

// ============================================================
//  Self-test
// ============================================================

/// Checks the rules the buffer is supposed to hold.
///
/// Returns `(passed, failed)`. This exists because the editor it replaces
/// had none: every movement rule below is one that a hand notices
/// immediately and that nothing in the system would otherwise catch.
pub fn selftest() -> (usize, usize) {
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut check = |label: &str, condition: bool| {
        if condition {
            passed += 1;
        } else {
            failed += 1;
            crate::serial_println!("[textbuf] ECHEC : {}", label);
        }
    };

    // --- a fresh buffer is usable ---
    let mut buffer = TextBuf::new();
    check(
        "un tampon neuf a une ligne vide et pas zero",
        buffer.line_count() == 1 && buffer.line(0).is_empty() && !buffer.is_dirty(),
    );

    // --- insertion ---
    for character in "Bonjour".chars() {
        buffer.insert(character);
    }
    check(
        "l'insertion ecrit et salit",
        buffer.line(0) == "Bonjour" && buffer.column() == 7 && buffer.is_dirty(),
    );
    check(
        "un caractere hors ASCII imprimable est refuse",
        buffer.insert('\t') == Edit::None
            && buffer.insert('\n') == Edit::None
            && buffer.line(0) == "Bonjour",
    );

    // --- insertion in the middle, which is what a click enables ---
    buffer.place(0, 3);
    buffer.insert('-');
    check(
        "l'insertion se fait au caret, pas en fin de ligne",
        buffer.line(0) == "Bon-jour" && buffer.column() == 4,
    );

    // --- newline splits ---
    buffer.place(0, 4);
    buffer.newline();
    check(
        "Entree coupe la ligne au caret",
        buffer.line_count() == 2
            && buffer.line(0) == "Bon-"
            && buffer.line(1) == "jour"
            && buffer.row() == 1
            && buffer.column() == 0,
    );

    // --- backspace at column zero joins ---
    buffer.backspace();
    check(
        "retour arriere en colonne 0 recolle les deux lignes",
        buffer.line_count() == 1 && buffer.line(0) == "Bon-jour" && buffer.column() == 4,
    );
    check(
        "retour arriere sur un tampon au debut ne fait rien",
        {
            buffer.place(0, 0);
            buffer.backspace() == Edit::None
        },
    );

    // --- delete ---
    buffer.place(0, 3);
    buffer.delete();
    check(
        "suppression enleve le caractere sous le caret",
        buffer.line(0) == "Bonjour" && buffer.column() == 3,
    );

    // --- the column survives a short line ---
    let mut wide = TextBuf::new();
    for character in "0123456789".chars() {
        wide.insert(character);
    }
    wide.newline();
    wide.insert('x');
    wide.newline();
    for character in "abcdefghij".chars() {
        wide.insert(character);
    }
    // caret at row 2, column 10; the middle line has one character.
    wide.place(2, 10);
    wide.up();
    let on_short = wide.column();
    wide.up();
    check(
        "la colonne memorisee survit a une ligne courte",
        on_short == 1 && wide.row() == 0 && wide.column() == 10,
    );

    // --- place clamps, unlike a vertical movement ---
    //
    // The caret is put at the start first, on purpose: after inserting
    // "abc" it already sits at column 3, and `place` there would rightly
    // answer `None`. The first version of this test asserted `Moved` and
    // failed -- the expectation was wrong, not the buffer.
    let mut clamp = TextBuf::new();
    for character in "abc".chars() {
        clamp.insert(character);
    }
    clamp.place(0, 0);
    check(
        "un clic au-dela de la fin de ligne se cale sur la fin",
        clamp.place(0, 99) == Edit::Moved && clamp.column() == 3,
    );
    check(
        "replacer le caret ou il est deja ne change rien",
        clamp.place(0, 3) == Edit::None && clamp.place(0, 99) == Edit::None,
    );
    clamp.place(0, 0);
    check(
        "un clic sous la derniere ligne se cale sur elle",
        clamp.place(99, 2) == Edit::Moved && clamp.row() == 0 && clamp.column() == 2,
    );

    // --- limits ---
    let mut full = TextBuf::new();
    for _ in 0..MAX_LINE {
        full.insert('a');
    }
    check(
        "une ligne pleine refuse un caractere de plus",
        full.line(0).len() == MAX_LINE && full.insert('b') == Edit::None,
    );
    check(
        "une ligne pleine peut encore etre coupee",
        full.newline() == Edit::Changed && full.line_count() == 2,
    );
    // Joining two lines that would overflow must refuse and put back.
    let mut join = TextBuf::new();
    for _ in 0..MAX_LINE {
        join.insert('a');
    }
    join.newline();
    for _ in 0..10 {
        join.insert('b');
    }
    join.place(1, 0);
    check(
        "un recollage qui deborderait est refuse, sans rien perdre",
        join.backspace() == Edit::None
            && join.line_count() == 2
            && join.line(0).len() == MAX_LINE
            && join.line(1).len() == 10,
    );

    // --- movement edges ---
    let mut edge = TextBuf::new();
    check(
        "a gauche du debut, rien",
        edge.left() == Edit::None && edge.up() == Edit::None,
    );
    check(
        "a droite de la fin, rien",
        edge.right() == Edit::None && edge.down() == Edit::None,
    );

    // --- Edit says what happened ---
    let mut kinds = TextBuf::new();
    check(
        "Edit distingue le texte du deplacement",
        kinds.insert('a').touched_text()
            && !kinds.home().touched_text()
            && kinds.home() == Edit::None
            && !Edit::None.needs_repaint()
            && Edit::Moved.needs_repaint(),
    );

    // --- name shown ---
    let mut named = TextBuf::new();
    check("sans chemin, le nom est \"sans nom\"", named.display_name() == "sans nom");
    named.set_path("/users/admin/home/documents/notes.txt");
    check(
        "avec un chemin, le nom est le dernier segment",
        named.display_name() == "notes.txt",
    );

    (passed, failed)
}

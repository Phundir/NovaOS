//! 10.1.5 -- Full keyboard navigation (spec 16.2, `keyboard-nav.mod`).
//!
//! ## What "complete, without a mouse" has to mean
//!
//! 16.2 asks for "complete keyboard navigation without a mouse", and 16.4
//! gives the keys. The demanding word is *complete*: it is not enough for
//! most things to be reachable. If one element of a dialog cannot take
//! the focus, a keyboard-only user cannot answer that dialog at all, and
//! no other key gets them out of it.
//!
//! Three properties follow, and each is checked by the self-test rather
//! than assumed:
//!
//! 1. **Tab wraps.** Reaching the last element and pressing Tab goes back
//!    to the first. A dead end is a trap.
//! 2. **Shift+Tab is the exact inverse.** Tab then Shift+Tab returns to
//!    where one was, from every position -- including across the wrap,
//!    which is where an off-by-one lives.
//! 3. **Every focusable element is reachable** from any starting point,
//!    in at most as many presses as there are elements.
//!
//! ## Why the key table is data
//!
//! The 16.4 table is transcribed once, here, as a table. Transcription is
//! verified by comparison, and a comparison needs something to compare
//! **to** -- a `match` scattered through a routing function cannot be
//! held up against the specification. The same reasoning as the layout
//! tables of 10.2.4, and the same reason phase 9 put the 13.2 table in
//! data.

use crate::gui::protocol::KeyEvent;

use super::tree::{NodeId, Tree};

// Scancodes of set 1 the 16.4 table names.
pub const SCAN_TAB: u8 = 0x0F;
pub const SCAN_ENTER: u8 = 0x1C;
pub const SCAN_SPACE: u8 = 0x39;
pub const SCAN_F6: u8 = 0x40;
pub const SCAN_ESCAPE: u8 = 0x01;
/// `s z c k`, the four accessibility toggles, on the physical keys an
/// AZERTY keyboard puts them.
pub const SCAN_S: u8 = 0x1F;
pub const SCAN_Z: u8 = 0x11;
pub const SCAN_C: u8 = 0x2E;
pub const SCAN_K: u8 = 0x25;

/// What a keystroke means to the navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Tab -- next focusable element.
    FocusNext,
    /// Shift+Tab -- previous.
    FocusPrevious,
    /// Enter or Space -- activate the focused element.
    Activate,
    /// F6 -- next panel of the window.
    NextPanel,
    /// Alt+Super+S -- toggle the screen reader.
    ToggleReader,
    /// Alt+Super+Z -- toggle the zoom.
    ToggleZoom,
    /// Alt+Super+C -- toggle high contrast.
    ToggleContrast,
    /// Alt+Super+K -- toggle keyboard navigation.
    ToggleKeyboard,
    /// Escape -- leave the current element.
    Cancel,
}

impl Action {
    pub fn as_str(&self) -> &'static str {
        match self {
            Action::FocusNext => "element suivant",
            Action::FocusPrevious => "element precedent",
            Action::Activate => "activer",
            Action::NextPanel => "panneau suivant",
            Action::ToggleReader => "lecteur d'ecran",
            Action::ToggleZoom => "zoom",
            Action::ToggleContrast => "contraste eleve",
            Action::ToggleKeyboard => "navigation clavier",
            Action::Cancel => "annuler",
        }
    }

    /// Does this action toggle an accessibility feature?
    ///
    /// Those four must reach the coordinator whatever else is going on --
    /// that is the whole content of "global" in 16.4.
    pub fn is_toggle(&self) -> bool {
        matches!(
            self,
            Action::ToggleReader
                | Action::ToggleZoom
                | Action::ToggleContrast
                | Action::ToggleKeyboard
        )
    }
}

/// The 16.4 table, transcribed.
///
/// Each row is (scancode, alt required, super required, shift required,
/// action). Written as data so the self-test can compare it with the
/// specification rather than re-read the code that implements it.
pub type Binding = (u8, bool, bool, bool, Action);

pub static BINDINGS: &[Binding] = &[
    (SCAN_S, true, true, false, Action::ToggleReader),
    (SCAN_Z, true, true, false, Action::ToggleZoom),
    (SCAN_C, true, true, false, Action::ToggleContrast),
    (SCAN_K, true, true, false, Action::ToggleKeyboard),
    // Shift+Tab before Tab: the first match wins, and a plain-Tab row
    // placed first would swallow the shifted one -- the classic way a
    // "backwards" shortcut silently never fires.
    (SCAN_TAB, false, false, true, Action::FocusPrevious),
    (SCAN_TAB, false, false, false, Action::FocusNext),
    (SCAN_ENTER, false, false, false, Action::Activate),
    (SCAN_SPACE, false, false, false, Action::Activate),
    (SCAN_F6, false, false, false, Action::NextPanel),
    (SCAN_ESCAPE, false, false, false, Action::Cancel),
];

/// Reads a key event as a navigation action.
///
/// The three modifiers are matched **exactly**. Matching "at least these"
/// looks more permissive and is worse: Shift+Tab would then also match
/// the plain-Tab row, and which of the two won would depend on the order
/// of the table -- a rule decided by a line number.
///
/// Returns `None` for anything the table does not name, which is what
/// lets an ordinary keystroke reach the application untouched: a
/// navigation module that consumed every key would make typing
/// impossible.
///
/// Super+Tab is deliberately absent from the table: 16.4 gives it to
/// window switching, which is the window manager's business
/// (`winmanager.mod`, phase 7.3.5). Claiming it here would give one key
/// two owners, and the exact match above is what keeps it out.
pub fn action_for(event: &KeyEvent) -> Option<Action> {
    if !event.pressed {
        return None;
    }
    let alt = event.has(KeyEvent::MOD_ALT);
    let super_key = event.has(KeyEvent::MOD_SUPER);
    let shift = event.has(KeyEvent::MOD_SHIFT);

    BINDINGS
        .iter()
        .find(|(scancode, needs_alt, needs_super, needs_shift, _)| {
            *scancode == event.scancode
                && *needs_alt == alt
                && *needs_super == super_key
                && *needs_shift == shift
        })
        .map(|(_, _, _, _, action)| *action)
}

/// Should this action be consumed rather than passed to the application?
///
/// Space activates a button and types a space in a text field, and both
/// are right. The role of the focused element is what tells them apart,
/// so the decision cannot live in `action_for` -- it has no tree. The
/// coordinator asks this before consuming.
pub fn consumes(action: Action, focused_role: Option<super::tree::Role>) -> bool {
    use super::tree::Role;
    match action {
        // Never given up: 16.4 calls them global, and a full-screen
        // application must not be able to make accessibility
        // unreachable.
        action if action.is_toggle() => true,
        // In a text field, Space and Enter belong to the field.
        Action::Activate => !matches!(focused_role, Some(Role::Field)),
        _ => true,
    }
}

/// Applies a navigation action to a tree.
///
/// Returns the node that ends up focused, when the action moves the
/// focus. The activation actions return the focused node without moving
/// it: it is the caller that knows what activating means for a button as
/// opposed to a check box.
pub fn apply(tree: &mut Tree, action: Action) -> Option<NodeId> {
    match action {
        Action::FocusNext => tree.move_focus(true),
        Action::FocusPrevious => tree.move_focus(false),
        Action::NextPanel => tree.next_panel(),
        Action::Activate => Some(tree.focus()).filter(|id| *id != 0),
        // Leaving an element means leaving it, not moving on: the focus
        // goes to the enclosing panel if there is one, and nowhere
        // otherwise. Making Escape move to the next element would make it
        // a second Tab, and there would then be no way to *stop*.
        Action::Cancel => {
            let parent = tree.focused().map(|node| node.parent).unwrap_or(0);
            if parent != 0 && tree.set_focus(parent) {
                Some(parent)
            } else {
                None
            }
        }
        _ => None,
    }
}

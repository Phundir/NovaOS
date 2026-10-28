//! 7.6 -- Notification system (spec 9).
//!
//! ## The five levels
//!
//! The 9.1 table crosses five levels with five behaviours. It is
//! transcribed as-is into `Level`, one method per column, with no
//! interpretation: it is a decision table, not an algorithm, and
//! transcribing it faithfully is all that is asked of it.
//!
//! The point deserving attention is `DND mode` -- "do not disturb". Two
//! levels ignore it (PRIORITY, URGENT); the other three are blocked. A
//! mode with no exception at all would let a critical battery warning
//! pass in silence, which nobody wants.
//! veut.
//!
//! ## Dismissal conditions (9.2)
//!
//! Five conditions, plus their combination `Any([...])` -- the first met
//! wins. The combination is not a special case to code separately:
//! `Dismissal` is a **vector** of conditions, and "a single condition" is
//! the one-element vector. The general case is the only case.
//!
//! A point the spec leaves open: what to do with a notification whose
//! vector is empty. Choice kept -- it never disappears by itself, like
//! PRIORITY. It is the least surprising behaviour: declaring nothing is
//! asking for nothing.
//!
//! ## Permissions (9.3)
//!
//! The table crosses level and binary type. A `.gs` cannot emit a
//! PRIORITY, a `.ras` can emit only LOW and NORMAL. The check happens at
//! emission, and a refusal is counted: an application that insists must
//! be visible in the diagnostics.

use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::header::BinaryFormat;
use crate::process::Pid;

/// Identifier of a live notification.
pub type NotificationId = u32;

/// Maximum number of notifications kept at the centre.
pub const MAX_NOTIFICATIONS: usize = 32;

/// Maximum number of toasts displayed at the same time.
pub const MAX_TOASTS: usize = 4;

// ============================================================
//  9.1 -- Priority levels
// ============================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Level {
    Prioritaire = 0,
    Urgent = 1,
    Haute = 2,
    Normale = 3,
    Basse = 4,
}

impl Level {
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Level::Prioritaire),
            1 => Some(Level::Urgent),
            2 => Some(Level::Haute),
            3 => Some(Level::Normale),
            4 => Some(Level::Basse),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Level::Prioritaire => "PRIORITAIRE",
            Level::Urgent => "URGENT",
            Level::Haute => "HAUTE",
            Level::Normale => "NORMALE",
            Level::Basse => "BASSE",
        }
    }

    /// "Toast" column. `Some(false)` means "only if the session is
    /// active" -- the NORMAL case, which the spec notes "If active".
    pub fn toast(&self, session_active: bool) -> bool {
        match self {
            Level::Prioritaire | Level::Urgent | Level::Haute => true,
            Level::Normale => session_active,
            Level::Basse => false,
        }
    }

    /// "Sound" column. NORMAL is noted "Opt.": the sound depends on the
    /// setting of the sender, hence the parameter.
    pub fn sound(&self, requested: bool) -> bool {
        match self {
            Level::Prioritaire | Level::Urgent | Level::Haute => true,
            Level::Normale => requested,
            Level::Basse => false,
        }
    }

    /// "Full screen" column.
    pub fn fullscreen(&self) -> bool {
        matches!(self, Level::Prioritaire | Level::Urgent)
    }

    /// "DND mode" column: `true` when the mode does not think.
    pub fn ignores_do_not_disturb(&self) -> bool {
        matches!(self, Level::Prioritaire | Level::Urgent)
    }

    /// "Dismissal" column, in milliseconds. `None` for PRIORITY, which
    /// demands a button, and for LOW, which is not displayed as a toast.
    pub fn auto_timeout_ms(&self) -> Option<u64> {
        match self {
            Level::Prioritaire => None,
            Level::Urgent => Some(15_000),
            Level::Haute => Some(8_000),
            Level::Normale => Some(5_000),
            Level::Basse => None,
        }
    }

    /// PRIORITY demands a button: it cannot disappear on its own.
    pub fn requires_action(&self) -> bool {
        *self == Level::Prioritaire
    }
}

/// 9.3 -- May a binary of this format emit at this level?
///
/// Direct transcription of the table. No rule derived: the HIGH line
/// allows `.cus` but not `.ras`, and nothing in the hierarchy of the
/// formats gave that away.
pub fn allowed(level: Level, format: BinaryFormat) -> bool {
    use BinaryFormat::*;
    match level {
        Level::Prioritaire => matches!(format, Dsa | Ss),
        Level::Urgent => matches!(format, Dsa | Ss | Gs),
        Level::Haute => matches!(format, Dsa | Ss | Gs | Cus),
        Level::Normale | Level::Basse => true,
    }
}

// ============================================================
//  9.2 -- Dismissal conditions
// ============================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    /// Fixed deadline, in milliseconds since the emission.
    Timeout(u64),
    /// Automatic duration according to the level.
    Automatic,
    /// Manual close by the user.
    Manual,
    /// Click on a given action button.
    Action(u8),
    /// IPC event carrying this label, sent by the sender.
    IpcSignal(u32),
}

impl Condition {
    pub fn as_str(&self) -> &'static str {
        match self {
            Condition::Timeout(_) => "timeout",
            Condition::Automatic => "duree automatique",
            Condition::Manual => "fermeture manuelle",
            Condition::Action(_) => "bouton d'action",
            Condition::IpcSignal(_) => "signal IPC",
        }
    }
}

/// What makes a notification disappear.
///
/// The `Any([c1, c2])` of 9.2 is the general case: the first condition
/// met wins. An empty vector means "none" -- the notification stays until
/// it is removed explicitly.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Dismissal {
    pub conditions: Vec<Condition>,
}

impl Dismissal {
    pub fn none() -> Self {
        Dismissal {
            conditions: Vec::new(),
        }
    }

    pub fn any(conditions: &[Condition]) -> Self {
        Dismissal {
            conditions: conditions.to_vec(),
        }
    }

    pub fn automatic() -> Self {
        Dismissal::any(&[Condition::Automatic, Condition::Manual])
    }

    /// Effective deadline, in ticks, or `None` if no temporal condition
    /// applies.
    ///
    /// The shortest wins: it is the reading of "the first met" applied to
    /// time.
    pub fn deadline_ticks(&self, level: Level, posted_at: u64) -> Option<u64> {
        let per_tick = 1000 / crate::TIMER_HZ as u64;
        self.conditions
            .iter()
            .filter_map(|condition| match condition {
                Condition::Timeout(ms) => Some(*ms),
                Condition::Automatic => level.auto_timeout_ms(),
                _ => None,
            })
            .map(|ms| posted_at + ms.div_ceil(per_tick).max(1))
            .min()
    }

    pub fn accepts_manual(&self) -> bool {
        self.conditions.contains(&Condition::Manual)
    }

    pub fn accepts_action(&self, button: u8) -> bool {
        self.conditions
            .iter()
            .any(|condition| *condition == Condition::Action(button))
    }

    pub fn accepts_signal(&self, tag: u32) -> bool {
        self.conditions
            .iter()
            .any(|condition| *condition == Condition::IpcSignal(tag))
    }
}

// ============================================================
//  Notification
// ============================================================

#[derive(Debug, Clone)]
pub struct Notification {
    pub id: NotificationId,
    pub sender: Pid,
    pub app_id: String,
    pub level: Level,
    pub title: String,
    pub body: String,
    pub dismissal: Dismissal,
    pub posted_at: u64,
    /// Number of buttons offered. PRIORITY demands at least one.
    pub buttons: u8,
    pub sound: bool,
    /// The toast is removed but the notification stays at the centre
    /// (9.1, LOW column: "Centre only").
    pub toast_visible: bool,
}

impl Notification {
    pub fn deadline(&self) -> Option<u64> {
        self.dismissal.deadline_ticks(self.level, self.posted_at)
    }
}

/// Reason for a refusal at emission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostError {
    /// The 9.3 table forbids this level to this format.
    LevelDenied,
    /// A PRIORITY with no button could never disappear.
    ActionRequired,
    /// The centre is full.
    Full,
    UnknownLevel,
}

impl PostError {
    pub fn message(&self) -> &'static str {
        match self {
            PostError::LevelDenied => "niveau interdit a ce type de binaire (spec 9.3)",
            PostError::ActionRequired => "PRIORITAIRE exige au moins un bouton (spec 9.1)",
            PostError::Full => "centre de notifications plein",
            PostError::UnknownLevel => "niveau inconnu",
        }
    }
}

// ============================================================
//  Notification centre
// ============================================================

pub struct Center {
    items: Vec<Notification>,
    next_id: NotificationId,
    pub do_not_disturb: bool,
    pub session_active: bool,
    pub posted: u64,
    pub refused: u64,
    pub expired: u64,
    pub dismissed: u64,
}

impl Center {
    pub const fn new() -> Self {
        Center {
            items: Vec::new(),
            next_id: 1,
            do_not_disturb: false,
            session_active: true,
            posted: 0,
            refused: 0,
            expired: 0,
            dismissed: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Notification> {
        self.items.iter()
    }

    /// Toasts to display, from the most urgent to the least, capped.
    ///
    /// The sort by level then by age guarantees that an URGENT never
    /// falls behind three NORMALs that arrived before it.
    pub fn toasts(&self) -> Vec<&Notification> {
        let mut visible: Vec<&Notification> = self
            .items
            .iter()
            .filter(|item| item.toast_visible)
            .collect();
        visible.sort_by_key(|item| (item.level, item.posted_at));
        visible.truncate(MAX_TOASTS);
        visible
    }

    /// The full-screen notification to display, if there is one.
    pub fn fullscreen(&self) -> Option<&Notification> {
        self.items
            .iter()
            .filter(|item| item.level.fullscreen() && item.toast_visible)
            .min_by_key(|item| (item.level, item.posted_at))
    }

    /// 9.3 -- Emits a notification on behalf of a given binary.
    #[allow(clippy::too_many_arguments)]
    pub fn post(
        &mut self,
        sender: Pid,
        app_id: &str,
        format: BinaryFormat,
        level: Level,
        title: &str,
        body: &str,
        dismissal: Dismissal,
        buttons: u8,
        sound: bool,
        now: u64,
    ) -> Result<NotificationId, PostError> {
        if !allowed(level, format) {
            self.refused += 1;
            return Err(PostError::LevelDenied);
        }
        if level.requires_action() && buttons == 0 {
            self.refused += 1;
            return Err(PostError::ActionRequired);
        }
        if self.items.len() >= MAX_NOTIFICATIONS {
            self.refused += 1;
            return Err(PostError::Full);
        }

        // The "do not disturb" mode does not remove the notification: it
        // takes away its toast. It stays readable at the centre, which
        // the column of 9.1 describes and a plain refusal would betray.
        let muted = self.do_not_disturb && !level.ignores_do_not_disturb();
        let toast_visible = level.toast(self.session_active) && !muted;

        let id = self.next_id;
        self.next_id += 1;

        self.items.push(Notification {
            id,
            sender,
            app_id: String::from(app_id),
            level,
            title: String::from(title),
            body: String::from(body),
            dismissal,
            posted_at: now,
            buttons,
            sound: level.sound(sound) && !muted,
            toast_visible,
        });
        self.posted += 1;
        Ok(id)
    }

    /// 9.2 -- Removes the notifications whose deadline has passed.
    ///
    /// Returns the number of removals, which tells the compositor whether
    /// to repaint -- a deadline falling is the only thing that changes the
    /// screen with no message arriving.
    pub fn expire(&mut self, now: u64) -> usize {
        let before = self.items.len();
        self.items.retain(|item| match item.deadline() {
            Some(deadline) => now < deadline,
            None => true,
        });
        let removed = before - self.items.len();
        self.expired += removed as u64;
        removed
    }

    /// Manual close.
    pub fn dismiss(&mut self, id: NotificationId) -> bool {
        let Some(index) = self.items.iter().position(|item| item.id == id) else {
            return false;
        };
        // PRIORITY closes only through a button: closing it with a click
        // elsewhere would cancel the one guarantee of the level.
        if !self.items[index].dismissal.accepts_manual() {
            return false;
        }
        self.items.remove(index);
        self.dismissed += 1;
        true
    }

    /// Click on an action button. Returns the sender to inform.
    pub fn act(&mut self, id: NotificationId, button: u8) -> Option<Pid> {
        let index = self.items.iter().position(|item| item.id == id)?;
        let sender = self.items[index].sender;
        // A button always removes a PRIORITY: it is its dismissal
        // condition by definition (9.1). For the others, the condition
        // must have been declared.
        let accepted = self.items[index].level.requires_action()
            || self.items[index].dismissal.accepts_action(button);
        if accepted {
            self.items.remove(index);
            self.dismissed += 1;
        }
        Some(sender)
    }

    /// IPC event emitted by the application: "the transfer is finished".
    pub fn signal(&mut self, sender: Pid, tag: u32) -> usize {
        let before = self.items.len();
        self.items
            .retain(|item| !(item.sender == sender && item.dismissal.accepts_signal(tag)));
        let removed = before - self.items.len();
        self.dismissed += removed as u64;
        removed
    }

    /// Removes everything coming from a departed process.
    pub fn forget(&mut self, sender: Pid) -> usize {
        let before = self.items.len();
        self.items.retain(|item| item.sender != sender);
        before - self.items.len()
    }

    pub fn counts(&self) -> [usize; 5] {
        let mut counts = [0usize; 5];
        for item in self.items.iter() {
            counts[item.level as usize] += 1;
        }
        counts
    }
}

impl Default for Center {
    fn default() -> Self {
        Center::new()
    }
}

// ============================================================
//  Protocol encoding (OP_NOTIFY_POST)
// ============================================================

/// Decodes the payload of an emission.
///
/// ```text
///   0x00  1  level
///   0x01  1  number of buttons
///   0x02  1  sound requested
///   0x03  1  number of conditions
///   0x04  n  conditions: 1 kind byte + 4 argument bytes
///   ...      title, one length byte then the text
///   ...      body, up to the end
/// ```
pub fn decode_post(payload: &[u8]) -> Option<(Level, u8, bool, Dismissal, String, String)> {
    use super::protocol::Reader;

    let mut reader = Reader::new(payload);
    let level = Level::from_code(reader.u8()?)?;
    let buttons = reader.u8()?;
    let sound = reader.u8()? != 0;
    let condition_count = reader.u8()?;

    let mut conditions = Vec::new();
    for _ in 0..condition_count {
        let kind = reader.u8()?;
        let argument = reader.u32()?;
        conditions.push(match kind {
            1 => Condition::Timeout(argument as u64),
            2 => Condition::Automatic,
            3 => Condition::Manual,
            4 => Condition::Action(argument as u8),
            5 => Condition::IpcSignal(argument),
            _ => return None,
        });
    }

    let title_len = reader.u8()? as usize;
    let title_bytes = reader.bytes(title_len)?;
    let title: String = title_bytes
        .iter()
        .copied()
        .map(|byte| if (0x20..=0x7e).contains(&byte) { byte as char } else { '?' })
        .collect();
    let body = reader.text();

    Some((level, buttons, sound, Dismissal { conditions }, title, body))
}

/// Encodes an emission - exact mirror of `decode_post`.
pub fn encode_post(
    level: Level,
    buttons: u8,
    sound: bool,
    dismissal: &Dismissal,
    title: &str,
    body: &str,
) -> Vec<u8> {
    use super::protocol::Writer;

    let mut writer = Writer::new()
        .u8(level as u8)
        .u8(buttons)
        .u8(u8::from(sound))
        .u8(dismissal.conditions.len() as u8);

    for condition in dismissal.conditions.iter() {
        let (kind, argument) = match condition {
            Condition::Timeout(ms) => (1u8, *ms as u32),
            Condition::Automatic => (2, 0),
            Condition::Manual => (3, 0),
            Condition::Action(button) => (4, *button as u32),
            Condition::IpcSignal(tag) => (5, *tag),
        };
        writer = writer.u8(kind).u32(argument);
    }

    let title_bytes = title.len().min(64);
    writer
        .u8(title_bytes as u8)
        .text(&title[..title_bytes])
        .text(body)
        .build()
}

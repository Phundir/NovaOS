//! 7.4 -- IPC protocol of the graphics system.
//!
//! ## Why IPC and not system calls
//!
//! 8.1 is explicit: "Applications never draw directly: they send IPC
//! instructions and the compositor performs the rendering." A `SYS_GUI_*`
//! domain would put the kernel on the drawing path, which the spec
//! refuses -- and the compositor is a `.ss` application, not a kernel
//! subsystem. So the protocol lives entirely above `ipc`, on the channel
//! named `sys.gui`.
//!
//! ## Form of a message
//!
//! The label carries the operation code, the payload the arguments. A
//! request receives an answer, an event expects none.
//!
//! The integers are decoded field by field in little-endian: the payload
//! arrives from a foreign address space and nothing guarantees its
//! alignment.
//!
//! ## The 256-byte limit
//!
//! `ipc::MAX_PAYLOAD` caps a message at 256 bytes. That is comfortable
//! for a rectangle or a line of text, and rigorously insufficient for an
//! image. `DRAW_ICON` therefore carries a **monochrome 16x16** icon, that
//! is thirty-two bytes of bits plus two colours: enough to paint a task
//! bar pictogram, not a photograph. Real images demand shared memory,
//! which demands a CR3 per process -- a limit opened by phase 3, and not
//! avoidable by a protocol.

use alloc::string::String;
use alloc::vec::Vec;

// ============================================================
//  Operation codes
// ============================================================

// --- surfaces (7.2.3) ---
pub const OP_SURFACE_CREATE: u32 = 0x01;
pub const OP_SURFACE_DESTROY: u32 = 0x02;
pub const OP_SURFACE_MOVE: u32 = 0x03;
pub const OP_SURFACE_RESIZE: u32 = 0x04;
pub const OP_SURFACE_RAISE: u32 = 0x05;
pub const OP_SURFACE_LOWER: u32 = 0x06;
pub const OP_SURFACE_SHOW: u32 = 0x07;
pub const OP_SURFACE_FOCUS: u32 = 0x08;
pub const OP_SURFACE_LIST: u32 = 0x09;
/// The windows with their titles, for a task bar. `OP_SURFACE_LIST`
/// carries four bytes per window and no title, which is how the bar came
/// to label every button "fenetre N".
///
/// Request: `u8 first` -- the rank to start from. Reply: error, `u8
/// total`, `u8 count`, then `count` entries `u16 id, u8 flags, u8 length,
/// title`. Flag bit 0: visible; bit 1: has the focus. As many entries as
/// fit in one message; the client asks again from `first + count`.
pub const OP_WINDOW_ENTRIES: u32 = 0x0A;
/// Changes the title of a surface: `u32 id, title`. A browser names its
/// window after the page it shows.
pub const OP_SURFACE_TITLE: u32 = 0x0B;

/// Bit of `OP_WINDOW_ENTRIES`: the window is shown.
pub const WINDOW_VISIBLE: u8 = 1;
/// Bit of `OP_WINDOW_ENTRIES`: the window has the focus.
pub const WINDOW_FOCUSED: u8 = 2;
/// Longest title an entry carries; the bar shows fewer characters anyway.
pub const WINDOW_TITLE_MAX: usize = 32;

// --- drawing (7.4.1) ---
pub const OP_DRAW_RESET: u32 = 0x10;
pub const OP_DRAW_FILL: u32 = 0x11;
pub const OP_DRAW_RECT: u32 = 0x12;
pub const OP_DRAW_FRAME: u32 = 0x13;
pub const OP_DRAW_TEXT: u32 = 0x14;
pub const OP_DRAW_LINE: u32 = 0x15;
pub const OP_DRAW_ICON: u32 = 0x16;
pub const OP_COMMIT: u32 = 0x1F;

/// Several drawing commands of one surface in one message.
///
/// ```text
///   u32 surface
///   then, repeated:  u8 operation (0x11..0x16)  u8 length  length bytes
/// ```
///
/// The bytes of an entry are the payload of the single command **without**
/// its leading surface identifier. A message carries 256 bytes and a
/// rectangle takes 16: one message per command, which is what the first
/// version sent, filled the compositor's box of 64 at the second window
/// and made every client wait for room -- the lag of the applications.
pub const OP_DRAW_BATCH: u32 = 0x17;
/// Copies a client's pixel canvas into its surface: `u32 surface, u32
/// canvas, i16 x, i16 y`. One command whatever the canvas holds -- see
/// `gui::canvas`. Not accepted inside a batch: it names an object, not a
/// stroke, and is refused when the canvas is not the sender's.
pub const OP_DRAW_CANVAS: u32 = 0x18;

/// Bit of the label: the sender does not want an answer.
///
/// The label stays the operation number -- entry 29 of DECISIONS.md --
/// and this bit rides on top of it. Without it the compositor answered
/// every drawing command, and those answers, which nobody reads, filled
/// the **client's** box of 64: a repaint of 150 commands left no room for
/// the next click, which was lost or waited behind them.
pub const NO_REPLY: u32 = 1 << 31;

// --- screen and theme ---
pub const OP_SCREEN_INFO: u32 = 0x20;
pub const OP_THEME_GET: u32 = 0x21;
pub const OP_THEME_RELOAD: u32 = 0x22;

/// Shape the pointer must take. Declared by whoever knows what is under
/// it -- that is the client, never the compositor: the compositor knows
/// which surface the pointer is over, and nothing of what is inside it.
pub const OP_CURSOR_SHAPE: u32 = 0x23;

// --- notifications (7.6) ---
pub const OP_NOTIFY_POST: u32 = 0x30;
pub const OP_NOTIFY_DISMISS: u32 = 0x31;
pub const OP_NOTIFY_COUNT: u32 = 0x32;
pub const OP_NOTIFY_ACTION: u32 = 0x33;

// --- modules (7.3) ---
pub const OP_MODULE_LIST: u32 = 0x40;
pub const OP_MODULE_RESCAN: u32 = 0x41;

/// Asks for the application menu to be opened or closed.
///
/// Sent by whoever owns the button that toggles it -- `taskbar.mod` -- and
/// **broadcast** by the compositor to the shell modules, exactly as a
/// global shortcut of 16.4 is. The task bar does not address the launcher
/// directly: they are two independently replaceable modules of 8.2, and
/// one knowing the other's PID would be a private protocol between two
/// components that must each be swappable without the other.
pub const OP_MENU_TOGGLE: u32 = 0x42;

/// Asks for a session or power action: lock, log out, reboot, power off.
///
/// One byte of payload, a `SessionAction` code. Only a **shell** module may
/// send it -- a client owning a surface of `Layer::Shell` -- and only while
/// a session is open and unlocked. An application that could power the
/// machine off through the graphics protocol would hold a capability 12.3
/// never gave it.
///
/// The compositor answers at once and performs the action **after** the
/// reply, outside its lock: every one of the four spawns a task, kills
/// tasks or touches the disk, none of which may happen in `handle`
/// (entry 47 of DECISIONS.md).
pub const OP_SESSION_ACTION: u32 = 0x43;

// --- events pushed towards the clients (7.4.2) ---
pub const EV_KEY: u32 = 0x80;
pub const EV_POINTER: u32 = 0x81;
pub const EV_FOCUS: u32 = 0x82;
pub const EV_CLOSE: u32 = 0x83;
pub const EV_RESIZED: u32 = 0x84;
pub const EV_NOTIFY_ACTION: u32 = 0x85;
pub const EV_REDRAW: u32 = 0x86;
/// The menu must open or close. Broadcast to the shell modules.
pub const EV_MENU_TOGGLE: u32 = 0x87;
/// The screen changed size: `u16` width, `u16` height. Broadcast to every
/// client owning a surface, because each one placed its surfaces from the
/// old size -- the task bar at the old bottom edge, the login panel at the
/// old centre -- and only the client knows how to place them again.
pub const EV_SCREEN_RESIZED: u32 = 0x88;
/// To the shell modules: a window opened, closed, took the focus or
/// changed its title. No payload -- the bar asks `OP_WINDOW_ENTRIES`.
pub const EV_WINDOWS_CHANGED: u32 = 0x89;

/// Readable name of an operation - used by the log and by `OP_MODULE_LIST`.
pub fn op_name(op: u32) -> &'static str {
    match op {
        OP_SURFACE_CREATE => "SURFACE_CREATE",
        OP_SURFACE_DESTROY => "SURFACE_DESTROY",
        OP_SURFACE_MOVE => "SURFACE_MOVE",
        OP_SURFACE_RESIZE => "SURFACE_RESIZE",
        OP_SURFACE_RAISE => "SURFACE_RAISE",
        OP_SURFACE_LOWER => "SURFACE_LOWER",
        OP_SURFACE_SHOW => "SURFACE_SHOW",
        OP_SURFACE_FOCUS => "SURFACE_FOCUS",
        OP_SURFACE_LIST => "SURFACE_LIST",
        OP_WINDOW_ENTRIES => "WINDOW_ENTRIES",
        OP_DRAW_CANVAS => "DRAW_CANVAS",
        EV_WINDOWS_CHANGED => "WINDOWS_CHANGED",
        OP_SURFACE_TITLE => "SURFACE_TITLE",
        OP_DRAW_RESET => "DRAW_RESET",
        OP_DRAW_FILL => "DRAW_FILL",
        OP_DRAW_RECT => "DRAW_RECT",
        OP_DRAW_FRAME => "DRAW_FRAME",
        OP_DRAW_TEXT => "DRAW_TEXT",
        OP_DRAW_LINE => "DRAW_LINE",
        OP_DRAW_ICON => "DRAW_ICON",
        OP_COMMIT => "COMMIT",
        OP_DRAW_BATCH => "DRAW_BATCH",
        OP_SCREEN_INFO => "SCREEN_INFO",
        OP_THEME_GET => "THEME_GET",
        OP_THEME_RELOAD => "THEME_RELOAD",
        OP_CURSOR_SHAPE => "CURSOR_SHAPE",
        OP_NOTIFY_POST => "NOTIFY_POST",
        OP_NOTIFY_DISMISS => "NOTIFY_DISMISS",
        OP_NOTIFY_COUNT => "NOTIFY_COUNT",
        OP_NOTIFY_ACTION => "NOTIFY_ACTION",
        OP_MODULE_LIST => "MODULE_LIST",
        OP_MODULE_RESCAN => "MODULE_RESCAN",
        OP_MENU_TOGGLE => "MENU_TOGGLE",
        OP_SESSION_ACTION => "SESSION_ACTION",
        EV_KEY => "EV_KEY",
        EV_POINTER => "EV_POINTER",
        EV_FOCUS => "EV_FOCUS",
        EV_CLOSE => "EV_CLOSE",
        EV_RESIZED => "EV_RESIZED",
        EV_NOTIFY_ACTION => "EV_NOTIFY_ACTION",
        EV_REDRAW => "EV_REDRAW",
        EV_MENU_TOGGLE => "EV_MENU_TOGGLE",
        EV_SCREEN_RESIZED => "EV_SCREEN_RESIZED",
        _ => "?",
    }
}

// ============================================================
//  Refusal codes
// ============================================================

/// A one-byte answer other than zero is a refusal.
///
/// The protocol does not take back `SysResult`: that one describes system
/// calls, and the compositor is an application. The vocabulary stays
/// short, failing which every client would have to know an error table
/// that does not concern it.
pub const ERR_NONE: u8 = 0;
pub const ERR_BAD_REQUEST: u8 = 1;
pub const ERR_NO_SURFACE: u8 = 2;
pub const ERR_NOT_OWNER: u8 = 3;
pub const ERR_TOO_MANY: u8 = 4;
pub const ERR_DENIED: u8 = 5;

pub fn error_name(code: u8) -> &'static str {
    match code {
        ERR_NONE => "accepte",
        ERR_BAD_REQUEST => "requete malformee",
        ERR_NO_SURFACE => "surface inconnue",
        ERR_NOT_OWNER => "surface appartenant a un autre processus",
        ERR_TOO_MANY => "trop de surfaces",
        ERR_DENIED => "refuse par la politique",
        _ => "?",
    }
}

/// Payload of a refusal.
pub fn refusal(code: u8) -> Vec<u8> {
    alloc::vec![code]
}

// ============================================================
//  Field-by-field reading
// ============================================================

/// Reading cursor on a payload.
///
/// Every read returns an `Option`: a truncated payload must end in a
/// refusal, never in an invented value nor in a panic. It is the only
/// discipline that holds against a malicious client, and the protocol
/// will have real ones when the modules run in ring 3.
pub struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Reader { bytes, offset: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.bytes.len() - self.offset.min(self.bytes.len())
    }

    pub fn u8(&mut self) -> Option<u8> {
        let value = *self.bytes.get(self.offset)?;
        self.offset += 1;
        Some(value)
    }

    pub fn u16(&mut self) -> Option<u16> {
        let slice = self.bytes.get(self.offset..self.offset + 2)?;
        self.offset += 2;
        Some(u16::from_le_bytes([slice[0], slice[1]]))
    }

    pub fn i16(&mut self) -> Option<i16> {
        self.u16().map(|value| value as i16)
    }

    pub fn u32(&mut self) -> Option<u32> {
        let slice = self.bytes.get(self.offset..self.offset + 4)?;
        self.offset += 4;
        Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
    }

    pub fn bytes(&mut self, count: usize) -> Option<&'a [u8]> {
        let slice = self.bytes.get(self.offset..self.offset + count)?;
        self.offset += count;
        Some(slice)
    }

    /// The rest of the payload, read as text.
    ///
    /// The non-printable bytes are discarded here and not at render time:
    /// a window title crosses the log, the task bar and the screen, and
    /// filtering it once on entry is worth more than three times on exit.
    pub fn text(&mut self) -> String {
        let slice = &self.bytes[self.offset.min(self.bytes.len())..];
        self.offset = self.bytes.len();
        slice
            .iter()
            .copied()
            .take_while(|byte| *byte != 0)
            .map(|byte| if (0x20..=0x7e).contains(&byte) { byte as char } else { '?' })
            .collect()
    }
}

// ============================================================
//  Writing
// ============================================================

/// Payload accumulator.
#[derive(Default)]
pub struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Writer { bytes: Vec::new() }
    }

    pub fn u8(mut self, value: u8) -> Self {
        self.bytes.push(value);
        self
    }

    pub fn u16(mut self, value: u16) -> Self {
        self.bytes.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub fn i16(self, value: i16) -> Self {
        self.u16(value as u16)
    }

    pub fn u32(mut self, value: u32) -> Self {
        self.bytes.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub fn raw(mut self, value: &[u8]) -> Self {
        self.bytes.extend_from_slice(value);
        self
    }

    /// Adds text, truncated to what the payload can still carry.
    /// Truncating a title is of no consequence; exceeding `MAX_PAYLOAD`
    /// would make the whole message be refused.
    pub fn text(mut self, value: &str) -> Self {
        let room = crate::ipc::MAX_PAYLOAD.saturating_sub(self.bytes.len());
        for byte in value.bytes().take(room) {
            self.bytes.push(byte);
        }
        self
    }

    pub fn build(self) -> Vec<u8> {
        self.bytes
    }
}

// ============================================================
//  Input events (7.4.2)
// ============================================================

/// Size of a keyboard event as it is pushed towards a client.
pub const KEY_EVENT_SIZE: usize = 8;
/// Size of a pointer event.
pub const POINTER_EVENT_SIZE: usize = 12;

/// Keyboard event, already decoded by the driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub scancode: u8,
    pub pressed: bool,
    pub modifiers: u8,
    pub codepoint: u32,
}

impl KeyEvent {
    pub const MOD_SHIFT: u8 = 1 << 0;
    pub const MOD_CTRL: u8 = 1 << 1;
    pub const MOD_ALT: u8 = 1 << 2;
    /// Added by the compositor: the Super key is not returned by the
    /// driver, which knows only scancodes.
    pub const MOD_SUPER: u8 = 1 << 3;

    /// Decodes the event published by `dev.keyboard`.
    pub fn decode(payload: &[u8]) -> Option<Self> {
        let mut reader = Reader::new(payload);
        if reader.u8()? != 1 {
            return None;
        }
        Some(KeyEvent {
            scancode: reader.u8()?,
            pressed: reader.u8()? != 0,
            modifiers: reader.u8()?,
            codepoint: reader.u32()?,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        Writer::new()
            .u8(1)
            .u8(self.scancode)
            .u8(u8::from(self.pressed))
            .u8(self.modifiers)
            .u32(self.codepoint)
            .build()
    }

    pub fn character(&self) -> Option<char> {
        if self.codepoint == 0 {
            return None;
        }
        char::from_u32(self.codepoint)
    }

    pub fn has(&self, modifier: u8) -> bool {
        self.modifiers & modifier != 0
    }
}

/// Shape of the pointer.
///
/// ## A choice the spec leaves open
///
/// Section 8.2 says of `cursor.mod` only "rendering of the mouse
/// cursor" -- neither a list of shapes nor the rule that picks one. This
/// set of three is therefore a decision, and a deliberately small one:
/// what a pointer must say is "you may click here", "you may type here",
/// and nothing otherwise. A resize shape will come with the day windows
/// are resizable by their edges, which no phase asks for yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    /// Default: the classic arrow.
    Arrow = 0,
    /// Over something clickable -- a button, a row that selects.
    Hand = 1,
    /// Over a text entry.
    Text = 2,
}

impl CursorShape {
    pub fn as_code(&self) -> u8 {
        *self as u8
    }

    /// An unknown code gives the arrow rather than nothing.
    ///
    /// A pointer that disappears because a client asked for a shape this
    /// build does not know would be far worse than a pointer that stays an
    /// arrow, and the caller has no way to find out which of the two it
    /// caused.
    pub fn from_code(code: u8) -> Self {
        match code {
            1 => CursorShape::Hand,
            2 => CursorShape::Text,
            _ => CursorShape::Arrow,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            CursorShape::Arrow => "fleche",
            CursorShape::Hand => "main",
            CursorShape::Text => "texte",
        }
    }
}

/// What `OP_SESSION_ACTION` asks for.
///
/// The codes are the wire format: a replacement task bar living outside
/// the kernel sends the byte, not the enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAction {
    /// Covers the screen with the login panel, restricted to the account
    /// in session. Nothing else changes: windows and modules stay alive.
    Lock = 0,
    /// Ends the applications of the session and returns to the full
    /// login screen.
    Logout = 1,
    Reboot = 2,
    PowerOff = 3,
}

impl SessionAction {
    pub const ALL: [SessionAction; 4] = [
        SessionAction::Lock,
        SessionAction::Logout,
        SessionAction::Reboot,
        SessionAction::PowerOff,
    ];

    pub fn as_code(&self) -> u8 {
        *self as u8
    }

    /// An unknown code is refused, unlike a cursor shape.
    ///
    /// A shape that falls back to the arrow costs nothing; an action that
    /// falls back to *some* action could reboot a machine that was asked
    /// to lock.
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(SessionAction::Lock),
            1 => Some(SessionAction::Logout),
            2 => Some(SessionAction::Reboot),
            3 => Some(SessionAction::PowerOff),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            SessionAction::Lock => "verrouillage",
            SessionAction::Logout => "deconnexion",
            SessionAction::Reboot => "redemarrage",
            SessionAction::PowerOff => "extinction",
        }
    }
}

/// Pointer event, in screen coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PointerEvent {
    pub buttons: u8,
    pub x: u16,
    pub y: u16,
    pub dx: i16,
    pub dy: i16,
    /// Shape the pointer must take, as a `CursorShape` code.
    ///
    /// ## Why it travels here rather than in an event of its own
    ///
    /// `cursor.mod` already receives an `EV_POINTER` on every movement --
    /// that is how it follows the pointer at all. Putting the shape in it
    /// costs **no extra message**, where a dedicated event would add one
    /// per boundary crossed. The field takes the place of the two bytes of
    /// padding `encode` already wrote and `decode` already ignored.
    ///
    /// The price is one event of lag: the shape is seen on the movement
    /// after the one that changed it. That movement is the one that caused
    /// the change, so nobody perceives it -- but a pointer stopped exactly
    /// on a boundary keeps the previous shape until it moves again.
    ///
    /// The mouse driver leaves it at zero; only the compositor fills it.
    pub shape: u8,
    /// Wheel notches since the previous event: positive away from the
    /// user (scroll up), negative towards. Rides in the last byte, which
    /// was padding, for the same reason as `shape`.
    pub wheel: i8,
}

impl PointerEvent {
    pub const BUTTON_LEFT: u8 = 1 << 0;
    pub const BUTTON_RIGHT: u8 = 1 << 1;
    pub const BUTTON_MIDDLE: u8 = 1 << 2;

    /// Decodes the event published by `dev.mouse`.
    pub fn decode(payload: &[u8]) -> Option<Self> {
        let mut reader = Reader::new(payload);
        if reader.u8()? != 2 {
            return None;
        }
        let buttons = reader.u8()?;
        let (x, y, dx, dy) = (reader.u16()?, reader.u16()?, reader.i16()?, reader.i16()?);
        Some(PointerEvent {
            buttons,
            x,
            y,
            dx,
            dy,
            // Tolerated absent, and the reason matters: this field took
            // the place of a padding nobody read. A payload that stops
            // before it is a complete pointer event, not a truncated one,
            // and refusing it would break every producer that has not
            // been recompiled.
            shape: reader.u8().unwrap_or(0),
            wheel: reader.u8().unwrap_or(0) as i8,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        Writer::new()
            .u8(2)
            .u8(self.buttons)
            .u16(self.x)
            .u16(self.y)
            .i16(self.dx)
            .i16(self.dy)
            .u8(self.shape)
            .u8(self.wheel as u8)
            .build()
    }

    /// The shape this event carries.
    pub fn cursor_shape(&self) -> CursorShape {
        CursorShape::from_code(self.shape)
    }

    pub fn pressed(&self, button: u8) -> bool {
        self.buttons & button != 0
    }
}

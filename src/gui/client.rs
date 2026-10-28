//! Client side of the 7.4 protocol.
//!
//! ## The problem this module solves
//!
//! A module asking for the creation of a surface sends a request and
//! waits for its answer. During that wait, the compositor may push it a
//! keyboard or pointer event: `ipc::recv` will therefore return a message
//! that is not the expected answer.
//!
//! Throwing it away would lose keystrokes. Confusing it with the answer
//! would decode an event as a surface identifier. So the client keeps a
//! **local waiting queue**: what arrives off-topic is filed there, and
//! the module finds it again on its next lap. Nothing is lost, nothing is
//! confused.
//!
//! ## Why a structure and not free functions
//!
//! That queue is per-task state. A free function would have to file it in
//! a static indexed by PID -- more code, one more lock, and no gain: each
//! module already has its task and can hold its queue on its stack.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use crate::ipc::{self, IpcError, Message, MessageKind};
use crate::process::Pid;

use super::protocol::{self as proto, KeyEvent, PointerEvent, Reader, Writer};
use super::surface::{Layer, SurfaceId};

/// Connection of a task to the compositor.
pub struct Client {
    server: Pid,
    pending: VecDeque<Message>,
    /// Drawing commands not sent yet, for `batch_surface`. See `draw`.
    batch: Vec<u8>,
    batch_surface: Option<SurfaceId>,
    pub sent: u64,
    pub refused: u64,
    /// Number of times the queue of the compositor was full.
    pub blocked: u64,
}

/// A window as `window_entries` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowEntry {
    pub id: SurfaceId,
    pub visible: bool,
    pub focused: bool,
    pub title: String,
}

/// Event returned to the module by its loop.
#[derive(Debug, Clone)]
pub enum Event {
    Key(KeyEvent),
    Pointer(PointerEvent),
    Focus { surface: SurfaceId, gained: bool },
    Close(SurfaceId),
    Redraw,
    /// The menu must open or close -- the button was clicked, or Super was
    /// pressed. Broadcast to the shell modules.
    MenuToggle,
    /// A window opened, closed, took the focus or was renamed. Broadcast
    /// to the shell modules.
    WindowsChanged,
    /// The screen has a new size. Broadcast to every client.
    ScreenResized { width: i32, height: i32 },
    /// Application message to the module, outside the graphics protocol.
    Other(Message),
}

impl Client {
    /// Connects to the compositor, waiting for it to have opened its channel.
    ///
    /// The wait is bounded: a module must not spin forever if the
    /// compositor has failed. Returning `None` lets the task end cleanly,
    /// and the log say why.
    pub fn connect(timeout_ms: u64) -> Option<Self> {
        let per_tick = 1000 / crate::TIMER_HZ as u64;
        let deadline = crate::idt::ticks() + timeout_ms.div_ceil(per_tick).max(1);

        loop {
            if let Some(server) = ipc::channel_owner(super::compositor::CHANNEL) {
                return Some(Client {
                    server,
                    pending: VecDeque::new(),
                    batch: Vec::new(),
                    batch_surface: None,
                    sent: 0,
                    refused: 0,
                    blocked: 0,
                });
            }
            if crate::idt::ticks() >= deadline {
                return None;
            }
            crate::scheduler::yield_now();
        }
    }

    pub fn server(&self) -> Pid {
        self.server
    }

    /// Sends a request and waits for its answer.
    ///
    /// The correlation is made on the label, which carries the operation
    /// code. Two requests of the same code cannot be in flight at the
    /// same time from one task -- it is sequential by construction -- so
    /// the label is enough.
    pub fn request(&mut self, op: u32, payload: &[u8]) -> Option<Vec<u8>> {
        if !self.deliver(op, payload) {
            return None;
        }

        // Bounded: if the compositor dies along the way, the module must
        // not wait for eternity.
        for _ in 0..512 {
            let incoming = ipc::recv();
            if incoming.kind == MessageKind::Response && incoming.tag == op {
                return Some(incoming.payload);
            }
            self.pending.push_back(incoming);
        }
        None
    }

    /// Request expecting nothing back.
    ///
    /// Marked `NO_REPLY`, so that the compositor does not answer: the
    /// first version waited for no answer **and still got one**, one per
    /// command, into a box of 64 that the next click then found full. The
    /// refusals stay visible: the compositor counts them.
    pub fn tell(&mut self, op: u32, payload: &[u8]) {
        self.deliver(op | proto::NO_REPLY, payload);
    }

    /// Queues one drawing command; `body` is its payload without the
    /// surface identifier.
    ///
    /// ## Why commands are batched
    ///
    /// A message carries 256 bytes and a rectangle takes 16. Sending one
    /// message per command -- the first version -- meant 150 messages for
    /// one repaint of the explorer, against a compositor box of 64 and a
    /// compositor taking 128 per frame: every repaint waited for room over
    /// several frames, and two windows repainting together each waited for
    /// the other. Packed, the same repaint is a dozen messages.
    ///
    /// The batch leaves before any other message (`deliver`), so the order
    /// the application wrote is the order the compositor applies: a COMMIT
    /// never overtakes the commands it publishes.
    fn draw(&mut self, id: SurfaceId, op: u32, body: &[u8]) {
        let entry = 2 + body.len();
        if entry + 4 > ipc::MAX_PAYLOAD {
            // Too big to share a message -- a long text. Sent alone, in the
            // single-command form, with its identifier in front.
            let mut payload = Vec::with_capacity(4 + body.len());
            payload.extend_from_slice(&id.to_le_bytes());
            payload.extend_from_slice(body);
            self.deliver(op | proto::NO_REPLY, &payload);
            return;
        }
        if self.batch_surface != Some(id) || self.batch.len() + entry > ipc::MAX_PAYLOAD {
            self.flush();
        }
        if self.batch.is_empty() {
            self.batch.extend_from_slice(&id.to_le_bytes());
            self.batch_surface = Some(id);
        }
        self.batch.push(op as u8);
        self.batch.push(body.len() as u8);
        self.batch.extend_from_slice(body);
    }

    /// Sends the pending drawing commands, if any.
    pub fn flush(&mut self) {
        if self.batch.is_empty() {
            return;
        }
        let batch = core::mem::take(&mut self.batch);
        self.batch_surface = None;
        self.send(proto::OP_DRAW_BATCH | proto::NO_REPLY, &batch);
    }

    /// Drops a request, waiting for there to be room.
    ///
    /// ## Why back pressure is indispensable here
    ///
    /// A mailbox caps at sixty-four messages. The wallpaper emits
    /// thirty-six commands for a single repaint, the task bar some
    /// twenty: two modules redrawing at the same time saturate the queue
    /// before the compositor has had its turn.
    /// Dropping the message would be the worst choice: a command lost in
    /// the middle of a display list does not show as an error, it shows
    /// as a badly drawn window, and the cause is at the other end of the
    /// system. So the client yields and tries again -- the compositor,
    /// for its part, never blocks on a client, so the wait cannot become
    /// circular.
    ///
    ///
    /// The wait stays bounded: if the compositor is dead, insisting will
    /// not resurrect it.
    fn deliver(&mut self, op: u32, payload: &[u8]) -> bool {
        // Whatever was drawn before this message goes before it.
        self.flush();
        self.send(op, payload)
    }

    /// Sends one message, waiting for room. See `deliver`.
    fn send(&mut self, op: u32, payload: &[u8]) -> bool {
        for _ in 0..256 {
            let message = Message::new(
                crate::scheduler::current_pid(),
                MessageKind::Request,
                op,
                payload,
            );
            match ipc::send_unchecked(self.server, message) {
                Ok(()) => {
                    self.sent += 1;
                    return true;
                }
                Err(IpcError::QueueFull) => {
                    self.blocked += 1;
                    crate::scheduler::yield_now();
                }
                Err(_) => break,
            }
        }
        self.refused += 1;
        false
    }

    /// Next event, emptying the local queue first.
    pub fn next_event(&mut self) -> Event {
        loop {
            let message = match self.pending.pop_front() {
                Some(message) => message,
                None => ipc::recv(),
            };
            if let Some(event) = classify(&message) {
                return event;
            }
        }
    }

    /// Next event if there is one, without waiting.
    pub fn poll_event(&mut self) -> Option<Event> {
        loop {
            let message = match self.pending.pop_front() {
                Some(message) => message,
                None => ipc::try_recv()?,
            };
            if let Some(event) = classify(&message) {
                return Some(event);
            }
        }
    }

    // --- protocol shorthands ---

    pub fn screen_info(&mut self) -> Option<(u16, u16, u16, u16)> {
        let reply = self.request(proto::OP_SCREEN_INFO, &[])?;
        let mut reader = Reader::new(&reply);
        if reader.u8()? != proto::ERR_NONE {
            return None;
        }
        Some((reader.u16()?, reader.u16()?, reader.u16()?, reader.u16()?))
    }

    pub fn create_surface(
        &mut self,
        layer: Layer,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        title: &str,
    ) -> Option<SurfaceId> {
        let payload = Writer::new()
            .u16(x.max(0) as u16)
            .u16(y.max(0) as u16)
            .u16(width.max(0) as u16)
            .u16(height.max(0) as u16)
            .u8(layer as u8)
            .text(title)
            .build();
        let reply = self.request(proto::OP_SURFACE_CREATE, &payload)?;
        let mut reader = Reader::new(&reply);
        if reader.u8()? != proto::ERR_NONE {
            return None;
        }
        reader.u32()
    }

    pub fn destroy_surface(&mut self, id: SurfaceId) {
        self.tell(proto::OP_SURFACE_DESTROY, &Writer::new().u32(id).build());
    }

    pub fn resize_surface(&mut self, id: SurfaceId, width: i32, height: i32) {
        self.tell(
            proto::OP_SURFACE_RESIZE,
            &Writer::new()
                .u32(id)
                .u16(width.max(1) as u16)
                .u16(height.max(1) as u16)
                .build(),
        );
    }

    pub fn move_surface(&mut self, id: SurfaceId, x: i32, y: i32) {
        self.tell(
            proto::OP_SURFACE_MOVE,
            &Writer::new()
                .u32(id)
                .u16(x.max(0) as u16)
                .u16(y.max(0) as u16)
                .build(),
        );
    }

    /// Asks for the application menu to be opened or closed.
    ///
    /// A `tell`: the answer carries nothing the caller can act on, and the
    /// menu is another module's surface anyway.
    pub fn toggle_menu(&mut self) {
        self.tell(proto::OP_MENU_TOGGLE, &[]);
    }

    /// Declares the shape the pointer must take.
    ///
    /// A `tell` and not a `request`: nothing useful comes back, and waiting
    /// for an answer on a boundary crossing would add a round trip to a
    /// path the pointer crosses several times a second. The caller is
    /// expected to send this only when the shape **changes** -- resending
    /// the same shape on every movement would cost one message per
    /// millimetre of mouse for no effect.
    pub fn set_cursor(&mut self, shape: proto::CursorShape) {
        self.tell(
            proto::OP_CURSOR_SHAPE,
            &Writer::new().u8(shape.as_code()).build(),
        );
    }

    pub fn show_surface(&mut self, id: SurfaceId, visible: bool) {
        self.tell(
            proto::OP_SURFACE_SHOW,
            &Writer::new().u32(id).u8(u8::from(visible)).build(),
        );
    }

    pub fn reset(&mut self, id: SurfaceId) {
        self.tell(proto::OP_DRAW_RESET, &Writer::new().u32(id).build());
    }

    pub fn commit(&mut self, id: SurfaceId) {
        self.tell(proto::OP_COMMIT, &Writer::new().u32(id).build());
    }

    pub fn fill(&mut self, id: SurfaceId, color: u32) {
        self.draw(id, proto::OP_DRAW_FILL, &Writer::new().u32(color).build());
    }

    pub fn rect(&mut self, id: SurfaceId, x: i32, y: i32, w: i32, h: i32, color: u32) {
        self.draw(
            id,
            proto::OP_DRAW_RECT,
            &Writer::new()
                .i16(x as i16)
                .i16(y as i16)
                .u16(w.max(0) as u16)
                .u16(h.max(0) as u16)
                .u32(color)
                .build(),
        );
    }

    pub fn frame(&mut self, id: SurfaceId, x: i32, y: i32, w: i32, h: i32, color: u32, thickness: u8) {
        self.draw(
            id,
            proto::OP_DRAW_FRAME,
            &Writer::new()
                .i16(x as i16)
                .i16(y as i16)
                .u16(w.max(0) as u16)
                .u16(h.max(0) as u16)
                .u32(color)
                .u8(thickness)
                .build(),
        );
    }

    pub fn line(&mut self, id: SurfaceId, x0: i32, y0: i32, x1: i32, y1: i32, color: u32) {
        self.draw(
            id,
            proto::OP_DRAW_LINE,
            &Writer::new()
                .i16(x0 as i16)
                .i16(y0 as i16)
                .i16(x1 as i16)
                .i16(y1 as i16)
                .u32(color)
                .build(),
        );
    }

    pub fn text(
        &mut self,
        id: SurfaceId,
        x: i32,
        y: i32,
        text: &str,
        color: u32,
        background: Option<u32>,
    ) {
        self.draw(
            id,
            proto::OP_DRAW_TEXT,
            &Writer::new()
                .i16(x as i16)
                .i16(y as i16)
                .u32(color)
                .u8(u8::from(background.is_some()))
                .u32(background.unwrap_or(0))
                .text(text)
                .build(),
        );
    }

    pub fn icon(
        &mut self,
        id: SurfaceId,
        x: i32,
        y: i32,
        bits: &[u8; 32],
        color: u32,
        background: Option<u32>,
    ) {
        self.draw(
            id,
            proto::OP_DRAW_ICON,
            &Writer::new()
                .i16(x as i16)
                .i16(y as i16)
                .u32(color)
                .u8(u8::from(background.is_some()))
                .u32(background.unwrap_or(0))
                .raw(bits)
                .build(),
        );
    }

    /// Fetches the current theme. The modules do not read `theme.cfg`
    /// themselves: the compositor is the only authority, failing which
    /// two modules could display two different themes.
    pub fn theme(&mut self) -> Option<super::theme::Theme> {
        let reply = self.request(proto::OP_THEME_GET, &[])?;
        let mut reader = Reader::new(&reply);
        if reader.u8()? != proto::ERR_NONE {
            return None;
        }
        let mut theme = super::theme::Theme::dark();
        let mut colors = [0u32; 12];
        for slot in colors.iter_mut() {
            *slot = reader.u32()?;
        }
        theme.background = colors[0];
        theme.surface = colors[1];
        theme.surface_alt = colors[2];
        theme.accent = colors[3];
        theme.text = colors[4];
        theme.text_dim = colors[5];
        theme.border = colors[6];
        theme.title_active = colors[7];
        theme.title_inactive = colors[8];
        theme.danger = colors[9];
        theme.warning = colors[10];
        theme.success = colors[11];
        theme.taskbar_height = reader.u16()?;
        theme.title_height = reader.u16()?;
        theme.border_width = reader.u16()?;
        theme.corner_radius = reader.u16()?;
        theme.padding = reader.u16()?;
        Some(theme)
    }

    /// Emits a notification (9).
    pub fn notify(
        &mut self,
        level: super::notify::Level,
        title: &str,
        body: &str,
        dismissal: &super::notify::Dismissal,
        buttons: u8,
    ) -> Option<u32> {
        let payload =
            super::notify::encode_post(level, buttons, false, dismissal, title, body);
        let reply = self.request(proto::OP_NOTIFY_POST, &payload)?;
        let mut reader = Reader::new(&reply);
        if reader.u8()? != proto::ERR_NONE {
            return None;
        }
        reader.u32()
    }

    /// List of the windows, for the task bar.
    /// The windows with their titles and focus, in creation order. Asked
    /// page by page: a title makes an entry too long for all of them to
    /// fit in one message.
    pub fn window_entries(&mut self) -> Vec<WindowEntry> {
        let mut entries = Vec::new();
        loop {
            let Some(reply) =
                self.request(proto::OP_WINDOW_ENTRIES, &[entries.len().min(255) as u8])
            else {
                return entries;
            };
            let mut reader = Reader::new(&reply);
            if reader.u8() != Some(proto::ERR_NONE) {
                return entries;
            }
            let (Some(total), Some(count)) = (reader.u8(), reader.u8()) else {
                return entries;
            };
            for _ in 0..count {
                let (Some(id), Some(flags), Some(length)) = (reader.u16(), reader.u8(), reader.u8())
                else {
                    return entries;
                };
                let Some(title) = reader.bytes(length as usize) else {
                    return entries;
                };
                entries.push(WindowEntry {
                    id: id as SurfaceId,
                    visible: flags & proto::WINDOW_VISIBLE != 0,
                    focused: flags & proto::WINDOW_FOCUSED != 0,
                    title: title
                        .iter()
                        .map(|byte| if (0x20..=0x7e).contains(byte) { *byte as char } else { '?' })
                        .collect(),
                });
            }
            // An empty page would loop for ever on a window list that
            // shrank between two pages.
            if count == 0 || entries.len() >= total as usize {
                return entries;
            }
        }
    }

    /// Shows a pixel canvas in a surface (`gui::canvas`). Goes after the
    /// pending batch, so it lands between the `reset` and the `commit`
    /// that surround it.
    pub fn canvas(&mut self, id: SurfaceId, canvas: u32, x: i32, y: i32) {
        self.flush();
        self.tell(
            proto::OP_DRAW_CANVAS,
            &Writer::new().u32(id).u32(canvas).i16(x as i16).i16(y as i16).build(),
        );
    }

    /// Renames a window -- its title bar and its task bar button.
    pub fn set_title(&mut self, id: SurfaceId, title: &str) {
        self.tell(proto::OP_SURFACE_TITLE, &Writer::new().u32(id).text(title).build());
    }

    pub fn window_list(&mut self) -> Vec<(SurfaceId, bool)> {
        let Some(reply) = self.request(proto::OP_SURFACE_LIST, &[]) else {
            return Vec::new();
        };
        let mut reader = Reader::new(&reply);
        let mut windows = Vec::new();
        if reader.u8() != Some(proto::ERR_NONE) {
            return windows;
        }
        let Some(count) = reader.u8() else {
            return windows;
        };
        for _ in 0..count {
            let (Some(id), Some(_layer), Some(visible)) =
                (reader.u16(), reader.u8(), reader.u8())
            else {
                break;
            };
            windows.push((id as SurfaceId, visible != 0));
        }
        windows
    }
}

fn classify(message: &Message) -> Option<Event> {
    match message.tag {
        proto::EV_KEY => KeyEvent::decode(&message.payload).map(Event::Key),
        proto::EV_POINTER => PointerEvent::decode(&message.payload).map(Event::Pointer),
        proto::EV_FOCUS => {
            let mut reader = Reader::new(&message.payload);
            Some(Event::Focus {
                surface: reader.u32()?,
                gained: reader.u8()? != 0,
            })
        }
        proto::EV_CLOSE => {
            let mut reader = Reader::new(&message.payload);
            Some(Event::Close(reader.u32()?))
        }
        proto::EV_REDRAW => Some(Event::Redraw),
        proto::EV_MENU_TOGGLE => Some(Event::MenuToggle),
        proto::EV_WINDOWS_CHANGED => Some(Event::WindowsChanged),
        proto::EV_SCREEN_RESIZED => {
            let mut reader = Reader::new(&message.payload);
            Some(Event::ScreenResized {
                width: reader.u16()? as i32,
                height: reader.u16()? as i32,
            })
        }
        _ => Some(Event::Other(message.clone())),
    }
}

/// Text truncated to a given width, in cells.
///
/// Cuts and adds an ASCII ellipsis rather than letting it overflow: a
/// window title too long in a task bar would crush the next button.
pub fn ellipsize(text: &str, cells: usize) -> String {
    if text.len() <= cells {
        return String::from(text);
    }
    if cells <= 3 {
        return text.chars().take(cells).collect();
    }
    let mut out: String = text.chars().take(cells - 3).collect();
    out.push_str("...");
    out
}

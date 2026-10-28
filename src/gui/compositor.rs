//! 7.2 -- The compositor (spec 8.1).
//!
//! ## What it is here, and what it will be
//!
//! 8.1 makes it a `.ss` application. There is no SDK yet to compile such
//! an application -- that is step 9.1 -- and phase 6 already settled that
//! case for `init.dsa` and the five drivers: write the component as a
//! **kernel task carrying the same contract**. The protocol, the module
//! slots, the z-order and the permission rules are real and checkable;
//! only the delivery of the code changes, and it will change.
//!
//! What is already true of 8.1: **nothing but the compositor writes on
//! the framebuffer.** The modules and the applications emit IPC
//! instructions; it is `render` that paints, and it alone.
//!
//! ## Render loop (7.2.2)
//!
//! One iteration:
//!
//! 1. drains the mailbox -- client requests, input events, notification
//!    deadlines;
//! 2. if something changed, repaints the damaged area and publishes it;
//! 3. sleeps until the next tick.
//!
//! Point 2 carries the whole interest. Phase 7.1 left a limit open: every
//! console line republished the whole screen. The compositor, on the
//! other hand, accumulates a **damaged rectangle** and publishes only the
//! rows concerned. Moving a window a hundred pixels costs the area it
//! occupied plus the one it occupies, not the screen.
//!
//! ## Why the render is complete inside the damaged area
//!
//! A surface does not keep its pixels, it keeps its instructions. So one
//! cannot copy back "what was underneath": every surface touching the
//! area must be replayed, from the back. That is the price of the choice
//! of `surface.rs`, and it is paid in time proportional to the number of
//! commands in the area -- not to the size of the screen.

use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::header::BinaryFormat;
use crate::ipc::{self, Message, MessageKind};
use crate::process::Pid;

use super::notify::{Center, Level};
use super::painter::{Painter, Target};
use super::protocol::{self as proto, KeyEvent, PointerEvent, Reader, Writer};
use super::surface::{Command, Layer, Rect, Stack, SurfaceId};
use super::theme::Theme;

/// Channel of the compositor. Applications resolve it by that name.
pub const CHANNEL: &str = "sys.gui";

/// Identity of the core of the compositor (8.2, non-replaceable line).
pub const CORE_APP_ID: &str = "os.nova.compositor";

/// Complete state of the graphics system, under a single lock.
pub struct Compositor {
    pub stack: Stack,
    pub center: Center,
    pub theme: Theme,
    pub width: i32,
    pub height: i32,
    /// Area to repaint on the next frame.
    damage: Rect,
    /// Position of the pointer, in screen coordinates.
    pub pointer_x: i32,
    pub pointer_y: i32,
    pub pointer_buttons: u8,
    /// Is the Super key pressed?
    ///
    /// The keyboard driver only tracks shift, ctrl and alt: Super is not
    /// a modifier of scancode set 1, it is an ordinary extended key. Its
    /// state is therefore held here, and the compositor injects it into
    /// the events it passes on -- failing which `MOD_SUPER` stays
    /// eternally zero and no shortcut of 16.4 fires.
    super_held: bool,
    /// Surface grabbed by a drag of the title bar.
    dragging: Option<(SurfaceId, i32, i32)>,
    /// Shape the pointer must take, declared by a client.
    pub cursor_shape: proto::CursorShape,
    /// Events a request produced, to be sent once the lock is released.
    ///
    /// ## Why a queue rather than a send on the spot
    ///
    /// `handle` runs **under the compositor lock** and returns a reply, not
    /// a list of destinations. Sending from inside it would put an IPC send
    /// inside the critical section, which DECISIONS.md forbids for the same
    /// reason everywhere: a full mailbox would then block the render.
    ///
    /// So a request that must reach a third party leaves it here, and
    /// `drain_mailbox` flushes it outside the lock -- exactly where it
    /// already sends the reply.
    pub pending: Vec<Routed>,
    /// Surface the pointer was over at the previous event.
    ///
    /// Only used to reset the shape when it changes. Section 8.3 wants a
    /// module that pauses or dies not to break the OS: without this reset,
    /// a client that asked for a text cursor and then stopped answering
    /// would leave that cursor over the whole desktop, and nothing would
    /// give it back.
    last_hover: Option<SurfaceId>,
    pub frames: u64,
    pub requests: u64,
    pub refusals: u64,
    pub events_routed: u64,
    pub session_user: Option<String>,
    /// Is the open session behind the lock screen?
    ///
    /// Held here and not asked of `login::is_showing`: `handle` runs under
    /// this lock, and taking the login lock from inside it would set an
    /// order between the two that the login code, which reaches the
    /// compositor through `access`, does not follow.
    pub session_locked: bool,
    /// Session action accepted by `handle`, performed by the compositor
    /// task once the lock is released (entry 47 of DECISIONS.md).
    pub session_request: Option<proto::SessionAction>,
    /// Is the compositor painting? False while the splash holds the screen.
    pub presenting: bool,
    /// Does the device's cursor plane show the pointer (phase 13.4.4)?
    /// Then the overlay surface is not composed, and moving it dirties
    /// nothing: the plane moves on its own.
    pub cursor_plane: bool,
}

impl Compositor {
    pub fn new(width: i32, height: i32, theme: Theme) -> Self {
        Compositor {
            stack: Stack::new(),
            center: Center::new(),
            theme,
            width,
            height,
            damage: Rect::new(0, 0, width, height),
            pointer_x: width / 2,
            pointer_y: height / 2,
            pointer_buttons: 0,
            super_held: false,
            dragging: None,
            cursor_shape: proto::CursorShape::Arrow,
            last_hover: None,
            pending: Vec::new(),
            frames: 0,
            requests: 0,
            refusals: 0,
            events_routed: 0,
            session_user: None,
            session_locked: false,
            session_request: None,
            presenting: true,
            cursor_plane: false,
        }
    }

    pub fn screen(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    /// Marks an area to repaint.
    ///
    /// The union rather than a list of rectangles: two distant areas give
    /// a bounding rectangle wider than needed, but a list would demand
    /// merging the overlaps on every addition. For an interface where
    /// most of the movement is local, the single rectangle wins easily --
    /// and it cannot forget an area, which a badly merged list would.
    pub fn damage_rect(&mut self, rect: Rect) {
        let clipped = rect.intersect(&self.screen());
        if !clipped.is_empty() {
            self.damage = self.damage.union(&clipped);
        }
    }

    pub fn damage_all(&mut self) {
        self.damage = self.screen();
    }

    pub fn damage_surface(&mut self, id: SurfaceId) {
        let title = self.theme.title_height as i32;
        let border = self.theme.border_width as i32;
        if let Some(surface) = self.stack.get(id) {
            // On the cursor plane, the pointer is not in the frame.
            if self.cursor_plane && surface.layer == Layer::Overlay {
                return;
            }
            let rect = surface.outer_rect(title, border);
            self.damage_rect(rect);
        }
    }

    pub fn pending_damage(&self) -> Rect {
        self.damage
    }
}

// ============================================================
//  Handling of a request (7.2.2)
// ============================================================

/// Answer to send back, or nothing for the operations with no answer.
pub struct Reply {
    pub payload: Vec<u8>,
}

impl Reply {
    fn ok(payload: Vec<u8>) -> Option<Reply> {
        Some(Reply { payload })
    }

    fn refuse(code: u8) -> Option<Reply> {
        Some(Reply {
            payload: proto::refusal(code),
        })
    }
}

/// A fingerprint of the windows as a task bar shows them: which exist,
/// in which order, shown or not, which has the focus, their titles.
///
/// Compared once per frame by the compositor loop. Hooking every place
/// that creates, destroys, focuses or renames a window would be a dozen
/// call sites, each one a chance to forget; one pass over at most a few
/// dozen windows per frame costs less than one draw command.
pub fn window_signature(comp: &Compositor) -> u64 {
    // FNV-1a: the value only has to change when the windows do.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |byte: u8| {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    };
    let focus = comp.stack.focus();
    for surface in comp.stack.iter().filter(|surface| surface.layer == Layer::Window) {
        for byte in surface.id.to_le_bytes() {
            mix(byte);
        }
        mix(u8::from(surface.visible) | (u8::from(focus == Some(surface.id)) << 1));
        for byte in surface.title.bytes() {
            mix(byte);
        }
        mix(0);
    }
    hash
}

/// Queues `EV_WINDOWS_CHANGED` for every shell module.
pub fn announce_windows(comp: &mut Compositor) {
    for owner in comp.stack.owners_in(Layer::Shell) {
        comp.pending.push(Routed {
            to: owner,
            tag: proto::EV_WINDOWS_CHANGED,
            payload: Vec::new(),
        });
    }
}

/// Applies a request. Returns the answer to post, if there is one.
///
/// A free function rather than a method: it takes the compositor mutably
/// and returns an answer to send **after** the lock is released. Sending
/// under the lock would amount to holding a lock across a call that can
/// block, which `docs/DECISIONS.md` forbids.
pub fn handle(comp: &mut Compositor, sender: Pid, op: u32, payload: &[u8]) -> Option<Reply> {
    comp.requests += 1;
    let mut reader = Reader::new(payload);

    match op {
        proto::OP_SURFACE_CREATE => {
            let (Some(x), Some(y), Some(w), Some(h), Some(layer_code)) = (
                reader.u16(),
                reader.u16(),
                reader.u16(),
                reader.u16(),
                reader.u8(),
            ) else {
                comp.refusals += 1;
                return Reply::refuse(proto::ERR_BAD_REQUEST);
            };
            let Some(layer) = Layer::from_code(layer_code) else {
                comp.refusals += 1;
                return Reply::refuse(proto::ERR_BAD_REQUEST);
            };
            let title = reader.text();
            let rect = Rect::new(x as i32, y as i32, w as i32, h as i32);

            match comp.stack.create(sender, layer, rect, title) {
                Some(id) => {
                    comp.damage_surface(id);
                    Reply::ok(Writer::new().u8(proto::ERR_NONE).u32(id).build())
                }
                None => {
                    comp.refusals += 1;
                    Reply::refuse(proto::ERR_TOO_MANY)
                }
            }
        }

        proto::OP_SURFACE_DESTROY => with_surface(comp, sender, &mut reader, |comp, id| {
            comp.damage_surface(id);
            comp.stack.destroy(id);
            proto::ERR_NONE
        }),

        proto::OP_SURFACE_MOVE => {
            let (Some(id), Some(x), Some(y)) = (reader.u32(), reader.u16(), reader.u16()) else {
                comp.refusals += 1;
                return Reply::refuse(proto::ERR_BAD_REQUEST);
            };
            match check(comp, sender, id) {
                Err(code) => {
                    comp.refusals += 1;
                    Reply::refuse(code)
                }
                Ok(()) => {
                    // The old place must be repainted as much as the new
                    // one: without that, the window leaves a trail.
                    comp.damage_surface(id);
                    if let Some(surface) = comp.stack.get_mut(id) {
                        surface.rect.x = x as i32;
                        surface.rect.y = y as i32;
                    }
                    comp.damage_surface(id);
                    Reply::ok(proto::refusal(proto::ERR_NONE))
                }
            }
        }

        proto::OP_SURFACE_RESIZE => {
            let (Some(id), Some(w), Some(h)) = (reader.u32(), reader.u16(), reader.u16()) else {
                comp.refusals += 1;
                return Reply::refuse(proto::ERR_BAD_REQUEST);
            };
            match check(comp, sender, id) {
                Err(code) => {
                    comp.refusals += 1;
                    Reply::refuse(code)
                }
                Ok(()) => {
                    comp.damage_surface(id);
                    if let Some(surface) = comp.stack.get_mut(id) {
                        surface.rect.width = w as i32;
                        surface.rect.height = h as i32;
                    }
                    comp.damage_surface(id);
                    Reply::ok(proto::refusal(proto::ERR_NONE))
                }
            }
        }

        proto::OP_SURFACE_RAISE => with_surface(comp, sender, &mut reader, |comp, id| {
            comp.stack.raise(id);
            comp.damage_surface(id);
            proto::ERR_NONE
        }),

        proto::OP_SURFACE_LOWER => with_surface(comp, sender, &mut reader, |comp, id| {
            comp.stack.lower(id);
            comp.damage_all();
            proto::ERR_NONE
        }),

        proto::OP_SURFACE_SHOW => {
            let (Some(id), Some(visible)) = (reader.u32(), reader.u8()) else {
                comp.refusals += 1;
                return Reply::refuse(proto::ERR_BAD_REQUEST);
            };
            match check(comp, sender, id) {
                Err(code) => {
                    comp.refusals += 1;
                    Reply::refuse(code)
                }
                Ok(()) => {
                    comp.damage_surface(id);
                    if let Some(surface) = comp.stack.get_mut(id) {
                        surface.visible = visible != 0;
                    }
                    comp.damage_surface(id);
                    Reply::ok(proto::refusal(proto::ERR_NONE))
                }
            }
        }

        proto::OP_SURFACE_FOCUS => with_surface(comp, sender, &mut reader, |comp, id| {
            if comp.stack.set_focus(id) {
                comp.damage_all();
                proto::ERR_NONE
            } else {
                proto::ERR_DENIED
            }
        }),

        proto::OP_SURFACE_LIST => {
            // Four bytes per surface: truncated identifier, layer,
            // visible. Enough for a task bar, and it fits in a message
            // even at the surface cap.
            let mut writer = Writer::new().u8(proto::ERR_NONE);
            let mut count = 0u8;
            let ids: Vec<SurfaceId> = comp
                .stack
                .iter()
                .filter(|surface| surface.layer == Layer::Window)
                .map(|surface| surface.id)
                .collect();
            for id in ids.iter().take(60) {
                let Some(surface) = comp.stack.get(*id) else {
                    continue;
                };
                writer = writer
                    .u16(*id as u16)
                    .u8(surface.layer as u8)
                    .u8(u8::from(surface.visible));
                count += 1;
            }
            let mut payload = writer.build();
            payload.insert(1, count);
            Reply::ok(payload)
        }

        proto::OP_WINDOW_ENTRIES => {
            let first = reader.u8().unwrap_or(0) as usize;
            let focus = comp.stack.focus();
            let windows: Vec<(SurfaceId, u8, String)> = comp
                .stack
                .iter()
                .filter(|surface| surface.layer == Layer::Window)
                .map(|surface| {
                    let mut flags = 0;
                    if surface.visible {
                        flags |= proto::WINDOW_VISIBLE;
                    }
                    if focus == Some(surface.id) {
                        flags |= proto::WINDOW_FOCUSED;
                    }
                    let title: String =
                        surface.title.chars().take(proto::WINDOW_TITLE_MAX).collect();
                    (surface.id, flags, title)
                })
                .collect();
            let total = windows.len().min(255);
            let mut entries = Vec::new();
            let mut count = 0u8;
            for (id, flags, title) in windows.iter().skip(first).take(total.saturating_sub(first)) {
                // Error, total and count come first: three bytes.
                if 3 + entries.len() + 4 + title.len() > crate::ipc::MAX_PAYLOAD {
                    break;
                }
                entries.extend_from_slice(&(*id as u16).to_le_bytes());
                entries.push(*flags);
                entries.push(title.len() as u8);
                entries.extend_from_slice(title.as_bytes());
                count += 1;
            }
            Reply::ok(
                Writer::new()
                    .u8(proto::ERR_NONE)
                    .u8(total as u8)
                    .u8(count)
                    .raw(&entries)
                    .build(),
            )
        }

        proto::OP_SURFACE_TITLE => {
            let Some(id) = reader.u32() else {
                comp.refusals += 1;
                return Reply::refuse(proto::ERR_BAD_REQUEST);
            };
            if let Err(code) = check(comp, sender, id) {
                comp.refusals += 1;
                return Reply::refuse(code);
            }
            let title = reader.text();
            if let Some(surface) = comp.stack.get_mut(id)
                && surface.title != title
            {
                surface.title = title;
                comp.damage_surface(id);
            }
            Reply::ok(proto::refusal(proto::ERR_NONE))
        }

        proto::OP_DRAW_RESET => with_surface(comp, sender, &mut reader, |comp, id| {
            if let Some(surface) = comp.stack.get_mut(id) {
                surface.reset();
            }
            proto::ERR_NONE
        }),

        proto::OP_COMMIT => with_surface(comp, sender, &mut reader, |comp, id| {
            if let Some(surface) = comp.stack.get_mut(id) {
                surface.commit();
            }
            comp.damage_surface(id);
            proto::ERR_NONE
        }),

        proto::OP_DRAW_FILL
        | proto::OP_DRAW_RECT
        | proto::OP_DRAW_FRAME
        | proto::OP_DRAW_TEXT
        | proto::OP_DRAW_LINE
        | proto::OP_DRAW_ICON => {
            let Some(id) = reader.u32() else {
                comp.refusals += 1;
                return Reply::refuse(proto::ERR_BAD_REQUEST);
            };
            if let Err(code) = check(comp, sender, id) {
                comp.refusals += 1;
                return Reply::refuse(code);
            }
            let Some(command) = decode_command(op, &mut reader) else {
                comp.refusals += 1;
                return Reply::refuse(proto::ERR_BAD_REQUEST);
            };
            let accepted = comp
                .stack
                .get_mut(id)
                .map(|surface| surface.push(command))
                .unwrap_or(false);
            if accepted {
                Reply::ok(proto::refusal(proto::ERR_NONE))
            } else {
                comp.refusals += 1;
                Reply::refuse(proto::ERR_TOO_MANY)
            }
        }

        proto::OP_DRAW_CANVAS => {
            let Some(id) = reader.u32() else {
                comp.refusals += 1;
                return Reply::refuse(proto::ERR_BAD_REQUEST);
            };
            if let Err(code) = check(comp, sender, id) {
                comp.refusals += 1;
                return Reply::refuse(code);
            }
            let (Some(canvas_id), Some(x), Some(y)) = (reader.u32(), reader.i16(), reader.i16()) else {
                comp.refusals += 1;
                return Reply::refuse(proto::ERR_BAD_REQUEST);
            };
            // Only the creator's canvas: an identifier is a small number,
            // and guessing another client's would show its pixels.
            let Some(canvas) = super::canvas::lookup(canvas_id, sender) else {
                comp.refusals += 1;
                return Reply::refuse(proto::ERR_DENIED);
            };
            let accepted = comp
                .stack
                .get_mut(id)
                .map(|surface| {
                    surface.push(Command::Canvas {
                        x: x as i32,
                        y: y as i32,
                        canvas,
                    })
                })
                .unwrap_or(false);
            if accepted {
                Reply::ok(proto::refusal(proto::ERR_NONE))
            } else {
                comp.refusals += 1;
                Reply::refuse(proto::ERR_TOO_MANY)
            }
        }

        proto::OP_DRAW_BATCH => {
            let Some(id) = reader.u32() else {
                comp.refusals += 1;
                return Reply::refuse(proto::ERR_BAD_REQUEST);
            };
            if let Err(code) = check(comp, sender, id) {
                comp.refusals += 1;
                return Reply::refuse(code);
            }
            // The owner is checked once for the whole batch: every entry
            // names the same surface by construction.
            let mut cursor = 4usize;
            let mut code = proto::ERR_NONE;
            while cursor + 2 <= payload.len() {
                let op = payload[cursor] as u32;
                let length = payload[cursor + 1] as usize;
                let start = cursor + 2;
                let Some(body) = payload.get(start..start + length) else {
                    code = proto::ERR_BAD_REQUEST;
                    break;
                };
                cursor = start + length;
                // Only the drawing operations may ride in a batch: a
                // RESET or a COMMIT in the middle of one would change the
                // meaning of the entries after it.
                let drawing = matches!(
                    op,
                    proto::OP_DRAW_FILL
                        | proto::OP_DRAW_RECT
                        | proto::OP_DRAW_FRAME
                        | proto::OP_DRAW_TEXT
                        | proto::OP_DRAW_LINE
                        | proto::OP_DRAW_ICON
                );
                let command = if drawing {
                    decode_command(op, &mut Reader::new(body))
                } else {
                    None
                };
                let Some(command) = command else {
                    code = proto::ERR_BAD_REQUEST;
                    break;
                };
                let accepted = comp
                    .stack
                    .get_mut(id)
                    .map(|surface| surface.push(command))
                    .unwrap_or(false);
                if !accepted {
                    code = proto::ERR_TOO_MANY;
                    break;
                }
            }
            if code != proto::ERR_NONE {
                comp.refusals += 1;
            }
            Reply::ok(proto::refusal(code))
        }

        proto::OP_SCREEN_INFO => Reply::ok(
            Writer::new()
                .u8(proto::ERR_NONE)
                .u16(comp.width as u16)
                .u16(comp.height as u16)
                .u16(comp.theme.taskbar_height)
                .u16(comp.theme.title_height)
                .u16(comp.stack.len() as u16)
                .build(),
        ),

        proto::OP_THEME_GET => {
            let mut writer = Writer::new().u8(proto::ERR_NONE);
            for (_, color) in comp.theme.colors() {
                writer = writer.u32(color);
            }
            Reply::ok(
                writer
                    .u16(comp.theme.taskbar_height)
                    .u16(comp.theme.title_height)
                    .u16(comp.theme.border_width)
                    .u16(comp.theme.corner_radius)
                    .u16(comp.theme.padding)
                    .build(),
            )
        }

        proto::OP_MENU_TOGGLE => {
            // Broadcast to the shell modules, and not to the sender alone:
            // the button that toggles the menu and the menu itself are two
            // separate modules of 8.2, and the compositor is the broker
            // precisely so that neither has to know the other's PID.
            //
            // This one really is pushed, unlike a cursor shape. A shape
            // rides the next pointer event, which is guaranteed to come
            // since a shape only ever changes because the pointer moved. A
            // menu toggle has no such event behind it: the hand that
            // clicked may then hold perfectly still, and the menu would
            // stay shut until the mouse happened to move.
            let shells = comp.stack.owners_in(Layer::Shell);
            for owner in shells {
                comp.pending.push(Routed {
                    to: owner,
                    tag: proto::EV_MENU_TOGGLE,
                    payload: Vec::new(),
                });
            }
            Reply::ok(Writer::new().u8(proto::ERR_NONE).build())
        }

        proto::OP_SESSION_ACTION => {
            let Some(action) = reader.u8().and_then(proto::SessionAction::from_code) else {
                comp.refusals += 1;
                return Reply::refuse(proto::ERR_BAD_REQUEST);
            };
            // Shell modules only. Owning a surface of `Layer::Shell` is the
            // one thing that sets them apart in the protocol, and the layer
            // of a surface is chosen at creation -- so this is as strong as
            // the check on `OP_SURFACE_CREATE`, and no stronger. Named in
            // ROADMAP.md as a limit: a real capability would be carried by
            // the module header, not by the layer it drew in.
            //
            // A session must be open and not locked: the lock screen is
            // modal and nothing should reach a module behind it, but a
            // request already in flight when the lock went down would
            // otherwise log out or power off from behind the panel.
            // A second request while the first is pending is refused
            // rather than queued: two clicks must not become two actions.
            let shell = comp.stack.owners_in(Layer::Shell).contains(&sender);
            if !shell
                || comp.session_user.is_none()
                || comp.session_locked
                || comp.session_request.is_some()
            {
                comp.refusals += 1;
                return Reply::refuse(proto::ERR_DENIED);
            }
            comp.session_request = Some(action);
            Reply::ok(Writer::new().u8(proto::ERR_NONE).build())
        }

        proto::OP_CURSOR_SHAPE => {
            let Some(code) = reader.u8() else {
                comp.refusals += 1;
                return Reply::refuse(proto::ERR_BAD_REQUEST);
            };
            let wanted = proto::CursorShape::from_code(code);
            if wanted != comp.cursor_shape {
                comp.cursor_shape = wanted;
                // Pushed rather than left to ride the next `EV_POINTER`.
                //
                // Riding the next event was the first design, and it was
                // wrong for a reason a continuous mouse hides: the shape
                // would be seen one movement late. With a pointer that
                // moves in steps -- an absolute tablet, a jump, a single
                // report that lands on a button and stops -- there is no
                // next movement, and the cursor keeps the arrow over a
                // button until the hand happens to twitch.
                //
                // The cost is one message per **boundary crossed**, which
                // is a handful a second. What had to be avoided was one
                // message per *movement*, and that is still avoided: a
                // client only sends `OP_CURSOR_SHAPE` when the shape
                // changes.
                if let Some(overlay) = comp.stack.overlay_owner() {
                    let synthetic = PointerEvent {
                        buttons: comp.pointer_buttons,
                        x: comp.pointer_x as u16,
                        y: comp.pointer_y as u16,
                        dx: 0,
                        dy: 0,
                        shape: code,
                        wheel: 0,
                    };
                    comp.pending.push(Routed {
                        to: overlay,
                        tag: proto::EV_POINTER,
                        payload: synthetic.encode(),
                    });
                }
            }
            Reply::ok(Writer::new().u8(proto::ERR_NONE).build())
        }

        proto::OP_THEME_RELOAD => {
            let user = comp.session_user.clone();
            let (theme, origin) = super::theme::load_for(user.as_deref());
            comp.theme = theme;
            comp.damage_all();
            Reply::ok(
                Writer::new()
                    .u8(proto::ERR_NONE)
                    .u16(origin.system_keys as u16)
                    .u16(origin.user_keys as u16)
                    .u8(u8::from(origin.user_present))
                    .build(),
            )
        }

        proto::OP_NOTIFY_POST => {
            let Some((level, buttons, sound, dismissal, title, body)) =
                super::notify::decode_post(payload)
            else {
                comp.refusals += 1;
                return Reply::refuse(proto::ERR_BAD_REQUEST);
            };
            let (app_id, format) = identity_of(sender);
            let now = crate::idt::ticks();
            match comp.center.post(
                sender, &app_id, format, level, &title, &body, dismissal, buttons, sound, now,
            ) {
                Ok(id) => {
                    comp.damage_all();
                    Reply::ok(Writer::new().u8(proto::ERR_NONE).u32(id).build())
                }
                Err(_) => {
                    comp.refusals += 1;
                    Reply::refuse(proto::ERR_DENIED)
                }
            }
        }

        proto::OP_NOTIFY_DISMISS => {
            let Some(id) = reader.u32() else {
                comp.refusals += 1;
                return Reply::refuse(proto::ERR_BAD_REQUEST);
            };
            let removed = comp.center.dismiss(id);
            if removed {
                comp.damage_all();
            }
            Reply::ok(proto::refusal(if removed {
                proto::ERR_NONE
            } else {
                proto::ERR_DENIED
            }))
        }

        proto::OP_NOTIFY_ACTION => {
            let (Some(id), Some(button)) = (reader.u32(), reader.u8()) else {
                comp.refusals += 1;
                return Reply::refuse(proto::ERR_BAD_REQUEST);
            };
            comp.center.act(id, button);
            comp.damage_all();
            Reply::ok(proto::refusal(proto::ERR_NONE))
        }

        proto::OP_NOTIFY_COUNT => {
            let counts = comp.center.counts();
            let mut writer = Writer::new().u8(proto::ERR_NONE).u8(comp.center.len() as u8);
            for count in counts {
                writer = writer.u8(count.min(255) as u8);
            }
            Reply::ok(writer.build())
        }

        proto::OP_MODULE_LIST => {
            let modules = super::registry_snapshot();
            let mut writer = Writer::new()
                .u8(proto::ERR_NONE)
                .u8(modules.len().min(255) as u8);
            for (slot, active) in modules.iter().take(32) {
                writer = writer.u8(*slot).u8(u8::from(*active));
            }
            Reply::ok(writer.build())
        }

        proto::OP_MODULE_RESCAN => {
            let count = super::rescan_modules(comp.session_user.as_deref());
            comp.damage_all();
            Reply::ok(Writer::new().u8(proto::ERR_NONE).u16(count as u16).build())
        }

        _ => {
            comp.refusals += 1;
            Reply::refuse(proto::ERR_BAD_REQUEST)
        }
    }
}

/// Does the surface exist, and does it belong to the sender?
///
/// The owner check is not decorative: without it, any application would
/// move the window of another, or erase its content. It is the graphical
/// equivalent of the rule of 12.3 that forbids inter-process memory
/// access.
fn check(comp: &Compositor, sender: Pid, id: SurfaceId) -> Result<(), u8> {
    match comp.stack.get(id) {
        None => Err(proto::ERR_NO_SURFACE),
        Some(surface) if surface.owner != sender => Err(proto::ERR_NOT_OWNER),
        Some(_) => Ok(()),
    }
}

fn with_surface(
    comp: &mut Compositor,
    sender: Pid,
    reader: &mut Reader,
    action: impl FnOnce(&mut Compositor, SurfaceId) -> u8,
) -> Option<Reply> {
    let Some(id) = reader.u32() else {
        comp.refusals += 1;
        return Reply::refuse(proto::ERR_BAD_REQUEST);
    };
    if let Err(code) = check(comp, sender, id) {
        comp.refusals += 1;
        return Reply::refuse(code);
    }
    let code = action(comp, id);
    if code != proto::ERR_NONE {
        comp.refusals += 1;
    }
    Reply::ok(proto::refusal(code))
}

fn decode_command(op: u32, reader: &mut Reader) -> Option<Command> {
    match op {
        proto::OP_DRAW_FILL => Some(Command::Fill {
            color: reader.u32()?,
        }),
        proto::OP_DRAW_RECT => {
            let rect = read_rect(reader)?;
            Some(Command::Rect {
                rect,
                color: reader.u32()?,
            })
        }
        proto::OP_DRAW_FRAME => {
            let rect = read_rect(reader)?;
            let color = reader.u32()?;
            Some(Command::Frame {
                rect,
                color,
                thickness: reader.u8()? as i32,
            })
        }
        proto::OP_DRAW_LINE => Some(Command::Line {
            x0: reader.i16()? as i32,
            y0: reader.i16()? as i32,
            x1: reader.i16()? as i32,
            y1: reader.i16()? as i32,
            color: reader.u32()?,
        }),
        proto::OP_DRAW_TEXT => {
            let x = reader.i16()? as i32;
            let y = reader.i16()? as i32;
            let color = reader.u32()?;
            let opaque = reader.u8()?;
            let background = reader.u32()?;
            Some(Command::Text {
                x,
                y,
                color,
                background: if opaque != 0 { Some(background) } else { None },
                text: reader.text(),
            })
        }
        proto::OP_DRAW_ICON => {
            let x = reader.i16()? as i32;
            let y = reader.i16()? as i32;
            let color = reader.u32()?;
            let opaque = reader.u8()?;
            let background = reader.u32()?;
            let mut bits = [0u8; 32];
            bits.copy_from_slice(reader.bytes(32)?);
            Some(Command::Icon {
                x,
                y,
                bits,
                color,
                background: if opaque != 0 { Some(background) } else { None },
            })
        }
        _ => None,
    }
}

fn read_rect(reader: &mut Reader) -> Option<Rect> {
    Some(Rect::new(
        reader.i16()? as i32,
        reader.i16()? as i32,
        reader.u16()? as i32,
        reader.u16()? as i32,
    ))
}

/// Identity of the sender, for the 9.3 table.
///
/// A kernel task has no application identity: it is treated as a `.dsa`,
/// which is what it functionally is -- the compositor and its modules are
/// system components. The distinction will become real when the modules
/// run in ring 3.
fn identity_of(pid: Pid) -> (String, BinaryFormat) {
    match crate::apps::identity_of(pid) {
        Some(identity) => (identity.app_id.clone(), identity.format),
        None => (String::from("os.nova.kernel"), BinaryFormat::Dsa),
    }
}

// ============================================================
//  7.2.2 -- Render
// ============================================================

/// Takes the damaged area for a frame, and counts the frame. `None` when
/// there is nothing to paint.
///
/// 16.2 -- Under magnification a rectangle of the back buffer lands on a
/// *different* rectangle of the screen, and the mapping moves with the
/// pointer. Publishing a partial area would leave the rest showing the
/// previous magnification, torn along the boundary. So the whole screen is
/// republished while the magnifier is on -- a real cost, paid only by
/// somebody who asked for it.
pub fn take_damage(comp: &mut Compositor) -> Option<Rect> {
    if crate::access::wants_full_frame() {
        comp.damage_all();
    }
    let damage = comp.damage;
    if damage.is_empty() || !comp.presenting {
        return None;
    }
    comp.damage = Rect::EMPTY;
    comp.frames += 1;
    Some(damage)
}

/// Paints the damaged area on a target: the background, then every
/// surface touching it, from the back. The processor's framebuffer and the
/// GPU's recorder (`gui::gpu`) go through here alike.
pub fn paint<T: Target + ?Sized>(comp: &Compositor, target: &mut T, damage: Rect) {
    let order = comp.stack.painting_order();
    let title_height = comp.theme.title_height as i32;
    let border = comp.theme.border_width as i32;
    let theme = comp.theme;
    let mut screen = Painter::screen(target);

    // **All** the painting of the frame happens inside this area.
    //
    // It is the point that makes the damaged-rectangle render correct,
    // and not merely thrifty. The first version replayed each surface
    // with the clip of the whole screen: the wallpaper, replayed because
    // it touches the area, then repainted **the entire back buffer**,
    // including over the cursor -- which was ignored, for want of
    // intersecting the area. The back buffer thus went out of sync with
    // the front buffer, and the cursor only reappeared in pieces, as the
    // areas were published.
    //
    // The clip of `Painter` composes: a surface therefore cannot paint
    // beyond what will be published.
    let mut zone = screen.region(damage);
    // The background covers the area: without it, what a moved window
    // leaves behind stays on screen.
    zone.fill(zone.bounds(), theme.background);

    for id in order.iter() {
        let Some(surface) = comp.stack.get(*id) else {
            continue;
        };
        if comp.cursor_plane && surface.layer == Layer::Overlay {
            continue;
        }
        let outer = surface.outer_rect(title_height, border);
        if outer.intersect(&damage).is_empty() {
            continue;
        }

        if surface.decorated {
            let focused = comp.stack.focus() == Some(surface.id);
            paint_decoration(&mut zone, surface, &theme, focused, damage);
        }

        // Coordinates relative to the area: it is the area that carries
        // the origin and the clip.
        let local = surface.rect.offset(-damage.x, -damage.y);
        let mut region = zone.region(local);
        region.replay(surface.display_list());
    }
}

/// Repaints the damaged area on the processor and publishes it. Returns
/// the height published.
///
/// The render never takes the compositor lock: it receives the state
/// already borrowed. That is what guarantees no `present` -- hence no copy
/// of several mebibytes -- happens with the screen lock taken as well.
pub fn render(comp: &mut Compositor) -> usize {
    let Some(damage) = take_damage(comp) else {
        return 0;
    };
    let comp = &*comp;
    let published = crate::graphics::with_surface(|fb| {
        paint(comp, fb, damage);

        // The area is published in one go, at the end: publishing per
        // surface would make the windows appear one by one on screen,
        // which is exactly the defect the double buffer exists to avoid.
        let (x, y, width, height) = (
            damage.x.max(0) as usize,
            damage.y.max(0) as usize,
            damage.width as usize,
            damage.height as usize,
        );

        // 16.1 -- The accessibility transforms act **here**, between the
        // back buffer and the screen. Not on the drawing commands: an
        // application draws what it draws, and a transform that changed
        // the commands would be visible to it. 8.1 holds -- the
        // compositor is still the only writer of the framebuffer, and
        // this is a stage of its publication.
        //
        // The plain path is kept for the common case. Somebody who has
        // not switched anything on must not pay a per-pixel closure call
        // for a feature they do not use.
        if crate::access::transforms_pixels() {
            let zoom_on = crate::access::is_on(crate::access::Feature::Zoom);
            let settings = crate::access::zoom_settings();
            let centre = crate::access::zoom_centre();
            let mode = crate::access::contrast_mode();
            let matrix = crate::access::custom_matrix();
            let (screen_width, screen_height) = (comp.width, comp.height);

            fb.present_area_mapped(x, y, width, height, &|sample, px, py| {
                crate::access::map_pixel(
                    sample,
                    screen_width,
                    screen_height,
                    px,
                    py,
                    zoom_on,
                    &settings,
                    centre,
                    mode,
                    &matrix,
                )
            });
        } else {
            fb.present_area(x, y, width, height);
        }
        damage.height as usize
    });

    published.unwrap_or(0)
}

/// Title bar and border of a window (7.2.3, 8.4 for the style).
///
/// Painted inside the damaged area, like everything else: `outer` is
/// therefore brought back to coordinates relative to that area.
/// Rectangle of the close cross, in coordinates local to `outer`.
///
/// ## Why this is a function and not two literals
///
/// The cross was drawn here and tested nowhere: `route_pointer` looked only
/// for a drag of the title bar, so **the close button did nothing at all**.
/// It had been painted since phase 7 and `EV_CLOSE` was never emitted by
/// anybody -- the event existed, the IDE of phase 9 handled it, and no line of
/// the system ever sent it. A drawing that promises an action the code does
/// not perform is entry 34 of DECISIONS.md, and this is its third form.
///
/// Now that two places need the rectangle, it is computed once. The hit
/// area is **grown by three pixels on each side**: a nine-pixel cross is
/// below what a hand aims at reliably, and a close button one misses is
/// worse than one that is slightly too eager -- the miss lands on the title
/// bar and starts a drag, so the window jumps instead of closing.
pub fn close_button(outer: Rect, theme: &Theme) -> Rect {
    let padding = theme.padding as i32;
    let baseline =
        ((theme.title_height as i32 - crate::graphics::font::GLYPH_HEIGHT as i32) / 2).max(0);
    Rect::new(outer.width - padding - 10, baseline + 3, 9, 9)
}

/// The same rectangle, enlarged to what a pointer can actually hit.
pub fn close_target(outer: Rect, theme: &Theme) -> Rect {
    let cross = close_button(outer, theme);
    Rect::new(cross.x - 3, cross.y - 3, cross.width + 6, cross.height + 6)
}

fn paint_decoration<T: Target + ?Sized>(
    zone: &mut Painter<'_, T>,
    surface: &super::surface::Surface,
    theme: &Theme,
    focused: bool,
    damage: Rect,
) {
    let title_height = theme.title_height as i32;
    let border = theme.border_width as i32;
    let outer = surface
        .outer_rect(title_height, border)
        .offset(-damage.x, -damage.y);

    let bar = if focused {
        theme.title_active
    } else {
        theme.title_inactive
    };

    let mut zone = zone.region(outer);
    zone.rounded(zone.bounds(), bar, theme.corner_radius as i32);
    zone.frame(zone.bounds(), theme.border, border.max(1));

    let label_color = if focused { theme.text } else { theme.text_dim };
    let padding = theme.padding as i32;
    // The title is centred vertically in its bar: the font is sixteen
    // pixels high, the bar is whatever the theme says.
    let baseline = (title_height - crate::graphics::font::GLYPH_HEIGHT as i32) / 2;
    zone.text(padding, baseline.max(0), &surface.title, label_color, None);

    // Close button: a cross on the right of the bar.
    let close = close_button(outer, theme);
    zone.line(
        close.x,
        close.y,
        close.right() - 1,
        close.bottom() - 1,
        theme.danger,
    );
    zone.line(
        close.right() - 1,
        close.y,
        close.x,
        close.bottom() - 1,
        theme.danger,
    );
}

// ============================================================
//  7.4.2 -- Routing of the input events
// ============================================================

/// What the compositor wants to pass on after an event.
pub struct Routed {
    pub to: Pid,
    pub tag: u32,
    pub payload: Vec<u8>,
}

/// Scancodes of the Super keys, left and right (set 1, extended).
pub const SCAN_SUPER_LEFT: u8 = 0x5B;
pub const SCAN_SUPER_RIGHT: u8 = 0x5C;

/// Scancodes of the global shortcuts of 16.4.
const SCAN_TAB: u8 = 0x0F;
const SCAN_F4: u8 = 0x3E;
const SCAN_F6: u8 = 0x40;
/// `s z c k` on AZERTY -- the four accessibility toggles of 16.4.
const SCAN_ACCESSIBILITY: [u8; 4] = [0x1F, 0x11, 0x2E, 0x25];

/// Does this shortcut belong to the system rather than to the application?
///
/// 16.4 declares these combinations **global**: they must work whatever
/// the window in the foreground, otherwise a full-screen application
/// would make accessibility unreachable. Passing them to the focused
/// application would therefore be exactly the opposite of what is wanted.
pub fn is_global_shortcut(event: &KeyEvent) -> bool {
    let alt = event.has(KeyEvent::MOD_ALT);
    let sup = event.has(KeyEvent::MOD_SUPER);

    match event.scancode {
        // Super alone opens the launcher.
        SCAN_SUPER_LEFT | SCAN_SUPER_RIGHT => true,
        // Super+Tab: move from one window to the next.
        SCAN_TAB => sup,
        // Alt+F4: close the active window.
        SCAN_F4 => alt,
        // F6: change panel.
        SCAN_F6 => true,
        // Alt+Super+S / Z / C / K: the four accessibility toggles.
        code => alt && sup && SCAN_ACCESSIBILITY.contains(&code),
    }
}

/// Routes a key.
///
/// ## Two destinations, not one
///
/// A global shortcut goes **to the shell modules**, an ordinary keystroke
/// to the focused surface. The first version had only one path, towards
/// the focus: `launcher.mod` and `winmanager.mod` therefore never
/// received their shortcuts, since a single surface has the focus at a
/// time and it was practically never theirs. The comment promised an
/// interception nothing performed.
pub fn route_key(comp: &mut Compositor, event: KeyEvent) -> Vec<Routed> {
    comp.events_routed += 1;

    // The state of Super is held here: the driver does not track it.
    if matches!(event.scancode, SCAN_SUPER_LEFT | SCAN_SUPER_RIGHT) {
        comp.super_held = event.pressed;
    }

    let mut event = event;
    if comp.super_held {
        event.modifiers |= KeyEvent::MOD_SUPER;
    }

    // 16.4 -- The accessibility subsystem sees the key first.
    //
    // First, and not "as one more subscriber": the four toggles must fire
    // even when no module is running and no window has the focus, which
    // is the state the machine is in at the login screen. Routing them to
    // the shell layer and hoping somebody is listening would make 16.4's
    // last sentence depend on what happens to be loaded.
    //
    // `handle_key` returns true when it consumed the key, and a consumed
    // key is not passed on: a Tab that moved the accessibility focus must
    // not also move the focus of the application underneath.
    if crate::access::handle_key(&event) {
        return Vec::new();
    }

    let payload = event.encode();
    let targets = if is_global_shortcut(&event) {
        comp.stack.owners_in(Layer::Shell)
    } else {
        comp.stack.focus_owner().into_iter().collect()
    };

    targets
        .into_iter()
        .map(|to| Routed {
            to,
            tag: proto::EV_KEY,
            payload: payload.clone(),
        })
        .collect()
}

/// Updates the pointer and routes the event.
///
/// The click **changes the focus** before being passed on: it is what one
/// expects of a click on a background window, and doing it here rather
/// than in the window manager guarantees the rule holds even if that
/// module is paused.
pub fn route_pointer(comp: &mut Compositor, event: PointerEvent) -> Vec<Routed> {
    comp.events_routed += 1;
    let previous = (comp.pointer_x, comp.pointer_y);
    comp.pointer_x = (event.x as i32).clamp(0, comp.width - 1);
    comp.pointer_y = (event.y as i32).clamp(0, comp.height - 1);

    let pressed_now = event.pressed(PointerEvent::BUTTON_LEFT);
    let was_pressed = comp.pointer_buttons & PointerEvent::BUTTON_LEFT != 0;
    comp.pointer_buttons = event.buttons;

    // 16.2 -- The magnifier follows the pointer. Stored unconditionally,
    // even when the zoom is off: switching it on must magnify where the
    // pointer *is*, not where it was when the feature was last used.
    crate::access::set_zoom_centre(comp.pointer_x, comp.pointer_y);

    if previous != (comp.pointer_x, comp.pointer_y) && !comp.cursor_plane {
        // The cursor is a surface like any other: moving the pointer
        // dirties its old and its new position.
        //
        // The size is read from the overlay surface, not guessed. It was
        // written "14 by 20" whereas `cursor.mod` creates a 16 by 16
        // surface: the three right-hand columns were never dirtied. The
        // defect did not show because the current arrow is only nine
        // pixels wide -- it would have appeared with the first wider
        // cursor, as a vertical band that does not clear, and nothing
        // would have pointed at this line.
        // The **position** is read from the overlay surface too, and not
        // deduced from the pointer.
        //
        // It used to be `previous`, which was right only as long as the
        // cursor sat exactly on the pointer. A shape has a hot spot -- the
        // beam of a text cursor is at its middle, not at its corner -- so
        // `cursor.mod` offsets its surface, and a damage rectangle computed
        // from the pointer would miss the rows above by that offset. That
        // is entry 35 again, and its symptom is a cursor that leaves a
        // trail.
        //
        // Reading the real rectangle is also more robust when the module
        // lags: it dirties where the cursor **is**, not where it ought to
        // be.
        let rect = comp
            .stack
            .overlay_rect()
            .unwrap_or(Rect::new(previous.0, previous.1, 16, 16));
        // A one-pixel margin on each side: the cursor is drawn from its
        // corner, and the rounding of the clip must not leave a column
        // behind it.
        let cursor = Rect::new(rect.x - 1, rect.y - 1, rect.width + 2, rect.height + 2);
        comp.damage_rect(cursor);
        comp.damage_rect(cursor.offset(comp.pointer_x - previous.0, comp.pointer_y - previous.1));
    }

    let title_height = comp.theme.title_height as i32;
    let border = comp.theme.border_width as i32;
    let hit = comp
        .stack
        .hit(comp.pointer_x, comp.pointer_y, title_height, border);

    // Leaving a surface gives the arrow back. The surface being entered
    // declares its own shape if it wants one; a surface that declares
    // nothing gets the default, which is what a surface that knows nothing
    // of cursors should get.
    if hit != comp.last_hover {
        comp.last_hover = hit;
        comp.cursor_shape = proto::CursorShape::Arrow;
    }

    // --- drag of a title bar ---
    if pressed_now && !was_pressed {
        if let Some(id) = hit {
            comp.stack.set_focus(id);
            comp.stack.raise(id);
            comp.damage_all();

            if let Some(surface) = comp.stack.get(id)
                && surface.decorated
                && comp.pointer_y < surface.rect.y
            {
                // The close cross is tested **before** the drag, and that
                // order is the whole behaviour: the cross sits inside the
                // title bar, so testing the drag first would grab the
                // window every time and the button would never fire.
                let outer = surface.outer_rect(title_height, border);
                let target = close_target(outer, &comp.theme);
                let local = (comp.pointer_x - outer.x, comp.pointer_y - outer.y);
                if target.contains(local.0, local.1) {
                    // Asked of the owner, never done to it. The compositor
                    // does not destroy a surface it does not own -- an
                    // application with unsaved work must be able to refuse,
                    // or at least to ask first. `winmanager.mod` takes the
                    // same care with Alt+F4.
                    comp.pending.push(Routed {
                        to: surface.owner,
                        tag: proto::EV_CLOSE,
                        payload: Writer::new().u32(id).build(),
                    });
                } else {
                    comp.dragging = Some((
                        id,
                        comp.pointer_x - surface.rect.x,
                        comp.pointer_y - surface.rect.y,
                    ));
                }
            }
        }
    } else if !pressed_now {
        comp.dragging = None;
    } else if let Some((id, offset_x, offset_y)) = comp.dragging {
        comp.damage_surface(id);
        let (x, y) = (comp.pointer_x - offset_x, comp.pointer_y - offset_y);
        if let Some(surface) = comp.stack.get_mut(id) {
            surface.rect.x = x;
            surface.rect.y = y;
        }
        comp.damage_surface(id);
    }

    // Two destinations. The overlay surface first: it is what draws the
    // cursor, and `Stack::hit` ignores it by construction -- without this
    // separate path, the mouse moves in the state of the compositor and
    // stays still on screen.
    let mut targets: Vec<Pid> = Vec::new();
    if let Some(overlay) = comp.stack.overlay_owner() {
        targets.push(overlay);
    }
    if let Some(owner) = hit.and_then(|id| comp.stack.get(id)).map(|s| s.owner)
        && !targets.contains(&owner)
    {
        targets.push(owner);
    }

    // The shape is stamped on the event on the way out: the driver leaves
    // the field at zero, only the compositor knows what a client asked
    // for.
    let mut outgoing = event;
    outgoing.shape = comp.cursor_shape.as_code();
    let payload = outgoing.encode();
    targets
        .into_iter()
        .map(|to| Routed {
            to,
            tag: proto::EV_POINTER,
            payload: payload.clone(),
        })
        .collect()
}

/// Posts a message without ever blocking.
///
/// A client that no longer reads its box must not freeze the compositor:
/// the send failure is counted, not waited for. It is the opposite rule
/// to that of a file service, and it comes from the fact that a blocked
/// compositor blocks the whole screen.
/// Where a window goes when the screen changes size.
///
/// Its size is kept -- the application laid its content out for it, and
/// is not told to relayout. Only its place moves, and only as much as it
/// takes for the title bar to stay on screen: a window whose title bar
/// left the screen could never be dragged back.
pub fn clamp_window(rect: Rect, width: i32, height: i32, title: i32) -> Rect {
    let x = rect.x.min(width - rect.width).max(0);
    let y = rect.y.min(height - rect.height).max(title);
    Rect::new(x, y, rect.width, rect.height)
}

/// Takes a new screen size. Returns the events to post, outside the lock.
///
/// Called by the compositor task once the framebuffer already has the new
/// size. Everything placed from the old one is redone: the pointer, the
/// windows, and -- through `EV_SCREEN_RESIZED` -- whatever each client
/// placed itself.
pub fn resize(comp: &mut Compositor, width: i32, height: i32) -> Vec<Routed> {
    comp.width = width;
    comp.height = height;
    comp.pointer_x = comp.pointer_x.clamp(0, width - 1);
    comp.pointer_y = comp.pointer_y.clamp(0, height - 1);
    comp.dragging = None;

    let title = comp.theme.title_height as i32;
    let windows: Vec<SurfaceId> = comp
        .stack
        .iter()
        .filter(|surface| surface.layer == Layer::Window)
        .map(|surface| surface.id)
        .collect();
    for id in windows {
        if let Some(surface) = comp.stack.get_mut(id) {
            surface.rect = clamp_window(surface.rect, width, height, title);
        }
    }
    comp.damage_all();

    let payload = Writer::new().u16(width as u16).u16(height as u16).build();
    let mut owners: Vec<Pid> = Vec::new();
    for surface in comp.stack.iter() {
        if !owners.contains(&surface.owner) {
            owners.push(surface.owner);
        }
    }
    owners
        .into_iter()
        .map(|to| Routed {
            to,
            tag: proto::EV_SCREEN_RESIZED,
            payload: payload.clone(),
        })
        .collect()
}

pub fn post(routed: &Routed, from: Pid) {
    let message = Message::new(from, MessageKind::Event, routed.tag, &routed.payload);
    let _ = ipc::send_unchecked(routed.to, message);
}

/// Emits a notification from the kernel, without going through IPC.
///
/// A useful shorthand for the system components -- the module loader, the
/// login service -- which already live on the right side of the lock. An
/// application must go through `OP_NOTIFY_POST`.
pub fn post_notification(
    comp: &mut Compositor,
    level: Level,
    title: &str,
    body: &str,
) -> Option<u32> {
    let now = crate::idt::ticks();
    let result = comp.center.post(
        crate::scheduler::current_pid(),
        CORE_APP_ID,
        BinaryFormat::Dsa,
        level,
        title,
        body,
        super::notify::Dismissal::automatic(),
        0,
        false,
        now,
    );
    if result.is_ok() {
        comp.damage_all();
    }
    result.ok()
}

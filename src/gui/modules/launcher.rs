//! `launcher.mod` -- menu and application launcher (8.2, Launcher slot).
//!
//! The menu lists what the filesystem really holds in `/apps` and
//! `/sysapp`: a hard-coded list would be a mock-up, not a launcher. It is
//! hidden by default and opens on the Super key or on a click on the task
//! bar button.
//!
//! ## Launching
//!
//! A click on a row starts the application, through `apps::launch`. That
//! sentence was in this comment from the first day and was false for as
//! long: it read "launching an application is done with `SYS_PROC_SPAWN`,
//! exactly like the phase 5 self-test" while no line of the file ever called
//! it. It is true now, with one reservation that has to be said in the same
//! breath: what starts is the **native** code of `apps::NATIVE_APPS`, not
//! the bytes of the file in `/apps`. Executing a foreign binary in its own
//! address space needs one CR3 per process, which phase 3 left open.
//!
//! A name the table does not know says so on the row rather than doing
//! nothing: a menu entry silent under the click is indistinguishable from a
//! broken one.
//!
//! The click on the task bar button goes through the compositor:
//! `OP_MENU_TOGGLE` up, `EV_MENU_TOGGLE` broadcast to the shell modules.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use super::super::client::{ellipsize, Event};
use super::super::protocol::{CursorShape, KeyEvent};
use super::super::surface::{Layer, Rect};
use super::super::theme::Theme;
use super::{Canvas, Hover, Spec};

/// Scancode of the left Super key, extended set 1.
const SCAN_SUPER: u8 = 0x5B;
const SCAN_ESCAPE: u8 = 0x01;

const WIDTH: i32 = 260;
const ROW_HEIGHT: i32 = 20;
const MAX_ROWS: usize = 12;

pub fn task() -> ! {
    super::run(&Spec {
        name: "launcher",
        layer: Layer::Shell,
        geometry,
        draw,
        on_event,
        tick_ms: 0,
        visible_at_start: false,
    })
}

fn geometry(_width: i32, height: i32, theme: &Theme) -> (i32, i32, i32, i32) {
    let bar = theme.taskbar_height as i32;
    let menu_height = ROW_HEIGHT * MAX_ROWS as i32 + 2 * theme.padding as i32 + ROW_HEIGHT;
    (
        theme.padding as i32,
        height - bar - menu_height - theme.padding as i32,
        WIDTH,
        menu_height,
    )
}

fn draw(canvas: &mut Canvas) {
    let theme = canvas.theme;
    let surface = canvas.surface;
    let padding = theme.padding as i32;

    canvas.client.fill(surface, theme.surface);
    canvas.client.frame(
        surface,
        0,
        0,
        canvas.width,
        canvas.height,
        theme.border,
        theme.border_width.max(1) as u8,
    );

    canvas.client.text(
        surface,
        padding,
        padding,
        "Applications",
        theme.accent,
        None,
    );
    canvas.client.rect(
        surface,
        padding,
        padding + ROW_HEIGHT - 4,
        canvas.width - 2 * padding,
        1,
        theme.border,
    );

    let entries = list_applications();
    let cells = ((canvas.width - 3 * padding) / crate::graphics::font::GLYPH_WIDTH as i32).max(1);

    if entries.is_empty() {
        ROWS.store(0, Ordering::Relaxed);
        canvas.client.text(
            surface,
            padding,
            padding + ROW_HEIGHT + 2,
            "aucune application installee",
            theme.text_dim,
            None,
        );
        return;
    }
    ROWS.store(entries.len().min(MAX_ROWS) as i32, Ordering::Relaxed);

    for (index, entry) in entries.iter().take(MAX_ROWS).enumerate() {
        let y = padding + ROW_HEIGHT * (index as i32 + 1) + 2;
        let rect = row_rect(canvas, index);
        let on_it = HOVER.is(index as i32);
        if on_it {
            // A shade between the surface and its alternate, not the
            // alternate itself: `surface_alt` is what a *chosen* thing is
            // painted with everywhere else in this system, and a row that
            // took it would read as chosen rather than as pointed at.
            canvas.client.rect(
                surface,
                rect.x,
                rect.y,
                rect.width,
                rect.height,
                crate::graphics::framebuffer::mix(theme.surface, theme.surface_alt, 85),
            );
        }
        let refused = *entry == *REFUSED.lock();
        let label = if refused {
            ellipsize(
                &alloc::format!("{} - pas de code natif", entry),
                cells as usize,
            )
        } else {
            ellipsize(entry, cells as usize)
        };
        canvas.client.text(
            surface,
            padding * 2,
            y,
            &label,
            match (refused, on_it) {
                (true, _) => theme.danger,
                (false, true) => theme.text,
                (false, false) => theme.text_dim,
            },
            None,
        );
    }
}

fn on_event(canvas: &mut Canvas, event: &Event) -> bool {
    match event {
        Event::Key(key) if key.pressed => {
            if key.scancode == SCAN_SUPER {
                // Asked of the compositor rather than done here, so that
                // both the Super key and the task bar button go through one
                // path. Handling Super locally and the click through the
                // broadcast would give two ways to open the menu that could
                // disagree -- and the task bar's button would show the
                // wrong state for one of them.
                canvas.client.toggle_menu();
                return false;
            }
            if key.scancode == SCAN_ESCAPE && OPEN.load(Ordering::Relaxed) {
                canvas.client.toggle_menu();
            }
            false
        }

        // The broadcast, whoever asked: Super, or a click on the bar.
        Event::MenuToggle => {
            toggle(canvas);
            true
        }

        Event::Pointer(pointer) => {
            let (x, y) = super::local(canvas, pointer);
            let row = row_at(canvas, x, y);
            let entered = HOVER.set(row);
            // The hand, now that a row really starts something. It was the
            // arrow for as long as the click did nothing, because a hand
            // over an inert row claims otherwise.
            super::want_cursor(
                canvas,
                pointer,
                if row == Hover::NONE {
                    CursorShape::Arrow
                } else {
                    CursorShape::Hand
                },
            );

            // Only the pressing edge launches. A module receives an event
            // per movement, so acting on the button's state would spawn one
            // task per report of the mouse -- a hundred a second on a USB
            // pointer.
            let down = pointer.pressed(super::super::protocol::PointerEvent::BUTTON_LEFT);
            let was_down = DOWN.swap(down, Ordering::Relaxed);
            if down && !was_down && row != Hover::NONE {
                return start(canvas, row as usize) || entered;
            }
            entered
        }

        Event::Redraw => true,
        _ => false,
    }
}

/// Rectangle of a row, in surface-local coordinates.
///
/// One arithmetic read by the drawing and by the hit test: two would drift,
/// and the symptom would be a highlight that does not sit on the row the
/// pointer is on.
fn row_rect(canvas: &Canvas, index: usize) -> Rect {
    let padding = canvas.theme.padding as i32;
    Rect::new(
        padding,
        padding + ROW_HEIGHT * (index as i32 + 1),
        canvas.width - 2 * padding,
        ROW_HEIGHT,
    )
}

/// Which row the pointer is on, or `Hover::NONE`.
///
/// Bounded by the number of rows the **last paint** really listed, not by
/// `MAX_ROWS`: below the last entry there is empty panel, and highlighting
/// a row that has no application on it would point at nothing.
///
/// The count comes from `ROWS` and not from `list_applications()`, and that
/// is not a detail: that function lists three directories on the
/// filesystem. Calling it here would have meant three directory reads **per
/// pointer movement**, which is worse than the IPC round trip the task bar
/// goes out of its way to avoid for the same reason.
fn row_at(canvas: &Canvas, x: i32, y: i32) -> i32 {
    let count = ROWS.load(Ordering::Relaxed).max(0) as usize;
    for index in 0..count {
        if row_rect(canvas, index).contains(x, y) {
            return index as i32;
        }
    }
    Hover::NONE
}

static HOVER: Hover = Hover::new();

/// Number of rows the last paint drew, published for the hit test.
static ROWS: AtomicI32 = AtomicI32::new(0);

/// Button state at the previous event, for the pressing edge.
static DOWN: AtomicBool = AtomicBool::new(false);

/// Starts the application on a row. Returns `true` if a repaint is due.
///
/// The menu **closes** on a successful launch, and stays open on a refusal.
/// Closing on a refusal would hide the only place able to say why, and the
/// click would read as "nothing happened".
fn start(canvas: &mut Canvas, row: usize) -> bool {
    let entries = list_applications();
    let Some(name) = entries.get(row).cloned() else {
        return false;
    };

    match crate::apps::launch(&name) {
        Some(_) => {
            *REFUSED.lock() = String::new();
            canvas.client.toggle_menu();
            false
        }
        None => {
            crate::serial_println!("[mod-launcher] \"{}\" n'a pas de code natif", name);
            *REFUSED.lock() = name;
            true
        }
    }
}

/// Name of the last entry that could not be launched.
///
/// Shown on its row so the refusal is where the click was, and not only on
/// a serial line nobody is watching.
static REFUSED: crate::vga::SpinLock<String> = crate::vga::SpinLock::new(String::new());

/// Is the menu open?
///
/// A static because a module is a single task: it is the same role as a
/// local variable, with the scope that lets `on_event` and `draw` share
/// it without passing it through the common canvas.
static OPEN: AtomicBool = AtomicBool::new(false);

pub fn is_open() -> bool {
    OPEN.load(Ordering::Relaxed)
}

/// Toggles the visibility of the menu.
fn toggle(canvas: &mut Canvas) {
    use super::super::protocol::{self as proto, Writer};

    let open = !OPEN.load(Ordering::Relaxed);
    OPEN.store(open, Ordering::Relaxed);

    if open {
        canvas.client.tell(
            proto::OP_SURFACE_RAISE,
            &Writer::new().u32(canvas.surface).build(),
        );
    }
    canvas.client.show_surface(canvas.surface, open);
    // Said on the serial line because it is the only place it can be
    // observed: the visibility of a surface is a state of the compositor,
    // and nothing in the boot log otherwise says whether the menu answered.
    crate::serial_println!(
        "[mod-launcher] menu {}",
        if open { "ouvert" } else { "ferme" }
    );
}

/// What `/apps` and `/sysapp` really hold.
fn list_applications() -> Vec<String> {
    use crate::fs::perm::Credentials;

    let mut entries = Vec::new();
    if !crate::fs::is_ready() {
        return entries;
    }
    let who = Credentials::kernel();

    for directory in ["/apps", "/sysapp", "/games"] {
        let Ok(listing) = crate::fs::list(directory, who) else {
            continue;
        };
        for entry in listing {
            entries.push(entry.name);
        }
    }
    entries.sort();
    entries
}

/// The modifiers the launcher recognises, exposed for the self-test.
pub const RECOGNISED_MODIFIERS: u8 = KeyEvent::MOD_SUPER;

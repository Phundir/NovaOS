//! `taskbar.mod` -- task bar (8.2, Taskbar slot).
//!
//! Four zones: the launcher button on the left, the open windows in the
//! middle, the session buttons -- lock, log out, reboot, power off -- then
//! the time and the notification counter on the right.
//!
//! The bar refreshes every second because it shows the time, and not
//! because something moves. It is the only module of the set with a fixed
//! period: the other five only repaint on an event, which avoids waking
//! the compositor for nothing.
//!
//! ## Why the paint publishes its geometry
//!
//! The window buttons depend on what `window_list()` answers, which is an
//! IPC round trip. A hit test that called it again would cost one round
//! trip **per pointer movement** -- and a pointer publishes an event per
//! movement. So the paint records the rectangles it drew, and the hit test
//! reads them.
//!
//! That is the same rule as `login::Layout` -- one arithmetic, read by the
//! drawing and by the hit test -- reached the other way round, because
//! here the geometry depends on content the module does not own. The price
//! is that the zones are those of the **last** paint: a window opened
//! between two ticks is not clickable in the bar for up to a second.
//!
//! ## Why the window buttons answer a click
//!
//! They were inert, and a hover effect on something inert is a lie: the
//! highlight says "this answers", the click says otherwise. Adding the
//! raise was the smaller of the two ways to make the highlight honest --
//! the other being to give the buttons no highlight at all, and a task bar
//! whose buttons do nothing is not much of a task bar.
//!
//! ## Why three of the session buttons ask twice
//!
//! Log out ends the applications without asking them, reboot and power
//! off end everything: a slip of the mouse on the bar would lose unsaved
//! work. The first click **arms** the button -- it turns red and reads
//! "Confirmer ?" -- and only a second click on the same button within
//! `CONFIRM_MS` sends the request. Any other click disarms it. Lock loses
//! nothing and acts on the first click.
//!
//! The bar does not perform the actions: it asks the compositor with
//! `OP_SESSION_ACTION`, for the reason entry 48 of DECISIONS.md gives for
//! the menu -- a replacement bar must reach the same behaviour through the
//! same door.

use alloc::format;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};

use crate::graphics::font::GLYPH_WIDTH;
use crate::vga::SpinLock;

use super::super::client::{ellipsize, Event};
use super::super::protocol::{CursorShape, SessionAction};
use super::super::surface::{Layer, Rect, SurfaceId};
use super::super::theme::Theme;
use super::{Canvas, Hover, Spec};

/// Width of a window button, in pixels.
const BUTTON_WIDTH: i32 = 140;

/// Rank of the launcher button in the hover state.
///
/// The window buttons take the ranks from zero up; the launcher is given a
/// rank of its own so that one integer describes the whole bar.
const LAUNCHER: i32 = -2;

/// Rank of the first session button; the others go downwards from it.
///
/// Far from `LAUNCHER` and from `Hover::NONE` so that no arithmetic slip
/// can turn a click on "power off" into a click on the menu.
const SESSION_RANK: i32 = -10;

/// The session buttons, left to right, with their labels.
const SESSION_BUTTONS: [(SessionAction, &str); 4] = [
    (SessionAction::Lock, "Verrouiller"),
    (SessionAction::Logout, "Deconnexion"),
    (SessionAction::Reboot, "Redemarrer"),
    (SessionAction::PowerOff, "Eteindre"),
];

/// Label of an armed button.
const CONFIRM: &str = "Confirmer ?";

/// How long an armed button waits for its second click.
const CONFIRM_MS: u64 = 4000;

/// Room kept on the right for the clock and the notification counter.
///
/// Fixed rather than measured on the text: the counter comes and goes, and
/// buttons that shifted left whenever a notification arrived would move
/// under a pointer about to click them.
const CLOCK_CHARS: i32 = 9;
const BADGE_CHARS: i32 = 5;

/// Zones the last paint produced, in surface-local coordinates.
struct Zones {
    launcher: Rect,
    /// One entry per window button drawn, in the order drawn.
    windows: Vec<(SurfaceId, Rect)>,
    /// One entry per session button, in `SESSION_BUTTONS` order.
    session: [Rect; 4],
}

const EMPTY: Rect = Rect {
    x: 0,
    y: 0,
    width: 0,
    height: 0,
};

static ZONES: SpinLock<Zones> = SpinLock::new(Zones {
    launcher: EMPTY,
    windows: Vec::new(),
    session: [EMPTY; 4],
});

static HOVER: Hover = Hover::new();

/// Button state at the previous event, for the pressing edge.
static DOWN: AtomicBool = AtomicBool::new(false);

/// Is the menu open, as far as this module knows?
///
/// Its **own** copy, kept from the `EV_MENU_TOGGLE` broadcast, and not read
/// from `launcher::is_open()`. Peeking at the other module's static would
/// work -- one address space -- and would quietly undo the reason the
/// compositor brokers the toggle in the first place: 8.2 makes the button
/// and the menu independently replaceable, and a replacement task bar
/// living outside the kernel would have no such static to read.
///
/// The broadcast reaches every shell module, so the two stay in step
/// whether the toggle came from the button or from the Super key.
static MENU_OPEN: AtomicBool = AtomicBool::new(false);

/// Index in `SESSION_BUTTONS` of the armed button, or -1.
static ARMED: AtomicI32 = AtomicI32::new(-1);
/// Tick at which it was armed.
static ARMED_AT: AtomicU64 = AtomicU64::new(0);

pub fn task() -> ! {
    super::run(&Spec {
        name: "taskbar",
        layer: Layer::Shell,
        geometry,
        draw,
        on_event,
        tick_ms: 1000,
        visible_at_start: true,
    })
}

fn geometry(width: i32, height: i32, theme: &Theme) -> (i32, i32, i32, i32) {
    let bar = theme.taskbar_height as i32;
    (0, height - bar, width, bar)
}

/// Lightens a colour towards white for the hover shade.
///
/// Towards white and not towards another colour of the theme: a theme
/// chooses its own palette, and any other colour picked as a hover shade
/// could be declared equal to the one it has to stand out from.
fn hovered(color: u32) -> u32 {
    crate::graphics::framebuffer::mix(color, 0xFF_FF_FF, 45)
}

/// Rectangle of the launcher button.
pub fn launcher_rect(height: i32, padding: i32) -> Rect {
    let width = 9 * GLYPH_WIDTH as i32 + 2 * padding;
    Rect::new(padding / 2, 2, width, height - 4)
}

/// Rectangles of the four session buttons, right-aligned before the clock.
///
/// Every button has the width of the longest text it can show -- its label
/// or `CONFIRM` -- so that arming one does not move the others.
///
/// Public for the self-test, which checks that the buttons fit beside the
/// launcher at the smallest resolution the kernel falls back to.
pub fn session_rects(width: i32, height: i32, padding: i32) -> [Rect; 4] {
    let glyph = GLYPH_WIDTH as i32;
    let reserved = (CLOCK_CHARS + BADGE_CHARS) * glyph + 3 * padding;
    let gap = padding / 2;

    let mut rects = [EMPTY; 4];
    let mut right = width - reserved;
    for index in (0..SESSION_BUTTONS.len()).rev() {
        let chars = SESSION_BUTTONS[index].1.len().max(CONFIRM.len()) as i32;
        let button = chars * glyph + 2 * padding;
        right -= button;
        rects[index] = Rect::new(right, 3, button, height - 6);
        right -= gap;
    }
    rects
}

/// The armed button, forgetting it once its delay has run out.
fn armed() -> Option<usize> {
    let index = ARMED.load(Ordering::Relaxed);
    if index < 0 {
        return None;
    }
    let per_tick = 1000 / crate::TIMER_HZ as u64;
    let deadline = ARMED_AT.load(Ordering::Relaxed) + CONFIRM_MS.div_ceil(per_tick);
    if crate::idt::ticks() > deadline {
        ARMED.store(-1, Ordering::Relaxed);
        return None;
    }
    Some(index as usize)
}

/// Forgets the armed button. Returns `true` if one was armed.
fn disarm() -> bool {
    ARMED.swap(-1, Ordering::Relaxed) >= 0
}

fn draw(canvas: &mut Canvas) {
    let theme = canvas.theme;
    let surface = canvas.surface;
    let padding = theme.padding as i32;
    let text_y = (canvas.height - crate::graphics::font::GLYPH_HEIGHT as i32) / 2;

    canvas.client.fill(surface, theme.surface);
    canvas
        .client
        .rect(surface, 0, 0, canvas.width, 1, theme.border);

    // --- launcher button ---
    let launcher = launcher_rect(canvas.height, padding);
    let menu_open = MENU_OPEN.load(Ordering::Relaxed);
    // The button says whether the menu is open. Without that, clicking it a
    // second time to close would be a guess: the menu may be hidden behind
    // a window, and the only thing always visible is the button.
    let base = if menu_open {
        crate::graphics::framebuffer::mix(theme.accent, 0x00_00_00, 60)
    } else {
        theme.accent
    };
    canvas.client.rect(
        surface,
        launcher.x,
        launcher.y,
        launcher.width,
        launcher.height,
        if HOVER.is(LAUNCHER) { hovered(base) } else { base },
    );
    canvas.client.text(
        surface,
        padding,
        text_y,
        "Nova OS",
        theme.background,
        None,
    );

    // --- session buttons, drawn before the windows so that the window
    // buttons know where to stop ---
    let session = session_rects(canvas.width, canvas.height, padding);
    let armed = armed();
    for (index, rect) in session.iter().enumerate() {
        let (action, label) = SESSION_BUTTONS[index];
        let is_armed = armed == Some(index);
        let base = if is_armed {
            theme.danger
        } else {
            theme.surface_alt
        };
        let rank = SESSION_RANK - index as i32;
        canvas.client.rect(
            surface,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            if HOVER.is(rank) { hovered(base) } else { base },
        );
        canvas.client.frame(
            surface,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            if HOVER.is(rank) {
                theme.text_dim
            } else {
                theme.border
            },
            1,
        );
        let text = if is_armed { CONFIRM } else { label };
        let text_width = GLYPH_WIDTH as i32 * text.len() as i32;
        // Power off is written in the danger colour even unarmed: it is
        // the one button whose effect cannot be undone from the screen.
        let color = if is_armed {
            theme.background
        } else if action == SessionAction::PowerOff {
            theme.danger
        } else {
            theme.text
        };
        canvas.client.text(
            surface,
            rect.x + (rect.width - text_width) / 2,
            text_y,
            text,
            color,
            None,
        );
    }

    // --- open windows ---
    let windows = canvas.client.window_entries();
    let mut x = launcher.x + launcher.width + padding;
    let limit = session[0].x - padding;
    let cells = ((BUTTON_WIDTH - 2 * padding) / GLYPH_WIDTH as i32).max(1);
    let mut zones = Vec::new();

    for (rank, window) in windows.iter().enumerate() {
        if x + BUTTON_WIDTH > limit {
            break;
        }
        let id = &window.id;
        let rect = Rect::new(x, 3, BUTTON_WIDTH, canvas.height - 6);
        // The window with the focus stands out: it is the application
        // the keyboard is talking to.
        let background = if window.focused {
            theme.accent
        } else if window.visible {
            theme.surface_alt
        } else {
            theme.surface
        };
        let background = if HOVER.is(rank as i32) {
            hovered(background)
        } else {
            background
        };
        canvas
            .client
            .rect(surface, rect.x, rect.y, rect.width, rect.height, background);
        canvas.client.frame(
            surface,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            if HOVER.is(rank as i32) {
                theme.text_dim
            } else {
                theme.border
            },
            1,
        );
        // The window's own title -- the one its title bar shows. This was
        // "fenetre N", the surface number: `OP_SURFACE_LIST` carries no
        // title.
        let name = if window.title.is_empty() {
            format!("fenetre {}", id)
        } else {
            window.title.clone()
        };
        let label = ellipsize(&name, cells as usize);
        canvas.client.text(
            surface,
            rect.x + padding,
            text_y,
            &label,
            if window.focused { theme.background } else { theme.text },
            None,
        );
        zones.push((*id, rect));
        x += BUTTON_WIDTH + padding / 2;
    }

    // Published for the hit test, which has no other way of knowing what
    // was drawn.
    *ZONES.lock() = Zones {
        launcher,
        windows: zones,
        session,
    };

    // --- time and notifications, on the right ---
    let clock = wall_clock();
    let clock_width = GLYPH_WIDTH as i32 * clock.len() as i32;
    canvas.client.text(
        surface,
        canvas.width - clock_width - padding,
        text_y,
        &clock,
        theme.text,
        None,
    );

    let badge = notification_badge(canvas);
    if !badge.is_empty() {
        let badge_width = GLYPH_WIDTH as i32 * badge.len() as i32;
        let x = canvas.width - clock_width - badge_width - 3 * padding;
        canvas
            .client
            .text(surface, x, text_y, &badge, theme.accent, None);
    }
}

/// What the pointer is over, by rank. `Hover::NONE` for the bare bar.
///
/// The clock and the notification counter are deliberately not zones: they
/// answer no click, so giving them a highlight would say they do.
fn zone_at(x: i32, y: i32) -> i32 {
    let zones = ZONES.lock();
    if zones.launcher.contains(x, y) {
        return LAUNCHER;
    }
    for (index, rect) in zones.session.iter().enumerate() {
        if rect.contains(x, y) {
            return SESSION_RANK - index as i32;
        }
    }
    for (rank, (_, rect)) in zones.windows.iter().enumerate() {
        if rect.contains(x, y) {
            return rank as i32;
        }
    }
    Hover::NONE
}

/// Index in `SESSION_BUTTONS` of a session rank.
fn session_index(zone: i32) -> Option<usize> {
    let index = SESSION_RANK - zone;
    if (0..SESSION_BUTTONS.len() as i32).contains(&index) {
        Some(index as usize)
    } else {
        None
    }
}

fn on_event(canvas: &mut Canvas, event: &Event) -> bool {
    match event {
        Event::Pointer(pointer) => {
            let (x, y) = super::local(canvas, pointer);
            let zone = zone_at(x, y);

            // Repainted on the **boundary**, never on the position. The
            // first version of this module repainted on every movement,
            // which cost some twenty IPC messages per millimetre of mouse
            // for no visible effect -- and is why the bar had no hover
            // effect at all for two phases.
            let entered = HOVER.set(zone);

            super::want_cursor(
                canvas,
                pointer,
                if zone == Hover::NONE {
                    CursorShape::Arrow
                } else {
                    CursorShape::Hand
                },
            );

            // Only the **pressing edge** acts. The bar receives an event
            // per movement, so acting on the button's state would toggle
            // the menu at the report rate of the mouse -- a hundred times
            // a second on a USB pointer -- for as long as the button is
            // held over it.
            let down = pointer.pressed(super::super::protocol::PointerEvent::BUTTON_LEFT);
            let was_down = DOWN.swap(down, Ordering::Relaxed);
            if down && !was_down {
                return act(canvas, zone) || entered;
            }
            entered
        }
        // The broadcast the compositor sends when the menu opens or closes,
        // whoever asked for it. Repainted at once rather than on the next
        // tick: a button that takes up to a second to show what it did
        // reads as a button that did nothing.
        Event::MenuToggle => {
            MENU_OPEN.fetch_xor(true, Ordering::Relaxed);
            true
        }
        // A window opened, closed, took the focus or was renamed.
        Event::WindowsChanged => true,
        Event::Redraw => true,
        _ => false,
    }
}

/// Acts on the zone under the pointer. Returns `true` if a repaint is due.
fn act(canvas: &mut Canvas, zone: i32) -> bool {
    if let Some(index) = session_index(zone) {
        return session_click(canvas, index);
    }

    // Any click elsewhere on the bar disarms a session button: a
    // confirmation must follow the arming, not come back to it later.
    let disarmed = disarm();

    match zone {
        LAUNCHER => {
            // The bar does not open the menu: it asks for it to be opened.
            // The menu is `launcher.mod`'s surface, and 8.2 makes the two
            // independently replaceable.
            canvas.client.toggle_menu();
            crate::serial_println!("[mod-taskbar] clic : bascule du menu demandee");
            // No repaint for the menu here: the button's colour follows
            // `MENU_OPEN`, which only the broadcast may change. Flipping it
            // locally as well would double-count a toggle the compositor
            // refused, and the button would then claim a state the menu
            // does not have.
            disarmed
        }
        rank if rank >= 0 => {
            use super::super::protocol::{self as proto, Writer};
            let Some((id, _)) = ZONES.lock().windows.get(rank as usize).copied() else {
                return disarmed;
            };
            canvas
                .client
                .tell(proto::OP_SURFACE_RAISE, &Writer::new().u32(id).build());
            canvas
                .client
                .tell(proto::OP_SURFACE_FOCUS, &Writer::new().u32(id).build());
            crate::serial_println!("[mod-taskbar] clic : fenetre {} au premier plan", id);
            true
        }
        _ => disarmed,
    }
}

/// A click on a session button: arm it, or send the request.
fn session_click(canvas: &mut Canvas, index: usize) -> bool {
    let (action, label) = SESSION_BUTTONS[index];

    if action != SessionAction::Lock && armed() != Some(index) {
        ARMED.store(index as i32, Ordering::Relaxed);
        ARMED_AT.store(crate::idt::ticks(), Ordering::Relaxed);
        crate::serial_println!(
            "[mod-taskbar] clic : \"{}\" arme - second clic pour confirmer",
            label
        );
        return true;
    }
    disarm();

    use super::super::protocol::{self as proto, Reader};
    let verdict = canvas
        .client
        .request(proto::OP_SESSION_ACTION, &[action.as_code()])
        .and_then(|reply| Reader::new(&reply).u8());
    match verdict {
        Some(proto::ERR_NONE) => {
            crate::serial_println!("[mod-taskbar] clic : {} demande(e)", action.as_str());
        }
        Some(code) => {
            crate::serial_println!(
                "[mod-taskbar] clic : {} refuse(e) - {}",
                action.as_str(),
                proto::error_name(code)
            );
        }
        None => {
            crate::serial_println!(
                "[mod-taskbar] clic : {} sans reponse du compositeur",
                action.as_str()
            );
        }
    }
    true
}

/// Wall time, or the tick counter if the clock did not answer.
///
/// Displaying a counter rather than `--:--:--` is a choice: a machine
/// with no RTC is not a machine with no time, and the tick is the closest
/// thing to a clock the system has.
fn wall_clock() -> alloc::string::String {
    // Local time of the session: its zone and its 12/24 h format (14.2,
    // stored in UTC, converted at display). The raw UTC reading shown
    // before put a Paris user's clock two hours behind in summer.
    if let Some(now) = crate::rtc::now() {
        return crate::i18n::settings().format_time(now);
    }
    let seconds = crate::idt::ticks() / crate::TIMER_HZ as u64;
    format!("+{:02}:{:02}", seconds / 60, seconds % 60)
}

fn notification_badge(canvas: &mut Canvas) -> alloc::string::String {
    use super::super::protocol::{self as proto, Reader};

    let Some(reply) = canvas.client.request(proto::OP_NOTIFY_COUNT, &[]) else {
        return alloc::string::String::new();
    };
    let mut reader = Reader::new(&reply);
    if reader.u8() != Some(proto::ERR_NONE) {
        return alloc::string::String::new();
    }
    match reader.u8() {
        Some(0) | None => alloc::string::String::new(),
        Some(count) => format!("[{}]", count),
    }
}

//! The loop shared by the windowed applications that need a clock.
//!
//! ## Why a second loop, beside the notepad's
//!
//! The notepad sleeps on its mailbox: nothing in it changes unless a key
//! or a click arrives. The audio applications are the opposite -- the
//! player must feed the mixer every few tens of milliseconds, the recorder
//! must drain the microphone, both must move a level meter -- so they wake
//! on a period and empty their mailbox each time, the arrangement
//! `gui::modules::run` found for the task bar (and whose history, a click
//! waiting 1336 ms behind a queue, is why the whole box is emptied).
//!
//! Three applications written separately would each have to get the
//! stop request, the close cross, Ctrl+W and the pressing edge right. One
//! loop gets them right once.
//!
//! ## The pointer
//!
//! The window is moved by the compositor without the application being
//! told, so its origin is asked on every pointer event (the reason the
//! notepad gives). The application receives local coordinates and three
//! facts: the button went down, is held, or came up -- a slider needs all
//! three, a button only the first.

use crate::gui::client::{Client, Event};
use crate::gui::protocol::{CursorShape, KeyEvent, PointerEvent};
use crate::gui::surface::{Layer, SurfaceId};
use crate::gui::theme::Theme;

/// Period of the loop, in milliseconds: one compositor frame, the finest
/// the 100 Hz timer can time. It was 20 ms; halving it makes a meter or a
/// progress bar move with the sound rather than a step behind, and an
/// idle wake costs a few hundred instructions -- the loop repaints only
/// what changed.
pub const PERIOD_MS: u64 = 10;

/// What an application draws with.
pub struct Ui<'a> {
    pub client: &'a mut Client,
    pub surface: SurfaceId,
    pub theme: Theme,
    pub width: i32,
    pub height: i32,
}

impl Ui<'_> {
    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        self.client.rect(self.surface, x, y, w, h, color);
    }

    pub fn frame(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        self.client.frame(self.surface, x, y, w, h, color, 1);
    }

    pub fn text(&mut self, x: i32, y: i32, text: &str, color: u32) {
        self.client.text(self.surface, x, y, text, color, None);
    }
}

/// State of the left button for one pointer event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Press {
    /// Went down on this event.
    pub pressed: bool,
    /// Is down.
    pub held: bool,
    /// Went up on this event.
    pub released: bool,
}

pub trait App {
    /// The size the window really got -- smaller than asked on a small
    /// screen. Called once, before the first paint.
    fn size(&mut self, width: i32, height: i32);
    /// Draws everything. Called between `reset` and `commit`.
    fn paint(&self, ui: &mut Ui);
    /// A key. Returns `true` if a repaint is due.
    fn key(&mut self, key: &KeyEvent) -> bool;
    /// The pointer, in window coordinates. Returns whether a repaint is
    /// due, and the shape the pointer should have.
    fn pointer(&mut self, x: i32, y: i32, press: Press) -> (bool, CursorShape);
    /// Called every `PERIOD_MS`. Returns `true` if a repaint is due.
    fn tick(&mut self) -> bool;
    /// Has the application asked to close?
    fn quit_requested(&self) -> bool {
        false
    }
    /// The mouse wheel turned over the window: notches, positive away
    /// from the user. Returns `true` if a repaint is due.
    fn wheel(&mut self, _x: i32, _y: i32, _notches: i32) -> bool {
        false
    }
    /// Does the application handle Ctrl+W itself (a browser closes a tab
    /// with it)? The window closes on it otherwise.
    fn handles_close_key(&self) -> bool {
        false
    }
    /// The title the window should carry now, if the application changes
    /// it -- a browser names its window after the page.
    fn title(&self) -> Option<alloc::string::String> {
        None
    }
    /// Last call: give back streams, the microphone, whatever is held.
    fn close(&mut self);
}

/// Creates the window, runs the application, and never returns.
///
/// `tag` names the application in the log.
pub fn run<A: App>(tag: &str, title: &str, width: i32, height: i32, app: &mut A) -> ! {
    let Some(mut client) = Client::connect(5_000) else {
        crate::serial_println!("[{}] compositeur injoignable", tag);
        app.close();
        crate::scheduler::exit_current_with(-1);
    };
    let Some((screen_w, screen_h, _, _)) = client.screen_info() else {
        app.close();
        crate::scheduler::exit_current_with(-1);
    };
    let theme = client.theme().unwrap_or_default();
    let width = width.min(screen_w as i32 - 40);
    let height = height.min(screen_h as i32 - 80);
    let x = (screen_w as i32 - width) / 2;
    let y = (screen_h as i32 - height) / 2;

    let Some(surface) = client.create_surface(Layer::Window, x, y, width, height, title) else {
        crate::serial_println!("[{}] creation de surface refusee", tag);
        app.close();
        crate::scheduler::exit_current_with(-1);
    };
    crate::serial_println!("[{}] surface {} - {}x{}", tag, surface, width, height);
    app.size(width, height);

    let mut open = true;
    let mut held = false;
    let mut dirty = true;
    let mut current_title = alloc::string::String::from(title);

    loop {
        for _ in 0..crate::ipc::MAX_QUEUE_LEN {
            let Some(event) = client.poll_event() else {
                break;
            };
            match &event {
                Event::Key(key) => {
                    // Ctrl+W closes, in every application, the same way
                    // the cross does.
                    if key.pressed
                        && key.has(KeyEvent::MOD_CTRL)
                        && key.character().map(|c| c.to_ascii_lowercase()) == Some('w')
                        && !app.handles_close_key()
                    {
                        open = false;
                    } else {
                        dirty |= app.key(key);
                    }
                }
                Event::Pointer(pointer) => {
                    let origin = crate::gui::surface_origin(surface).unwrap_or((x, y));
                    let down = pointer.pressed(PointerEvent::BUTTON_LEFT);
                    let press = Press {
                        pressed: down && !held,
                        held: down,
                        released: !down && held,
                    };
                    held = down;
                    let (local_x, local_y) = (pointer.x as i32 - origin.0, pointer.y as i32 - origin.1);
                    if pointer.wheel != 0 {
                        dirty |= app.wheel(local_x, local_y, pointer.wheel as i32);
                    }
                    let (repaint, shape) = app.pointer(local_x, local_y, press);
                    dirty |= repaint;
                    if pointer.cursor_shape() != shape {
                        client.set_cursor(shape);
                    }
                }
                Event::Close(id) if *id == surface => open = false,
                Event::Focus { .. } | Event::Redraw => dirty = true,
                Event::Other(message) if crate::gui::stop_requested(message) => open = false,
                _ => {}
            }
        }

        if !open || app.quit_requested() {
            app.close();
            client.destroy_surface(surface);
            // A pixel canvas the application forgot to drop would stay
            // registered, its megabytes with it: the task will not unwind.
            crate::gui::canvas::forget_owner(crate::scheduler::current_pid());
            crate::serial_println!("[{}] ferme - surface rendue", tag);
            crate::scheduler::exit_current_with(0);
        }

        dirty |= app.tick();
        if let Some(wanted) = app.title()
            && wanted != current_title
        {
            client.set_title(surface, &wanted);
            current_title = wanted;
        }
        if dirty {
            client.reset(surface);
            let mut ui = Ui {
                client: &mut client,
                surface,
                theme,
                width,
                height,
            };
            ui.client.fill(surface, theme.background);
            app.paint(&mut ui);
            client.commit(surface);
            dirty = false;
        }

        let _ = crate::syscall::invoke(
            crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
            [PERIOD_MS, 0, 0, 0, 0, 0],
        );
    }
}

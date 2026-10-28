//! 6.1.1 -- Keyboard driver, migrated out of the kernel.
//!
//! ## What changed from phase 2
//!
//! The decoding lived in `src/keyboard.rs` and ran **inside the interrupt
//! handler**: the handler read port 0x60, translated the scancode and
//! printed it on screen. Three defects, in order of severity:
//!
//! 1. **The policy was in the kernel.** "A key produces a character on
//!    screen" is an interface decision, not a property of the hardware.
//!    The login screen of 2.4 and the compositor of 8.1 want the same
//!    key stream without anybody printing it.
//!
//! 2. **A single consumer.** The handler decided the recipient on its
//!    own. Publishing on a channel lets several subscribers receive the
//!    same event -- that is what an accessibility module (16) needs, one
//!    that must see the keystrokes without intercepting them.
//! 3. **No configurable layout.** 14.2 wants a per-user arrangement; a
//!    table inside an interrupt handler cannot be reloaded.
//!
//! The handler now does nothing but increment a counter and wake this
//! task. All the rest is here.
//!
//! ## Event format
//!
//! Published on the `dev.keyboard` channel, eight bytes:
//!
//! ```text
//!   0x00  1  kind: 1 = key
//!   0x01  1  raw scancode
//!   0x02  1  1 = pressed, 0 = released
//!   0x03  1  modifiers: bit 0 shift, bit 1 ctrl, bit 2 alt
//!   0x04  4  character, as a code point; zero if the key produces none
//! ```
//!
//! The raw scancode accompanies the character instead of being replaced
//! by it: a function key or an arrow has no character, and a shortcut is
//! described by the physical key, not by the glyph the current layout
//! associates with it.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::ipc::{self, Message, MessageKind};
use crate::vga::SpinLock;

use super::{DriverClass, ps2};

/// Channel through which the driver offers its service.
pub const CHANNEL: &str = "dev.keyboard";

/// Size of a published event.
pub const EVENT_SIZE: usize = 8;

/// Decoded key, as a kernel consumer sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Tab,
    Escape,
    Unknown(u8),
}

/// Active modifiers.
#[derive(Debug, Clone, Copy, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl Modifiers {
    fn bits(&self) -> u8 {
        (self.shift as u8) | ((self.ctrl as u8) << 1) | ((self.alt as u8) << 2)
    }
}

static KEY_COUNT: AtomicU64 = AtomicU64::new(0);

/// State of the decoder: the modifiers, and the queue of keys handed to
/// the kernel consumers that do not go through IPC.
struct State {
    modifiers: Modifiers,
    /// The 0xE0 prefix announces an extended key: the next scancode does
    /// not belong to the same code space.
    extended: bool,
    /// Num Lock: the keypad types digits (on) or moves (off). On at
    /// boot, as most machines leave it; toggled by its key, and shown on
    /// the keyboard's LED.
    num_lock: bool,
    queue: [Key; 32],
    head: usize,
    len: usize,
}

static STATE: SpinLock<State> = SpinLock::new(State {
    modifiers: Modifiers {
        shift: false,
        ctrl: false,
        alt: false,
    },
    extended: false,
    num_lock: true,
    queue: [Key::Unknown(0); 32],
    head: 0,
    len: 0,
});

/// Removes a key from the queue, without waiting.
///
/// Reserved to kernel consumers -- the authorisation prompt of 12.5,
/// which must read an answer before the compositor exists. A program, on
/// the other hand, subscribes to the channel.
pub fn read_key() -> Option<Key> {
    let mut state = STATE.lock();
    if state.len == 0 {
        return None;
    }
    let key = state.queue[state.head];
    state.head = (state.head + 1) % state.queue.len();
    state.len -= 1;
    Some(key)
}

/// Empties the queue of keys not yet consumed.
///
/// ## Why this is a security function
///
/// A modal prompt reads the keyboard queue. Whatever was typed **before**
/// it appeared is still in that queue, and would answer it: press `o` for
/// something else, and the authorisation dialog that opens a moment later
/// is already accepted, without anything having been displayed long
/// enough to read.
///
/// It is not hypothetical. The phase 10.3 self-test raised a real prompt
/// and saw it answered instantly by a keystroke left over from the login
/// screen -- the case where the leftover key happens to be `o` is the
/// case where a program gets what it asked for.
///
/// So every reader of an answer drains first. Returns how many keys were
/// thrown away, which is worth logging: a large number means somebody was
/// typing when the prompt opened.
pub fn drain() -> usize {
    let mut state = STATE.lock();
    let dropped = state.len;
    state.len = 0;
    state.head = 0;
    dropped
}

/// Number of keys decoded since boot.
pub fn key_count() -> u64 {
    KEY_COUNT.load(Ordering::Relaxed)
}

/// Scancode set 1, translated through the **active layout** (spec 14.2).
///
/// Phase 6.1.1 wrote a single AZERTY table into this function, and its
/// own module header listed that as defect number three: "14.2 wants a
/// per-user arrangement; a table inside an interrupt handler cannot be
/// reloaded". Step 10.2.4 lifted the table into `i18n::keyboard`, where
/// it is data rather than code, and this function now asks which layout
/// is in force.
///
/// What stays here is what does **not** depend on the layout: Escape,
/// Backspace, Tab, Enter and the physical position of the space bar are
/// the same on AZERTY, QWERTY, QWERTZ and Dvorak alike.
///
/// The accented keys of a French keyboard still return their bare
/// letter: everything displayed must stay in the 0x20-0x7E range,
/// failing which the VGA writer replaces the character with a solid
/// square.
fn decode(code: u8, shift: bool) -> Key {
    decode_with(crate::i18n::active_layout(), code, shift)
}

/// Is Num Lock on?
pub fn num_lock() -> bool {
    STATE.lock().num_lock
}

/// Keypad key under Num Lock: (character, code published). The code is
/// that of the main-row key typing the same digit, not the keypad's own:
/// without Num Lock the keypad's codes are the arrows, Home, End, Page
/// Up/Down, Insert and Delete (0x47 to 0x53), and an application reading
/// the code would move its cursor when a digit was typed.
fn keypad_digit(code: u8) -> Option<(char, u8)> {
    Some(match code {
        0x47 => ('7', 0x08),
        0x48 => ('8', 0x09),
        0x49 => ('9', 0x0A),
        0x4B => ('4', 0x05),
        0x4C => ('5', 0x06),
        0x4D => ('6', 0x07),
        0x4F => ('1', 0x02),
        0x50 => ('2', 0x03),
        0x51 => ('3', 0x04),
        0x52 => ('0', 0x0B),
        0x53 => ('.', 0x34),
        _ => return None,
    })
}

/// Keypad operators, typed whatever Num Lock says. `/` and Enter come
/// with the 0xE0 prefix, the others without.
fn keypad_operator(code: u8, extended: bool) -> Option<Key> {
    match (code, extended) {
        (0x37, false) => Some(Key::Char('*')),
        (0x4A, false) => Some(Key::Char('-')),
        (0x4E, false) => Some(Key::Char('+')),
        (0x35, true) => Some(Key::Char('/')),
        (0x1C, true) => Some(Key::Enter),
        _ => None,
    }
}

/// Same translation, against an explicit layout.
///
/// Separated so the self-test can exercise a layout without changing the
/// one the machine is using: a test that had to switch the real keyboard
/// to check QWERTY would leave it switched if it failed halfway.
pub fn decode_with(layout: crate::i18n::keyboard::Layout, code: u8, shift: bool) -> Key {
    match code {
        0x01 => Key::Escape,
        0x0E => Key::Backspace,
        0x0F => Key::Tab,
        0x1C => Key::Enter,
        // The `m` key of a French keyboard sits at 0x27, where an English
        // one has the semicolon. Phase 7.7 paid for that difference: the
        // "admin" account turned out to be untypable. The layout tables
        // carry it now, which is why this function no longer decides.
        other => match layout.character(other, shift) {
            Some(glyph) => Key::Char(glyph),
            None => Key::Unknown(other),
        },
    }
}

/// Translates a raw byte. Returns the event to publish, if there is one.
///
/// Public because it is the entry point of the decoding: the driver task
/// calls it on what the controller hands it, and the self-test calls it
/// on a fabricated scancode sequence. Exercising the decoding assumes
/// being able to give it bytes with no keyboard.
pub fn decode_byte(byte: u8) -> Option<[u8; EVENT_SIZE]> {
    let mut state = STATE.lock();

    // Acknowledgement (0xFA) and resend request (0xFE) of a command to the
    // keyboard -- the Num Lock LED: answers, not keys.
    if byte == 0xFA || byte == 0xFE {
        return None;
    }

    if byte == 0xE0 {
        state.extended = true;
        return None;
    }
    let extended = core::mem::take(&mut state.extended);

    let released = byte & 0x80 != 0;
    let code = byte & 0x7F;

    // 10.2.4 -- The layout switch key (spec 14.2).
    //
    // It is consumed here: it produces neither event nor modifier. That
    // is a real cost, and a deliberate one -- naming a key as the switch
    // takes its other meaning away. The default is the **right** Alt,
    // which is why the extended flag is part of the test: the left Alt
    // carries the shortcuts of 16.4, and having those flip the layout
    // would be the opposite of useful.
    if !released && crate::i18n::is_switch_key(code, extended) {
        let layout = crate::i18n::toggle_layout();
        crate::serial_println!("[clavier] agencement {}", layout.as_str());
        return None;
    }

    // Modifiers: they change the state without producing a character
    // event. Ctrl and Alt have an identical code in their extended form
    // -- right Ctrl and AltGr -- handled as their left counterpart.
    match code {
        0x2A | 0x36 => {
            state.modifiers.shift = !released;
            return None;
        }
        0x1D => {
            state.modifiers.ctrl = !released;
            return None;
        }
        0x38 => {
            state.modifiers.alt = !released;
            return None;
        }
        _ => {}
    }

    // Num Lock itself: toggles, lights its LED, types nothing.
    if code == 0x45 && !extended {
        if !released {
            state.num_lock = !state.num_lock;
            let on = state.num_lock;
            drop(state);
            super::ps2::set_keyboard_leds(if on { 0b010 } else { 0 });
            crate::serial_println!("[clavier] pave numerique {}", if on { "actif" } else { "inactif" });
        }
        return None;
    }

    // Shift inverts Num Lock for the keypad's digit keys, as on any PC:
    // Shift + 8 moves up when Num Lock is on.
    let digits = state.num_lock != state.modifiers.shift;
    let mut published = code;
    let key = if let Some(key) = keypad_operator(code, extended) {
        key
    } else if !extended && digits && let Some((digit, main_row)) = keypad_digit(code) {
        published = main_row;
        Key::Char(digit)
    } else if extended || keypad_digit(code).is_some() || code == 0x4C {
        // The extended keys -- arrows, right keypad -- and the keypad
        // without Num Lock carry no character. Returning them as unknown
        // is accurate: a consumer will recognise them by their scancode.
        // The keypad's 5 then does nothing, like everywhere else.
        Key::Unknown(code)
    } else {
        decode(code, state.modifiers.shift)
    };

    let mut event = [0u8; EVENT_SIZE];
    event[0] = 1;
    event[1] = published;
    event[2] = u8::from(!released);
    event[3] = state.modifiers.bits();

    let character = match key {
        Key::Char(c) => c as u32,
        Key::Enter => '\n' as u32,
        Key::Tab => '\t' as u32,
        _ => 0,
    };
    event[4..8].copy_from_slice(&character.to_le_bytes());

    if released {
        return Some(event);
    }

    KEY_COUNT.fetch_add(1, Ordering::Relaxed);

    // Queue of the kernel consumers: only pressed keys enter it, a
    // release interests nothing but a shortcut handler.
    if state.len == state.queue.len() {
        state.head = (state.head + 1) % state.queue.len();
        state.len -= 1;
    }
    let tail = (state.head + state.len) % state.queue.len();
    state.queue[tail] = key;
    state.len += 1;

    Some(event)
}

/// Driver task.
pub fn task() -> ! {
    let handle = match super::register("clavier", DriverClass::Input, CHANNEL) {
        Some(handle) => handle,
        None => crate::scheduler::exit_current_with(-1),
    };

    if ipc::open_channel(CHANNEL).is_err() {
        super::failed(handle, "canal deja ouvert");
        crate::scheduler::exit_current_with(-1);
    }

    ps2::init();
    // Num Lock is on from boot: its LED says so.
    ps2::set_keyboard_leds(if num_lock() { 0b010 } else { 0 });

    let mut line = match super::irq::IrqLine::attach(crate::apic::VECTOR_KEYBOARD) {
        Some(line) => line,
        None => {
            super::failed(handle, "table d'attente d'interruption pleine");
            crate::scheduler::exit_current_with(-1)
        }
    };

    super::ready(handle, "8042 PS/2, jeu de scancodes 1, AZERTY", Some(line.vector()));
    crate::serial_println!("[drv-clavier] pret - canal \"{}\"", CHANNEL);

    let channel = ipc::find_channel(CHANNEL);

    loop {
        // The 200 ms deadline is not a latency compromise: a keystroke
        // wakes the task immediately. It bounds the case where the line
        // would be badly routed, and serves as a heartbeat to empty a
        // buffer the controller may have filled before the task existed.
        line.wait(200);
        ps2::drain();

        let mut produced = 0;
        while let Some(byte) = ps2::take_keyboard() {
            if let Some(event) = decode_byte(byte) {
                produced += 1;
                if let Some(id) = channel {
                    let message =
                        Message::new(crate::scheduler::current_pid(), MessageKind::Event, byte as u32, &event);
                    let _ = ipc::publish(id, message);
                }
            }
        }

        if produced != 0 {
            super::count_event(handle, produced);
        }
    }
}

//! Arbitration of the 8042 controller between keyboard and mouse.
//!
//! The two PS/2 devices share **a single data port**, 0x60. Bit 5 of the
//! status register says which of the two produced the byte available. Two
//! drivers reading 0x60 each on their own would therefore steal bytes
//! from each other: the keyboard would consume a third of a mouse packet,
//! the mouse would swallow a scancode, and the two streams would go out
//! of sync without ever catching up -- a mouse packet carries no reliable
//! start marker.
//!
//! This module is the answer: a single reader, under a lock, that empties
//! the controller buffer and files each byte in the queue of the device
//! that emitted it. The two drivers no longer touch the hardware, they
//! read their queue.
//!
//! It is the first place where the split into distinct drivers costs
//! something -- and where ignoring it would produce an intermittent bug
//! impossible to diagnose from either driver.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::port::{inb, outb};
use crate::vga::SpinLock;

const DATA: u16 = 0x60;
const STATUS: u16 = 0x64;
const COMMAND: u16 = 0x64;

/// Bit 0 of the status register: a byte is waiting to be read.
const STATUS_OUTPUT_FULL: u8 = 1 << 0;
/// Bit 1: the controller has not yet consumed what was written to it.
const STATUS_INPUT_FULL: u8 = 1 << 1;
/// Bit 5: the available byte comes from the auxiliary port, hence the mouse.
const STATUS_FROM_AUX: u8 = 1 << 5;

/// Controller commands.
const CMD_DISABLE_PORT1: u8 = 0xAD;
const CMD_ENABLE_PORT1: u8 = 0xAE;
const CMD_DISABLE_PORT2: u8 = 0xA7;
const CMD_ENABLE_PORT2: u8 = 0xA8;
const CMD_READ_CONFIG: u8 = 0x20;
const CMD_WRITE_CONFIG: u8 = 0x60;
/// Prefix: the next byte written to 0x60 goes to the mouse.
const CMD_TO_MOUSE: u8 = 0xD4;

/// Bits of the configuration byte.
const CONFIG_IRQ_PORT1: u8 = 1 << 0;
const CONFIG_IRQ_PORT2: u8 = 1 << 1;

/// Mouse device commands.
const MOUSE_SET_DEFAULTS: u8 = 0xF6;
const MOUSE_ENABLE_REPORT: u8 = 0xF4;
const MOUSE_ACK: u8 = 0xFA;

/// Number of polling rounds before giving a wait up.
///
/// The 8042 controller answers in a few tens of microseconds, and one
/// access to port 0x64 costs about one microsecond -- the bound therefore
/// represents several milliseconds of real waiting, amply enough to let a
/// working controller answer.
///
/// The value is a compromise in both directions. Too low, a slow mouse
/// would never be detected. Too high, the wait is paid **with interrupts
/// masked** -- the controller lock masks them -- and a machine with no
/// PS/2 mouse would lose clock ticks on every attempt. Ten thousand
/// rounds hold both ends.
const SPIN_LIMIT: u32 = 10_000;

/// Depth of the queues. An overflow loses the **oldest** byte: a scancode
/// several seconds old interests nobody any more, whereas the most recent
/// is the one the user has just typed.
const QUEUE_LEN: usize = 64;

struct Queue {
    bytes: [u8; QUEUE_LEN],
    head: usize,
    len: usize,
    dropped: u64,
}

impl Queue {
    const fn new() -> Self {
        Queue {
            bytes: [0; QUEUE_LEN],
            head: 0,
            len: 0,
            dropped: 0,
        }
    }

    fn push(&mut self, value: u8) {
        if self.len == QUEUE_LEN {
            self.head = (self.head + 1) % QUEUE_LEN;
            self.len -= 1;
            self.dropped += 1;
        }
        let tail = (self.head + self.len) % QUEUE_LEN;
        self.bytes[tail] = value;
        self.len += 1;
    }

    fn pop(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let value = self.bytes[self.head];
        self.head = (self.head + 1) % QUEUE_LEN;
        self.len -= 1;
        Some(value)
    }
}

struct Controller {
    keyboard: Queue,
    mouse: Queue,
}

static CONTROLLER: SpinLock<Controller> = SpinLock::new(Controller {
    keyboard: Queue::new(),
    mouse: Queue::new(),
});

/// Initialisation happens once only, whichever driver arrives first.
static INITIALISED: AtomicBool = AtomicBool::new(false);
static MOUSE_PRESENT: AtomicBool = AtomicBool::new(false);

fn wait_writable() -> bool {
    for _ in 0..SPIN_LIMIT {
        if unsafe { inb(STATUS) } & STATUS_INPUT_FULL == 0 {
            return true;
        }
    }
    false
}

fn wait_readable() -> bool {
    for _ in 0..SPIN_LIMIT {
        if unsafe { inb(STATUS) } & STATUS_OUTPUT_FULL != 0 {
            return true;
        }
    }
    false
}

fn command(value: u8) -> bool {
    if !wait_writable() {
        return false;
    }
    unsafe { outb(COMMAND, value) };
    true
}

fn write_data(value: u8) -> bool {
    if !wait_writable() {
        return false;
    }
    unsafe { outb(DATA, value) };
    true
}

fn read_data() -> Option<u8> {
    if !wait_readable() {
        return None;
    }
    Some(unsafe { inb(DATA) })
}

/// Sends a command to the mouse and waits for its acknowledgement.
fn mouse_command(value: u8) -> bool {
    if !command(CMD_TO_MOUSE) || !write_data(value) {
        return false;
    }
    // The mouse may precede its ACK with residual bytes; we tolerate a
    // few rather than fail on the first one that is not 0xFA.
    for _ in 0..4 {
        match read_data() {
            Some(MOUSE_ACK) => return true,
            Some(_) => continue,
            None => return false,
        }
    }
    false
}

/// Initialises the controller for both devices.
///
/// Runs with the ports disabled: while they are, no byte arrives, and the
/// command sequence is not cut by a keystroke. The interrupts are armed
/// only at the very end, when both queues are ready to receive.
///
/// Returns `true` if a mouse answered.
pub fn init() -> bool {
    if INITIALISED.swap(true, Ordering::SeqCst) {
        return MOUSE_PRESENT.load(Ordering::Relaxed);
    }

    let _guard = CONTROLLER.lock();

    command(CMD_DISABLE_PORT1);
    command(CMD_DISABLE_PORT2);

    // Empties what the firmware may have left: one forgotten byte would
    // shift the first mouse packet.
    for _ in 0..16 {
        if unsafe { inb(STATUS) } & STATUS_OUTPUT_FULL == 0 {
            break;
        }
        let _ = unsafe { inb(DATA) };
    }

    command(CMD_ENABLE_PORT2);

    let mouse = mouse_command(MOUSE_SET_DEFAULTS) && mouse_command(MOUSE_ENABLE_REPORT);
    MOUSE_PRESENT.store(mouse, Ordering::Relaxed);

    // Configuration byte: we arm both interrupts without touching the
    // other bits, among them the scancode set translation the firmware
    // may have enabled.
    if command(CMD_READ_CONFIG)
        && let Some(config) = read_data()
    {
        let mut value = config | CONFIG_IRQ_PORT1;
        if mouse {
            value |= CONFIG_IRQ_PORT2;
        }
        if command(CMD_WRITE_CONFIG) {
            write_data(value);
        }
    }

    command(CMD_ENABLE_PORT1);
    if mouse {
        command(CMD_ENABLE_PORT2);
    }

    mouse
}

/// Did a mouse answer at initialisation?
pub fn mouse_present() -> bool {
    MOUSE_PRESENT.load(Ordering::Relaxed)
}

/// Empties the controller buffer into the two queues.
///
/// Returns (keyboard bytes, mouse bytes) filed during this pass.
pub fn drain() -> (usize, usize) {
    let mut controller = CONTROLLER.lock();
    let mut from_keyboard = 0;
    let mut from_mouse = 0;

    // Bounds the number of bytes handled in one pass: a faulty device can
    // keep the buffer full indefinitely, and the driver task must keep
    // the ability to hand back.
    for _ in 0..(2 * QUEUE_LEN) {
        let status = unsafe { inb(STATUS) };
        if status & STATUS_OUTPUT_FULL == 0 {
            break;
        }
        let byte = unsafe { inb(DATA) };
        if status & STATUS_FROM_AUX != 0 {
            controller.mouse.push(byte);
            from_mouse += 1;
        } else {
            controller.keyboard.push(byte);
            from_keyboard += 1;
        }
    }

    (from_keyboard, from_mouse)
}

/// Lights the keyboard's LEDs: bit 0 Scroll Lock, bit 1 Num Lock, bit 2
/// Caps Lock (command 0xED, then the mask). The keyboard answers each
/// byte with 0xFA, which arrives in the keyboard queue: the decoder drops
/// acknowledgements.
pub fn set_keyboard_leds(mask: u8) {
    let _guard = CONTROLLER.lock();
    if write_data(0xED) {
        // The keyboard takes the mask once it has acknowledged the
        // command; the acknowledgement itself is drained by the IRQ path.
        for _ in 0..1000 {
            core::hint::spin_loop();
        }
        write_data(mask & 0x07);
    }
}

/// Removes a byte from the keyboard queue.
pub fn take_keyboard() -> Option<u8> {
    CONTROLLER.lock().keyboard.pop()
}

/// Removes a byte from the mouse queue.
pub fn take_mouse() -> Option<u8> {
    CONTROLLER.lock().mouse.pop()
}

/// Bytes lost to overflow: (keyboard, mouse).
pub fn dropped() -> (u64, u64) {
    let controller = CONTROLLER.lock();
    (controller.keyboard.dropped, controller.mouse.dropped)
}

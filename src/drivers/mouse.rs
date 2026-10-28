//! 6.1.2 -- PS/2 mouse driver.
//!
//! ## The packet framing problem
//!
//! The mouse emits three-byte packets, and **none of them carries a start
//! marker**. The only hint is bit 3 of the first byte, always set by the
//! protocol: if the byte received at position 0 does not have it, we are
//! reading in the middle of a packet. Resynchronising then means throwing
//! that byte away and starting again -- failing which a single lost byte
//! shifts the stream **for good**, and the mouse goes diagonal forever.
//!
//! It is also why the keyboard and the mouse go through the `ps2`
//! arbiter: a mouse byte consumed by the keyboard driver would produce
//! exactly that shift.
//!
//! ## Overflow
//!
//! Bits 6 and 7 signal that the displacement exceeded what a signed byte
//! can carry. The value returned is then wrong, not approximate -- so the
//! packet is ignored. A very fast movement loses a frame rather than
//! making the cursor jump to the opposite side of the screen.
//!
//! ## Coordinates
//!
//! The mouse returns relative displacements only; the absolute position
//! is held here, bounded by a surface. The default value, 1024 by 768,
//! holds only while nothing better is known: since step 7.1, the driver
//! task lays down the real framebuffer resolution through `set_surface`
//! as soon as a graphics mode is active. The compositor of 8.1 will use
//! it the same way when it changes mode.
//!
//! ## Event format
//!
//! Published on `dev.mouse`, twelve bytes:
//!
//! ```text
//!   0x00  1  kind: 2 = mouse
//!   0x01  1  buttons: bit 0 left, bit 1 right, bit 2 middle
//!   0x02  2  absolute x
//!   0x04  2  absolute y
//!   0x06  2  x displacement, signed integer
//!   0x08  2  y displacement, signed integer
//!   0x0A  2  reserved
//! ```

use crate::ipc::{self, Message, MessageKind};
use crate::vga::SpinLock;

use super::{DriverClass, ps2};

pub const CHANNEL: &str = "dev.mouse";
pub const EVENT_SIZE: usize = 12;

/// IRQ of the auxiliary port of the 8042.
const MOUSE_IRQ: u8 = 12;

/// Bit always set in the first byte of a packet.
const PACKET_SYNC: u8 = 1 << 3;
const PACKET_X_SIGN: u8 = 1 << 4;
const PACKET_Y_SIGN: u8 = 1 << 5;
const PACKET_X_OVERFLOW: u8 = 1 << 6;
const PACKET_Y_OVERFLOW: u8 = 1 << 7;

struct Pointer {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    buttons: u8,
    /// Bytes already gathered of the packet in progress.
    packet: [u8; 3],
    filled: usize,
    /// Packets dropped for want of framing or for overflow.
    discarded: u64,
}

static POINTER: SpinLock<Pointer> = SpinLock::new(Pointer {
    x: 512,
    y: 384,
    width: 1024,
    height: 768,
    buttons: 0,
    packet: [0; 3],
    filled: 0,
    discarded: 0,
});

/// Current position and buttons: (x, y, buttons).
pub fn position() -> (i32, i32, u8) {
    let pointer = POINTER.lock();
    (pointer.x, pointer.y, pointer.buttons)
}

/// Dimensions of the surface the cursor is bounded to.
pub fn surface() -> (i32, i32) {
    let pointer = POINTER.lock();
    (pointer.width, pointer.height)
}

/// Redefines the surface the cursor is bounded to.
///
/// The compositor will call it with the real framebuffer resolution.
pub fn set_surface(width: i32, height: i32) {
    let mut pointer = POINTER.lock();
    pointer.width = width.max(1);
    pointer.height = height.max(1);
    pointer.x = pointer.x.clamp(0, pointer.width - 1);
    pointer.y = pointer.y.clamp(0, pointer.height - 1);
}

/// Number of packets dropped -- framing lost or overflow.
pub fn discarded() -> u64 {
    POINTER.lock().discarded
}

// ============================================================
//  Entries common to both buses
// ============================================================

/// Builds the event from the current state of the pointer.
///
/// Extracted from `decode_byte` so that USB produces exactly the same
/// format: the compositor must not be able to tell a PS/2 movement from
/// a USB one, failing which two routing paths would be needed for a
/// single notion of pointer.
fn build_event(pointer: &Pointer, dx: i32, dy: i32, wheel: i8) -> [u8; EVENT_SIZE] {
    let mut event = [0u8; EVENT_SIZE];
    event[0] = 2;
    event[1] = pointer.buttons;
    event[0x02..0x04].copy_from_slice(&(pointer.x as u16).to_le_bytes());
    event[0x04..0x06].copy_from_slice(&(pointer.y as u16).to_le_bytes());
    event[0x06..0x08].copy_from_slice(&(dx as i16).to_le_bytes());
    event[0x08..0x0A].copy_from_slice(&(dy as i16).to_le_bytes());
    // 0x0A is the cursor shape, stamped by the compositor.
    event[0x0B] = wheel as u8;
    event
}

/// Publishes an event on `dev.mouse`, if the channel exists.
///
/// The pointer lock is **already released** when this function is called:
/// `ipc::publish` takes that of the channels and may wake the compositor.
fn publish(event: [u8; EVENT_SIZE]) -> bool {
    let Some(id) = ipc::find_channel(CHANNEL) else {
        return false;
    };
    let message = Message::new(
        crate::scheduler::current_pid(),
        MessageKind::Event,
        event[1] as u32,
        &event,
    );
    ipc::publish(id, message).is_ok()
}

/// Applies a relative displacement coming from a bus other than PS/2.
///
/// Returns `true` if something changed and was published. A report that
/// moves nothing and changes no button is not published: a USB mouse
/// emits at a fixed rate, and republishing an identical position would
/// repaint the cursor for nothing.
pub fn apply_relative(dx: i32, dy: i32, buttons: u8, wheel: i8) -> bool {
    let event = {
        let mut pointer = POINTER.lock();
        let before = (pointer.x, pointer.y, pointer.buttons);

        // The vertical axis of a HID mouse points down, like that of the
        // screen: unlike PS/2, there is no conversion.
        pointer.x = (pointer.x + dx).clamp(0, pointer.width - 1);
        pointer.y = (pointer.y + dy).clamp(0, pointer.height - 1);
        pointer.buttons = buttons & 0b111;

        // A wheel notch moves nothing and must still be published.
        if (pointer.x, pointer.y, pointer.buttons) == before && wheel == 0 {
            return false;
        }
        build_event(&pointer, dx, dy, wheel)
    };

    publish(event)
}

/// Applies an absolute position coming from a USB tablet.
///
/// The coordinates are on the logical scale of the device, not in pixels:
/// the conversion happens here because this is where the surface is
/// known, and it changes when the graphics mode changes.
pub fn set_absolute(x: u32, y: u32, buttons: u8, wheel: i8) -> bool {
    let event = {
        let mut pointer = POINTER.lock();
        let before = (pointer.x, pointer.y, pointer.buttons);

        let new_x = crate::usb::hid::to_screen(x, pointer.width);
        let new_y = crate::usb::hid::to_screen(y, pointer.height);
        let dx = new_x - pointer.x;
        let dy = new_y - pointer.y;

        pointer.x = new_x;
        pointer.y = new_y;
        pointer.buttons = buttons & 0b111;

        // A wheel notch moves nothing and must still be published.
        if (pointer.x, pointer.y, pointer.buttons) == before && wheel == 0 {
            return false;
        }
        build_event(&pointer, dx, dy, wheel)
    };

    publish(event)
}

/// Accumulates a byte. Returns the event when a packet is complete.
///
/// Public for the same reason as its keyboard counterpart: packet framing
/// is the delicate part of this driver, and the only way to exercise it
/// is to submit a chosen sequence to it.
pub fn decode_byte(byte: u8) -> Option<[u8; EVENT_SIZE]> {
    let mut pointer = POINTER.lock();

    // Framing: the first byte of a packet always has bit 3 set.
    if pointer.filled == 0 && byte & PACKET_SYNC == 0 {
        pointer.discarded += 1;
        return None;
    }

    let index = pointer.filled;
    pointer.packet[index] = byte;
    pointer.filled += 1;
    if pointer.filled < 3 {
        return None;
    }
    pointer.filled = 0;

    let flags = pointer.packet[0];
    if flags & (PACKET_X_OVERFLOW | PACKET_Y_OVERFLOW) != 0 {
        pointer.discarded += 1;
        return None;
    }

    // The displacement is a nine-bit signed integer: eight in the byte,
    // the ninth in the sign bit of the first byte. Rebuilding it by sign
    // extension on the byte alone would be wrong for any displacement of
    // more than 127 units.
    let mut dx = pointer.packet[1] as i32;
    let mut dy = pointer.packet[2] as i32;
    if flags & PACKET_X_SIGN != 0 {
        dx -= 256;
    }
    if flags & PACKET_Y_SIGN != 0 {
        dy -= 256;
    }

    // The vertical axis of the mouse points up, that of a screen points
    // down: the subtraction is not a choice, it is the conversion between
    // the two conventions.
    pointer.x = (pointer.x + dx).clamp(0, pointer.width - 1);
    pointer.y = (pointer.y - dy).clamp(0, pointer.height - 1);
    pointer.buttons = flags & 0b111;

    let mut event = [0u8; EVENT_SIZE];
    event[0] = 2;
    event[1] = pointer.buttons;
    event[0x02..0x04].copy_from_slice(&(pointer.x as u16).to_le_bytes());
    event[0x04..0x06].copy_from_slice(&(pointer.y as u16).to_le_bytes());
    event[0x06..0x08].copy_from_slice(&(dx as i16).to_le_bytes());
    event[0x08..0x0A].copy_from_slice(&((-dy) as i16).to_le_bytes());

    Some(event)
}

/// Driver task.
pub fn task() -> ! {
    let handle = match super::register("souris", DriverClass::Input, CHANNEL) {
        Some(handle) => handle,
        None => crate::scheduler::exit_current_with(-1),
    };

    // --- USB first ---
    //
    // The enumeration is tried before PS/2 for a reason of diagnostics:
    // it logs what it finds, and knowing that an absolute tablet answers
    // changes the reading of everything that follows. The two buses then
    // coexist; neither is exclusive.
    let usb_pointers = unsafe { crate::usb::init() };

    // The keyboard initialises the same controller; `ps2::init` is
    // idempotent and returns the mouse answer, whichever driver arrives
    // first.
    let ps2_present = ps2::init();

    // The driver is absent only if **neither** of the two buses answers.
    // Declaring absence because PS/2 is silent would deprive the system
    // of a perfectly working USB mouse -- exactly the case of a modern
    // machine, which has no PS/2 port at all.
    if !ps2_present && usb_pointers == 0 {
        super::absent(handle, "aucun peripherique de pointage, ni PS/2 ni USB");
        crate::serial_println!("[drv-souris] absente - aucun bus ne repond");
        crate::scheduler::exit_current_with(0);
    }

    if ipc::open_channel(CHANNEL).is_err() {
        super::failed(handle, "canal deja ouvert");
        crate::scheduler::exit_current_with(-1);
    }

    let vector = super::VECTOR_DEVICE_BASE + MOUSE_IRQ;
    crate::ioapic::set_irq(MOUSE_IRQ, vector, crate::apic::id() as u8);
    crate::ioapic::unmask(MOUSE_IRQ);

    let mut line = match super::irq::IrqLine::attach(vector) {
        Some(line) => line,
        None => {
            super::failed(handle, "table d'attente d'interruption pleine");
            crate::scheduler::exit_current_with(-1)
        }
    };

    // 7.1: the cursor is bounded to the real screen, not to a guess.
    // Without that, the right half of a 1280 screen would be unreachable
    // and an 800 screen would let the cursor leave the framebuffer.
    let surface = match crate::graphics::resolution() {
        Some((width, height)) => {
            set_surface(width as i32, height as i32);
            alloc::format!("{}x{} (framebuffer)", width, height)
        }
        None => {
            let pointer = POINTER.lock();
            alloc::format!("{}x{} (par defaut)", pointer.width, pointer.height)
        }
    };

    let description = match (ps2_present, usb_pointers) {
        (true, 0) => "PS/2 trois boutons",
        (false, _) => "USB HID",
        (true, _) => "PS/2 et USB HID",
    };
    super::ready(handle, description, Some(vector));
    crate::serial_println!(
        "[drv-souris] prete - canal \"{}\", {} ({} pointage USB), surface {}",
        CHANNEL,
        description,
        usb_pointers,
        surface
    );

    let channel = ipc::find_channel(CHANNEL);

    // Two waiting regimes, because the two buses do not signal the same
    // way. PS/2 raises IRQ 12: the task can sleep until then. USB has no
    // configured interrupt -- an MSI-X table would be needed -- and is
    // read by polling its event ring.
    //
    // The wait deadline is therefore short when a USB pointer answers,
    // long otherwise. Eight milliseconds match the interval a USB mouse
    // declares: polling faster would return nothing more.
    let wait_ms = if usb_pointers > 0 { 8 } else { 200 };

    loop {
        line.wait(wait_ms);

        let mut produced = 0;

        // --- PS/2 ---
        if ps2_present {
            ps2::drain();
            while let Some(byte) = ps2::take_mouse() {
                if let Some(event) = decode_byte(byte) {
                    produced += 1;
                    if let Some(id) = channel {
                        let message = Message::new(
                            crate::scheduler::current_pid(),
                            MessageKind::Event,
                            event[1] as u32,
                            &event,
                        );
                        let _ = ipc::publish(id, message);
                    }
                }
            }
        }

        // --- USB ---
        //
        // `poll` publishes on the channel itself: it knows the shape of
        // the report, and making it return raw bytes would force this
        // driver to re-decode what the stack has already decoded.
        if usb_pointers > 0 {
            produced += crate::usb::poll() as u64;
        }

        if produced != 0 {
            super::count_event(handle, produced);
        }
    }
}

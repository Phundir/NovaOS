//! 6.1.5 -- AC'97 audio driver: output and input.
//!
//! ## Why AC'97 and not HDA
//!
//! Both exist under QEMU. AC'97 is driven through some thirty registers
//! in I/O space and a buffer descriptor list; HDA demands a command ring
//! to the codec, a verb protocol, and the discovery of the widget graph
//! -- work out of all proportion for the same result. AC'97 is therefore
//! the choice, and the HDA controllers present are declared not driven.
//!
//! ## The descriptor ring, and a defect of the first version
//!
//! Each box -- PCM out, PCM in -- reads a list of **thirty-two**
//! descriptors, and its current index `CIV` runs modulo 32. The first
//! version wrote four descriptors and published `LVI` in 0..3: past the
//! fourth buffer the controller either stopped, with a gap in the sound,
//! or walked into twenty-eight descriptors nobody had written. Nobody had
//! heard it -- the machine ran with `-audiodev none` -- so nobody noticed.
//!
//! Now all thirty-two descriptors are written once, descriptor `d`
//! pointing at buffer `d % 4`. The four buffers turn eight times around
//! the ring, `CIV % 4` names the buffer in use, and `LVI` runs modulo 32
//! like `CIV` does. `ahead(from, to)` is the one piece of arithmetic both
//! directions share.
//!
//! ## Output
//!
//! The mixer fills buffer `next % 4` whenever fewer than four descriptors
//! are queued ahead of the one being played; `LVI` then names it. When the
//! mixer has nothing left, nothing more is queued, and the box is stopped
//! once the controller reports it has played the last one (`DCH`): the
//! tail of a sound is played, not cut.
//!
//! A buffer is only mixed from a stream holding a whole buffer's worth,
//! unless the ring is about to run dry: a stream mixed short is padded
//! with silence **in the middle of its sound**, which is a click. The
//! same rule primes the ring at the start (see `feed_output`).
//!
//! ## Input
//!
//! The capture box writes buffers; the driver reads each one the
//! controller has finished, hands it to `audio::capture`, and gives the
//! buffer back by moving `LVI` four descriptors past it. The DMA runs only
//! while somebody reads or watches the input.
//!
//! ## Settings the codec gets
//!
//! * master and PCM out at 0 dB -- the volume is applied by the mixer, in
//!   software, so that it works whatever the backend;
//! * record gain at its **maximum**, `0x0F0F`, and not muted. Its reset
//!   value is `0x8000`, muted. The first version wrote `0x0000`, which the
//!   AC'97 specification reads as 0 dB -- and which QEMU reads as **zero
//!   volume**: its codec maps the record gain linearly, `255 * reg / 15`,
//!   with no inversion (the inversion is only for the output
//!   attenuations). So the capture ran, the DMA moved, and every sample
//!   was silence: "the microphone is not detected". At `0x0F0F` QEMU
//!   passes the input at full volume; on a real codec it is +22.5 dB,
//!   which the software gain of `audio::capture` brings back down --
//!   that gain is the one the settings expose;
//! * record select: microphone or line in, both channels.
//!
//! ## What is not verified
//!
//! **Nobody has heard this driver, nor recorded with it**, until a run
//! with a real audio backend says otherwise. The ring arithmetic is
//! checked by the self-test; the sound is checked by a person.
//!
//! ## Protocol of the `dev.audio` channel
//!
//! ```text
//!   1 STATE   -> 4 bytes: controller present, transfer in progress,
//!                         streams open, mute
//!   2 MASTER  -> 5 bytes: master volume (u32) then mute
//!   3 SET     <- bytes 1..5: master volume to set (u32)
//!             -> 1 byte: accepted
//! ```

use alloc::vec::Vec;

use crate::ipc::{self, Message, MessageKind};
use crate::port::{inb, inw, outb, outl, outw};
use crate::vga::SpinLock;

use super::DriverClass;

pub const CHANNEL: &str = "dev.audio";

/// Operations of the `dev.audio` channel, first byte of the payload.
pub const OP_STATE: u8 = 1;
pub const OP_MASTER: u8 = 2;
pub const OP_SET_MASTER: u8 = 3;

/// Identifiers of the AC'97 controller emulated by QEMU.
const VENDOR_INTEL: u16 = 0x8086;
const DEVICE_AC97: u16 = 0x2415;

/// PCI class of a multimedia device, HDA audio subclass.
const PCI_CLASS_MULTIMEDIA: u8 = 0x04;
const PCI_SUBCLASS_HDA: u8 = 0x03;

/// Mixer registers (BAR 0).
mod nam {
    pub const RESET: u16 = 0x00;
    pub const MASTER_VOLUME: u16 = 0x02;
    pub const MIC_VOLUME: u16 = 0x0E;
    pub const LINE_IN_VOLUME: u16 = 0x10;
    pub const PCM_VOLUME: u16 = 0x18;
    pub const RECORD_SELECT: u16 = 0x1A;
    pub const RECORD_GAIN: u16 = 0x1C;

    /// Record select values, one per channel byte.
    pub const SELECT_MIC: u16 = 0;
    pub const SELECT_LINE: u16 = 4;
    /// 0 dB on a volume register of the mixer: attenuation zero, gain
    /// eight steps of 1.5 dB for the input registers, which count from
    /// +12 dB.
    pub const INPUT_UNITY: u16 = 0x0808;
    /// Record gain at its top step on both channels, unmuted. See the
    /// module header for why not zero.
    pub const RECORD_GAIN_FULL: u16 = 0x0F0F;
}

/// Bus master registers (BAR 1): one box per direction, same layout.
mod nabm {
    /// Base of the PCM in box.
    pub const PCM_IN: u16 = 0x00;
    /// Base of the PCM out box.
    pub const PCM_OUT: u16 = 0x10;

    // Offsets inside a box.
    pub const BDBAR: u16 = 0x00;
    pub const CIV: u16 = 0x04;
    pub const LVI: u16 = 0x05;
    pub const SR: u16 = 0x06;
    pub const PICB: u16 = 0x08;
    pub const CR: u16 = 0x0B;

    pub const GLOB_CNT: u16 = 0x2C;

    /// Start the transfer.
    pub const CR_RPBM: u8 = 1 << 0;
    /// Reset the registers of the box.
    pub const CR_RESET: u8 = 1 << 1;
    /// DMA controller halted: it reached `LVI` and has finished it.
    pub const SR_DCH: u16 = 1 << 0;
    /// Write-one-to-clear status bits.
    pub const SR_CLEAR: u16 = 0x1C;
    /// Bit 1 of GLOB_CNT: leave the cold state.
    pub const GLOB_COLD_RESET: u32 = 1 << 1;
}

/// Descriptors in a ring, fixed by the controller.
pub const RING: u8 = 32;

/// Buffers per direction.
pub const BUFFERS: u8 = 4;

/// Samples per buffer. 2048 stereo samples, that is 1024 frames: about
/// twenty-one milliseconds at 48 kHz, and exactly one 4 KiB frame.
const SAMPLES_PER_BUFFER: usize = 2048;

/// A descriptor is eight bytes; the list must be eight-byte aligned.
const DESCRIPTOR_SIZE: u64 = 8;

/// Bit 15 of the flags of a descriptor: interrupt at the end of the buffer.
const DESCRIPTOR_IOC: u16 = 1 << 15;

/// Descriptors from `from` up to, not including, `to`, on the ring.
pub fn ahead(from: u8, to: u8) -> u8 {
    to.wrapping_sub(from) % RING
}

/// One direction of the controller.
struct DmaBox {
    /// I/O base of the box registers.
    base: u16,
    list: u64,
    buffers: [u64; BUFFERS as usize],
    /// Output: next descriptor to fill. Input: next descriptor to read.
    next: u8,
    running: bool,
}

impl DmaBox {
    fn civ(&self) -> u8 {
        unsafe { inb(self.base + nabm::CIV) }
    }

    fn halted(&self) -> bool {
        let status = unsafe { inw(self.base + nabm::SR) };
        status & nabm::SR_DCH != 0
    }

    fn set_lvi(&self, index: u8) {
        unsafe { outb(self.base + nabm::LVI, index % RING) };
    }

    fn buffer(&self, descriptor: u8) -> *mut i16 {
        crate::memory::phys_to_virt(self.buffers[(descriptor % BUFFERS) as usize]) as *mut i16
    }

    /// Writes the 32 descriptors once: `d` points at buffer `d % 4`.
    fn write_ring(&self) {
        for descriptor in 0..RING {
            let entry = crate::memory::phys_to_virt(self.list) + descriptor as u64 * DESCRIPTOR_SIZE;
            let buffer = self.buffers[(descriptor % BUFFERS) as usize];
            unsafe {
                (entry as *mut u32).write_volatile(buffer as u32);
                ((entry + 4) as *mut u16).write_volatile(SAMPLES_PER_BUFFER as u16);
                ((entry + 6) as *mut u16).write_volatile(DESCRIPTOR_IOC);
            }
        }
    }

    /// Starts the box with `last` as the last valid descriptor.
    fn start(&mut self, last: u8) {
        unsafe {
            outl(self.base + nabm::BDBAR, self.list as u32);
            outb(self.base + nabm::LVI, last % RING);
            outb(self.base + nabm::CR, nabm::CR_RPBM);
        }
        self.running = true;
    }

    fn stop(&mut self) {
        unsafe {
            outb(self.base + nabm::CR, 0);
            // The reset bit clears itself once the box has been
            // flattened; waiting for it avoids relaunching a transfer on
            // a half-erased state.
            outb(self.base + nabm::CR, nabm::CR_RESET);
            for _ in 0..100_000 {
                if inb(self.base + nabm::CR) & nabm::CR_RESET == 0 {
                    break;
                }
                core::hint::spin_loop();
            }
            outw(self.base + nabm::SR, nabm::SR_CLEAR);
        }
        self.running = false;
        self.next = 0;
    }
}

struct Ac97 {
    mixer: u16,
    output: DmaBox,
    input: DmaBox,
    /// Working buffer, allocated once: see the history in `fill`.
    scratch: Vec<i16>,
    /// The playback position, smoothed (see `output_delay_frames`).
    clock: PlaybackClock,
}

/// Frames the output DMA has consumed since the box started, as a clock
/// that moves smoothly.
///
/// An emulated controller does not consume sample by sample: QEMU's audio
/// timer hands the host 10 ms at a time, and the position read from `CIV`
/// and `PICB` jumps by as much. A video player timed on that clock shows
/// its frames on a 10 ms grid that beats against the 60 Hz of the screen:
/// one frame in eight or so waited one refresh too long and the next one
/// too short (17 then 50 ms instead of 33 and 33). Between two jumps the
/// position is extrapolated at the nominal rate, by one jump at most --
/// a real card's pointer is as smooth as its crystal -- and it never
/// goes back, whatever the next reading says.
#[derive(Default)]
struct PlaybackClock {
    /// Frames written into the ring since the start.
    queued: u64,
    /// Last consumed count read from the registers, and when it changed.
    raw: u64,
    raw_since: u64,
    /// Last value handed out.
    reported: u64,
}

/// Largest extrapolation: one period of QEMU's audio timer (10 ms), or of
/// a host sound server's (up to 1024 frames); beyond, the DMA has stalled.
const CLOCK_EXTRAPOLATION_FRAMES: u64 = 1024;

static DEVICE: SpinLock<Option<Ac97>> = SpinLock::new(None);

/// Is a controller driven?
/// Times the output DMA ran dry while sound was waiting (see `feed_output`).
static STARVED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

pub fn starved() -> u64 {
    STARVED.load(core::sync::atomic::Ordering::Relaxed)
}

pub fn is_present() -> bool {
    DEVICE.lock().is_some()
}

/// Frames already handed to the controller and not yet played: the rest
/// of the buffer being read (`PICB` counts its samples left) and the
/// buffers queued after it, up to `LVI`.
///
/// It is the depth of the hardware queue that a player must add to its
/// own to know when a sample written now will be heard -- and, read at
/// any moment, it moves sample by sample, where the mixer's queue moves a
/// whole buffer (21 ms) at a time.
pub fn output_delay_frames() -> u32 {
    let mut device = DEVICE.lock();
    let Some(controller) = device.as_mut() else {
        return 0;
    };
    let output = &controller.output;
    if !output.running || output.halted() {
        return 0;
    }
    // SAFETY: registers of the box this driver owns; reads have no effect.
    let (civ, lvi, picb) = unsafe {
        (inb(output.base + nabm::CIV), inb(output.base + nabm::LVI), inw(output.base + nabm::PICB))
    };
    let frames_per_buffer = (SAMPLES_PER_BUFFER / crate::audio::CHANNELS) as u64;
    let remaining = ahead(civ, lvi) as u64 * frames_per_buffer + picb as u64 / crate::audio::CHANNELS as u64;

    // The consumed count, smoothed between the controller's jumps (see
    // `PlaybackClock`).
    let clock = &mut controller.clock;
    let now = crate::random::rdtsc();
    let raw = clock.queued.saturating_sub(remaining);
    if raw != clock.raw {
        clock.raw = raw;
        clock.raw_since = now;
    }
    let per_ms = crate::posix::time::tsc_per_ms().max(1);
    // Frames at the nominal rate since the last jump; the elapsed time is
    // bounded first (100 ms) so that the product cannot overflow.
    let elapsed = now.wrapping_sub(clock.raw_since).min(per_ms * 100) * (crate::audio::SAMPLE_RATE as u64 / 1000) / per_ms;
    let estimate = (raw + elapsed.min(CLOCK_EXTRAPOLATION_FRAMES)).max(clock.reported).min(clock.queued);
    clock.reported = estimate;
    (clock.queued - estimate) as u32
}

/// Index of the buffer the output box is reading.
///
/// It is the only observable witness that a transfer really progresses,
/// and it is what the self-test watches.
pub fn current_buffer() -> Option<u8> {
    DEVICE.lock().as_ref().map(|device| device.output.civ())
}

/// Is the capture DMA running?
pub fn is_capturing() -> bool {
    DEVICE.lock().as_ref().is_some_and(|device| device.input.running)
}

/// Chooses what the capture box records.
///
/// Remembered by the codec itself; with no controller it is a no-op, and
/// the settings file still keeps the choice for the next machine.
pub fn set_input_source(source: crate::audio::settings::Source) {
    let select = match source {
        crate::audio::settings::Source::Microphone => nam::SELECT_MIC,
        crate::audio::settings::Source::Line => nam::SELECT_LINE,
    };
    if let Some(device) = DEVICE.lock().as_ref() {
        unsafe { outw(device.mixer + nam::RECORD_SELECT, select << 8 | select) };
    }
}

impl Ac97 {
    /// Fills one output descriptor's buffer from the mixer.
    ///
    /// `scratch` was once allocated here, a 4 KiB allocation and its free
    /// a hundred times a second while anything played; the screen reader
    /// of 10.1.2 plays continuously, and the cost became the whole machine.
    fn fill(&mut self, descriptor: u8) {
        if self.scratch.len() != SAMPLES_PER_BUFFER {
            self.scratch = alloc::vec![0i16; SAMPLES_PER_BUFFER];
        }
        crate::audio::mix_into(&mut self.scratch);
        self.clock.queued += (SAMPLES_PER_BUFFER / crate::audio::CHANNELS) as u64;
        let buffer = self.output.buffer(descriptor);
        for (position, sample) in self.scratch.iter().enumerate() {
            unsafe { buffer.add(position).write_volatile(*sample) };
        }
    }

    /// One lap of the output. Returns the buffers queued.
    fn feed_output(&mut self) -> usize {
        // The controller stopped on its last descriptor while it was
        // running: every queued buffer was played before the next was
        // written -- a gap in the sound, whatever the mixer holds.
        if self.output.running && self.output.halted() && crate::audio::has_audio() {
            STARVED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }
        if !crate::audio::has_audio() {
            // Nothing more to queue: let the controller play what is
            // queued, and stop the box only once it reports the end.
            if self.output.running && self.output.halted() {
                self.output.stop();
            }
            return 0;
        }

        if !self.output.running {
            self.clock = PlaybackClock::default();
            // Priming: the buffers the streams can fill **whole**, at
            // least one, then the start with `LVI` on the last of them;
            // the laps that follow add the rest. Priming all four came
            // first: a player starts with what its latency allows -- 40 ms
            // for Firefox's Web Audio, under two buffers -- so the second
            // buffer was half silence and the last two all of it: 64 ms of
            // silence right after the first note of every sound.
            let mut primed = 0;
            while primed < BUFFERS && (primed == 0 || crate::audio::streams_ready(SAMPLES_PER_BUFFER)) {
                self.fill(primed);
                primed += 1;
            }
            self.output.next = primed;
            self.output.start(primed - 1);
            crate::posix::audio::tick();
            return primed as usize;
        }

        // At most two buffers per lap. The loop runs every 10 ms and a
        // buffer lasts 21: two per lap is four times the consumption,
        // enough to catch up after a late wake-up, and it bounds the work
        // an emulated controller that "consumes" instantly can demand --
        // the phase 10 lesson, where filling every free buffer each lap
        // starved the rest of the machine.
        let mut queued = 0;
        while queued < 2 && ahead(self.output.civ(), self.output.next) < BUFFERS {
            // A stream short of a whole buffer would be mixed with silence
            // in the middle of its sound: a click. While the ring still
            // holds the playing buffer and two more (43 ms), the producer
            // has time to catch up, and waiting a lap costs nothing
            // audible; only a ring about to run dry takes the short buffer.
            if ahead(self.output.civ(), self.output.next) >= 3 && !crate::audio::streams_ready(SAMPLES_PER_BUFFER) {
                break;
            }
            let descriptor = self.output.next;
            self.fill(descriptor);
            // Moving `LVI` past a halted box restarts it: that is how an
            // underrun ends.
            self.output.set_lvi(descriptor);
            self.output.next = (descriptor + 1) % RING;
            queued += 1;
        }
        unsafe { outw(self.output.base + nabm::SR, nabm::SR_CLEAR) };
        if queued != 0 {
            // Room was made in the streams: their writers look again now
            // rather than at the next tick.
            crate::posix::audio::tick();
        }
        queued
    }

    /// One lap of the input. Returns the buffers read.
    fn drain_input(&mut self) -> usize {
        let wanted = crate::audio::capture::wanted();
        if !wanted {
            if self.input.running {
                self.input.stop();
            }
            return 0;
        }
        if !self.input.running {
            self.input.next = 0;
            self.input.start(BUFFERS - 1);
            return 0;
        }

        let civ = self.input.civ();
        let mut done = ahead(self.input.next, civ);
        // A halted box has finished the descriptor it stopped on too.
        if self.input.halted() {
            done += 1;
        }
        let done = done.min(BUFFERS);

        let mut block = Vec::with_capacity(SAMPLES_PER_BUFFER);
        for _ in 0..done {
            let descriptor = self.input.next;
            let buffer = self.input.buffer(descriptor);
            block.clear();
            for position in 0..SAMPLES_PER_BUFFER {
                block.push(unsafe { buffer.add(position).read_volatile() });
            }
            crate::audio::capture::push(&block);
            // The buffer is free again: it becomes the descriptor four
            // places on, and `LVI` moves there.
            self.input.set_lvi(descriptor.wrapping_add(BUFFERS));
            self.input.next = (descriptor + 1) % RING;
        }
        unsafe { outw(self.input.base + nabm::SR, nabm::SR_CLEAR) };
        done as usize
    }
}

/// AC'97 addresses DMA on **thirty-two bits** only: the list base register
/// and the address field of a descriptor are four bytes. A frame beyond
/// four gibibytes would be silently truncated and the controller would
/// read some arbitrary area of low memory.
fn low_frame(what: &'static str) -> Result<u64, &'static str> {
    let frame = crate::frame::allocate_zeroed_frame().ok_or(what)?;
    let address = frame.start_addr();
    if address + crate::frame::FRAME_SIZE > u32::MAX as u64 {
        return Err("cadre au-dela de la portee DMA du controleur AC'97");
    }
    Ok(address)
}

fn make_box(base: u16) -> Result<DmaBox, &'static str> {
    let list = low_frame("plus de cadre pour la liste")?;
    let mut buffers = [0u64; BUFFERS as usize];
    for slot in buffers.iter_mut() {
        *slot = low_frame("plus de cadre pour les tampons")?;
    }
    let mut result = DmaBox {
        base,
        list,
        buffers,
        next: 0,
        running: false,
    };
    result.write_ring();
    result.stop();
    Ok(result)
}

/// Looks for and initialises an AC'97 controller.
fn probe() -> Result<(), &'static str> {
    let device = crate::pci::enumerate()
        .into_iter()
        .find(|d| d.vendor_id == VENDOR_INTEL && d.device_id == DEVICE_AC97)
        .ok_or("aucun controleur AC'97 sur le bus PCI")?;

    unsafe { device.enable(crate::pci::command::IO_SPACE | crate::pci::command::BUS_MASTER) };

    // AC'97 lives in I/O space, not MMIO: both BARs carry bit 0, and
    // their useful value fits in sixteen bits.
    let mixer = (device.bar(0) & 0xFFFF) as u16;
    let bus = (device.bar(1) & 0xFFFF) as u16;
    if mixer == 0 || bus == 0 {
        return Err("BAR d'entree-sortie absent");
    }

    unsafe {
        // Leaving the cold state before anything else: the mixer
        // registers do not answer until the link with the codec is
        // established.
        outl(bus + nabm::GLOB_CNT, nabm::GLOB_COLD_RESET);
        for _ in 0..100_000 {
            core::hint::spin_loop();
        }

        // Any write to the reset register of the mixer returns it to its
        // default values, whatever the value written.
        outw(mixer + nam::RESET, 0);

        // Output volumes. The encoding is **attenuating**: zero is the
        // maximum, and bit 15 is the mute -- writing 0x8000 would give a
        // driver that looks correct and makes no sound at all.
        outw(mixer + nam::MASTER_VOLUME, 0x0000);
        outw(mixer + nam::PCM_VOLUME, 0x0000);

        // Input: sources at 0 dB and unmuted, record gain at 0 dB and
        // unmuted, microphone selected. See the module header for why
        // the gain register must be written.
        outw(mixer + nam::MIC_VOLUME, nam::INPUT_UNITY & 0x00FF);
        outw(mixer + nam::LINE_IN_VOLUME, nam::INPUT_UNITY);
        outw(mixer + nam::RECORD_GAIN, nam::RECORD_GAIN_FULL);
        outw(mixer + nam::RECORD_SELECT, nam::SELECT_MIC << 8 | nam::SELECT_MIC);

        let check = inw(mixer + nam::MASTER_VOLUME);
        if check == 0xFFFF {
            return Err("le codec ne repond pas");
        }
    }

    let output = make_box(bus + nabm::PCM_OUT)?;
    let input = make_box(bus + nabm::PCM_IN)?;

    crate::serial_println!(
        "[drv-audio] AC'97 - mixeur 0x{:04x}, maitre de bus 0x{:04x}, {} tampons de {} echantillons par sens, anneau de {}",
        mixer,
        bus,
        BUFFERS,
        SAMPLES_PER_BUFFER,
        RING
    );

    *DEVICE.lock() = Some(Ac97 {
        mixer,
        output,
        input,
        scratch: alloc::vec![0i16; SAMPLES_PER_BUFFER],
        clock: PlaybackClock::default(),
    });

    // The source chosen before the driver existed -- a session opened
    // early -- is applied now.
    set_input_source(crate::audio::settings::current().input_source);
    Ok(())
}

/// Answers a request of the `dev.audio` channel.
fn serve(payload: &[u8]) -> Vec<u8> {
    let operation = payload.first().copied().unwrap_or(0);
    let (master, muted) = crate::audio::master();

    match operation {
        OP_STATE => {
            let running = DEVICE.lock().as_ref().map(|d| d.output.running).unwrap_or(false);
            alloc::vec![
                u8::from(is_present()),
                u8::from(running),
                crate::audio::streams().len().min(255) as u8,
                u8::from(muted),
            ]
        }
        OP_MASTER => {
            let mut answer = Vec::with_capacity(5);
            answer.extend_from_slice(&master.to_le_bytes());
            answer.push(u8::from(muted));
            answer
        }
        OP_SET_MASTER => {
            if payload.len() < 5 {
                return Vec::new();
            }
            let volume = u32::from_le_bytes([payload[1], payload[2], payload[3], payload[4]]);
            crate::audio::set_master(volume);
            alloc::vec![1u8]
        }
        _ => Vec::new(),
    }
}

/// Driver task: feeds the output and drains the input.
pub fn task() -> ! {
    let handle = match super::register("audio", DriverClass::Audio, CHANNEL) {
        Some(handle) => handle,
        None => crate::scheduler::exit_current_with(-1),
    };

    // Same reason as for the network: the name must answer before the
    // probe, otherwise "no controller" and "no driver" are
    // indistinguishable from the client's point of view.
    if ipc::open_channel(CHANNEL).is_err() {
        super::failed(handle, "canal deja ouvert");
        crate::scheduler::exit_current_with(-1);
    }

    match probe() {
        Ok(()) => super::ready(handle, "Intel AC'97 - 48 kHz, stereo, 16 bits, lecture et capture", None),
        Err(reason) => {
            let hda = crate::pci::enumerate()
                .into_iter()
                .filter(|d| d.class == PCI_CLASS_MULTIMEDIA && d.subclass == PCI_SUBCLASS_HDA)
                .count();
            let detail = if hda != 0 {
                "controleur HDA present, non pilote"
            } else {
                reason
            };
            super::absent(handle, detail);
            crate::serial_println!("[drv-audio] {}", detail);
            crate::scheduler::exit_current_with(0);
        }
    }

    crate::serial_println!("[drv-audio] pret - canal \"{}\"", CHANNEL);

    loop {
        // The task cannot block on `recv`: it must come back and feed the
        // buffers. So the requests are drained with no waiting.
        while let Some(request) = ipc::try_recv() {
            if request.kind != MessageKind::Request {
                continue;
            }
            let answer = serve(&request.payload);
            let kind = if answer.is_empty() {
                MessageKind::Signal
            } else {
                MessageKind::Response
            };
            let reply =
                Message::new(crate::scheduler::current_pid(), kind, request.tag, &answer);
            if ipc::send(request.sender, reply).is_err() {
                super::count_error(handle);
            }
        }

        let moved = {
            let mut device = DEVICE.lock();
            match device.as_mut() {
                Some(controller) => controller.feed_output() + controller.drain_input(),
                None => 0,
            }
        };
        if moved != 0 {
            super::count_event(handle, moved as u64);
        }

        let _ = crate::syscall::invoke(
            crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
            [10, 0, 0, 0, 0, 0],
        );
    }
}

/// Logs the state of the controller.
pub fn dump_state() {
    let device = DEVICE.lock();
    let Some(controller) = device.as_ref() else {
        return;
    };
    let picb = unsafe { inw(controller.output.base + nabm::PICB) };
    // Read back from the codec rather than remembered: the only way to
    // establish that the mixer accepted what was written, and a mute
    // codec returns 0xFFFF.
    let attenuation = unsafe { inw(controller.mixer + nam::MASTER_VOLUME) };
    let gain = unsafe { inw(controller.mixer + nam::RECORD_GAIN) };
    let select = unsafe { inw(controller.mixer + nam::RECORD_SELECT) };
    crate::serial_println!(
        "[drv-audio] sortie : descripteur {} - {} echantillons restants - {} - attenuation 0x{:04x}",
        controller.output.civ(),
        picb,
        if controller.output.running { "actif" } else { "arrete" },
        attenuation
    );
    crate::serial_println!(
        "[drv-audio] entree : descripteur {} - {} - gain 0x{:04x}, source 0x{:04x}",
        controller.input.civ(),
        if controller.input.running { "active" } else { "arretee" },
        gain,
        select
    );
    let (captured, dropped, clipped) = crate::audio::capture::counters();
    crate::serial_println!(
        "[drv-audio] capture : {} echantillons, {} perdus, {} ecretes",
        captured,
        dropped,
        clipped
    );
}

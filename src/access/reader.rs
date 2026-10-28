//! 10.1.2 -- `screenreader.mod`: what is announced, and when.
//!
//! ## Two things, kept apart
//!
//! `speech` turns text into samples. This module decides **which text**,
//! and it is a different problem: a reader that announced everything the
//! moment it changed would talk over itself continuously, and a reader
//! that announced nothing until asked would be useless.
//!
//! Three rules settle it, and they are the whole policy:
//!
//! 1. **The focus is announced.** Moving to an element says what it is.
//!    That is the one announcement a keyboard user cannot do without.
//! 2. **A new announcement interrupts the previous one.** Pressing Tab
//!    four times fast must say the fourth element, not queue four. The
//!    queue holds one utterance, and a second replaces it.
//! 3. **A notification of PRIORITY level jumps the queue** and is not
//!    interrupted. 9 gives that level to what must not be missed, and
//!    silently dropping it would defeat the point.
//!
//! ## Why the synthesis runs in the reader task
//!
//! Rendering a five-word label at 48 kHz is around fifty thousand samples
//! and a hundred and fifty thousand fixed-point multiplies. Doing that on
//! the keystroke path -- inside `handle_key`, which the compositor calls
//! while holding its own lock -- would stall the render for as long as it
//! takes. So `say` **queues** and returns, and the task renders.
//!
//! That is also what makes rule 2 free: replacing the pending text is one
//! assignment, whereas cancelling a synthesis in flight would not be.
//!
//! ## No audio device
//!
//! A machine with no sound card must not make the reader fail; it must
//! make it silent and say so. The utterance is still transcribed to the
//! serial console, which is what lets the self-test check what *would*
//! have been said on a machine with no speaker -- the case every test
//! machine is in.

use alloc::string::String;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::vga::SpinLock;

use super::speech::{self, VoiceSettings};

/// How long the reader waits for the audio driver to settle, in ticks.
///
/// The reader is a compositor module, and the compositor starts its
/// modules **before** `drivers::start_all`: when this task first runs the
/// AC'97 probe has not happened yet, so asking the driver straight away
/// would get "no controller" on every machine. Five seconds is far more
/// than the probe takes and it is finite, which is what matters on a
/// machine where the audio driver never declares itself at all.
const AUDIO_SETTLE_TICKS: u64 = 5 * crate::TIMER_HZ as u64;

/// Longest text accepted for one utterance.
///
/// A label, not a document. Beyond this the text is cut -- and cutting is
/// right: a reader that spent forty seconds on a malformed window title
/// would be unusable exactly when something has gone wrong.
pub const MAX_TEXT: usize = 240;

/// Text waiting to be spoken. One slot: a new one replaces the old.
static PENDING: SpinLock<Option<String>> = SpinLock::new(None);
/// Priority text, which is never replaced and never interrupted.
static URGENT: SpinLock<Option<String>> = SpinLock::new(None);

/// Voice settings, held as three atomics rather than under a lock: they
/// are read by the synthesis loop and written from a settings file.
static PITCH: AtomicU32 = AtomicU32::new(120);
static RATE: AtomicU32 = AtomicU32::new(100);
static VOLUME: AtomicU32 = AtomicU32::new(180);

/// Is an audio device available?
static AUDIBLE: AtomicBool = AtomicBool::new(false);
/// Has the reader module actually started?
///
/// Distinct from `AUDIBLE`, and the distinction matters in the state
/// dump: "no audio output" and "the module never started" are different
/// diagnoses, and only the second is what a machine booted with no
/// framebuffer shows -- the modules are launched by the compositor, so
/// with no compositor there is no reader at all.
static STARTED: AtomicBool = AtomicBool::new(false);

/// Set while a self-test flips the reader on and off. Its announcements
/// then go to the serial console only: the access self-test switches the
/// reader on by its shortcut, and the machine used to greet every boot
/// with "lecteur d'ecran actif" out of the loudspeaker.
static HUSHED: AtomicBool = AtomicBool::new(false);

static SPOKEN: AtomicU64 = AtomicU64::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);
static SAMPLES: AtomicU64 = AtomicU64::new(0);

/// The voice as configured.
pub fn voice() -> VoiceSettings {
    VoiceSettings {
        pitch_hz: PITCH.load(Ordering::Relaxed),
        rate_percent: RATE.load(Ordering::Relaxed),
        volume: VOLUME.load(Ordering::Relaxed),
    }
    .sanitised()
}

/// Sets the pitch from a settings value. Returns whether it was read.
pub fn set_pitch(value: &str) -> bool {
    match value.parse::<u32>() {
        Ok(hertz) => {
            PITCH.store(hertz, Ordering::Relaxed);
            true
        }
        Err(_) => false,
    }
}

pub fn set_rate(value: &str) -> bool {
    match value.parse::<u32>() {
        Ok(percent) => {
            RATE.store(percent, Ordering::Relaxed);
            true
        }
        Err(_) => false,
    }
}

pub fn set_volume(value: &str) -> bool {
    match value.parse::<u32>() {
        Ok(level) => {
            VOLUME.store(level, Ordering::Relaxed);
            true
        }
        Err(_) => false,
    }
}

/// Queues a text to be spoken.
///
/// Returns immediately: the synthesis happens in the reader task. See the
/// module header for why that separation is not optional.
///
/// A text queued while another is pending **replaces** it. That is rule 2
/// and it is what makes fast navigation usable.
pub fn say(text: &str) {
    if !super::is_on(super::Feature::Reader) {
        return;
    }
    let text = truncate(text);
    // The serial transcript happens whatever the audio device does: it is
    // what lets the behaviour be checked on a machine with no speaker,
    // which is every machine this is tested on.
    crate::serial_println!("[lecteur] \"{}\"", text);
    if HUSHED.load(Ordering::Relaxed) {
        return;
    }

    let mut pending = PENDING.lock();
    if pending.is_some() {
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
    *pending = Some(text);
}

/// Queues a text that must not be dropped or interrupted (spec 9,
/// PRIORITY level).
pub fn say_urgent(text: &str) {
    let text = truncate(text);
    crate::serial_println!("[lecteur] priorite \"{}\"", text);
    let mut urgent = URGENT.lock();
    // An urgent text already waiting is **kept**: two things that must
    // not be missed are two things that must not be missed, and the
    // second one arriving is no reason to drop the first.
    if urgent.is_none() {
        *urgent = Some(text);
    } else {
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Silences the reader for a self-test: what it would say is logged, not
/// spoken. `false` also drops whatever was queued meanwhile.
pub fn hush(on: bool) {
    HUSHED.store(on, Ordering::Relaxed);
    if !on {
        PENDING.lock().take();
    }
}

fn truncate(text: &str) -> String {
    if text.len() <= MAX_TEXT {
        return String::from(text);
    }
    text.chars().take(MAX_TEXT).collect()
}

/// Counters: (utterances spoken, texts dropped, samples produced).
pub fn counters() -> (u64, u64, u64) {
    (
        SPOKEN.load(Ordering::Relaxed),
        DROPPED.load(Ordering::Relaxed),
        SAMPLES.load(Ordering::Relaxed),
    )
}

/// Is there a text waiting?
pub fn has_pending() -> bool {
    PENDING.lock().is_some() || URGENT.lock().is_some()
}

/// The reader task -- `screenreader.mod` of 16.2.
///
/// It does not create a surface: a screen reader has nothing to show. It
/// therefore does not go through `modules::run`, whose whole shape is
/// "create a surface, draw, wait" -- a module with no surface would sit
/// there holding an empty rectangle in the shell layer.
pub fn task() -> ! {
    // Whether an audio device answers at all is settled once, here: a
    // machine with no sound card must make the reader silent, not make it
    // fail. The **stream**, on the other hand, is opened per utterance --
    // see `speak`.
    STARTED.store(true, Ordering::Relaxed);
    if wait_for_audio() {
        AUDIBLE.store(true, Ordering::Relaxed);
        crate::serial_println!("[lecteur] synthese vocale prete - sortie audio disponible");
    } else {
        crate::serial_println!(
            "[lecteur] pas de sortie audio ({}) - annonces sur la console serie seule",
            audio_absence_reason()
        );
    }

    loop {
        // Urgent first, and taken out under the lock so a second urgent
        // text arriving between the test and the take is not lost.
        let text = {
            let taken = URGENT.lock().take();
            match taken {
                Some(text) => Some(text),
                None => PENDING.lock().take(),
            }
        };

        match text {
            Some(text) => speak(&text),
            None => {
                // Nothing to say: sleep. Polling here would be exactly the
                // "tight loop with nothing to do" `DECISIONS.md` names as
                // an invariant not to break -- and the symptom would be
                // "the mouse does not move".
                let _ = crate::syscall::invoke(
                    crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
                    [50, 0, 0, 0, 0, 0],
                );
            }
        }
    }
}

/// Waits for the audio driver to say whether it found a controller.
///
/// Returns whether one is driven. The question used to be put to the
/// **mixer** -- open a stream, take success for an answer -- and that was
/// wrong: `audio::open` reserves a slot in a mixer that exists whether or
/// not any hardware plays it, so it succeeds on a machine with no sound
/// card. The reader then announced itself audible while nothing could be
/// heard, and worse, fed samples into a queue no controller ever drained.
///
/// The driver registry is the only place that knows, and it knows only
/// once the probe has run -- hence the wait.
fn wait_for_audio() -> bool {
    let deadline = crate::idt::ticks() + AUDIO_SETTLE_TICKS;
    loop {
        if let Some(info) = crate::drivers::find("audio")
            && info.state != crate::drivers::DriverState::Starting
        {
            break;
        }
        if crate::idt::ticks() >= deadline {
            break;
        }
        let _ = crate::syscall::invoke(
            crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
            [10, 0, 0, 0, 0, 0],
        );
    }
    crate::drivers::audio::is_present()
}

/// Why there is no audio output, in the driver's own words.
///
/// Repeating its diagnosis rather than inventing one keeps a single
/// account of the hardware: "no AC'97 controller on the PCI bus" and "the
/// codec does not answer" are different machines, and a reader that said
/// only "no audio" would hide which.
fn audio_absence_reason() -> String {
    match crate::drivers::find("audio") {
        Some(info) if !info.device.is_empty() => info.device,
        Some(_) => String::from("pilote audio en cours de demarrage"),
        None => String::from("aucun pilote audio declare"),
    }
}

/// Renders one utterance and pushes it to the mixer.
///
/// ## The stream is opened here and closed here
///
/// Holding it open for the life of the reader looks tidier and costs the
/// whole machine. A stream that exists makes `audio::has_audio` true,
/// which makes the AC'97 driver fill and publish buffers a hundred times
/// a second **for ever** -- for silence, between two announcements a
/// minute apart. Under emulation that is enough to starve every other
/// task, and the symptom names neither audio nor the driver: the system
/// simply stops answering shortly after the reader is switched on.
///
/// Opening per utterance means the device is idle whenever nobody is
/// being spoken to, which is nearly always.
fn speak(text: &str) {
    let settings = voice();
    let phonemes = speech::graphemes(text);
    let mono = speech::render(&phonemes, settings);

    SPOKEN.fetch_add(1, Ordering::Relaxed);
    SAMPLES.fetch_add(mono.len() as u64, Ordering::Relaxed);

    let handle = if AUDIBLE.load(Ordering::Relaxed) {
        crate::audio::open("lecteur-ecran").ok()
    } else {
        None
    };

    let Some(handle) = handle else {
        // No device: the utterance was transcribed by `say`, and the time
        // it would have taken is spent sleeping. Returning instantly
        // would let the queue drain faster than a person could listen,
        // which would make the self-test measure something that cannot
        // happen on a real machine.
        let duration = speech::duration_ms(&phonemes, settings) as u64;
        let _ = crate::syscall::invoke(
            crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
            [duration.min(2_000), 0, 0, 0, 0, 0],
        );
        return;
    };

    // The mixer takes interleaved stereo. Both channels carry the same
    // signal: a synthesised voice has no stereo image, and pretending
    // otherwise would only double the arithmetic.
    let mut stereo = alloc::vec::Vec::with_capacity(mono.len() * crate::audio::CHANNELS);
    for sample in mono.iter() {
        for _ in 0..crate::audio::CHANNELS {
            stereo.push(*sample);
        }
    }

    // Written in slices, with a pause between them: the mixer buffer is
    // finite, and handing it a whole sentence at once would have it
    // refuse most of it. The slice is a tenth of a second, which is short
    // enough to fit and long enough not to spend the whole utterance in
    // system calls.
    let slice = (speech::SAMPLE_RATE as usize / 10) * crate::audio::CHANNELS;
    let mut offset = 0;
    // Bounded: a device that never drains would otherwise keep this loop
    // going for as long as the machine runs. The budget is generous
    // against the longest utterance the reader accepts and finite, which
    // is what matters.
    let deadline = crate::idt::ticks()
        + (speech::duration_ms(&phonemes, settings) as u64 * 4 / 10).max(200);

    while offset < stereo.len() && crate::idt::ticks() < deadline {
        let end = (offset + slice).min(stereo.len());
        match crate::audio::write(handle, &stereo[offset..end]) {
            Ok(written) if written > 0 => offset += written,
            // The device refused: wait for it to drain rather than
            // spinning. A reader that busy-waited on a full buffer would
            // hold a core for the length of the sentence.
            _ => {
                let _ = crate::syscall::invoke(
                    crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
                    [20, 0, 0, 0, 0, 0],
                );
            }
        }

        // A new text arriving interrupts this one (rule 2). Checked
        // between slices rather than between samples: a tenth of a second
        // of latency is imperceptible, and testing every sample would
        // cost more than the synthesis.
        if PENDING.lock().is_some() || URGENT.lock().is_some() {
            break;
        }
    }

    // Closed whatever happened -- including on the interruption above.
    // A stream left open is a driver left running.
    crate::audio::close(handle);
}

pub fn dump_state() {
    let (spoken, dropped, samples) = counters();
    if spoken == 0 && dropped == 0 {
        return;
    }
    let settings = voice();
    crate::serial_println!(
        "[lecteur] {} enonce(s), {} remplace(s), {} echantillon(s) - {} Hz, {} %, volume {}",
        spoken,
        dropped,
        samples,
        settings.pitch_hz,
        settings.rate_percent,
        settings.volume
    );
    if !STARTED.load(Ordering::Relaxed) {
        crate::serial_println!(
            "[lecteur] module non demarre - pas de compositeur, donc pas de modules"
        );
    } else if !AUDIBLE.load(Ordering::Relaxed) {
        crate::serial_println!("[lecteur] aucune sortie audio - annonces sur la console seule");
    }
}

//! Network time: an SNTP client (RFC 4330) that sets the wall clock.
//!
//! ## Why
//!
//! The wall clock is the CMOS reading of boot plus the tick counter
//! (`rtc`). The CMOS of a machine is as right as its last owner left it,
//! the tick drifts a few seconds a day, and TLS refuses a certificate
//! "not yet valid" the day the clock falls behind. When the machine is
//! online, the time is one datagram away.
//!
//! ## The exchange
//!
//! One 48-byte request (version 4, mode 3 "client") to port 123, one
//! answer (mode 4 "server"). Four instants, in two clocks:
//!
//! ```text
//!   T1  request leaves          (ours)
//!   T2  request reaches server  (server's)
//!   T3  answer leaves server    (server's)
//!   T4  answer arrives          (ours)
//! ```
//!
//! The round trip minus the server's own time is the time on the wire,
//! half of it on the way back: at T4 the server's clock reads
//! `T3 + ((T4 - T1) - (T3 - T2)) / 2`. T1 and T4 come from the TSC
//! (`linux::time::monotonic_ns`), not from the 100 Hz tick, so the round
//! trip of a few milliseconds is measured, not rounded to ten.
//!
//! The answer is checked before it is believed: its mode, a stratum
//! between 1 and 15 (0 is a "kiss of death", a refusal), and its
//! **origin** field, which must echo the transmit time we sent -- the
//! only thing tying the answer to our question, since UDP will deliver
//! a datagram from anybody.
//!
//! ## What is set
//!
//! Both wall clocks: Nova's (`rtc::set_now_millis`, the taskbar, file
//! dates, TLS) and the Linux subsystem's `CLOCK_REALTIME`
//! (`linux::time::set_realtime_ns`, the Web Browser). The monotonic
//! clocks never move -- a timeout must not jump because the date did.
//!
//! ## What is not done
//!
//! No slewing: the clock is stepped, as `ntpdate` does. No CMOS write:
//! the next boot reads the CMOS and synchronises again. No NTP
//! authentication (RFC 8915 NTS): an attacker on the path can move the
//! clock, as he can for any machine that trusts plain NTP.

use alloc::format;
use alloc::string::String;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::{Ipv4, dns, udp};
use crate::fs::perm::{Credentials, Mode};
use crate::vga::SpinLock;

const NTP_PORT: u16 = 123;
const PACKET: usize = 48;
/// Seconds from 1900-01-01 (NTP era 0) to 1970-01-01 (Unix).
const NTP_TO_UNIX: u64 = 2_208_988_800;
/// Servers tried in order. Anycast pools: the nearest answers.
const SERVERS: [&str; 3] = ["pool.ntp.org", "time.cloudflare.com", "time.google.com"];
const TIMEOUT_MS: u64 = 1500;

/// Between two synchronisations: six hours, well under the drift that
/// would matter (a few seconds a day).
const PERIOD_MS: u64 = 6 * 3600 * 1000;
/// After a failure (no network yet, server silent): a minute.
const RETRY_MS: u64 = 60 * 1000;

/// Machine setting: synchronise automatically?
pub const CONFIG_PATH: &str = "/sys/config/horloge.cfg";

static ENABLED: AtomicBool = AtomicBool::new(true);
static STARTED: AtomicBool = AtomicBool::new(false);
/// Asks the service to synchronise now (settings button, toggle on).
static WAKE: AtomicBool = AtomicBool::new(false);
static SYNCS: AtomicU64 = AtomicU64::new(0);

/// The last successful synchronisation.
#[derive(Clone)]
pub struct Report {
    pub server: String,
    pub address: Ipv4,
    /// Wall clock set, in milliseconds since the epoch.
    pub epoch_ms: u64,
    /// Correction applied, in milliseconds (positive: the clock was late).
    pub correction_ms: i64,
    pub round_trip_ms: u64,
}

static LAST: SpinLock<Option<Report>> = SpinLock::new(None);
static LAST_ERROR: SpinLock<Option<&'static str>> = SpinLock::new(None);

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub fn last() -> Option<Report> {
    LAST.lock().clone()
}

pub fn last_error() -> Option<&'static str> {
    *LAST_ERROR.lock()
}

/// Turns the automatic synchronisation on or off, and records it.
pub fn set_enabled(on: bool) -> Result<(), crate::fs::FsError> {
    ENABLED.store(on, Ordering::Relaxed);
    if on {
        WAKE.store(true, Ordering::Relaxed);
    }
    let text = format!("# Ecrit par horloge.ss\nsynchro = {}\n", if on { "oui" } else { "non" });
    crate::fs::write_file(CONFIG_PATH, text.as_bytes(), Credentials::kernel(), Mode::system_file(), crate::idt::ticks())
        .map(|_| ())
}

/// Asks the service for a synchronisation as soon as possible.
pub fn request() {
    WAKE.store(true, Ordering::Relaxed);
}

fn load_config() {
    if let Ok(bytes) = crate::fs::read_file(CONFIG_PATH, Credentials::kernel()) {
        let text = core::str::from_utf8(&bytes).unwrap_or("");
        let off = crate::config::store::parse(text).iter().any(|(key, value)| key == "synchro" && value == "non");
        ENABLED.store(!off, Ordering::Relaxed);
    }
}

/// Starts the service. Called once the network is configured (or not:
/// the service waits for it).
pub fn start() {
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    load_config();
    crate::scheduler::spawn("horloge-ntp", crate::process::BinaryType::Ss, service);
}

fn sleep_ms(ms: u64) {
    let _ = crate::syscall::invoke(crate::syscall::domain_time::SYS_TIME_SLEEP_MS, [ms, 0, 0, 0, 0, 0]);
}

fn service() -> ! {
    loop {
        let wait = if !enabled() {
            PERIOD_MS
        } else {
            match sync_now() {
                Ok(_) => PERIOD_MS,
                Err(_) => RETRY_MS,
            }
        };
        // Sleeps in steps of 200 ms so a request from the settings is
        // served at once rather than hours later.
        let mut slept = 0;
        while slept < wait {
            if WAKE.swap(false, Ordering::Relaxed) {
                break;
            }
            sleep_ms(200);
            slept += 200;
        }
    }
}

/// One synchronisation, against the first server that answers.
pub fn sync_now() -> Result<Report, &'static str> {
    let outcome = try_servers();
    match &outcome {
        Ok(report) => {
            SYNCS.fetch_add(1, Ordering::Relaxed);
            *LAST_ERROR.lock() = None;
            let date = crate::rtc::DateTime::from_epoch(report.epoch_ms / 1000).format();
            crate::serial_println!(
                "[ntp] horloge reglee sur {} ({}) : {} UTC - correction {} ms, aller-retour {} ms",
                report.server,
                report.address.text(),
                core::str::from_utf8(&date).unwrap_or("?"),
                report.correction_ms,
                report.round_trip_ms
            );
            *LAST.lock() = Some(report.clone());
        }
        Err(reason) => {
            *LAST_ERROR.lock() = Some(reason);
            crate::serial_println!("[ntp] synchronisation impossible : {}", reason);
        }
    }
    outcome
}

fn try_servers() -> Result<Report, &'static str> {
    if !super::interface().configured() {
        return Err("pas de connexion reseau");
    }
    let mut failure = "aucun serveur de temps";
    for server in SERVERS {
        let address = match dns::resolve(server) {
            Ok(address) => address,
            Err(reason) => {
                failure = reason;
                continue;
            }
        };
        match query(address) {
            Ok((epoch_ms, correction_ms, round_trip_ms)) => {
                return Ok(Report { server: String::from(server), address, epoch_ms, correction_ms, round_trip_ms });
            }
            Err(reason) => failure = reason,
        }
    }
    Err(failure)
}

/// NTP 64-bit timestamp to milliseconds since the Unix epoch. Era 1
/// (after 2036-02-07) wraps the 32-bit seconds: a value below the Unix
/// offset can only be that.
fn ntp_to_unix_ms(raw: &[u8]) -> u64 {
    let seconds = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as u64;
    let fraction = u32::from_be_bytes([raw[4], raw[5], raw[6], raw[7]]) as u64;
    let seconds = if seconds >= NTP_TO_UNIX { seconds - NTP_TO_UNIX } else { seconds + (1 << 32) - NTP_TO_UNIX };
    seconds * 1000 + ((fraction * 1000) >> 32)
}

/// Asks one server. Returns (clock set, correction, round trip).
fn query(address: Ipv4) -> Result<(u64, i64, u64), &'static str> {
    let (socket, _) = udp::bind(0)?;
    let outcome = exchange(socket, address);
    udp::close(socket);
    outcome
}

fn exchange(socket: udp::Handle, address: Ipv4) -> Result<(u64, i64, u64), &'static str> {
    let mut request = [0u8; PACKET];
    // LI 0 (no warning), version 4, mode 3 (client).
    request[0] = (4 << 3) | 3;
    // Transmit timestamp: random bytes the server must echo as its
    // origin. Not our clock -- nothing needs it, and a random value is
    // what makes a forged answer a guess.
    let mut nonce = [0u8; 8];
    crate::random::fill(&mut nonce);
    request[40..48].copy_from_slice(&nonce);

    let t1 = crate::posix::time::monotonic_ns();
    udp::send_to(socket, address, NTP_PORT, &request)?;
    let deadline = crate::idt::ticks() + TIMEOUT_MS / (1000 / crate::TIMER_HZ as u64);
    loop {
        let remaining_ticks = deadline.saturating_sub(crate::idt::ticks());
        if remaining_ticks == 0 {
            return Err("le serveur de temps ne repond pas");
        }
        let Some(datagram) = udp::recv(socket, remaining_ticks * (1000 / crate::TIMER_HZ as u64)) else {
            return Err("le serveur de temps ne repond pas");
        };
        let t4 = crate::posix::time::monotonic_ns();
        let answer = &datagram.payload;
        if datagram.source != address || datagram.port != NTP_PORT || answer.len() < PACKET {
            continue;
        }
        if answer[24..32] != nonce {
            // Not the answer to this question.
            continue;
        }
        if answer[0] & 0x07 != 4 {
            return Err("reponse NTP invalide (mode)");
        }
        let stratum = answer[1];
        if stratum == 0 || stratum > 15 {
            return Err("serveur de temps non synchronise ou refus");
        }
        let t2 = ntp_to_unix_ms(&answer[32..40]);
        let t3 = ntp_to_unix_ms(&answer[40..48]);
        let round_trip_ms = (t4 - t1) / 1_000_000;
        let server_ms = t3.saturating_sub(t2);
        let wire_ms = round_trip_ms.saturating_sub(server_ms);
        let now_ms = t3 + wire_ms / 2;
        // Microseconds of the TSC since t4, added so that the value set is
        // "now" and not "when the answer arrived".
        let since_ms = (crate::posix::time::monotonic_ns() - t4) / 1_000_000;
        let now_ms = now_ms + since_ms;
        if now_ms / 1000 < 1_600_000_000 {
            return Err("heure recue invraisemblable");
        }
        let before = crate::rtc::now_millis().unwrap_or(0);
        crate::rtc::set_now_millis(now_ms);
        crate::posix::time::set_realtime_ns(now_ms * 1_000_000);
        return Ok((now_ms, now_ms as i64 - before as i64, round_trip_ms));
    }
}

pub fn dump_state() {
    match last() {
        Some(report) => crate::serial_println!(
            "[ntp] {} synchronisation(s), derniere sur {} - correction {} ms",
            SYNCS.load(Ordering::Relaxed),
            report.server,
            report.correction_ms
        ),
        None => crate::serial_println!(
            "[ntp] aucune synchronisation{}",
            if enabled() { "" } else { " (desactivee)" }
        ),
    }
}

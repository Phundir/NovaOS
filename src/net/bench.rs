//! Throughput measurement: one HTTP download, timed, its bytes thrown
//! away.
//!
//! ## Why it exists
//!
//! "The browser can reach 200 MB/s" is a claim about the card, the driver,
//! TCP and the host together, and only a transfer measures that. The boot
//! must not depend on the network, so the measurement runs **only when
//! asked**, through the QEMU firmware configuration:
//!
//! ```text
//!   -fw_cfg name=opt/nova/netbench,string=10.0.2.2:8000/bench.bin
//! ```
//!
//! `make bench-net` sets it up: a file served by the host, the guest
//! fetching it once the network is configured and the self-tests done.
//! The address is an IPv4 literal -- no DNS, so that nothing but the
//! transfer is timed.
//!
//! ## What is timed
//!
//! From the request sent to the end of the stream: the connection setup
//! is excluded, the slow start of the sender is not. The body is read
//! through `tcp::recv` exactly as the browser reads it, and discarded --
//! the rate is that of the stack, not of the parser above it. Units are
//! decimal: 1 Mo = 1 000 000 bytes, as network rates are counted.

use alloc::format;
use alloc::string::String;

use super::{Ipv4, tcp};

const CONNECT_MS: u64 = 5_000;
const READ_MS: u64 = 20_000;
/// Bytes asked of `tcp::recv` per call: large, so that the measure is not
/// the cost of the call.
const CHUNK: usize = 256 * 1024;

/// `host:port/path`, host an IPv4 literal.
fn parse(target: &str) -> Option<(Ipv4, u16, String)> {
    let (authority, path) = match target.find('/') {
        Some(slash) => (&target[..slash], &target[slash..]),
        None => (target, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().ok()?),
        None => (authority, 80),
    };
    Some((Ipv4::parse(host)?, port, String::from(path)))
}

/// Starts the measurement if the host asked for one.
///
/// It runs in a task of its own, of the class of the browser's loading
/// tasks (`.cus`), and only once the boot self-tests are over: measured
/// on core 0 among the `.dsa` self-tests, the first version timed the
/// scheduler rather than the network.
pub fn run_if_requested() {
    if crate::fwcfg::option("netbench").is_some() {
        crate::scheduler::spawn("mesure-debit", crate::process::BinaryType::Cus, task);
    }
}

/// Longest wait for the self-tests, in milliseconds.
const SELFTESTS_MS: u64 = 180_000;

fn task() -> ! {
    let mut waited = 0;
    while crate::selftests_pending().0 != 0 && waited < SELFTESTS_MS {
        let _ = crate::syscall::invoke(crate::syscall::domain_time::SYS_TIME_SLEEP_MS, [100, 0, 0, 0, 0, 0]);
        waited += 100;
    }
    let tsc_per_ms = calibrate();
    if let Some(target) = crate::fwcfg::option("netbench")
        && let Err(reason) = measure(&target, tsc_per_ms)
    {
        crate::serial_println!("[debit] ECHEC : {}", reason);
    }
    crate::scheduler::exit_current_with(0)
}

/// Time-stamp counter increments per millisecond, measured against the
/// clock over half a second of sleep.
///
/// ## Why not the ticks alone
///
/// The tick counter advances with the timer of core 0 only. A core that
/// keeps it waiting on a lock -- interrupts masked -- makes it lose ticks,
/// and a transfer timed in ticks then looks impossibly fast: the first
/// version of the busy wait of `tcp::wait_step` reported 5 GB/s. Timed
/// by the TSC, calibrated while nothing spins, the duration is right
/// whatever happens to core 0; the two are printed side by side, and a
/// gap between them is itself a finding.
fn calibrate() -> u64 {
    let per_tick = 1000 / crate::TIMER_HZ as u64;
    let (tsc_start, tick_start) = (crate::random::rdtsc(), crate::idt::ticks());
    let _ = crate::syscall::invoke(crate::syscall::domain_time::SYS_TIME_SLEEP_MS, [500, 0, 0, 0, 0, 0]);
    let (tsc_end, tick_end) = (crate::random::rdtsc(), crate::idt::ticks());
    let ms = ((tick_end - tick_start) * per_tick).max(1);
    (tsc_end.wrapping_sub(tsc_start) / ms).max(1)
}

fn measure(target: &str, tsc_per_ms: u64) -> Result<(), String> {
    let (address, port, path) = parse(target).ok_or_else(|| format!("cible illisible \"{}\"", target))?;
    crate::serial_println!(
        "[debit] carte {} - GET http://{}:{}{}",
        crate::drivers::nic::model(),
        address.text(),
        port,
        path
    );

    let connection = tcp::connect(address, port, CONNECT_MS).map_err(String::from)?;
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\n\r\n",
        path,
        address.text(),
        port
    );
    let (_, frames_before, lost_before) = crate::drivers::nic::counters();
    let start = crate::idt::ticks();
    let tsc_start = crate::random::rdtsc();
    if let Err(error) = tcp::send(connection, request.as_bytes()) {
        tcp::close(connection);
        return Err(String::from(error));
    }

    // The header is read off the start of the stream, the rest counted.
    let mut header = alloc::vec::Vec::new();
    let mut body: u64 = 0;
    let mut status = String::new();
    loop {
        let chunk = match tcp::recv(connection, CHUNK, READ_MS) {
            Ok(chunk) => chunk,
            Err(error) => {
                tcp::close(connection);
                return Err(format!("{} apres {} octets", error, body));
            }
        };
        if chunk.is_empty() {
            break;
        }
        if status.is_empty() {
            header.extend_from_slice(&chunk);
            if let Some(end) = header.windows(4).position(|w| w == b"\r\n\r\n") {
                let line_end = header.iter().position(|&b| b == b'\r').unwrap_or(0);
                status = header[..line_end]
                    .iter()
                    .filter(|b| (0x20..0x7F).contains(*b))
                    .map(|&b| b as char)
                    .collect();
                body += (header.len() - end - 4) as u64;
                header = alloc::vec::Vec::new();
            } else if header.len() > 16 * 1024 {
                tcp::close(connection);
                return Err(String::from("en-tete HTTP interminable"));
            }
        } else {
            body += chunk.len() as u64;
        }
    }
    let ticks = crate::idt::ticks() - start;
    let tsc_ms = (crate::random::rdtsc().wrapping_sub(tsc_start) / tsc_per_ms).max(1);
    // Segments out of order and the like: what explains a slow run.
    tcp::dump_state();
    tcp::close(connection);

    let per_tick = 1000 / crate::TIMER_HZ as u64;
    let tick_ms = ticks * per_tick;
    // Tenths of a megabyte per second: bytes / ms / 100.
    let tenths = body / tsc_ms / 100;
    let (_, frames_after, lost_after) = crate::drivers::nic::counters();
    crate::serial_println!("[debit] reponse \"{}\"", status);
    crate::serial_println!(
        "[debit] {} octets en {} ms ({} ms au compteur de tics) - {}.{} Mo/s ({} trames recues, {} perdues)",
        body,
        tsc_ms,
        tick_ms,
        tenths / 10,
        tenths % 10,
        frames_after - frames_before,
        lost_after - lost_before
    );
    Ok(())
}

//! The small calls: identity of the system, randomness, resource usage.
//!
//! `uname` answers by the door the program came through. A Linux program
//! is told "Linux 6.6.0": it reads the release to decide which system
//! calls it may use (glibc refuses to start below its minimum), and what
//! it compares against is the Linux version whose calls the personality
//! translates. A ported program is told the truth -- "Nova" -- since it
//! makes no Linux call to choose among.

use alloc::sync::Arc;

use super::errno::*;
use super::Thread;

/// Bytes from the kernel's generator (`random`, ChaCha-based, seeded from
/// RDRAND when present and from timing otherwise).
pub fn fill_random(buf: &mut [u8]) {
    crate::random::fill(buf);
}

pub fn getrandom(buf: u64, len: u64, _flags: u64) -> LxResult<u64> {
    let len = len.min(1 << 20) as usize;
    let mut data = super::files::user_buffer(len)?;
    fill_random(&mut data);
    crate::vm::copy_to_user(buf, &data).map_err(|_| EFAULT)?;
    Ok(len as u64)
}

pub fn uname(t: &Arc<Thread>, buf: u64) -> LxResult<u64> {
    // struct utsname: six fields of 65 bytes.
    let fields = match t.process.abi {
        super::Abi::Linux => ["Linux", "nova", "6.6.0-nova", "#1 SMP Nova OS", "x86_64", "(none)"],
        super::Abi::Nova => ["Nova", "nova", crate::version::VERSION, "#1 SMP Nova OS", "x86_64", "(none)"],
    };
    let mut out = [0u8; 65 * 6];
    for (i, f) in fields.iter().enumerate() {
        out[i * 65..i * 65 + f.len()].copy_from_slice(f.as_bytes());
    }
    crate::vm::copy_to_user(buf, &out).map_err(|_| EFAULT)?;
    Ok(0)
}

pub fn sysinfo(buf: u64) -> LxResult<u64> {
    let (total, _, free) = crate::frame::stats();
    let uptime = super::time::monotonic_ns() / 1_000_000_000;
    // struct sysinfo on x86-64: 112 bytes.
    let mut out = [0u8; 112];
    out[0..8].copy_from_slice(&uptime.to_le_bytes());
    out[32..40].copy_from_slice(&(total as u64 * 4096).to_le_bytes());
    out[40..48].copy_from_slice(&(free as u64 * 4096).to_le_bytes());
    let procs = crate::scheduler::stats().0 as u16;
    out[80..82].copy_from_slice(&procs.to_le_bytes());
    out[104..108].copy_from_slice(&1u32.to_le_bytes()); // mem_unit
    crate::vm::copy_to_user(buf, &out).map_err(|_| EFAULT)?;
    Ok(0)
}

/// `getrusage`: CPU time from the tick counter, the rest zero.
pub fn getrusage(t: &Arc<Thread>, _who: u64, buf: u64) -> LxResult<u64> {
    let _ = t;
    let ticks = crate::scheduler::current_info().map(|i| i.ticks_used).unwrap_or(0);
    let usec = ticks * (1_000_000 / crate::TIMER_HZ as u64);
    let mut out = [0u8; 144];
    out[0..8].copy_from_slice(&(usec / 1_000_000).to_le_bytes());
    out[8..16].copy_from_slice(&(usec % 1_000_000).to_le_bytes());
    crate::vm::copy_to_user(buf, &out).map_err(|_| EFAULT)?;
    Ok(0)
}

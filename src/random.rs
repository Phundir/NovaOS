//! Cryptographic randomness: an entropy pool and a ChaCha20 generator.
//!
//! ## Why it exists
//!
//! TLS needs secrets nobody can guess -- the ephemeral key of each
//! handshake, its random. A predictable one lets whoever records the
//! traffic decrypt it. Until the browser, nothing in the kernel needed
//! more than a counter; the TCP initial sequence number said as much.
//!
//! ## Sources
//!
//! * `RDSEED` and `RDRAND`, when the processor has them -- with KVM on a
//!   recent host, or `-cpu max`;
//! * the jitter of the time-stamp counter around short loops -- under
//!   emulation it carries the host's scheduling noise;
//! * the arrival times of keyboard and mouse events, `mix_event`,
//!   stirred in as they come: a human is the one source nobody can replay.
//!
//! **Named limit.** With QEMU's default CPU there is no `RDRAND`, and the
//! first handshake may happen before any key was pressed: the seed then
//! rests on timing jitter alone. That is what every embedded device
//! without a hardware generator lives with, and it is weaker than a
//! virtio-rng device would be (the virtio transport of `virtio_gpu` would
//! have to be generalised first).
//!
//! ## The generator
//!
//! "Fast key erasure": each request runs ChaCha20 from the current key,
//! hands out the stream, and replaces the key with the next 32 bytes of
//! that same stream. A key once used exists no more, so a memory dump
//! reveals nothing of past output.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::crypto::chacha;
use crate::crypto::sha2::{Hash, Sha256};
use crate::vga::SpinLock;

struct Generator {
    key: [u8; 32],
    seeded: bool,
}

static GENERATOR: SpinLock<Generator> = SpinLock::new(Generator {
    key: [0; 32],
    seeded: false,
});

/// Pending entropy from events, folded into the key at the next request.
static EVENTS: SpinLock<Sha256Pool> = SpinLock::new(Sha256Pool { bytes: [0; 64], filled: 0, count: 0 });

/// A small buffer of event timestamps.
struct Sha256Pool {
    bytes: [u8; 64],
    filled: usize,
    count: u64,
}

static HARDWARE_LOGGED: AtomicBool = AtomicBool::new(false);

pub(crate) fn rdtsc() -> u64 {
    let low: u32;
    let high: u32;
    // Safe: reads the time-stamp counter, no side effect.
    unsafe { core::arch::asm!("rdtsc", out("eax") low, out("edx") high, options(nomem, nostack)) };
    ((high as u64) << 32) | low as u64
}

/// (RDRAND available, RDSEED available), from CPUID.
fn hardware() -> (bool, bool) {
    let ecx: u32;
    let ebx: u32;
    // Safe: CPUID leaves 1 and 7 exist on every x86-64 processor. RBX is
    // saved by hand, LLVM reserves it.
    unsafe {
        core::arch::asm!(
            "push rbx",
            "mov eax, 1",
            "xor ecx, ecx",
            "cpuid",
            "pop rbx",
            out("ecx") ecx,
            out("eax") _,
            out("edx") _,
        );
        core::arch::asm!(
            "push rbx",
            "mov eax, 7",
            "xor ecx, ecx",
            "cpuid",
            "mov {out:e}, ebx",
            "pop rbx",
            out = out(reg) ebx,
            out("eax") _,
            out("ecx") _,
            out("edx") _,
        );
    }
    (ecx & (1 << 30) != 0, ebx & (1 << 18) != 0)
}

/// One 64-bit value from RDSEED (preferred) or RDRAND, retried a few
/// times as Intel's guide advises. `None` if the unit keeps failing.
fn hardware_word(seed: bool) -> Option<u64> {
    for _ in 0..10 {
        let value: u64;
        let ok: u8;
        // Safe: the instruction exists -- checked by `hardware` -- and
        // only writes its register and the carry flag.
        unsafe {
            if seed {
                core::arch::asm!("rdseed {v}", "setc {ok}", v = out(reg) value, ok = out(reg_byte) ok, options(nomem, nostack));
            } else {
                core::arch::asm!("rdrand {v}", "setc {ok}", v = out(reg) value, ok = out(reg_byte) ok, options(nomem, nostack));
            }
        }
        if ok != 0 {
            return Some(value);
        }
    }
    None
}

/// Gathers a seed. Called with no lock held: the jitter loop takes a
/// few milliseconds.
fn gather() -> [u8; 32] {
    let mut hash = Sha256::new();
    let (rdrand, rdseed) = hardware();
    if !HARDWARE_LOGGED.swap(true, Ordering::Relaxed) {
        crate::serial_println!(
            "[random] sources : RDSEED {}, RDRAND {}, gigue du TSC, evenements d'entree",
            if rdseed { "oui" } else { "non" },
            if rdrand { "oui" } else { "non" }
        );
    }
    let mut hardware_bytes = 0;
    for _ in 0..8 {
        let word = if rdseed {
            hardware_word(true)
        } else if rdrand {
            hardware_word(false)
        } else {
            None
        };
        if let Some(word) = word {
            hash.update(&word.to_le_bytes());
            hardware_bytes += 8;
        }
    }
    // Timing jitter: the low bits of the time a short loop takes, over and
    // over. Each sample carries a bit or so; four thousand of them.
    let mut scratch = [0u64; 64];
    for round in 0..4096u64 {
        let start = rdtsc();
        for index in 0..(8 + (round & 7) as usize) {
            scratch[index * 7 % 64] = scratch[index * 7 % 64].wrapping_mul(6364136223846793005).wrapping_add(start);
        }
        let delta = rdtsc().wrapping_sub(start);
        hash.update(&delta.to_le_bytes()[..2]);
    }
    hash.update(&scratch[0].to_le_bytes());
    hash.update(&crate::idt::ticks().to_le_bytes());
    if let Some(now) = crate::rtc::now() {
        hash.update(&now.to_le_bytes());
    }
    hash.update(&(hardware_bytes as u64).to_le_bytes());
    let digest = hash.finish();
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&digest[..32]);
    seed
}

/// Stirs the time of an input event into the pool. Cheap: called by the
/// compositor for every key and pointer event.
pub fn mix_event(value: u64) {
    let stamp = rdtsc() ^ value.rotate_left(17);
    let mut pool = EVENTS.lock();
    let at = pool.filled;
    pool.bytes[at..at + 8].copy_from_slice(&stamp.to_le_bytes());
    pool.filled = (at + 8) % 64;
    pool.count += 1;
}

/// Fills `buffer` with unpredictable bytes.
pub fn fill(buffer: &mut [u8]) {
    let fresh = {
        let seeded = GENERATOR.lock().seeded;
        (!seeded).then(gather)
    };
    let events = {
        let pool = EVENTS.lock();
        (pool.count, pool.bytes)
    };
    // A nonce per call from the TSC: two calls with the same key never
    // share a stream, even if the rekeying below were ever skipped.
    let mut nonce = [0u8; 12];
    nonce[..8].copy_from_slice(&rdtsc().to_le_bytes());

    let mut generator = GENERATOR.lock();
    if let Some(seed) = fresh
        && !generator.seeded
    {
        generator.key = seed;
        generator.seeded = true;
    }
    // Fold the event pool into the key.
    let mut hash = Sha256::new();
    hash.update(&generator.key);
    hash.update(&events.0.to_le_bytes());
    hash.update(&events.1);
    let digest = hash.finish();
    generator.key.copy_from_slice(&digest[..32]);

    let mut counter = 0u32;
    // The first block rekeys; the rest is output.
    let block = chacha::block(&generator.key, counter, &nonce);
    counter += 1;
    let mut next_key = [0u8; 32];
    next_key.copy_from_slice(&block[..32]);
    for chunk in buffer.chunks_mut(64) {
        let stream = chacha::block(&generator.key, counter, &nonce);
        counter += 1;
        chunk.copy_from_slice(&stream[..chunk.len()]);
    }
    generator.key = next_key;
}

pub fn u64() -> u64 {
    let mut bytes = [0u8; 8];
    fill(&mut bytes);
    u64::from_le_bytes(bytes)
}

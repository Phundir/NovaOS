//! Floating point and SSE for ring 3 (phase 11.2).
//!
//! ## Why the kernel arms SSE without using it
//!
//! The kernel is compiled soft-float: no instruction of its own touches
//! the x87 stack or an XMM register. Programs compiled for x86_64 by an
//! ordinary compiler, on the other hand, use SSE2 everywhere -- it is part
//! of the base of the architecture, a `double` lives in `xmm0`. The
//! Web Browser port (Firefox, phase 11) cannot run without it.
//!
//! Two things are therefore needed, and only two:
//!
//! 1. **Arming**, on every core, since CR0 and CR4 are per processor:
//!    `CR0.EM` cleared (no x87 emulation trap), `CR0.MP` set, `CR0.TS`
//!    cleared, `CR4.OSFXSR` (FXSAVE saves the XMM registers and SSE
//!    instructions stop raising `#UD`) and `CR4.OSXMMEXCPT` (an unmasked
//!    SIMD exception raises `#XM` rather than `#UD`).
//! 2. **Saving per task.** The registers belong to whichever task last
//!    ran: without a save, two tasks computing on the same core would
//!    read each other's values. `switch_context` saves the 512-byte
//!    FXSAVE image on the outgoing stack, next to the general registers,
//!    and restores the incoming one. A new task starts from `INITIAL`,
//!    the state `fninit` would give plus the default MXCSR.
//!
//! The save is eager -- on every switch -- rather than lazy through
//! `CR0.TS` and `#NM`. Lazy saving saves 512 bytes of copy for tasks that
//! never compute, at the price of a trap and of a per-core owner to track
//! across migrations and task deaths. An FXSAVE costs less than the
//! switch around it; the simple version is the one kept.
//!
//! ## What is left out
//!
//! AVX and later (XSAVE, `CR4.OSXSAVE`, `XCR0`) are **not** armed: CPUID
//! then reports OSXSAVE clear, and correct software -- Firefox included --
//! detects AVX as unusable and stays on SSE2. Arming them needs an XSAVE
//! area whose size CPUID dictates, which is left for when a program needs
//! it.

use core::arch::asm;

/// Size of an FXSAVE image. The instruction demands 16-byte alignment.
pub const AREA_SIZE: usize = 512;

/// FXSAVE image of a fresh task.
///
/// FCW 0x037F: every x87 exception masked, 64-bit precision, round to
/// nearest -- the value `fninit` loads. MXCSR 0x1F80: every SIMD
/// exception masked, round to nearest, no flush to zero -- the reset
/// value. Everything else zero: empty x87 stack (abridged tag word 0),
/// null registers.
pub const INITIAL: [u8; AREA_SIZE] = {
    let mut area = [0u8; AREA_SIZE];
    area[0] = 0x7F;
    area[1] = 0x03;
    area[24] = 0x80;
    area[25] = 0x1F;
    area
};

const CR0_MP: u64 = 1 << 1;
const CR0_EM: u64 = 1 << 2;
const CR0_TS: u64 = 1 << 3;
const CR4_OSFXSR: u64 = 1 << 9;
const CR4_OSXMMEXCPT: u64 = 1 << 10;

/// CPUID leaf 1, EDX: FXSR, SSE, SSE2.
const CPUID_FXSR: u32 = 1 << 24;
const CPUID_SSE: u32 = 1 << 25;
const CPUID_SSE2: u32 = 1 << 26;

fn cpuid1_edx() -> u32 {
    let edx: u32;
    // SAFETY: CPUID leaf 1 exists on every x86_64 processor. RBX is
    // reserved by LLVM, hence the save around the instruction.
    unsafe {
        asm!(
            "mov {tmp}, rbx",
            "cpuid",
            "mov rbx, {tmp}",
            tmp = out(reg) _,
            inout("eax") 1u32 => _,
            inout("ecx") 0u32 => _,
            out("edx") edx,
            options(nomem, nostack, preserves_flags)
        );
    }
    edx
}

/// Does the processor have FXSAVE, SSE and SSE2? Always true in long
/// mode on a real x86_64, checked anyway: a false answer would otherwise
/// show up as `#UD` in the first `switch_context`.
pub fn supported() -> bool {
    let edx = cpuid1_edx();
    let wanted = CPUID_FXSR | CPUID_SSE | CPUID_SSE2;
    edx & wanted == wanted
}

/// Arms the FPU and SSE on the calling core.
///
/// Returns false, leaving the control registers alone, if the processor
/// lacks one of the three features.
///
/// # Safety
/// To be called on every core, before the first `switch_context` of that
/// core: the switch runs FXSAVE and FXRSTOR, which fault while `CR0.EM`
/// is set.
pub unsafe fn enable() -> bool {
    if !supported() {
        return false;
    }
    unsafe {
        let mut cr0: u64;
        asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack, preserves_flags));
        cr0 = (cr0 & !(CR0_EM | CR0_TS)) | CR0_MP;
        asm!("mov cr0, {}", in(reg) cr0, options(nomem, nostack, preserves_flags));

        let mut cr4: u64;
        asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags));
        cr4 |= CR4_OSFXSR | CR4_OSXMMEXCPT;
        asm!("mov cr4, {}", in(reg) cr4, options(nomem, nostack, preserves_flags));

        // Leaves the core's own x87 state clean as well: the boot code
        // never initialised it.
        asm!("fninit", options(nomem, nostack));
    }
    true
}

/// Is SSE armed on the calling core?
pub fn enabled() -> bool {
    let cr4: u64;
    unsafe { asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags)) };
    cr4 & CR4_OSFXSR != 0
}

// ============================================================
//  Self-test
// ============================================================

/// Loads `seed`-derived values into the sixteen XMM registers and MXCSR.
///
/// The kernel is soft-float: the XMM registers are named in the assembly
/// text itself, never as operands, so no SSE code generation is needed.
fn load_pattern(seed: u64) {
    let mxcsr: u32 = 0x1F80 | if seed & 1 == 0 { 0x6000 } else { 0x2000 };
    unsafe {
        asm!(
            "movq xmm0, {v}", "add {v}, {s}",
            "movq xmm1, {v}", "add {v}, {s}",
            "movq xmm2, {v}", "add {v}, {s}",
            "movq xmm3, {v}", "add {v}, {s}",
            "movq xmm4, {v}", "add {v}, {s}",
            "movq xmm5, {v}", "add {v}, {s}",
            "movq xmm6, {v}", "add {v}, {s}",
            "movq xmm7, {v}", "add {v}, {s}",
            "movq xmm8, {v}", "add {v}, {s}",
            "movq xmm9, {v}", "add {v}, {s}",
            "movq xmm10, {v}", "add {v}, {s}",
            "movq xmm11, {v}", "add {v}, {s}",
            "movq xmm12, {v}", "add {v}, {s}",
            "movq xmm13, {v}", "add {v}, {s}",
            "movq xmm14, {v}", "add {v}, {s}",
            "movq xmm15, {v}",
            "ldmxcsr [{m}]",
            v = inout(reg) seed => _,
            s = in(reg) seed,
            m = in(reg) &mxcsr,
            options(nostack)
        );
    }
}

/// Reads back what `load_pattern(seed)` wrote; true if all of it is
/// still there.
fn check_pattern(seed: u64) -> bool {
    let mut regs = [0u64; 16];
    let mut mxcsr: u32 = 0;
    unsafe {
        asm!(
            "movq [{r}], xmm0", "movq [{r} + 8], xmm1",
            "movq [{r} + 16], xmm2", "movq [{r} + 24], xmm3",
            "movq [{r} + 32], xmm4", "movq [{r} + 40], xmm5",
            "movq [{r} + 48], xmm6", "movq [{r} + 56], xmm7",
            "movq [{r} + 64], xmm8", "movq [{r} + 72], xmm9",
            "movq [{r} + 80], xmm10", "movq [{r} + 88], xmm11",
            "movq [{r} + 96], xmm12", "movq [{r} + 104], xmm13",
            "movq [{r} + 112], xmm14", "movq [{r} + 120], xmm15",
            "stmxcsr [{m}]",
            r = in(reg) regs.as_mut_ptr(),
            m = in(reg) &mut mxcsr,
            options(nostack)
        );
    }
    let mut expected = seed;
    for value in regs {
        if value != expected {
            return false;
        }
        expected = expected.wrapping_add(seed);
    }
    mxcsr == 0x1F80 | if seed & 1 == 0 { 0x6000 } else { 0x2000 }
}

/// Rounds of yield-and-check each test task performs.
const ROUNDS: u32 = 2000;

static CORRUPTED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static FINISHED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

fn worker(seed: u64) {
    use core::sync::atomic::Ordering;
    // A fresh task must start from the initial image, not from whatever
    // the previous task of the core left behind.
    let mut fresh: u32 = 0;
    unsafe { asm!("stmxcsr [{}]", in(reg) &mut fresh, options(nostack)) };
    if fresh != 0x1F80 {
        CORRUPTED.fetch_add(1, Ordering::SeqCst);
    }
    for _ in 0..ROUNDS {
        load_pattern(seed);
        crate::scheduler::yield_now();
        if !check_pattern(seed) {
            CORRUPTED.fetch_add(1, Ordering::SeqCst);
        }
    }
    FINISHED.fetch_add(1, Ordering::SeqCst);
    // `exit_current_with`, not `exit_current`: only the former wakes a
    // task blocked in `wait_for`.
    crate::scheduler::exit_current_with(0);
}

fn worker_a() -> ! {
    worker(0x1111_2222_3333_4444);
    unreachable!()
}

fn worker_b() -> ! {
    worker(0x5555_6666_7777_8889);
    unreachable!()
}

fn worker_c() -> ! {
    worker(0x0A0B_0C0D_0E0F_1011);
    unreachable!()
}

/// Launches three kernel tasks on the same core, each filling every XMM
/// register and MXCSR with its own pattern, yielding, and checking the
/// pattern survived. Two thousand switches each: a missing save shows up
/// as a corrupted pattern at once.
///
/// Returns (checks passed, checks failed); the verdict line is printed
/// by the caller once the tasks are done.
pub fn selftest() -> (u32, u32) {
    use core::sync::atomic::Ordering;
    let mut ok = 0;
    let mut failed = 0;
    let mut check = |cond: bool, what: &str| {
        if cond {
            ok += 1;
        } else {
            failed += 1;
            crate::serial_println!("[fpu] ECHEC : {}", what);
        }
    };

    check(supported(), "FXSR, SSE et SSE2 annonces par CPUID");
    check(enabled(), "CR4.OSFXSR arme sur ce coeur");
    check(INITIAL.len() == AREA_SIZE, "image initiale de 512 octets");

    // Three .cus tasks share the upper half of the cores (spec 7.2): with
    // one or two cores there, at least two of them alternate on the same
    // core, which is what the test needs.
    let entries: [fn() -> !; 3] = [worker_a, worker_b, worker_c];
    let names = ["fpu-a", "fpu-b", "fpu-c"];
    let mut pids = [0; 3];
    for (slot, (entry, name)) in pids.iter_mut().zip(entries.into_iter().zip(names)) {
        *slot = crate::scheduler::spawn(name, crate::process::BinaryType::Cus, entry);
    }
    for pid in pids {
        crate::scheduler::wait_for(pid);
    }
    check(FINISHED.load(Ordering::SeqCst) == 3, "les trois taches de calcul ont fini");
    let corrupted = CORRUPTED.load(Ordering::SeqCst);
    check(corrupted == 0, "registres XMM et MXCSR intacts apres chaque commutation");
    crate::serial_println!(
        "[fpu] 3 taches x {} commutations - {} etat(s) corrompu(s)",
        ROUNDS,
        corrupted
    );
    (ok, failed)
}

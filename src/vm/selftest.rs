//! Self-test of private address spaces (phase 11.3).
//!
//! Runs the ring 3 programs of `boot/vmtest.asm`, each in a space of its
//! own, and checks what only a working implementation can produce:
//! pages that read zero before their first write, two processes that
//! write different values at the same address and each read back their
//! own, faults that end one task and leave the core running, a program
//! that never makes a system call and still lets its core serve others,
//! and a kernel copy from a bad user address that returns an error
//! instead of faulting.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use super::{Backing, PAGE, Pager, Prot, Space, UserRegs};
use crate::process::BinaryType;

unsafe extern "C" {
    static vmtest_start: u8;
    static vmtest_end: u8;
    static vmtest_zero: u8;
    static vmtest_isolation: u8;
    static vmtest_spin: u8;
    static vmtest_null: u8;
    static vmtest_write_ro: u8;
    static vmtest_sse: u8;
}

/// Where the test program is mapped in every space.
const CODE: u64 = 0x40_0000;
const STACK_TOP: u64 = 0x7000_0000_0000;
const STACK_SIZE: u64 = 64 * 1024;
const DATA: u64 = 0x1000_0000;

/// Serves the bytes of the program, read-only, to the fault handler.
struct StaticPager(&'static [u8]);

impl Pager for StaticPager {
    fn fill(&self, offset: u64, page: &mut [u8]) -> bool {
        let start = (offset as usize).min(self.0.len());
        let end = (start + page.len()).min(self.0.len());
        page[..end - start].copy_from_slice(&self.0[start..end]);
        true
    }
}

fn program() -> &'static [u8] {
    let start = &raw const vmtest_start as usize;
    let end = &raw const vmtest_end as usize;
    unsafe { core::slice::from_raw_parts(start as *const u8, end - start) }
}

fn entry(symbol: *const u8) -> u64 {
    CODE + (symbol as u64 - &raw const vmtest_start as u64)
}

/// A space holding the program and a stack.
fn new_space() -> Option<Arc<Space>> {
    let space = Space::new()?;
    let code = program();
    space
        .map(CODE, code.len() as u64, Prot::RX, Backing::File { pager: Arc::new(StaticPager(code)), offset: 0, shared: false }, true, "vmtest")
        .ok()?;
    space.map(STACK_TOP - STACK_SIZE, STACK_SIZE, Prot::RW, Backing::Anon, true, "[stack]").ok()?;
    Some(space)
}

fn start(name: &str, space: &Arc<Space>, symbol: *const u8, rsi: u64, rdx: u64) -> u64 {
    let regs = UserRegs { rip: entry(symbol), rsp: STACK_TOP - 16, rflags: 0x202, rsi, rdx, ..Default::default() };
    super::spawn_user(name, BinaryType::Cus, space.clone(), regs, 0)
}

static CHECKER_RAN: AtomicBool = AtomicBool::new(false);

fn checker() -> ! {
    CHECKER_RAN.store(true, Ordering::SeqCst);
    crate::scheduler::exit_current_with(0)
}

pub fn run() -> (u32, u32) {
    let mut ok = 0u32;
    let mut failed = 0u32;
    let mut check = |cond: bool, what: &str| {
        if cond {
            ok += 1;
        } else {
            failed += 1;
            crate::serial_println!("[vm] ECHEC : {}", what);
        }
    };

    check(super::kernel_pml4() != 0, "PML4 du noyau relevee");
    let (free_before, _, _) = (crate::frame::stats().2, 0, 0);

    // --- regions, without running anything ---
    if let Some(space) = Space::new() {
        let a = space.map(0, 3 * PAGE, Prot::RW, Backing::Anon, false, "a");
        let b = space.map(0, PAGE, Prot::RW, Backing::Anon, false, "b");
        check(a.is_ok() && b.is_ok() && a != b, "deux regions placees sans chevauchement");
        let a = a.unwrap_or(0);
        check(space.map_exact(a, PAGE, Prot::RW, Backing::Anon, "c") == Err(super::VmError::Exists), "une region deja prise est refusee");
        check(space.protect(a + PAGE, PAGE, Prot::R).is_ok() && space.stats().0 == 4, "mprotect d'une page centrale coupe la region en trois");
        check(space.unmap(a, 3 * PAGE).is_ok() && space.stats().0 == 1, "munmap retire les trois morceaux");
        check(space.map(PAGE, PAGE, Prot::RW, Backing::Anon, true, "bas") == Err(super::VmError::Invalid), "les 64 premiers Kio restent interdits");
    } else {
        check(false, "creation d'un espace");
    }

    // --- demand-zero pages ---
    match new_space() {
        Some(space) => {
            let pages = 256;
            let mapped = space.map(DATA, pages * PAGE, Prot::RW, Backing::Anon, true, "data").is_ok();
            check(mapped && space.stats().1 == 0, "une region reservee n'occupe aucun cadre");
            let pid = start("vm-zero", &space, &raw const vmtest_zero, DATA, pages);
            let code = crate::scheduler::wait_for(pid);
            check(code == Some(0), "pages a zero avant la premiere ecriture, puis relues");
            let (_, resident, faults) = space.stats();
            crate::serial_println!("[vm] 256 pages touchees : {} cadres residents, {} fautes resolues", resident, faults);
            check(faults >= pages && resident as u64 >= pages, "une faute resolue par page touchee");
        }
        None => check(false, "creation d'un espace"),
    }

    // --- isolation: same address, different values ---
    match (new_space(), new_space()) {
        (Some(one), Some(two)) => {
            let m1 = one.map(DATA, PAGE, Prot::RW, Backing::Anon, true, "data").is_ok();
            let m2 = two.map(DATA, PAGE, Prot::RW, Backing::Anon, true, "data").is_ok();
            let p1 = start("vm-iso-a", &one, &raw const vmtest_isolation, DATA, 0xAAAA_0001);
            let p2 = start("vm-iso-b", &two, &raw const vmtest_isolation, DATA, 0xBBBB_0002);
            let c1 = crate::scheduler::wait_for(p1);
            let c2 = crate::scheduler::wait_for(p2);
            check(m1 && m2 && c1 == Some(0) && c2 == Some(0), "deux processus, meme adresse, chacun relit sa valeur");
        }
        _ => check(false, "creation de deux espaces"),
    }

    // --- SSE in ring 3 ---
    match (new_space(), new_space()) {
        (Some(one), Some(two)) => {
            let p1 = start("vm-sse-a", &one, &raw const vmtest_sse, 0, 0x1111_0000_0000_0000);
            let p2 = start("vm-sse-b", &two, &raw const vmtest_sse, 0, 0x2222_0000_0000_0000);
            let c1 = crate::scheduler::wait_for(p1);
            let c2 = crate::scheduler::wait_for(p2);
            check(c1 == Some(0) && c2 == Some(0), "registres XMM conserves en ring 3 a travers 200 commutations");
        }
        _ => check(false, "creation de deux espaces"),
    }

    // --- faults end the task, not the core ---
    let kills_before = crate::idt::user_kills();
    if let Some(space) = new_space() {
        let pid = start("vm-null", &space, &raw const vmtest_null, 0, 0);
        check(crate::scheduler::wait_for(pid) == Some(-142), "lecture de l'adresse 0 : tache terminee (code -142)");
    }
    if let Some(space) = new_space() {
        let ro = 0x3000_0000;
        let mapped = space.map(ro, PAGE, Prot::R, Backing::Anon, true, "ro").is_ok();
        let pid = start("vm-ro", &space, &raw const vmtest_write_ro, ro, 0);
        check(mapped && crate::scheduler::wait_for(pid) == Some(-142), "ecriture dans une region en lecture seule : tache terminee");
    }
    check(crate::idt::user_kills() >= kills_before + 2, "les deux fautes comptees, le coeur continue");

    // --- preemption of ring 3 ---
    CHECKER_RAN.store(false, Ordering::SeqCst);
    if let Some(space) = new_space() {
        let pid = start("vm-spin", &space, &raw const vmtest_spin, 0, 0);
        let core = crate::scheduler::task_profile(pid).map(|p| p.2).unwrap_or(0);
        let deadline = crate::idt::ticks() + 300;
        // Let the spinner get going before asking for its core.
        let warm = crate::idt::ticks() + 5;
        while crate::idt::ticks() < warm {
            crate::scheduler::yield_now();
        }
        let checker_pid = crate::scheduler::spawn_pinned("vm-voisin", BinaryType::Cus, checker, core);
        while !CHECKER_RAN.load(Ordering::SeqCst) && crate::idt::ticks() < deadline {
            crate::scheduler::yield_now();
        }
        let ran = CHECKER_RAN.load(Ordering::SeqCst);
        crate::serial_println!(
            "[vm] boucle ring 3 sans appel systeme sur le coeur {} - voisin {}",
            core,
            if ran { "execute" } else { "affame" }
        );
        check(ran, "un voisin s'execute sur le coeur d'une boucle ring 3 (preemption)");
        crate::scheduler::kill(pid, -9);
        check(crate::scheduler::wait_for(pid) == Some(-9), "la boucle ring 3 se termine sur demande");
        crate::scheduler::wait_for(checker_pid);
    }

    // --- kernel copy from a bad user address ---
    let mut buffer = [0u8; 16];
    let before = super::fault_stats().1;
    check(super::copy_from_user(&mut buffer, 0x6000_0000_0000).is_err(), "copie depuis une adresse non mappee : erreur, pas de faute noyau");
    check(super::fault_stats().1 == before + 1, "rattrapage par la table d'exception de user_copy");
    check(super::copy_from_user(&mut buffer, 0xFFFF_8000_0000_0000).is_err(), "copie depuis une adresse noyau refusee");

    // --- everything given back ---
    let (resolved, recovered, killed) = super::fault_stats();
    let free_after = crate::frame::stats().2;
    crate::serial_println!(
        "[vm] fautes : {} resolues, {} rattrapees, {} fatales - espaces vivants : {} - cadres libres {} -> {}",
        resolved,
        recovered,
        killed,
        super::live_spaces(),
        free_before,
        free_after
    );
    check(super::live_spaces() == 0, "tous les espaces liberes apres la fin de leurs taches");
    (ok, failed)
}

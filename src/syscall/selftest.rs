//! Phase 5 self-test -- the system API exercised call by call.
//!
//! ## Why from inside the kernel
//!
//! Writing one ring 3 program per call would demand eighty binaries, and
//! checking their results would assume a return channel that would
//! itself have to be tested. `syscall::invoke` takes exactly the same
//! path -- same table, same rights, same modes -- differing only on the
//! provenance of the pointers.
//!
//! That difference is itself tested: a set of cases checks that a call of
//! **user origin** carrying a kernel pointer is refused. It is the only
//! place where the two origins diverge, and therefore the only place
//! where it must be proven that they diverge the right way.
//!
//! ## What the self-test does not cover
//!
//! The real passage through the `syscall` instruction from ring 3: it is
//! exercised by the demonstration program of `boot/user.asm`, whose
//! serial output is the proof. Here we check the semantics, there the
//! mechanism.

use alloc::string::String;
use alloc::vec::Vec;

use super::error::ErrorCode;
use super::{Call, Mode, Origin, SysResult, domain_core, domain_fs, domain_hw, domain_ipc};
use super::{domain_mem, domain_net, domain_proc, domain_time, domain_user, invoke, table};
use crate::fs::perm::{Credentials, Permissions};

struct Report {
    passed: usize,
    failed: usize,
}

impl Report {
    fn check(&mut self, label: &str, condition: bool) {
        if condition {
            self.passed += 1;
        } else {
            self.failed += 1;
            crate::serial_println!("[api] ECHEC : {}", label);
        }
    }

    /// Checks that a call succeeds, and returns its value.
    fn ok(&mut self, label: &str, result: SysResult<u64>) -> Option<u64> {
        match result {
            Ok(value) => {
                self.passed += 1;
                Some(value)
            }
            Err(error) => {
                self.failed += 1;
                crate::serial_println!(
                    "[api] ECHEC : {} - {} ({})",
                    label,
                    error.name(),
                    error.context()
                );
                None
            }
        }
    }

    /// Checks that a call fails, and for the right reason.
    fn refused(&mut self, label: &str, result: SysResult<u64>, expected: ErrorCode) {
        match result {
            Ok(value) => {
                self.failed += 1;
                crate::serial_println!("[api] ECHEC : {} aurait du echouer (rendu {})", label, value);
            }
            Err(error) if error.code == expected => {
                self.passed += 1;
                crate::serial_println!("[api]   {} -> {} : {}", label, error.name(), error.context());
            }
            Err(error) => {
                self.failed += 1;
                crate::serial_println!(
                    "[api] ECHEC : {} a rendu {} au lieu de {}",
                    label,
                    error.name(),
                    expected.name()
                );
            }
        }
    }
}

/// Shorthand: a call with no argument.
fn call0(number: u16) -> SysResult<u64> {
    invoke(number, [0; 6])
}

/// Shorthand: a call whose arguments are given at the head.
fn call(number: u16, args: &[u64]) -> SysResult<u64> {
    let mut full = [0u64; 6];
    for (slot, value) in full.iter_mut().zip(args.iter()) {
        *slot = *value;
    }
    invoke(number, full)
}

/// Passes a path, then any extra arguments.
fn call_path(number: u16, path: &str, rest: &[u64]) -> SysResult<u64> {
    let mut args = [0u64; 6];
    args[0] = path.as_ptr() as u64;
    args[1] = path.len() as u64;
    for (index, value) in rest.iter().enumerate() {
        if index + 2 < 6 {
            args[index + 2] = *value;
        }
    }
    invoke(number, args)
}

/// Runs the complete self-test. Returns (passed, failed).
pub fn run() -> (usize, usize) {
    let mut report = Report {
        passed: 0,
        failed: 0,
    };

    crate::serial_println!("[api] --- API systeme (spec 6) ---");

    infrastructure(&mut report);
    pointers(&mut report);
    time_domain(&mut report);
    memory_domain(&mut report);
    ipc_domain(&mut report);
    user_domain(&mut report);
    hardware_domain(&mut report);
    network_domain(&mut report);
    filesystem_domain(&mut report);
    node_calls(&mut report);
    process_domain(&mut report);
    spawn_cycle(&mut report);
    query_respond(&mut report);
    identity_descent(&mut report);
    shared_memory(&mut report);

    crate::serial_println!(
        "[api] autotest termine : {} reussites, {} echecs",
        report.passed,
        report.failed
    );
    (report.passed, report.failed)
}

// ============================================================
//  5.1 -- Infrastructure
// ============================================================

fn infrastructure(report: &mut Report) {
    crate::serial_println!("[api] --- 5.1 infrastructure et table de dispatch ---");

    report.check("les tables sont bien formees", table::validate().is_ok());

    // Every declared number must be found again, and land on its own
    // entry: that is what proves the binary search reads the table in the
    // right order.
    let mut resolved = 0;
    for (_, domain) in table::DOMAINS.iter() {
        for item in domain.iter() {
            match table::lookup(item.number) {
                Some(found) if found.number == item.number => resolved += 1,
                _ => {}
            }
        }
    }
    report.check(
        "chaque appel declare est resolu par la table",
        resolved == table::count(),
    );

    crate::serial_println!(
        "[api]   {} appels sur {} domaines, dont {} asynchrones",
        table::count(),
        table::DOMAINS.len(),
        table::DOMAINS
            .iter()
            .flat_map(|(_, d)| d.iter())
            .filter(|e| e.mode == Mode::Async)
            .count()
    );

    report.refused(
        "numero inconnu",
        call0(0x09FF),
        ErrorCode::InvalidSyscall,
    );
    report.refused(
        "domaine inexistant",
        call0(0x1234),
        ErrorCode::InvalidSyscall,
    );

    // 5.1.2: the structured error must be recoverable in its complete
    // form, context included.
    let mut record = [0u8; domain_core::ERROR_RECORD_SIZE];
    let written = report.ok(
        "SYS_ERROR_LAST",
        call(
            domain_core::SYS_ERROR_LAST,
            &[record.as_mut_ptr() as u64, record.len() as u64],
        ),
    );
    report.check(
        "l'enregistrement d'erreur est complet",
        written == Some(domain_core::ERROR_RECORD_SIZE as u64),
    );

    let code = u32::from_le_bytes([record[0], record[1], record[2], record[3]]);
    let number = u16::from_le_bytes([record[4], record[5]]);
    let name_len = record[6] as usize;
    let context_len = record[8] as usize;
    report.check(
        "le code retenu est celui du dernier echec",
        code == ErrorCode::InvalidSyscall as u32 && number == 0x1234,
    );
    report.check("le nom symbolique est present", name_len > 0);
    report.check("le contexte est present", context_len > 0);
    crate::serial_println!(
        "[api]   derniere erreur : {} sur 0x{:04x} - contexte \"{}\"",
        core::str::from_utf8(&record[0x10..0x10 + name_len]).unwrap_or("?"),
        number,
        core::str::from_utf8(&record[0x70..0x70 + context_len]).unwrap_or("?")
    );

    // 5.1.3: the API must be able to describe itself.
    let mut entries = alloc::vec![0u8; 8 * domain_core::API_ENTRY_SIZE];
    let total = report.ok(
        "SYS_API_INFO",
        call(
            domain_core::SYS_API_INFO,
            &[entries.as_mut_ptr() as u64, entries.len() as u64],
        ),
    );
    report.check(
        "le total annonce correspond a la table",
        total == Some(table::count() as u64),
    );
    let first = u16::from_le_bytes([entries[0], entries[1]]);
    report.check(
        "la description commence au premier appel declare",
        first == domain_core::SYS_DEBUG_PRINT,
    );

    // 5.1.4: a synchronous call must not be able to block. The check
    // bears on the mechanism itself: a `Call` built in synchronous mode
    // must refuse the request.
    let synchronous = Call {
        number: domain_proc::SYS_PROC_SELF,
        args: [0; 6],
        origin: Origin::Kernel,
        mode: Mode::Sync,
        pid: crate::scheduler::current_pid(),
    };
    report.check(
        "un appel synchrone refuse de bloquer",
        synchronous.require_async("test").is_err(),
    );
    let asynchronous = Call {
        mode: Mode::Async,
        ..synchronous
    };
    report.check(
        "un appel asynchrone accepte de bloquer",
        asynchronous.require_async("test").is_ok(),
    );
}

// ============================================================
//  Boundary of the spaces
// ============================================================

fn pointers(report: &mut Report) {
    crate::serial_println!("[api] --- frontiere ring 0 / ring 3 ---");

    // A kernel pointer presented by a call of user origin must be
    // refused. It is the only point where the two origins diverge.
    let mut buffer = [0u8; 64];
    let kernel_pointer = buffer.as_mut_ptr() as u64;

    let from_user = Call {
        number: domain_core::SYS_ERROR_LAST,
        args: [kernel_pointer, buffer.len() as u64, 0, 0, 0, 0],
        origin: Origin::User,
        mode: Mode::Sync,
        pid: crate::scheduler::current_pid(),
    };
    report.check(
        "un pointeur noyau est refuse a un appelant ring 3",
        from_user.write_bytes(kernel_pointer, 8, &[0u8; 8]).is_err(),
    );
    report.check(
        "la lecture d'un pointeur noyau est refusee de meme",
        from_user.read_bytes(kernel_pointer, 8).is_err(),
    );

    // The non-canonical high address must fall before the translation.
    report.check(
        "une adresse hors de l'espace utilisateur est rejetee",
        super::validate_user_range(0xFFFF_FFFF_8000_0000, 8, false).is_err(),
    );
    report.check(
        "une adresse basse non mappee est rejetee",
        super::validate_user_range(0x0000_7FFF_0000_0000, 8, false).is_err(),
    );

    // An outsized transfer request is refused before any allocation:
    // that is what stops ring 3 dictating the size of a kernel buffer.
    // `SYS_IPC_SEND` serves as the carrier because it does not truncate
    // the length asked for, unlike the trace.
    report.refused(
        "transfert au-dela de la borne",
        call(
            domain_ipc::SYS_IPC_SEND,
            &[
                crate::scheduler::current_pid(),
                kernel_pointer,
                super::TRANSFER_MAX + 1,
                0,
            ],
        ),
        ErrorCode::TooLarge,
    );
}

// ============================================================
//  5.6.3 -- Time
// ============================================================

fn time_domain(report: &mut Report) {
    crate::serial_println!("[api] --- 5.6.3 temps ---");

    let first = report.ok("SYS_TIME_TICKS", call0(domain_time::SYS_TIME_TICKS));
    report.ok("SYS_TIME_UPTIME_MS", call0(domain_time::SYS_TIME_UPTIME_MS));
    let resolution = report.ok(
        "SYS_TIME_RESOLUTION",
        call0(domain_time::SYS_TIME_RESOLUTION),
    );
    report.check(
        "la resolution annoncee est celle du timer",
        resolution == Some(crate::TIMER_HZ as u64),
    );

    if crate::rtc::is_available() {
        let epoch = report.ok("SYS_TIME_NOW", call0(domain_time::SYS_TIME_NOW));
        report.check(
            "l'horloge murale est posterieure a 2024",
            epoch.map(|value| value > 1_704_067_200).unwrap_or(false),
        );

        let mut record = [0u8; domain_time::DATE_SIZE];
        report.ok(
            "SYS_TIME_DATE",
            call(
                domain_time::SYS_TIME_DATE,
                &[record.as_mut_ptr() as u64, record.len() as u64],
            ),
        );
        let year = u16::from_le_bytes([record[8], record[9]]);
        report.check(
            "la date decomposee est coherente",
            year >= 2024 && record[0x0A] >= 1 && record[0x0A] <= 12,
        );
        crate::serial_println!(
            "[api]   horloge murale : {}",
            core::str::from_utf8(&record[0x10..0x10 + 19]).unwrap_or("?")
        );
    } else {
        // With no clock, the wall calls must refuse outright rather than
        // invent a date.
        report.refused(
            "SYS_TIME_NOW sans horloge",
            call0(domain_time::SYS_TIME_NOW),
            ErrorCode::Unsupported,
        );
    }

    // The tick counter must advance: a stopped timer would make all the
    // rest wrong with nothing to signal it.
    let second = call0(domain_time::SYS_TIME_TICKS).unwrap_or(0);
    report.check(
        "le compteur de tics ne recule pas",
        second >= first.unwrap_or(0),
    );

    report.refused(
        "sommeil au-dela d'une heure",
        call(domain_time::SYS_TIME_SLEEP_MS, &[7_200_000]),
        ErrorCode::InvalidArgument,
    );
}

// ============================================================
//  5.4 -- Memory
// ============================================================

fn memory_domain(report: &mut Report) {
    crate::serial_println!("[api] --- 5.4 memoire ---");

    let (_, _, free_before) = crate::frame::stats();

    let base = report.ok(
        "SYS_MEM_ALLOC de 12 Kio",
        call(domain_mem::SYS_MEM_ALLOC, &[12_288, 0]),
    );
    let Some(base) = base else {
        return;
    };
    report.check(
        "l'allocation tombe dans l'arene utilisateur",
        base >= domain_mem::HEAP_ARENA_BASE
            && base < domain_mem::HEAP_ARENA_BASE + domain_mem::HEAP_ARENA_SIZE,
    );

    // The area must really be writable, and zeroed: a recycled page
    // keeping the content of another task would be an information leak.
    let slice = unsafe { core::slice::from_raw_parts_mut(base as *mut u8, 12_288) };
    report.check("la zone allouee est remise a zero", slice.iter().all(|b| *b == 0));
    slice[0] = 0xA5;
    slice[12_287] = 0x5A;
    report.check(
        "la zone allouee est inscriptible de bout en bout",
        slice[0] == 0xA5 && slice[12_287] == 0x5A,
    );

    // The pages must carry the USER bit: without it, ring 3 could not use
    // them, and the allocation would be pointless.
    let translation = unsafe { crate::memory::translate(base) };
    report.check(
        "les pages allouees sont accessibles au ring 3",
        translation.map(|t| t.user && t.writable).unwrap_or(false),
    );
    report.check(
        "les pages de donnees ne sont pas executables",
        !crate::memory::nx_enabled()
            || unsafe { crate::memory::translate(base) }
                .map(|t| !t.executable)
                .unwrap_or(false),
    );

    // Growing then shrinking: the content must survive the first, and the
    // pages come back at the second.
    let grown = report.ok(
        "SYS_MEM_RESIZE vers 32 Kio",
        call(domain_mem::SYS_MEM_RESIZE, &[base, 32_768]),
    );
    if let Some(grown) = grown {
        let kept = unsafe { core::ptr::read(grown as *const u8) };
        report.check("le contenu survit a l'agrandissement", kept == 0xA5);

        let shrunk = report.ok(
            "SYS_MEM_RESIZE vers 4 Kio",
            call(domain_mem::SYS_MEM_RESIZE, &[grown, 4096]),
        );

        // W^X: a writable area cannot become executable.
        if let Some(shrunk) = shrunk {
            report.ok(
                "SYS_MEM_PROTECT en lecture seule",
                call(domain_mem::SYS_MEM_PROTECT, &[shrunk, 4096, 1]),
            );
            let readonly = unsafe { crate::memory::translate(shrunk) };
            report.check(
                "la protection en lecture seule est effective",
                readonly.map(|t| !t.writable).unwrap_or(false),
            );

            report.ok(
                "SYS_MEM_LOCK",
                call(domain_mem::SYS_MEM_LOCK, &[shrunk, 4096, 1]),
            );
            report.refused(
                "SYS_MEM_PROTECT sans lecture",
                call(domain_mem::SYS_MEM_PROTECT, &[shrunk, 4096, 2]),
                ErrorCode::Unsupported,
            );
            report.refused(
                "SYS_MEM_FREE d'une adresse inconnue",
                call(domain_mem::SYS_MEM_FREE, &[shrunk + 0x10_0000]),
                ErrorCode::NotFound,
            );
            report.ok("SYS_MEM_FREE", call(domain_mem::SYS_MEM_FREE, &[shrunk]));
        }
    }

    report.refused(
        "allocation demesuree",
        call(domain_mem::SYS_MEM_ALLOC, &[domain_mem::ALLOC_MAX + 1, 0]),
        ErrorCode::TooLarge,
    );
    report.refused(
        "allocation de taille nulle",
        call(domain_mem::SYS_MEM_ALLOC, &[0, 0]),
        ErrorCode::InvalidArgument,
    );

    // The invariant that counts: everything taken has been given back.
    let (_, _, free_after) = crate::frame::stats();
    report.check(
        "les cadres pris par l'arene sont tous rendus",
        free_after == free_before,
    );
    if free_after != free_before {
        crate::serial_println!(
            "[api]   cadres libres : {} avant, {} apres",
            free_before,
            free_after
        );
    }

    let mut record = [0u8; domain_mem::MEM_STAT_SIZE];
    report.ok(
        "SYS_MEM_STAT",
        call(
            domain_mem::SYS_MEM_STAT,
            &[record.as_mut_ptr() as u64, record.len() as u64],
        ),
    );
    let pages = u64::from_le_bytes(record[0..8].try_into().unwrap_or([0; 8]));
    report.check("l'arene est revenue a vide", pages == 0);
}

// ============================================================
//  5.5 -- IPC
// ============================================================

fn ipc_domain(report: &mut Report) {
    crate::serial_println!("[api] --- 5.5 communication ---");

    let me = crate::scheduler::current_pid();

    // A send to oneself is the only way to check the complete loop
    // without depending on a second task.
    let payload = b"message de l'autotest";
    let sent = report.ok(
        "SYS_IPC_SEND",
        call(
            domain_ipc::SYS_IPC_SEND,
            &[me, payload.as_ptr() as u64, payload.len() as u64, 0x1234],
        ),
    );
    report.check("l'envoi annonce la taille emise", sent == Some(payload.len() as u64));

    let mut buffer = [0u8; domain_ipc::MESSAGE_HEADER + 64];
    let received = report.ok(
        "SYS_IPC_RECV",
        call(
            domain_ipc::SYS_IPC_RECV,
            &[buffer.as_mut_ptr() as u64, buffer.len() as u64, 0],
        ),
    );
    report.check(
        "la reception rend l'en-tete et la charge",
        received == Some((domain_ipc::MESSAGE_HEADER + payload.len()) as u64),
    );

    let sender = u64::from_le_bytes(buffer[0..8].try_into().unwrap_or([0; 8]));
    let tag = u32::from_le_bytes(buffer[8..12].try_into().unwrap_or([0; 4]));
    let length = u16::from_le_bytes(buffer[14..16].try_into().unwrap_or([0; 2]));
    report.check("l'en-tete porte l'expediteur", sender == me);
    report.check("l'en-tete porte l'etiquette", tag == 0x1234);
    report.check(
        "la charge utile est intacte",
        length as usize == payload.len()
            && &buffer[domain_ipc::MESSAGE_HEADER..domain_ipc::MESSAGE_HEADER + payload.len()]
                == payload,
    );

    report.refused(
        "reception non bloquante sur boite vide",
        call(
            domain_ipc::SYS_IPC_RECV,
            &[buffer.as_mut_ptr() as u64, buffer.len() as u64, 0],
        ),
        ErrorCode::WouldBlock,
    );
    report.refused(
        "envoi vers un PID inexistant",
        call(
            domain_ipc::SYS_IPC_SEND,
            &[99_999, payload.as_ptr() as u64, payload.len() as u64, 0],
        ),
        ErrorCode::NotFound,
    );

    // Named channels: it is what lifts the limit "a ring 3 program cannot
    // resolve a service by its name".
    let name = "nova.autotest";
    let channel = report.ok(
        "SYS_IPC_OPEN",
        call(
            domain_ipc::SYS_IPC_OPEN,
            &[name.as_ptr() as u64, name.len() as u64],
        ),
    );
    report.refused(
        "ouverture d'un canal deja pris",
        call(
            domain_ipc::SYS_IPC_OPEN,
            &[name.as_ptr() as u64, name.len() as u64],
        ),
        ErrorCode::AlreadyExists,
    );
    let subscribed = report.ok(
        "SYS_IPC_SUBSCRIBE par nom",
        call(
            domain_ipc::SYS_IPC_SUBSCRIBE,
            &[name.as_ptr() as u64, name.len() as u64],
        ),
    );
    report.check(
        "l'abonnement resout le meme canal que l'ouverture",
        subscribed == channel,
    );
    report.refused(
        "abonnement a un canal inconnu",
        call(
            domain_ipc::SYS_IPC_SUBSCRIBE,
            &["nova.absent".as_ptr() as u64, 11],
        ),
        ErrorCode::NotFound,
    );

    if let Some(channel) = channel {
        report.ok(
            "SYS_IPC_PUBLISH",
            call(
                domain_ipc::SYS_IPC_PUBLISH,
                &[channel, payload.as_ptr() as u64, payload.len() as u64, 7],
            ),
        );
        // An API does not query itself: the owner of the channel would
        // receive its own request and wait for its own answer.
        report.refused(
            "SYS_IPC_QUERY vers sa propre API",
            call(
                domain_ipc::SYS_IPC_QUERY,
                &[name.as_ptr() as u64, name.len() as u64, payload.as_ptr() as u64, 4],
            ),
            ErrorCode::InvalidArgument,
        );
        report.ok("SYS_IPC_CLOSE", call(domain_ipc::SYS_IPC_CLOSE, &[channel]));
        report.refused(
            "fermeture d'un canal deja ferme",
            call(domain_ipc::SYS_IPC_CLOSE, &[channel]),
            ErrorCode::NotFound,
        );
    }

    // The box is emptied: a message published to oneself must not
    // falsify the tests that follow.
    while crate::ipc::try_recv().is_some() {}
}

// ============================================================
//  5.6.1 -- Accounts
// ============================================================

fn user_domain(report: &mut Report) {
    crate::serial_println!("[api] --- 5.6.1 comptes et permissions ---");

    let mut record = [0u8; domain_user::ACCOUNT_SIZE];
    report.ok(
        "SYS_USER_SELF",
        call(
            domain_user::SYS_USER_SELF,
            &[record.as_mut_ptr() as u64, record.len() as u64],
        ),
    );
    let uid = u32::from_le_bytes(record[0..4].try_into().unwrap_or([0; 4]));
    report.check(
        "l'autotest s'execute sous le compte du noyau",
        uid == crate::fs::perm::ROOT_UID,
    );

    let count = report.ok(
        "SYS_USER_LIST",
        call(
            domain_user::SYS_USER_LIST,
            &[record.as_mut_ptr() as u64, record.len() as u64],
        ),
    );
    report.check(
        "la table des comptes est peuplee",
        count.map(|value| value >= 4).unwrap_or(false),
    );

    let mut found = [0u8; domain_user::ACCOUNT_SIZE];
    let name = "admin";
    report.ok(
        "SYS_USER_LOOKUP",
        call(
            domain_user::SYS_USER_LOOKUP,
            &[
                name.as_ptr() as u64,
                name.len() as u64,
                found.as_mut_ptr() as u64,
                found.len() as u64,
            ],
        ),
    );
    report.check(
        "le compte admin porte le role ADMIN",
        found[8] == crate::fs::perm::Role::Admin as u8,
    );
    report.refused(
        "recherche d'un compte inexistant",
        call(
            domain_user::SYS_USER_LOOKUP,
            &["fantome".as_ptr() as u64, 7, found.as_mut_ptr() as u64, found.len() as u64],
        ),
        ErrorCode::NotFound,
    );

    // Creation and deletion, reserved to ADMIN and ROOT.
    let created = "autotest";
    report.ok(
        "SYS_USER_CREATE",
        call(
            domain_user::SYS_USER_CREATE,
            &[
                created.as_ptr() as u64,
                created.len() as u64,
                4242,
                100,
                crate::fs::perm::Role::User as u64,
            ],
        ),
    );
    report.refused(
        "creation d'un compte deja pris",
        call(
            domain_user::SYS_USER_CREATE,
            &[
                created.as_ptr() as u64,
                created.len() as u64,
                4243,
                100,
                crate::fs::perm::Role::User as u64,
            ],
        ),
        ErrorCode::AlreadyExists,
    );
    report.refused(
        "role inconnu",
        call(
            domain_user::SYS_USER_CREATE,
            &["autre".as_ptr() as u64, 5, 4244, 100, 99],
        ),
        ErrorCode::InvalidArgument,
    );
    report.ok(
        "SYS_USER_DELETE",
        call(
            domain_user::SYS_USER_DELETE,
            &[created.as_ptr() as u64, created.len() as u64],
        ),
    );
    // The self-test runs under ROOT: deleting "root" is therefore a
    // self-deletion, refused as such. An account deleting itself would
    // leave live processes with no identity.
    report.refused(
        "suppression de son propre compte",
        call(domain_user::SYS_USER_DELETE, &["root".as_ptr() as u64, 4]),
        ErrorCode::InvalidArgument,
    );
    report.refused(
        "suppression d'un compte inexistant",
        call(domain_user::SYS_USER_DELETE, &["fantome".as_ptr() as u64, 7]),
        ErrorCode::NotFound,
    );

    // 5.6.1: the right is read, it is not declared.
    if crate::fs::is_ready() {
        let granted = report.ok(
            "SYS_USER_CAN sur un dossier systeme",
            call_path(domain_user::SYS_USER_CAN, "/apps", &[Permissions::ACCESS]),
        );
        report.check(
            "le noyau peut parcourir /apps",
            granted == Some(1),
        );
        let absent = report.ok(
            "SYS_USER_CAN sur un chemin inexistant",
            call_path(domain_user::SYS_USER_CAN, "/absent", &[Permissions::READ]),
        );
        report.check("un chemin inexistant n'accorde rien", absent == Some(0));
        report.refused(
            "SYS_USER_CAN avec deux permissions",
            call_path(
                domain_user::SYS_USER_CAN,
                "/apps",
                &[Permissions::READ | Permissions::WRITE],
            ),
            ErrorCode::InvalidArgument,
        );
    }
}

// ============================================================
//  5.6.2 -- Hardware
// ============================================================

fn hardware_domain(report: &mut Report) {
    crate::serial_println!("[api] --- 5.6.2 materiel ---");

    let mut record = [0u8; domain_hw::HW_INFO_SIZE];
    report.ok(
        "SYS_HW_INFO",
        call(
            domain_hw::SYS_HW_INFO,
            &[record.as_mut_ptr() as u64, record.len() as u64],
        ),
    );
    let frames = u64::from_le_bytes(record[0..8].try_into().unwrap_or([0; 8]));
    let cores = u32::from_le_bytes(record[0x28..0x2C].try_into().unwrap_or([0; 4]));
    report.check("le nombre de cadres est plausible", frames > 1024);
    report.check(
        "le nombre de coeurs correspond au demarrage SMP",
        cores as usize == crate::smp_core_count(),
    );

    let mut cpus = alloc::vec![0u8; 8 * domain_hw::CPU_ENTRY_SIZE];
    let listed = report.ok(
        "SYS_HW_CPU_LIST",
        call(
            domain_hw::SYS_HW_CPU_LIST,
            &[cpus.as_mut_ptr() as u64, cpus.len() as u64],
        ),
    );
    report.check(
        "chaque coeur en ligne est decrit",
        listed == Some(crate::smp_core_count() as u64),
    );

    let mut memory = [0u8; domain_hw::HW_MEMORY_SIZE];
    report.ok(
        "SYS_HW_MEMORY",
        call(
            domain_hw::SYS_HW_MEMORY,
            &[memory.as_mut_ptr() as u64, memory.len() as u64],
        ),
    );
    let total = u64::from_le_bytes(memory[0..8].try_into().unwrap_or([0; 8]));
    let used = u64::from_le_bytes(memory[8..16].try_into().unwrap_or([0; 8]));
    let free = u64::from_le_bytes(memory[16..24].try_into().unwrap_or([0; 8]));
    report.check("les comptes de cadres se recoupent", used + free == total);

    let mut devices = alloc::vec![0u8; 16 * domain_hw::PCI_ENTRY_SIZE];
    let pci = report.ok(
        "SYS_HW_PCI_LIST",
        call(
            domain_hw::SYS_HW_PCI_LIST,
            &[devices.as_mut_ptr() as u64, devices.len() as u64],
        ),
    );
    report.check(
        "le bus PCI expose au moins un peripherique",
        pci.map(|value| value > 0).unwrap_or(false),
    );
}

// ============================================================
//  5.6.4 -- Network
// ============================================================

fn network_domain(report: &mut Report) {
    crate::serial_println!("[api] --- 5.6.4 reseau ---");

    let mut record = [0u8; domain_net::NET_STATUS_SIZE];
    report.ok(
        "SYS_NET_STATUS",
        call(
            domain_net::SYS_NET_STATUS,
            &[record.as_mut_ptr() as u64, record.len() as u64],
        ),
    );

    // The state announced is checked by its **internal coherence**, not
    // against an expected value: the configuration may change between two
    // instructions -- DHCP runs in another task -- and comparing with a
    // snapshot taken just afterwards would be a race. The property "a
    // stack declared operational carries an address" is true at every
    // instant, though.
    report.check(
        "une pile annoncee operationnelle porte une adresse",
        record[4] == 0 || record[0x18..0x1C] != [0, 0, 0, 0],
    );
    report.check(
        "le nombre de cartes annoncees est celui du bus PCI",
        u32::from_le_bytes(record[0..4].try_into().unwrap_or([0; 4])) as usize
            == crate::pci::enumerate().iter().filter(|d| d.class == 0x02).count(),
    );

    // The invalid arguments are refused before any network access: a null
    // port makes no sense, and saying so demands no frame.
    report.refused(
        "SYS_NET_CONNECT sur un port nul",
        call(domain_net::SYS_NET_CONNECT, &[0, 0, 0, 0]),
        ErrorCode::InvalidArgument,
    );
    report.refused(
        "SYS_NET_CLOSE sur un identifiant fabrique",
        call(domain_net::SYS_NET_CLOSE, &[9999]),
        ErrorCode::InvalidArgument,
    );

    // The rule of 6.6: a binary that has not declared the network is
    // refused the domain. Checked on a real task, failing which we would
    // only test the kernel path, which is exempt.
    let denied = crate::scheduler::task_list().iter().any(|task| {
        task.app_flags & crate::binfmt::header::flags::NETWORK == 0
            && task.binary_type != crate::process::BinaryType::Kernel
    });
    report.check(
        "au moins une tache chargee n'a pas declare le reseau",
        denied,
    );
}

// ============================================================
//  5.2 -- Filesystem
// ============================================================

/// Recursively erases a subtree left by an earlier pass.
///
/// Goes through `fs::` and not through the system calls: it is a
/// prerequisite of the test, not a thing tested, and it must succeed even
/// if domain 0x01 is broken. The depth is bounded -- the self-test nests
/// only one level -- but the bound is explicit: a loop of virtual links
/// is possible since 4.1, and an unguarded recursion on a 64 KiB kernel
/// stack ends in a triple fault.
fn purge(path: &str) {
    fn walk(path: &str, depth: usize) {
        if depth == 0 {
            return;
        }
        let who = Credentials::kernel();
        let Ok(node) = crate::fs::stat(path, who) else {
            return;
        };
        if node.kind == crate::fs::format::NodeKind::Directory
            && let Ok(entries) = crate::fs::list(path, who)
        {
            for entry in entries {
                walk(
                    &alloc::format!("{}/{}", path.trim_end_matches('/'), entry.name),
                    depth - 1,
                );
            }
        }
        let _ = crate::fs::delete(path, who, crate::idt::ticks());
    }

    walk(path, 8);
}

/// The node of a path, by `SYS_FS_STAT`.
fn node_of(path: &str) -> u64 {
    let mut stat = [0u8; domain_fs::STAT_SIZE];
    match call_path(domain_fs::SYS_FS_STAT, path, &[stat.as_mut_ptr() as u64, stat.len() as u64]) {
        Ok(_) => u64::from_le_bytes(stat[0..8].try_into().unwrap_or([0; 8])),
        Err(_) => 0,
    }
}

/// 14.4.1 -- The calls by node identifier (spec 6.2), on the root when it
/// is LIFS; on a NovaFS root they must say they are not supported.
fn node_calls(report: &mut Report) {
    crate::serial_println!("[api] --- 14.4.1 appels par identifiant de noeud ---");
    if !crate::fs::is_ready() {
        return;
    }
    let letter = crate::fs::BOOT_LETTER as u64;
    let lifs = crate::fs::disk_mount(crate::fs::BOOT_LETTER).map(|d| d.format == "LIFS").unwrap_or(false);
    if !lifs {
        let mut stat = [0u8; domain_fs::STAT_SIZE];
        report.refused(
            "SYS_FS_NODE_STAT sur une racine NovaFS",
            call(domain_fs::SYS_FS_NODE_STAT, &[letter, 1, stat.as_mut_ptr() as u64, stat.len() as u64]),
            ErrorCode::Unsupported,
        );
        return;
    }
    let root = "/cache/api-noeuds";
    purge(root);
    let _ = call_path(domain_fs::SYS_FS_DIR_CREATE, root, &[0]);
    let _ = call_path(domain_fs::SYS_FS_DIR_CREATE, "/cache/api-noeuds/sous", &[0]);
    let path = "/cache/api-noeuds/f";
    let content = b"par identifiant";
    let _ = call_path(domain_fs::SYS_FS_WRITE, path, &[content.as_ptr() as u64, content.len() as u64, 0]);
    let (dir, sub, file) = (node_of(root), node_of("/cache/api-noeuds/sous"), node_of(path));
    report.check("noeuds du dossier et du fichier", dir != 0 && sub != 0 && file != 0);

    let mut stat = [0u8; domain_fs::STAT_SIZE];
    report.ok("SYS_FS_NODE_STAT", call(domain_fs::SYS_FS_NODE_STAT, &[letter, file, stat.as_mut_ptr() as u64, stat.len() as u64]));
    report.check("NODE_STAT : la taille du fichier", u64::from_le_bytes(stat[8..16].try_into().unwrap_or([0; 8])) == content.len() as u64);

    let name = "clone";
    let clone = report.ok(
        "SYS_FS_NODE_CLONE",
        call(domain_fs::SYS_FS_NODE_CLONE, &[letter, file, dir, name.as_ptr() as u64, name.len() as u64]),
    );
    report.check("le clone est un autre noeud", clone.is_some_and(|c| c != file) && node_of("/cache/api-noeuds/clone") == clone.unwrap_or(0));

    let tag = "etiquette";
    report.ok("SYS_FS_NODE_TAG", call(domain_fs::SYS_FS_NODE_TAG, &[letter, file, sub, tag.as_ptr() as u64, tag.len() as u64]));
    report.check("le Tag designe le meme noeud", node_of("/cache/api-noeuds/sous/etiquette") == file);
    let mut tags = alloc::vec![0u8; 4 * domain_fs::TAG_ENTRY_SIZE];
    let count = report.ok("SYS_FS_NODE_TAGS", call(domain_fs::SYS_FS_NODE_TAGS, &[letter, file, tags.as_mut_ptr() as u64, tags.len() as u64]));
    report.check("deux dossiers designent le fichier", count == Some(2));

    report.ok("SYS_FS_NODE_MOVE", call(domain_fs::SYS_FS_NODE_MOVE, &[letter, dir, name.as_ptr() as u64, name.len() as u64, sub]));
    report.check("le clone a change de dossier", node_of("/cache/api-noeuds/sous/clone") == clone.unwrap_or(0) && node_of("/cache/api-noeuds/clone") == 0);

    report.ok("SYS_FS_NODE_UNLINK", call(domain_fs::SYS_FS_NODE_UNLINK, &[letter, sub, tag.as_ptr() as u64, tag.len() as u64]));
    let count = call(domain_fs::SYS_FS_NODE_TAGS, &[letter, file, tags.as_mut_ptr() as u64, tags.len() as u64]).ok();
    report.check("un seul Tag reste", count == Some(1) && node_of(path) == file);

    report.refused(
        "lettre de disque invalide",
        call(domain_fs::SYS_FS_NODE_STAT, &[0x7A, file, stat.as_mut_ptr() as u64, stat.len() as u64]),
        ErrorCode::InvalidArgument,
    );
    report.refused(
        "noeud inexistant",
        call(domain_fs::SYS_FS_NODE_STAT, &[letter, u64::MAX / 2, stat.as_mut_ptr() as u64, stat.len() as u64]),
        ErrorCode::NotFound,
    );
    purge(root);
}

fn filesystem_domain(report: &mut Report) {
    crate::serial_println!("[api] --- 5.2 systeme de fichiers ---");

    if !crate::fs::is_ready() {
        crate::serial_println!("[api]   aucun volume monte - domaine 0x01 non exerce");
        return;
    }

    let root = "/cache/api";
    // The volume is persistent: a pass interrupted half way leaves its
    // tree behind it, and the next pass would fail on a directory it
    // believes empty or a file it believes absent. Purging before
    // starting makes the self-test idempotent whatever the state of the
    // disk at boot.
    purge(root);
    let _ = call_path(domain_fs::SYS_FS_DIR_CREATE, root, &[0]);

    let path = "/cache/api/note.txt";
    let content = b"ecrit par l'API systeme";

    // A missing file must report itself as such, with its path in the
    // context: that is the whole point of the structured error.
    let mut buffer = [0u8; 128];
    report.refused(
        "lecture d'un fichier absent",
        call(
            domain_fs::SYS_FS_READ,
            &[
                path.as_ptr() as u64,
                path.len() as u64,
                buffer.as_mut_ptr() as u64,
                buffer.len() as u64,
                0,
            ],
        ),
        ErrorCode::NotFound,
    );

    let written = report.ok(
        "SYS_FS_WRITE",
        call(
            domain_fs::SYS_FS_WRITE,
            &[
                path.as_ptr() as u64,
                path.len() as u64,
                content.as_ptr() as u64,
                content.len() as u64,
                0,
            ],
        ),
    );
    report.check("l'ecriture annonce sa taille", written == Some(content.len() as u64));

    // WRITE does not replace: it is OVERWRITE that does. The distinction
    // is the one of 5 between two different permissions.
    report.refused(
        "SYS_FS_WRITE sur un fichier existant",
        call(
            domain_fs::SYS_FS_WRITE,
            &[
                path.as_ptr() as u64,
                path.len() as u64,
                content.as_ptr() as u64,
                content.len() as u64,
                0,
            ],
        ),
        ErrorCode::AlreadyExists,
    );

    let read = report.ok(
        "SYS_FS_READ",
        call(
            domain_fs::SYS_FS_READ,
            &[
                path.as_ptr() as u64,
                path.len() as u64,
                buffer.as_mut_ptr() as u64,
                buffer.len() as u64,
                0,
            ],
        ),
    );
    report.check(
        "le contenu relu est identique",
        read == Some(content.len() as u64) && &buffer[..content.len()] == content,
    );

    // The offset allows re-reading a file in several pieces.
    let tail = report.ok(
        "SYS_FS_READ avec decalage",
        call(
            domain_fs::SYS_FS_READ,
            &[
                path.as_ptr() as u64,
                path.len() as u64,
                buffer.as_mut_ptr() as u64,
                buffer.len() as u64,
                8,
            ],
        ),
    );
    report.check(
        "le decalage saute le debut du fichier",
        tail == Some((content.len() - 8) as u64) && buffer[0] == content[8],
    );

    let mut stat = [0u8; domain_fs::STAT_SIZE];
    report.ok(
        "SYS_FS_STAT",
        call(
            domain_fs::SYS_FS_STAT,
            &[
                path.as_ptr() as u64,
                path.len() as u64,
                stat.as_mut_ptr() as u64,
                stat.len() as u64,
            ],
        ),
    );
    let size = u64::from_le_bytes(stat[8..16].try_into().unwrap_or([0; 8]));
    report.check(
        "la taille et la nature sont exactes",
        size == content.len() as u64 && stat[0x4A] == 1,
    );

    // A real timestamp, when the clock exists: it is what phase 4 could
    // not produce.
    if crate::rtc::is_available() {
        let modified = u64::from_le_bytes(stat[0x18..0x20].try_into().unwrap_or([0; 8]));
        report.check(
            "le fichier porte une date murale, pas un compteur de tics",
            modified > 1_704_067_200,
        );
    }

    // Virtual link: two paths, a single node (4.1).
    let alias = "/cache/api/alias.txt";
    let node = report.ok(
        "SYS_FS_LINK",
        call(
            domain_fs::SYS_FS_LINK,
            &[
                path.as_ptr() as u64,
                path.len() as u64,
                alias.as_ptr() as u64,
                alias.len() as u64,
            ],
        ),
    );
    let mut alias_stat = [0u8; domain_fs::STAT_SIZE];
    report.ok(
        "SYS_FS_STAT du lien",
        call(
            domain_fs::SYS_FS_STAT,
            &[
                alias.as_ptr() as u64,
                alias.len() as u64,
                alias_stat.as_mut_ptr() as u64,
                alias_stat.len() as u64,
            ],
        ),
    );
    let alias_node = u64::from_le_bytes(alias_stat[0..8].try_into().unwrap_or([0; 8]));
    report.check(
        "le lien designe le meme noeud, sans copie",
        node == Some(alias_node),
    );

    // Listing.
    let mut entries = alloc::vec![0u8; 16 * domain_fs::DIR_ENTRY_SIZE];
    let listed = report.ok(
        "SYS_FS_DIR_ACCESS",
        call(
            domain_fs::SYS_FS_DIR_ACCESS,
            &[
                root.as_ptr() as u64,
                root.len() as u64,
                entries.as_mut_ptr() as u64,
                entries.len() as u64,
            ],
        ),
    );
    report.check("le dossier contient les deux entrees", listed == Some(2));
    report.check(
        "l'entree est decrite comme un fichier",
        entries[8] == 1 && entries[9] > 0,
    );

    // CHMOD then read-back of the mask.
    report.ok(
        "SYS_FS_CHMOD",
        call(
            domain_fs::SYS_FS_CHMOD,
            &[
                path.as_ptr() as u64,
                path.len() as u64,
                Permissions::READ,
                0,
                0,
            ],
        ),
    );
    report.ok(
        "SYS_FS_STAT apres CHMOD",
        call(
            domain_fs::SYS_FS_STAT,
            &[
                path.as_ptr() as u64,
                path.len() as u64,
                stat.as_mut_ptr() as u64,
                stat.len() as u64,
            ],
        ),
    );
    let owner_mask = u64::from_le_bytes(stat[0x28..0x30].try_into().unwrap_or([0; 8]));
    report.check(
        "le masque relu est celui qu'on a pose",
        owner_mask == Permissions::READ,
    );
    report.refused(
        "masque comportant un bit inconnu",
        call(
            domain_fs::SYS_FS_CHMOD,
            &[path.as_ptr() as u64, path.len() as u64, 1 << 40, 0, 0],
        ),
        ErrorCode::InvalidArgument,
    );

    // OVERWRITE: this one replaces, unlike WRITE.
    let replaced = b"contenu remplace";
    report.ok(
        "SYS_FS_OVERWRITE",
        call(
            domain_fs::SYS_FS_OVERWRITE,
            &[
                path.as_ptr() as u64,
                path.len() as u64,
                replaced.as_ptr() as u64,
                replaced.len() as u64,
                0,
            ],
        ),
    );
    let after = report.ok(
        "relecture apres OVERWRITE",
        call(
            domain_fs::SYS_FS_READ,
            &[
                path.as_ptr() as u64,
                path.len() as u64,
                buffer.as_mut_ptr() as u64,
                buffer.len() as u64,
                0,
            ],
        ),
    );
    report.check(
        "le contenu a bien ete remplace",
        after == Some(replaced.len() as u64) && &buffer[..replaced.len()] == replaced,
    );

    // COPY: it really copies, both paths diverge afterwards.
    let duplicate = "/cache/api/copie.txt";
    report.ok(
        "SYS_FS_COPY",
        call(
            domain_fs::SYS_FS_COPY,
            &[
                path.as_ptr() as u64,
                path.len() as u64,
                duplicate.as_ptr() as u64,
                duplicate.len() as u64,
            ],
        ),
    );
    let mut copy_stat = [0u8; domain_fs::STAT_SIZE];
    report.ok(
        "SYS_FS_STAT de la copie",
        call(
            domain_fs::SYS_FS_STAT,
            &[
                duplicate.as_ptr() as u64,
                duplicate.len() as u64,
                copy_stat.as_mut_ptr() as u64,
                copy_stat.len() as u64,
            ],
        ),
    );
    let copy_node = u64::from_le_bytes(copy_stat[0..8].try_into().unwrap_or([0; 8]));
    let source_node = u64::from_le_bytes(stat[0..8].try_into().unwrap_or([0; 8]));
    report.check(
        "une copie porte un noeud distinct, contrairement a un lien",
        copy_node != source_node && copy_node != 0,
    );

    // MOVE inside the volume: link rewriting, no byte copied -- that is
    // what the zero returned means (4.1.3).
    let moved = "/cache/api/deplace.txt";
    let recopied = report.ok(
        "SYS_FS_MOVE",
        call(
            domain_fs::SYS_FS_MOVE,
            &[
                duplicate.as_ptr() as u64,
                duplicate.len() as u64,
                moved.as_ptr() as u64,
                moved.len() as u64,
            ],
        ),
    );
    report.check(
        "un deplacement intra-volume ne recopie aucun octet",
        recopied == Some(0),
    );
    report.refused(
        "l'ancien chemin ne repond plus",
        call(
            domain_fs::SYS_FS_STAT,
            &[
                duplicate.as_ptr() as u64,
                duplicate.len() as u64,
                copy_stat.as_mut_ptr() as u64,
                copy_stat.len() as u64,
            ],
        ),
        ErrorCode::NotFound,
    );

    // RENAME: the node does not move, only its name changes.
    let renamed = "renomme.txt";
    report.ok(
        "SYS_FS_RENAME",
        call(
            domain_fs::SYS_FS_RENAME,
            &[
                moved.as_ptr() as u64,
                moved.len() as u64,
                renamed.as_ptr() as u64,
                renamed.len() as u64,
            ],
        ),
    );
    let final_path = "/cache/api/renomme.txt";
    report.ok(
        "SYS_FS_STAT apres RENAME",
        call(
            domain_fs::SYS_FS_STAT,
            &[
                final_path.as_ptr() as u64,
                final_path.len() as u64,
                copy_stat.as_mut_ptr() as u64,
                copy_stat.len() as u64,
            ],
        ),
    );
    report.check(
        "le renommage conserve le noeud",
        u64::from_le_bytes(copy_stat[0..8].try_into().unwrap_or([0; 8])) == copy_node,
    );

    // CHOWN then read-back of the owner.
    report.ok(
        "SYS_FS_CHOWN",
        call(
            domain_fs::SYS_FS_CHOWN,
            &[final_path.as_ptr() as u64, final_path.len() as u64, 1001, 100],
        ),
    );
    report.ok(
        "SYS_FS_STAT apres CHOWN",
        call(
            domain_fs::SYS_FS_STAT,
            &[
                final_path.as_ptr() as u64,
                final_path.len() as u64,
                copy_stat.as_mut_ptr() as u64,
                copy_stat.len() as u64,
            ],
        ),
    );
    let owner = u32::from_le_bytes(copy_stat[0x40..0x44].try_into().unwrap_or([0; 4]));
    let group = u32::from_le_bytes(copy_stat[0x44..0x48].try_into().unwrap_or([0; 4]));
    report.check("le proprietaire relu est celui qu'on a pose", owner == 1001 && group == 100);

    report.ok(
        "SYS_FS_DELETE du renomme",
        call_path(domain_fs::SYS_FS_DELETE, final_path, &[]),
    );

    // Volumes.
    let mut disks = alloc::vec![0u8; 4 * domain_fs::DISK_ENTRY_SIZE];
    let volumes = report.ok(
        "SYS_FS_DISK_LIST",
        call(
            domain_fs::SYS_FS_DISK_LIST,
            &[disks.as_mut_ptr() as u64, disks.len() as u64],
        ),
    );
    report.check(
        "au moins un volume est monte",
        volumes.map(|value| value >= 1).unwrap_or(false),
    );
    report.ok(
        "SYS_FS_DISK_MOUNT",
        call(domain_fs::SYS_FS_DISK_MOUNT, &[b'/' as u64]),
    );
    report.refused(
        "montage d'une lettre inconnue",
        call(domain_fs::SYS_FS_DISK_MOUNT, &[b'Z' as u64]),
        ErrorCode::NotFound,
    );

    // Audit log -- reserved to ADMIN and ROOT.
    let mut journal = alloc::vec![0u8; 4 * domain_fs::AUDIT_ENTRY_SIZE];
    report.ok(
        "SYS_FS_AUDIT",
        call(
            domain_fs::SYS_FS_AUDIT,
            &[journal.as_mut_ptr() as u64, journal.len() as u64],
        ),
    );

    // Tidying: both links then the directory.
    report.ok(
        "SYS_FS_DELETE du lien",
        call_path(domain_fs::SYS_FS_DELETE, alias, &[]),
    );
    report.ok(
        "SYS_FS_DELETE du fichier",
        call_path(domain_fs::SYS_FS_DELETE, path, &[]),
    );
    report.refused(
        "suppression d'un fichier deja supprime",
        call_path(domain_fs::SYS_FS_DELETE, path, &[]),
        ErrorCode::NotFound,
    );
    report.ok(
        "SYS_FS_DELETE du dossier",
        call_path(domain_fs::SYS_FS_DELETE, root, &[]),
    );
}

// ============================================================
//  5.3 -- Processes
// ============================================================

fn process_domain(report: &mut Report) {
    crate::serial_println!("[api] --- 5.3 processus ---");

    let me = report.ok("SYS_PROC_SELF", call0(domain_proc::SYS_PROC_SELF));
    report.check(
        "SELF rend le PID de l'appelant",
        me == Some(crate::scheduler::current_pid()),
    );

    let mut record = [0u8; domain_proc::PROC_ENTRY_SIZE];
    report.ok(
        "SYS_PROC_STAT",
        call(
            domain_proc::SYS_PROC_STAT,
            &[0, record.as_mut_ptr() as u64, record.len() as u64],
        ),
    );
    let pid = u64::from_le_bytes(record[0..8].try_into().unwrap_or([0; 8]));
    let name_len = record[0x44] as usize;
    report.check(
        "l'enregistrement decrit bien l'appelant",
        Some(pid) == me && name_len > 0,
    );
    crate::serial_println!(
        "[api]   appelant : PID {} \"{}\" - priorite {}, coeur {}",
        pid,
        core::str::from_utf8(&record[0x48..0x48 + name_len]).unwrap_or("?"),
        record[0x3B],
        record[0x3C]
    );

    let mut tasks = alloc::vec![0u8; 32 * domain_proc::PROC_ENTRY_SIZE];
    let count = report.ok(
        "SYS_PROC_LIST",
        call(
            domain_proc::SYS_PROC_LIST,
            &[tasks.as_mut_ptr() as u64, tasks.len() as u64],
        ),
    );
    report.check(
        "la liste couvre toutes les taches vivantes",
        count == Some(crate::scheduler::task_list().len() as u64),
    );

    report.refused(
        "STAT d'un PID inconnu",
        call(
            domain_proc::SYS_PROC_STAT,
            &[99_999, record.as_mut_ptr() as u64, record.len() as u64],
        ),
        ErrorCode::NotFound,
    );

    priority_policy(report);

    report.refused(
        "une tache ne se suspend pas elle-meme",
        call(domain_proc::SYS_PROC_SUSPEND, &[me.unwrap_or(0)]),
        ErrorCode::InvalidArgument,
    );
    report.refused(
        "une tache ne s'attend pas elle-meme",
        call(domain_proc::SYS_PROC_WAIT, &[me.unwrap_or(0)]),
        ErrorCode::InvalidArgument,
    );
    report.refused(
        "attente d'un PID inconnu",
        call(domain_proc::SYS_PROC_WAIT, &[99_999]),
        ErrorCode::NotFound,
    );
    report.refused(
        "suspension d'un PID inconnu",
        call(domain_proc::SYS_PROC_SUSPEND, &[99_999]),
        ErrorCode::NotFound,
    );

    report.ok("SYS_PROC_YIELD", call0(domain_proc::SYS_PROC_YIELD));

    // 5.3.1: SPAWN loads from the filesystem. The refusal of a missing
    // path proves the check precedes any allocation.
    report.refused(
        "SPAWN d'un binaire absent",
        call_path(domain_proc::SYS_PROC_SPAWN, "/apps/absent.cus", &[]),
        ErrorCode::NotFound,
    );

    suspend_cycle(report);
    child_lifecycle(report);
}

// ============================================================
//  5.3.2 -- SETPRIO confronted with the 7.3 table
// ============================================================

/// Checks the per-type rule, line by line, on the tasks really alive.
///
/// 7.3 gives for each format a background slice and a foreground one, and
/// explicitly notes the two formats whose slice does not move -- "Never
/// reduced" for `.dsa`, "Never modifiable" for `.ras`. This test
/// confronts the implementation with that table rather than with a chosen
/// value: if the spec changes, this is where it will show.
fn priority_policy(report: &mut Report) {
    use crate::process::BinaryType::*;

    // A priority class is not asked for: 7.1 declares it fixed by the
    // format.
    report.refused(
        "SETPRIO vers une classe arbitraire",
        call(domain_proc::SYS_PROC_SETPRIO, &[0, 5]),
        ErrorCode::InvalidArgument,
    );

    let mut variable = 0usize;
    let mut fixed = 0usize;

    for task in crate::scheduler::task_list() {
        let expected_variable = matches!(task.binary_type, Ss | Gs | Cus);
        let label = task.binary_type.as_str();

        if expected_variable {
            variable += 1;
            let background = call(
                domain_proc::SYS_PROC_SETPRIO,
                &[task.pid, domain_proc::PRIO_BACKGROUND],
            );
            report.check(
                "la tranche d'arriere-plan est celle du 7.3",
                background == Ok(u64::from(task.binary_type.time_slice(false))),
            );

            let foreground = call(
                domain_proc::SYS_PROC_SETPRIO,
                &[task.pid, domain_proc::PRIO_FOREGROUND],
            );
            report.check(
                "la tranche de premier plan est celle du 7.3",
                foreground == Ok(u64::from(task.binary_type.time_slice(true))),
            );

            // The original state is restored: the self-test must not
            // leave the system in a configuration it chose.
            let _ = call(
                domain_proc::SYS_PROC_SETPRIO,
                &[
                    task.pid,
                    if task.foreground {
                        domain_proc::PRIO_FOREGROUND
                    } else {
                        domain_proc::PRIO_BACKGROUND
                    },
                ],
            );
        } else {
            fixed += 1;
            report.refused(
                label,
                call(
                    domain_proc::SYS_PROC_SETPRIO,
                    &[task.pid, domain_proc::PRIO_BACKGROUND],
                ),
                ErrorCode::PermissionDenied,
            );
        }
    }

    report.check(
        "les deux categories du 7.3 sont representees",
        variable > 0 && fixed > 0,
    );
    crate::serial_println!(
        "[api]   tranche modifiable sur {} taches, figee sur {} (spec 7.3)",
        variable,
        fixed
    );
}

// ============================================================
//  5.3.1 -- Suspend / resume cycle
// ============================================================

/// Order given to the target task: carry on or stop.
static VICTIM_RUNNING: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(1);

/// Target of the suspend/resume cycle.
///
/// A dedicated task, and not one of the demonstration tasks: those block
/// on IPC receptions, and a sleeping task is not suspendable -- which
/// would make the test depend on the moment it runs rather than on what
/// it checks.
fn victim_task() -> ! {
    while VICTIM_RUNNING.load(core::sync::atomic::Ordering::Relaxed) != 0 {
        crate::scheduler::yield_now();
    }
    crate::scheduler::exit_current_with(0);
}

fn suspend_cycle(report: &mut Report) {
    VICTIM_RUNNING.store(1, core::sync::atomic::Ordering::Relaxed);
    let victim = crate::scheduler::spawn(
        "api-cible",
        crate::process::BinaryType::Kernel,
        victim_task,
    );

    report.ok(
        "SYS_PROC_SUSPEND",
        call(domain_proc::SYS_PROC_SUSPEND, &[victim]),
    );
    let suspended = crate::scheduler::task_info(victim)
        .map(|info| info.state == crate::process::State::Suspended)
        .unwrap_or(false);
    report.check("la tache est effectivement suspendue", suspended);

    // A suspended task is not suspended twice: its state is no longer
    // eligible.
    report.refused(
        "seconde suspension",
        call(domain_proc::SYS_PROC_SUSPEND, &[victim]),
        ErrorCode::NotFound,
    );

    report.ok(
        "SYS_PROC_RESUME",
        call(domain_proc::SYS_PROC_RESUME, &[victim]),
    );
    // READY **or** RUNNING: on several cores, the task may have been
    // elected between the resume and this read. Demanding READY alone
    // would make the test depend on the relative speed of the cores,
    // which has nothing to do with what we want to check.
    let resumed = crate::scheduler::task_info(victim)
        .map(|info| {
            matches!(
                info.state,
                crate::process::State::Ready | crate::process::State::Running
            )
        })
        .unwrap_or(false);
    report.check("la tache reprend son etat eligible", resumed);

    // It stops by itself, and we wait for it: it is also the path that
    // checks a task still elected on another core is not reaped from
    // under its feet.
    VICTIM_RUNNING.store(0, core::sync::atomic::Ordering::Relaxed);
    let code = report.ok("SYS_PROC_WAIT sur la cible", call(domain_proc::SYS_PROC_WAIT, &[victim]));
    report.check("la cible se termine proprement", code == Some(0));
    crate::serial_println!("[api]   cycle suspension/reprise verifie sur PID {}", victim);
}

// ============================================================
//  5.3.3 -- Complete life cycle of a child task
// ============================================================

/// PID of the waiting task, published before creating the child.
static PARENT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Agreed exit code: a value recognisable in the trace.
const CHILD_EXIT_CODE: i64 = 42;

/// Body of the child task: it greets its parent then terminates.
fn child_task() -> ! {
    let parent = PARENT.load(core::sync::atomic::Ordering::Relaxed);
    let _ = crate::ipc::send_text(
        parent,
        crate::ipc::MessageKind::Data,
        0xC1,
        "bonjour du fils",
    );
    crate::scheduler::exit_current_with(CHILD_EXIT_CODE);
}

/// Checks what no synthetic test can check: that a task is born,
/// communicates, dies, and gives back everything it held.
fn child_lifecycle(report: &mut Report) {
    crate::serial_println!("[api] --- 5.3.3 cycle de vie et recuperation ---");

    let me = crate::scheduler::current_pid();
    PARENT.store(me, core::sync::atomic::Ordering::Relaxed);

    let (_, _, free_before) = crate::frame::stats();

    // Sleeping is the third blocking call: it must hand back after at
    // least the duration asked for, never before.
    let before = crate::idt::ticks();
    report.ok(
        "SYS_TIME_SLEEP_MS",
        call(domain_time::SYS_TIME_SLEEP_MS, &[50]),
    );
    let slept = crate::idt::ticks() - before;
    report.check("le sommeil dure au moins la duree demandee", slept >= 5);
    crate::serial_println!("[api]   sommeil de 50 ms - {} tics ecoules", slept);

    // The number of live tasks is read at the same time as the frames:
    // the measurement that follows only means something if nothing else
    // is born or dies meanwhile. See the comment on the check, further
    // down.
    let (tasks_before, _, _) = crate::scheduler::stats();

    let child = crate::scheduler::spawn(
        "api-fils",
        crate::process::BinaryType::Kernel,
        child_task,
    );
    report.check("la tache fille a recu un PID", child != 0);

    // Blocking reception: the child may not have run yet. That is
    // precisely what the asynchronous mode allows.
    let mut buffer = [0u8; domain_ipc::MESSAGE_HEADER + 64];
    let received = report.ok(
        "SYS_IPC_RECV bloquant",
        call(
            domain_ipc::SYS_IPC_RECV,
            &[buffer.as_mut_ptr() as u64, buffer.len() as u64, 1],
        ),
    );
    let sender = u64::from_le_bytes(buffer[0..8].try_into().unwrap_or([0; 8]));
    report.check(
        "le message vient bien de la tache fille",
        received.is_some() && sender == child,
    );

    // 5.3.3: the wait returns the exit code, and the reaping returns the
    // stack. It is the limit "the DEAD tasks free nothing" that closes
    // here.
    let code = report.ok("SYS_PROC_WAIT", call(domain_proc::SYS_PROC_WAIT, &[child]));
    report.check(
        "le code de sortie remonte tel quel",
        code == Some(CHILD_EXIT_CODE as u64),
    );

    let after = crate::scheduler::task_info(child);
    report.check(
        "la tache attendue est morte et reclamee",
        after
            .map(|info| info.state == crate::process::State::Dead && info.stack_bytes == 0)
            .unwrap_or(false),
    );
    report.refused(
        "seconde attente sur une tache deja reclamee",
        call(domain_proc::SYS_PROC_WAIT, &[child]),
        ErrorCode::NotFound,
    );

    let (_, _, free_after) = crate::frame::stats();
    let (tasks_after, _, _) = crate::scheduler::stats();

    // `frame::stats` counts the frames of **the whole machine**. Comparing
    // two readings only says something about the cycle on condition that
    // nobody else allocated between the two -- and since phase 7, the
    // compositor and its seven modules are born precisely during that
    // window. The measurement was becoming a false failure: it observed
    // the appearance of the graphics system, not a leak.
    //
    // The condition is therefore made explicit and checked. When it
    // holds, the check is as strong as before; when it does not, saying
    // so is worth more than returning a verdict on a measurement that
    // does not measure what it claims.
    if tasks_after == tasks_before {
        report.check(
            "le cycle de vie complet ne fuit aucun cadre",
            free_after >= free_before,
        );
    } else {
        crate::serial_println!(
            "[api]   {} tache(s) nees pendant le cycle - mesure de cadres non concluante",
            tasks_after.saturating_sub(tasks_before)
        );
    }
    crate::serial_println!(
        "[api]   cadres libres : {} avant la fille, {} apres recuperation",
        free_before,
        free_after
    );

    let _ = String::new();
    let _: Vec<u8> = Vec::new();
}

// ============================================================
//  5.3.1 -- SPAWN: the complete chain, from file to ring 3
// ============================================================

/// Directory of the self-test binaries.
const BIN_DIR: &str = "/cache/api/bin";

/// Installs a binary in the filesystem, with the rights the two-level
/// rule demands (5.1: `execute` on the file **and** on the directory
/// containing it).
fn install(report: &mut Report, path: &str, image: &[u8]) -> bool {
    let mask = Permissions::READ | Permissions::EXECUTE | Permissions::DELETE;
    let written = call(
        domain_fs::SYS_FS_WRITE,
        &[
            path.as_ptr() as u64,
            path.len() as u64,
            image.as_ptr() as u64,
            image.len() as u64,
            mask,
        ],
    );
    report.ok("installation du binaire", written) == Some(image.len() as u64)
}

/// Checks what no synthetic test can check: that a binary laid on the
/// disk becomes a process running in ring 3, then terminates with the
/// code it chose.
///
/// It is the longest path of the system: reading the file, checking the
/// right to execute, parsing the header, verifying the signature,
/// applying the rules of 3.4, mapping the sections, relocation, creating
/// the task, switching to ring 3, system calls, termination, then
/// reaping. Testing it in pieces does not prove it holds end to end.
fn spawn_cycle(report: &mut Report) {
    crate::serial_println!("[api] --- 5.3.1 SPAWN : du fichier au ring 3 ---");

    if !crate::fs::is_ready() {
        crate::serial_println!("[api]   aucun volume monte - SPAWN non exerce");
        return;
    }

    // Same precaution as in 5.2: the install directory must owe nothing
    // to an earlier pass.
    purge(BIN_DIR);

    let dir_mask = Permissions::ACCESS
        | Permissions::WRITE
        | Permissions::EXECUTE
        | Permissions::DELETE;
    report.ok(
        "creation du dossier d'installation",
        call_path(domain_fs::SYS_FS_DIR_CREATE, BIN_DIR, &[dir_mask]),
    );

    // The cycle is played **twice**, and it is the second pass that
    // carries the invariant. The first may legitimately hold back a few
    // frames: a page table created for an arena slot stays in place if
    // another slot shares it, and the intermediate tables are only given
    // back once empty. That cost is one-off. A repeated cycle, on the
    // other hand, must cost nothing at all -- that is the difference
    // between shared infrastructure and a leak.
    let (_, _, before_first) = crate::frame::stats();
    run_spawn_cycle(report, false);
    let (_, _, after_first) = crate::frame::stats();

    run_spawn_cycle(report, true);
    let (_, _, after_second) = crate::frame::stats();

    report.check(
        "un second cycle de lancement ne coute aucun cadre",
        after_second >= after_first,
    );
    crate::serial_println!(
        "[api]   cadres libres : {} au depart, {} apres un cycle, {} apres deux",
        before_first,
        after_first,
        after_second
    );

    report.ok(
        "suppression du dossier d'installation",
        call_path(domain_fs::SYS_FS_DELETE, BIN_DIR, &[]),
    );

    // The parent directory, created by 5.2 then implicitly recreated
    // here, disappears with the last test that uses it: the self-test
    // must leave nothing on a volume that outlives the reboot.
    purge("/cache/api");
}

/// One complete pass: install, launch, wait, uninstall.
fn run_spawn_cycle(report: &mut Report, quiet: bool) {
    // --- a file that is not a binary ---
    // The refusal must come from the parsing, not from a crash: that is
    // what the magic check of phase 3.1 guarantees.
    let junk_path = "/cache/api/bin/pas-un-binaire.cus";
    let junk = b"ceci n'est pas un binaire Nova OS";
    install(report, junk_path, junk);
    report.refused(
        "SPAWN d'un fichier qui n'est pas un binaire",
        call_path(domain_proc::SYS_PROC_SPAWN, junk_path, &[]),
        ErrorCode::InvalidArgument,
    );
    report.ok(
        "suppression du fichier invalide",
        call_path(domain_fs::SYS_FS_DELETE, junk_path, &[]),
    );

    // --- the real binary ---
    let image = crate::binfmt::sample::executable_cus();
    let path = "/cache/api/bin/demo.cus";
    if !install(report, path, &image) {
        return;
    }
    if !quiet {
        crate::serial_println!(
            "[api]   binaire de {} octets installe dans {}",
            image.len(),
            BIN_DIR
        );
    }

    let pid = report.ok(
        "SYS_PROC_SPAWN",
        call_path(domain_proc::SYS_PROC_SPAWN, path, &[]),
    );
    let Some(pid) = pid else {
        return;
    };

    // 3.4.1 and 7.1: the priority and the core follow from the format
    // declared in the header, never from a parameter of the caller.
    let info = crate::scheduler::task_info(pid);
    report.check(
        "la tache lancee porte le format declare par l'en-tete",
        info.as_ref()
            .map(|i| i.binary_type == crate::process::BinaryType::Cus && i.priority == 4)
            .unwrap_or(false),
    );
    report.check(
        "la tache lancee herite du compte de son lanceur",
        info.as_ref()
            .map(|i| i.uid == crate::fs::perm::ROOT_UID && i.parent == crate::scheduler::current_pid())
            .unwrap_or(false),
    );

    // The demonstration program ends with SYS_PROC_EXIT(7). Finding that
    // 7 here means observing that the loop is closed.
    let code = report.ok(
        "SYS_PROC_WAIT sur le processus lance",
        call(domain_proc::SYS_PROC_WAIT, &[pid]),
    );
    report.check(
        "le code de sortie choisi par le programme ring 3 remonte",
        code == Some(7),
    );

    // --- 5.6.4: the network declaration crosses the whole chain ---
    let network = crate::binfmt::sample::executable_network();
    let net_path = "/cache/api/bin/reseau.cus";
    if install(report, net_path, &network) {
        if let Some(net_pid) = report.ok(
            "SPAWN d'un binaire declarant le reseau",
            call_path(domain_proc::SYS_PROC_SPAWN, net_path, &[]),
        ) {
            let declared = crate::scheduler::task_info(net_pid)
                .map(|i| i.app_flags & crate::binfmt::header::flags::NETWORK != 0)
                .unwrap_or(false);
            report.check(
                "le drapeau NETWORK de l'en-tete atteint l'ordonnanceur",
                declared,
            );
            report.ok(
                "attente du client reseau",
                call(domain_proc::SYS_PROC_WAIT, &[net_pid]),
            );
        }
        report.ok(
            "suppression du binaire reseau",
            call_path(domain_fs::SYS_FS_DELETE, net_path, &[]),
        );
    }

    report.ok(
        "suppression du binaire",
        call_path(domain_fs::SYS_FS_DELETE, path, &[]),
    );
}

// ============================================================
//  5.5.4 -- QUERY / RESPOND between two tasks
// ============================================================

const API_CHANNEL: &str = "nova.api.echo";

/// Has the server opened its channel?
static SERVER_READY: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Minimal server: it exposes an API, waits for a request, answers it.
///
/// Written with the same system calls as an application: it is the
/// protocol of 6.5 exercised on both sides, not only on the client side.
fn echo_server() -> ! {
    let opened = call(
        domain_ipc::SYS_IPC_OPEN,
        &[API_CHANNEL.as_ptr() as u64, API_CHANNEL.len() as u64],
    );
    SERVER_READY.store(1, core::sync::atomic::Ordering::Release);

    let mut buffer = [0u8; domain_ipc::MESSAGE_HEADER + 64];
    let received = call(
        domain_ipc::SYS_IPC_RECV,
        &[buffer.as_mut_ptr() as u64, buffer.len() as u64, 1],
    );

    let verdict = match received {
        Ok(length) if length as usize > domain_ipc::MESSAGE_HEADER => {
            let client = u64::from_le_bytes(buffer[0..8].try_into().unwrap_or([0; 8]));
            let tag = u32::from_le_bytes(buffer[8..12].try_into().unwrap_or([0; 4]));
            // Nature 1 = request: it is what QUERY must have laid down.
            if buffer[0x0C] != 1 {
                2
            } else {
                // The answer takes back the label of the request: it is
                // what lets the client attach the answer.
                let payload = b"echo";
                let sent = call(
                    domain_ipc::SYS_IPC_RESPOND,
                    &[
                        client,
                        payload.as_ptr() as u64,
                        payload.len() as u64,
                        tag as u64,
                    ],
                );
                if sent.is_ok() { 0 } else { 3 }
            }
        }
        _ => 1,
    };

    if let Ok(id) = opened {
        let _ = call(domain_ipc::SYS_IPC_CLOSE, &[id]);
    }
    crate::scheduler::exit_current_with(verdict);
}

fn query_respond(report: &mut Report) {
    crate::serial_println!("[api] --- 5.5.4 requete et reponse entre deux taches ---");

    SERVER_READY.store(0, core::sync::atomic::Ordering::Release);
    let server = crate::scheduler::spawn(
        "api-echo",
        crate::process::BinaryType::Dsa,
        echo_server,
    );

    // The channel must exist before it is queried: the name is the only
    // rendezvous point, and it only appears at the opening.
    let mut guard = 0;
    while SERVER_READY.load(core::sync::atomic::Ordering::Acquire) == 0 && guard < 10_000 {
        crate::scheduler::yield_now();
        guard += 1;
    }
    report.check("le serveur a publie son API", guard < 10_000);

    let question = b"ping";
    let request = report.ok(
        "SYS_IPC_QUERY",
        call(
            domain_ipc::SYS_IPC_QUERY,
            &[
                API_CHANNEL.as_ptr() as u64,
                API_CHANNEL.len() as u64,
                question.as_ptr() as u64,
                question.len() as u64,
            ],
        ),
    );
    report.check(
        "la requete recoit un identifiant non nul",
        request.map(|value| value != 0).unwrap_or(false),
    );

    let mut buffer = [0u8; domain_ipc::MESSAGE_HEADER + 64];
    let length = report.ok(
        "reception de la reponse",
        call(
            domain_ipc::SYS_IPC_RECV,
            &[buffer.as_mut_ptr() as u64, buffer.len() as u64, 1],
        ),
    );

    let sender = u64::from_le_bytes(buffer[0..8].try_into().unwrap_or([0; 8]));
    let tag = u32::from_le_bytes(buffer[8..12].try_into().unwrap_or([0; 4]));
    report.check("la reponse vient du serveur", sender == server);
    report.check("la reponse est marquee comme telle", buffer[0x0C] == 2);
    report.check(
        "l'etiquette rattache la reponse a sa requete",
        Some(tag as u64) == request,
    );
    report.check(
        "la charge utile de la reponse est intacte",
        length == Some((domain_ipc::MESSAGE_HEADER + 4) as u64)
            && &buffer[domain_ipc::MESSAGE_HEADER..domain_ipc::MESSAGE_HEADER + 4] == b"echo",
    );

    let verdict = report.ok(
        "attente du serveur",
        call(domain_proc::SYS_PROC_WAIT, &[server]),
    );
    report.check("le serveur n'a rien trouve d'anormal", verdict == Some(0));

    report.refused(
        "requete vers une API inexistante",
        call(
            domain_ipc::SYS_IPC_QUERY,
            &["nova.api.absente".as_ptr() as u64, 16, question.as_ptr() as u64, 4],
        ),
        ErrorCode::NotFound,
    );
}

// ============================================================
//  5.6.1 -- Privilege drop
// ============================================================

/// Task that gives up its rights and reports through its exit code.
///
/// The verdict goes through the exit code rather than through a shared
/// variable: it is the only channel that also proves `SYS_PROC_WAIT`
/// faithfully carries what the task decided.
fn descent_task() -> ! {
    let mut record = [0u8; domain_user::ACCOUNT_SIZE];

    // To start with, the identity is that of the launcher.
    if call(
        domain_user::SYS_USER_SELF,
        &[record.as_mut_ptr() as u64, record.len() as u64],
    )
    .is_err()
    {
        crate::scheduler::exit_current_with(1);
    }
    if u32::from_le_bytes(record[0..4].try_into().unwrap_or([0; 4])) != crate::fs::perm::ROOT_UID {
        crate::scheduler::exit_current_with(2);
    }

    // Drop to a user account: allowed.
    let target = "nova";
    if call(
        domain_user::SYS_USER_SETID,
        &[target.as_ptr() as u64, target.len() as u64],
    )
    .is_err()
    {
        crate::scheduler::exit_current_with(3);
    }

    // The identity really changed, and the filesystem must see it: that
    // is the whole point of the operation.
    if call(
        domain_user::SYS_USER_SELF,
        &[record.as_mut_ptr() as u64, record.len() as u64],
    )
    .is_err()
    {
        crate::scheduler::exit_current_with(4);
    }
    if record[8] != crate::fs::perm::Role::User as u8 {
        crate::scheduler::exit_current_with(5);
    }

    // Climbing back: refused, without exception.
    let back = "root";
    if call(
        domain_user::SYS_USER_SETID,
        &[back.as_ptr() as u64, back.len() as u64],
    )
    .is_ok()
    {
        crate::scheduler::exit_current_with(6);
    }

    // And the account adopted must now meet the masks: the audit log is
    // reserved to ADMIN and ROOT (5.1).
    let mut journal = [0u8; 128];
    if call(
        domain_fs::SYS_FS_AUDIT,
        &[journal.as_mut_ptr() as u64, journal.len() as u64],
    )
    .is_ok()
    {
        crate::scheduler::exit_current_with(7);
    }

    crate::scheduler::exit_current_with(0);
}

fn identity_descent(report: &mut Report) {
    crate::serial_println!("[api] --- 5.6.1 descente de privilege ---");

    let child = crate::scheduler::spawn(
        "api-descente",
        crate::process::BinaryType::Kernel,
        descent_task,
    );
    let verdict = report.ok(
        "attente de la tache qui abandonne ses droits",
        call(domain_proc::SYS_PROC_WAIT, &[child]),
    );
    report.check(
        "descente autorisee, remontee refusee, masques appliques",
        verdict == Some(0),
    );
    if verdict != Some(0) {
        crate::serial_println!("[api]   etape en echec : {:?}", verdict);
    }

    // The caller has not moved: a task changes only its own identity.
    let mut record = [0u8; domain_user::ACCOUNT_SIZE];
    report.ok(
        "identite de l'appelant apres la descente du fils",
        call(
            domain_user::SYS_USER_SELF,
            &[record.as_mut_ptr() as u64, record.len() as u64],
        ),
    );
    report.check(
        "l'identite du pere est intacte",
        u32::from_le_bytes(record[0..4].try_into().unwrap_or([0; 4])) == crate::fs::perm::ROOT_UID,
    );
}

// ============================================================
//  5.4.2 -- Shared memory
// ============================================================

/// Pattern the recipient writes into the region entrusted to it.
const SHARE_PATTERN: u64 = 0x5041_5254_4147_45;

/// Recipient of a share: it waits for the notification, reads the address
/// and the size in it, then writes into the region to prove it reaches it.
fn share_task() -> ! {
    let mut buffer = [0u8; domain_ipc::MESSAGE_HEADER + 32];
    let received = call(
        domain_ipc::SYS_IPC_RECV,
        &[buffer.as_mut_ptr() as u64, buffer.len() as u64, 1],
    );

    if received.is_err() {
        crate::scheduler::exit_current_with(1);
    }
    // The label of the notification is the number of SYS_MEM_SHARE: it is
    // how the recipient tells it from an ordinary message.
    let tag = u32::from_le_bytes(buffer[8..12].try_into().unwrap_or([0; 4]));
    if tag != domain_mem::SYS_MEM_SHARE as u32 {
        crate::scheduler::exit_current_with(2);
    }

    let base = u64::from_le_bytes(
        buffer[domain_ipc::MESSAGE_HEADER..domain_ipc::MESSAGE_HEADER + 8]
            .try_into()
            .unwrap_or([0; 8]),
    );
    let size = u64::from_le_bytes(
        buffer[domain_ipc::MESSAGE_HEADER + 8..domain_ipc::MESSAGE_HEADER + 16]
            .try_into()
            .unwrap_or([0; 8]),
    );
    if base == 0 || size == 0 {
        crate::scheduler::exit_current_with(3);
    }

    unsafe { core::ptr::write_volatile(base as *mut u64, SHARE_PATTERN) };
    crate::scheduler::exit_current_with(0);
}

fn shared_memory(report: &mut Report) {
    crate::serial_println!("[api] --- 5.4.2 memoire partagee ---");

    let (_, _, free_before) = crate::frame::stats();

    let Some(base) = report.ok(
        "allocation de la region a partager",
        call(domain_mem::SYS_MEM_ALLOC, &[4096, 0]),
    ) else {
        return;
    };

    let peer = crate::scheduler::spawn(
        "api-partage",
        crate::process::BinaryType::Kernel,
        share_task,
    );

    let size = report.ok(
        "SYS_MEM_SHARE",
        call(domain_mem::SYS_MEM_SHARE, &[base, 4096, peer]),
    );
    report.check("le partage annonce la taille de la region", size == Some(4096));

    report.refused(
        "partage vers un PID inexistant",
        call(domain_mem::SYS_MEM_SHARE, &[base, 4096, 99_999]),
        ErrorCode::NotFound,
    );
    report.refused(
        "partage d'une adresse qui n'est pas une region",
        call(domain_mem::SYS_MEM_SHARE, &[base + 0x10_0000, 4096, peer]),
        ErrorCode::NotFound,
    );

    let verdict = report.ok(
        "attente du destinataire",
        call(domain_proc::SYS_PROC_WAIT, &[peer]),
    );
    report.check("le destinataire a recu et decode la notification", verdict == Some(0));

    let written = unsafe { core::ptr::read_volatile(base as *const u64) };
    report.check(
        "le destinataire a bien ecrit dans la region partagee",
        written == SHARE_PATTERN,
    );

    report.ok("liberation de la region", call(domain_mem::SYS_MEM_FREE, &[base]));

    let (_, _, free_after) = crate::frame::stats();
    report.check("le partage ne fuit aucun cadre", free_after >= free_before);
}

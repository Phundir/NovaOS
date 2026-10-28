//! Self-test of the ported programs (phase 12): the test programs of
//! `tools/ports/tests/`, linked dynamically and started through the Nova
//! interface (spec 6.7) -- their C library is the Nova one, and not one
//! Linux call may reach the kernel meanwhile. Each program checks one
//! area -- threads and futexes, memory, files, pipes and epoll, signals
//! -- and ends with a known exit code; its standard output goes to the
//! serial console. Without the programs the test says so and passes
//! nothing: there is nothing to run.
//!
//! The same programs built statically for Linux are the Linux
//! personality's test (`linux::selftest`).

use alloc::vec::Vec;

/// Phase 12: the programs of `/nova-tests/natif`, started through the Nova
/// interface. The first is a linked image (spec 3.5), the others bare ELF
/// files run with the Nova C library; `spawn` also execs a native program
/// and forks. (program, arguments, expected exit code)
const NATIVE: &[(&str, &[&str], i64)] = &[
    ("/nova-tests/natif/hello.cus", &[], 7),
    ("/nova-tests/natif/hello", &[], 7),
    ("/nova-tests/natif/threads", &[], 0),
    ("/nova-tests/natif/memory", &[], 0),
    ("/nova-tests/natif/files", &[], 0),
    ("/nova-tests/natif/events", &[], 0),
    ("/nova-tests/natif/signals", &[], 0),
    ("/nova-tests/natif/spawn", &[], 0),
    ("/nova-tests/natif/database", &[], 0),
    ("/nova-tests/natif/mmstress", &["m"], 0),
    ("/nova-tests/natif/mmstress", &["b"], 0),
];

/// (programs conforming, failures, absent, Linux calls made meanwhile).
pub fn run_native() -> (u32, u32, u32, u64) {
    let mut passed = 0;
    let mut failed = 0;
    let mut absent = 0;
    let linux_before = crate::linux::syscall::call_count();
    for (path, args, expected) in NATIVE {
        if super::vfs::walk("/", path, true).is_err() {
            absent += 1;
            continue;
        }
        let mut argv: Vec<&str> = alloc::vec![*path];
        argv.extend_from_slice(args);
        let envp = ["PATH=/usr/bin:/bin", "HOME=/home/nova", "LANG=C.UTF-8"];
        let start = crate::idt::ticks();
        let outcome = super::spawn_native(path, &argv, &envp).map(|pid| super::wait_exit(pid, Some(6000)));
        let elapsed = crate::idt::ticks() - start;
        match outcome {
            Ok(Some(code)) if code == *expected => {
                passed += 1;
                crate::serial_println!("[natif] {} : code {} en {} tics - conforme", path, code, elapsed);
            }
            Ok(Some(code)) => {
                failed += 1;
                crate::serial_println!("[natif] ECHEC {} : code {} (attendu {})", path, code, expected);
            }
            Ok(None) => {
                failed += 1;
                crate::serial_println!("[natif] ECHEC {} : pas termine en 60 s", path);
            }
            Err(e) => {
                failed += 1;
                crate::serial_println!("[natif] ECHEC {} : lancement refuse, errno {}", path, e);
            }
        }
    }
    (passed, failed, absent, crate::linux::syscall::call_count() - linux_before)
}

//! Self-test of the Linux personality (phase 11.5): runs the test
//! programs of the root image, `/nova-tests/*`, built statically for
//! Linux from `tools/ports/tests/` -- each makes its calls by Linux
//! number, through `linux::syscall`, onto the kernel's POSIX objects.
//!
//! Each program checks one area -- threads and futexes, memory, files,
//! pipes and epoll, signals -- and ends with a known exit code; its
//! standard output goes to the serial console. Without a root image the
//! test says so and passes nothing: there is nothing to run.

use alloc::vec::Vec;

use crate::posix::{vfs, wait_exit};

/// (program, arguments, expected exit code)
const PROGRAMS: &[(&str, &[&str], i64)] = &[
    ("/nova-tests/hello", &[], 7),
    ("/nova-tests/threads", &[], 0),
    ("/nova-tests/memory", &[], 0),
    ("/nova-tests/files", &[], 0),
    ("/nova-tests/events", &[], 0),
    ("/nova-tests/signals", &[], 0),
    ("/nova-tests/dynamic", &[], 0),
    ("/nova-tests/spawn", &[], 0),
    ("/nova-tests/mmstress", &["1"], 0),
    ("/nova-tests/mmstress", &["m"], 0),
    ("/nova-tests/mmstress", &["b"], 0),
    ("/nova-tests/mmstress", &[], 0),
    ("/nova-tests/alsa", &["muet"], 0),
];

pub fn run() -> (u32, u32, u32) {
    let mut passed = 0;
    let mut failed = 0;
    let mut absent = 0;
    for (path, args, expected) in PROGRAMS {
        if vfs::walk("/", path, true).is_err() {
            absent += 1;
            continue;
        }
        let mut argv: Vec<&str> = alloc::vec![*path];
        argv.extend_from_slice(args);
        let envp = ["PATH=/usr/bin:/bin", "HOME=/home/nova", "LANG=C.UTF-8"];
        let start = crate::idt::ticks();
        let outcome = super::spawn(path, &argv, &envp).map(|pid| wait_exit(pid, Some(6000)));
        let elapsed = crate::idt::ticks() - start;
        match outcome {
            Ok(Some(code)) if code == *expected => {
                passed += 1;
                crate::serial_println!("[linux] {} : code {} en {} tics - conforme", path, code, elapsed);
            }
            Ok(Some(code)) => {
                failed += 1;
                crate::serial_println!("[linux] ECHEC {} : code {} (attendu {})", path, code, expected);
            }
            Ok(None) => {
                failed += 1;
                crate::serial_println!("[linux] ECHEC {} : pas termine en 60 s", path);
            }
            Err(e) => {
                failed += 1;
                crate::serial_println!("[linux] ECHEC {} : lancement refuse, errno {}", path, e);
            }
        }
    }
    (passed, failed, absent)
}

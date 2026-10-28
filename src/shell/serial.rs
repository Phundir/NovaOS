//! The shell on the serial console (spec 2.2, mode recovery).
//!
//! The recovery entry of the GRUB menu promises "kernel + fs.dsa + shell
//! CLI only": a machine whose graphical session does not start must
//! still be repairable. COM1 is the one console that needs neither the
//! compositor nor a keyboard driver. The task reads the UART by polling
//! (the boot never armed its interrupt, and a person types slowly), echoes
//! what is typed, and runs each line in a shell with ROOT's credentials --
//! a machine under repair has no session.
//!
//! Outside the recovery mode the same task listens too, silently: it
//! prints nothing until a line is typed, so the boot log stays the boot
//! log, and `make run-nogui` gets a shell for free.

use alloc::string::String;

use super::Shell;

/// Longest line accepted.
const MAX_LINE: usize = 512;

fn put(text: &str) {
    crate::serial_print!("{}", text);
}

/// The `shell-serie` task.
pub fn task() -> ! {
    let recovery = crate::multiboot::is_recovery();
    let mut shell = Shell::new("root", crate::fs::perm::Credentials::kernel());
    if recovery {
        crate::serial_println!();
        crate::serial_println!("=== Nova OS - mode recovery : shell sur la console serie ===");
        crate::serial_println!("\"aide\" liste les commandes.");
        put(&shell.prompt());
    }

    let mut line = String::new();
    let mut prompted = recovery;
    loop {
        let mut got = false;
        while let Some(byte) = crate::serial::try_read() {
            got = true;
            match byte {
                b'\r' | b'\n' => {
                    crate::serial_println!();
                    let output = shell.run(&line);
                    for text in &output.lines {
                        crate::serial_println!("{}", text);
                    }
                    line.clear();
                    if output.exit {
                        crate::serial_println!("(le shell serie reste disponible : une ligne le rouvre)");
                    }
                    put(&shell.prompt());
                    prompted = true;
                }
                0x08 | 0x7F => {
                    if line.pop().is_some() {
                        crate::serial::write_bytes(&[0x08, b' ', 0x08]);
                    }
                }
                0x03 => {
                    // Ctrl+C: the line is dropped.
                    line.clear();
                    crate::serial_println!("^C");
                    put(&shell.prompt());
                }
                0x20..=0x7E if line.len() < MAX_LINE => {
                    if !prompted {
                        // First key outside recovery: show where we are.
                        crate::serial_println!();
                        put(&shell.prompt());
                        prompted = true;
                    }
                    line.push(byte as char);
                    crate::serial_print!("{}", byte as char);
                }
                _ => {}
            }
        }
        let wait = if got { 5 } else { 50 };
        let _ = crate::syscall::invoke(crate::syscall::domain_time::SYS_TIME_SLEEP_MS, [wait, 0, 0, 0, 0, 0]);
    }
}

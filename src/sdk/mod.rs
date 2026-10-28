//! Phase 9 -- SDK and languages (spec 13 and 15).
//!
//! ## What this phase adds
//!
//! Until now, everything Nova OS executes has been **made by the
//! kernel**: the sample binaries by `binfmt::sample`, the GUI modules
//! by `install_native_modules`, the phase 8 packages by
//! `spm::package::build`. Even the door SPM opened to outside content
//! assumes someone, somewhere, produced the `.spm`.
//!
//! Phase 9 is that "someone, somewhere". It is the first time the
//! system can turn **text written by a human** into a binary that runs
//! in ring 3. The whole chain is five steps, and none of them is
//! simulated:
//!
//! ```text
//!   dev.toml  --> manifest::parse   --> DevManifest valid (3.4, 12.4, 13.2)
//!   main.asm  --> asm::assemble     --> Object (bytes + labels + holes)
//!             --> emit::link        --> RIP displacements resolved
//!             --> emit::emit        --> complete .cus/.gs/.dsa file
//!             --> loader::load      --> ring 3 task
//! ```
//!
//! ## Layout
//!
//! | Module | Covers |
//! |---|---|
//! | `runtime`   | 9.5 -- table 13.2, locations 13.3, user > system resolution |
//! | `manifest`  | 9.2 -- `dev.toml` and its cross-validation |
//! | `asm`       | 9.5.7 -- x86_64 assembler, position-independent |
//! | `emit`      | 9.1.2 -- linking and emission of a Nova binary |
//! | `script`    | 9.1.4 -- built-in interpreter, bounded in steps and depth |
//! | `template`  | 9.1.1 -- the five templates of 15.5 |
//! | `sandbox`   | 9.3 -- dedicated memory, FS overlay, loopback, `.ras` replay, purge |
//! | `etr`       | 9.6 -- JavaScript AST analysis, mapping, 70 % threshold |
//! | `libdev`    | 9.4.3 -- catalogue of the abstractions and of their cost |
//! | `cli`       | 9.1 -- the six verbs of 15.2 |
//!
//! ## What phase 9 does not do, and why
//!
//! * **No Rust, C, C#, Java, Python or JavaScript compiler.** Seven of
//!   the eight lines of 13.2 assume an external toolchain.
//!   `sdk::runtime` declares them, knows how to resolve them under
//!   `/sys/runtimes/`, and **refuses by name** when they are missing. No
//!   `dev build` will hand back a binary of `nop` and claim it compiled.
//! * **No debugger.** 15.2 writes `dev run --debug`; there is neither a
//!   breakpoint nor register inspection. What `--debug` does is described
//!   in `sdk::cli`, and stops at what it actually does.
//! * **`libdev` is not shipped as a binary.** That would take a compiler.
//!   What exists is its contract, checked against the phase 5 call table.

pub mod asm;
pub mod cli;
pub mod emit;
pub mod etr;
pub mod debug;
pub mod js;
pub mod libdev;
pub mod manifest;
pub mod python;
pub mod runtime;
pub mod rustc;
pub mod sandbox;
pub mod script;
pub mod selftest;
pub mod template;

use core::sync::atomic::{AtomicBool, Ordering};

use crate::fs::perm::{Credentials, Mode};

/// Set once `start` has run, whatever it managed to do.
///
/// The phase 9 self-test runs as a task, and nothing in the scheduler
/// guarantees it is created after `start`. It used to gate itself on
/// `runtime::resolve`, which is already true on a warm volume: the layout
/// survives a reboot. The self-test therefore overtook `start` on every
/// boot but the first, and raced its stale-sandbox purge.
static STARTED: AtomicBool = AtomicBool::new(false);

/// Whether `start` has already run on this boot.
pub fn is_started() -> bool {
    STARTED.load(Ordering::Acquire)
}

/// Root of the SDK tools (spec 17).
pub const SDK_DIR: &str = "/sys/tools/sdk";
/// Root of the ETR tool.
pub const ETR_DIR: &str = "/sys/tools/etr";
/// Root of the interpreters.
pub const INTERPRET_DIR: &str = "/sys/tools/interpret";
/// Root of the compilers.
pub const COMPILE_DIR: &str = "/sys/tools/compile";

/// Lays down the SDK directory tree.
///
/// Idempotent, like the phase 8 one: a second boot must recreate nothing
/// and overwrite nothing, or a runtime the user installed would vanish on
/// every restart.
pub fn install_layout(now: u64) -> usize {
    if !crate::fs::is_ready() {
        return 0;
    }
    let who = Credentials::kernel();
    let mut created = 0;

    for path in [
        SDK_DIR,
        ETR_DIR,
        INTERPRET_DIR,
        COMPILE_DIR,
        runtime::SYSTEM_DIR,
        sandbox::OVERLAY_ROOT,
    ] {
        if crate::fs::stat(path, who).is_err() {
            // Sandboxes live under `/cache/`, open to the group: opening
            // a test context must not require any particular right. The
            // rest is system-owned.
            let mode = if path == sandbox::OVERLAY_ROOT {
                Mode::shared_dir()
            } else {
                Mode::system_dir()
            };
            if crate::fs::create_directory(path, who, mode, now).is_ok() {
                created += 1;
            }
        }
    }

    created += runtime::install_layout(now);
    created
}

/// Starts phase 9.
///
/// Called by `init.dsa` after `spm::start`: the SDK writes under `/sys/`,
/// which assumes the filesystem is mounted.
pub fn start(now: u64) {
    if !crate::fs::is_ready() {
        crate::serial_println!("[sdk] systeme de fichiers absent - phase 9 inactive");
        STARTED.store(true, Ordering::Release);
        return;
    }

    let created = install_layout(now);

    crate::serial_println!("[sdk] arborescence prete - {} entree(s) creee(s)", created);

    // A sandbox left behind by an interrupted run would survive the
    // reboot: `/cache/` is persistent. The phase 7 lesson applies here
    // unchanged -- purge on the way in as much as on the way out.
    let stale = purge_stale_sandboxes(now);
    if stale > 0 {
        crate::serial_println!(
            "[sdk] {} bac(s) a sable laisse(s) par une session precedente - purges",
            stale
        );
    }

    // Released last: a task waiting on this flag must not start before the
    // purge above is done, or it would create a sandbox the purge then
    // deletes.
    STARTED.store(true, Ordering::Release);
}

/// Deletes orphaned sandbox directories.
///
/// `/cache/` is persistent, so a sandbox interrupted by a reset leaves its
/// overlay behind; nothing else will ever come and collect it.
///
/// What this must not do is delete a sandbox that is alive. The registry
/// is in memory and therefore empty at boot, but "at boot" is not "when
/// this runs": tasks are already being scheduled when `start` is called,
/// and one of them may have opened a sandbox in the meantime. A directory
/// whose name matches a live identifier is skipped -- everything else
/// predates the reboot.
fn purge_stale_sandboxes(now: u64) -> usize {
    let who = Credentials::kernel();
    let Ok(entries) = crate::fs::list(sandbox::OVERLAY_ROOT, who) else {
        return 0;
    };
    let live = sandbox::live_ids();
    let mut purged = 0;
    for entry in entries {
        // A sandbox directory is named after its identifier. Anything else
        // under this root was not put there by `sandbox::create`, and is
        // swept with the rest.
        if let Ok(id) = entry.name.parse::<u32>()
            && live.contains(&id)
        {
            continue;
        }
        let path = alloc::format!("{}/{}", sandbox::OVERLAY_ROOT, entry.name);
        if purge_tree(&path, now) > 0 {
            purged += 1;
        }
    }
    purged
}

/// Erases a subtree, leaves first.
fn purge_tree(path: &str, now: u64) -> usize {
    let who = Credentials::kernel();
    let mut removed = 0;
    if let Ok(entries) = crate::fs::list(path, who) {
        for entry in entries {
            let child = alloc::format!("{}/{}", path, entry.name);
            if crate::fs::list(&child, who).is_ok() {
                removed += purge_tree(&child, now);
            }
            if crate::fs::delete(&child, who, now).is_ok() {
                removed += 1;
            }
        }
    }
    if crate::fs::delete(path, who, now).is_ok() {
        removed += 1;
    }
    removed
}

pub fn dump_state() {
    runtime::dump_state();
    rustc_state();
    cli::dump_state();
    sandbox::dump_state();
}

/// What the Rust front-end of 9.5.1 covers.
///
/// Printed with the rest because a subset that is not stated is a subset
/// that will be discovered by a refusal. The line above it in the 13.2
/// table now says "chaine complete" for Rust, and this is what that
/// claim actually amounts to.
fn rustc_state() {
    crate::serial_println!(
        "[rustc] sous-ensemble Rust compile en natif - profondeur d'interpretation {} appels",
        rustc::interp::MAX_DEPTH
    );
    for (topic, detail) in rustc::describe() {
        crate::serial_println!("[rustc]   {:<12} {}", topic, detail);
    }
    crate::serial_println!("[rustc] appels integres :");
    for entry in rustc::BUILTINS.iter() {
        crate::serial_println!(
            "[rustc]   {:<10} -> {}",
            entry.name,
            entry.call
        );
    }
}

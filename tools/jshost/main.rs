//! The JavaScript engine of Nova, compiled for the host (`make js-test`).
//!
//! The engine (`src/web/js/`) is plain `alloc` code: the same files
//! compile here with `std` underneath, and the corpus of
//! `web::js::selftest` runs in a fraction of a second instead of a boot
//! of QEMU. What the kernel provides is shimmed below: the stack headroom
//! (measured against this thread's own stack), the heap check (the host
//! has room), and `serial_println!` (the terminal).
//!
//! Built by `rustc` directly, not by cargo: the repository's
//! `.cargo/config.toml` points every cargo build at the kernel target.
//!
//! Usage:
//!   jshost                  run the corpus
//!   jshost 'return 1 + 1'   run one function body, print the outcome

#![allow(dead_code, unused_imports, unused_macros, unexpected_cfgs)]

extern crate alloc;

macro_rules! serial_println {
    ($($t:tt)*) => { println!($($t)*) };
}
pub(crate) use serial_println;

pub mod scheduler {
    use std::cell::Cell;

    thread_local! {
        pub static BASE: Cell<usize> = const { Cell::new(0) };
    }

    pub const SIZE: usize = 8 << 20;

    pub fn stack_headroom() -> usize {
        let marker = 0u8;
        let here = &marker as *const u8 as usize;
        let base = BASE.with(|b| b.get());
        SIZE.saturating_sub(base.saturating_sub(here))
    }
}

pub mod version {
    pub const VERSION: &str = "hote";
}

pub mod heap {
    pub fn room_for(_bytes: usize) -> bool {
        true
    }
}

#[path = "../../src/json.rs"]
pub mod json;

/// `crate::web`, with only what the engine uses (`web/mod.rs`).
pub mod web;

fn run() -> bool {
    let args: Vec<String> = std::env::args().collect();
    let headroom = &scheduler::stack_headroom;
    // `jshost --module main.ts`: a module from the disk, with its
    // directory's files, as `dev interpret` would run it.
    if args.get(1).map(String::as_str) == Some("--module") {
        let Some(main) = args.get(2) else {
            println!("usage : jshost --module fichier");
            return false;
        };
        let main = std::fs::canonicalize(main).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| main.clone());
        let directory = std::path::Path::new(&main).parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let mut owned: Vec<(String, String)> = Vec::new();
        let mut pending = vec![directory];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if let Ok(text) = std::fs::read_to_string(&path) {
                    owned.push((path.to_string_lossy().into_owned(), text));
                }
            }
        }
        let files: Vec<(&str, &str)> = owned.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
        println!("{}", web::js::selftest::run_module_case(&files, &main, headroom));
        return true;
    }
    if let Some(body) = args.get(1) {
        let outcome = web::js::selftest::run_case(body, headroom);
        println!("{:?}", outcome);
        return outcome.is_ok();
    }
    let (passed, failed) = web::js::selftest::run(headroom);
    println!("[jshost] {} cas reussis, {} en echec", passed, failed);
    failed == 0
}

fn main() {
    let child = std::thread::Builder::new()
        .stack_size(scheduler::SIZE)
        .spawn(|| {
            let marker = 0u8;
            scheduler::BASE.with(|b| b.set(&marker as *const u8 as usize));
            run()
        })
        .expect("thread");
    let ok = child.join().unwrap_or(false);
    std::process::exit(if ok { 0 } else { 1 });
}

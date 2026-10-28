//! The Python interpreter of Nova, compiled for the host (`make py-test`).
//!
//! The interpreter (`src/python/`) is plain `alloc` code over the
//! JavaScript engine's soft-float numbers and regular expressions: the
//! same files compile here with `std` underneath. The host is the real
//! disk and terminal. The corpus (`tests/python/*.py`) runs here against
//! the outputs CPython 3.13 gave for the same programs (`*.out`, written
//! by `tools/pyhost/reference.sh`), in a second instead of a boot of QEMU.
//!
//! Built by `rustc` directly, not by cargo: the repository's
//! `.cargo/config.toml` points every cargo build at the kernel target.
//!
//! Usage:
//!   pyhost program.py [args...]   run one program
//!   pyhost --corpus directory      run every test against its .out

#![allow(dead_code, unused_imports, unused_macros, unexpected_cfgs)]

extern crate alloc;

use std::io::{BufRead, Write};

/// `crate::web::js`, with only what the interpreter uses.
pub mod web;

#[path = "../../src/python/mod.rs"]
pub mod python;

use python::{FileError, PyHost};

mod stack {
    use std::cell::Cell;

    thread_local! {
        pub static BASE: Cell<usize> = const { Cell::new(0) };
    }

    pub const SIZE: usize = 64 << 20;

    pub fn headroom() -> usize {
        let marker = 0u8;
        let here = &marker as *const u8 as usize;
        let base = BASE.with(|b| b.get());
        SIZE.saturating_sub(base.saturating_sub(here))
    }
}

static HEADROOM: fn() -> usize = stack::headroom;

fn headroom_ref() -> &'static dyn Fn() -> usize {
    &HEADROOM
}

/// The host: the disk relative to the current directory, the terminal
/// or buffers for the corpus.
struct StdHost {
    stdout: Option<Vec<u8>>,
    stderr: Option<Vec<u8>>,
    input: Vec<String>,
    start: std::time::Instant,
}

fn file_error(e: std::io::Error) -> FileError {
    match e.kind() {
        std::io::ErrorKind::NotFound => FileError::NotFound,
        std::io::ErrorKind::AlreadyExists => FileError::Exists,
        std::io::ErrorKind::PermissionDenied => FileError::Permission,
        _ => {
            let text = e.to_string();
            if text.contains("Is a directory") {
                FileError::IsDir
            } else if text.contains("Not a directory") {
                FileError::NotDir
            } else if text.contains("not empty") {
                FileError::NotEmpty
            } else {
                FileError::Other(text)
            }
        }
    }
}

impl PyHost for StdHost {
    fn write(&mut self, text: &str, error: bool) {
        let target = if error { &mut self.stderr } else { &mut self.stdout };
        match target {
            Some(buffer) => buffer.extend_from_slice(text.as_bytes()),
            None => {
                if error {
                    let _ = std::io::stderr().write_all(text.as_bytes());
                } else {
                    let _ = std::io::stdout().write_all(text.as_bytes());
                }
            }
        }
    }

    fn input(&mut self) -> Option<String> {
        if self.stdout.is_some() {
            if self.input.is_empty() {
                return None;
            }
            return Some(self.input.remove(0));
        }
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        match std::io::stdin().lock().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line),
        }
    }

    fn read_file(&mut self, path: &str) -> Result<Vec<u8>, FileError> {
        if std::path::Path::new(path).is_dir() {
            return Err(FileError::IsDir);
        }
        std::fs::read(path).map_err(file_error)
    }

    fn write_file(&mut self, path: &str, data: &[u8]) -> Result<(), FileError> {
        if std::path::Path::new(path).is_dir() {
            return Err(FileError::IsDir);
        }
        std::fs::write(path, data).map_err(file_error)
    }

    fn list_dir(&mut self, path: &str) -> Result<Vec<String>, FileError> {
        let mut names: Vec<String> = std::fs::read_dir(path)
            .map_err(file_error)?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        Ok(names)
    }

    fn make_dir(&mut self, path: &str) -> Result<(), FileError> {
        std::fs::create_dir(path).map_err(file_error)
    }

    fn remove(&mut self, path: &str) -> Result<(), FileError> {
        if std::path::Path::new(path).is_dir() {
            std::fs::remove_dir(path).map_err(file_error)
        } else {
            std::fs::remove_file(path).map_err(file_error)
        }
    }

    fn kind(&mut self, path: &str) -> Option<bool> {
        std::fs::metadata(path).ok().map(|m| m.is_dir())
    }

    fn cwd(&self) -> String {
        std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| String::from("."))
    }

    fn time_ns(&self) -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0)
    }

    fn monotonic_ns(&self) -> u64 {
        self.start.elapsed().as_nanos() as u64
    }

    fn sleep(&mut self, nanoseconds: u64) {
        std::thread::sleep(std::time::Duration::from_nanos(nanoseconds.min(5_000_000_000)));
    }

    fn memory_ok(&self) -> bool {
        true
    }

    fn can_allocate(&self, _bytes: usize) -> bool {
        true
    }
}

/// Runs `body` on a thread with a large stack, as the kernel's task.
fn on_big_stack<T: Send + 'static>(body: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(stack::SIZE)
        .spawn(move || {
            let marker = 0u8;
            stack::BASE.with(|b| b.set(&marker as *const u8 as usize));
            body()
        })
        .expect("thread")
        .join()
        .expect("join")
}

const FUEL: u64 = 2_000_000_000;

/// One program: (stdout, stderr, exit code).
fn run_program(path: &str, arguments: Vec<String>, capture: bool, input: Vec<String>) -> (String, String, i64) {
    let path = path.to_string();
    on_big_stack(move || {
        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => return (String::new(), format!("{}: {}\n", path, e), 2),
        };
        let host = StdHost {
            stdout: if capture { Some(Vec::new()) } else { None },
            stderr: if capture { Some(Vec::new()) } else { None },
            input,
            start: std::time::Instant::now(),
        };
        // The host's buffers come back through a shared slot.
        let shared = std::rc::Rc::new(std::cell::RefCell::new((Vec::new(), Vec::new())));
        let host = Capturing { inner: host, out: shared.clone() };
        let mut argv = vec![path.clone()];
        argv.extend(arguments);
        let outcome = python::run_main(Box::new(host), &path, &source, argv, FUEL, headroom_ref());
        let (out, err) = shared.borrow().clone();
        let mut err = String::from_utf8_lossy(&err).into_owned();
        if let Some(reason) = &outcome.halted {
            err.push_str(&format!("[arret] {}\n", reason));
        }
        (String::from_utf8_lossy(&out).into_owned(), err, outcome.exit_code)
    })
}

/// A host whose buffers are handed back when the run ends.
struct Capturing {
    inner: StdHost,
    out: std::rc::Rc<std::cell::RefCell<(Vec<u8>, Vec<u8>)>>,
}

impl Drop for Capturing {
    fn drop(&mut self) {
        let out = self.inner.stdout.take().unwrap_or_default();
        let err = self.inner.stderr.take().unwrap_or_default();
        *self.out.borrow_mut() = (out, err);
    }
}

impl PyHost for Capturing {
    fn write(&mut self, text: &str, error: bool) {
        self.inner.write(text, error)
    }
    fn input(&mut self) -> Option<String> {
        self.inner.input()
    }
    fn read_file(&mut self, path: &str) -> Result<Vec<u8>, FileError> {
        self.inner.read_file(path)
    }
    fn write_file(&mut self, path: &str, data: &[u8]) -> Result<(), FileError> {
        self.inner.write_file(path, data)
    }
    fn list_dir(&mut self, path: &str) -> Result<Vec<String>, FileError> {
        self.inner.list_dir(path)
    }
    fn make_dir(&mut self, path: &str) -> Result<(), FileError> {
        self.inner.make_dir(path)
    }
    fn remove(&mut self, path: &str) -> Result<(), FileError> {
        self.inner.remove(path)
    }
    fn kind(&mut self, path: &str) -> Option<bool> {
        self.inner.kind(path)
    }
    fn cwd(&self) -> String {
        self.inner.cwd()
    }
    fn time_ns(&self) -> u64 {
        self.inner.time_ns()
    }
    fn monotonic_ns(&self) -> u64 {
        self.inner.monotonic_ns()
    }
    fn sleep(&mut self, nanoseconds: u64) {
        self.inner.sleep(nanoseconds)
    }
    fn memory_ok(&self) -> bool {
        true
    }
    fn can_allocate(&self, bytes: usize) -> bool {
        self.inner.can_allocate(bytes)
    }
}

/// The last line of a traceback: the exception and its message.
fn last_line(text: &str) -> &str {
    text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("")
}

fn corpus(directory: &str) -> bool {
    let mut tests: Vec<std::path::PathBuf> = std::fs::read_dir(directory)
        .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "py")).collect())
        .unwrap_or_default();
    tests.sort();
    let mut passed = 0;
    let mut failed = 0;
    let started = std::time::Instant::now();
    for test in &tests {
        let name = test.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name.starts_with('_') {
            continue;
        }
        let expected_path = test.with_extension("out");
        let Ok(expected) = std::fs::read_to_string(&expected_path) else {
            println!("SANS REFERENCE  {}", name);
            failed += 1;
            continue;
        };
        let input_path = test.with_extension("in");
        let input: Vec<String> = std::fs::read_to_string(&input_path).map(|t| t.lines().map(|l| format!("{}\n", l)).collect()).unwrap_or_default();
        // The reference: stdout, then "--- exit N" and the last line of
        // stderr (the uncaught exception), if any.
        let directory = test.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let previous = std::env::current_dir().ok();
        let _ = std::env::set_current_dir(&directory);
        let (out, err, code) = run_program(&name, Vec::new(), true, input);
        if let Some(p) = previous {
            let _ = std::env::set_current_dir(p);
        }
        let mut got = out.clone();
        if code != 0 || !err.is_empty() {
            got.push_str(&format!("--- exit {}\n", code));
            let last = last_line(&err);
            if !last.is_empty() {
                got.push_str(last);
                got.push('\n');
            }
        }
        if got == expected {
            passed += 1;
        } else {
            failed += 1;
            println!("ECHEC  {}", name);
            let expected_lines: Vec<&str> = expected.lines().collect();
            let got_lines: Vec<&str> = got.lines().collect();
            for i in 0..expected_lines.len().max(got_lines.len()) {
                let e = expected_lines.get(i).copied().unwrap_or("<fin>");
                let g = got_lines.get(i).copied().unwrap_or("<fin>");
                if e != g {
                    println!("  ligne {}:\n    attendu : {}\n    obtenu  : {}", i + 1, e, g);
                    break;
                }
            }
            if !err.is_empty() && code != 0 {
                for line in err.lines().take(12) {
                    println!("    | {}", line);
                }
            }
        }
    }
    println!("[py-test] {} programmes conformes a CPython 3.13, {} en echec ({} ms)", passed, failed, started.elapsed().as_millis());
    failed == 0
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let ok = match args.get(1).map(String::as_str) {
        Some("--corpus") => corpus(args.get(2).map(String::as_str).unwrap_or("tests/python")),
        Some(path) => {
            let (_, _, code) = run_program(path, args[2..].to_vec(), false, Vec::new());
            std::process::exit(code as i32);
        }
        None => {
            println!("usage : pyhost programme.py [arguments] | pyhost --corpus repertoire");
            false
        }
    };
    std::process::exit(if ok { 0 } else { 1 });
}

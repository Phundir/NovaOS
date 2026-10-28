//! `dev interpret main.py` (16.5): the "interpreter" column of the Python
//! row of spec 13.2, answered by Nova's own interpreter (`crate::python`)
//! rather than a CPython that does not exist here.
//!
//! ## What a program finds
//!
//! The language (Python 3.13 minus the parts `crate::python` lists),
//! the standard modules it ships, and the program's own modules: `import
//! util` finds `util.py` beside `main.py`, a directory with `__init__.py`
//! is a package. Relative file paths (`open("data.txt")`) start from the
//! program's directory, as Node's do in `sdk::js`.
//!
//! ## With whose rights
//!
//! Those `dev interpret` passes: the session's, or ROOT's on the recovery
//! console (`sdk::js::session_rights`). A program reads and writes what
//! its user may, nothing more.
//!
//! ## Where it runs
//!
//! The interpreter keeps Python's frames on the heap; Rust recursion only
//! happens when native code calls back into Python (`sorted(key=...)`,
//! `__eq__` from a dict), each level checked against the stack headroom.
//! The program runs in a task of its own with 1 MiB of stack (`dev-py`),
//! and the caller waits for it. Its standard input is what the caller
//! hands over (empty for `dev interpret`: `input()` raises `EOFError`).

pub mod selftest;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use crate::fs::perm::Credentials;
use crate::python::{FileError, PyHost};
use crate::vga::SpinLock;

/// Steps a program may take, all included.
const FUEL: u64 = 400_000_000;
/// Stack of the task that runs it.
const STACK: usize = 1024 * 1024;
/// Largest source accepted.
const MAX_SOURCE: usize = 4 * 1024 * 1024;

/// What a run gives back.
#[derive(Debug, Clone)]
pub struct PyRun {
    /// Standard output, by line.
    pub output: Vec<String>,
    /// Standard error (tracebacks), by line.
    pub errors: Vec<String>,
    /// `sys.exit(n)`, 1 after an uncaught exception, else 0.
    pub exit_code: i64,
    /// Why the run was stopped from outside the language, if it was.
    pub halted: Option<String>,
}

/// What the program wrote, shared with the host it was handed to.
type Streams = Rc<RefCell<(Vec<u8>, Vec<u8>)>>;

/// The kernel's side of `PyHost`.
struct KernelHost {
    directory: String,
    who: Credentials,
    streams: Streams,
    input: Vec<String>,
}

fn file_error(error: crate::fs::FsError) -> FileError {
    match error {
        crate::fs::FsError::NotFound => FileError::NotFound,
        crate::fs::FsError::AlreadyExists => FileError::Exists,
        crate::fs::FsError::PermissionDenied(_) => FileError::Permission,
        crate::fs::FsError::IsADirectory => FileError::IsDir,
        crate::fs::FsError::NotADirectory => FileError::NotDir,
        crate::fs::FsError::DirectoryNotEmpty => FileError::NotEmpty,
        other => FileError::Other(String::from(other.message())),
    }
}

impl KernelHost {
    /// A path of the program, absolute.
    fn full(&self, path: &str) -> String {
        let joined = if path.starts_with('/') { String::from(path) } else { alloc::format!("{}/{}", self.directory, path) };
        crate::shell::normalize(&joined)
    }

    fn now(&self) -> u64 {
        crate::idt::ticks()
    }
}

impl PyHost for KernelHost {
    fn write(&mut self, text: &str, error: bool) {
        let mut streams = self.streams.borrow_mut();
        let target = if error { &mut streams.1 } else { &mut streams.0 };
        // A runaway print loop stops filling memory at 4 MiB.
        if target.len() < 4 * 1024 * 1024 {
            target.extend_from_slice(text.as_bytes());
        }
    }

    fn input(&mut self) -> Option<String> {
        if self.input.is_empty() {
            None
        } else {
            Some(self.input.remove(0))
        }
    }

    fn read_file(&mut self, path: &str) -> Result<Vec<u8>, FileError> {
        crate::fs::read_file(&self.full(path), self.who).map_err(file_error)
    }

    fn write_file(&mut self, path: &str, data: &[u8]) -> Result<(), FileError> {
        let now = self.now();
        crate::fs::write_file(&self.full(path), data, self.who, crate::fs::perm::Mode::private_file(), now)
            .map(|_| ())
            .map_err(file_error)
    }

    fn list_dir(&mut self, path: &str) -> Result<Vec<String>, FileError> {
        let entries = crate::fs::list(&self.full(path), self.who).map_err(file_error)?;
        let mut names: Vec<String> = entries.into_iter().map(|e| e.name).filter(|n| n != "." && n != "..").collect();
        names.sort();
        Ok(names)
    }

    fn make_dir(&mut self, path: &str) -> Result<(), FileError> {
        let now = self.now();
        crate::fs::create_directory(&self.full(path), self.who, crate::fs::perm::Mode::private_dir(), now)
            .map(|_| ())
            .map_err(file_error)
    }

    fn remove(&mut self, path: &str) -> Result<(), FileError> {
        let now = self.now();
        crate::fs::delete(&self.full(path), self.who, now).map_err(file_error)
    }

    fn kind(&mut self, path: &str) -> Option<bool> {
        crate::fs::stat(&self.full(path), self.who).ok().map(|node| node.kind == crate::fs::format::NodeKind::Directory)
    }

    fn cwd(&self) -> String {
        self.directory.clone()
    }

    fn time_ns(&self) -> u64 {
        let ms = crate::rtc::now_millis().unwrap_or(crate::idt::ticks() * (1000 / crate::TIMER_HZ as u64));
        ms.saturating_mul(1_000_000)
    }

    fn monotonic_ns(&self) -> u64 {
        crate::idt::ticks().saturating_mul(1_000_000_000 / crate::TIMER_HZ as u64)
    }

    fn sleep(&mut self, nanoseconds: u64) {
        let ticks = nanoseconds.div_ceil(1_000_000_000 / crate::TIMER_HZ as u64).min(60 * crate::TIMER_HZ as u64);
        let deadline = crate::idt::ticks() + ticks;
        while crate::idt::ticks() < deadline {
            crate::scheduler::yield_now();
        }
    }

    fn memory_ok(&self) -> bool {
        crate::heap::room_for(0)
    }

    fn can_allocate(&self, bytes: usize) -> bool {
        crate::heap::room_for(bytes)
    }
}

fn lines(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut out: Vec<String> = text.split('\n').map(String::from).collect();
    if out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out
}

fn dirname(path: &str) -> String {
    match path.rfind('/') {
        Some(0) => String::from("/"),
        Some(index) => String::from(&path[..index]),
        None => String::from("."),
    }
}

/// Runs `path` here, in the calling task: it must have the stack
/// (`run_file` gives it one).
fn run_here(path: &str, arguments: &[String], input: Vec<String>, who: Credentials) -> PyRun {
    let streams: Streams = Rc::new(RefCell::new((Vec::new(), Vec::new())));
    let failed = |message: String| PyRun { output: Vec::new(), errors: alloc::vec![message], exit_code: 2, halted: None };
    let source = match crate::fs::read_file(path, who) {
        Ok(bytes) if bytes.len() > MAX_SOURCE => return failed(alloc::format!("{} : source trop grande", path)),
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => return failed(alloc::format!("{} : source non UTF-8", path)),
        },
        Err(error) => return failed(alloc::format!("python: can't open file '{}': {}", path, error.message())),
    };
    let host = KernelHost { directory: dirname(path), who, streams: streams.clone(), input };
    let mut argv = alloc::vec![String::from(path)];
    argv.extend(arguments.iter().cloned());
    static HEADROOM: fn() -> usize = crate::scheduler::stack_headroom;
    let outcome = crate::python::run_main(Box::new(host), path, &source, argv, FUEL, &HEADROOM);
    let (out, err) = streams.borrow().clone();
    PyRun { output: lines(&out), errors: lines(&err), exit_code: outcome.exit_code, halted: outcome.halted }
}

/// The run handed to the `dev-py` task, and its result.
struct Job {
    path: String,
    arguments: Vec<String>,
    input: Vec<String>,
    who: Credentials,
    run: Option<PyRun>,
}

static JOB: SpinLock<Option<Job>> = SpinLock::new(None);
/// One run at a time: the interpreter's registry of containers is global.
static BUSY: SpinLock<bool> = SpinLock::new(false);

fn worker() -> ! {
    let job = JOB.lock().as_ref().map(|j| (j.path.clone(), j.arguments.clone(), j.input.clone(), j.who));
    if let Some((path, arguments, input, who)) = job {
        let run = run_here(&path, &arguments, input, who);
        if let Some(entry) = JOB.lock().as_mut() {
            entry.run = Some(run);
        }
    }
    crate::scheduler::exit_current_with(0)
}

/// Runs a Python file with the rights `who` and the standard input
/// `input` (lines), in a task of its own, and waits for it.
pub fn run_file(path: &str, arguments: &[String], input: Vec<String>, who: Credentials) -> Result<PyRun, String> {
    {
        let mut busy = BUSY.lock();
        if *busy {
            return Err(String::from("un autre programme Python tourne deja"));
        }
        *busy = true;
    }
    *JOB.lock() = Some(Job { path: String::from(path), arguments: arguments.to_vec(), input, who, run: None });
    let pid = crate::scheduler::spawn_with_stack("dev-py", crate::process::BinaryType::Cus, worker, STACK);
    crate::scheduler::wait_for(pid);
    let result = JOB.lock().take().and_then(|job| job.run);
    *BUSY.lock() = false;
    result.ok_or_else(|| String::from("le programme s'est arrete sans resultat"))
}

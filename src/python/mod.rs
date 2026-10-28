//! Python 3: the interpreter behind `dev interpret main.py` (16.5).
//!
//! ## A bytecode machine
//!
//! The source goes through a lexer (indentation becomes INDENT/DEDENT
//! tokens), a recursive-descent parser, a symbol table (which name is
//! local, global, free, a cell), and a compiler to CPython-like stack
//! code (`code.rs`). The virtual machine (`vm.rs`) keeps its frames on
//! the heap: a Python call is a frame pushed, not a Rust call, so
//! `yield` can suspend a frame and recursion is bounded by
//! `sys.getrecursionlimit()`, not by the kernel stack.
//!
//! ## CPython's behaviour, observable
//!
//! What a program prints is what CPython prints: unbounded ints (`int.rs`),
//! IEEE doubles with the shortest repr (`format.rs`), dicts in insertion
//! order and sets in CPython's own table order (`dict.rs`), the same
//! exception classes and messages, `random.seed(n)` giving the same
//! numbers (Mersenne Twister, `modules/random.rs`). The test corpus is
//! checked against CPython 3.13 on the host (`make py-test`).
//!
//! ## The subset
//!
//! Python 3.13's language minus: `async`/`await`, `match`, `except*`,
//! type parameter syntax, metaclasses (`metaclass=` is accepted and
//! ignored), `complex`, `bytearray`, `memoryview`; the standard library is
//! the modules of `modules/` (native or shipped as Python source).
//!
//! ## The host
//!
//! Everything outside the language goes through `PyHost`: the console,
//! files, the clock, the memory check. The kernel's host (`sdk::python`)
//! answers with the file system and the session's rights; the host build
//! (`tools/pyhost`) with the real disk.

pub mod args;
pub mod ast;
pub mod builtins;
pub mod code;
pub mod compile;
pub mod dict;
pub mod format;
pub mod int;
pub mod lex;
pub mod modules;
pub mod ops;
pub mod parse;
pub mod symtable;
pub mod types;
pub mod value;
pub mod vm;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use code::Code;
use dict::Dict;
use value::*;
use vm::{Exit, PyErr, PyResult, Vm};

/// Why a file operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileError {
    NotFound,
    Exists,
    Permission,
    IsDir,
    NotDir,
    NotEmpty,
    Other(String),
}

/// What the interpreter needs from its surroundings.
pub trait PyHost {
    /// Program output: stdout, or stderr when `error`.
    fn write(&mut self, text: &str, error: bool);
    /// A line of input, None at the end.
    fn input(&mut self) -> Option<String>;
    fn read_file(&mut self, path: &str) -> Result<Vec<u8>, FileError>;
    fn write_file(&mut self, path: &str, data: &[u8]) -> Result<(), FileError>;
    fn list_dir(&mut self, path: &str) -> Result<Vec<String>, FileError>;
    fn make_dir(&mut self, path: &str) -> Result<(), FileError>;
    fn remove(&mut self, path: &str) -> Result<(), FileError>;
    /// Some(true) for a directory, Some(false) for a file, None if absent.
    fn kind(&mut self, path: &str) -> Option<bool>;
    /// The directory relative paths start from.
    fn cwd(&self) -> String;
    /// Wall clock, nanoseconds since 1970.
    fn time_ns(&self) -> u64;
    /// A clock that only goes forward, nanoseconds.
    fn monotonic_ns(&self) -> u64;
    fn sleep(&mut self, nanoseconds: u64);
    /// Is the kernel heap still comfortable?
    fn memory_ok(&self) -> bool;
    /// Could `bytes` more be allocated?
    fn can_allocate(&self, bytes: usize) -> bool;
}

/// How a source is compiled.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Module,
    Eval,
}

/// A `SyntaxError` exception.
pub fn syntax_error(vm: &mut Vm, message: &str, filename: &str, line: u32, text: Option<&str>) -> PyErr {
    let class = vm.types.syntax_error.clone();
    let err = vm.error(&class, message);
    if let PyErr::Exc(Value::Instance(i)) = &err {
        let mut d = i.dict.borrow_mut();
        d.set_str("msg", Value::str(message));
        d.set_str("filename", Value::str(filename));
        d.set_str("lineno", Value::int(line as i64));
        d.set_str("text", text.map(Value::str).unwrap_or(Value::None));
    }
    err
}

/// Source to code; a syntax error becomes a `SyntaxError`.
pub fn compile(vm: &mut Vm, source: &str, filename: &str, mode: Mode) -> PyResult<Rc<Code>> {
    let result = match mode {
        Mode::Module => parse::parse(source)
            .map_err(|e| (e.message, e.line))
            .and_then(|body| compile::compile_module(&body, filename).map_err(|e| (e.message, e.line))),
        Mode::Eval => parse::parse_eval(source)
            .map_err(|e| (e.message, e.line))
            .and_then(|expr| compile::compile_eval(&expr, filename).map_err(|e| (e.message, e.line))),
    };
    // Tracebacks show the lines of files, not of `exec`'d strings.
    if !filename.starts_with('<') {
        vm.remember_source(filename, source);
    }
    match result {
        Ok(code) => Ok(code),
        Err((message, line)) => {
            let text = source.lines().nth((line as usize).saturating_sub(1));
            Err(syntax_error(vm, &message, filename, line, text))
        }
    }
}

/// Runs a module-level code with `globals`, names stored in
/// `namespace` (the module's dict if None), and gives its value.
pub fn run_code(vm: &mut Vm, code: Rc<Code>, globals: Rc<Module>, namespace: Option<Rc<RefCell<Dict>>>) -> PyResult<Value> {
    let mut frame = vm.module_frame(code, globals);
    if namespace.is_some() {
        frame.namespace = namespace;
    }
    vm.enter_native()?;
    let base = vm.frames.len();
    if let Err(e) = vm.push_frame(frame) {
        vm.leave_native();
        return Err(e);
    }
    let result = vm.run_from(base);
    vm.leave_native();
    match result? {
        Exit::Return(v) => Ok(v),
        Exit::Yield(_) => vm.type_error("'yield' outside function"),
    }
}

// ------------------------------------------------------------
//  Tracebacks
// ------------------------------------------------------------

/// The text CPython prints for an uncaught exception, chained ones
/// first.
pub fn format_exception(vm: &mut Vm, exc: &Value) -> String {
    let mut out = String::new();
    let mut seen: Vec<usize> = Vec::new();
    format_chain(vm, exc, &mut out, &mut seen);
    out
}

fn format_chain(vm: &mut Vm, exc: &Value, out: &mut String, seen: &mut Vec<usize>) {
    let Value::Instance(instance) = exc else {
        return;
    };
    let id = exc.pointer().unwrap_or(0);
    if seen.contains(&id) || seen.len() > 50 {
        return;
    }
    seen.push(id);
    let cause = instance.cause.borrow().clone();
    let context = instance.context.borrow().clone();
    let suppress = matches!(instance.dict.borrow().get_str("__suppress_context__"), Some(Value::Bool(true)));
    if !cause.is_none() {
        format_chain(vm, &cause, out, seen);
        out.push_str("\nThe above exception was the direct cause of the following exception:\n\n");
    } else if !context.is_none() && !suppress {
        format_chain(vm, &context, out, seen);
        out.push_str("\nDuring handling of the above exception, another exception occurred:\n\n");
    }
    let entries = instance.traceback.borrow().clone();
    if !entries.is_empty() {
        out.push_str("Traceback (most recent call last):\n");
        for (file, line, name) in entries.iter().rev() {
            out.push_str(&alloc::format!("  File \"{}\", line {}, in {}\n", file, line, name));
            if let Some(text) = vm.source_line(file, *line) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    out.push_str(&alloc::format!("    {}\n", trimmed));
                }
            }
        }
    }
    let class = instance.class();
    if class.is_subclass(&vm.types.syntax_error) {
        let d = instance.dict.borrow().clone();
        if let (Some(Value::Str(file)), Some(Value::Int(line))) = (d.get_str("filename"), d.get_str("lineno")) {
            let line = line.to_i64().unwrap_or(0) as u32;
            out.push_str(&alloc::format!("  File \"{}\", line {}\n", file, line));
            if let Some(Value::Str(text)) = d.get_str("text") {
                out.push_str(&alloc::format!("    {}\n", text.trim()));
            }
        }
    }
    out.push_str(&exception_line(vm, exc));
    out.push('\n');
    if let Some(Value::List(notes)) = instance.dict.borrow().get_str("__notes__") {
        for note in notes.borrow().iter() {
            if let Value::Str(s) = note {
                out.push_str(s);
                out.push('\n');
            }
        }
    }
}

/// `ValueError: message` (the qualified name for a user's exception
/// outside builtins).
pub fn exception_line(vm: &mut Vm, exc: &Value) -> String {
    let class = vm.class_of(exc);
    let name = match class.dict.borrow().get_str("__module__") {
        Some(Value::Str(m)) if &*m != "builtins" && &*m != "__main__" => alloc::format!("{}.{}", m, class.qualname.borrow()),
        _ => String::from(&**class.qualname.borrow()),
    };
    let text = if class.is_subclass(&vm.types.syntax_error) {
        match exc {
            Value::Instance(i) => match i.dict.borrow().get_str("msg") {
                Some(Value::Str(m)) => String::from(&*m),
                _ => format::safe_str(vm, exc),
            },
            _ => String::new(),
        }
    } else {
        format::safe_str(vm, exc)
    };
    if text.is_empty() { name } else { alloc::format!("{}: {}", name, text) }
}

// ------------------------------------------------------------
//  Running a program
// ------------------------------------------------------------

/// How a program ended.
pub struct Outcome {
    /// `sys.exit(n)`, 1 after an uncaught exception, else 0.
    pub exit_code: i64,
    /// Why the run was stopped from outside the language (budget,
    /// memory), if it was.
    pub halted: Option<String>,
}

/// Runs `source` as the `__main__` module of the program at `path`.
pub fn run_main(host: Box<dyn PyHost>, path: &str, source: &str, argv: Vec<String>, fuel: u64, headroom: &'static dyn Fn() -> usize) -> Outcome {
    let mut vm = Vm::new(host, headroom, fuel);
    let directory = match path.rfind('/') {
        Some(0) => String::from("/"),
        Some(i) => String::from(&path[..i]),
        None => String::from("."),
    };
    vm.path = alloc::vec![directory];
    vm.argv = argv;
    let module = modules::new_module(&mut vm, "__main__", path);
    let outcome = match compile(&mut vm, source, path, Mode::Module) {
        Ok(code) => {
            let result = run_code(&mut vm, code, module.clone(), None);
            finish(&mut vm, result.map(|_| ()))
        }
        Err(e) => finish(&mut vm, Err(e)),
    };
    vm.teardown();
    outcome
}

/// The end of a run: the files flushed, an uncaught exception printed.
fn finish(vm: &mut Vm, result: PyResult<()>) -> Outcome {
    let result = match result {
        Ok(()) => modules::flush_all(vm),
        Err(e) => {
            let _ = modules::flush_all(vm);
            Err(e)
        }
    };
    match result {
        Ok(()) => Outcome { exit_code: vm.exit_code.unwrap_or(0), halted: None },
        Err(PyErr::Halt(reason)) => Outcome { exit_code: 1, halted: Some(reason) },
        Err(PyErr::Exc(exc)) => {
            let system_exit = vm.types.system_exit.clone();
            if vm.class_of(&exc).is_subclass(&system_exit) {
                let code = match &exc {
                    Value::Instance(i) => i.dict.borrow().get_str("code").unwrap_or(Value::None),
                    _ => Value::None,
                };
                let exit_code = match &code {
                    Value::None => 0,
                    Value::Int(i) => i.to_i64().unwrap_or(1),
                    Value::Bool(b) => *b as i64,
                    other => {
                        let text = format::safe_str(vm, other);
                        vm.host.write(&alloc::format!("{}\n", text), true);
                        1
                    }
                };
                return Outcome { exit_code, halted: None };
            }
            let text = format_exception(vm, &exc);
            vm.host.write(&text, true);
            Outcome { exit_code: 1, halted: None }
        }
    }
}

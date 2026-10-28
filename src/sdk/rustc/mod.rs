//! 9.5.1 -- Rust: the interpreter and the native compiler (spec 13.2).
//!
//! ## What this is, stated plainly
//!
//! It is a compiler for a **defined subset of Rust**, written entirely
//! here, that produces x86_64 machine code running in ring 3 on this
//! system -- plus an interpreter over the same tree, which is the other
//! column 13.2 gives the language.
//!
//! It is not `rustc`. `rustc` is a borrow checker, a trait solver, a
//! monomorphiser and LLVM; the repository rule forbids the dependency and
//! the size forbids the copy. What was possible was to take the part of
//! the language a systems program actually uses -- functions, integers,
//! booleans, string literals, control flow -- and compile it for real,
//! then refuse everything else **by name**. `struct` does not produce
//! "unexpected token"; it produces "struct is not in the Nova OS Rust
//! subset - data is passed as parameters, one scalar at a time".
//!
//! That discipline is the same one 9.5 already applies to a missing
//! runtime: a `dev build --lang java` says which piece is absent instead
//! of handing back a binary of `nop`.
//!
//! ## The subset
//!
//! | | |
//! |---|---|
//! | Types | `i64`, `u64`, `bool`, `&str`, `()` |
//! | Items | `fn`, `const` |
//! | Statements | `let`, `let mut`, assignment and `op=`, `if` / `else if` / `else`, `while`, `loop`, `break`, `continue`, `return` |
//! | Expressions | literals, names, `- !`, the eighteen binary operators, calls, `as`, blocks and `if` as values |
//! | Macros | `print!`, `println!` with `{}` holes |
//! | Builtins | `exit`, `sleep_ms`, `yield_now`, `pid` |
//!
//! What is out, and refused with its own message: `struct`, `enum`,
//! `trait`, `impl`, `match`, `for`, closures, generics, references other
//! than `&str`, methods, indexing, tuples, `String`, `Vec`, `Option`,
//! `Result`, `?`, `unsafe`, `async`, `use`, modules.
//!
//! ## Two places where it differs from `rustc`, and both are deliberate
//!
//! * **Arithmetic wraps.** Rust panics on overflow in a debug build and
//!   wraps in a release build; x86 does neither on its own, and emitting
//!   a check after every addition would double the code. The compiled
//!   form and the interpreter both wrap, so the two always agree -- and
//!   agreeing is what makes the interpreter usable as the oracle the
//!   compiler is tested against.
//! * **A shift count is masked to six bits**, because that is what the
//!   processor does. Rust would panic.
//!
//! Both are stated rather than hidden, and both are the *release*
//! behaviour of Rust rather than an invention.
//!
//! ## The pipeline
//!
//! ```text
//!   main.rs
//!     -> lex      tokens, with their lines
//!     -> parse    ast::Program, or a named refusal
//!     -> check    types, mutability, arity, definite return, stack slots
//!     |
//!     +-> codegen  assembly text -> sdk::asm -> sdk::emit -> .cus ring 3
//!     +-> interp   tree walk -> values and output (dev interpret)
//! ```
//!
//! The compiler emits **text** and hands it to `sdk::asm`, exactly as
//! `rustc` hands its output to an assembler. The alternative was a second
//! instruction encoder in the tree, and two encoders drift silently.

pub mod ast;
pub mod check;
pub mod codegen;
pub mod interp;
pub mod lex;
pub mod parse;

use alloc::string::String;
use alloc::vec::Vec;

use ast::Type;

/// Extension the SDK associates with this front-end.
pub const EXTENSION: &str = "rs";

/// The builtins, as `check`, `codegen` and `interp` all read them.
///
/// One table, three consumers. Three lists would agree today and diverge
/// at the first addition, and the divergence would show as "the compiler
/// knows a function the interpreter does not".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    Exit,
    SleepMs,
    Yield,
    Pid,
}

pub struct BuiltinSpec {
    pub name: &'static str,
    pub params: &'static [Type],
    pub result: Type,
    pub kind: Builtin,
    /// The system call behind it, for the state dump.
    pub call: &'static str,
}

pub static BUILTINS: &[BuiltinSpec] = &[
    BuiltinSpec {
        name: "exit",
        params: &[Type::I64],
        result: Type::Unit,
        kind: Builtin::Exit,
        call: "SYS_PROC_EXIT",
    },
    BuiltinSpec {
        name: "sleep_ms",
        params: &[Type::U64],
        result: Type::Unit,
        kind: Builtin::SleepMs,
        call: "SYS_TIME_SLEEP_MS",
    },
    BuiltinSpec {
        name: "yield_now",
        params: &[],
        result: Type::Unit,
        kind: Builtin::Yield,
        call: "SYS_PROC_YIELD",
    },
    BuiltinSpec {
        name: "pid",
        params: &[],
        result: Type::U64,
        kind: Builtin::Pid,
        call: "SYS_PROC_SELF",
    },
];

/// Anything that can stop a compilation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RustError {
    Lex(lex::LexError),
    Parse(parse::ParseError),
    Check(check::CheckError),
    /// The generated assembly did not assemble. Reaching this is a bug in
    /// `codegen`, not in the program being compiled, and the message says
    /// so -- otherwise the user hunts for a mistake in their own source.
    Backend(String),
}

impl RustError {
    pub fn message(&self) -> String {
        match self {
            RustError::Lex(error) => error.message(),
            RustError::Parse(error) => error.message(),
            RustError::Check(error) => error.message(),
            RustError::Backend(detail) => alloc::format!(
                "defaut interne du generateur de code Nova Rust : {} - le programme source n'est pas en cause",
                detail
            ),
        }
    }
}

/// A compilation that succeeded.
pub struct Compiled {
    pub object: super::asm::Object,
    /// The assembly the compiler produced, kept for inspection.
    pub assembly: String,
    pub functions: usize,
    pub constants: usize,
}

/// Front to back: source in, linkable object out.
pub fn compile(source: &str) -> Result<Compiled, RustError> {
    let program = parse::program(source)?;
    let checked = check::check(&program).map_err(RustError::Check)?;
    let assembly = codegen::emit(&program, &checked);

    let object = super::asm::assemble(&assembly)
        .map_err(|error| RustError::Backend(error.message()))?;

    Ok(Compiled {
        functions: program.functions.len(),
        constants: program.constants.len(),
        object,
        assembly,
    })
}

/// The result of interpreting a source.
pub struct Interpreted {
    pub output: Vec<String>,
    pub steps: u64,
    pub exit_code: Option<i64>,
}

/// Runs a source without compiling it -- the interpreter column of 13.2.
pub fn interpret(source: &str, budget: u64) -> Result<Interpreted, RustError> {
    let program = parse::program(source)?;
    let checked = check::check(&program).map_err(RustError::Check)?;

    let (interpreter, outcome) = interp::run(&program, &checked, budget);
    let steps = interpreter.steps_used();
    let mut output = interpreter.output;
    let exit_code = interpreter.exit_code;

    if let Err(error) = outcome {
        // A run that stops has still produced what it produced. Losing
        // the output would hide the last thing the program managed to
        // say, which is usually the thing that explains the stop.
        output.push(alloc::format!("[rustc] arret : {}", error.message()));
    }

    Ok(Interpreted {
        output,
        steps,
        exit_code,
    })
}

/// Compiles and returns only the assembly, for inspection and for tests.
pub fn to_assembly(source: &str) -> Result<String, RustError> {
    let program = parse::program(source)?;
    let checked = check::check(&program).map_err(RustError::Check)?;
    Ok(codegen::emit(&program, &checked))
}

/// The subset, as a table for the state dump.
pub fn describe() -> Vec<(&'static str, &'static str)> {
    alloc::vec![
        ("types", "i64, u64, bool, &str, ()"),
        ("items", "fn, const"),
        ("controle", "if / else if / else, while, loop, break, continue, return"),
        ("expressions", "18 operateurs binaires, - et !, as, appels, blocs valeur"),
        ("macros", "print!, println! avec trous {}"),
        ("appels", "exit, sleep_ms, yield_now, pid"),
        ("refuse", "struct, enum, trait, impl, match, for, fermetures, generiques"),
    ]
}

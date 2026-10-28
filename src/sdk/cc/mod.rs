//! 16.6 -- C: the compiler (spec 13.2, "C / C++" row).
//!
//! ## What this is
//!
//! A compiler for **C11 minus a stated list**, written here, producing
//! x86_64 code that runs in ring 3 as a Nova binary, with its own C
//! library. 13.2 names LLVM for this row; LLVM is millions of lines of
//! C++ and the repository forbids the dependency. What a kernel with no
//! dependency can hold is a compiler of the size of chibicc -- lexer,
//! preprocessor, parser with types, generator -- and that is what this
//! is, written in Rust over the assembler of 9.5.7.
//!
//! ## The pipeline
//!
//! ```text
//!   main.c, util.c (+ the C library, lib/libc.c)
//!     -> lex      tokens with their spelling            lex.rs
//!     -> pp       #include, #define, #if                pp.rs
//!     -> parse    typed tree, one per translation unit  parse/
//!     -> codegen  assembly text of the whole program    codegen/
//!     -> sdk::asm the bytes, then sdk::emit a .cus      (as for Rust)
//! ```
//!
//! ## The subset
//!
//! In: every statement of C11, every operator, `char` to `long long`
//! signed and unsigned, `_Bool`, `float`, `double`, pointers, arrays,
//! `struct`, `union`, `enum`, `typedef`, function pointers, variadic
//! functions, `static` and `extern`, initializers with designators,
//! compound literals, `sizeof`, `_Alignof`, `_Static_assert`, the whole
//! preprocessor, several `.c` files linked together.
//!
//! Out, each refused by name: bit-fields, variable-length arrays,
//! `long double`, `_Complex`, `_Generic`, `_Atomic`, wide characters and
//! strings, statement expressions, inline assembly, computed `goto`,
//! K&R function definitions -- and **C++**, which the row of 13.2 also
//! names: it is a second language the size of ten C compilers, and a
//! `.cpp` file is told so rather than half compiled.
//!
//! ## The C library
//!
//! `lib/` holds the headers (`stdio.h`, `stdlib.h`, `string.h`, `math.h`,
//! ...) and `libc.c`, compiled with every program as a translation unit
//! of its own; the generator keeps only the functions the program
//! reaches. Its lowest layer is the only part that differs between Nova
//! (`__nova__`: `SYS_DEBUG_PRINT`, `SYS_MEM_ALLOC`, `SYS_PROC_EXIT`) and
//! the Linux build of the host test bench (`__linux__`), so the same C
//! library is tested against gcc's output on the host and runs in Nova.

pub mod ast;
pub mod codegen;
pub mod lex;
pub mod lib;
pub mod parse;
pub mod pp;
pub mod ty;

use alloc::string::String;
use alloc::vec::Vec;

use lex::Loc;

/// A compilation error, with its place when it has one.
#[derive(Debug, Clone)]
pub struct CcError {
    pub loc: Option<Loc>,
    /// File name, filled once the error leaves its translation unit.
    pub file: Option<String>,
    pub message: String,
}

impl CcError {
    pub fn at(loc: Loc, message: &str) -> CcError {
        CcError { loc: Some(loc), file: None, message: String::from(message) }
    }

    pub fn plain(message: &str) -> CcError {
        CcError { loc: None, file: None, message: String::from(message) }
    }

    /// `file:line: message`, as a C compiler says it.
    pub fn render(&self) -> String {
        match (&self.file, self.loc) {
            (Some(file), Some(loc)) => alloc::format!("{}:{}: {}", file, loc.line, self.message),
            _ => self.message.clone(),
        }
    }
}

/// The system the program is built for: decides the lowest layer of the
/// C library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Nova,
    /// The host test bench (`tools/cchost`).
    Linux,
}

/// What a compilation produces.
pub struct Compiled {
    pub assembly: String,
    pub object: super::asm::Object,
}

/// Headers of C++, to refuse a C++ program by name.
const CPP_HEADERS: &[&str] = &[
    "iostream", "vector", "string", "map", "cstdio", "cstdlib", "cstring", "algorithm", "memory", "fstream",
    "sstream", "iomanip", "set", "unordered_map", "cmath", "list", "queue", "stack", "utility", "functional",
];

/// Has this source the look of C++?
fn looks_like_cpp(source: &str) -> bool {
    source.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("class ")
            || line.starts_with("namespace ")
            || line.starts_with("template<")
            || line.starts_with("template <")
            || line.starts_with("using namespace")
            || CPP_HEADERS.iter().any(|h| line.starts_with("#include") && line.contains(&alloc::format!("<{}>", h)))
    })
}

const CPP_REFUSAL: &str = "C++ n'est pas pris en charge : le compilateur de Nova compile du C (C11), pas du C++";

/// Compiles the C files `files` (paths read through `sources`) with the
/// C library into an assembled object.
pub fn compile(sources: &dyn pp::Sources, files: &[&str], target: Target) -> Result<Compiled, CcError> {
    let assembly = compile_to_assembly(sources, files, target)?;
    let object = super::asm::assemble(&assembly)
        .map_err(|e| CcError::plain(&alloc::format!("assemblage du code produit : {}", e.message())))?;
    Ok(Compiled { assembly, object })
}

pub fn compile_to_assembly(sources: &dyn pp::Sources, files: &[&str], target: Target) -> Result<String, CcError> {
    for file in files {
        let lower = file.to_ascii_lowercase();
        if [".cpp", ".cc", ".cxx", ".hpp", ".c++"].iter().any(|e| lower.ends_with(e)) {
            return Err(CcError::plain(CPP_REFUSAL));
        }
    }
    let mut predefined: Vec<(&str, &str)> = alloc::vec![
        ("__STDC__", "1"),
        ("__STDC_VERSION__", "201112L"),
        ("__STDC_HOSTED__", "1"),
        ("__x86_64__", "1"),
        ("__x86_64", "1"),
        ("__LP64__", "1"),
        ("_LP64", "1"),
        ("__CHAR_BIT__", "8"),
        ("__SIZEOF_INT__", "4"),
        ("__SIZEOF_LONG__", "8"),
        ("__SIZEOF_POINTER__", "8"),
        ("__nova_cc__", "1"),
    ];
    predefined.push(match target {
        Target::Nova => ("__nova__", "1"),
        Target::Linux => ("__linux__", "1"),
    });

    let mut parser = parse::Parser::new();
    let mut units: Vec<(String, String)> = Vec::new();
    units.push((String::from("<libc.c>"), String::from(lib::LIBC)));
    units.push((String::from("<libm.c>"), String::from(lib::LIBM)));
    for file in files {
        let Some(source) = sources.read(file) else {
            return Err(CcError::plain(&alloc::format!("{} : fichier introuvable", file)));
        };
        if looks_like_cpp(&source) {
            return Err(CcError::plain(CPP_REFUSAL));
        }
        units.push((String::from(*file), source));
    }
    for (path, source) in units.iter() {
        let mut pp = pp::Preprocessor::new(sources, &predefined);
        let named = |mut error: CcError, names: &[String]| {
            if let Some(loc) = error.loc {
                error.file = names.get(loc.file as usize).cloned();
            }
            error
        };
        let tokens = match pp.run(path, source) {
            Ok(tokens) => tokens,
            Err(error) => {
                let refused = error.message.contains("absent de la bibliotheque")
                    && CPP_HEADERS.iter().any(|h| error.message.contains(&alloc::format!("<{}>", h)));
                if refused {
                    return Err(CcError::plain(CPP_REFUSAL));
                }
                return Err(named(error, &pp.names));
            }
        };
        if let Err(error) = parser.unit(tokens) {
            return Err(named(error, &pp.names));
        }
    }
    codegen::generate(&parser.program)
}

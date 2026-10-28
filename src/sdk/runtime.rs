//! 9.5 -- Multi-language runtimes (spec 13.2 and 13.3).
//!
//! ## What this module claims, and what it does not
//!
//! 13.2 lines up eight languages with, for each, an interpreter and a
//! compiler. Seven of those eight lines assume a toolchain weighing
//! several hundred megabytes -- LLVM, Mono, a JVM, V8. None of that can
//! live in a kernel with no external dependency, and pretending
//! otherwise would produce a `dev build --lang rust` that hands back a
//! binary of `nop` while announcing a compilation.
//!
//! The table below is therefore **a data table doubled with a state of
//! completion**. It transcribes 13.2 line by line -- which is what the
//! self-test checks -- and each line additionally carries a `Support`
//! saying what the SDK can do today:
//!
//! * `InTree` -- the toolchain is written here, from source to bytes;
//! * `Interpreted` -- the SDK runs, it does not compile;
//! * `Delegated` -- the SDK knows how to resolve the runtime and refuses
//!   cleanly when it is missing. That covers the seven lines demanding an
//!   external toolchain.
//!
//! ## The ninth line
//!
//! `Language::Script` is not in 13.2. It is a deliberate addition, of the
//! same kind as the `NETWORK` flag added to the binary format in phase
//! 5.6.4: 15.2 shows `dev interpret main.py`, so the SDK must have an
//! interpreter, and writing a complete Python interpreter is not an
//! option. Nova Script stands in its place -- it is written entirely in
//! `sdk::script`, and it is flagged as an extension so that nobody takes
//! it for a line of the specification.
//!
//! ## Locations (13.3, step 9.5.8)
//!
//! Two roots, `/sys/runtimes/` and `/users/{user}/runtimes/`, and one
//! arbitration rule: the user's runtime wins. That is not a matter of
//! taste -- it is what lets a developer try an alternative version
//! without touching the system, hence without ADMIN rights, hence without
//! breaking the other accounts.

use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::header::BinaryFormat;
use crate::fs::perm::{Credentials, Mode};

/// Root of the system runtimes (13.3).
pub const SYSTEM_DIR: &str = "/sys/runtimes";

/// Name of the descriptor dropped in a runtime directory.
pub const DESCRIPTOR: &str = "runtime.cfg";

// ============================================================
//  Languages
// ============================================================

/// The eight languages of 13.2, plus the acknowledged addition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    C,
    CSharp,
    Java,
    Python,
    Node,
    Bun,
    Asm,
    /// Outside 13.2 -- see the module header.
    Script,
}

impl Language {
    /// Short name, the one a `dev.toml` writes and a `--lang` accepts.
    pub fn key(&self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::C => "c",
            Language::CSharp => "csharp",
            Language::Java => "java",
            Language::Python => "python",
            Language::Node => "node",
            Language::Bun => "bun",
            Language::Asm => "asm",
            Language::Script => "script",
        }
    }

    pub fn from_key(key: &str) -> Option<Language> {
        Some(match key {
            "rust" => Language::Rust,
            "c" | "cpp" | "c++" => Language::C,
            "csharp" | "cs" | "c#" => Language::CSharp,
            "java" => Language::Java,
            "python" | "py" => Language::Python,
            "node" | "nodejs" | "js" => Language::Node,
            "bun" | "ts" => Language::Bun,
            "asm" | "x86_64" => Language::Asm,
            "script" | "ns" => Language::Script,
            _ => return None,
        })
    }

    /// Derives the language from a source file extension.
    ///
    /// Used by `dev interpret`, whose 15.2 form gives the file as an
    /// argument without repeating the language.
    pub fn from_extension(path: &str) -> Option<Language> {
        let extension = path.rsplit('.').next()?;
        Some(match extension {
            "rs" => Language::Rust,
            "c" | "h" | "cc" | "cpp" | "hpp" => Language::C,
            "cs" => Language::CSharp,
            "java" => Language::Java,
            "py" => Language::Python,
            "js" | "mjs" => Language::Node,
            "ts" => Language::Bun,
            "asm" | "s" => Language::Asm,
            "ns" => Language::Script,
            _ => return None,
        })
    }
}

// ============================================================
//  State of completion
// ============================================================

/// What the SDK can really do with a language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// Source -> bytes, written here, with nothing external.
    InTree,
    /// Runs without compiling.
    Interpreted,
    /// Delegated to an installed runtime. Absent: explicit refusal.
    Delegated,
}

impl Support {
    pub fn as_str(&self) -> &'static str {
        match self {
            Support::InTree => "chaine complete",
            Support::Interpreted => "interprete seul",
            Support::Delegated => "runtime externe",
        }
    }

    /// Can the SDK produce an executable binary for this language?
    pub fn can_compile(&self) -> bool {
        matches!(self, Support::InTree)
    }

    /// Can the SDK run a source without compiling it?
    pub fn can_interpret(&self) -> bool {
        matches!(self, Support::InTree | Support::Interpreted)
    }
}

// ============================================================
//  The 13.2 table
// ============================================================

/// One row of the 13.2 table, augmented with its state.
pub struct RuntimeSpec {
    pub language: Language,
    /// "Language" column label, to the letter.
    pub label: &'static str,
    /// "Interpreter" column.
    pub interpreter: &'static str,
    /// "Compiler" column.
    pub compiler: &'static str,
    /// "Target formats" column.
    pub targets: &'static [BinaryFormat],
    /// Formats from the target column that additionally demand a SYSTEM
    /// signature. 13.2 notes it only for the assembler.
    pub system_required: &'static [BinaryFormat],
    pub extensions: &'static [&'static str],
    pub support: Support,
    /// Name of the directory expected under `/sys/runtimes/` or
    /// `/users/{user}/runtimes/`.
    pub directory: &'static str,
}

const ALL_FORMATS: &[BinaryFormat] = &[
    BinaryFormat::Dsa,
    BinaryFormat::Ss,
    BinaryFormat::Gs,
    BinaryFormat::Cus,
    BinaryFormat::Ras,
];

const FROM_SS: &[BinaryFormat] = &[
    BinaryFormat::Ss,
    BinaryFormat::Gs,
    BinaryFormat::Cus,
    BinaryFormat::Ras,
];

const FROM_GS: &[BinaryFormat] = &[BinaryFormat::Gs, BinaryFormat::Cus, BinaryFormat::Ras];

const APP_ONLY: &[BinaryFormat] = &[BinaryFormat::Cus, BinaryFormat::Ras];

const NONE_FORMATS: &[BinaryFormat] = &[];

/// Transcription of the 13.2 table, in its order.
///
/// The order matters: the self-test compares row by row, and a reordered
/// table can no longer be read back against the specification.
pub static TABLE: &[RuntimeSpec] = &[
    RuntimeSpec {
        language: Language::Rust,
        label: "Rust",
        interpreter: "REPL interactif",
        compiler: "Natif direct",
        targets: ALL_FORMATS,
        system_required: NONE_FORMATS,
        extensions: &["rs"],
        // 9.5.1 -- `sdk::rustc` compiles a defined subset of the language
        // to x86_64 and runs it in ring 3, and interprets the same tree.
        // Both columns of 13.2 are therefore answered here, with no
        // external toolchain. The subset is stated in that module's
        // header and every construct outside it is refused by name.
        support: Support::InTree,
        directory: "rust",
    },
    RuntimeSpec {
        language: Language::C,
        label: "C / C++",
        interpreter: "LLVM JIT",
        compiler: "LLVM -> natif",
        // 13.2 enumerates the five formats for C/C++ where it writes
        // "All" for Rust. Same set, different wording: the table
        // reproduces it as-is rather than harmonising.
        targets: ALL_FORMATS,
        system_required: NONE_FORMATS,
        extensions: &["c", "cc", "cpp", "h", "hpp"],
        support: Support::Delegated,
        directory: "llvm",
    },
    RuntimeSpec {
        language: Language::CSharp,
        label: "C#",
        interpreter: "Runtime Mono allege",
        compiler: "Transpilation -> Rust",
        targets: FROM_SS,
        system_required: NONE_FORMATS,
        extensions: &["cs"],
        support: Support::Delegated,
        directory: "mono",
    },
    RuntimeSpec {
        language: Language::Java,
        label: "Java",
        interpreter: "JVM maison minimale",
        compiler: "Bytecode -> Rust",
        targets: FROM_GS,
        system_required: NONE_FORMATS,
        extensions: &["java", "class"],
        support: Support::Delegated,
        directory: "jvm",
    },
    RuntimeSpec {
        language: Language::Python,
        label: "Python",
        interpreter: "CPython adapte",
        compiler: "rustpython -> natif",
        targets: APP_ONLY,
        system_required: NONE_FORMATS,
        extensions: &["py"],
        support: Support::Delegated,
        directory: "python",
    },
    RuntimeSpec {
        language: Language::Node,
        label: "Node.js",
        interpreter: "V8 allege",
        compiler: "JS -> Rust",
        targets: APP_ONLY,
        system_required: NONE_FORMATS,
        extensions: &["js", "mjs"],
        support: Support::Delegated,
        directory: "node",
    },
    RuntimeSpec {
        language: Language::Bun,
        label: "Bun",
        interpreter: "Runtime Bun porte",
        compiler: "TS/JS -> Rust",
        targets: APP_ONLY,
        system_required: NONE_FORMATS,
        extensions: &["ts", "tsx"],
        support: Support::Delegated,
        directory: "bun",
    },
    RuntimeSpec {
        language: Language::Asm,
        label: "ASM x86_64",
        interpreter: "Emulateur sandboxe",
        compiler: "Assemblage direct",
        targets: ALL_FORMATS,
        // 13.2 notes "SYSTEM required for .dsa" on this row alone.
        system_required: &[BinaryFormat::Dsa],
        extensions: &["asm", "s"],
        support: Support::InTree,
        directory: "asm",
    },
    RuntimeSpec {
        language: Language::Script,
        label: "Nova Script",
        interpreter: "Interprete integre",
        compiler: "aucun - interprete seul",
        targets: NONE_FORMATS,
        system_required: NONE_FORMATS,
        extensions: &["ns"],
        support: Support::Interpreted,
        directory: "script",
    },
];

/// Table row for a given language.
pub fn spec(language: Language) -> &'static RuntimeSpec {
    // The table covers the nine variants: the search always succeeds.
    // `unwrap_or` on the first row avoids a `panic!` in the kernel for a
    // case the type makes impossible.
    TABLE
        .iter()
        .find(|entry| entry.language == language)
        .unwrap_or(&TABLE[0])
}

/// Can a language produce this format? ("target formats" column)
pub fn targets(language: Language, format: BinaryFormat) -> bool {
    spec(language).targets.contains(&format)
}

/// Does this language/format pair demand a SYSTEM signature?
pub fn requires_system_signature(language: Language, format: BinaryFormat) -> bool {
    spec(language).system_required.contains(&format)
}

// ============================================================
//  9.5.8 -- Locations and resolution
// ============================================================

/// Where a resolved runtime came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    /// `/sys/runtimes/` -- global.
    System = 0,
    /// `/users/{user}/runtimes/` -- wins (13.3).
    User = 1,
}

impl Origin {
    pub fn as_str(&self) -> &'static str {
        match self {
            Origin::System => "systeme",
            Origin::User => "utilisateur",
        }
    }
}

/// A runtime actually present on the volume.
#[derive(Debug, Clone)]
pub struct Installed {
    pub language: Language,
    pub origin: Origin,
    pub path: String,
    pub version: String,
}

/// Root of an account's runtimes.
pub fn user_dir(user: &str) -> String {
    alloc::format!("/users/{}/runtimes", user)
}

/// Content of a runtime descriptor.
///
/// Same choice as for the `.spm` manifest: key-value text, because it
/// will be read back by a human the day a runtime refuses to load, and
/// because one more binary decoder would be one more decoder to audit.
/// a auditer.
fn descriptor_text(spec: &RuntimeSpec, version: &str) -> String {
    alloc::format!(
        "# descripteur de runtime - spec 13.3\nlangage={}\nlibelle={}\nversion={}\ninterprete={}\ncompilateur={}\netat={}\n",
        spec.language.key(),
        spec.label,
        version,
        spec.interpreter,
        spec.compiler,
        spec.support.as_str()
    )
}

/// Reads the version declared by a descriptor.
fn read_version(path: &str) -> Option<String> {
    let who = Credentials::kernel();
    let bytes = crate::fs::read_file(path, who).ok()?;
    let text = core::str::from_utf8(&bytes).ok()?;
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=')
            && key.trim() == "version"
        {
            return Some(String::from(value.trim()));
        }
    }
    None
}

/// Looks for a runtime for this language, user account first.
///
/// 9.5.8: the priority is not negotiable -- a personal runtime must mask
/// the system runtime, otherwise installing an alternative version would
/// have no observable effect.
pub fn resolve(language: Language, user: Option<&str>) -> Option<Installed> {
    if !crate::fs::is_ready() {
        return None;
    }
    let entry = spec(language);
    let who = Credentials::kernel();

    if let Some(user) = user {
        let base = alloc::format!("{}/{}", user_dir(user), entry.directory);
        let descriptor = alloc::format!("{}/{}", base, DESCRIPTOR);
        if crate::fs::stat(&descriptor, who).is_ok() {
            return Some(Installed {
                language,
                origin: Origin::User,
                version: read_version(&descriptor).unwrap_or_else(|| String::from("inconnue")),
                path: base,
            });
        }
    }

    let base = alloc::format!("{}/{}", SYSTEM_DIR, entry.directory);
    let descriptor = alloc::format!("{}/{}", base, DESCRIPTOR);
    if crate::fs::stat(&descriptor, who).is_ok() {
        return Some(Installed {
            language,
            origin: Origin::System,
            version: read_version(&descriptor).unwrap_or_else(|| String::from("inconnue")),
            path: base,
        });
    }

    None
}

/// Drops the descriptors of the runtimes the SDK carries itself.
///
/// Only `InTree` and `Interpreted` are written. Dropping a descriptor for
/// `rust` or `python` would suggest an installed runtime and would turn a
/// clear refusal -- "runtime missing" -- into an obscure failure at
/// compile time.
pub fn install_layout(now: u64) -> usize {
    if !crate::fs::is_ready() {
        return 0;
    }
    let who = Credentials::kernel();
    let mut created = 0;

    for entry in TABLE.iter() {
        if entry.support == Support::Delegated {
            continue;
        }
        let base = alloc::format!("{}/{}", SYSTEM_DIR, entry.directory);
        if crate::fs::stat(&base, who).is_err()
            && crate::fs::create_directory(&base, who, Mode::system_dir(), now).is_err()
        {
            continue;
        }
        let descriptor = alloc::format!("{}/{}", base, DESCRIPTOR);
        if crate::fs::stat(&descriptor, who).is_ok() {
            continue;
        }
        let text = descriptor_text(entry, "1.0.0");
        if crate::fs::write_file(&descriptor, text.as_bytes(), who, Mode::system_file(), now).is_ok()
        {
            created += 1;
        }
    }

    created
}

/// List of the runtimes visible to an account, priority applied.
pub fn list(user: Option<&str>) -> Vec<Installed> {
    TABLE
        .iter()
        .filter_map(|entry| resolve(entry.language, user))
        .collect()
}

pub fn dump_state() {
    crate::serial_println!("[sdk] runtimes du tableau 13.2 :");
    for entry in TABLE.iter() {
        let targets: Vec<&str> = entry
            .targets
            .iter()
            .map(|format| format.extension())
            .collect();
        crate::serial_println!(
            "[sdk]   {:<12} {:<22} {:<24} {:<16} cibles [{}]",
            entry.label,
            entry.interpreter,
            entry.compiler,
            entry.support.as_str(),
            targets.join(" ")
        );
    }
}

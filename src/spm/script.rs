//! 8.1.3 -- Pre and post installation scripts (spec 10.1).
//!
//! ## A script is not code, and that is deliberate
//!
//! 10.1 places "pre/post installation scripts" in the package without
//! saying what they contain. Two readings were possible.
//!
//! The first -- a binary run at install time -- is that of the usual
//! package managers, and it is indefensible here: the installer runs
//! under kernel rights, because it writes into `/sys/` and `/apps/`.
//! Having arbitrary code run by that context would offer any unsigned
//! `.ras` the shortest path to ring 0, and 12.4 would have been written
//! for nothing.
//!
//! The second, the one kept: a **declarative instruction set**. Six
//! verbs, no branching, no loop, no expression. What an installation
//! script really does -- create a directory, drop a configuration file,
//! check a dependency -- is written with those six verbs, and nothing
//! else is written in it.
//!
//! ```text
//!   mkdir  <path>                creates a directory, parents included
//!   write  <path> <text>         writes a text file
//!   copy   <source> <dest>       copies a file already laid down
//!   delete <path>                deletes a file or an empty directory
//!   require <path>               fails if the path does not exist
//!   echo   <message>             logs, with no effect on the disk
//! ```
//!
//! ## Confinement is the important point
//!
//! Every path is normalised then compared with the **allowed roots** of
//! the script, which are the installation prefix of the package and
//! nothing more. A `write /sys/gui/modules/compositor_core.mod` is
//! refused, and refused before anything has been written: the check bears
//! on the path requested, not on the result of a write one would undo.
//!
//! The check is textual, which is enough here because `fs::normalize`
//! resolves `.` and `..` before comparison -- a `../..` therefore cannot
//! leave the root by passing itself off as a directory name. The
//! reservation that remains concerns the virtual links of 4.1: a link
//! laid inside the prefix and pointing outside would take a write out of
//! the perimeter. Refusing `link` in a script closes that path, and that
//! is why the verb does not exist.
//!
//! ## What a failure entails
//!
//! A pre-installation script that fails **cancels** the installation:
//! nothing has been laid down yet, there is nothing to undo. A
//! post-installation script that fails does not cancel it -- the files
//! are written and working -- but the package is marked as partially
//! installed, which `spm update` will read back.

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::perm::{Credentials, Mode};

/// Maximum number of instructions per script.
///
/// An installation script exceeding that size no longer describes an
/// installation: it programs. Refusing it is more honest than letting it
/// run for a long time under kernel rights.
pub const MAX_LINES: usize = 64;

/// Maximum size of a file written by `write`.
pub const MAX_WRITE: usize = 4096;

/// The six verbs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Instruction {
    Mkdir(String),
    Write(String, String),
    Copy(String, String),
    Delete(String),
    Require(String),
    Echo(String),
}

impl Instruction {
    pub fn verb(&self) -> &'static str {
        match self {
            Instruction::Mkdir(_) => "mkdir",
            Instruction::Write(_, _) => "write",
            Instruction::Copy(_, _) => "copy",
            Instruction::Delete(_) => "delete",
            Instruction::Require(_) => "require",
            Instruction::Echo(_) => "echo",
        }
    }

    /// Paths the instruction touches, to be confronted with the roots.
    ///
    /// `echo` touches none: it is the only instruction that neither
    /// writes nor reads the filesystem.
    pub fn paths(&self) -> Vec<&str> {
        match self {
            Instruction::Mkdir(path)
            | Instruction::Delete(path)
            | Instruction::Require(path)
            | Instruction::Write(path, _) => alloc::vec![path.as_str()],
            Instruction::Copy(from, to) => alloc::vec![from.as_str(), to.as_str()],
            Instruction::Echo(_) => Vec::new(),
        }
    }
}

/// Reason for a refusal, at parse time as at run time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptError {
    UnknownVerb,
    MissingArgument,
    TooManyLines,
    PayloadTooLarge,
    OutsideSandbox,
    FilesystemError,
    RequirementMissing,
}

impl ScriptError {
    pub fn message(&self) -> &'static str {
        match self {
            ScriptError::UnknownVerb => "verbe inconnu",
            ScriptError::MissingArgument => "argument manquant",
            ScriptError::TooManyLines => "script trop long",
            ScriptError::PayloadTooLarge => "contenu a ecrire trop volumineux",
            ScriptError::OutsideSandbox => "chemin hors du prefixe d'installation",
            ScriptError::FilesystemError => "operation refusee par le systeme de fichiers",
            ScriptError::RequirementMissing => "prerequis absent",
        }
    }
}

/// One refused instruction, with its line number.
///
/// The number matters: "path outside the prefix" without saying where
/// sends the author of the package back to reading their whole script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub line: usize,
    pub verb: &'static str,
    pub error: ScriptError,
}

/// Result of an execution.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub executed: usize,
    pub failures: Vec<Failure>,
    /// Files and directories created, in order. It is that list the
    /// uninstall reads back: a package must not leave behind what its
    /// post-installation script laid down.
    pub created: Vec<String>,
}

impl Outcome {
    pub fn succeeded(&self) -> bool {
        self.failures.is_empty()
    }
}

fn split_two(rest: &str) -> Option<(&str, &str)> {
    let rest = rest.trim();
    match rest.split_once(char::is_whitespace) {
        Some((first, second)) => {
            let second = second.trim();
            if first.is_empty() || second.is_empty() {
                None
            } else {
                Some((first, second))
            }
        }
        None => None,
    }
}

/// Parses a script without running anything.
///
/// Separate from the execution on purpose, as `module::parse` is from
/// `module::authorize`: an offline tool must be able to say of a package
/// what it would do without letting it do it.
pub fn parse(text: &str) -> Result<Vec<Instruction>, (usize, ScriptError)> {
    let mut program = Vec::new();

    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if program.len() >= MAX_LINES {
            return Err((number, ScriptError::TooManyLines));
        }

        let (verb, rest) = match line.split_once(char::is_whitespace) {
            Some((verb, rest)) => (verb, rest.trim()),
            None => (line, ""),
        };

        let instruction = match verb {
            "mkdir" | "delete" | "require" => {
                if rest.is_empty() {
                    return Err((number, ScriptError::MissingArgument));
                }
                match verb {
                    "mkdir" => Instruction::Mkdir(String::from(rest)),
                    "delete" => Instruction::Delete(String::from(rest)),
                    _ => Instruction::Require(String::from(rest)),
                }
            }
            "echo" => {
                if rest.is_empty() {
                    return Err((number, ScriptError::MissingArgument));
                }
                Instruction::Echo(String::from(rest))
            }
            "write" => {
                let (path, content) =
                    split_two(rest).ok_or((number, ScriptError::MissingArgument))?;
                if content.len() > MAX_WRITE {
                    return Err((number, ScriptError::PayloadTooLarge));
                }
                Instruction::Write(String::from(path), String::from(content))
            }
            "copy" => {
                let (from, to) = split_two(rest).ok_or((number, ScriptError::MissingArgument))?;
                Instruction::Copy(String::from(from), String::from(to))
            }
            _ => return Err((number, ScriptError::UnknownVerb)),
        };

        program.push(instruction);
    }

    Ok(program)
}

/// Is a path contained in one of the roots?
///
/// The comparison is made on the **normalised** path: without that,
/// `/apps/monapp/../../sys/kernel` would pass the check by presenting
/// itself as starting with `/apps/monapp`.
///
/// The boundary is the separator, not the textual prefix: `/apps/mon`
/// must not allow `/apps/monautre`.
pub fn within(path: &str, roots: &[String]) -> bool {
    let Ok((letter, rest)) = crate::fs::split_disk(path) else {
        return false;
    };
    let normalized = crate::fs::normalize(rest);

    roots.iter().any(|root| {
        let Ok((root_letter, root_rest)) = crate::fs::split_disk(root) else {
            return false;
        };
        if root_letter != letter {
            return false;
        }
        let root_path = crate::fs::normalize(root_rest);
        if root_path == "/" {
            return true;
        }
        normalized == root_path
            || (normalized.starts_with(&root_path)
                && normalized.as_bytes().get(root_path.len()) == Some(&b'/'))
    })
}

/// Checks that no instruction leaves the roots.
///
/// Runs before any execution: a script whose last line overflows must not
/// have run the previous ones.
pub fn check_confinement(
    program: &[Instruction],
    roots: &[String],
) -> Result<(), Failure> {
    for (index, instruction) in program.iter().enumerate() {
        for path in instruction.paths() {
            if !within(path, roots) {
                return Err(Failure {
                    line: index + 1,
                    verb: instruction.verb(),
                    error: ScriptError::OutsideSandbox,
                });
            }
        }
    }
    Ok(())
}

/// Runs a script confined to the given roots.
///
/// Execution stops at the first failing instruction: an installation
/// script describes a series of steps each of which assumes the previous,
/// and carrying on after a failed `mkdir` would amount to writing into a
/// directory that does not exist.
pub fn execute(text: &str, roots: &[String], now: u64) -> Outcome {
    let mut outcome = Outcome::default();

    let program = match parse(text) {
        Ok(program) => program,
        Err((line, error)) => {
            outcome.failures.push(Failure {
                line,
                verb: "?",
                error,
            });
            return outcome;
        }
    };

    if let Err(failure) = check_confinement(&program, roots) {
        outcome.failures.push(failure);
        return outcome;
    }

    let who = Credentials::kernel();

    for (index, instruction) in program.iter().enumerate() {
        let result = match instruction {
            Instruction::Mkdir(path) => {
                match crate::fs::create_directory(path, who, Mode::system_dir(), now) {
                    Ok(_) => {
                        outcome.created.push(path.clone());
                        Ok(())
                    }
                    Err(_) => Err(ScriptError::FilesystemError),
                }
            }
            Instruction::Write(path, content) => {
                let bytes = content.as_bytes();
                match crate::fs::write_file(path, bytes, who, Mode::system_file(), now) {
                    Ok(_) => {
                        outcome.created.push(path.clone());
                        Ok(())
                    }
                    Err(_) => Err(ScriptError::FilesystemError),
                }
            }
            Instruction::Copy(from, to) => match crate::fs::copy(from, to, who, now) {
                Ok(_) => {
                    outcome.created.push(to.clone());
                    Ok(())
                }
                Err(_) => Err(ScriptError::FilesystemError),
            },
            Instruction::Delete(path) => match crate::fs::delete(path, who, now) {
                Ok(()) => Ok(()),
                Err(_) => Err(ScriptError::FilesystemError),
            },
            Instruction::Require(path) => {
                if crate::fs::stat(path, who).is_ok() {
                    Ok(())
                } else {
                    Err(ScriptError::RequirementMissing)
                }
            }
            Instruction::Echo(message) => {
                crate::serial_println!("[spm-script] {}", message);
                Ok(())
            }
        };

        match result {
            Ok(()) => outcome.executed += 1,
            Err(error) => {
                outcome.failures.push(Failure {
                    line: index + 1,
                    verb: instruction.verb(),
                    error,
                });
                break;
            }
        }
    }

    outcome
}

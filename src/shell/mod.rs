//! The command interpreter of Nova (spec 1.1 "Shell / CLI", 2.2).
//!
//! ## Why it exists
//!
//! Until phase 16, `spm` (10.3) and `dev` (15.2) were command lines that
//! `spm::manager::execute` and `sdk::cli::execute` parsed -- and nobody
//! could type them: Nova had no shell, and the recovery mode of 2.2
//! promised "kernel + fs.dsa + shell CLI" with no shell to offer. This is
//! that shell. It is a library: `terminal.ss` gives it a window, the
//! serial console gives it a line (`serial_task`), and both see the same
//! commands, the same paths, the same rights.
//!
//! ## Rights
//!
//! Every command runs with the credentials of the account the shell was
//! opened for -- the session's in the terminal, ROOT on the serial console
//! of a recovery boot (a machine being repaired has no session). The
//! filesystem checks them as for any program: the shell grants nothing.
//!
//! ## Words
//!
//! A line is cut on blanks; double or single quotes keep blanks inside a
//! word. Commands are French first, with the usual English names as
//! aliases (`ls` and `liste`, `cat` and `lire`): the system speaks French
//! (14.1) and its users type `ls`.

pub mod commands;
pub mod selftest;
pub mod serial;

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::perm::Credentials;

/// What a command produced.
#[derive(Debug, Default)]
pub struct Output {
    pub lines: Vec<String>,
    /// The terminal should forget what it shows (`effacer`).
    pub clear: bool,
    /// The shell should close (`quitter`).
    pub exit: bool,
    /// The command failed (its lines say why).
    pub failed: bool,
}

impl Output {
    pub fn line(text: impl Into<String>) -> Output {
        Output { lines: alloc::vec![text.into()], ..Output::default() }
    }

    pub fn error(text: impl Into<String>) -> Output {
        Output { lines: alloc::vec![text.into()], failed: true, ..Output::default() }
    }

    pub fn lines(lines: Vec<String>) -> Output {
        Output { lines, ..Output::default() }
    }
}

/// One shell: a working directory, an account, a history.
pub struct Shell {
    pub cwd: String,
    pub user: String,
    pub who: Credentials,
    pub history: Vec<String>,
}

/// Longest history kept.
const HISTORY: usize = 100;

impl Shell {
    /// A shell for `user`, starting in its home when it has one.
    pub fn new(user: &str, who: Credentials) -> Shell {
        let home = alloc::format!("/users/{}/home", user);
        let cwd = if crate::fs::stat(&home, who).is_ok() { home } else { String::from("/") };
        Shell { cwd, user: String::from(user), who, history: Vec::new() }
    }

    /// The shell of the session, or ROOT's when there is none.
    pub fn for_session() -> Shell {
        match crate::apps::session_credentials() {
            Some((user, who)) => Shell::new(&user, who),
            None => Shell::new("root", Credentials::kernel()),
        }
    }

    pub fn prompt(&self) -> String {
        alloc::format!("{}:{}$ ", self.user, self.cwd)
    }

    /// An absolute, normalized path for `path` from the working directory.
    /// A disk letter (`A:/...`) is absolute.
    pub fn resolve(&self, path: &str) -> String {
        if path.len() >= 2 && path.as_bytes()[1] == b':' {
            let (disk, rest) = path.split_at(2);
            return alloc::format!("{}{}", disk, normalize(rest));
        }
        if path.starts_with('/') {
            return normalize(path);
        }
        if self.cwd.len() >= 2 && self.cwd.as_bytes()[1] == b':' {
            let (disk, rest) = self.cwd.split_at(2);
            return alloc::format!("{}{}", disk, normalize(&alloc::format!("{}/{}", rest, path)));
        }
        normalize(&alloc::format!("{}/{}", self.cwd, path))
    }

    /// Runs one line.
    pub fn run(&mut self, line: &str) -> Output {
        let line = line.trim();
        if line.is_empty() {
            return Output::default();
        }
        if self.history.last().map(String::as_str) != Some(line) {
            self.history.push(String::from(line));
            if self.history.len() > HISTORY {
                self.history.remove(0);
            }
        }
        let words = match split(line) {
            Ok(words) => words,
            Err(reason) => return Output::error(reason),
        };
        commands::run(self, line, &words)
    }
}

/// Removes `.`, resolves `..` and doubled slashes. Never climbs above `/`.
pub fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    let mut out = String::from("/");
    out.push_str(&parts.join("/"));
    out
}

/// Cuts a line into words; quotes keep blanks inside a word.
pub fn split(line: &str) -> Result<Vec<String>, &'static str> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in line.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => current.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                started = true;
            }
            None if c.is_whitespace() => {
                if started {
                    words.push(core::mem::take(&mut current));
                    started = false;
                }
            }
            None => {
                current.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err("guillemet non ferme");
    }
    if started {
        words.push(current);
    }
    Ok(words)
}

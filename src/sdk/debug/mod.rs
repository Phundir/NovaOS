//! `dev run --debug` (16.4): a debugger for the program of the sandbox.
//!
//! ## A session
//!
//! `dev run --debug` builds and replays the program as `dev run` does,
//! but attaches to it before it first runs and puts a breakpoint on its
//! entry: the program stops there, and the shell hands every line that
//! follows to this module until the session ends (the prompt says
//! `(ndb)`). The kernel side -- traps, registers, memory -- is
//! `crate::debug`; what lives here is what makes it usable: symbols from
//! the assembler's object, the commands, the reports.
//!
//! ## Commands
//!
//! French first, the gdb names as aliases, as the shell does:
//! `continuer` (c), `pas` (s, si), `suivant` (n), `arret` (b),
//! `effacer` (d), `points`, `registres` (r), `memoire` (x), `ecrire`
//! (set), `pile` (bt), `symboles`, `quitter` (q), `aide`.
//!
//! A target is a symbol (`calcule`, `calcule+4`), an address (`0x...`)
//! or, for `memoire`, a register (`rsp`).
//!
//! ## The call stack
//!
//! Read through the frame pointers: the Rust subset's functions all begin
//! with `push rbp; mov rbp, rsp`, so `[rbp]` is the caller's frame and
//! `[rbp + 8]` the return address. Hand-written assembly without frames,
//! or a stop on a function's first instruction, shows a shorter chain:
//! there is no unwinding table to do better from.

use alloc::string::String;
use alloc::vec::Vec;

use crate::debug::{self, Event, Registers, Resume, Stop, REGISTER_NAMES};
use crate::process::Pid;
use crate::vga::SpinLock;

pub mod selftest;

/// Frames `pile` follows at most.
const MAX_FRAMES: usize = 32;
/// Bytes `memoire` shows at most.
const MAX_DUMP: usize = 512;
/// Steps one `pas N` takes at most.
const MAX_STEPS: usize = 10_000;

/// What a session knows of its program.
struct Session {
    pid: Pid,
    sandbox: u32,
    app_id: String,
    /// (name, address), sorted by address.
    symbols: Vec<(String, u64)>,
    /// Extent of `.text` in memory.
    text: (u64, u64),
    /// Breakpoints in the order they were set: their numbers.
    numbered: Vec<u64>,
}

static SESSIONS: SpinLock<Vec<Session>> = SpinLock::new(Vec::new());

/// What `dev run --debug` hands the shell: the program stopped on its
/// entry, and what to tell the user.
#[derive(Debug, Clone)]
pub struct Started {
    pub pid: Pid,
    /// The sandbox kept for the program.
    pub sandbox: u32,
    pub lines: Vec<String>,
}

/// Opens a session on `pid`, attached and stopped by the caller (`dev
/// run --debug`). `symbols` are at their run-time addresses.
pub fn open(pid: Pid, sandbox: u32, app_id: &str, symbols: Vec<(String, u64)>, text: (u64, u64), entry: u64) -> Started {
    let mut symbols = symbols;
    symbols.sort_by_key(|(_, address)| *address);
    SESSIONS.lock().push(Session { pid, sandbox, app_id: String::from(app_id), symbols, text, numbered: Vec::new() });
    let mut lines = alloc::vec![alloc::format!(
        "debogueur : {} (tache {}, bac a sable {}) - \"aide\" liste les commandes",
        app_id,
        pid,
        sandbox
    )];
    match debug::wait(pid) {
        Event::Stopped(stop, registers) => {
            // The entry's breakpoint has done its work: it is not one of
            // the user's.
            let _ = debug::clear_breakpoint(pid, entry);
            lines.extend(describe_stop(pid, stop, &registers));
        }
        Event::Exited(code) => {
            lines.push(alloc::format!("programme termine avant son entree - code {}", code));
            finish(pid);
        }
    }
    Started { pid, sandbox, lines }
}

/// Is a session open on `pid`?
pub fn is_open(pid: Pid) -> bool {
    SESSIONS.lock().iter().any(|s| s.pid == pid)
}

/// Ends the session on `pid`, the program with it if it still runs.
pub fn close(pid: Pid) {
    if debug::is_traced(pid) {
        debug::kill(pid);
    }
    finish(pid);
}

/// The program is over: the sandbox is kept (that is what `--debug`
/// promised before 16.4 already), the session forgotten.
fn finish(pid: Pid) {
    let session = {
        let mut sessions = SESSIONS.lock();
        sessions.iter().position(|s| s.pid == pid).map(|index| sessions.remove(index))
    };
    if let Some(session) = session {
        super::cli::keep_sandbox(session.sandbox);
        crate::serial_println!("[dev] debogage de {} termine - bac a sable {} conserve", session.app_id, session.sandbox);
    }
}

// ------------------------------------------------------------
//  Symbols
// ------------------------------------------------------------

fn with_session<T>(pid: Pid, work: impl FnOnce(&mut Session) -> T) -> Option<T> {
    SESSIONS.lock().iter_mut().find(|s| s.pid == pid).map(work)
}

/// A label inside a function rather than a function: the compiler's
/// (`__L...`) and the assembler's local ones (`boucle.fin`). They name
/// places, not frames, so addresses are told from the function.
fn is_local(name: &str) -> bool {
    name.starts_with("__") || name.contains('.')
}

/// `name+offset` for an address of the program.
fn symbolize(pid: Pid, address: u64) -> String {
    with_session(pid, |session| {
        let found = session.symbols.iter().rev().find(|(name, a)| *a <= address && !is_local(name));
        match found {
            Some((name, start)) if address < session.text.1 || *start == address => {
                if *start == address {
                    alloc::format!("0x{:x} <{}>", address, name)
                } else {
                    alloc::format!("0x{:x} <{}+{}>", address, name, address - start)
                }
            }
            _ => alloc::format!("0x{:x}", address),
        }
    })
    .unwrap_or_else(|| alloc::format!("0x{:x}", address))
}

fn parse_number(text: &str) -> Option<u64> {
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else if let Some(negative) = text.strip_prefix('-') {
        negative.parse::<u64>().ok().map(|n| n.wrapping_neg())
    } else {
        text.parse().ok()
    }
}

/// A target: `0x...`, `symbole`, `symbole+N`, a number, or (when
/// `registers` is given) a register.
fn resolve(pid: Pid, target: &str, registers: Option<&Registers>) -> Result<u64, String> {
    if let Some(value) = parse_number(target) {
        return Ok(value);
    }
    if let Some(value) = registers.and_then(|r| r.get(target)) {
        return Ok(value);
    }
    let (name, offset) = match target.split_once('+') {
        Some((name, offset)) => (name, parse_number(offset).ok_or_else(|| alloc::format!("decalage invalide : {}", offset))?),
        None => (target, 0),
    };
    with_session(pid, |session| session.symbols.iter().find(|(n, _)| n == name).map(|(_, a)| a + offset))
        .flatten()
        .ok_or_else(|| alloc::format!("symbole inconnu : {}", name))
}

// ------------------------------------------------------------
//  Reports
// ------------------------------------------------------------

fn describe_stop(pid: Pid, stop: Stop, registers: &Registers) -> Vec<String> {
    let here = symbolize(pid, registers.rip);
    let line = match stop {
        Stop::Breakpoint(address) => {
            let number = with_session(pid, |s| s.numbered.iter().position(|a| *a == address)).flatten();
            match number {
                Some(n) => alloc::format!("arret au point {} : {}", n + 1, here),
                None => alloc::format!("arret a l'entree : {}", here),
            }
        }
        Stop::Step => alloc::format!("pas : {}", here),
        Stop::Trap(address) => alloc::format!("int3 du programme en {}", symbolize(pid, address)),
    };
    let mut lines = alloc::vec![line];
    if let Ok(bytes) = debug::read_memory(pid, registers.rip, 8) {
        let hex: Vec<String> = bytes.iter().map(|b| alloc::format!("{:02x}", b)).collect();
        lines.push(alloc::format!("  octets : {}", hex.join(" ")));
    }
    lines
}

fn describe_event(pid: Pid, event: Event) -> (Vec<String>, bool) {
    match event {
        Event::Stopped(stop, registers) => (describe_stop(pid, stop, &registers), false),
        Event::Exited(code) => {
            finish(pid);
            (alloc::vec![alloc::format!("programme termine - code de sortie {}", code)], true)
        }
    }
}

fn registers_lines(registers: &Registers) -> Vec<String> {
    let mut lines = Vec::new();
    for pair in REGISTER_NAMES.chunks(2) {
        let cells: Vec<String> = pair
            .iter()
            .map(|name| alloc::format!("{:<6} 0x{:016x}", name, registers.get(name).unwrap_or(0)))
            .collect();
        lines.push(alloc::format!("  {}", cells.join("   ")));
    }
    lines
}

fn dump(pid: Pid, address: u64, length: usize) -> Result<Vec<String>, String> {
    let bytes = debug::read_memory(pid, address, length)?;
    let mut lines = Vec::new();
    for (row, chunk) in bytes.chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| alloc::format!("{:02x}", b)).collect();
        let text: String = chunk.iter().map(|b| if (0x20..0x7f).contains(b) { *b as char } else { '.' }).collect();
        lines.push(alloc::format!("  0x{:x} : {:<47}  {}", address + row as u64 * 16, hex.join(" "), text));
    }
    Ok(lines)
}

fn read_word(pid: Pid, address: u64) -> Option<u64> {
    let bytes = debug::read_memory(pid, address, 8).ok()?;
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}

/// The chain of frames, from the stop up.
fn backtrace(pid: Pid, registers: &Registers) -> Vec<String> {
    let mut lines = alloc::vec![alloc::format!("  #0 {}", symbolize(pid, registers.rip))];
    let mut rbp = registers.rbp;
    let text = with_session(pid, |s| s.text).unwrap_or((0, 0));
    let mut first = 1;
    // On a function's first instruction (or its second, after `push
    // rbp`) the frame is not built yet: the return address is on the
    // stack, and `rbp` is still the caller's frame.
    let start = with_session(pid, |s| {
        s.symbols.iter().rev().find(|(name, a)| *a <= registers.rip && !is_local(name)).map(|(_, a)| *a)
    })
    .flatten();
    let pending = match start {
        Some(start) if registers.rip == start => read_word(pid, registers.rsp),
        Some(start) if registers.rip == start + 1 => read_word(pid, registers.rsp + 8),
        _ => None,
    };
    if let Some(ret) = pending.filter(|ret| *ret >= text.0 && *ret < text.1) {
        lines.push(alloc::format!("  #1 {}", symbolize(pid, ret)));
        first = 2;
    }
    for depth in first..MAX_FRAMES {
        if rbp == 0 || rbp % 8 != 0 {
            break;
        }
        let Ok(words) = debug::read_memory(pid, rbp, 16) else {
            break;
        };
        let next = u64::from_le_bytes(words[0..8].try_into().unwrap_or([0; 8]));
        let ret = u64::from_le_bytes(words[8..16].try_into().unwrap_or([0; 8]));
        if ret < text.0 || ret >= text.1 {
            break;
        }
        lines.push(alloc::format!("  #{} {}", depth, symbolize(pid, ret)));
        // Frames grow towards higher addresses as they are popped.
        if next <= rbp {
            break;
        }
        rbp = next;
    }
    lines
}

// ------------------------------------------------------------
//  Commands
// ------------------------------------------------------------

const HELP: &[&str] = &[
    "continuer (c)          reprend jusqu'au prochain arret ou la fin",
    "pas [n] (s, si)        execute une instruction (ou n)",
    "suivant (n)            comme pas, mais passe par-dessus un call",
    "arret <cible> (b)      pose un point d'arret (symbole, symbole+N, 0x...)",
    "effacer <n|cible> (d)  retire un point d'arret",
    "points                 liste les points d'arret",
    "registres (r)          affiche les registres",
    "memoire <cible> [n] (x)  affiche n octets (registre accepte : rsp)",
    "ecrire <registre> <valeur> (set)  change un registre",
    "pile (bt)              chaine des appels",
    "symboles               liste les symboles du programme",
    "quitter (q)            termine le programme et la session",
];

/// Runs one debugger line on the session of `pid`. Returns the lines to
/// show, and whether the session is over.
pub fn command(pid: Pid, line: &str) -> (Vec<String>, bool) {
    let words: Vec<&str> = line.split_whitespace().collect();
    let Some(first) = words.first() else {
        return (Vec::new(), false);
    };
    if !is_open(pid) {
        return (alloc::vec![String::from("aucune session de debogage")], true);
    }
    let args = &words[1..];
    let stopped = debug::stopped(pid);
    let need_stop = |what: &str| -> Result<(Stop, Registers), String> {
        stopped.ok_or_else(|| alloc::format!("{} : le programme n'est pas arrete", what))
    };
    let result: Result<(Vec<String>, bool), String> = match *first {
        "aide" | "help" | "?" => Ok((HELP.iter().map(|l| alloc::format!("  {}", l)).collect(), false)),
        "continuer" | "c" | "continue" => need_stop("continuer").and_then(|_| {
            debug::resume(pid, Resume::Continue)?;
            Ok(describe_event(pid, debug::wait(pid)))
        }),
        "pas" | "s" | "si" | "step" | "stepi" => need_stop("pas").and_then(|_| {
            let count = args.first().and_then(|n| n.parse::<usize>().ok()).unwrap_or(1).clamp(1, MAX_STEPS);
            let mut last = (Vec::new(), false);
            for _ in 0..count {
                debug::resume(pid, Resume::Step)?;
                last = describe_event(pid, debug::wait(pid));
                if last.1 {
                    break;
                }
                // A breakpoint met on the way stops the count.
                if !matches!(debug::stopped(pid), Some((Stop::Step, _))) {
                    break;
                }
            }
            Ok(last)
        }),
        "suivant" | "n" | "next" | "nexti" => need_stop("suivant").and_then(|(_, registers)| step_over(pid, &registers)),
        "arret" | "b" | "break" => {
            let Some(target) = args.first() else {
                return (alloc::vec![String::from("arret : une cible est attendue")], false);
            };
            resolve(pid, target, None).and_then(|address| {
                debug::set_breakpoint(pid, address)?;
                let number = with_session(pid, |s| {
                    if !s.numbered.contains(&address) {
                        s.numbered.push(address);
                    }
                    s.numbered.iter().position(|a| *a == address).unwrap_or(0) + 1
                })
                .unwrap_or(0);
                Ok((alloc::vec![alloc::format!("point d'arret {} en {}", number, symbolize(pid, address))], false))
            })
        }
        "effacer" | "d" | "delete" => {
            let Some(target) = args.first() else {
                return (alloc::vec![String::from("effacer : un numero ou une cible est attendu")], false);
            };
            let by_number = target
                .parse::<usize>()
                .ok()
                .and_then(|n| with_session(pid, |s| s.numbered.get(n.wrapping_sub(1)).copied()).flatten());
            let address = match by_number {
                Some(address) => Ok(address),
                None => resolve(pid, target, None),
            };
            address.and_then(|address| {
                debug::clear_breakpoint(pid, address)?;
                with_session(pid, |s| s.numbered.retain(|a| *a != address));
                Ok((alloc::vec![alloc::format!("point d'arret retire de {}", symbolize(pid, address))], false))
            })
        }
        "points" | "info" => {
            let numbered = with_session(pid, |s| s.numbered.clone()).unwrap_or_default();
            let lines: Vec<String> = if numbered.is_empty() {
                alloc::vec![String::from("aucun point d'arret")]
            } else {
                numbered.iter().enumerate().map(|(i, a)| alloc::format!("  {} : {}", i + 1, symbolize(pid, *a))).collect()
            };
            Ok((lines, false))
        }
        "registres" | "r" | "regs" | "registers" => need_stop("registres").map(|(_, registers)| (registers_lines(&registers), false)),
        "memoire" | "x" => need_stop("memoire").and_then(|(_, registers)| {
            let Some(target) = args.first() else {
                return Err(String::from("memoire : une cible est attendue"));
            };
            let address = resolve(pid, target, Some(&registers))?;
            let length = args.get(1).and_then(|n| parse_number(n)).unwrap_or(32).clamp(1, MAX_DUMP as u64) as usize;
            Ok((dump(pid, address, length)?, false))
        }),
        "ecrire" | "set" => need_stop("ecrire").and_then(|(_, mut registers)| {
            let (Some(name), Some(value)) = (args.first(), args.get(1)) else {
                return Err(String::from("ecrire : un registre et une valeur sont attendus"));
            };
            let value = parse_number(value).ok_or_else(|| alloc::format!("valeur invalide : {}", value))?;
            if !registers.set(name, value) {
                return Err(alloc::format!("registre inconnu : {}", name));
            }
            debug::set_registers(pid, registers)?;
            Ok((alloc::vec![alloc::format!("{} = 0x{:x}", name, value)], false))
        }),
        "pile" | "bt" | "backtrace" => need_stop("pile").map(|(_, registers)| (backtrace(pid, &registers), false)),
        "symboles" | "symbols" => {
            let symbols = with_session(pid, |s| s.symbols.clone()).unwrap_or_default();
            Ok((symbols.iter().map(|(name, address)| alloc::format!("  0x{:x} {}", address, name)).collect(), false))
        }
        "quitter" | "q" | "quit" | "kill" => {
            close(pid);
            Ok((alloc::vec![String::from("programme termine par le debogueur")], true))
        }
        other => Err(alloc::format!("{} : commande du debogueur inconnue - \"aide\" les liste", other)),
    };
    match result {
        Ok(outcome) => outcome,
        Err(message) => (alloc::vec![message], false),
    }
}

/// `suivant`: over a `call rel32` (0xE8), to the instruction after it;
/// anything else is one step.
fn step_over(pid: Pid, registers: &Registers) -> Result<(Vec<String>, bool), String> {
    let opcode = debug::read_memory(pid, registers.rip, 1)?;
    if opcode.first() != Some(&0xE8) {
        debug::resume(pid, Resume::Step)?;
        return Ok(describe_event(pid, debug::wait(pid)));
    }
    let after = registers.rip + 5;
    let already = debug::breakpoints(pid).contains(&after);
    if !already {
        debug::set_breakpoint(pid, after)?;
    }
    debug::resume(pid, Resume::Continue)?;
    let event = debug::wait(pid);
    if !already && matches!(event, Event::Stopped(..)) {
        let _ = debug::clear_breakpoint(pid, after);
    }
    let (mut lines, ended) = describe_event(pid, event);
    // The temporary breakpoint is not one of the user's.
    if let Some(first) = lines.first_mut() {
        if first.starts_with("arret a l'entree") {
            *first = alloc::format!("suivant : {}", symbolize(pid, after));
        }
    }
    Ok((lines, ended))
}

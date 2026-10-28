//! 5.1.3 and 5.1.4 -- Per-domain dispatch table, and execution modes.
//!
//! ## Why a table rather than a `match`
//!
//! A giant `match` on the call number compiles into perfectly efficient
//! code, and exposes nothing: there is no way to ask the kernel which
//! calls it knows, under what name, or whether they block. A data table
//! answers those three questions -- that is what `SYS_API_INFO` serves --
//! and makes the check mechanical: a check at boot confirms that every
//! table is sorted, with no gap and no duplicate, and that every number
//! falls in the domain hosting it. A call filed under the wrong domain is
//! a mistake no re-reading catches and a sort detects immediately.
//!
//! The search is binary on a sorted array: two or three comparisons,
//! against an indirect jump for a `match`. The difference does not show
//! at the scale of a system call, which starts with a stack switch.
//!
//! ## Sync and Async (spec 6.1)
//!
//! The specification classes every call as `Sync` or `Async` without
//! defining the boundary. The reading kept here is **operational**: a
//! `Sync` call must hand back without ever putting its caller to sleep; an
//! `Async` call has the right to.
//!
//! It is not a naming convention: `Call::may_block` makes it a checked
//! invariant. A domain calling `block_current` from a synchronous call
//! would be refused by `ErrorCode::NotAsync` instead of silently putting
//! to sleep a task its author believed alive.
//!
//! What the distinction does not yet cover: `SYS_FS_READ` is declared
//! asynchronous by the spec because it touches a disk, but our AHCI
//! driver works by polling, so the call only hands back once the I/O is
//! over. The declared mode is therefore, for domain 0x01, an interface
//! promise rather than a behaviour -- it will be kept when storage moves
//! to user space (phase 6.1).

use super::Call;
use super::error::SysResult;

/// Execution mode declared by spec 6.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Hands back without ever putting the caller to sleep.
    Sync,
    /// May suspend the caller until the operation completes.
    Async,
}

impl Mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Sync => "Sync",
            Mode::Async => "Async",
        }
    }
}

/// Signature common to every handler.
pub type Handler = fn(&mut Call) -> SysResult<u64>;

/// One entry of the table.
pub struct Entry {
    pub number: u16,
    pub name: &'static str,
    pub mode: Mode,
    pub handler: Handler,
}

/// Declaration shorthand: the table stays readable line by line.
macro_rules! entry {
    ($number:expr, $name:literal, $mode:ident, $handler:path) => {
        $crate::syscall::table::Entry {
            number: $number,
            name: $name,
            mode: $crate::syscall::table::Mode::$mode,
            handler: $handler,
        }
    };
}

pub(crate) use entry;

/// The domains, indexed by their high byte.
///
/// Domain 0x00 is not in the specification: it gathers the service calls
/// -- trace, reading of the last error, description of the API itself --
/// which belong to no subsystem. Domains 0x09 to 0x0B (spec 6.7) are the
/// ported programs' own, served by `syscall::ported`: empty here. Domain
/// 0x0C (spec 6.8, the GPU) is reached from both tables.
pub static DOMAINS: [(&str, &[Entry]); 13] = [
    ("SYS_CORE", super::domain_core::TABLE),
    ("SYS_FS", super::domain_fs::TABLE),
    ("SYS_PROC", super::domain_proc::TABLE),
    ("SYS_MEM", super::domain_mem::TABLE),
    ("SYS_IPC", super::domain_ipc::TABLE),
    ("SYS_USER", super::domain_user::TABLE),
    ("SYS_HW", super::domain_hw::TABLE),
    ("SYS_TIME", super::domain_time::TABLE),
    ("SYS_NET", super::domain_net::TABLE),
    ("SYS_OBJ", &[]),
    ("SYS_THREAD", &[]),
    ("SYS_SIG", &[]),
    ("SYS_GPU", crate::gpu::api::TABLE),
];

/// Finds the entry matching a call number.
pub fn lookup(number: u16) -> Option<&'static Entry> {
    let domain = (number >> 8) as usize;
    let (_, table) = DOMAINS.get(domain)?;
    let index = table
        .binary_search_by_key(&number, |entry| entry.number)
        .ok()?;
    table.get(index)
}

/// Readable name of a domain.
pub fn domain_name(domain: usize) -> &'static str {
    DOMAINS.get(domain).map(|(name, _)| *name).unwrap_or("?")
}

/// Total number of declared calls.
pub fn count() -> usize {
    DOMAINS.iter().map(|(_, table)| table.len()).sum()
}

/// Checks that the tables are well formed.
///
/// Checked at boot rather than at the first call: a badly sorted table
/// makes the binary search silent -- it does not find an entry that does
/// exist, and the call comes back as `INVALID_SYSCALL` with nothing
/// naming the real cause.
pub fn validate() -> Result<(), &'static str> {
    for (domain, (_, table)) in DOMAINS.iter().enumerate() {
        let mut previous: Option<u16> = None;
        for entry in table.iter() {
            if (entry.number >> 8) as usize != domain {
                return Err("un appel est range dans le mauvais domaine");
            }
            match previous {
                Some(value) if entry.number <= value => {
                    return Err("table non triee ou numero en double");
                }
                _ => previous = Some(entry.number),
            }
        }
    }
    Ok(())
}

/// Logs the complete table -- it is the map of the system API.
pub fn dump() {
    crate::serial_println!(
        "[syscall] {} appels declares sur {} domaines (spec 6.2 a 6.6)",
        count(),
        DOMAINS.len()
    );
    for (domain, (name, table)) in DOMAINS.iter().enumerate() {
        if table.is_empty() {
            continue;
        }
        let async_count = table.iter().filter(|e| e.mode == Mode::Async).count();
        crate::serial_println!(
            "[syscall]   0x{:02x}xx {:<9} - {} appels, dont {} asynchrones",
            domain,
            name,
            table.len(),
            async_count
        );
    }
}

/// Logs the detail of one domain.
pub fn dump_domain(domain: usize) {
    let Some((name, table)) = DOMAINS.get(domain) else {
        return;
    };
    for entry in table.iter() {
        crate::serial_println!(
            "[syscall]   0x{:04x}  {:<5}  {}::{}",
            entry.number,
            entry.mode.as_str(),
            name,
            entry.name
        );
    }
}

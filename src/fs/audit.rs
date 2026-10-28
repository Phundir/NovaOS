//! 4.5.4 -- Access audit log.
//!
//! Every permission check leaves a trace here: who, on what, which
//! permission, granted or refused. It is not a debug log -- it is what
//! lets an administrator establish that an application tried to open a
//! file it had no business touching.
//!
//! **Readable by ADMIN and ROOT only** (spec 5.1). The restriction is
//! applied on reading, not on writing: everybody produces entries, nobody
//! else reads them back.
//!
//! The log lives in memory, in a fixed-size circular buffer. So it does
//! not survive a reboot. Persisting it to `/logs/kernel/` would mean
//! writing into the filesystem from the permission checker of that same
//! filesystem: the code path is reentrant, and handling it properly
//! demands a deferred queue. That is what `SYS_FS_AUDIT` will do (step
//! 5.2.3).

use alloc::string::String;
use alloc::vec::Vec;

use super::perm::{Credentials, Role, Uid, permission_name};
use crate::vga::SpinLock;

/// Entries kept before the oldest are overwritten.
pub const CAPACITY: usize = 128;

#[derive(Debug, Clone)]
pub struct Entry {
    /// Timer tick at the moment of the access.
    pub when: u64,
    pub uid: Uid,
    pub role: Role,
    pub path: String,
    pub permission: u64,
    pub granted: bool,
}

struct Journal {
    entries: Vec<Entry>,
    /// Write position in the circular buffer.
    cursor: usize,
    /// Total accesses observed, including those already overwritten.
    total: u64,
    denied: u64,
    /// Is the log active? Switched off while the tree is being built,
    /// which would produce thousands of uninteresting entries.
    enabled: bool,
}

impl Journal {
    const fn new() -> Self {
        Journal {
            entries: Vec::new(),
            cursor: 0,
            total: 0,
            denied: 0,
            enabled: false,
        }
    }
}

static JOURNAL: SpinLock<Journal> = SpinLock::new(Journal::new());

/// Enables or switches off the logging.
pub fn set_enabled(enabled: bool) {
    JOURNAL.lock().enabled = enabled;
}

/// Records an access.
pub fn record(who: Credentials, path: &str, permission: u64, granted: bool) {
    let mut journal = JOURNAL.lock();
    if !journal.enabled {
        return;
    }

    journal.total += 1;
    if !granted {
        journal.denied += 1;
    }

    let entry = Entry {
        when: crate::idt::ticks(),
        uid: who.uid,
        role: who.role,
        path: String::from(path),
        permission,
        granted,
    };

    if journal.entries.len() < CAPACITY {
        journal.entries.push(entry);
        journal.cursor = journal.entries.len() % CAPACITY;
    } else {
        let cursor = journal.cursor;
        journal.entries[cursor] = entry;
        journal.cursor = (cursor + 1) % CAPACITY;
    }
}

/// Cause of a read refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotPermitted;

/// Reads the log. Reserved to ADMIN and ROOT (spec 5.1).
pub fn read(who: Credentials) -> Result<Vec<Entry>, NotPermitted> {
    if !who.role.may_audit() {
        // The refusal is itself recorded: an attempt to read the log by
        // an unauthorised account is exactly the kind of event an audit
        // log must retain.
        record(who, "<journal d'audit>", super::perm::Permissions::AUDIT, false);
        return Err(NotPermitted);
    }

    let journal = JOURNAL.lock();
    Ok(journal.entries.clone())
}

/// (accesses observed, refusals).
pub fn stats() -> (u64, u64) {
    let journal = JOURNAL.lock();
    (journal.total, journal.denied)
}

/// Logs the last refusals on the serial console.
pub fn dump_denials(who: Credentials, limit: usize) {
    let Ok(entries) = read(who) else {
        crate::serial_println!(
            "[audit] consultation refusee - role {} (ADMIN requis, spec 5.1)",
            who.role.as_str()
        );
        return;
    };

    let (total, denied) = stats();
    crate::serial_println!("[audit] {} acces observes, {} refus", total, denied);

    let mut shown = 0;
    for entry in entries.iter().rev() {
        if entry.granted {
            continue;
        }
        crate::serial_println!(
            "[audit]   tic {} - uid {} ({}) - {} refuse sur \"{}\"",
            entry.when,
            entry.uid,
            entry.role.as_str(),
            permission_name(entry.permission),
            entry.path
        );
        shown += 1;
        if shown >= limit {
            break;
        }
    }

    if shown == 0 {
        crate::serial_println!("[audit]   aucun refus enregistre");
    }
}

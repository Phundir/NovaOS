//! 5.6.1 -- Domain 0x05, accounts and permissions (spec 6.6).
//!
//! ## The point this domain closes
//!
//! The whole of phase 4 was written receiving a `Credentials` from its
//! caller: the thirteen permissions of 5 were applied to the letter, but
//! nothing checked that whoever presented themselves as ROOT was. The
//! roadmap carried it as an open limit, to be lifted here.
//!
//! It is lifted by a single principle: **an identity is read, it is not
//! declared**. Every task carries an account, inherited from its creator
//! at launch, and `Call::who` goes to fetch it from the scheduler. No
//! call of this domain or of another lets a task give itself an identity
//! it does not have.
//!
//! ## Go down, never up
//!
//! `SETID` exists so that a privileged service can give up its rights
//! before doing work on behalf of a user -- it is the classic pattern,
//! and the only legitimate use of an identity change with no password.
//! Going up is refused without exception: it would assume a proof --
//! password, token -- that nothing can produce yet. The day an
//! authentication exists, it will take the form of a distinct call, and
//! `SETID` will keep its meaning.
//!
//! ## What is still missing
//!
//! No secret is stored: the accounts have no password, hence no login in
//! the usual sense. Creating and deleting an account is reserved to ROOT
//! and ADMIN, but nothing proves *a human* is behind the request. The
//! complete chain -- login screen, verification, session -- belongs to
//! the OS layer.

use alloc::vec::Vec;

use super::error::{self, ErrorCode, SysError, SysResult};
use super::table::{Entry, entry};
use super::Call;
use crate::fs::{self, perm::Credentials, perm::Permissions, perm::Role};

pub const SYS_USER_SELF: u16 = 0x0501;
pub const SYS_USER_LIST: u16 = 0x0502;
pub const SYS_USER_LOOKUP: u16 = 0x0503;
pub const SYS_USER_SETID: u16 = 0x0504;
pub const SYS_USER_CAN: u16 = 0x0505;
pub const SYS_USER_CREATE: u16 = 0x0506;
pub const SYS_USER_DELETE: u16 = 0x0507;

pub static TABLE: &[Entry] = &[
    entry!(SYS_USER_SELF, "SELF", Sync, user_self),
    entry!(SYS_USER_LIST, "LIST", Sync, list),
    entry!(SYS_USER_LOOKUP, "LOOKUP", Sync, lookup),
    entry!(SYS_USER_SETID, "SETID", Sync, setid),
    entry!(SYS_USER_CAN, "CAN", Sync, can),
    entry!(SYS_USER_CREATE, "CREATE", Sync, create),
    entry!(SYS_USER_DELETE, "DELETE", Sync, delete),
];

const NAME_MAX: u64 = 56;

/// Layout of an account record.
///
/// ```text
///   0x00  4  uid
///   0x04  4  gid
///   0x08  1  role
///   0x09  1  length of the name
///   0x0A 56  name
/// ```
pub const ACCOUNT_SIZE: usize = 0x42;

fn encode_account(uid: u32, gid: u32, role: Role, name: &str) -> [u8; ACCOUNT_SIZE] {
    let mut record = [0u8; ACCOUNT_SIZE];
    record[0x00..0x04].copy_from_slice(&uid.to_le_bytes());
    record[0x04..0x08].copy_from_slice(&gid.to_le_bytes());
    record[0x08] = role as u8;
    let bytes = name.as_bytes();
    let length = bytes.len().min(ACCOUNT_SIZE - 0x0A);
    record[0x09] = length as u8;
    record[0x0A..0x0A + length].copy_from_slice(&bytes[..length]);
    record
}

/// Does a role give the right to administer the accounts?
fn may_administer(who: &Credentials) -> bool {
    matches!(who.role, Role::Root | Role::Admin)
}

/// `SELF(buffer, size)` -- identity of the caller.
fn user_self(call: &mut Call) -> SysResult<u64> {
    let who = call.who();
    let name = fs::account_of(who.uid)
        .map(|(name, _, _)| name)
        .unwrap_or_else(|| alloc::string::String::from("?"));
    let record = encode_account(who.uid, who.gid, who.role, &name);
    call.write_bytes(call.arg(0), call.arg(1), &record)
}

/// `LIST(buffer, size)` -- returns the total number of accounts.
fn list(call: &mut Call) -> SysResult<u64> {
    let accounts = fs::account_names();
    let capacity = (call.arg(1) as usize) / ACCOUNT_SIZE;
    let count = accounts.len().min(capacity);

    let mut buffer = Vec::with_capacity(count * ACCOUNT_SIZE);
    for (name, role, uid) in accounts.iter().take(count) {
        let gid = fs::account_of(*uid).map(|(_, _, gid)| gid).unwrap_or(0);
        buffer.extend_from_slice(&encode_account(*uid, gid, *role, name));
    }

    call.write_bytes(call.arg(0), call.arg(1), &buffer)?;
    Ok(accounts.len() as u64)
}

/// `LOOKUP(name, length, buffer, size)`.
fn lookup(call: &mut Call) -> SysResult<u64> {
    let name = call.read_str(call.arg(0), call.arg(1).min(NAME_MAX))?;
    let who = fs::credentials_of(&name).ok_or_else(|| error::missing(&name))?;
    let record = encode_account(who.uid, who.gid, who.role, &name);
    call.write_bytes(call.arg(2), call.arg(3), &record)
}

/// `SETID(name, length)` -- gives up one's rights for those of another
/// account.
///
/// Refused as soon as the operation would increase the privilege, the
/// comparison being made on the **rank** of the role: ROOT is 0, GUEST is
/// 3. A service dropping to SERVICE therefore never climbs back, and a
/// user account cannot declare itself administrator.
fn setid(call: &mut Call) -> SysResult<u64> {
    let name = call.read_str(call.arg(0), call.arg(1).min(NAME_MAX))?;
    let target = fs::credentials_of(&name).ok_or_else(|| error::missing(&name))?;
    let current = call.who();

    if rank(target.role) < rank(current.role) {
        return Err(SysError::with_two(
            ErrorCode::PermissionDenied,
            "elevation de privilege refusee vers",
            &name,
        ));
    }

    if !crate::scheduler::set_credentials(call.pid, target) {
        return Err(error::missing("tache appelante"));
    }

    crate::serial_println!(
        "[user] PID {} passe sous le compte \"{}\" ({})",
        call.pid,
        name,
        target.role.as_str()
    );
    Ok(u64::from(target.uid))
}

/// Rank of a role: the lower it is, the more privileged the role.
///
/// SERVICE is treated as a system role: it is internal to the `.dsa` and
/// must not be adoptable by a user account.
fn rank(role: Role) -> u8 {
    match role {
        Role::Root => 0,
        Role::Service => 1,
        Role::Admin => 2,
        Role::User => 3,
        Role::Guest => 4,
    }
}

/// `CAN(path, length, permission)` -- does the caller have this right?
///
/// Returns 1 or 0, never an access error: the question bears on the
/// right, and "no" is a valid answer. A program can thus grey out a
/// button without causing a refusal entry in the audit log on every
/// screen refresh.
fn can(call: &mut Call) -> SysResult<u64> {
    if !fs::is_ready() {
        return Err(SysError::with(ErrorCode::NoDevice, "aucun volume monte"));
    }
    let path = call.read_str(call.arg(0), call.arg(1).min(1024))?;
    let permission = Permissions(call.arg(2));
    if permission.unknown_bits() != 0 || call.arg(2).count_ones() != 1 {
        return Err(error::invalid("une seule permission connue a la fois"));
    }

    let who = call.who();
    let node = match fs::stat(&path, who) {
        Ok(node) => node,
        // An unreachable path obviously grants no right.
        Err(_) => return Ok(0),
    };

    let granted = if who.role.bypasses_masks() {
        true
    } else if node.owner == who.uid {
        node.perm_owner.contains(call.arg(2))
    } else if node.group == who.gid {
        node.perm_group.contains(call.arg(2))
    } else {
        node.perm_other.contains(call.arg(2))
    };

    Ok(u64::from(granted))
}

/// `CREATE(name, length, uid, gid, role)`.
fn create(call: &mut Call) -> SysResult<u64> {
    let who = call.who();
    if !may_administer(&who) {
        return Err(error::denied("creation de compte reservee a ADMIN"));
    }

    let name = call.read_str(call.arg(0), call.arg(1).min(NAME_MAX))?;
    let uid = u32::try_from(call.arg(2)).map_err(|_| error::invalid("uid hors bornes"))?;
    let gid = u32::try_from(call.arg(3)).map_err(|_| error::invalid("gid hors bornes"))?;
    let role = Role::from_code(call.arg(4) as u8).ok_or_else(|| error::invalid("role inconnu"))?;

    // An ADMIN does not make a ROOT: that would be a roundabout
    // elevation, obtained by creating an account then moving into it.
    if rank(role) < rank(who.role) {
        return Err(error::denied("creation d'un compte plus privilegie"));
    }

    fs::create_account(uid, gid, &name, role).map_err(|e| error::from_fs(e, &name))?;
    crate::serial_println!(
        "[user] compte \"{}\" cree - uid {}, role {}",
        name,
        uid,
        role.as_str()
    );
    Ok(u64::from(uid))
}

/// `DELETE(name, length)`.
fn delete(call: &mut Call) -> SysResult<u64> {
    let who = call.who();
    if !may_administer(&who) {
        return Err(error::denied("suppression de compte reservee a ADMIN"));
    }

    let name = call.read_str(call.arg(0), call.arg(1).min(NAME_MAX))?;
    let target = fs::credentials_of(&name).ok_or_else(|| error::missing(&name))?;

    if target.uid == who.uid {
        return Err(error::invalid("un compte ne se supprime pas lui-meme"));
    }
    if rank(target.role) < rank(who.role) {
        return Err(error::denied("suppression d'un compte plus privilegie"));
    }

    fs::delete_account(&name).map_err(|e| error::from_fs(e, &name))?;
    crate::serial_println!("[user] compte \"{}\" supprime", name);
    Ok(0)
}

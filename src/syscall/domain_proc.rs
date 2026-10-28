//! 5.3 -- Domain 0x02, processes (spec 6.3).
//!
//! ## What `SETPRIO` modifies, and why it is the only possible reading
//!
//! 6.3 lists `SYS_PROC_SETPRIO` (0x0207, "Modify the priority") and 7.1
//! declares the priority fixed. The contradiction is only apparent: 7.1
//! ends with its own exception -- "the priority [...] does not change
//! while running, **except for the `.gs` which switch automatically
//! between foreground and background**".
//!
//! There are therefore two distinct quantities, and the spec never
//! confuses them:
//!
//! - the **priority class**, between 0 and 6, fixed by the format of the
//!   binary; it is what governs election and the 7.2 affinity, and it is
//!   what 7.1 declares immutable;
//! - the **CPU slice**, which additionally depends on the
//!   foreground / background state, and which 7.3 gives explicitly in two
//!   columns.
//!
//! `SETPRIO` acts on the second. It is not a compromise: 7.3 **itself
//! enumerates** the types whose slice is modifiable, in its "Note" column.
//!
//! | Type | Slice bg/fg | Note of 7.3 | `SETPRIO` |
//! |---|---|---|---|
//! | `.dsa` | 40 / 40 | "Never reduced" | refused |
//! | `.ss` | 15 / 25 | -- | accepted |
//! | `.gs` | 2 / 20 | "Switches automatically" | accepted |
//! | `.cus` | 4 / 10 | -- | accepted |
//! | `.ras` | 1 / 5 | "Never modifiable" | refused |
//!
//! The two refusals are not caution on our part: they are written in the
//! 7.3 table. The internal kernel tasks and the idle one are refused too,
//! being outside the hierarchy of 7.1.
//!
//! ## Who has the right to promote
//!
//! 7.1 says the `.gs` switch **automatically**, and 3.4 that their
//! background mode cuts the CPU to 2 %. If an application could declare
//! itself in the foreground, that guarantee would be worth nothing: one
//! call would be enough to refuse being paused. The rule is therefore
//! asymmetric, and it follows from the spec:
//!
//! - **going down to the background is always allowed** -- yielding
//!   CPU is never a privilege;
//! - **coming back to the foreground is reserved** to the system:
//!   `.dsa`, `.ss` -- the compositor of 8.1 -- and ROOT and ADMIN.
//!
//! It is through this call that the compositor will exercise the switch
//! of 3.4 in phase 7.2. The mechanism already existed
//! (`scheduler::set_foreground`); what was missing was the policy.
//!
//! ## What `WAIT` closes
//!
//! Since phase 2 the roadmap carried a limit: "the DEAD tasks free
//! neither their stack nor their image". `try_reap` returns both -- the
//! kernel stack to the heap allocator, the loader image to the frame
//! allocator. A claimed task stays in the table as a tombstone:
//! removing it would shift the indexes each core holds on its
//! current task.

use alloc::vec::Vec;

use super::error::{self, ErrorCode, SysError, SysResult};
use super::table::{Entry, entry};
use super::Call;
use crate::process::State;
use crate::scheduler::{self, TaskInfo};

pub const SYS_PROC_SPAWN: u16 = 0x0201;
pub const SYS_PROC_KILL: u16 = 0x0202;
pub const SYS_PROC_SUSPEND: u16 = 0x0203;
pub const SYS_PROC_RESUME: u16 = 0x0204;
pub const SYS_PROC_STAT: u16 = 0x0205;
pub const SYS_PROC_LIST: u16 = 0x0206;
pub const SYS_PROC_SETPRIO: u16 = 0x0207;
/// Absent from the spec, indispensable to cooperative scheduling.
pub const SYS_PROC_YIELD: u16 = 0x0208;
pub const SYS_PROC_WAIT: u16 = 0x0209;
pub const SYS_PROC_SELF: u16 = 0x020A;
/// Absent from the spec: termination with an exit code. `KILL` ends
/// without saying anything, which makes `WAIT` mute.
pub const SYS_PROC_EXIT: u16 = 0x020B;

pub static TABLE: &[Entry] = &[
    entry!(SYS_PROC_SPAWN, "SPAWN", Async, spawn),
    entry!(SYS_PROC_KILL, "KILL", Sync, kill),
    entry!(SYS_PROC_SUSPEND, "SUSPEND", Sync, suspend),
    entry!(SYS_PROC_RESUME, "RESUME", Sync, resume),
    entry!(SYS_PROC_STAT, "STAT", Sync, stat),
    entry!(SYS_PROC_LIST, "LIST", Sync, list),
    entry!(SYS_PROC_SETPRIO, "SETPRIO", Sync, setprio),
    entry!(SYS_PROC_YIELD, "YIELD", Sync, yield_now),
    entry!(SYS_PROC_WAIT, "WAIT", Async, wait),
    entry!(SYS_PROC_SELF, "SELF", Sync, self_pid),
    entry!(SYS_PROC_EXIT, "EXIT", Sync, exit),
];

// ============================================================
//  5.3.1 -- SPAWN / KILL / SUSPEND / RESUME
// ============================================================

/// `SPAWN(path, length)` -- loads a binary and launches it.
///
/// The path names a file of the filesystem; its content crosses the whole
/// phase 3 check -- magic, header, rules of 3.4, signature -- before a
/// task exists. A refused binary leaves nothing behind it.
/// The priority and the core are not parameters: they follow from the
/// format declared in the header (7.1 and 7.2).
fn spawn(call: &mut Call) -> SysResult<u64> {
    if !crate::fs::is_ready() {
        return Err(SysError::with(ErrorCode::NoDevice, "aucun volume monte"));
    }

    let length = call.arg(1);
    if length > 1024 {
        return Err(SysError::with(ErrorCode::NameTooLong, "chemin"));
    }
    let path = call.read_str(call.arg(0), length)?;

    // The right to execute is that of 5: it bears on the file **and** on
    // the directory containing it. `fs::is_executable` applies the
    // two-level rule; bypassing it here would make it decorative.
    let executable = crate::fs::is_executable(&path, call.who())
        .map_err(|e| error::from_fs(e, &path))?;
    if !executable {
        return Err(error::denied(&path));
    }

    let data = crate::fs::read_file(&path, call.who()).map_err(|e| error::from_fs(e, &path))?;

    match crate::binfmt::loader::load_and_spawn(&data) {
        Ok((image, pid)) => {
            crate::binfmt::loader::keep(pid, image);
            Ok(pid)
        }
        Err(load) => {
            // The distinction matters to the caller: a malformed binary
            // is fixed by rebuilding it, a refused binary by changing its
            // signature or its format, a saturated arena by closing
            // something else. Crushing them all into one error would make
            // the diagnosis impossible.
            use crate::binfmt::loader::LoadError;
            let code = match &load {
                LoadError::Malformed(_) => ErrorCode::InvalidArgument,
                LoadError::Refused(_) | LoadError::Untrusted(_) => ErrorCode::PermissionDenied,
                LoadError::TooLarge(_) | LoadError::StackTooLarge(_) => ErrorCode::TooLarge,
                LoadError::ArenaExhausted
                | LoadError::OutOfFrames
                | LoadError::MappingFailed(_) => ErrorCode::OutOfMemory,
                LoadError::UnsupportedReloc => ErrorCode::Unsupported,
            };
            Err(SysError::with_two(code, &path, load.message()))
        }
    }
}

/// `KILL(pid)` -- a null PID names the caller.
fn kill(call: &mut Call) -> SysResult<u64> {
    let target = if call.arg(0) == 0 {
        call.pid
    } else {
        call.arg(0)
    };

    if target == call.pid {
        crate::serial_println!("[syscall] PID {} demande sa propre terminaison", target);
        // Never returns: the context switches to another task.
        scheduler::exit_current_with(0);
    }

    guard_target(call, target)?;
    if scheduler::kill(target, -1) {
        Ok(0)
    } else {
        Err(error::missing("PID"))
    }
}

/// `EXIT(code)` -- ends the caller, publishing an exit code.
fn exit(call: &mut Call) -> SysResult<u64> {
    scheduler::exit_current_with(call.arg(0) as i64);
}

/// Checks that one has the right to act on a task that is not oneself.
///
/// Rule: ROOT and ADMIN act on everything; the others only on a task of
/// the same account. Without that, any program could kill the compositor.
fn guard_target(call: &Call, target: u64) -> SysResult<()> {
    let who = call.who();
    if matches!(
        who.role,
        crate::fs::perm::Role::Root | crate::fs::perm::Role::Admin
    ) {
        return Ok(());
    }

    match scheduler::task_info(target) {
        Some(info) if info.uid == who.uid => Ok(()),
        Some(_) => Err(error::denied("tache d'un autre compte")),
        None => Err(error::missing("PID")),
    }
}

fn suspend(call: &mut Call) -> SysResult<u64> {
    let target = call.arg(0);
    guard_target(call, target)?;

    // Suspending oneself is a disguised block: the call would only hand
    // back if somebody else came to wake us, and nothing guarantees it.
    // `YIELD` or `WAIT` are the right tools.
    if target == call.pid {
        return Err(error::invalid("une tache ne se suspend pas elle-meme"));
    }

    if scheduler::suspend(target) {
        Ok(0)
    } else {
        Err(error::missing("PID ou tache non eligible"))
    }
}

fn resume(call: &mut Call) -> SysResult<u64> {
    let target = call.arg(0);
    guard_target(call, target)?;
    if scheduler::resume(target) {
        Ok(0)
    } else {
        Err(error::missing("PID ou tache non suspendue"))
    }
}

// ============================================================
//  5.3.2 -- STAT / LIST / SETPRIO
// ============================================================

/// Layout of a process record.
///
/// ```text
///   0x00  8  pid
///   0x08  8  pid of the parent
///   0x10  8  CPU ticks consumed
///   0x18  8  number of elections
///   0x20  8  stack memory tied up
///   0x28  8  exit code (signed)
///   0x30  4  uid
///   0x34  4  gid
///   0x38  1  role
///   0x39  1  binary type
///   0x3A  1  state
///   0x3B  1  priority
///   0x3C  1  core
///   0x3D  1  foreground
///   0x3E  2  slice in ticks
///   0x40  4  flags declared by the header
///   0x44  1  length of the name
///   0x45  3  reserved
///   0x48 32  name
/// ```
pub const PROC_ENTRY_SIZE: usize = 0x68;

fn state_code(state: State) -> u8 {
    match state {
        State::Ready => 0,
        State::Running => 1,
        State::Sleeping => 2,
        State::Suspended => 3,
        State::Dead => 4,
    }
}

fn binary_code(kind: crate::process::BinaryType) -> u8 {
    use crate::process::BinaryType::*;
    match kind {
        Dsa => 1,
        Ss => 2,
        Gs => 3,
        Cus => 4,
        Ras => 5,
        Kernel => 6,
        Idle => 7,
    }
}

fn encode_task(info: &TaskInfo) -> [u8; PROC_ENTRY_SIZE] {
    let mut record = [0u8; PROC_ENTRY_SIZE];
    record[0x00..0x08].copy_from_slice(&info.pid.to_le_bytes());
    record[0x08..0x10].copy_from_slice(&info.parent.to_le_bytes());
    record[0x10..0x18].copy_from_slice(&info.ticks_used.to_le_bytes());
    record[0x18..0x20].copy_from_slice(&info.schedule_count.to_le_bytes());
    record[0x20..0x28].copy_from_slice(&info.stack_bytes.to_le_bytes());
    record[0x28..0x30].copy_from_slice(&info.exit_code.to_le_bytes());
    record[0x30..0x34].copy_from_slice(&info.uid.to_le_bytes());
    record[0x34..0x38].copy_from_slice(&info.gid.to_le_bytes());
    record[0x38] = info.role as u8;
    record[0x39] = binary_code(info.binary_type);
    record[0x3A] = state_code(info.state);
    record[0x3B] = info.priority;
    record[0x3C] = info.cpu;
    record[0x3D] = u8::from(info.foreground);
    record[0x3E..0x40].copy_from_slice(&(info.slice.min(u16::MAX as u32) as u16).to_le_bytes());
    record[0x40..0x44].copy_from_slice(&info.app_flags.to_le_bytes());

    let name = info.name.as_bytes();
    let length = name.len().min(PROC_ENTRY_SIZE - 0x48);
    record[0x44] = length as u8;
    record[0x48..0x48 + length].copy_from_slice(&name[..length]);
    record
}

/// `STAT(pid, buffer, size)` -- a null PID names the caller.
fn stat(call: &mut Call) -> SysResult<u64> {
    let target = if call.arg(0) == 0 {
        call.pid
    } else {
        call.arg(0)
    };
    let info = scheduler::task_info(target).ok_or_else(|| error::missing("PID"))?;
    call.write_bytes(call.arg(1), call.arg(2), &encode_task(&info))
}

/// `LIST(buffer, size)` -- returns the total number of live tasks.
fn list(call: &mut Call) -> SysResult<u64> {
    let tasks = scheduler::task_list();
    let capacity = (call.arg(1) as usize) / PROC_ENTRY_SIZE;
    let count = tasks.len().min(capacity);

    let mut buffer = Vec::with_capacity(count * PROC_ENTRY_SIZE);
    for info in tasks.iter().take(count) {
        buffer.extend_from_slice(&encode_task(info));
    }

    call.write_bytes(call.arg(0), call.arg(1), &buffer)?;
    Ok(tasks.len() as u64)
}

/// Value of `SETPRIO`: put the task in the background.
pub const PRIO_BACKGROUND: u64 = 0;
/// Value of `SETPRIO`: put the task in the foreground.
pub const PRIO_FOREGROUND: u64 = 1;

/// Is the slice of this type modifiable? Table of 7.3.
fn slice_is_variable(kind: crate::process::BinaryType) -> bool {
    use crate::process::BinaryType::*;
    match kind {
        // "Never reduced" and "Never modifiable": those are the words of
        // the spec, not an interpretation.
        Dsa | Ras => false,
        // Outside the hierarchy of 7.1: nothing to switch.
        Kernel | Idle => false,
        Ss | Gs | Cus => true,
    }
}

/// `SETPRIO(pid, value)` -- foreground / background switch (7.3).
///
/// Returns the **effective slice in ticks** after the operation: the
/// caller thus learns what it really obtained, instead of a zero that
/// would say nothing. See the module header for the per-type rule and for
/// the asymmetry between going down and coming back.
fn setprio(call: &mut Call) -> SysResult<u64> {
    let target = if call.arg(0) == 0 {
        call.pid
    } else {
        call.arg(0)
    };
    guard_target(call, target)?;

    let info = scheduler::task_info(target).ok_or_else(|| error::missing("PID"))?;

    let foreground = match call.arg(1) {
        PRIO_BACKGROUND => false,
        PRIO_FOREGROUND => true,
        // A value beyond can only name a priority class. The refusal
        // names the real class of the task: it is what lets the caller
        // establish that it is not negotiable.
        _ => {
            return Err(SysError::with_two(
                ErrorCode::InvalidArgument,
                "classe de priorite fixee par le format (spec 7.1) :",
                info.binary_type.as_str(),
            ));
        }
    };

    if !slice_is_variable(info.binary_type) {
        return Err(SysError::with_two(
            ErrorCode::PermissionDenied,
            "tranche non modifiable pour ce format (spec 7.3) :",
            info.binary_type.as_str(),
        ));
    }

    // Going down is always allowed; coming back is reserved to the
    // system. Without that asymmetry, the background mode of 3.4 -- "CPU
    // cut to 2 %" -- would be no more than a suggestion an application
    // would refuse with one call.
    if foreground && !may_promote(call, target) {
        return Err(error::denied(
            "seul le systeme remonte une tache au premier plan (spec 7.1)",
        ));
    }

    if !scheduler::set_foreground(target, foreground) {
        return Err(error::missing("PID"));
    }

    Ok(u64::from(info.binary_type.time_slice(foreground)))
}

/// May the caller bring a task back to the foreground?
///
/// The compositor is a `.ss` (8.1): it is what will exercise the switch
/// of 3.4. The `.dsa` are the system itself, and ROOT as ADMIN keep their
/// hand on everything.
fn may_promote(call: &Call, target: u64) -> bool {
    if call.origin == super::Origin::Kernel {
        return true;
    }
    if matches!(
        call.who().role,
        crate::fs::perm::Role::Root | crate::fs::perm::Role::Admin
    ) {
        return true;
    }

    match scheduler::task_info(call.pid) {
        Some(info) => {
            // A system task promotes whoever it likes; an application
            // promotes nobody, not even itself.
            let _ = target;
            matches!(
                info.binary_type,
                crate::process::BinaryType::Dsa
                    | crate::process::BinaryType::Ss
                    | crate::process::BinaryType::Kernel
            )
        }
        None => false,
    }
}

fn yield_now(_call: &mut Call) -> SysResult<u64> {
    scheduler::yield_now();
    Ok(0)
}

// ============================================================
//  5.3.3 -- WAIT / SELF
// ============================================================

/// `WAIT(pid)` -- waits for the end of a task and returns its exit code.
///
/// The only call of the domain that puts its caller to sleep, hence its
/// asynchronous classification. The exit code comes back as-is, including
/// negative: the caller tells it from an error by the fact that `WAIT`
/// never returns a code below `-0xFF`.
fn wait(call: &mut Call) -> SysResult<u64> {
    call.require_async("WAIT")?;
    let target = call.arg(0);

    if target == call.pid {
        return Err(error::invalid("une tache ne s'attend pas elle-meme"));
    }
    if !scheduler::exists(target) {
        return Err(error::missing("PID"));
    }

    super::count_block();
    match scheduler::wait_for(target) {
        Some(code) => Ok(code as u64),
        None => Err(error::missing("la tache a disparu avant d'etre attendue")),
    }
}

fn self_pid(call: &mut Call) -> SysResult<u64> {
    Ok(call.pid)
}

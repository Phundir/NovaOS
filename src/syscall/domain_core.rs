//! Domain 0x00 -- service calls.
//!
//! This domain is not in the specification: it gathers what belongs to no
//! subsystem. Three calls only, and each has a precise reason to exist.
//!
//! `SYS_DEBUG_PRINT` is an acknowledged relic of phase 2: it writes to
//! both consoles without going through the filesystem. It will disappear
//! when `/dev/console` exists, that is when the drivers are in user
//! space.
//!
//! `SYS_ERROR_LAST` is the indispensable counterpart of the ABI: RAX
//! carries only the code, and spec 6.1 demands a name, a message and a
//! context. Without this call, a program would know *that* it failed but
//! never *on what*.
//!
//! `SYS_API_INFO` makes the dispatch table queryable. A program
//! discovering the kernel it runs on can thus check that a call exists
//! before emitting it, rather than deriving a version from an
//! `INVALID_SYSCALL`.

use super::error::{self, SysResult};
use super::table::{Entry, entry};
use super::{Call, table};
use crate::vga::Color;

pub const SYS_DEBUG_PRINT: u16 = 0x0001;
pub const SYS_ERROR_LAST: u16 = 0x0002;
pub const SYS_API_INFO: u16 = 0x0003;

pub static TABLE: &[Entry] = &[
    entry!(SYS_DEBUG_PRINT, "DEBUG_PRINT", Sync, debug_print),
    entry!(SYS_ERROR_LAST, "ERROR_LAST", Sync, error_last),
    entry!(SYS_API_INFO, "API_INFO", Sync, api_info),
];

/// Maximum length of a trace message.
const PRINT_MAX: u64 = 512;

/// Writes a message to both consoles.
fn debug_print(call: &mut Call) -> SysResult<u64> {
    let length = call.arg(1).min(PRINT_MAX);
    let text = call.read_str(call.arg(0), length)?;

    crate::vga::set_color(Color::Pink, Color::Black);
    crate::kprintln!("{}", text);
    crate::vga::set_color(Color::LightGray, Color::Black);

    Ok(text.len() as u64)
}

/// Layout of the record returned by `SYS_ERROR_LAST`.
///
/// ```text
///   0x00  4  error code
///   0x04  2  number of the offending call
///   0x06  1  length of the name
///   0x07  1  length of the message
///   0x08  1  length of the context
///   0x09  7  reserved
///   0x10 32  symbolic name
///   0x30 64  message
///   0x70 96  context
/// ```
pub const ERROR_RECORD_SIZE: usize = 0xD0;

const NAME_FIELD: usize = 32;
const MESSAGE_FIELD: usize = 64;

/// Returns the last error of the calling task, in its complete form.
fn error_last(call: &mut Call) -> SysResult<u64> {
    let Some(last) = super::last_error(call.pid) else {
        // No recorded error is a legitimate state, not a failure:
        // returning zero bytes says so without polluting the record.
        return Ok(0);
    };

    let mut record = [0u8; ERROR_RECORD_SIZE];
    record[0..4].copy_from_slice(&(last.error.code as u32).to_le_bytes());
    record[4..6].copy_from_slice(&last.number.to_le_bytes());

    let mut put = |offset: usize, width: usize, text: &str| -> u8 {
        let bytes = text.as_bytes();
        let length = bytes.len().min(width);
        record[offset..offset + length].copy_from_slice(&bytes[..length]);
        length as u8
    };

    let name_len = put(0x10, NAME_FIELD, last.error.name());
    let message_len = put(0x30, MESSAGE_FIELD, last.error.message());
    let context_len = put(0x70, error::CONTEXT_MAX, last.error.context());

    record[6] = name_len;
    record[7] = message_len;
    record[8] = context_len;

    call.write_bytes(call.arg(0), call.arg(1), &record)
}

/// Layout of a call description entry.
///
/// ```text
///   0x00  2  number
///   0x02  1  mode: 0 synchronous, 1 asynchronous
///   0x03  1  length of the name
///   0x04 28  name, without the domain prefix
/// ```
pub const API_ENTRY_SIZE: usize = 32;

/// Describes the API: one record per known call.
///
/// Returns the **total number** of calls, not the number of bytes
/// written: a caller with too short a buffer thus learns what it needs.
fn api_info(call: &mut Call) -> SysResult<u64> {
    let total = table::count() as u64;
    let capacity = (call.arg(1) as usize) / API_ENTRY_SIZE;
    if capacity == 0 {
        return Ok(total);
    }

    let mut buffer = alloc::vec::Vec::with_capacity(capacity * API_ENTRY_SIZE);
    let mut written = 0usize;

    'domains: for (_, domain) in table::DOMAINS.iter() {
        for item in domain.iter() {
            if written == capacity {
                break 'domains;
            }
            let mut record = [0u8; API_ENTRY_SIZE];
            record[0..2].copy_from_slice(&item.number.to_le_bytes());
            record[2] = u8::from(item.mode == super::Mode::Async);
            let name = item.name.as_bytes();
            let length = name.len().min(API_ENTRY_SIZE - 4);
            record[3] = length as u8;
            record[4..4 + length].copy_from_slice(&name[..length]);
            buffer.extend_from_slice(&record);
            written += 1;
        }
    }

    call.write_bytes(call.arg(0), call.arg(1), &buffer)?;
    Ok(total)
}

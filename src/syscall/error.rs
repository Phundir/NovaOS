//! 5.1.1 and 5.1.2 -- System call result and error (spec 6.1).
//!
//! The specification demands that "every syscall return a `SysResult<T>`
//! holding `success`, `data` and a structured `SysError` (code, name,
//! message, context)". Two constraints stand against it:
//!
//! - **RAX carries only 64 bits.** A code, a name, a message and a
//!   context do not fit in it. So the ABI carries the part that must be
//!   immediate -- success or error code -- and the rest waits to be
//!   claimed.
//! - **The error path must allocate nothing.** An error may happen
//!   because the heap is full; building a `String` to explain it would
//!   fail in its turn.
//!
//! Hence the form kept: `SysResult<T>` is an ordinary `Result` on the
//! kernel side, `SysError` carries a fixed-size context, and the last
//! failure of each task is kept so that `SYS_ERROR_LAST` can return its
//! complete version. The `success` and `data` of the spec are exactly the
//! two arms of the `Result`; naming them otherwise would add nothing.
//!
//! ## The context, and why it matters
//!
//! `NOT_FOUND` alone helps nobody. `NOT_FOUND` accompanied by
//! `/users/admin/settings/absent.cfg` says everything. The context is
//! that precision: the path refused, the PID not found, the permission
//! that was missing. It is truncated rather than allocated, and truncated
//! is better than absent.

use crate::fs::FsError;
use crate::ipc::IpcError;

/// Maximum length of the context of an error.
///
/// A complete Nova OS path fits in 55 characters per segment; 96 bytes
/// cover a path of usual depth plus a pattern. What exceeds is cut -- an
/// error must never fail.
pub const CONTEXT_MAX: usize = 96;

/// System error codes (spec 6.1).
///
/// The code is what crosses the ABI: returned negated in RAX, it must
/// therefore stay small and stable. The values are never reassigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ErrorCode {
    InvalidSyscall = 0x0001,
    InvalidArgument = 0x0002,
    PermissionDenied = 0x0003,
    NotFound = 0x0004,
    OutOfMemory = 0x0005,
    WouldBlock = 0x0006,
    QueueFull = 0x0007,
    AlreadyExists = 0x0008,
    NotADirectory = 0x0009,
    IsADirectory = 0x000A,
    DirectoryNotEmpty = 0x000B,
    NoSpace = 0x000C,
    ReadOnly = 0x000D,
    TooLarge = 0x000E,
    Busy = 0x000F,
    NoDevice = 0x0010,
    Corrupt = 0x0011,
    NameTooLong = 0x0012,
    /// The caller asked for a blocking operation from a call declared
    /// synchronous (spec 6.1, "Mode" column).
    NotAsync = 0x0013,
    /// The domain exists, the call too, but the hardware or the service
    /// needed is not there.
    Unsupported = 0x0014,
    NotImplemented = 0x00FF,
}

impl ErrorCode {
    /// Symbolic name, as a program would log it.
    pub fn name(&self) -> &'static str {
        match self {
            ErrorCode::InvalidSyscall => "SYS_ERR_INVALID_SYSCALL",
            ErrorCode::InvalidArgument => "SYS_ERR_INVALID_ARGUMENT",
            ErrorCode::PermissionDenied => "SYS_ERR_PERMISSION_DENIED",
            ErrorCode::NotFound => "SYS_ERR_NOT_FOUND",
            ErrorCode::OutOfMemory => "SYS_ERR_OUT_OF_MEMORY",
            ErrorCode::WouldBlock => "SYS_ERR_WOULD_BLOCK",
            ErrorCode::QueueFull => "SYS_ERR_QUEUE_FULL",
            ErrorCode::AlreadyExists => "SYS_ERR_ALREADY_EXISTS",
            ErrorCode::NotADirectory => "SYS_ERR_NOT_A_DIRECTORY",
            ErrorCode::IsADirectory => "SYS_ERR_IS_A_DIRECTORY",
            ErrorCode::DirectoryNotEmpty => "SYS_ERR_DIRECTORY_NOT_EMPTY",
            ErrorCode::NoSpace => "SYS_ERR_NO_SPACE",
            ErrorCode::ReadOnly => "SYS_ERR_READ_ONLY",
            ErrorCode::TooLarge => "SYS_ERR_TOO_LARGE",
            ErrorCode::Busy => "SYS_ERR_BUSY",
            ErrorCode::NoDevice => "SYS_ERR_NO_DEVICE",
            ErrorCode::Corrupt => "SYS_ERR_CORRUPT",
            ErrorCode::NameTooLong => "SYS_ERR_NAME_TOO_LONG",
            ErrorCode::NotAsync => "SYS_ERR_NOT_ASYNC",
            ErrorCode::Unsupported => "SYS_ERR_UNSUPPORTED",
            ErrorCode::NotImplemented => "SYS_ERR_NOT_IMPLEMENTED",
        }
    }

    /// Explanation meant for a human. Printable ASCII only: it ends up on
    /// the serial console or on screen.
    pub fn message(&self) -> &'static str {
        match self {
            ErrorCode::InvalidSyscall => "numero d'appel systeme inconnu",
            ErrorCode::InvalidArgument => "argument invalide",
            ErrorCode::PermissionDenied => "acces refuse",
            ErrorCode::NotFound => "ressource introuvable",
            ErrorCode::OutOfMemory => "memoire insuffisante",
            ErrorCode::WouldBlock => "rien a lire sans attendre",
            ErrorCode::QueueFull => "file d'attente pleine",
            ErrorCode::AlreadyExists => "existe deja",
            ErrorCode::NotADirectory => "n'est pas un dossier",
            ErrorCode::IsADirectory => "est un dossier",
            ErrorCode::DirectoryNotEmpty => "dossier non vide",
            ErrorCode::NoSpace => "plus de place sur le support",
            ErrorCode::ReadOnly => "support en lecture seule",
            ErrorCode::TooLarge => "taille au-dela de la limite",
            ErrorCode::Busy => "ressource occupee",
            ErrorCode::NoDevice => "peripherique absent",
            ErrorCode::Corrupt => "structures incoherentes",
            ErrorCode::NameTooLong => "nom trop long",
            ErrorCode::NotAsync => "appel synchrone : blocage interdit",
            ErrorCode::Unsupported => "non pris en charge par ce noyau",
            ErrorCode::NotImplemented => "appel non encore implemente",
        }
    }
}

/// Structured error as spec 6.1 demands it.
///
/// `code`, `name` and `message` come from `ErrorCode`; only `context` is
/// proper to the occurrence. The buffer is fixed: building an error must
/// never depend on the state of the heap.
#[derive(Debug, Clone, Copy)]
pub struct SysError {
    pub code: ErrorCode,
    context: [u8; CONTEXT_MAX],
    context_len: u8,
}

impl SysError {
    pub const fn new(code: ErrorCode) -> Self {
        SysError {
            code,
            context: [0; CONTEXT_MAX],
            context_len: 0,
        }
    }

    /// Attaches a precision to the error: the path refused, the name not
    /// found, the value out of bounds.
    pub fn with(code: ErrorCode, context: &str) -> Self {
        let mut error = SysError::new(code);
        let bytes = context.as_bytes();
        let length = bytes.len().min(CONTEXT_MAX);
        error.context[..length].copy_from_slice(&bytes[..length]);
        error.context_len = length as u8;
        error
    }

    /// Two precisions concatenated, separated by a space. Avoids a
    /// `format!` on the error path, which could fail.
    pub fn with_two(code: ErrorCode, first: &str, second: &str) -> Self {
        let mut error = SysError::new(code);
        let mut length = 0;
        for source in [first.as_bytes(), b" ", second.as_bytes()] {
            let take = source.len().min(CONTEXT_MAX - length);
            error.context[length..length + take].copy_from_slice(&source[..take]);
            length += take;
            if length == CONTEXT_MAX {
                break;
            }
        }
        error.context_len = length as u8;
        error
    }

    pub fn name(&self) -> &'static str {
        self.code.name()
    }

    pub fn message(&self) -> &'static str {
        self.code.message()
    }

    pub fn context(&self) -> &str {
        core::str::from_utf8(&self.context[..self.context_len as usize]).unwrap_or("")
    }

    /// Value as it appears in RAX: the code, negated.
    pub fn encoded(&self) -> u64 {
        (-(self.code as i64)) as u64
    }
}

impl PartialEq for SysError {
    /// Two errors are equal if their codes are. The context describes the
    /// occurrence, not the nature of the problem: a test expecting
    /// `NOT_FOUND` has no business knowing the exact path.
    fn eq(&self, other: &Self) -> bool {
        self.code == other.code
    }
}

impl Eq for SysError {}

/// Result of a system call (spec 6.1).
///
/// `Ok` carries the data, `Err` the structured error: it is exactly the
/// `success` / `data` / `error` triple of the specification, in the form
/// Rust makes impossible to misuse -- one cannot read `data` without
/// having established `success`.
pub type SysResult<T> = Result<T, SysError>;

// ============================================================
//  Translation of the errors of the lower layers
// ============================================================

/// Converts an error of the filesystem.
///
/// The correspondence is explicit rather than automatic: each layer has
/// its vocabulary, and crushing `DirectoryNotEmpty` into a generic
/// `InvalidArgument` would deprive the caller of the information letting
/// it react.
pub fn from_fs(error: FsError, context: &str) -> SysError {
    let code = match error {
        FsError::NotFound | FsError::NoSuchDisk => ErrorCode::NotFound,
        FsError::AlreadyExists => ErrorCode::AlreadyExists,
        FsError::NotADirectory => ErrorCode::NotADirectory,
        FsError::IsADirectory => ErrorCode::IsADirectory,
        FsError::DirectoryNotEmpty => ErrorCode::DirectoryNotEmpty,
        FsError::PermissionDenied(_) => ErrorCode::PermissionDenied,
        FsError::InvalidName | FsError::InvalidPath | FsError::InvalidPermission => {
            ErrorCode::InvalidArgument
        }
        FsError::NoSpace | FsError::NoNodes => ErrorCode::NoSpace,
        FsError::TooLarge => ErrorCode::TooLarge,
        FsError::Corrupt | FsError::Format(_) => ErrorCode::Corrupt,
        FsError::Unsupported => ErrorCode::Unsupported,
        FsError::NotMounted => ErrorCode::NoDevice,
        FsError::Block(_) => ErrorCode::NoDevice,
    };
    SysError::with_two(code, context, error.message())
}

/// Converts an IPC error.
pub fn from_ipc(error: IpcError, context: &str) -> SysError {
    let code = match error {
        IpcError::NoSuchProcess | IpcError::NoSuchChannel => ErrorCode::NotFound,
        IpcError::QueueFull => ErrorCode::QueueFull,
        IpcError::PayloadTooLarge => ErrorCode::TooLarge,
        IpcError::ChannelExists => ErrorCode::AlreadyExists,
        // 12.1: both isolation refusals are permission refusals, and the
        // context attached says which -- "forbidden" calls for no
        // correction, "internal port demanded" calls for a precise one.
        IpcError::Forbidden | IpcError::NeedsPort => ErrorCode::PermissionDenied,
    };
    SysError::with_two(code, context, error.as_str())
}

/// Construction shorthands, so that the code of the domains stays
/// readable: `denied("path")` rather than six lines of structure.
pub fn invalid(context: &str) -> SysError {
    SysError::with(ErrorCode::InvalidArgument, context)
}

pub fn denied(context: &str) -> SysError {
    SysError::with(ErrorCode::PermissionDenied, context)
}

pub fn missing(context: &str) -> SysError {
    SysError::with(ErrorCode::NotFound, context)
}

pub fn unsupported(context: &str) -> SysError {
    SysError::with(ErrorCode::Unsupported, context)
}

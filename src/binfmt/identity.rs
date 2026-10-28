//! 3.1.3 -- Application identity metadata (spec 12.1).
//!
//! The `AppIdentity` is fixed at compile time and immutable: it is what
//! determines who the application may communicate with, which resources
//! it may request, and which warning is shown at install time.

use alloc::string::String;

use super::header::{BinaryFormat, SignatureType};

// ============================================================
//  Version
// ============================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
    pub build: u16,
}

impl Version {
    pub const fn new(major: u16, minor: u16, patch: u16, build: u16) -> Self {
        Version {
            major,
            minor,
            patch,
            build,
        }
    }
}

impl core::fmt::Display for Version {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{}.{}.{}+{}",
            self.major, self.minor, self.patch, self.build
        )
    }
}

// ============================================================
//  Declared permissions (spec 12.3, allow-list)
// ============================================================

/// Mask of the sensitive accesses declared in the header.
///
/// Allow-list model: what is not declared is refused, with no way to
/// obtain it at run time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Permissions(pub u64);

impl Permissions {
    pub const NONE: Permissions = Permissions(0);

    // --- network ---
    pub const NETWORK_INTERNET: u64 = 1 << 0;
    pub const NETWORK_LOCAL: u64 = 1 << 1;
    // --- devices ---
    pub const MICROPHONE: u64 = 1 << 2;
    pub const WEBCAM: u64 = 1 << 3;
    pub const AUDIO_OUTPUT: u64 = 1 << 4;
    // --- system ---
    pub const SYS_DIRECTORY: u64 = 1 << 5;
    pub const HARDWARE: u64 = 1 << 6;
    pub const USER_MANAGEMENT: u64 = 1 << 7;
    pub const KERNEL_SPACE: u64 = 1 << 8;
    // --- files ---
    pub const FS_USER_HOME: u64 = 1 << 9;
    pub const FS_FULL: u64 = 1 << 10;
    // --- communication ---
    pub const IPC_PUBLIC: u64 = 1 << 11;
    pub const IPC_INTERNAL_PORTS: u64 = 1 << 12;
    // --- interface ---
    pub const NOTIFICATIONS: u64 = 1 << 13;
    pub const FULLSCREEN: u64 = 1 << 14;
    pub const CLIPBOARD: u64 = 1 << 15;

    /// Set of the bits recognised. Any bit outside this mask makes the
    /// binary be rejected: an older kernel must not silently grant a
    /// permission it does not understand.
    pub const KNOWN: u64 = Self::NETWORK_INTERNET
        | Self::NETWORK_LOCAL
        | Self::MICROPHONE
        | Self::WEBCAM
        | Self::AUDIO_OUTPUT
        | Self::SYS_DIRECTORY
        | Self::HARDWARE
        | Self::USER_MANAGEMENT
        | Self::KERNEL_SPACE
        | Self::FS_USER_HOME
        | Self::FS_FULL
        | Self::IPC_PUBLIC
        | Self::IPC_INTERNAL_PORTS
        | Self::NOTIFICATIONS
        | Self::FULLSCREEN
        | Self::CLIPBOARD;

    pub const fn contains(&self, permission: u64) -> bool {
        self.0 & permission != 0
    }

    pub const fn unknown_bits(&self) -> u64 {
        self.0 & !Self::KNOWN
    }

    pub fn count(&self) -> u32 {
        (self.0 & Self::KNOWN).count_ones()
    }

    /// Permissions reachable by a given format (spec 12.4).
    ///
    /// The per-type sandbox imposes a ceiling: a `.cus` cannot claim
    /// access to kernel space, whatever its header announces.
    pub fn ceiling(format: BinaryFormat) -> u64 {
        match format {
            // Total trust.
            BinaryFormat::Dsa => Self::KNOWN,
            // Everything but kernel space and user management.
            BinaryFormat::Ss => Self::KNOWN & !Self::KERNEL_SPACE & !Self::USER_MANAGEMENT,
            // Isolated: no system, no hardware, no user management.
            BinaryFormat::Gs | BinaryFormat::Cus => {
                Self::NETWORK_INTERNET
                    | Self::NETWORK_LOCAL
                    | Self::MICROPHONE
                    | Self::WEBCAM
                    | Self::AUDIO_OUTPUT
                    | Self::FS_USER_HOME
                    | Self::IPC_INTERNAL_PORTS
                    | Self::NOTIFICATIONS
                    | Self::FULLSCREEN
                    | Self::CLIPBOARD
            }
            // Deny-list of 12.3: no communication, network blocked by
            // default. Hardware allowed, that is the whole point of the
            // format (spec 6.6).
            BinaryFormat::Ras => Self::HARDWARE | Self::AUDIO_OUTPUT | Self::FULLSCREEN,
        }
    }

    /// Readable list of the permissions set.
    pub fn names(&self) -> [(&'static str, bool); 16] {
        [
            ("NETWORK_INTERNET", self.contains(Self::NETWORK_INTERNET)),
            ("NETWORK_LOCAL", self.contains(Self::NETWORK_LOCAL)),
            ("MICROPHONE", self.contains(Self::MICROPHONE)),
            ("WEBCAM", self.contains(Self::WEBCAM)),
            ("AUDIO_OUTPUT", self.contains(Self::AUDIO_OUTPUT)),
            ("SYS_DIRECTORY", self.contains(Self::SYS_DIRECTORY)),
            ("HARDWARE", self.contains(Self::HARDWARE)),
            ("USER_MANAGEMENT", self.contains(Self::USER_MANAGEMENT)),
            ("KERNEL_SPACE", self.contains(Self::KERNEL_SPACE)),
            ("FS_USER_HOME", self.contains(Self::FS_USER_HOME)),
            ("FS_FULL", self.contains(Self::FS_FULL)),
            ("IPC_PUBLIC", self.contains(Self::IPC_PUBLIC)),
            ("IPC_INTERNAL_PORTS", self.contains(Self::IPC_INTERNAL_PORTS)),
            ("NOTIFICATIONS", self.contains(Self::NOTIFICATIONS)),
            ("FULLSCREEN", self.contains(Self::FULLSCREEN)),
            ("CLIPBOARD", self.contains(Self::CLIPBOARD)),
        ]
    }
}

// ============================================================
//  AppIdentity (spec 12.1)
// ============================================================

/// Immutable identity of an application.
///
/// Passed with every inter-application request: the receiving application
/// receives it in full and decides whether to accept (spec 12.2).
#[derive(Debug, Clone)]
pub struct AppIdentity {
    /// Unique identifier, in reverse notation: `com.editeur.produit`.
    pub app_id: String,
    /// Publisher or developer.
    pub author: String,
    /// Application group. Two apps of the same group communicate with no
    /// user prompt (spec 12.1).
    pub group: String,
    /// Name shown to the user.
    pub display_name: String,
    pub version: Version,
    pub format: BinaryFormat,
    pub signature: SignatureType,
}

impl AppIdentity {
    /// Do two applications belong to the same group?
    pub fn same_group(&self, other: &AppIdentity) -> bool {
        !self.group.is_empty() && self.group == other.group
    }

    /// Communication rules of the 12.1 table.
    ///
    /// Returns the communication mode allowed between two applications.
    pub fn communication_with(&self, other: &AppIdentity) -> Communication {
        use BinaryFormat::*;

        // Absolute deny-list: a `.ras` is totally isolated.
        if self.format == Ras || other.format == Ras {
            return Communication::Blocked;
        }

        if self.same_group(other) {
            if self.format == other.format {
                return Communication::DirectIpc;
            }
            // Compatible types within the same group.
            let compatible = matches!(
                (self.format, other.format),
                (Dsa, Ss) | (Ss, Dsa) | (Ss, Cus) | (Cus, Ss) | (Cus, Gs) | (Gs, Cus)
            );
            if compatible {
                return Communication::DirectIpc;
            }
        }

        Communication::InternalPort
    }
}

/// Communication mode resulting from the 12.1 table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Communication {
    /// Direct IPC, with no prompt.
    DirectIpc,
    /// Dedicated internal port -- the receiving app accepts or refuses,
    /// with a user prompt.
    InternalPort,
    /// Never allowed.
    Blocked,
}

impl Communication {
    pub fn as_str(&self) -> &'static str {
        match self {
            Communication::DirectIpc => "IPC directe",
            Communication::InternalPort => "port interne + prompt",
            Communication::Blocked => "bloquee",
        }
    }
}

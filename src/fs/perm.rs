//! 4.5 -- Permissions and roles (spec 5).
//!
//! Thirteen permissions, some of which only make sense on a file and
//! others only on a directory. The 5.1 table tells them apart, and the
//! checker refuses a permission requested on a node type that does not
//! carry it: granting `read` on a directory would make no sense, and
//! granting it silently would mask a caller error.
//!
//! Each node carries **three** masks -- owner, group, others -- rather
//! than one. The specification does not say so explicitly, but it
//! describes roles and a transferable ownership (`own`, `chown`): with no
//! access classes, those two notions would have no effect.
//!
//! ## The two-level rule
//!
//! `execute` is the only permission demanding **two** checks: on the file
//! and on the directory containing it (spec 5.1). A directory with no
//! `execute` therefore forbids launching anything at all from it,
//! whatever the rights of the files. That is what will let
//! `/users/{user}/home/download/` hold a binary without a double click
//! being enough to launch it.

use alloc::string::String;
use alloc::vec::Vec;

// ============================================================
//  4.5.1 -- The thirteen permissions (spec 5.1)
// ============================================================

/// Permission mask. One bit per entry of the 5.1 table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Permissions(pub u64);

impl Permissions {
    pub const NONE: Permissions = Permissions(0);

    /// Read the content of a file.
    pub const READ: u64 = 1 << 0;
    /// Create a new file or directory.
    pub const WRITE: u64 = 1 << 1;
    /// Execute -- both levels required, file **and** directory.
    pub const EXECUTE: u64 = 1 << 2;
    /// Replace an existing file. Distinct from `write`, which only covers
    /// creation: an editor may have the right to create without having
    /// the right to overwrite.
    pub const OVERWRITE: u64 = 1 << 3;
    pub const DELETE: u64 = 1 << 4;
    pub const RENAME: u64 = 1 << 5;
    /// See the content of a directory.
    pub const ACCESS: u64 = 1 << 6;
    /// Create a virtual link to this node (spec 4.1).
    pub const LINK: u64 = 1 << 7;
    /// Move -- also demands `write` on the destination directory.
    pub const MOVE: u64 = 1 << 8;
    /// Copy -- also demands `write` on the destination directory.
    pub const COPY: u64 = 1 << 9;
    pub const CHMOD: u64 = 1 << 10;
    /// Transfer ownership.
    pub const OWN: u64 = 1 << 11;
    /// Read the access logs -- ADMIN only.
    pub const AUDIT: u64 = 1 << 12;

    pub const ALL: u64 = Self::READ
        | Self::WRITE
        | Self::EXECUTE
        | Self::OVERWRITE
        | Self::DELETE
        | Self::RENAME
        | Self::ACCESS
        | Self::LINK
        | Self::MOVE
        | Self::COPY
        | Self::CHMOD
        | Self::OWN
        | Self::AUDIT;

    /// Permissions that make sense on a file ("File" column).
    pub const FILE_APPLICABLE: u64 = Self::READ
        | Self::WRITE
        | Self::EXECUTE
        | Self::OVERWRITE
        | Self::DELETE
        | Self::RENAME
        | Self::LINK
        | Self::MOVE
        | Self::COPY
        | Self::CHMOD
        | Self::OWN
        | Self::AUDIT;

    /// Permissions that make sense on a directory ("Directory" column).
    pub const DIR_APPLICABLE: u64 = Self::WRITE
        | Self::EXECUTE
        | Self::DELETE
        | Self::RENAME
        | Self::ACCESS
        | Self::LINK
        | Self::MOVE
        | Self::COPY
        | Self::CHMOD
        | Self::OWN
        | Self::AUDIT;

    pub const fn contains(&self, permission: u64) -> bool {
        self.0 & permission == permission && permission != 0
    }

    pub const fn union(&self, other: u64) -> Permissions {
        Permissions(self.0 | other)
    }

    pub const fn without(&self, other: u64) -> Permissions {
        Permissions(self.0 & !other)
    }

    /// Bits outside the thirteen defined. A mask containing any is rejected.
    pub const fn unknown_bits(&self) -> u64 {
        self.0 & !Self::ALL
    }

    pub fn names(&self) -> [(&'static str, bool); 13] {
        [
            ("read", self.contains(Self::READ)),
            ("write", self.contains(Self::WRITE)),
            ("execute", self.contains(Self::EXECUTE)),
            ("overwrite", self.contains(Self::OVERWRITE)),
            ("delete", self.contains(Self::DELETE)),
            ("rename", self.contains(Self::RENAME)),
            ("access", self.contains(Self::ACCESS)),
            ("link", self.contains(Self::LINK)),
            ("move", self.contains(Self::MOVE)),
            ("copy", self.contains(Self::COPY)),
            ("chmod", self.contains(Self::CHMOD)),
            ("own", self.contains(Self::OWN)),
            ("audit", self.contains(Self::AUDIT)),
        ]
    }

    /// Compact representation, in the order of the 5.1 table.
    pub fn compact(&self) -> String {
        const LETTERS: [char; 13] = [
            'r', 'w', 'x', 'o', 'd', 'n', 'a', 'l', 'm', 'c', 'p', 'q', 'u',
        ];
        let mut text = String::new();
        for (index, (_, set)) in self.names().iter().enumerate() {
            text.push(if *set { LETTERS[index] } else { '-' });
        }
        text
    }
}

/// Readable name of a single permission -- for refusal messages.
pub fn permission_name(permission: u64) -> &'static str {
    match permission {
        Permissions::READ => "read",
        Permissions::WRITE => "write",
        Permissions::EXECUTE => "execute",
        Permissions::OVERWRITE => "overwrite",
        Permissions::DELETE => "delete",
        Permissions::RENAME => "rename",
        Permissions::ACCESS => "access",
        Permissions::LINK => "link",
        Permissions::MOVE => "move",
        Permissions::COPY => "copy",
        Permissions::CHMOD => "chmod",
        Permissions::OWN => "own",
        Permissions::AUDIT => "audit",
        _ => "?",
    }
}

// ============================================================
//  4.5.3 -- Roles (spec 5.2)
// ============================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Role {
    /// Internal system account. Never used directly: it exists so that
    /// the kernel can act without being blocked by its own rules.
    Root = 0,
    /// Installing applications, modifying the system.
    Admin = 1,
    /// Access to one's own `/users/{user}/` and to the common applications.
    User = 2,
    /// Temporary session, erased at logout.
    Guest = 3,
    /// Internal role of the system processes (`.dsa`).
    Service = 4,
}

impl Role {
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Role::Root),
            1 => Some(Role::Admin),
            2 => Some(Role::User),
            3 => Some(Role::Guest),
            4 => Some(Role::Service),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Root => "ROOT",
            Role::Admin => "ADMIN",
            Role::User => "USER",
            Role::Guest => "GUEST",
            Role::Service => "SERVICE",
        }
    }

    /// Does the role override the masks of the node?
    ///
    /// Only ROOT does so unreservedly. ADMIN has a "near-total" power
    /// (spec 5.2): it bypasses the masks, but not the two-level rule on
    /// `execute` -- that one protects the user from themselves as much as
    /// it protects the system, and an administrator launching a downloaded
    /// binary by mistake is still an administrator.
    pub fn bypasses_masks(&self) -> bool {
        matches!(self, Role::Root | Role::Admin)
    }

    /// May the role read the audit logs (spec 5.1)?
    pub fn may_audit(&self) -> bool {
        matches!(self, Role::Root | Role::Admin)
    }

    /// A GUEST keeps nothing: its session is erased at logout, and it
    /// never writes outside its own space.
    pub fn is_ephemeral(&self) -> bool {
        matches!(self, Role::Guest)
    }
}

// ============================================================
//  Users
// ============================================================

pub type Uid = u32;
pub type Gid = u32;

/// Identifier of the system account.
pub const ROOT_UID: Uid = 0;
/// Group the system processes belong to.
pub const SYSTEM_GID: Gid = 0;
/// Group of the human accounts.
pub const USERS_GID: Gid = 100;

#[derive(Debug, Clone)]
pub struct Account {
    pub uid: Uid,
    pub gid: Gid,
    pub name: String,
    pub role: Role,
}

/// Access context: who asks, in what capacity.
///
/// Passed to every filesystem operation. In phase 5.6.1 it will replace a
/// lookup of the calling process -- for now the caller supplies it
/// explicitly, which makes the rules testable without having to create a
/// process per case.
#[derive(Debug, Clone, Copy)]
pub struct Credentials {
    pub uid: Uid,
    pub gid: Gid,
    pub role: Role,
}

impl Credentials {
    pub const fn new(uid: Uid, gid: Gid, role: Role) -> Self {
        Credentials { uid, gid, role }
    }

    /// Kernel context: passes everywhere, by construction.
    pub const fn kernel() -> Self {
        Credentials::new(ROOT_UID, SYSTEM_GID, Role::Root)
    }
}

/// Table of the accounts. Minimal as long as `SYS_USER_*` (phase 5.6.1)
/// does not exist: it serves to give meaning to the uids stored in the
/// nodes, not to manage an authentication.
pub struct AccountTable {
    accounts: Vec<Account>,
}

impl AccountTable {
    pub const fn new() -> Self {
        AccountTable {
            accounts: Vec::new(),
        }
    }

    pub fn insert(&mut self, uid: Uid, gid: Gid, name: &str, role: Role) {
        self.accounts.retain(|a| a.uid != uid);
        self.accounts.push(Account {
            uid,
            gid,
            name: String::from(name),
            role,
        });
    }

    /// Removes an account. Returns `false` if it did not exist.
    pub fn remove(&mut self, uid: Uid) -> bool {
        let before = self.accounts.len();
        self.accounts.retain(|a| a.uid != uid);
        self.accounts.len() != before
    }

    pub fn find(&self, uid: Uid) -> Option<&Account> {
        self.accounts.iter().find(|a| a.uid == uid)
    }

    pub fn find_by_name(&self, name: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.name == name)
    }

    pub fn credentials(&self, uid: Uid) -> Option<Credentials> {
        self.find(uid)
            .map(|a| Credentials::new(a.uid, a.gid, a.role))
    }

    pub fn iter(&self) -> core::slice::Iter<'_, Account> {
        self.accounts.iter()
    }

    pub fn len(&self) -> usize {
        self.accounts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }
}

impl Default for AccountTable {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================
//  Usual permission sets
// ============================================================

/// Rights of a node as it has just been created.
#[derive(Debug, Clone, Copy)]
pub struct Mode {
    pub owner: Permissions,
    pub group: Permissions,
    pub other: Permissions,
}

impl Mode {
    pub const fn new(owner: u64, group: u64, other: u64) -> Self {
        Mode {
            owner: Permissions(owner),
            group: Permissions(group),
            other: Permissions(other),
        }
    }

    /// Ordinary file of a user: they alone touch it.
    pub const fn private_file() -> Self {
        Mode::new(
            Permissions::READ
                | Permissions::WRITE
                | Permissions::OVERWRITE
                | Permissions::DELETE
                | Permissions::RENAME
                | Permissions::LINK
                | Permissions::MOVE
                | Permissions::COPY
                | Permissions::CHMOD
                | Permissions::OWN,
            0,
            0,
        )
    }

    /// Home directory: its owner does everything, nobody else even sees
    /// the content.
    pub const fn private_dir() -> Self {
        Mode::new(
            Permissions::WRITE
                | Permissions::EXECUTE
                | Permissions::DELETE
                | Permissions::RENAME
                | Permissions::ACCESS
                | Permissions::LINK
                | Permissions::MOVE
                | Permissions::COPY
                | Permissions::CHMOD
                | Permissions::OWN,
            0,
            0,
        )
    }

    /// System directory: readable and traversable by all, modifiable by
    /// the owner alone.
    pub const fn system_dir() -> Self {
        Mode::new(
            Permissions::WRITE
                | Permissions::EXECUTE
                | Permissions::DELETE
                | Permissions::RENAME
                | Permissions::ACCESS
                | Permissions::LINK
                | Permissions::MOVE
                | Permissions::COPY
                | Permissions::CHMOD
                | Permissions::OWN,
            Permissions::ACCESS | Permissions::EXECUTE,
            Permissions::ACCESS | Permissions::EXECUTE,
        )
    }

    /// System binary: executable by all, modifiable by nobody but its
    /// owner.
    pub const fn system_binary() -> Self {
        Mode::new(
            Permissions::READ
                | Permissions::EXECUTE
                | Permissions::OVERWRITE
                | Permissions::DELETE
                | Permissions::RENAME
                | Permissions::CHMOD
                | Permissions::OWN,
            Permissions::READ | Permissions::EXECUTE,
            Permissions::READ | Permissions::EXECUTE,
        )
    }

    /// System file readable by all, modifiable by its owner alone --
    /// configuration, themes, module descriptors.
    ///
    /// Distinct from `system_binary`: a configuration file is not
    /// executable, and giving `EXECUTE` to everybody on a `.cfg` would
    /// open the door of 5.1 to content that is not code.
    pub const fn system_file() -> Self {
        Mode::new(
            Permissions::READ
                | Permissions::WRITE
                | Permissions::OVERWRITE
                | Permissions::DELETE
                | Permissions::RENAME
                | Permissions::CHMOD
                | Permissions::OWN,
            Permissions::READ,
            Permissions::READ,
        )
    }

    /// File dropped in a shared directory (`/cache/`, `/ras/`,
    /// `/users/_shared/`).
    ///
    /// The counterpart of `shared_dir` at file level. Without it, a file
    /// created in a directory open to the group was born with the rights
    /// of a private file: the directory accepted writes, and the file
    /// written became at once untouchable for any account but its
    /// creator. A shared directory whose content is not shared shares
    /// nothing.
    ///
    /// No `EXECUTE`: a directory open for writing to the group is exactly
    /// the place where a binary dropped by anybody must not become
    /// executable for everybody.
    pub const fn shared_file() -> Self {
        Mode::new(
            Permissions::READ
                | Permissions::WRITE
                | Permissions::OVERWRITE
                | Permissions::DELETE
                | Permissions::RENAME
                | Permissions::LINK
                | Permissions::MOVE
                | Permissions::COPY
                | Permissions::CHMOD
                | Permissions::OWN,
            Permissions::READ | Permissions::WRITE | Permissions::OVERWRITE | Permissions::COPY,
            Permissions::READ | Permissions::COPY,
        )
    }

    /// Directory shared between users (`/users/_shared/`).
    pub const fn shared_dir() -> Self {
        Mode::new(
            Permissions::WRITE
                | Permissions::EXECUTE
                | Permissions::DELETE
                | Permissions::RENAME
                | Permissions::ACCESS
                | Permissions::LINK
                | Permissions::MOVE
                | Permissions::COPY
                | Permissions::CHMOD
                | Permissions::OWN,
            Permissions::WRITE | Permissions::ACCESS | Permissions::EXECUTE | Permissions::COPY,
            Permissions::ACCESS | Permissions::EXECUTE | Permissions::COPY,
        )
    }
}

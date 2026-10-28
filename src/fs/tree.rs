//! 4.4 -- Creation of the system and user tree (spec 17).
//!
//! Run once only, on the first boot of a fresh volume. The content comes
//! straight from the 4.3 table and the 17 tree: the order of the
//! directories is reproduced as-is, with no interpretation.
//!
//! The rights are not uniform, and that is the important point. Three
//! families:
//!
//! * `/sys/`, `/sysapp/`, `/libs/` -- traversable and readable by all,
//!   modifiable by the system owner alone. That is what gives its meaning
//!   to the `SYS_DIRECTORY` permission declared by a binary (12.3);
//! * `/users/{user}/` -- the owner does everything, nobody else sees the
//!   content;
//! * `/users/_shared/`, `/cache/`, `/ras/` -- writing open to the group,
//!   because their reason for existing is precisely to hold what several
//!   accounts drop there.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::perm::{Credentials, Gid, Mode, Role, SYSTEM_GID, Uid, USERS_GID};
use super::filesystem::Filesystem;
use super::FsError;

/// System directories, in the order of spec 17.
const SYSTEM_DIRECTORIES: &[&str] = &[
    "/sysapp",
    "/sys",
    "/sys/kernel",
    "/sys/kernel/modules",
    // Spec 3.6: the signed GPU driver modules (.kmd) of vendors.
    "/sys/kernel/modules/gpu",
    "/sys/drivers",
    "/sys/config",
    "/sys/gui",
    "/sys/gui/modules",
    "/sys/gui/modules/boot",
    "/sys/gui/modules/accessibility",
    "/sys/gui/fonts",
    "/sys/gui/themes",
    "/sys/i18n",
    "/sys/runtimes",
    "/sys/tools",
    "/sys/tools/spm",
    "/sys/tools/sdk",
    "/sys/tools/etr",
    "/sys/tools/interpret",
    "/sys/tools/compile",
    "/sys/updates",
    "/sys/updates/rollback",
    "/games",
    "/apps",
    "/libs",
    "/libs/sys",
    "/libs/shared",
    "/logs",
    "/logs/kernel",
    "/logs/drivers",
    "/logs/apps",
    "/net",
    "/users",
];

/// Directories open for writing to the group: their reason for existing
/// is to hold what several accounts drop there.
const SHARED_DIRECTORIES: &[&str] = &["/cache", "/ras", "/users/_shared"];

/// Sub-directories of a user account (spec 17).
const USER_DIRECTORIES: &[&str] = &[
    "apps",
    "settings",
    "settings/permissions",
    // 8.4: the modules and the theme of an account win over those of the
    // system. The directories exist from the creation of the account,
    // failing which the user would have to create them before being able
    // to customise anything at all.
    "settings/gui",
    "settings/gui/modules",
    "settings/gui/modules/boot",
    "cache",
    "runtimes",
    "logs",
    "home",
    "home/desktop",
    "home/downloads",
    "home/archive",
    "home/images",
    "home/videos",
    "home/documents",
    "home/executables",
];

/// 4.4.1 -- Creates the system tree.
///
/// Returns the number of directories created.
pub fn create_system_tree(volume: &mut dyn Filesystem, now: u64) -> Result<usize, FsError> {
    let mut created = 0;

    for path in SYSTEM_DIRECTORIES {
        created += usize::from(enforce(volume, path, Mode::system_dir(), 0, SYSTEM_GID, now)?);
    }
    for path in SHARED_DIRECTORIES {
        created += usize::from(enforce(volume, path, Mode::shared_dir(), 0, USERS_GID, now)?);
    }

    Ok(created)
}

/// Creates a directory of the tree if it is missing, and in every case
/// brings its rights and its owner back to what the spec says.
///
/// The pass runs at every boot, on volumes this kernel did not
/// necessarily format: `tools/nova-fs.py` writes the Linux root of the
/// browser into the image from the host, and creates the parents it
/// needs along the way. Creating only what is missing is not enough --
/// a `/users/` made as a mere parent carries the rights of a parent, and
/// then no account can traverse it. Spec 17 fixes these rights; they are
/// therefore imposed, not proposed.
///
/// Returns true when the directory did not exist.
fn enforce(
    volume: &mut dyn Filesystem,
    path: &str,
    mode: Mode,
    uid: Uid,
    gid: Gid,
    now: u64,
) -> Result<bool, FsError> {
    let kernel = Credentials::kernel();
    let existing = volume.resolve(path, kernel).ok();
    let fresh = existing.is_none();
    volume.create_path(path, kernel, mode, now)?;

    let aligned = existing.is_some_and(|node| {
        node.perm_owner == mode.owner
            && node.perm_group == mode.group
            && node.perm_other == mode.other
            && node.owner == uid
            && node.group == gid
    });
    if !aligned {
        volume.chmod(path, mode, kernel, now)?;
        volume.chown(path, uid, gid, kernel, now)?;
    }
    Ok(fresh)
}

/// 4.4.2 -- Creates the tree of a user account.
pub fn create_user_tree(
    volume: &mut dyn Filesystem,
    user: &str,
    uid: Uid,
    gid: Gid,
    role: Role,
    now: u64,
) -> Result<usize, FsError> {
    let root = format!("/users/{}", user);

    // The head directory is created by the kernel then given to the
    // account: somebody with the right to write into `/users/` has to
    // make it, and it is not the user yet.
    let mut created = usize::from(enforce(volume, &root, Mode::private_dir(), uid, gid, now)?);

    let owner = Credentials::new(uid, gid, role);
    for suffix in USER_DIRECTORIES {
        let path = format!("{}/{}", root, suffix);
        created += usize::from(enforce(volume, &path, Mode::private_dir(), uid, gid, now)?);
    }

    // An empty preferences file, so that the account has something of its
    // own from the first boot.
    let settings = format!("{}/settings/user.cfg", root);
    let content = format!(
        "# Preferences de {}\nrole={}\nuid={}\nlangue=fr-FR\n",
        user,
        role.as_str(),
        uid
    );
    volume.write_file(
        &settings,
        content.as_bytes(),
        owner,
        Mode::private_file(),
        now,
    )?;

    Ok(created)
}

/// Paths spec 12.4 associates with each binary type.
///
/// The sandbox of 12.4 is expressed in terms of paths: a `.gs` sees only
/// `/games/{app}/` and the home of the user, a `.ras` only `/ras/{app}/`.
/// The table is here rather than in `binfmt` because it describes a tree,
/// not a file format.
pub fn sandbox_roots(format: crate::binfmt::header::BinaryFormat, app_id: &str) -> Vec<String> {
    use crate::binfmt::header::BinaryFormat;

    match format {
        // Total trust: complete access.
        BinaryFormat::Dsa => alloc::vec![String::from("/")],
        // Extended by declaration -- the system root stays open to it.
        BinaryFormat::Ss => alloc::vec![
            String::from("/sys"),
            String::from("/sysapp"),
            String::from("/libs"),
            String::from("/users"),
        ],
        BinaryFormat::Gs => alloc::vec![format!("/games/{}", app_id)],
        BinaryFormat::Cus => alloc::vec![format!("/apps/{}", app_id)],
        // Total isolation: nothing but its own directory.
        BinaryFormat::Ras => alloc::vec![format!("/ras/{}", app_id)],
    }
}

/// Creates the sandbox directory of an application (spec 12.4).
pub fn create_sandbox(
    volume: &mut dyn Filesystem,
    format: crate::binfmt::header::BinaryFormat,
    app_id: &str,
    now: u64,
) -> Result<String, FsError> {
    use crate::binfmt::header::BinaryFormat;

    let base = match format {
        BinaryFormat::Gs => format!("/games/{}", app_id),
        BinaryFormat::Cus => format!("/apps/{}", app_id),
        BinaryFormat::Ras => format!("/ras/{}", app_id),
        BinaryFormat::Ss | BinaryFormat::Dsa => format!("/sysapp/{}", app_id),
    };

    let kernel = Credentials::kernel();
    let mode = match format {
        BinaryFormat::Ras => Mode::private_dir(),
        _ => Mode::system_dir(),
    };
    volume.create_path(&base, kernel, mode, now)?;
    Ok(base)
}

/// Group assigned by default to a role.
pub fn default_gid(role: Role) -> Gid {
    match role {
        Role::Root | Role::Service => SYSTEM_GID,
        _ => USERS_GID,
    }
}

/// Reduced tree of a secondary disk.
///
/// A secondary volume receives neither `/sys/` nor `/users/`: there is
/// only one system, and duplicating its directories would suggest a
/// second exists. Only the content locations are created.
pub fn create_system_tree_minimal(volume: &mut dyn Filesystem, now: u64) -> Result<usize, FsError> {
    let kernel = Credentials::kernel();
    let mut created = 0;

    for path in ["/apps", "/games", "/libs"] {
        volume.create_path(path, kernel, Mode::system_dir(), now)?;
        created += 1;
    }
    for path in ["/cache", "/echange"] {
        volume.create_path(path, kernel, Mode::shared_dir(), now)?;
        created += 1;
    }

    Ok(created)
}

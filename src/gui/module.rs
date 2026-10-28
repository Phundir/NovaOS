//! 7.3 -- `.mod` format, loader and life cycle (spec 8.2-8.3).
//!
//! ## Why a separate format
//!
//! 3.3 gives five magic numbers, one per application type. A GUI module
//! is none of them: it does not launch, it **occupies a slot** at the
//! compositor, and it carries two pieces of information no application
//! has -- which slot, and whether it is replaceable. Sticking the binary
//! header on it would mean adding a sixth format to a hierarchy the spec
//! declares closed.
//!
//! `.mod` therefore has its own 128-byte header, decoded field by field
//! like everything coming from a file.
//!
//! ```text
//!   0x00  4  magic "MOD\0"
//!   0x04  2  format version
//!   0x06  1  slot (see Slot)
//!   0x07  1  signature type: 1 SELF, 2 ENTERPRISE, 3 SYSTEM
//!   0x08  4  flags: bit 0 replaceable
//!   0x0C  4  length of the payload
//!   0x10 24  app_id
//!   0x28 24  display name
//!   0x40 24  author
//!   0x58  8  version: major, minor, patch, build (u16 each)
//!   0x60 32  SHA-256 digest of the payload
//!   0x80 --  payload
//! ```
//!
//! ## Signature (7.3.2)
//!
//! The digest of the payload is recomputed and compared with the one in
//! the header: that is what detects an alteration. The **level** of the
//! signature decides what is accepted:
//!
//! * SYSTEM: the digest must be in the kernel store, the very one phase
//!   3.3 feeds. A system module does not declare itself, it is
//!   recognised.
//! * ENTERPRISE: accepted, with the same reservation as in phase 3 -- the
//!   chain is not a proof yet, for want of an asymmetric signature.
//! * SELF: accepted for a replaceable slot only. A self-signed module has
//!   no business in a non-replaceable slot, and `compositor_core` is one.
//!
//! ## Isolation (7.3.3)
//!
//! Every module runs in its own task, with its own stack. 8.3 demands
//! that "the crash of a module does not block the OS": it does not, a
//! task can die without carrying off the compositor, which observes its
//! absence and frees the slot. **Memory** isolation stays conventional as
//! long as there is a single CR3 -- a limit opened by phase 3, and one
//! that holds here as elsewhere.
//!
//! ## Pause (7.3.4)
//!
//! Renaming `name.mod` to `name.pause` deregisters the module at the next
//! sweep. The file stays, the slot frees up. Renaming the other way
//! brings it back. No state is held anywhere but in the name of the file:
//! that is what makes the operation safe even if the compositor restarts
//! meanwhile.

use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::header::SignatureType;
use crate::binfmt::sha256;
use crate::fs::perm::Credentials;

/// Size of the `.mod` header.
pub const HEADER_SIZE: usize = 128;

/// Magic of a GUI module.
pub const MAGIC: [u8; 4] = [0x4D, 0x4F, 0x44, 0x00]; // "MOD\0"

/// Format version produced and accepted by this kernel.
pub const FORMAT_VERSION: u16 = 1;

/// Directory of the system modules (spec 17).
pub const SYSTEM_DIR: &str = "/sys/gui/modules";
/// Directory of the boot modules.
pub const BOOT_DIR: &str = "/sys/gui/modules/boot";
/// Directory of the accessibility modules.
pub const ACCESSIBILITY_DIR: &str = "/sys/gui/modules/accessibility";
/// Directory of the modules of an account, relative to `/users/{user}/`.
pub const USER_SUBDIR: &str = "settings/gui/modules";

/// Flags of the header.
pub mod flags {
    /// The slot accepts a replacement (8.2, "Replaceable" column).
    pub const REPLACEABLE: u32 = 1 << 0;
    /// The module is loaded before the login screen (2.3).
    pub const BOOT: u32 = 1 << 1;
    /// The module belongs to accessibility (16.2).
    pub const ACCESSIBILITY: u32 = 1 << 2;
}

// ============================================================
//  Slots (spec 8.2)
// ============================================================

/// The slots of the 8.2 table, plus those of 2.3 and 16.2.
///
/// A slot holds only one module at a time: that is what gives meaning to
/// "replaceable". `Core` is the only one that is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Slot {
    /// `compositor_core.mod` -- not replaceable.
    Core = 0,
    Taskbar = 1,
    WindowManager = 2,
    Notifications = 3,
    Wallpaper = 4,
    Launcher = 5,
    Overlay = 6,
    /// Boot splash (2.3).
    Splash = 7,
    /// Accessibility coordinator (16.2).
    Accessibility = 8,
    // 16.2 lists five accessibility modules and marks all five
    // replaceable. A slot holds one module at a time -- that is what
    // gives "replaceable" its meaning -- so four are added beside the
    // coordinator rather than making them share one. 8.2 freezes its own
    // column; it does not forbid the slots the rest of the specification
    // asks for, and `Splash` set that precedent in 7.7.
    /// `screenreader.mod` (16.2).
    ScreenReader = 9,
    /// `zoom.mod` (16.2).
    Zoom = 10,
    /// `contrast.mod` (16.2).
    Contrast = 11,
    /// `keyboard-nav.mod` (16.2).
    KeyboardNav = 12,
    /// Slot reserved for diagnostics -- no native module claims it.
    ///
    /// The phase 7.3.4 life-cycle test lays a real `.mod` on the disk and
    /// watches the loader pick it up, pause it and drop it. Doing that on
    /// a slot a native occupies would switch off a real module during the
    /// test; doing it on a slot nothing occupies exercises the loader
    /// with nothing visible depending on the outcome. The test used the
    /// `Accessibility` slot for exactly that reason, and step 10.1 gave
    /// that slot a real occupant.
    ///
    /// A slot that exists for a test is a cost, and the alternative was
    /// worse: a test that pauses the screen reader to check that pausing
    /// works.
    Diagnostic = 13,
}

/// Number of slots, to size the table.
pub const SLOT_COUNT: usize = 14;

impl Slot {
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Slot::Core),
            1 => Some(Slot::Taskbar),
            2 => Some(Slot::WindowManager),
            3 => Some(Slot::Notifications),
            4 => Some(Slot::Wallpaper),
            5 => Some(Slot::Launcher),
            6 => Some(Slot::Overlay),
            7 => Some(Slot::Splash),
            8 => Some(Slot::Accessibility),
            9 => Some(Slot::ScreenReader),
            10 => Some(Slot::Zoom),
            11 => Some(Slot::Contrast),
            12 => Some(Slot::KeyboardNav),
            13 => Some(Slot::Diagnostic),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Slot::Core => "Core",
            Slot::Taskbar => "Taskbar",
            Slot::WindowManager => "WindowManager",
            Slot::Notifications => "Notifications",
            Slot::Wallpaper => "Wallpaper",
            Slot::Launcher => "Launcher",
            Slot::Overlay => "Overlay",
            Slot::Splash => "Splash",
            Slot::Accessibility => "Accessibility",
            Slot::ScreenReader => "ScreenReader",
            Slot::Zoom => "Zoom",
            Slot::Contrast => "Contrast",
            Slot::KeyboardNav => "KeyboardNav",
            Slot::Diagnostic => "Diagnostic",
        }
    }

    /// 8.2 freezes this column: only the core of the compositor is not
    /// replaceable. The header flag cannot contradict it -- that is
    /// checked at load time, not assumed.
    pub fn replaceable(&self) -> bool {
        *self != Slot::Core
    }
}

// ============================================================
//  Header
// ============================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleHeader {
    pub version: u16,
    pub slot: Slot,
    pub signature: SignatureType,
    pub flags: u32,
    pub payload_len: u32,
    pub app_id: String,
    pub display_name: String,
    pub author: String,
    pub module_version: [u16; 4],
    pub digest: [u8; 32],
}

impl ModuleHeader {
    pub fn is_replaceable(&self) -> bool {
        self.flags & flags::REPLACEABLE != 0
    }

    pub fn version_string(&self) -> String {
        alloc::format!(
            "{}.{}.{}+{}",
            self.module_version[0],
            self.module_version[1],
            self.module_version[2],
            self.module_version[3]
        )
    }
}

/// Refusal reasons. A refused module is named and its reason logged: an
/// empty slot with no explanation is the worst of diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleError {
    TooShort,
    BadMagic,
    BadVersion,
    UnknownSlot,
    UnknownSignature,
    PayloadMismatch,
    DigestMismatch,
    NotOfficial,
    SelfSignedCore,
    SlotContradiction,
}

impl ModuleError {
    pub fn message(&self) -> &'static str {
        match self {
            ModuleError::TooShort => "fichier plus court qu'un en-tete",
            ModuleError::BadMagic => "magic absent - ce n'est pas un .mod",
            ModuleError::BadVersion => "version de format non geree",
            ModuleError::UnknownSlot => "emplacement inconnu",
            ModuleError::UnknownSignature => "type de signature inconnu",
            ModuleError::PayloadMismatch => "longueur de charge utile incoherente",
            ModuleError::DigestMismatch => "empreinte invalide - charge utile alteree",
            ModuleError::NotOfficial => "signature SYSTEM sans empreinte au magasin",
            ModuleError::SelfSignedCore => "auto-signe dans un emplacement non remplacable",
            ModuleError::SlotContradiction => "drapeau remplacable contraire au tableau 8.2",
        }
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn read_name(bytes: &[u8], offset: usize, length: usize) -> String {
    bytes[offset..offset + length]
        .iter()
        .copied()
        .take_while(|byte| *byte != 0)
        .map(|byte| if (0x20..=0x7e).contains(&byte) { byte as char } else { '?' })
        .collect()
}

/// Parses a `.mod` file without loading anything.
pub fn parse(data: &[u8]) -> Result<ModuleHeader, ModuleError> {
    if data.len() < HEADER_SIZE {
        return Err(ModuleError::TooShort);
    }
    if data[0..4] != MAGIC {
        return Err(ModuleError::BadMagic);
    }
    let version = read_u16(data, 0x04);
    if version != FORMAT_VERSION {
        return Err(ModuleError::BadVersion);
    }
    let slot = Slot::from_code(data[0x06]).ok_or(ModuleError::UnknownSlot)?;
    let signature =
        SignatureType::from_code(data[0x07]).ok_or(ModuleError::UnknownSignature)?;
    let flags = read_u32(data, 0x08);
    let payload_len = read_u32(data, 0x0C);

    if HEADER_SIZE + payload_len as usize != data.len() {
        return Err(ModuleError::PayloadMismatch);
    }

    // The flag and the 8.2 table must say the same thing. A file
    // declaring itself replaceable in the Core slot is not "almost
    // right": it lies about what it is.
    if (flags & flags::REPLACEABLE != 0) != slot.replaceable() {
        return Err(ModuleError::SlotContradiction);
    }

    let mut digest = [0u8; 32];
    digest.copy_from_slice(&data[0x60..0x80]);

    let computed = sha256::digest(&data[HEADER_SIZE..]);
    if !sha256::equal(&computed, &digest) {
        return Err(ModuleError::DigestMismatch);
    }

    Ok(ModuleHeader {
        version,
        slot,
        signature,
        flags,
        payload_len,
        app_id: read_name(data, 0x10, 24),
        display_name: read_name(data, 0x28, 24),
        author: read_name(data, 0x40, 24),
        module_version: [
            read_u16(data, 0x58),
            read_u16(data, 0x5A),
            read_u16(data, 0x5C),
            read_u16(data, 0x5E),
        ],
        digest,
    })
}

/// 7.3.2 -- Checks that the signature level allows this load.
///
/// Separate from `parse` on purpose: parsing a file and deciding to trust
/// it are two questions, and an offline tool must be able to ask the
/// first without the trust store of the kernel.
pub fn authorize(header: &ModuleHeader) -> Result<(), ModuleError> {
    match header.signature {
        SignatureType::System => {
            if crate::binfmt::signature::is_official(&header.digest) {
                Ok(())
            } else {
                Err(ModuleError::NotOfficial)
            }
        }
        SignatureType::Enterprise => Ok(()),
        // A module with no signature at all is treated as a self-signed
        // one: 8.3 does not provide for an anonymous module, and refusing
        // it in a non-replaceable slot is the same rule for the same
        // reason.
        SignatureType::SelfSigned | SignatureType::None => {
            if header.slot.replaceable() {
                Ok(())
            } else {
                Err(ModuleError::SelfSignedCore)
            }
        }
    }
}

// ============================================================
//  Building a .mod file
// ============================================================

/// Build parameters of a module.
pub struct BuildSpec<'a> {
    pub slot: Slot,
    pub signature: SignatureType,
    pub app_id: &'a str,
    pub display_name: &'a str,
    pub author: &'a str,
    pub version: [u16; 4],
    pub extra_flags: u32,
    pub payload: &'a [u8],
}

fn put_name(buffer: &mut [u8], offset: usize, length: usize, text: &str) {
    for (index, byte) in text.bytes().take(length - 1).enumerate() {
        buffer[offset + index] = byte;
    }
}

/// Assembles a complete `.mod` file.
///
/// The `REPLACEABLE` flag is not a parameter: it follows from the 8.2
/// table. Letting the caller choose it would allow building the
/// contradictory file `parse` refuses -- useful for a test, but then it
/// is the test that must corrupt the byte, not the builder that must
/// offer to do it.
pub fn build(spec: &BuildSpec) -> Vec<u8> {
    let mut file = alloc::vec![0u8; HEADER_SIZE + spec.payload.len()];

    file[0..4].copy_from_slice(&MAGIC);
    file[0x04..0x06].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    file[0x06] = spec.slot as u8;
    file[0x07] = spec.signature as u8;

    let mut header_flags = spec.extra_flags;
    if spec.slot.replaceable() {
        header_flags |= flags::REPLACEABLE;
    }
    file[0x08..0x0C].copy_from_slice(&header_flags.to_le_bytes());
    file[0x0C..0x10].copy_from_slice(&(spec.payload.len() as u32).to_le_bytes());

    put_name(&mut file, 0x10, 24, spec.app_id);
    put_name(&mut file, 0x28, 24, spec.display_name);
    put_name(&mut file, 0x40, 24, spec.author);
    for (index, part) in spec.version.iter().enumerate() {
        file[0x58 + index * 2..0x5A + index * 2].copy_from_slice(&part.to_le_bytes());
    }

    file[HEADER_SIZE..].copy_from_slice(spec.payload);
    let digest = sha256::digest(spec.payload);
    file[0x60..0x80].copy_from_slice(&digest);

    file
}

/// Digest of the payload of an already assembled file.
pub fn payload_digest(file: &[u8]) -> Option<[u8; 32]> {
    if file.len() < HEADER_SIZE {
        return None;
    }
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&file[0x60..0x80]);
    Some(digest)
}

// ============================================================
//  Directory sweep (7.3.1)
// ============================================================

/// State of a module as the loader sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// In service.
    Active,
    /// File renamed to `.pause` (8.3).
    Paused,
    /// Parsing or signature refused.
    Rejected,
    /// A module of better priority already occupies the slot.
    Shadowed,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Active => "actif",
            State::Paused => "en pause",
            State::Rejected => "refuse",
            State::Shadowed => "masque",
        }
    }
}

/// Provenance of a module -- decides its priority (8.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    /// `/sys/gui/modules/`
    System = 0,
    /// `/users/{user}/settings/gui/modules/` -- wins.
    User = 1,
}

impl Origin {
    pub fn as_str(&self) -> &'static str {
        match self {
            Origin::System => "systeme",
            Origin::User => "utilisateur",
        }
    }
}

/// A module discovered by the sweep.
#[derive(Debug, Clone)]
pub struct Found {
    pub path: String,
    pub file_name: String,
    pub origin: Origin,
    pub state: State,
    pub header: Option<ModuleHeader>,
    pub error: Option<ModuleError>,
}

impl Found {
    pub fn slot(&self) -> Option<Slot> {
        self.header.as_ref().map(|header| header.slot)
    }

    pub fn app_id(&self) -> &str {
        self.header
            .as_ref()
            .map(|header| header.app_id.as_str())
            .unwrap_or("?")
    }
}

/// Sweeps a directory and parses everything in it.
///
/// The files carrying neither of the two extensions are ignored without a
/// word: `/sys/gui/modules/` may hold something else, and complaining
/// about a file that does not claim to be a module would be noise.
pub fn scan(directory: &str, origin: Origin) -> Vec<Found> {
    let mut found = Vec::new();
    if !crate::fs::is_ready() {
        return found;
    }
    let who = Credentials::kernel();
    let Ok(entries) = crate::fs::list(directory, who) else {
        return found;
    };

    for entry in entries {
        let paused = entry.name.ends_with(".pause");
        if !entry.name.ends_with(".mod") && !paused {
            continue;
        }
        let path = alloc::format!("{}/{}", directory, entry.name);

        let (header, error) = match crate::fs::read_file(&path, who) {
            Ok(data) => match parse(&data) {
                Ok(header) => match authorize(&header) {
                    Ok(()) => (Some(header), None),
                    Err(error) => (Some(header), Some(error)),
                },
                Err(error) => (None, Some(error)),
            },
            // An unreadable file is not an invalid file: the distinction
            // matters to whoever is looking for why their module does not
            // load.
            Err(_) => (None, Some(ModuleError::TooShort)),
        };

        let state = if error.is_some() {
            State::Rejected
        } else if paused {
            State::Paused
        } else {
            State::Active
        };

        found.push(Found {
            path,
            file_name: entry.name,
            origin,
            state,
            header,
            error,
        });
    }

    found
}

/// 7.3.1 and 7.5.1 -- Sweeps both directories and resolves the slots.
///
/// The priority is carried by the sort, not by a chain of conditions: we
/// order by slot then by decreasing provenance, and the first of each
/// slot wins. The others go to `Shadowed`. A system module therefore
/// stays **visible and explained** instead of disappearing from the
/// diagnostics when a user module replaces it.
pub fn resolve(user: Option<&str>) -> Vec<Found> {
    let mut all = scan(SYSTEM_DIR, Origin::System);
    all.extend(scan(BOOT_DIR, Origin::System));
    all.extend(scan(ACCESSIBILITY_DIR, Origin::System));

    if let Some(user) = user {
        let base = alloc::format!("/users/{}/{}", user, USER_SUBDIR);
        all.extend(scan(&base, Origin::User));
        all.extend(scan(&alloc::format!("{}/boot", base), Origin::User));
    }

    // Ordering: slot, then decreasing provenance. `sort_by_key` is
    // stable, so two modules of the same provenance keep the order of the
    // sweep -- reproducible from one boot to the next.
    all.sort_by_key(|found| {
        (
            found.slot().map(|slot| slot as u8).unwrap_or(u8::MAX),
            core::cmp::Reverse(found.origin),
        )
    });

    let mut taken = [false; SLOT_COUNT];
    for found in all.iter_mut() {
        let Some(slot) = found.slot() else {
            continue;
        };
        if found.state != State::Active {
            continue;
        }
        if taken[slot as usize] {
            found.state = State::Shadowed;
        } else {
            taken[slot as usize] = true;
        }
    }

    all
}

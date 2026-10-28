//! GPU driver modules: signed `.kmd` files loaded at boot (spec 3.6).
//!
//! ## What a module is
//!
//! The built-in backend drives virtio-gpu; a real card needs its vendor's
//! driver. Nova does not write those (DECISIONS, "La pile graphique est à
//! Nova") but gives their authors a way in: a position-independent ELF,
//! wrapped in a `.kmd` container that carries its digest and its signature,
//! installed in `/sys/kernel/modules/gpu/`, loaded at boot before the
//! compositor. `tools/kmd/nova_kmd.h` is the whole contract.
//!
//! ## The order of the checks
//!
//! Everything that can refuse a module runs before any of its bytes is
//! executed, in this order:
//!
//! 1. the container: magic, class, **interface version equal** to the
//!    core's (no negotiation), sizes inside the file;
//! 2. the **signature**: the payload's digest, then the level -- SYSTEM
//!    (the authenticity digest is in the kernel's table), ENTERPRISE (a
//!    known authority, a binding to this very module), SELF only if an
//!    administrator authorized this exact digest in
//!    `/sys/config/modules-autorises` (with a warning at every boot);
//! 3. the **ELF**: a shared object for x86-64, no library needed, no
//!    relocation but `R_X86_64_RELATIVE`, no page both writable and
//!    executable -- the kernel's W^X holds inside modules too.
//!
//! A refusal is written in `/logs/kernel/modules.txt` and the boot goes on
//! without the module.
//!
//! ## What a module sees
//!
//! Only `KernelApi`: logging, memory, device registers, PCI, time. No
//! symbol of the kernel is linked into it, so nothing else of the kernel
//! is reachable by name -- which is not a protection (it runs in ring 0,
//! and the signature is its only barrier), but keeps the interface one
//! table long, versioned, and the same for every module.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use super::backend::{self, Command, Description, Id, ImageDesc, MemKind, ObjectKind, Window, Work, WorkState};
use crate::binfmt::sha256;
use crate::memory::{self, flags, phys_to_virt};

/// Where modules live in the kernel's address space: a PML4 entry of their
/// own (see ARCHITECTURE.md), code and data, then device registers they
/// map, in the upper half.
const MODULE_VMA: u64 = 0xFFFF_8400_0000_0000;
const MMIO_VMA: u64 = 0xFFFF_8480_0000_0000;
static NEXT_MODULE: AtomicU64 = AtomicU64::new(MODULE_VMA);
static NEXT_MMIO: AtomicU64 = AtomicU64::new(MMIO_VMA);

/// Largest module, in bytes of memory.
const MAX_IMAGE: u64 = 16 << 20;
const HEADER: usize = 256;
/// Directory of the GPU modules (spec 17).
pub const DIRECTORY: &str = "/sys/kernel/modules/gpu";
/// Digests of self-signed modules an administrator allowed, one in hex
/// per line.
pub const AUTHORIZED: &str = "/sys/config/modules-autorises";
const LOG: &str = "/logs/kernel/modules.txt";

pub type Refusal = &'static str;

// ============================================================
//  The container
// ============================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    SelfSigned,
    Enterprise,
    System,
}

pub struct Container {
    pub name: String,
    pub vendor: String,
    pub interface: u32,
    pub level: Level,
    payload: core::ops::Range<usize>,
    payload_hash: [u8; 32],
    authority: [u8; 32],
    binding: [u8; 32],
}

fn u16_at(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([data[at], data[at + 1]])
}

fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

fn u64_at(data: &[u8], at: usize) -> u64 {
    u32_at(data, at) as u64 | (u32_at(data, at + 4) as u64) << 32
}

fn i64_at(data: &[u8], at: usize) -> i64 {
    u64_at(data, at) as i64
}

fn ascii(bytes: &[u8]) -> String {
    bytes.iter().take_while(|b| **b != 0).map(|b| if (0x20..0x7F).contains(b) { *b as char } else { '?' }).collect()
}

fn array32(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes[..32]);
    out
}

/// Reads the container of a `.kmd`.
pub fn parse(data: &[u8]) -> Result<Container, Refusal> {
    if data.len() < HEADER || &data[0..4] != b"KMD\0" {
        return Err("ce n'est pas un module .kmd");
    }
    if u16_at(data, 4) != 1 {
        return Err("version de conteneur inconnue");
    }
    if u16_at(data, 6) != 1 {
        return Err("classe de module inconnue (seul le GPU existe)");
    }
    let interface = u32_at(data, 8);
    let level = match u32_at(data, 12) {
        1 => Level::SelfSigned,
        2 => Level::Enterprise,
        3 => Level::System,
        _ => return Err("module non signe"),
    };
    let offset = u64_at(data, 16);
    let size = u64_at(data, 24);
    let end = offset.checked_add(size).ok_or("charge utile hors du fichier")?;
    if offset < HEADER as u64 || size == 0 || end > data.len() as u64 {
        return Err("charge utile hors du fichier");
    }
    Ok(Container {
        name: ascii(&data[64..112]),
        vendor: ascii(&data[112..144]),
        interface,
        level,
        payload: offset as usize..end as usize,
        payload_hash: array32(&data[32..64]),
        authority: array32(&data[144..176]),
        binding: array32(&data[176..208]),
    })
}

/// The digest SYSTEM looks up and ENTERPRISE binds: the header up to the
/// signature fields, then the payload.
pub fn authenticity_digest(data: &[u8], container: &Container) -> [u8; 32] {
    let mut hasher = sha256::Sha256::new();
    hasher.update(&data[..144]);
    hasher.update(&data[container.payload.clone()]);
    hasher.finish()
}

/// The self-signed digests an administrator authorized.
fn authorized_digests() -> Vec<[u8; 32]> {
    let Ok(text) = crate::fs::read_file(AUTHORIZED, crate::fs::perm::Credentials::kernel()) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&text);
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.len() != 64 {
                return None;
            }
            let mut digest = [0u8; 32];
            for (index, byte) in digest.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&line[index * 2..index * 2 + 2], 16).ok()?;
            }
            Some(digest)
        })
        .collect()
}

/// Checks the container and its signature. Returns the level accepted.
pub fn verify(data: &[u8], container: &Container) -> Result<Level, Refusal> {
    if container.interface != backend::INTERFACE_VERSION {
        return Err("interface de backend d'une autre version");
    }
    let payload = sha256::digest(&data[container.payload.clone()]);
    if !sha256::equal(&payload, &container.payload_hash) {
        return Err("charge utile alteree (empreinte fausse)");
    }
    let digest = authenticity_digest(data, container);
    match container.level {
        Level::System => {
            if crate::binfmt::signature::is_official(&digest) {
                Ok(Level::System)
            } else {
                Err("SYSTEM : empreinte absente de la table du noyau")
            }
        }
        Level::Enterprise => {
            if !crate::binfmt::signature::is_trusted_authority(&container.authority) {
                return Err("ENTERPRISE : autorite inconnue");
            }
            let expected = crate::binfmt::signature::expected_binding(&container.authority, &digest);
            if !sha256::equal(&expected, &container.binding) {
                return Err("ENTERPRISE : certificat d'un autre module");
            }
            Ok(Level::Enterprise)
        }
        Level::SelfSigned => {
            if authorized_digests().iter().any(|d| sha256::equal(d, &digest)) {
                Ok(Level::SelfSigned)
            } else {
                Err("SELF : non autorise par un administrateur")
            }
        }
    }
}

mod abi;
mod context;
mod elf;

pub use abi::*;
pub use context::*;
use elf::*;

// ============================================================
//  Loading, and the adapter to the core
// ============================================================

/// A module loaded and initialized.
pub struct Loaded {
    image: Option<Image>,
    table: &'static KmdGpu,
    pub name: String,
}

// SAFETY: the module's table is immutable once returned; its functions are
// only called under the core's locks.
unsafe impl Send for Loaded {}
unsafe impl Sync for Loaded {}

fn error(code: i32) -> backend::Error {
    match code {
        -1 => backend::Error::Unsupported,
        -2 => backend::Error::NoMemory,
        -3 => backend::Error::Lost,
        _ => backend::Error::Device("refus du module"),
    }
}

/// Verifies, loads and initializes a module. The table it returns is
/// checked before anything else of it is called.
pub fn open(data: &[u8]) -> Result<Loaded, Refusal> {
    let container = parse(data)?;
    verify(data, &container)?;
    let image = load(&data[container.payload.clone()])?;
    // SAFETY: the entry was checked to lie in the module's code; the
    // module promises the signature of `nova_kmd_init`.
    let init: extern "C" fn(*const KernelApi) -> *const KmdGpu = unsafe { core::mem::transmute(image.entry as *const ()) };
    let table = init(&KERNEL_API);
    let in_image = |pointer: u64| (image.base..image.base + image.frames.len() as u64 * PAGE).contains(&pointer);
    if table.is_null() || !in_image(table as u64) {
        image.unload();
        return Err("le module a decline son initialisation");
    }
    // SAFETY: a pointer into the module's own image, checked just above.
    let table: &'static KmdGpu = unsafe { &*table };
    if table.interface != backend::INTERFACE_VERSION || (table.size as usize) < core::mem::size_of::<KmdGpu>() {
        image.unload();
        return Err("table du module d'une autre version");
    }
    let functions: [u64; 20] = [
        table.probe as *const () as u64,
        table.describe as *const () as u64,
        table.open as *const () as u64,
        table.alloc as *const () as u64,
        table.free as *const () as u64,
        table.map as *const () as u64,
        table.unmap as *const () as u64,
        table.create_buffer as *const () as u64,
        table.create_image as *const () as u64,
        table.create_sampler as *const () as u64,
        table.create_shader as *const () as u64,
        table.create_layout as *const () as u64,
        table.create_pipeline as *const () as u64,
        table.destroy as *const () as u64,
        table.submit as *const () as u64,
        table.poll as *const () as u64,
        table.export_image as *const () as u64,
        table.import_image as *const () as u64,
        table.close as *const () as u64,
        table.name as u64,
    ];
    if !functions.iter().all(|f| in_image(*f)) {
        image.unload();
        return Err("table du module incomplete ou hors de son image");
    }
    let name = if container.name.is_empty() { String::from("?") } else { container.name.clone() };
    Ok(Loaded { image: Some(image), table, name })
}

impl Loaded {
    /// Is the module's card present?
    pub fn probe(&self) -> bool {
        (self.table.probe)() == 0
    }

    /// Unloads the module. Nothing of it may be called any more.
    pub fn unload(mut self) {
        if let Some(image) = self.image.take() {
            image.unload();
        }
    }
}

/// A module as a backend device.
pub struct ModuleDevice {
    loaded: Loaded,
}

impl ModuleDevice {
    pub fn new(loaded: Loaded) -> ModuleDevice {
        ModuleDevice { loaded }
    }

    /// Unloads the module: no context of it may be open.
    pub fn unload(self) {
        self.loaded.unload();
    }
}

impl backend::Device for ModuleDevice {
    fn describe(&self) -> Description {
        let mut out = KmdDescription {
            name: [0; 64],
            vendor_id: 0,
            device_id: 0,
            max_image_size: 0,
            max_push_constants: 0,
            local_memory: 0,
            visible_memory: 0,
            formats: 0,
            sharing: 0,
            buffer_alignment: 256,
        };
        let _ = (self.loaded.table.describe)(&mut out);
        Description {
            name: ascii(&out.name),
            vendor_id: out.vendor_id,
            device_id: out.device_id,
            max_image_size: out.max_image_size,
            max_push_constants: out.max_push_constants.min(256),
            local_memory: out.local_memory,
            visible_memory: out.visible_memory,
            formats: out.formats,
            sharing: out.sharing != 0,
            buffer_alignment: out.buffer_alignment.clamp(1, 65536),
        }
    }

    fn open(&self, name: &str) -> backend::Result<Box<dyn backend::Context>> {
        let mut c_name = [0u8; 64];
        let length = name.len().min(63);
        c_name[..length].copy_from_slice(&name.as_bytes()[..length]);
        let mut context: *mut u8 = core::ptr::null_mut();
        let code = (self.loaded.table.open)(c_name.as_ptr(), &mut context);
        if code != 0 || context.is_null() {
            return Err(error(code));
        }
        Ok(Box::new(ModuleContext { table: self.loaded.table, context }))
    }
}

// ============================================================
//  At boot
// ============================================================

fn log_refusal(file: &str, reason: &str) {
    crate::serial_println!("[kmd] {} refuse - {}", file, reason);
    let kernel = crate::fs::perm::Credentials::kernel();
    let mut text = crate::fs::read_file(LOG, kernel).unwrap_or_default();
    if text.len() > 64 * 1024 {
        text.clear();
    }
    text.extend_from_slice(alloc::format!("{} refuse - {}\n", file, reason).as_bytes());
    let _ = crate::fs::write_file(LOG, &text, kernel, crate::fs::perm::Mode::system_binary(), crate::idt::ticks());
}

/// Loads the modules of `/sys/kernel/modules/gpu/`. Returns the first
/// whose card is present, when the built-in backend has none; the others
/// are unloaded. (One device at a time: the core drives a single GPU.)
pub fn load_all(built_in_present: bool) -> Option<ModuleDevice> {
    let kernel = crate::fs::perm::Credentials::kernel();
    let Ok(links) = crate::fs::list(DIRECTORY, kernel) else {
        return None;
    };
    let mut chosen = None;
    let mut seen = 0;
    for link in links {
        let name = link.name.clone();
        if !name.ends_with(".kmd") {
            continue;
        }
        seen += 1;
        let path = alloc::format!("{}/{}", DIRECTORY, name);
        let data = match crate::fs::read_file(&path, kernel) {
            Ok(data) => data,
            Err(_) => {
                log_refusal(&path, "illisible");
                continue;
            }
        };
        let loaded = match open(&data) {
            Ok(loaded) => loaded,
            Err(reason) => {
                log_refusal(&path, reason);
                continue;
            }
        };
        if let Ok(container) = parse(&data)
            && container.level == Level::SelfSigned
        {
            crate::serial_println!("[kmd] AVERTISSEMENT - {} est auto-signe : autorise par un administrateur, provenance non etablie", path);
        }
        if built_in_present || chosen.is_some() {
            crate::serial_println!("[kmd] {} charge puis retire - un GPU est deja servi", loaded.name);
            loaded.unload();
            continue;
        }
        if loaded.probe() {
            crate::serial_println!("[kmd] {} : carte trouvee - backend du GPU", loaded.name);
            chosen = Some(ModuleDevice::new(loaded));
        } else {
            loaded.unload();
        }
    }
    crate::serial_println!("[kmd] {} module(s) GPU dans {}", seen, DIRECTORY);
    chosen
}

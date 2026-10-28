//! Domain 0C, `SYS_GPU_*` (spec 6.8): the calls, as programs make them.
//!
//! ## One implementation, two doors
//!
//! A Nova program (`.ss`, `.gs`...) reaches these calls through the Nova
//! table (`syscall::table`, domain 0x0C); a program of the Nova C library
//! (spec 6.7) through `syscall::ported`. Both land in `dispatch`, with the
//! same convention: arguments in RDI, RSI, RDX, R10, R8, R9, and a result
//! that is either a value or a negative error number (spec 6.8, "pour tous
//! les types de binaires"). `nova_gfx.h` is the same for both.
//!
//! ## What crosses
//!
//! Structures are read from the program's memory with their size, field
//! by field (`from_le_bytes`), never by casting a pointer: nothing
//! guarantees their alignment, and a size shorter than the structure is
//! refused rather than read past. Sizes a program announces -- a shader, a
//! command buffer -- are bounded before anything is allocated, and the
//! allocation itself is fallible: a refusal is `ENOMEM`, never a kernel
//! panic (DECISIONS, "Une taille venue d'un programme").

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::backend::{self, BindingKind, Blend, Format, ImageDesc, LayoutBinding, MemKind, Topology, VertexFormat};
use super::{Create, Error, Owner, PipelineRequest};
use crate::posix::errno::*;
use crate::syscall::table::{entry, Entry};
use crate::syscall::{Call, Origin, SysResult};

/// Call numbers (spec 6.8).
pub mod nr {
    pub const INFO: u64 = 0x0C01;
    pub const CONTEXT_CREATE: u64 = 0x0C02;
    pub const CONTEXT_DESTROY: u64 = 0x0C03;
    pub const MEM_ALLOC: u64 = 0x0C04;
    pub const MEM_FREE: u64 = 0x0C05;
    pub const MEM_MAP: u64 = 0x0C06;
    pub const MEM_UNMAP: u64 = 0x0C07;
    pub const OBJECT_CREATE: u64 = 0x0C08;
    pub const OBJECT_DESTROY: u64 = 0x0C09;
    pub const SUBMIT: u64 = 0x0C0A;
    pub const FENCE_CREATE: u64 = 0x0C0B;
    pub const FENCE_WAIT: u64 = 0x0C0C;
    pub const FENCE_EXPORT: u64 = 0x0C0D;
    pub const SHARE: u64 = 0x0C0E;
    pub const IMPORT: u64 = 0x0C0F;
    pub const PRESENT: u64 = 0x0C10;
}

/// Object kinds of `SYS_GPU_OBJECT_CREATE` (`NOVA_OBJ_*`).
pub mod kind {
    pub const BUFFER: u64 = 1;
    pub const IMAGE: u64 = 2;
    pub const SAMPLER: u64 = 3;
    pub const SHADER: u64 = 4;
    pub const LAYOUT: u64 = 5;
    pub const PIPELINE: u64 = 6;
}

/// Size of `struct nova_gpu_info`.
pub const INFO_SIZE: usize = 128;
/// Sizes of the descriptions.
pub const BUFFER_DESC: usize = 24;
pub const IMAGE_DESC: usize = 32;
pub const SAMPLER_DESC: usize = 8;
pub const SHADER_DESC: usize = 16;
pub const LAYOUT_HEADER: usize = 8;
pub const LAYOUT_BINDING: usize = 12;
pub const PIPELINE_DESC: usize = 212;

/// `EDQUOT`: the process's GPU quota is spent.
pub const EDQUOT: i64 = 122;
/// `ENOTRECOVERABLE`: the context is lost (spec 6.8, "contexte perdu").
pub const ENOTRECOVERABLE: i64 = 131;

fn errno(error: Error) -> i64 {
    match error {
        Error::Absent => ENODEV,
        Error::BadHandle => EBADF,
        Error::Invalid(reason) => {
            crate::serial_println!("[gpu] refus - {}", reason);
            EINVAL
        }
        Error::NoMemory => ENOMEM,
        Error::Limit => EDQUOT,
        Error::Unsupported => EOPNOTSUPP,
        Error::Lost => ENOTRECOVERABLE,
        Error::TimedOut => ETIMEDOUT,
        Error::Interrupted => EINTR,
    }
}

// ============================================================
//  The program's memory
// ============================================================

/// Copies `len` bytes from the caller, with a fallible allocation.
fn read(origin: Origin, ptr: u64, len: usize) -> Result<Vec<u8>, i64> {
    if len == 0 {
        return Ok(Vec::new());
    }
    if origin == Origin::User {
        crate::syscall::validate_user_range(ptr, len as u64, false).map_err(|_| EFAULT)?;
    } else if ptr == 0 {
        return Err(EFAULT);
    }
    let mut out = Vec::new();
    out.try_reserve_exact(len).map_err(|_| ENOMEM)?;
    out.resize(len, 0);
    // SAFETY: the range was validated as mapped, readable user memory (or
    // is the kernel's own, for a kernel caller).
    unsafe { core::ptr::copy_nonoverlapping(ptr as *const u8, out.as_mut_ptr(), len) };
    Ok(out)
}

fn write(origin: Origin, ptr: u64, bytes: &[u8]) -> Result<(), i64> {
    if bytes.is_empty() {
        return Ok(());
    }
    if origin == Origin::User {
        crate::syscall::validate_user_range(ptr, bytes.len() as u64, true).map_err(|_| EFAULT)?;
    } else if ptr == 0 {
        return Err(EFAULT);
    }
    // SAFETY: as in `read`, for writing.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len()) };
    Ok(())
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u32_at(bytes, offset) as u64 | (u32_at(bytes, offset + 4) as u64) << 32
}

/// A handle argument: a 32-bit value, zero never valid.
fn handle(value: u64) -> Result<u32, i64> {
    if value == 0 || value > u32::MAX as u64 { Err(EBADF) } else { Ok(value as u32) }
}

// ============================================================
//  Dispatch
// ============================================================

/// Runs call `number`. Returns the value, or a negative errno.
pub fn dispatch(number: u64, args: [u64; 6], origin: Origin) -> i64 {
    match call(number, args, origin) {
        Ok(value) => value as i64,
        Err(error) => -error,
    }
}

fn call(number: u64, a: [u64; 6], origin: Origin) -> Result<u64, i64> {
    let owner = Owner::current();
    let e = errno;
    match number {
        nr::INFO => info(origin, a[0], a[1]),
        nr::CONTEXT_CREATE => {
            let name = process_name();
            super::context_create(&owner, &name).map(u64::from).map_err(e)
        }
        nr::CONTEXT_DESTROY => super::context_destroy(&owner, handle(a[0])?).map(|_| 0).map_err(e),
        nr::MEM_ALLOC => {
            let kind = match a[2] {
                0 => MemKind::Local,
                1 => MemKind::Visible,
                2 => MemKind::Readback,
                _ => return Err(EINVAL),
            };
            super::mem_alloc(&owner, handle(a[0])?, a[1], kind).map(u64::from).map_err(e)
        }
        nr::MEM_FREE => super::mem_free(&owner, handle(a[0])?, handle(a[1])?).map(|_| 0).map_err(e),
        nr::MEM_MAP => super::mem_map(&owner, handle(a[0])?, handle(a[1])?).map_err(e),
        nr::MEM_UNMAP => super::mem_unmap(&owner, handle(a[0])?, handle(a[1])?).map(|_| 0).map_err(e),
        nr::OBJECT_CREATE => object_create(&owner, origin, handle(a[0])?, a[1], a[2], a[3]),
        nr::OBJECT_DESTROY => super::object_destroy(&owner, handle(a[0])?, handle(a[1])?).map(|_| 0).map_err(e),
        nr::SUBMIT => submit(&owner, origin, handle(a[0])?, a[1], a[2], a[3], a[4], a[5]),
        nr::FENCE_CREATE => super::fence_create(&owner, handle(a[0])?).map(u64::from).map_err(e),
        nr::FENCE_WAIT => fence_wait(&owner, origin, a[0], a[1], a[2], a[3]),
        nr::FENCE_EXPORT => fence_export(&owner, handle(a[0])?),
        nr::SHARE => super::share(&owner, handle(a[0])?, handle(a[1])?).map_err(e),
        nr::IMPORT => super::import(&owner, handle(a[0])?, a[1]).map(u64::from).map_err(e),
        nr::PRESENT => super::present::present(&owner, handle(a[0])?, handle(a[1])?, a[2], a[3]).map(|_| 0).map_err(e),
        _ => Err(ENOSYS),
    }
}

/// The caller's name, for the host's log.
fn process_name() -> String {
    match crate::posix::current_thread() {
        Some(thread) => thread.process.name.clone(),
        None => crate::scheduler::current_info().map(|i| i.name).unwrap_or_else(|| String::from("noyau")),
    }
}

/// `SYS_GPU_INFO(info, size)`.
fn info(origin: Origin, ptr: u64, size: u64) -> Result<u64, i64> {
    let size = (size as usize).min(INFO_SIZE);
    if size < 8 {
        return Err(EINVAL);
    }
    let mut out = [0u8; INFO_SIZE];
    out[0..4].copy_from_slice(&(INFO_SIZE as u32).to_le_bytes());
    let description = super::description();
    if let Some(d) = description.as_ref() {
        let flags: u32 = if d.sharing { 1 } else { 0 };
        let fields: [(usize, u32); 7] = [
            (4, 1),
            (8, backend::INTERFACE_VERSION),
            (12, d.vendor_id),
            (16, d.device_id),
            (20, d.max_image_size),
            (24, d.max_push_constants),
            (28, d.formats),
        ];
        for (offset, value) in fields {
            out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        out[32..40].copy_from_slice(&d.local_memory.to_le_bytes());
        out[40..48].copy_from_slice(&d.visible_memory.to_le_bytes());
        out[48..56].copy_from_slice(&super::QUOTA.to_le_bytes());
        out[56..60].copy_from_slice(&flags.to_le_bytes());
        let name = d.name.as_bytes();
        let length = name.len().min(63);
        out[64..64 + length].copy_from_slice(&name[..length]);
    }
    write(origin, ptr, &out[..size])?;
    if description.is_some() { Ok(0) } else { Err(ENODEV) }
}

fn format(value: u32) -> Result<Format, i64> {
    Format::from_u32(value).ok_or(EINVAL)
}

/// `SYS_GPU_OBJECT_CREATE(ctx, kind, desc, size)`.
fn object_create(owner: &Owner, origin: Origin, context: u32, kind: u64, desc: u64, size: u64) -> Result<u64, i64> {
    let size = size as usize;
    let create = |bytes: &[u8], wanted: usize| -> Result<(), i64> { if bytes.len() < wanted { Err(EINVAL) } else { Ok(()) } };
    let result = match kind {
        kind::BUFFER => {
            let d = read(origin, desc, size.min(BUFFER_DESC))?;
            create(&d, BUFFER_DESC)?;
            super::object_create(
                owner,
                context,
                Create::Buffer { size: u64_at(&d, 0), usage: u32_at(&d, 8), memory: u32_at(&d, 12), offset: u64_at(&d, 16) },
            )
        }
        kind::IMAGE => {
            let d = read(origin, desc, size.min(IMAGE_DESC))?;
            create(&d, IMAGE_DESC)?;
            let image = ImageDesc {
                format: format(u32_at(&d, 0))?,
                width: u32_at(&d, 4),
                height: u32_at(&d, 8),
                usage: u32_at(&d, 12),
                shareable: u32_at(&d, 20) & 1 != 0,
            };
            super::object_create(owner, context, Create::Image { desc: image, memory: u32_at(&d, 16), offset: u64_at(&d, 24) })
        }
        kind::SAMPLER => {
            let d = read(origin, desc, size.min(SAMPLER_DESC))?;
            create(&d, SAMPLER_DESC)?;
            let sampler = backend::SamplerDesc { linear: u32_at(&d, 0) == 1, address: u32_at(&d, 4) };
            super::object_create(owner, context, Create::Sampler(sampler))
        }
        kind::SHADER => {
            let d = read(origin, desc, size.min(SHADER_DESC))?;
            create(&d, SHADER_DESC)?;
            let code_size = u32_at(&d, 8) as usize;
            if code_size == 0 || code_size > super::spirv::MAX_SIZE || code_size % 4 != 0 {
                return Err(EINVAL);
            }
            let stage = super::spirv::Stage::from_u32(u32_at(&d, 12)).ok_or(EINVAL)?;
            let bytes = read(origin, u64_at(&d, 0), code_size)?;
            let words = super::spirv::words(&bytes).ok_or(ENOMEM)?;
            drop(bytes);
            super::object_create(owner, context, Create::Shader { code: &words, stage })
        }
        kind::LAYOUT => {
            let header = read(origin, desc, LAYOUT_HEADER.min(size))?;
            create(&header, LAYOUT_HEADER)?;
            let count = u32_at(&header, 0) as usize;
            if count > 16 || size < LAYOUT_HEADER + count * LAYOUT_BINDING {
                return Err(EINVAL);
            }
            let d = read(origin, desc, LAYOUT_HEADER + count * LAYOUT_BINDING)?;
            let mut bindings = Vec::with_capacity(count);
            for index in 0..count {
                let at = LAYOUT_HEADER + index * LAYOUT_BINDING;
                let kind = match u32_at(&d, at + 4) {
                    1 => BindingKind::SampledImage,
                    2 => BindingKind::UniformBuffer,
                    3 => BindingKind::StorageBuffer,
                    _ => return Err(EINVAL),
                };
                bindings.push(LayoutBinding { binding: u32_at(&d, at), kind, stages: u32_at(&d, at + 8) });
            }
            let layout = backend::LayoutDesc { bindings, push_constants: u32_at(&d, 4) };
            super::object_create(owner, context, Create::Layout(layout))
        }
        kind::PIPELINE => {
            let d = read(origin, desc, size.min(PIPELINE_DESC))?;
            create(&d, PIPELINE_DESC)?;
            let w = |index: usize| u32_at(&d, index * 4);
            let topology = match w(3) {
                0 => Topology::Triangles,
                1 => Topology::TriangleStrip,
                2 => Topology::Lines,
                3 => Topology::Points,
                _ => return Err(EINVAL),
            };
            let blend = match w(6) {
                0 => Blend::None,
                1 => Blend::Premultiplied,
                2 => Blend::Alpha,
                3 => Blend::Additive,
                _ => return Err(EINVAL),
            };
            if w(4) > 2 {
                return Err(EINVAL);
            }
            let binding_count = w(11) as usize;
            if binding_count > 4 {
                return Err(EINVAL);
            }
            let bindings = (0..binding_count)
                .map(|i| backend::VertexBinding { stride: w(12 + i * 2), per_instance: w(13 + i * 2) != 0 })
                .collect();
            let attribute_count = w(20) as usize;
            if attribute_count > 8 {
                return Err(EINVAL);
            }
            let mut attributes = Vec::with_capacity(attribute_count);
            for i in 0..attribute_count {
                let base = 21 + i * 4;
                attributes.push(backend::VertexAttribute {
                    location: w(base),
                    binding: w(base + 1),
                    format: VertexFormat::from_u32(w(base + 2)).ok_or(EINVAL)?,
                    offset: w(base + 3),
                });
            }
            let request = PipelineRequest {
                layout: handle(w(0) as u64)?,
                vertex: handle(w(1) as u64)?,
                fragment: handle(w(2) as u64)?,
                desc: backend::PipelineDesc {
                    layout: 0,
                    vertex: 0,
                    fragment: 0,
                    topology,
                    cull: w(4),
                    front_ccw: w(5) != 0,
                    blend,
                    color_format: format(w(7))?,
                    depth_format: if w(8) == 0 { None } else { Some(format(w(8))?) },
                    depth_test: w(9) != 0,
                    depth_write: w(10) != 0,
                    bindings,
                    attributes,
                },
            };
            super::object_create(owner, context, Create::Pipeline(request))
        }
        _ => return Err(EINVAL),
    };
    result.map(u64::from).map_err(errno)
}

/// `SYS_GPU_SUBMIT(ctx, cmds, size, signal, waits, wait_count)`.
#[allow(clippy::too_many_arguments)]
fn submit(owner: &Owner, origin: Origin, context: u32, cmds: u64, size: u64, signal: u64, waits: u64, count: u64) -> Result<u64, i64> {
    let size = size as usize;
    if size == 0 || size > super::cmd::MAX_SIZE || size % 4 != 0 || count > 16 {
        return Err(EINVAL);
    }
    let bytes = read(origin, cmds, size)?;
    let words = super::spirv::words(&bytes).ok_or(ENOMEM)?;
    drop(bytes);
    let wait_bytes = read(origin, waits, count as usize * 4)?;
    let wait_handles: Vec<u32> = (0..count as usize).map(|i| u32_at(&wait_bytes, i * 4)).collect();
    let signal = if signal == 0 { 0 } else { handle(signal)? };
    super::submit(owner, context, &words, signal, &wait_handles).map(|_| 0).map_err(errno)
}

/// `SYS_GPU_FENCE_WAIT(fences, count, timeout_ns, all)`.
fn fence_wait(owner: &Owner, origin: Origin, fences: u64, count: u64, timeout_ns: u64, all: u64) -> Result<u64, i64> {
    if count == 0 || count > 64 {
        return Err(EINVAL);
    }
    let bytes = read(origin, fences, count as usize * 4)?;
    let mut views = Vec::with_capacity(count as usize);
    for index in 0..count as usize {
        views.push(super::fence_view(owner, u32_at(&bytes, index * 4)).map_err(errno)?);
    }
    let timeout = if timeout_ns == u64::MAX { None } else { Some(timeout_ns.div_ceil(1_000_000)) };
    super::fence_wait(&views, all != 0, timeout).map(|_| 0).map_err(errno)
}

/// A fence as a descriptor: readable once signalled (`poll`, `epoll`,
/// `SYS_OBJ_WAIT`).
struct FenceFile {
    view: super::FenceView,
}

impl crate::posix::fd::FileOps for FenceFile {
    fn read(&self, buf: &mut [u8], nonblock: bool) -> crate::posix::errno::LxResult<usize> {
        if buf.len() < 8 {
            return Err(EINVAL);
        }
        if !self.view.signalled() {
            if nonblock {
                return Err(EAGAIN);
            }
            super::fence_wait(core::slice::from_ref(&self.view), true, None).map_err(errno)?;
        }
        buf[..8].copy_from_slice(&1u64.to_le_bytes());
        Ok(8)
    }
    fn stat(&self) -> crate::posix::errno::LxResult<crate::posix::vfs::Stat> {
        Ok(crate::posix::vfs::Stat { dev: 0xa, mode: 0o400, ..Default::default() })
    }
    fn kind(&self) -> crate::posix::vfs::Kind {
        crate::posix::vfs::Kind::File
    }
    fn poll(&self) -> u32 {
        if self.view.signalled() { crate::posix::fd::POLLIN } else { 0 }
    }
    fn wait_queue(&self) -> Option<&crate::posix::wait::WaitQueue> {
        Some(&super::EVENTS)
    }
    fn describe(&self) -> String {
        String::from("anon_inode:[nova-gpu-fence]")
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

/// `SYS_GPU_FENCE_EXPORT(fence)`: a descriptor of the caller's table.
fn fence_export(owner: &Owner, fence: u32) -> Result<u64, i64> {
    let view = super::fence_view(owner, fence).map_err(errno)?;
    let thread = crate::posix::current_thread().ok_or(EOPNOTSUPP)?;
    let file: Arc<dyn crate::posix::fd::FileOps> = Arc::new(FenceFile { view });
    let description = crate::posix::fd::Description::new(file, crate::posix::fd::O_RDONLY);
    let fd = thread.process.files.lock().insert(description, true)?;
    Ok(fd as u64)
}

// ============================================================
//  The Nova table
// ============================================================

fn nova(call: &mut Call) -> SysResult<u64> {
    Ok(dispatch(call.number as u64, call.args, call.origin) as u64)
}

/// Domain 0x0C of the Nova table. Every entry lands in `dispatch`, which
/// answers with a value or a negative errno (spec 6.8).
pub static TABLE: &[Entry] = &[
    entry!(0x0C01, "SYS_GPU_INFO", Sync, nova),
    entry!(0x0C02, "SYS_GPU_CONTEXT_CREATE", Sync, nova),
    entry!(0x0C03, "SYS_GPU_CONTEXT_DESTROY", Sync, nova),
    entry!(0x0C04, "SYS_GPU_MEM_ALLOC", Sync, nova),
    entry!(0x0C05, "SYS_GPU_MEM_FREE", Sync, nova),
    entry!(0x0C06, "SYS_GPU_MEM_MAP", Sync, nova),
    entry!(0x0C07, "SYS_GPU_MEM_UNMAP", Sync, nova),
    entry!(0x0C08, "SYS_GPU_OBJECT_CREATE", Sync, nova),
    entry!(0x0C09, "SYS_GPU_OBJECT_DESTROY", Sync, nova),
    entry!(0x0C0A, "SYS_GPU_SUBMIT", Async, nova),
    entry!(0x0C0B, "SYS_GPU_FENCE_CREATE", Sync, nova),
    entry!(0x0C0C, "SYS_GPU_FENCE_WAIT", Async, nova),
    entry!(0x0C0D, "SYS_GPU_FENCE_EXPORT", Sync, nova),
    entry!(0x0C0E, "SYS_GPU_SHARE", Sync, nova),
    entry!(0x0C0F, "SYS_GPU_IMPORT", Sync, nova),
    entry!(0x0C10, "SYS_GPU_PRESENT", Async, nova),
];

/// The door of the ported programs (`syscall::ported`).
pub fn ported(number: u64, args: [u64; 6]) -> crate::posix::errno::LxResult<u64> {
    let value = dispatch(number, args, Origin::User);
    if value < 0 { Err(-value) } else { Ok(value as u64) }
}

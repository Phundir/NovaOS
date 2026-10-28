//! The C ABI between the kernel and a module (`tools/kmd/nova_kmd.h`):
//! `KernelApi`, the only table a module sees, and the `repr(C)` mirrors of
//! what the module offers. Kept apart so the header and this file can be
//! compared side by side; `abi_sizes`/`abi_offsets` let the selftest check
//! that the C compiler agrees.

use super::*;

#[repr(C)]
pub struct KernelApi {
    pub(super) interface: u32,
    pub(super) size: u32,
    pub(super) log: extern "C" fn(*const u8),
    pub(super) alloc: extern "C" fn(usize) -> *mut u8,
    pub(super) free: extern "C" fn(*mut u8, usize),
    pub(super) pages_alloc: extern "C" fn(usize, *mut *mut u8) -> u64,
    pub(super) pages_free: extern "C" fn(u64, usize),
    pub(super) mmio_map: extern "C" fn(u64, usize) -> *mut u8,
    pub(super) pci_find: extern "C" fn(u16, u16, u32, *mut u32) -> i32,
    pub(super) pci_read32: extern "C" fn(u32, u32) -> u32,
    pub(super) pci_write32: extern "C" fn(u32, u32, u32),
    pub(super) pci_bar: extern "C" fn(u32, u32) -> u64,
    pub(super) time_ns: extern "C" fn() -> u64,
    pub(super) sleep_ms: extern "C" fn(u32),
}

extern "C" fn api_log(text: *const u8) {
    if text.is_null() {
        return;
    }
    let mut line = String::new();
    for index in 0..240 {
        // SAFETY: the module promises a NUL-terminated string; the read is
        // bounded at 240 bytes whatever it gave.
        let byte = unsafe { text.add(index).read() };
        if byte == 0 {
            break;
        }
        line.push(if (0x20..0x7F).contains(&byte) { byte as char } else { '?' });
    }
    crate::serial_println!("{}", line);
}

extern "C" fn api_alloc(size: usize) -> *mut u8 {
    match core::alloc::Layout::from_size_align(size.max(1), 16) {
        // SAFETY: a non-zero, valid layout.
        Ok(layout) if size <= 16 << 20 => unsafe { alloc::alloc::alloc(layout) },
        _ => core::ptr::null_mut(),
    }
}

extern "C" fn api_free(pointer: *mut u8, size: usize) {
    if pointer.is_null() {
        return;
    }
    if let Ok(layout) = core::alloc::Layout::from_size_align(size.max(1), 16) {
        // SAFETY: the module frees what `api_alloc` gave it, with its size.
        unsafe { alloc::alloc::dealloc(pointer, layout) };
    }
}

extern "C" fn api_pages_alloc(count: usize, virtual_address: *mut *mut u8) -> u64 {
    if count == 0 || count > 4096 {
        return 0;
    }
    let Some(frame) = crate::frame::allocate_contiguous(count) else {
        return 0;
    };
    let physical = frame.start_addr();
    // SAFETY: frames just allocated, reached through the physical window.
    unsafe { core::ptr::write_bytes(phys_to_virt(physical) as *mut u8, 0, count * PAGE as usize) };
    if !virtual_address.is_null() {
        // SAFETY: the module passes where to write the address.
        unsafe { virtual_address.write(phys_to_virt(physical) as *mut u8) };
    }
    physical
}

extern "C" fn api_pages_free(physical: u64, count: usize) {
    for page in 0..count as u64 {
        // SAFETY: the module gives back what `api_pages_alloc` lent.
        unsafe { crate::frame::deallocate_frame(crate::frame::Frame(physical + page * PAGE)) };
    }
}

extern "C" fn api_mmio_map(physical: u64, size: usize) -> *mut u8 {
    let base = physical & !(PAGE - 1);
    let pages = (physical - base + size as u64).div_ceil(PAGE).max(1);
    let virt = NEXT_MMIO.fetch_add((pages + 1) * PAGE, Ordering::Relaxed);
    let mut page_flags = flags::PRESENT | flags::WRITABLE | flags::NO_CACHE | flags::WRITE_THROUGH;
    if memory::nx_enabled() {
        page_flags |= flags::NO_EXECUTE;
    }
    for page in 0..pages {
        // SAFETY: device registers the module names; the window is its own.
        if unsafe { memory::map_page(virt + page * PAGE, base + page * PAGE, page_flags) }.is_err() {
            return core::ptr::null_mut();
        }
    }
    (virt + (physical - base)) as *mut u8
}

pub(super) fn address(bdf: u32) -> crate::pci::Address {
    crate::pci::Address { bus: (bdf >> 8) as u8, device: ((bdf >> 3) & 0x1F) as u8, function: (bdf & 7) as u8 }
}

extern "C" fn api_pci_find(vendor: u16, device: u16, index: u32, out: *mut u32) -> i32 {
    let found = crate::pci::enumerate()
        .into_iter()
        .filter(|d| d.vendor_id == vendor && d.device_id == device)
        .nth(index as usize);
    match found {
        Some(d) if !out.is_null() => {
            let a = d.address;
            // SAFETY: the module passes where to write the address.
            unsafe { out.write((a.bus as u32) << 8 | (a.device as u32) << 3 | a.function as u32) };
            0
        }
        _ => -1,
    }
}

extern "C" fn api_pci_read32(bdf: u32, offset: u32) -> u32 {
    if offset > 0xFC {
        return u32::MAX;
    }
    crate::pci::read32(address(bdf), offset as u8 & 0xFC)
}

extern "C" fn api_pci_write32(bdf: u32, offset: u32, value: u32) {
    if offset <= 0xFC {
        // SAFETY: the module drives its own card; spec 3.6: its signature
        // is its only barrier.
        unsafe { crate::pci::write32(address(bdf), offset as u8 & 0xFC, value) };
    }
}

extern "C" fn api_pci_bar(bdf: u32, index: u32) -> u64 {
    if index > 5 {
        return 0;
    }
    let device = crate::pci::enumerate().into_iter().find(|d| d.address.bus as u32 == bdf >> 8 && d.address.device as u32 == (bdf >> 3) & 0x1F && d.address.function as u32 == bdf & 7);
    device.map(|d| d.bar(index as u8)).unwrap_or(0)
}

extern "C" fn api_time_ns() -> u64 {
    crate::idt::fine_ticks().saturating_mul(1_000_000)
}

extern "C" fn api_sleep_ms(ms: u32) {
    let _ = crate::syscall::invoke(crate::syscall::domain_time::SYS_TIME_SLEEP_MS, [ms as u64, 0, 0, 0, 0, 0]);
}

pub(super) static KERNEL_API: KernelApi = KernelApi {
    interface: backend::INTERFACE_VERSION,
    size: core::mem::size_of::<KernelApi>() as u32,
    log: api_log,
    alloc: api_alloc,
    free: api_free,
    pages_alloc: api_pages_alloc,
    pages_free: api_pages_free,
    mmio_map: api_mmio_map,
    pci_find: api_pci_find,
    pci_read32: api_pci_read32,
    pci_write32: api_pci_write32,
    pci_bar: api_pci_bar,
    time_ns: api_time_ns,
    sleep_ms: api_sleep_ms,
};

// ============================================================
//  What the module offers (tools/kmd/nova_kmd.h)
// ============================================================

#[repr(C)]
#[derive(Clone, Copy)]
pub struct KmdDescription {
    pub(super) name: [u8; 64],
    pub(super) vendor_id: u32,
    pub(super) device_id: u32,
    pub(super) max_image_size: u32,
    pub(super) max_push_constants: u32,
    pub(super) local_memory: u64,
    pub(super) visible_memory: u64,
    pub(super) formats: u32,
    pub(super) sharing: u32,
    pub(super) buffer_alignment: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct KmdWindow {
    pub(super) physical: u64,
    pub(super) size: u64,
    pub(super) cache_flags: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct KmdImageDesc {
    pub(super) format: u32,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) usage: u32,
    pub(super) shareable: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct KmdBufferDesc {
    pub(super) size: u64,
    pub(super) usage: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct KmdSamplerDesc {
    pub(super) linear: u32,
    pub(super) address: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct KmdLayoutBinding {
    pub(super) binding: u32,
    pub(super) kind: u32,
    pub(super) stages: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct KmdLayoutDesc {
    pub(super) count: u32,
    pub(super) push_constants: u32,
    pub(super) bindings: [KmdLayoutBinding; 16],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct KmdPipelineDesc {
    pub(super) layout: u64,
    pub(super) vertex: u64,
    pub(super) fragment: u64,
    pub(super) topology: u32,
    pub(super) cull: u32,
    pub(super) front_ccw: u32,
    pub(super) blend: u32,
    pub(super) color_format: u32,
    pub(super) depth_format: u32,
    pub(super) depth_test: u32,
    pub(super) depth_write: u32,
    pub(super) binding_count: u32,
    pub(super) bindings: [[u32; 2]; 4],
    pub(super) attribute_count: u32,
    pub(super) attributes: [[u32; 4]; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct KmdImageRef {
    pub(super) id: u64,
    pub(super) format: u32,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) first_use: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct KmdRect {
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) width: u32,
    pub(super) height: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct KmdWrite {
    pub(super) binding: u32,
    pub(super) kind: u32,
    pub(super) object: u64,
    pub(super) sampler: u64,
    pub(super) offset: u64,
    pub(super) range: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BeginRender {
    pub(super) color: KmdImageRef,
    pub(super) load: u32,
    pub(super) clear: [u32; 4],
    pub(super) has_depth: u32,
    pub(super) depth: KmdImageRef,
    pub(super) depth_load: u32,
    pub(super) depth_clear: u32,
    pub(super) area: KmdRect,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BufferImage {
    pub(super) buffer: u64,
    pub(super) offset: u64,
    pub(super) row_pixels: u32,
    pub(super) image: KmdImageRef,
    pub(super) rect: KmdRect,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CopyImage {
    pub(super) src: KmdImageRef,
    pub(super) dst: KmdImageRef,
    pub(super) src_x: i32,
    pub(super) src_y: i32,
    pub(super) dst_x: i32,
    pub(super) dst_y: i32,
    pub(super) width: u32,
    pub(super) height: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Blit {
    pub(super) src: KmdImageRef,
    pub(super) dst: KmdImageRef,
    pub(super) src_rect: KmdRect,
    pub(super) dst_rect: KmdRect,
    pub(super) linear: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BindVertex {
    pub(super) slot: u32,
    pub(super) pad: u32,
    pub(super) buffer: u64,
    pub(super) offset: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BindIndex {
    pub(super) buffer: u64,
    pub(super) offset: u64,
    pub(super) wide: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Push {
    pub(super) layout: u64,
    pub(super) count: u32,
    pub(super) writes: *const KmdWrite,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Constants {
    pub(super) layout: u64,
    pub(super) offset: u32,
    pub(super) size: u32,
    pub(super) data: *const u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Draw {
    pub(super) count: u32,
    pub(super) instances: u32,
    pub(super) first: u32,
    pub(super) first_instance: u32,
    pub(super) vertex_offset: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Clear {
    pub(super) image: KmdImageRef,
    pub(super) color: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Fill {
    pub(super) buffer: u64,
    pub(super) offset: u64,
    pub(super) size: u64,
    pub(super) value: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Update {
    pub(super) buffer: u64,
    pub(super) offset: u64,
    pub(super) size: u64,
    pub(super) data: *const u8,
}

/// The union of `struct nova_kmd_command`: every variant `repr(C)`, as the
/// C side lays it out.
#[repr(C)]
#[derive(Clone, Copy)]
pub union Body {
    pub(super) begin_render: BeginRender,
    pub(super) one: u64,
    pub(super) bind_vertex: BindVertex,
    pub(super) bind_index: BindIndex,
    pub(super) push: Push,
    pub(super) viewport: [u32; 6],
    pub(super) rect: KmdRect,
    pub(super) constants: Constants,
    pub(super) draw: Draw,
    pub(super) copy_buffer: [u64; 5],
    pub(super) buffer_image: BufferImage,
    pub(super) copy_image: CopyImage,
    pub(super) blit: Blit,
    pub(super) clear: Clear,
    pub(super) fill: Fill,
    pub(super) update: Update,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct KmdCommand {
    pub(super) opcode: u32,
    pub(super) pad: u32,
    pub(super) body: Body,
}

#[repr(C)]
pub struct KmdGpu {
    pub(super) interface: u32,
    pub(super) size: u32,
    pub(super) name: *const u8,
    pub(super) probe: extern "C" fn() -> i32,
    pub(super) describe: extern "C" fn(*mut KmdDescription) -> i32,
    pub(super) open: extern "C" fn(*const u8, *mut *mut u8) -> i32,
    pub(super) alloc: extern "C" fn(*mut u8, u64, u32, *mut u64, *mut u64) -> i32,
    pub(super) free: extern "C" fn(*mut u8, u64),
    pub(super) map: extern "C" fn(*mut u8, u64, *mut KmdWindow) -> i32,
    pub(super) unmap: extern "C" fn(*mut u8, u64),
    pub(super) create_buffer: extern "C" fn(*mut u8, *const KmdBufferDesc, u64, u64, *mut u64) -> i32,
    pub(super) create_image: extern "C" fn(*mut u8, *const KmdImageDesc, u64, u64, i32, *mut u64, *mut u64) -> i32,
    pub(super) create_sampler: extern "C" fn(*mut u8, *const KmdSamplerDesc, *mut u64) -> i32,
    pub(super) create_shader: extern "C" fn(*mut u8, *const u32, u32, *mut u64) -> i32,
    pub(super) create_layout: extern "C" fn(*mut u8, *const KmdLayoutDesc, *mut u64) -> i32,
    pub(super) create_pipeline: extern "C" fn(*mut u8, *const KmdPipelineDesc, *mut u64) -> i32,
    pub(super) destroy: extern "C" fn(*mut u8, u32, u64),
    pub(super) submit: extern "C" fn(*mut u8, *const KmdCommand, u32, *mut u64) -> i32,
    pub(super) poll: extern "C" fn(*mut u8, u64) -> i32,
    pub(super) export_image: extern "C" fn(*mut u8, u64, *mut u32) -> i32,
    pub(super) import_image: extern "C" fn(*mut u8, u32, *const KmdImageDesc, *mut u64) -> i32,
    pub(super) close: extern "C" fn(*mut u8),
}

/// Sizes the C side must agree with (checked by the self-test against the
/// values `tools/kmd/nova_kmd.h` gives with gcc).
pub fn abi_sizes() -> [usize; 6] {
    [
        core::mem::size_of::<KernelApi>(),
        core::mem::size_of::<KmdGpu>(),
        core::mem::size_of::<KmdCommand>(),
        core::mem::size_of::<KmdPipelineDesc>(),
        core::mem::size_of::<KmdDescription>(),
        core::mem::size_of::<KmdWrite>(),
    ]
}

/// Offsets inside `struct nova_kmd_command` the C side must agree with:
/// fill value, update data, copy size, render area, draw vertex offset,
/// resource writes.
pub fn abi_offsets() -> [usize; 6] {
    let body = core::mem::offset_of!(KmdCommand, body);
    [
        body + core::mem::offset_of!(Fill, value),
        body + core::mem::offset_of!(Update, data),
        body + 4 * 8,
        body + core::mem::offset_of!(BeginRender, area),
        body + core::mem::offset_of!(Draw, vertex_offset),
        body + core::mem::offset_of!(Push, writes),
    ]
}

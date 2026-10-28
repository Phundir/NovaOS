//! Commands of the virtio-gpu control queue (virtio 1.2, section 5.7.6).
//!
//! Pure functions: each command is laid out into a byte buffer and each
//! answer is read back from one, field by field with `to_le_bytes` /
//! `from_le_bytes`. Nothing here touches the device, which is what lets
//! the self-test check every layout against the sizes the specification
//! gives.
//!
//! Every command starts with the same 24-byte header:
//!
//! ```text
//!   0   u32  type
//!   4   u32  flags
//!   8   u64  fence_id
//!   16  u32  ctx_id
//!   20  u8   ring_idx, then 3 bytes of padding
//! ```
//!
//! The 2D commands of 7.1 leave everything after `type` at zero. The 3D
//! commands of phase 13 fill `ctx_id` -- the context the command acts in
//! -- and, for a fenced submission, `flags`, `fence_id` and `ring_idx`:
//! the host then answers only once the work is **done** on its GPU, on
//! the timeline `ring_idx` names (see `Fenced`).

use alloc::vec::Vec;

pub const HEADER_SIZE: usize = 24;

// --- 2D commands ---
pub const CMD_GET_DISPLAY_INFO: u32 = 0x0100;
pub const CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
pub const CMD_RESOURCE_UNREF: u32 = 0x0102;
pub const CMD_SET_SCANOUT: u32 = 0x0103;
pub const CMD_RESOURCE_FLUSH: u32 = 0x0104;
pub const CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
pub const CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
pub const CMD_RESOURCE_DETACH_BACKING: u32 = 0x0107;

// --- cursor queue ---
pub const CMD_UPDATE_CURSOR: u32 = 0x0300;
pub const CMD_MOVE_CURSOR: u32 = 0x0301;

/// Side of the cursor image: the host's cursor plane is 64 x 64.
pub const CURSOR_SIDE: u32 = 64;

// --- answers ---
pub const RESP_OK_NODATA: u32 = 0x1100;
pub const RESP_OK_DISPLAY_INFO: u32 = 0x1101;

/// `B8G8R8X8_UNORM`: in memory B, G, R, then an unused byte. Read as a
/// little-endian `u32`, that is `0x00RRGGBB` -- red at bit 16, green at
/// bit 8, blue at bit 0, which `Format` describes like any GRUB mode.
pub const FORMAT_B8G8R8X8: u32 = 2;
/// `B8G8R8A8_UNORM`: the same, with the fourth byte an opacity -- what a
/// cursor image needs around its shape.
pub const FORMAT_B8G8R8A8: u32 = 1;

/// Scanouts a display-info answer describes (`VIRTIO_GPU_MAX_SCANOUTS`).
pub const MAX_SCANOUTS: usize = 16;

/// Size of the display-info answer: header, then 16 x (rect + enabled +
/// flags).
pub const DISPLAY_INFO_SIZE: usize = HEADER_SIZE + MAX_SCANOUTS * 24;

/// One entry of the backing list: address, length, padding.
pub const BACKING_ENTRY_SIZE: usize = 16;

/// Bit of `events_read` announcing that the display configuration changed
/// -- a host window resized, a monitor plugged.
pub const EVENT_DISPLAY: u32 = 1;

/// A rectangle in the resource, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Area {
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Area { x, y, width, height }
    }
}

fn header(buffer: &mut Vec<u8>, kind: u32) {
    buffer.extend_from_slice(&kind.to_le_bytes());
    buffer.extend_from_slice(&[0u8; HEADER_SIZE - 4]);
}

/// Header of a command addressed to a context, fenced or not.
fn header_3d(buffer: &mut Vec<u8>, kind: u32, context: u32, fenced: Option<Fenced>) {
    buffer.extend_from_slice(&kind.to_le_bytes());
    let (flags, fence, ring) = match fenced {
        Some(f) => (FLAG_FENCE | FLAG_INFO_RING_IDX, f.fence_id, f.ring),
        None => (0, 0, 0),
    };
    buffer.extend_from_slice(&flags.to_le_bytes());
    buffer.extend_from_slice(&fence.to_le_bytes());
    buffer.extend_from_slice(&context.to_le_bytes());
    buffer.push(ring);
    buffer.extend_from_slice(&[0u8; 3]);
}

fn area(buffer: &mut Vec<u8>, area: Area) {
    for value in [area.x, area.y, area.width, area.height] {
        buffer.extend_from_slice(&value.to_le_bytes());
    }
}

fn u32s(buffer: &mut Vec<u8>, values: &[u32]) {
    for value in values {
        buffer.extend_from_slice(&value.to_le_bytes());
    }
}

pub fn get_display_info() -> Vec<u8> {
    let mut buffer = Vec::with_capacity(HEADER_SIZE);
    header(&mut buffer, CMD_GET_DISPLAY_INFO);
    buffer
}

/// 40 bytes.
pub fn resource_create_2d(resource: u32, width: u32, height: u32) -> Vec<u8> {
    resource_create_2d_format(resource, FORMAT_B8G8R8X8, width, height)
}

/// 40 bytes, in a format of the caller's choice.
pub fn resource_create_2d_format(resource: u32, format: u32, width: u32, height: u32) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(40);
    header(&mut buffer, CMD_RESOURCE_CREATE_2D);
    u32s(&mut buffer, &[resource, format, width, height]);
    buffer
}

/// 56 bytes: shows `resource` (0 hides the cursor) on scanout 0 with its
/// hot spot at `(x, y)`.
pub fn update_cursor(resource: u32, x: u32, y: u32, hot_x: u32, hot_y: u32) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(56);
    header(&mut buffer, CMD_UPDATE_CURSOR);
    u32s(&mut buffer, &[0, x, y, 0, resource, hot_x, hot_y, 0]);
    buffer
}

/// 56 bytes: moves the cursor, image unchanged.
pub fn move_cursor(x: u32, y: u32) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(56);
    header(&mut buffer, CMD_MOVE_CURSOR);
    u32s(&mut buffer, &[0, x, y, 0, 0, 0, 0, 0]);
    buffer
}

/// 32 bytes.
pub fn resource_unref(resource: u32) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(32);
    header(&mut buffer, CMD_RESOURCE_UNREF);
    u32s(&mut buffer, &[resource, 0]);
    buffer
}

/// 48 bytes. A resource of zero turns the scanout off.
pub fn set_scanout(scanout: u32, resource: u32, rect: Area) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(48);
    header(&mut buffer, CMD_SET_SCANOUT);
    area(&mut buffer, rect);
    u32s(&mut buffer, &[scanout, resource]);
    buffer
}

/// 48 bytes.
pub fn resource_flush(resource: u32, rect: Area) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(48);
    header(&mut buffer, CMD_RESOURCE_FLUSH);
    area(&mut buffer, rect);
    u32s(&mut buffer, &[resource, 0]);
    buffer
}

/// 56 bytes. `offset` is where the rectangle starts in the backing,
/// in bytes: the host reads each row from there, one stride apart.
pub fn transfer_to_host_2d(resource: u32, rect: Area, offset: u64) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(56);
    header(&mut buffer, CMD_TRANSFER_TO_HOST_2D);
    area(&mut buffer, rect);
    buffer.extend_from_slice(&offset.to_le_bytes());
    u32s(&mut buffer, &[resource, 0]);
    buffer
}

/// 32 bytes, followed on the queue by the entries of `backing_entries`.
pub fn attach_backing(resource: u32, entries: u32) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(32);
    header(&mut buffer, CMD_RESOURCE_ATTACH_BACKING);
    u32s(&mut buffer, &[resource, entries]);
    buffer
}

/// 32 bytes.
pub fn detach_backing(resource: u32) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(32);
    header(&mut buffer, CMD_RESOURCE_DETACH_BACKING);
    u32s(&mut buffer, &[resource, 0]);
    buffer
}

/// Runs of physically contiguous frames, as `(address, length)`.
///
/// The frame allocator gives no contiguity guarantee, so the backing of a
/// resource is a scatter list. Merging neighbours keeps it short -- a
/// freshly booted allocator hands out long runs -- and the list length is
/// what bounds the descriptors the command needs.
pub fn coalesce(frames: &[u64], frame_size: u64) -> Vec<(u64, u32)> {
    let mut runs: Vec<(u64, u32)> = Vec::new();
    for &frame in frames {
        if let Some((start, length)) = runs.last_mut()
            && *start + *length as u64 == frame
            && (*length as u64 + frame_size) <= u32::MAX as u64
        {
            *length += frame_size as u32;
            continue;
        }
        runs.push((frame, frame_size as u32));
    }
    runs
}

/// The entries following an `attach_backing`.
pub fn backing_entries(runs: &[(u64, u32)]) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(runs.len() * BACKING_ENTRY_SIZE);
    for (address, length) in runs {
        buffer.extend_from_slice(&address.to_le_bytes());
        buffer.extend_from_slice(&length.to_le_bytes());
        buffer.extend_from_slice(&[0u8; 4]);
    }
    buffer
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let b = bytes.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Type of an answer.
pub fn response_type(bytes: &[u8]) -> Option<u32> {
    read_u32(bytes, 0)
}

/// Size of scanout 0 in a display-info answer, if it is enabled.
///
/// Only scanout 0: one screen is what the compositor knows how to drive.
/// A second host window would be a second scanout, and is ignored.
pub fn scanout_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if response_type(bytes)? != RESP_OK_DISPLAY_INFO {
        return None;
    }
    let base = HEADER_SIZE;
    let width = read_u32(bytes, base + 8)?;
    let height = read_u32(bytes, base + 12)?;
    let enabled = read_u32(bytes, base + 16)?;
    (enabled != 0 && width != 0 && height != 0).then_some((width, height))
}

// ============================================================
//  3D: capability sets, contexts, blobs, submissions (phase 13)
// ============================================================

pub const CMD_GET_CAPSET_INFO: u32 = 0x0108;
pub const CMD_GET_CAPSET: u32 = 0x0109;
pub const CMD_RESOURCE_CREATE_BLOB: u32 = 0x010C;
pub const CMD_CTX_CREATE: u32 = 0x0200;
pub const CMD_CTX_DESTROY: u32 = 0x0201;
pub const CMD_CTX_ATTACH_RESOURCE: u32 = 0x0202;
pub const CMD_CTX_DETACH_RESOURCE: u32 = 0x0203;
pub const CMD_SUBMIT_3D: u32 = 0x0207;
pub const CMD_RESOURCE_MAP_BLOB: u32 = 0x0208;
pub const CMD_RESOURCE_UNMAP_BLOB: u32 = 0x0209;

pub const RESP_OK_CAPSET_INFO: u32 = 0x1102;
pub const RESP_OK_CAPSET: u32 = 0x1103;
pub const RESP_OK_MAP_INFO: u32 = 0x1106;
/// First error answer; everything from here up is a refusal.
pub const RESP_ERR_UNSPEC: u32 = 0x1200;

/// `VIRTIO_GPU_FLAG_FENCE`: answer only once the work is done.
pub const FLAG_FENCE: u32 = 1;
/// `VIRTIO_GPU_FLAG_INFO_RING_IDX`: the fence belongs to `ring_idx`.
pub const FLAG_INFO_RING_IDX: u32 = 2;

/// `VIRTIO_GPU_CAPSET_VENUS`: serialized Vulkan.
pub const CAPSET_VENUS: u32 = 4;

/// Where a blob's memory lives: `HOST3D` is host memory the context
/// allocated (a Vulkan allocation, or a shared-memory buffer).
pub const BLOB_MEM_HOST3D: u32 = 2;
/// The guest may map the blob in the shared memory region.
pub const BLOB_FLAG_MAPPABLE: u32 = 1;
/// The blob may be attached to other contexts.
pub const BLOB_FLAG_SHAREABLE: u32 = 2;

/// Cache type of a mapped blob, low nibble of `map_info`.
pub const MAP_CACHE_CACHED: u32 = 1;
pub const MAP_CACHE_UNCACHED: u32 = 2;
pub const MAP_CACHE_WC: u32 = 3;

/// Size of the capset-info answer, and of a map-info answer.
pub const CAPSET_INFO_SIZE: usize = HEADER_SIZE + 16;
pub const MAP_INFO_SIZE: usize = HEADER_SIZE + 8;
/// The Venus capability set is 160 bytes today; the answer is read up to
/// the size the device announced, and this bounds it.
pub const CAPSET_MAX: usize = 1024;

/// A fence: the host answers the command once the work queued before it
/// on timeline `ring` is done. Ring 0 is the host's CPU timeline -- done
/// as soon as the command stream is decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fenced {
    pub fence_id: u64,
    pub ring: u8,
}

/// 32 bytes.
pub fn get_capset_info(index: u32) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(32);
    header(&mut buffer, CMD_GET_CAPSET_INFO);
    u32s(&mut buffer, &[index, 0]);
    buffer
}

/// 32 bytes.
pub fn get_capset(id: u32, version: u32) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(32);
    header(&mut buffer, CMD_GET_CAPSET);
    u32s(&mut buffer, &[id, version]);
    buffer
}

/// `(id, max_version, max_size)` of a capset-info answer.
pub fn capset_info(bytes: &[u8]) -> Option<(u32, u32, u32)> {
    if response_type(bytes)? != RESP_OK_CAPSET_INFO {
        return None;
    }
    Some((
        read_u32(bytes, HEADER_SIZE)?,
        read_u32(bytes, HEADER_SIZE + 4)?,
        read_u32(bytes, HEADER_SIZE + 8)?,
    ))
}

/// 96 bytes: the context's identifier is in the header, its protocol in
/// `context_init` (low byte: the capset), and a name for the host's log.
pub fn ctx_create(context: u32, capset: u32, name: &str) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(96);
    header_3d(&mut buffer, CMD_CTX_CREATE, context, None);
    let name = name.as_bytes();
    let length = name.len().min(63);
    u32s(&mut buffer, &[length as u32, capset & 0xFF]);
    let mut field = [0u8; 64];
    field[..length].copy_from_slice(&name[..length]);
    buffer.extend_from_slice(&field);
    buffer
}

/// 24 bytes.
pub fn ctx_destroy(context: u32) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(24);
    header_3d(&mut buffer, CMD_CTX_DESTROY, context, None);
    buffer
}

/// 32 bytes. Makes a blob another context created visible to `context`.
pub fn ctx_attach_resource(context: u32, resource: u32) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(32);
    header_3d(&mut buffer, CMD_CTX_ATTACH_RESOURCE, context, None);
    u32s(&mut buffer, &[resource, 0]);
    buffer
}

/// 32 bytes.
pub fn ctx_detach_resource(context: u32, resource: u32) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(32);
    header_3d(&mut buffer, CMD_CTX_DETACH_RESOURCE, context, None);
    u32s(&mut buffer, &[resource, 0]);
    buffer
}

/// 56 bytes: a blob of host memory, `blob_id` naming what backs it in the
/// context (0: a fresh shared-memory buffer; otherwise the object id of
/// a Vulkan allocation). No guest pages: `nr_entries` is 0.
pub fn resource_create_blob(context: u32, resource: u32, flags: u32, blob_id: u64, size: u64) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(56);
    header_3d(&mut buffer, CMD_RESOURCE_CREATE_BLOB, context, None);
    u32s(&mut buffer, &[resource, BLOB_MEM_HOST3D, flags, 0]);
    buffer.extend_from_slice(&blob_id.to_le_bytes());
    buffer.extend_from_slice(&size.to_le_bytes());
    buffer
}

/// 40 bytes: map the blob at `offset` of the shared memory region.
pub fn resource_map_blob(context: u32, resource: u32, offset: u64) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(40);
    header_3d(&mut buffer, CMD_RESOURCE_MAP_BLOB, context, None);
    u32s(&mut buffer, &[resource, 0]);
    buffer.extend_from_slice(&offset.to_le_bytes());
    buffer
}

/// 32 bytes.
pub fn resource_unmap_blob(context: u32, resource: u32) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(32);
    header_3d(&mut buffer, CMD_RESOURCE_UNMAP_BLOB, context, None);
    u32s(&mut buffer, &[resource, 0]);
    buffer
}

/// Cache type of a map-info answer.
pub fn map_info(bytes: &[u8]) -> Option<u32> {
    if response_type(bytes)? != RESP_OK_MAP_INFO {
        return None;
    }
    Some(read_u32(bytes, HEADER_SIZE)? & 0x0F)
}

/// 32 bytes, followed on the queue by `size` bytes of command stream.
pub fn submit_3d(context: u32, size: u32, fenced: Option<Fenced>) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(32);
    header_3d(&mut buffer, CMD_SUBMIT_3D, context, fenced);
    u32s(&mut buffer, &[size, 0]);
    buffer
}

/// The Venus capability set (`virgl_renderer_capset_venus`).
#[derive(Debug, Clone)]
pub struct VenusCapset {
    pub wire_format_version: u32,
    pub vk_xml_version: u32,
    pub command_serialization_version: u32,
    pub venus_protocol_version: u32,
    pub supports_blob_id_0: bool,
    /// Bit N set: the host's protocol knows Vulkan extension number N.
    /// Bit 0 set: the mask is valid at all.
    pub extension_mask: [u32; 32],
    pub allow_vk_wait_syncs: bool,
    pub supports_multiple_timelines: bool,
    pub use_guest_vram: bool,
}

impl VenusCapset {
    /// Decodes the answer to `GET_CAPSET`, field by field.
    pub fn parse(bytes: &[u8]) -> Option<VenusCapset> {
        if response_type(bytes)? != RESP_OK_CAPSET {
            return None;
        }
        let at = |index: usize| read_u32(bytes, HEADER_SIZE + index * 4);
        let mut extension_mask = [0u32; 32];
        for (index, word) in extension_mask.iter_mut().enumerate() {
            *word = at(5 + index)?;
        }
        Some(VenusCapset {
            wire_format_version: at(0)?,
            vk_xml_version: at(1)?,
            command_serialization_version: at(2)?,
            venus_protocol_version: at(3)?,
            supports_blob_id_0: at(4)? != 0,
            extension_mask,
            // Fields added after the first version read as zero on an
            // older host, which is exactly "not supported".
            allow_vk_wait_syncs: at(37).unwrap_or(0) != 0,
            supports_multiple_timelines: at(38).unwrap_or(0) != 0,
            use_guest_vram: at(39).unwrap_or(0) != 0,
        })
    }

    /// Does the host's protocol know Vulkan extension number `number`?
    pub fn knows_extension(&self, number: u32) -> bool {
        if self.extension_mask[0] & 1 == 0 {
            // No valid mask: every extension is assumed known.
            return true;
        }
        let word = (number / 32) as usize;
        word < 32 && self.extension_mask[word] & (1 << (number % 32)) != 0
    }
}

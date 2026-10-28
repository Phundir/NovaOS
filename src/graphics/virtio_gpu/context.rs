//! virtio-gpu in 3D: contexts, blobs, fenced submissions.
//!
//! ## What this layer is, and is not
//!
//! It is the transport of the Nova graphics stack's built-in backend
//! (spec 8.5): the handful of virtio-gpu commands that carry a Venus
//! stream to the host and host memory back to the guest. It knows nothing
//! of Vulkan -- `crate::gpu::venus` encodes the streams -- and nothing of
//! programs -- `crate::gpu` owns the handles, the accounting and the
//! validation. What it guarantees is narrower: a context, a blob or a
//! submission it hands out is the device's, correctly laid out, and
//! answered or reported lost.
//!
//! ## Contexts
//!
//! A virtio-gpu context is a separate protocol session on the host; with
//! `CONTEXT_INIT` and the Venus capset, a separate **Vulkan instance** in
//! a separate render-server process. The GPU core opens one per Nova GPU
//! context, so that a program's errors -- or a hang of its shaders --
//! stay in its own session on the host too.
//!
//! ## Fenced submissions
//!
//! `SUBMIT_3D` with `FLAG_FENCE | FLAG_INFO_RING_IDX` is answered by the
//! host only when the fence retires on timeline `ring_idx`:
//!
//! - ring 0 is the host's CPU timeline: the fence retires as soon as the
//!   stream has been decoded -- and its replies written. That is how a
//!   command awaiting a reply is waited for.
//! - ring N > 0 is the timeline of the Vulkan queue bound to it
//!   (`VkDeviceQueueTimelineInfoMESA`): the fence retires once the GPU
//!   has finished everything submitted to that queue before it. That is
//!   a GPU fence, delivered as a virtio answer and an MSI-X interrupt.
//!
//! ## Waiting
//!
//! Short commands (create, map) are answered by QEMU at once and are run
//! synchronously, lock held, like the 2D ones. A fenced submission may be
//! answered hundreds of milliseconds later -- a `vkCreateDevice` on the
//! host, a long draw: `wait` sleeps on `DONE`, which the GPU task
//! notifies when the device returns requests.

use alloc::string::String;
use alloc::vec::Vec;

use super::hostmem::{self, Hostmem};
use super::proto::{self, Fenced, VenusCapset};
use super::queue::{Exchange, Ticket, MAX_REQUEST};
use super::transport::{Negotiated, FEATURES_3D};
use super::{Gpu, Request, GPU};
use crate::posix::wait::WaitQueue;

/// Requests of the 3D side returned by the device. Waiters re-test their
/// own ticket; the queue says only "something came back".
pub static DONE: WaitQueue = WaitQueue::new();

/// Largest command stream one submission carries: the queue's largest
/// request, minus the `SUBMIT_3D` header.
pub const MAX_STREAM: usize = MAX_REQUEST - 32;

/// The 3D state of the device.
pub struct ThreeD {
    pub venus: VenusCapset,
    hostmem: Hostmem,
    next_context: u32,
    next_fence: u64,
    contexts: usize,
    blobs: usize,
    submissions: u64,
}

impl ThreeD {
    pub fn summary(&self) -> String {
        let xml = self.venus.vk_xml_version;
        alloc::format!(
            "3D Venus - format {} (vk.xml {}.{}.{}), protocole Venus v{}, delimitations multiples {}, region partagee {} Mio a 0x{:x} ({} Mio libres), {} contexte(s), {} blob(s), {} soumission(s)",
            self.venus.wire_format_version,
            (xml >> 22) & 0x7F,
            (xml >> 12) & 0x3FF,
            xml & 0xFFF,
            self.venus.venus_protocol_version,
            if self.venus.supports_multiple_timelines { "oui" } else { "non" },
            self.hostmem.length >> 20,
            self.hostmem.base,
            self.hostmem.available() >> 20,
            self.contexts,
            self.blobs,
            self.submissions
        )
    }
}

/// Reads the capability sets and the shared region, right after bring-up.
pub(super) fn discover(gpu: &mut Gpu, negotiated: &Negotiated) -> Result<ThreeD, &'static str> {
    if negotiated.features & FEATURES_3D != FEATURES_3D {
        return Err("fonctions 3D non proposees par l'hote (virtio-vga-gl, blob=true, venus=true)");
    }
    let (base, length) = negotiated
        .shared_memory
        .ok_or("pas de region de memoire partagee (hostmem=)")?;
    // `num_capsets`, offset 12 of the device configuration (5.7.4).
    let count = gpu.transport.device_read32(12).min(16);
    let mut venus = None;
    for index in 0..count {
        let request = proto::get_capset_info(index);
        let answers = gpu
            .run(&[Exchange { request: &[&request], response: proto::CAPSET_INFO_SIZE }])
            .ok_or("GET_CAPSET_INFO sans reponse")?;
        let Some((id, version, size)) = proto::capset_info(&answers[0]) else {
            continue;
        };
        if id != proto::CAPSET_VENUS {
            continue;
        }
        let size = (size as usize).min(proto::CAPSET_MAX);
        let request = proto::get_capset(id, version);
        let answers = gpu
            .run(&[Exchange { request: &[&request], response: proto::HEADER_SIZE + size }])
            .ok_or("GET_CAPSET sans reponse")?;
        venus = VenusCapset::parse(&answers[0]);
    }
    let venus = venus.ok_or("l'hote ne propose pas le protocole Venus (venus=true)")?;
    if venus.wire_format_version == 0 {
        return Err("capset Venus illisible");
    }
    // The page tables of the window's first megabytes, made now: every
    // session maps its reply stream there, and the tables would otherwise
    // be allocated by the first one -- whenever that is.
    // SAFETY: the page is the region's own, unmapped again at once.
    unsafe {
        if hostmem::map_kernel(base, 0, 4096, proto::MAP_CACHE_CACHED).is_some() {
            hostmem::unmap_kernel(0, 4096);
        }
    }
    Ok(ThreeD {
        venus,
        hostmem: Hostmem::new(base, length),
        next_context: 1,
        next_fence: 1,
        contexts: 0,
        blobs: 0,
        submissions: 0,
    })
}

/// Runs `f` on the device and its 3D state, lock held.
fn with<R>(f: impl FnOnce(&mut Gpu) -> Result<R, &'static str>) -> Result<R, &'static str> {
    let mut guard = GPU.lock();
    let gpu = guard.as_mut().ok_or("pas de virtio-gpu")?;
    if gpu.broken {
        return Err("peripherique hors service");
    }
    if gpu.three_d.is_none() {
        return Err("pas de 3D");
    }
    f(gpu)
}

fn three_d(gpu: &mut Gpu) -> &mut ThreeD {
    // `with` checked it.
    gpu.three_d.as_mut().expect("3D verifiee par with")
}

/// Is the 3D side up?
pub fn available() -> bool {
    GPU.lock().as_ref().is_some_and(|gpu| gpu.three_d.is_some() && !gpu.broken)
}

/// The host's Venus capability set.
pub fn capset() -> Option<VenusCapset> {
    GPU.lock().as_ref().and_then(|gpu| gpu.three_d.as_ref().map(|t| t.venus.clone()))
}

/// Does the device interrupt when it returns requests?
pub fn interrupts() -> bool {
    GPU.lock().as_ref().is_some_and(|gpu| gpu.interrupts)
}

/// Runs commands answered with `OK_NODATA`, waiting with the lock
/// released (`super::transact`). `build` reserves what it names -- an
/// identifier, a range of the region -- under the lock, so that two
/// callers never pick the same.
fn nodata<R>(what: &'static str, build: impl FnOnce(&mut Gpu) -> Result<(Vec<Vec<Vec<u8>>>, R), &'static str>) -> Result<R, &'static str> {
    let (answers, extra) = super::transact(|gpu| {
        if gpu.three_d.is_none() {
            return Err("pas de 3D");
        }
        let (requests, extra) = build(gpu)?;
        Ok((requests.into_iter().map(Request::nodata).collect(), extra))
    })?;
    if super::all_ok(&answers) { Ok(extra) } else { Err(what) }
}

/// Posts commands whose answers nobody needs: the destructions. The
/// control queue runs in order, so whatever is sent after them -- a new
/// mapping at the same place of the region -- comes after them on the
/// host too. Never waits: a context torn down while the host's GPU is
/// held (13.11.3) must not hold its caller.
fn post(requests: &[Vec<u8>]) {
    let mut guard = GPU.lock();
    let Some(gpu) = guard.as_mut() else {
        return;
    };
    if gpu.broken {
        return;
    }
    let parts: Vec<[&[u8]; 1]> = requests.iter().map(|request| [request.as_slice()]).collect();
    let exchanges: Vec<Exchange> = parts.iter().map(|part| Exchange { request: part, response: proto::HEADER_SIZE }).collect();
    gpu.queue.post(&exchanges);
}

/// Opens a Venus context. `name` shows in the host's log.
pub fn create_context(name: &str) -> Result<u32, &'static str> {
    let id = nodata("CTX_CREATE refuse", |gpu| {
        let state = three_d(gpu);
        let id = state.next_context;
        state.next_context = state.next_context.wrapping_add(1).max(1);
        Ok((alloc::vec![alloc::vec![proto::ctx_create(id, proto::CAPSET_VENUS, name)]], id))
    })?;
    if let Some(gpu) = GPU.lock().as_mut() {
        three_d(gpu).contexts += 1;
    }
    Ok(id)
}

/// Closes a context: the host destroys its Vulkan instance and everything
/// in it, and detaches the blobs it held.
pub fn destroy_context(context: u32) {
    post(&[proto::ctx_destroy(context)]);
    if let Some(gpu) = GPU.lock().as_mut()
        && let Some(state) = gpu.three_d.as_mut()
    {
        state.contexts = state.contexts.saturating_sub(1);
    }
}

/// What a blob is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobUse {
    /// Mapped in the shared region, for the CPU.
    Mapped,
    /// Attached to other contexts; never mapped (the memory need not be
    /// host-visible).
    Shared,
}

/// Creates a blob of host memory in `context`: `blob_id` 0 for a fresh
/// shared-memory buffer, otherwise the Venus object id of a
/// `VkDeviceMemory` of the context. Returns the resource id.
pub fn create_blob(context: u32, blob_id: u64, size: u64, usage: BlobUse) -> Result<u32, &'static str> {
    let resource = nodata("RESOURCE_CREATE_BLOB refuse", |gpu| {
        let resource = gpu.next_resource;
        gpu.next_resource = gpu.next_resource.wrapping_add(1).max(1);
        let flags = match usage {
            BlobUse::Mapped => proto::BLOB_FLAG_MAPPABLE,
            BlobUse::Shared => proto::BLOB_FLAG_SHAREABLE,
        };
        Ok((alloc::vec![alloc::vec![proto::resource_create_blob(context, resource, flags, blob_id, size)]], resource))
    })?;
    if let Some(gpu) = GPU.lock().as_mut()
        && let Some(state) = gpu.three_d.as_mut()
    {
        state.blobs += 1;
    }
    Ok(resource)
}

/// Where a blob shows in the shared region.
#[derive(Debug, Clone, Copy)]
pub struct Mapping {
    pub resource: u32,
    /// Offset in the region, physical address, whole pages.
    pub offset: u64,
    pub phys: u64,
    pub size: u64,
    /// `proto::MAP_CACHE_*`.
    pub cache: u32,
}

/// Maps a blob in the shared region.
pub fn map_blob(context: u32, resource: u32, size: u64) -> Result<Mapping, &'static str> {
    let pages = (size + 4095) & !4095;
    let answer = super::transact(|gpu| {
        let state = gpu.three_d.as_mut().ok_or("pas de 3D")?;
        let offset = state.hostmem.allocate(size).ok_or("region partagee pleine")?;
        let request = Request::new(alloc::vec![proto::resource_map_blob(context, resource, offset)], proto::MAP_INFO_SIZE);
        Ok((alloc::vec![request], (offset, state.hostmem.base + offset)))
    });
    let (answers, (offset, phys)) = answer?;
    match proto::map_info(&answers[0]) {
        Some(cache) => Ok(Mapping { resource, offset, phys, size: pages, cache }),
        None => {
            if let Some(gpu) = GPU.lock().as_mut()
                && let Some(state) = gpu.three_d.as_mut()
            {
                state.hostmem.release(offset, pages);
            }
            Err("RESOURCE_MAP_BLOB refuse")
        }
    }
}

/// Unmaps a blob from the shared region and gives its range back.
///
/// The caller guarantees nothing maps the range any more: neither the
/// kernel window nor a program.
pub fn unmap_blob(context: u32, mapping: Mapping) {
    post(&[proto::resource_unmap_blob(context, mapping.resource)]);
    // Given back at once: the unmap is ahead in the queue of anything that
    // maps this range again.
    if let Some(gpu) = GPU.lock().as_mut()
        && let Some(state) = gpu.three_d.as_mut()
    {
        state.hostmem.release(mapping.offset, mapping.size);
    }
}

/// Maps a mapped blob in the kernel window. Returns its virtual address.
pub fn map_kernel(mapping: &Mapping) -> Option<u64> {
    // SAFETY: `mapping` is a blob the host maps at this offset, until
    // `unmap_blob`, which the caller does only after `unmap_kernel`.
    unsafe { hostmem::map_kernel(mapping.phys, mapping.offset, mapping.size, mapping.cache) }
}

/// Removes the kernel mapping of a blob.
pub fn unmap_kernel(mapping: &Mapping) {
    // SAFETY: the caller stops using the range before this.
    unsafe { hostmem::unmap_kernel(mapping.offset, mapping.size) };
}

/// Destroys a blob: detached from every context, its host memory freed
/// once no context uses it.
pub fn destroy_resource(resource: u32) {
    post(&[proto::resource_unref(resource)]);
    if let Some(gpu) = GPU.lock().as_mut()
        && let Some(state) = gpu.three_d.as_mut()
    {
        state.blobs = state.blobs.saturating_sub(1);
    }
}

/// Makes a shareable blob of another context visible to `context`.
pub fn attach_resource(context: u32, resource: u32) -> Result<(), &'static str> {
    nodata("CTX_ATTACH_RESOURCE refuse", |_| {
        Ok((alloc::vec![alloc::vec![proto::ctx_attach_resource(context, resource)]], ()))
    })
}

pub fn detach_resource(context: u32, resource: u32) {
    post(&[proto::ctx_detach_resource(context, resource)]);
}

/// Which fence a submission carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fence {
    /// Answered as soon as QEMU has handed the stream on.
    None,
    /// Answered once the host has decoded the stream (replies written).
    Decoded,
    /// Answered once the GPU queue bound to this timeline is done.
    Queue(u8),
}

/// A submission in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Submission(Ticket);

/// Sends a Venus stream to `context`.
pub fn submit(context: u32, stream: &[u8], fence: Fence) -> Result<Submission, &'static str> {
    if stream.len() > MAX_STREAM {
        return Err("flux Venus trop long");
    }
    with(|gpu| {
        let state = three_d(gpu);
        let fenced = match fence {
            Fence::None => None,
            Fence::Decoded => Some(Fenced { fence_id: state.next_fence, ring: 0 }),
            Fence::Queue(ring) => Some(Fenced { fence_id: state.next_fence, ring }),
        };
        if fenced.is_some() {
            state.next_fence += 1;
        }
        let header = proto::submit_3d(context, stream.len() as u32, fenced);
        let tickets = gpu.queue.submit(&[Exchange { request: &[&header, stream], response: proto::HEADER_SIZE }]);
        let ticket = tickets.into_iter().next().flatten().ok_or("file de controle pleine")?;
        three_d(gpu).submissions += 1;
        Ok(Submission(ticket))
    })
}

/// Has a submission been answered? `Some(Ok)` once accepted and (for a
/// fenced one) retired, `Some(Err)` if the host refused it; its slot is
/// freed either way. `None` while in flight.
pub fn poll(submission: Submission) -> Option<Result<(), u32>> {
    let mut guard = GPU.lock();
    let gpu = guard.as_mut()?;
    gpu.queue.reap();
    let answer = gpu.queue.take(submission.0)?;
    match proto::response_type(&answer) {
        Some(proto::RESP_OK_NODATA) => Some(Ok(())),
        Some(kind) => Some(Err(kind)),
        None => Some(Err(0)),
    }
}

/// Gives up on a submission's answer.
pub fn forget(submission: Submission) {
    if let Some(gpu) = GPU.lock().as_mut() {
        gpu.queue.forget(submission.0);
    }
}

/// Waits for a submission, sleeping, at most `timeout_ms`.
pub fn wait(submission: Submission, timeout_ms: u64) -> Result<(), &'static str> {
    let deadline = crate::idt::fine_ticks() + timeout_ms;
    let outcome = crate::posix::wait::wait_until(&[&DONE], Some(deadline), || poll(submission));
    match outcome {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err("soumission refusee par l'hote"),
        Err(_) => {
            forget(submission);
            Err("soumission sans reponse")
        }
    }
}

/// Reaps the used ring and wakes whoever waits on it. Called by the GPU
/// task after each interrupt (or each tick, when the queue is polled).
pub fn service() -> usize {
    let returned = match GPU.lock().as_mut() {
        Some(gpu) => gpu.queue.reap(),
        None => 0,
    };
    // Waiters are woken even when nothing returned: a poll that found
    // nothing costs a look, a lost wake-up costs a timeout.
    DONE.notify_all();
    returned
}

/// Requests in flight, and free slots, for the monitor.
pub fn queue_state() -> (usize, usize) {
    GPU.lock()
        .as_ref()
        .map(|gpu| (gpu.queue.in_flight(), gpu.queue.free_slots()))
        .unwrap_or((0, 0))
}

/// Streams too long for one submission are split by the encoder; this is
/// the bound it splits at.
pub fn max_stream() -> usize {
    MAX_STREAM
}

/// The shared region: physical base and length.
pub fn region() -> Option<(u64, u64)> {
    GPU.lock()
        .as_ref()
        .and_then(|gpu| gpu.three_d.as_ref().map(|t| (t.hostmem.base, t.hostmem.length)))
}

/// The `map_info` cache type whose page flags `hostmem::cache_flags`
/// gives -- the way back, for a window described by its flags.
pub fn cache_of_flags(page_flags: u64) -> u32 {
    if page_flags == 0 {
        proto::MAP_CACHE_CACHED
    } else if page_flags & crate::memory::PAT_4K != 0 {
        proto::MAP_CACHE_WC
    } else {
        proto::MAP_CACHE_UNCACHED
    }
}

/// Bytes of the shared region not handed out.
pub fn hostmem_available() -> u64 {
    GPU.lock()
        .as_ref()
        .and_then(|gpu| gpu.three_d.as_ref().map(|t| t.hostmem.available()))
        .unwrap_or(0)
}

/// The hostmem allocator of the running device, for the self-test.
pub fn hostmem_selftest() -> Vec<(u64, u64)> {
    let mut region = Hostmem::new(0, 1 << 20);
    let a = region.allocate(4096).unwrap_or(u64::MAX);
    let b = region.allocate(10_000).unwrap_or(u64::MAX);
    let c = region.allocate(4096).unwrap_or(u64::MAX);
    region.release(b, 10_000);
    let d = region.allocate(8192).unwrap_or(u64::MAX);
    region.release(a, 4096);
    region.release(c, 4096);
    region.release(d, 8192);
    alloc::vec![(a, b), (c, d), (region.available(), 1 << 20)]
}

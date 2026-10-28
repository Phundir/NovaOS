//! The built-in backend: Nova objects and commands, in Vulkan, over Venus.
//!
//! ## One translation, fixed choices
//!
//! The core hands over objects described in Nova terms and command lists
//! already validated (`gpu::cmd`). This file turns them into Vulkan calls
//! for the host's driver, with a handful of choices made once for every
//! program:
//!
//! - **images live in `VK_IMAGE_LAYOUT_GENERAL`**, from their first use to
//!   their destruction. A program never names a layout, so it can never
//!   name a wrong one; the cost on current GPUs is small next to what a
//!   layout mistake costs (undefined results, not an error).
//! - **a full memory barrier separates passes**: before each render pass,
//!   before each copy, and a last one towards the host at the end of each
//!   command buffer. A program cannot write a read-after-write hazard.
//! - **resources are pushed** (`vkCmdPushDescriptorSet`): no descriptor
//!   pool or set to manage, and nothing outlives the command buffer.
//! - **rendering is dynamic** (`vkCmdBeginRendering`): no render pass or
//!   framebuffer objects.
//! - **every image has its own memory** unless the program places it: the
//!   size the host needs is only known after creation, and the core charges
//!   it then.
//!
//! ## Round trips
//!
//! Creating an object waits for the host once or twice (its result, its
//! memory requirements): creation is not on the frame path. Submitting
//! work never waits: the stream goes out with a fence on the queue's
//! timeline, and `poll` reads the answer when the GPU is done.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use super::cs::{Dec, Enc};
use super::protocol::*;
use super::{check, Error as VenusError, Session};
use crate::gpu::backend::{
    self, BindingKind, Blend, Command, Description, Error, Format, Id, ImageDesc, ImageRef, LoadOp, MemKind,
    ObjectKind, Rect, ResourceWrite, Topology, VertexFormat, Window, Work, WorkState,
};
use crate::graphics::virtio_gpu::context::{self as transport, Mapping, Submission};

impl From<VenusError> for Error {
    fn from(error: VenusError) -> Error {
        match error {
            VenusError::Vulkan(vk::VK_ERROR_OUT_OF_DEVICE_MEMORY) | VenusError::Vulkan(vk::VK_ERROR_OUT_OF_HOST_MEMORY) => {
                Error::NoMemory
            }
            VenusError::Vulkan(vk::VK_ERROR_FORMAT_NOT_SUPPORTED) | VenusError::Vulkan(vk::VK_ERROR_FEATURE_NOT_PRESENT) => {
                Error::Unsupported
            }
            VenusError::Lost | VenusError::Vulkan(vk::VK_ERROR_DEVICE_LOST) => Error::Lost,
            other => Error::Device(other.describe()),
        }
    }
}

type Result<T> = core::result::Result<T, Error>;

fn vk_format(format: Format) -> i32 {
    match format {
        Format::Bgra8 => vk::VK_FORMAT_B8G8R8A8_UNORM,
        Format::Rgba8 => vk::VK_FORMAT_R8G8B8A8_UNORM,
        Format::R8 => vk::VK_FORMAT_R8_UNORM,
        Format::Rgba16F => vk::VK_FORMAT_R16G16B16A16_SFLOAT,
        Format::Depth32F => vk::VK_FORMAT_D32_SFLOAT,
        Format::Rgba8Srgb => vk::VK_FORMAT_R8G8B8A8_SRGB,
        Format::Bgra8Srgb => vk::VK_FORMAT_B8G8R8A8_SRGB,
    }
}

fn aspect(format: Format) -> u32 {
    if format.is_depth() { vk::VK_IMAGE_ASPECT_DEPTH_BIT } else { vk::VK_IMAGE_ASPECT_COLOR_BIT }
}

fn whole(format: Format) -> VkImageSubresourceRange {
    VkImageSubresourceRange { aspect_mask: aspect(format), base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: 1 }
}

fn layers(format: Format) -> VkImageSubresourceLayers {
    VkImageSubresourceLayers { aspect_mask: aspect(format), mip_level: 0, base_array_layer: 0, layer_count: 1 }
}

fn load(op: LoadOp) -> i32 {
    match op {
        LoadOp::Clear => vk::VK_ATTACHMENT_LOAD_OP_CLEAR,
        LoadOp::Load => vk::VK_ATTACHMENT_LOAD_OP_LOAD,
        LoadOp::DontCare => vk::VK_ATTACHMENT_LOAD_OP_DONT_CARE,
    }
}

fn offset3(x: i32, y: i32) -> VkOffset3D {
    VkOffset3D { x, y, z: 0 }
}

fn extent3(width: u32, height: u32) -> VkExtent3D {
    VkExtent3D { width, height, depth: 1 }
}

// ============================================================
//  The device
// ============================================================

/// The host's GPU, as the first session found it.
pub struct VirtioDevice {
    description: Description,
}

impl VirtioDevice {
    /// Opens a session to learn the device, and closes it.
    pub fn probe() -> core::result::Result<VirtioDevice, &'static str> {
        let mut session = Session::open("nova-sonde").map_err(|e| e.describe())?;
        let info = session.info.clone();
        // Which formats render, sample, blit and copy.
        let mut enc = Enc::new();
        for format in Format::ALL {
            get_physical_device_format_properties(&mut enc, GENERATE_REPLY, session.physical, vk_format(format));
        }
        let reply = session.call(&enc);
        session.close();
        let reply = reply.map_err(|e| e.describe())?;
        let mut dec = Dec::new(&reply);
        let mut formats = 0u32;
        for format in Format::ALL {
            let properties = reply_get_physical_device_format_properties(&mut dec)
                .and_then(|r| r.format_properties)
                .ok_or("reponse de format illisible")?;
            let features = properties.optimal_tiling_features;
            let transfer = vk::VK_FORMAT_FEATURE_TRANSFER_SRC_BIT | vk::VK_FORMAT_FEATURE_TRANSFER_DST_BIT;
            let wanted = if format.is_depth() {
                vk::VK_FORMAT_FEATURE_DEPTH_STENCIL_ATTACHMENT_BIT | transfer
            } else {
                vk::VK_FORMAT_FEATURE_COLOR_ATTACHMENT_BIT
                    | vk::VK_FORMAT_FEATURE_COLOR_ATTACHMENT_BLEND_BIT
                    | vk::VK_FORMAT_FEATURE_SAMPLED_IMAGE_BIT
                    | vk::VK_FORMAT_FEATURE_SAMPLED_IMAGE_FILTER_LINEAR_BIT
                    | vk::VK_FORMAT_FEATURE_BLIT_SRC_BIT
                    | vk::VK_FORMAT_FEATURE_BLIT_DST_BIT
                    | transfer
            };
            if features & wanted == wanted {
                formats |= 1 << format as u32;
            }
        }
        let memory = &info.memory;
        let mut local = 0u64;
        let mut visible = 0u64;
        for heap in 0..memory.memory_heap_count.min(16) as usize {
            let size = memory.memory_heaps[heap].size;
            let device_local = memory.memory_heaps[heap].flags & vk::VK_MEMORY_HEAP_DEVICE_LOCAL_BIT != 0;
            let host_visible = (0..memory.memory_type_count.min(32) as usize).any(|t| {
                memory.memory_types[t].heap_index as usize == heap
                    && memory.memory_types[t].property_flags & vk::VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT != 0
            });
            if device_local {
                local += size;
            }
            if host_visible {
                visible += size;
            }
        }
        Ok(VirtioDevice {
            description: Description {
                name: info.name.clone(),
                vendor_id: info.vendor_id,
                device_id: info.device_id,
                max_image_size: info.limits.max_image_dimension2_d.min(16384),
                max_push_constants: info.limits.max_push_constants_size.min(256),
                local_memory: local,
                visible_memory: visible,
                formats,
                sharing: info.dma_buf,
                buffer_alignment: 256,
            },
        })
    }
}

impl backend::Device for VirtioDevice {
    fn describe(&self) -> Description {
        self.description.clone()
    }

    fn open(&self, name: &str) -> Result<Box<dyn backend::Context>> {
        let session = Session::open(name)?;
        Ok(Box::new(VenusContext {
            session,
            feedback: None,
            retired: Vec::new(),
            retired_ring: Vec::new(),
            memories: BTreeMap::new(),
            images: BTreeMap::new(),
            layouts: BTreeMap::new(),
            free_buffers: Vec::new(),
            in_flight: BTreeMap::new(),
            next_work: 1,
            exports: BTreeMap::new(),
            imports: BTreeMap::new(),
        }))
    }
}

// ============================================================
//  A context
// ============================================================

struct MemoryInfo {
    size: u64,
    type_index: u32,
    blob: Option<Mapping>,
}

struct ImageInfo {
    view: u64,
    /// Memory of its own, freed with it.
    memory: Option<u64>,
}

struct LayoutInfo {
    set_layout: u64,
    pipeline_layout: u64,
}

pub struct VenusContext {
    session: Session,
    /// The completion word, made with the first command buffer. `None` if
    /// it could not be: the virtio fence alone then says when work ends.
    feedback: Option<Feedback>,
    /// Work the completion word said was done, whose virtio answer has not
    /// come yet: the command buffer is reused only once it has.
    retired: Vec<(Submission, u64)>,
    /// Work through the ring the completion word said was done: its
    /// command buffer is reused once a later work is done too -- the
    /// queue runs in order, so the earlier buffer has then surely left the
    /// pending state.
    retired_ring: Vec<(u64, u64)>,
    memories: BTreeMap<u64, MemoryInfo>,
    images: BTreeMap<u64, ImageInfo>,
    layouts: BTreeMap<u64, LayoutInfo>,
    /// Command buffers whose work is done, ready to be recorded again.
    free_buffers: Vec<u64>,
    /// Work in flight: its virtio submission (`None` through the ring)
    /// and its command buffer.
    in_flight: BTreeMap<u64, (Option<Submission>, u64)>,
    next_work: u64,
    /// Blobs made of exported image memory: image -> resource.
    exports: BTreeMap<u64, u32>,
    /// Resources attached by an import: image -> resource.
    imports: BTreeMap<u64, u32>,
}

impl VenusContext {
    fn memory_type(&self, allowed: u32, kind: MemKind) -> Option<u32> {
        use vk::*;
        let info = &self.session.info;
        match kind {
            MemKind::Local => info.memory_type(allowed, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT),
            // Written by the CPU, read by the GPU: video memory the CPU
            // reaches through the BAR (resizable BAR) when the card has
            // some, system memory otherwise.
            MemKind::Visible => info
                .memory_type(
                    allowed,
                    VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT | VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT,
                    VK_MEMORY_PROPERTY_HOST_CACHED_BIT,
                )
                .or_else(|| {
                    info.memory_type(
                        allowed,
                        VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT,
                        VK_MEMORY_PROPERTY_HOST_CACHED_BIT,
                    )
                }),
            MemKind::Readback => info
                .memory_type(
                    allowed,
                    VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT | VK_MEMORY_PROPERTY_HOST_CACHED_BIT,
                    0,
                )
                .or_else(|| {
                    info.memory_type(allowed, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT, 0)
                }),
        }
    }

    /// Allocates `size` bytes of type `type_index`, with chained info.
    fn allocate(&mut self, size: u64, type_index: u32, next: &[VkNext]) -> Result<u64> {
        let id = self.session.id();
        let info = VkMemoryAllocateInfo { next, allocation_size: size, memory_type_index: type_index };
        let mut enc = Enc::new();
        allocate_memory(&mut enc, GENERATE_REPLY, self.session.device, &info, id);
        let reply = self.session.call(&enc)?;
        let answer = reply_allocate_memory(&mut Dec::new(&reply)).ok_or(Error::Device("reponse illisible"))?;
        check(answer.ret)?;
        Ok(id)
    }

    /// Creates the view every image gets.
    fn view_info(&self, image: u64, format: Format) -> VkImageViewCreateInfo<'static> {
        VkImageViewCreateInfo {
            image,
            view_type: vk::VK_IMAGE_VIEW_TYPE_2D,
            format: vk_format(format),
            subresource_range: whole(format),
            ..Default::default()
        }
    }

    fn image_create_info(desc: &ImageDesc, next: &'static [VkNext<'static>]) -> VkImageCreateInfo<'static> {
        let mut usage = vk::VK_IMAGE_USAGE_TRANSFER_SRC_BIT | vk::VK_IMAGE_USAGE_TRANSFER_DST_BIT;
        if desc.usage & backend::image_usage::RENDER != 0 {
            usage |= vk::VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT;
        }
        if desc.usage & backend::image_usage::SAMPLED != 0 {
            usage |= vk::VK_IMAGE_USAGE_SAMPLED_BIT;
        }
        if desc.usage & backend::image_usage::DEPTH != 0 {
            usage |= vk::VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT;
        }
        VkImageCreateInfo {
            next,
            image_type: vk::VK_IMAGE_TYPE_2D,
            format: vk_format(desc.format),
            extent: extent3(desc.width, desc.height),
            mip_levels: 1,
            array_layers: 1,
            samples: vk::VK_SAMPLE_COUNT_1_BIT as i32,
            tiling: vk::VK_IMAGE_TILING_OPTIMAL,
            usage,
            sharing_mode: vk::VK_SHARING_MODE_EXCLUSIVE,
            initial_layout: vk::VK_IMAGE_LAYOUT_UNDEFINED,
            ..Default::default()
        }
    }

    /// Creates an image and learns its memory requirements.
    fn new_image(&mut self, info: &VkImageCreateInfo) -> Result<(u64, VkMemoryRequirements)> {
        let id = self.session.id();
        let mut enc = Enc::new();
        create_image(&mut enc, GENERATE_REPLY, self.session.device, info, id);
        get_image_memory_requirements(&mut enc, GENERATE_REPLY, self.session.device, id);
        let reply = self.session.call(&enc)?;
        let mut dec = Dec::new(&reply);
        let created = reply_create_image(&mut dec).ok_or(Error::Device("reponse illisible"))?;
        check(created.ret)?;
        let requirements = reply_get_image_memory_requirements(&mut dec)
            .and_then(|r| r.memory_requirements)
            .ok_or(Error::Device("reponse illisible"))?;
        Ok((id, requirements))
    }

    fn destroy_image_now(&mut self, image: u64) {
        let Some(info) = self.images.remove(&image) else {
            return;
        };
        let mut enc = Enc::new();
        destroy_image_view(&mut enc, 0, self.session.device, info.view);
        destroy_image(&mut enc, 0, self.session.device, image);
        if let Some(memory) = info.memory {
            free_memory(&mut enc, 0, self.session.device, memory);
        }
        let _ = self.session.send(&enc);
        if let Some(resource) = self.exports.remove(&image) {
            transport::destroy_resource(resource);
        }
        if let Some(resource) = self.imports.remove(&image) {
            transport::detach_resource(self.session.context(), resource);
        }
    }

    fn view_of(&self, image: Id) -> u64 {
        self.images.get(&image).map(|i| i.view).unwrap_or(0)
    }
}

mod feedback;
mod objects;
mod record;

use feedback::Feedback;

impl backend::Context for VenusContext {
    fn alloc(&mut self, size: u64, kind: MemKind) -> Result<(Id, u64)> {
        let size = (size + 4095) & !4095;
        let type_index = self.memory_type(u32::MAX, kind).ok_or(Error::Unsupported)?;
        let id = self.allocate(size, type_index, &[])?;
        self.memories.insert(id, MemoryInfo { size, type_index, blob: None });
        Ok((id, size))
    }

    fn free(&mut self, mem: Id) {
        if let Some(info) = self.memories.remove(&mem) {
            if let Some(blob) = info.blob {
                transport::unmap_blob(self.session.context(), blob);
                transport::destroy_resource(blob.resource);
            }
            let mut enc = Enc::new();
            free_memory(&mut enc, 0, self.session.device, mem);
            let _ = self.session.send(&enc);
        }
    }

    fn map(&mut self, mem: Id) -> Result<Window> {
        let context = self.session.context();
        let info = self.memories.get_mut(&mem).ok_or(Error::Device("memoire inconnue"))?;
        if info.blob.is_none() {
            let resource = transport::create_blob(context, mem, info.size, transport::BlobUse::Mapped).map_err(Error::Device)?;
            match transport::map_blob(context, resource, info.size) {
                Ok(mapping) => info.blob = Some(mapping),
                Err(reason) => {
                    transport::destroy_resource(resource);
                    return Err(Error::Device(reason));
                }
            }
        }
        let blob = info.blob.as_ref().expect("blob cree");
        Ok(Window { phys: blob.phys, size: blob.size, cache_flags: crate::graphics::virtio_gpu::hostmem::cache_flags(blob.cache) })
    }

    fn unmap(&mut self, mem: Id) {
        let context = self.session.context();
        if let Some(info) = self.memories.get_mut(&mem)
            && let Some(blob) = info.blob.take()
        {
            transport::unmap_blob(context, blob);
            transport::destroy_resource(blob.resource);
        }
    }

    fn create_buffer(&mut self, desc: &backend::BufferDesc, mem: Id, offset: u64) -> Result<Id> {
        self.make_buffer(desc, mem, offset)
    }

    fn create_image(&mut self, desc: &ImageDesc, memory: Option<(Id, u64)>) -> Result<(Id, u64)> {
        self.make_image(desc, memory)
    }

    fn create_sampler(&mut self, desc: &backend::SamplerDesc) -> Result<Id> {
        self.make_sampler(desc)
    }

    fn create_shader(&mut self, code: &[u32]) -> Result<Id> {
        self.make_shader(code)
    }

    fn create_layout(&mut self, desc: &backend::LayoutDesc) -> Result<Id> {
        self.make_layout(desc)
    }

    fn create_pipeline(&mut self, desc: &backend::PipelineDesc) -> Result<Id> {
        self.make_pipeline(desc)
    }

    fn destroy(&mut self, kind: ObjectKind, id: Id) {
        let device = self.session.device;
        let mut enc = Enc::new();
        match kind {
            ObjectKind::Buffer => destroy_buffer(&mut enc, 0, device, id),
            ObjectKind::Image => {
                self.destroy_image_now(id);
                return;
            }
            ObjectKind::Sampler => destroy_sampler(&mut enc, 0, device, id),
            ObjectKind::Shader => destroy_shader_module(&mut enc, 0, device, id),
            ObjectKind::Layout => {
                if let Some(layout) = self.layouts.remove(&id) {
                    destroy_pipeline_layout(&mut enc, 0, device, layout.pipeline_layout);
                    destroy_descriptor_set_layout(&mut enc, 0, device, layout.set_layout);
                }
            }
            ObjectKind::Pipeline => destroy_pipeline(&mut enc, 0, device, id),
        }
        let _ = self.session.send(&enc);
    }

    fn submit(&mut self, commands: &[Command]) -> Result<Work> {
        self.record(commands)
    }

    fn poll(&mut self, work: Work) -> WorkState {
        self.collect_retired();
        let Some(&(submission, buffer)) = self.in_flight.get(&work.0) else {
            return WorkState::Done;
        };
        let Some(submission) = submission else {
            // Through the ring: the completion word, or the ring's fatal
            // state, is all there is to look at.
            if self.feedback_reached(work.0) {
                self.in_flight.remove(&work.0);
                self.retired_ring.push((work.0, buffer));
                return WorkState::Done;
            }
            if self.session.is_lost() {
                self.in_flight.remove(&work.0);
                return WorkState::Failed;
            }
            return WorkState::Pending;
        };
        match transport::poll(submission) {
            None if self.feedback_reached(work.0) => {
                self.in_flight.remove(&work.0);
                self.retired.push((submission, buffer));
                WorkState::Done
            }
            None => WorkState::Pending,
            Some(Ok(())) => {
                self.in_flight.remove(&work.0);
                self.free_buffers.push(buffer);
                WorkState::Done
            }
            Some(Err(_)) => {
                self.in_flight.remove(&work.0);
                self.session.mark_lost();
                WorkState::Failed
            }
        }
    }

    fn export_image(&mut self, image: Id) -> Result<u32> {
        if let Some(resource) = self.exports.get(&image) {
            return Ok(*resource);
        }
        let memory = self.images.get(&image).and_then(|i| i.memory).ok_or(Error::Unsupported)?;
        if !self.session.info.dma_buf {
            return Err(Error::Unsupported);
        }
        let size = self.memory_size_of(image)?;
        let resource = transport::create_blob(self.session.context(), memory, size, transport::BlobUse::Shared).map_err(Error::Device)?;
        self.exports.insert(image, resource);
        Ok(resource)
    }

    fn import_image(&mut self, shared: u32, desc: &ImageDesc) -> Result<Id> {
        transport::attach_resource(self.session.context(), shared).map_err(Error::Device)?;
        static EXTERNAL: [VkNext<'static>; 1] = [VkNext::ExternalMemoryImageCreateInfo(VkExternalMemoryImageCreateInfo {
            handle_types: vk::VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
        })];
        let info = Self::image_create_info(desc, &EXTERNAL);
        let (id, requirements) = match self.new_image(&info) {
            Ok(result) => result,
            Err(error) => {
                transport::detach_resource(self.session.context(), shared);
                return Err(error);
            }
        };
        let type_index = self.memory_type(requirements.memory_type_bits, MemKind::Local).unwrap_or(0);
        let chain = [
            VkNext::ImportMemoryResourceInfoMESA(VkImportMemoryResourceInfoMESA { resource_id: shared }),
            VkNext::MemoryDedicatedAllocateInfo(VkMemoryDedicatedAllocateInfo { image: id, buffer: 0 }),
        ];
        let memory = match self.allocate(requirements.size, type_index, &chain) {
            Ok(memory) => memory,
            Err(error) => {
                let mut enc = Enc::new();
                destroy_image(&mut enc, 0, self.session.device, id);
                let _ = self.session.send(&enc);
                transport::detach_resource(self.session.context(), shared);
                return Err(error);
            }
        };
        let view = self.session.id();
        let view_info = self.view_info(id, desc.format);
        let mut enc = Enc::new();
        bind_image_memory(&mut enc, GENERATE_REPLY, self.session.device, id, memory, 0);
        create_image_view(&mut enc, GENERATE_REPLY, self.session.device, &view_info, view);
        let reply = self.session.call(&enc)?;
        let mut dec = Dec::new(&reply);
        let bound = reply_bind_image_memory(&mut dec).map(|r| r.ret).unwrap_or(vk::VK_ERROR_UNKNOWN);
        let viewed = reply_create_image_view(&mut dec).map(|r| r.ret).unwrap_or(vk::VK_ERROR_UNKNOWN);
        self.images.insert(id, ImageInfo { view, memory: Some(memory) });
        self.imports.insert(id, shared);
        if bound < 0 || viewed < 0 {
            self.destroy_image_now(id);
            return Err(VenusError::Vulkan(bound.min(viewed)).into());
        }
        Ok(id)
    }

    fn close(&mut self) {
        // Work the completion word reported done may not have had its
        // virtio answer yet: nobody will take it now, so its queue slot is
        // freed when it comes rather than held for ever.
        for (submission, _) in core::mem::take(&mut self.retired) {
            transport::forget(submission);
        }
        if let Some(feedback) = self.feedback.take() {
            transport::unmap_kernel(&feedback.mapping);
            self.free(feedback.memory);
        }
        self.session.close();
    }
}

impl VenusContext {
    /// The size of an image's own memory, asked of the host again.
    fn memory_size_of(&mut self, image: Id) -> Result<u64> {
        let mut enc = Enc::new();
        get_image_memory_requirements(&mut enc, GENERATE_REPLY, self.session.device, image);
        let reply = self.session.call(&enc)?;
        let requirements = reply_get_image_memory_requirements(&mut Dec::new(&reply))
            .and_then(|r| r.memory_requirements)
            .ok_or(Error::Device("reponse illisible"))?;
        Ok((requirements.size + 4095) & !4095)
    }
}

fn stages(bits: u32) -> u32 {
    let mut out = 0;
    if bits & backend::stage::VERTEX != 0 {
        out |= vk::VK_SHADER_STAGE_VERTEX_BIT;
    }
    if bits & backend::stage::FRAGMENT != 0 {
        out |= vk::VK_SHADER_STAGE_FRAGMENT_BIT;
    }
    out
}

//! Venus sessions: one Vulkan device of the host per Nova GPU context.
//!
//! ## What a session is
//!
//! A virtio-gpu context speaking the Venus protocol
//! (`graphics::virtio_gpu::context`), and in it the objects every Nova GPU
//! context starts with: a Vulkan instance, the physical device chosen, a
//! logical device with one graphics queue, and a command pool. What the
//! program then creates -- memory, images, pipelines -- lives in that
//! device, named by object ids the session hands out. The ids are the
//! guest's to choose (Venus lets the driver name the host's objects), so
//! creating an object never waits for the host to answer with a handle.
//!
//! ## Replies
//!
//! A command that answers something -- a `VkResult`, a memory requirement,
//! a list of physical devices -- sets `GENERATE_REPLY` and the host writes
//! the reply in the session's reply stream: a blob of shared memory
//! (`blob_id` 0, a memfd on the host) mapped in the kernel window. Before a
//! batch the stream is rewound (`vkSeekReplyCommandStreamMESA(0)`); the
//! batch is submitted with a CPU fence (ring 0), answered once the host has
//! decoded it; the replies are then read in order, from the start.
//!
//! ## The command ring (13.5.2)
//!
//! Every batch goes through the session's **ring**: a region of shared
//! memory (a second `blob_id` 0 blob) where the kernel writes the Venus
//! stream and moves a `tail`, and where a thread of the host's renderer
//! reads it and moves a `head` after each command
//! (`vkCreateRingMESA`, VK_MESA_venus_protocol). Nothing crosses QEMU on
//! the way: no virtio request per batch, and -- what 13.11.3 showed to
//! matter -- no wait behind QEMU's main loop, which a program holding the
//! host's GPU can block for seconds. A call is answered once `head` has
//! passed it; its replies are then in the reply stream, which the ring has
//! its own copy of the settings for. The host's thread sleeps after
//! `RING_IDLE_NS` without work and says so in `status`; the next batch
//! then wakes it with `vkNotifyRingMESA`, the one command that still goes
//! through the control queue. A ring the host cannot create leaves the
//! session on the control queue, as it was before.
//!
//! ## Work
//!
//! Recorded command buffers and their `vkQueueSubmit` go out like any
//! batch. The end of their work is seen in a word the GPU writes last
//! (`backend`, the completion word); without a ring, they go out with a
//! fence on the queue's own timeline (ring 1, bound by
//! `VkDeviceQueueTimelineInfoMESA`), which QEMU answers when the GPU has
//! finished -- about ten milliseconds late, QEMU looking at the host's
//! fences on a timer.
//!
//! ## Why a device per context
//!
//! A shared device would be cheaper to open, but a shader that hangs, or a
//! stream the host cannot decode, loses the whole device -- every program
//! at once, the compositor included. One device per context keeps such a
//! loss inside the program that caused it (spec 6.8, "contexte perdu").


pub mod backend;
pub mod cs;
pub mod protocol;
mod ring;

pub use ring::{RING_BATCHES, RING_REFUSED, RING_WAKEUPS};
use ring::Ring;

use alloc::string::String;
use alloc::vec::Vec;

use crate::graphics::virtio_gpu::context::{self as transport, Fence, Mapping, Submission};
use cs::{Dec, Enc};
use protocol::*;

/// Size of the reply stream. The longest reply the session reads is the
/// device extension list: about two hundred entries of 268 bytes.
const REPLY_SIZE: u64 = 256 * 1024;

/// Bytes of replies `Session::call` returns.
const SMALL_REPLY: usize = 4096;

/// The timeline the session's queue is bound to.
pub const QUEUE_RING: u8 = 1;

/// Longest wait for a reply: `vkCreateDevice` loads the host's driver.
const REPLY_TIMEOUT_MS: u64 = 5000;

/// The Vulkan version the session asks for: 1.3 brings dynamic rendering
/// and synchronization2 into the core, which the translation relies on.
const API_VERSION_1_3: u32 = (1 << 22) | (3 << 12);

/// What the core needs to know of the device a session runs on.
#[derive(Clone)]
pub struct DeviceInfo {
    pub name: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub device_type: i32,
    pub api_version: u32,
    pub driver_version: u32,
    pub queue_family: u32,
    pub memory: VkPhysicalDeviceMemoryProperties,
    pub limits: VkPhysicalDeviceLimits,
    /// Push descriptors come from the core (1.4) or from the extension.
    pub push_descriptor_extension: bool,
    /// Memory can be exported as a dma-buf -- the condition for sharing
    /// an image with another context (`CTX_ATTACH_RESOURCE` carries only
    /// dma-bufs and shared memory).
    pub dma_buf: bool,
    pub robust_buffer_access: bool,
}

impl DeviceInfo {
    /// A memory type with all of `wanted`, allowed by `allowed` (a
    /// `memoryTypeBits` mask). Prefers one with none of `avoid`.
    pub fn memory_type(&self, allowed: u32, wanted: u32, avoid: u32) -> Option<u32> {
        let count = self.memory.memory_type_count.min(32);
        let fits = |index: u32| {
            allowed & (1 << index) != 0
                && self.memory.memory_types[index as usize].property_flags & wanted == wanted
        };
        (0..count)
            .find(|&index| fits(index) && self.memory.memory_types[index as usize].property_flags & avoid == 0)
            .or_else(|| (0..count).find(|&index| fits(index)))
    }
}

/// Why a session call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The transport refused or lost the request.
    Transport(&'static str),
    /// The encoder was handed inconsistent values: nothing was sent.
    Encoding,
    /// A reply was missing or malformed.
    Reply,
    /// The host's Vulkan answered with this `VkResult`.
    Vulkan(i32),
    /// The session is lost; every call fails from now on.
    Lost,
}

impl Error {
    pub fn describe(&self) -> &'static str {
        match self {
            Error::Transport(reason) => reason,
            Error::Encoding => "flux Venus incoherent",
            Error::Reply => "reponse Venus illisible",
            Error::Vulkan(vk::VK_ERROR_OUT_OF_DEVICE_MEMORY) => "memoire du GPU epuisee",
            Error::Vulkan(vk::VK_ERROR_OUT_OF_HOST_MEMORY) => "memoire de l'hote epuisee",
            Error::Vulkan(vk::VK_ERROR_DEVICE_LOST) => "GPU perdu",
            Error::Vulkan(_) => "refus du Vulkan de l'hote",
            Error::Lost => "session GPU perdue",
        }
    }
}

pub type Result<T> = core::result::Result<T, Error>;

pub struct Session {
    context: u32,
    reply_blob: Mapping,
    reply_va: u64,
    /// `None` when the host could not make one: the control queue then
    /// carries every batch.
    ring: Option<Ring>,
    next_id: u64,
    pub instance: u64,
    pub physical: u64,
    pub device: u64,
    pub queue: u64,
    pub pool: u64,
    pub info: DeviceInfo,
    lost: bool,
}

// The session is only ever reached under the GPU core's lock of the
// context owning it; the raw address of its reply window is what keeps it
// from being `Send` automatically.
unsafe impl Send for Session {}

impl Session {
    /// Opens a context on the host and brings a device up in it. `name`
    /// shows in the host's log.
    pub fn open(name: &str) -> Result<Session> {
        let context = transport::create_context(name).map_err(Error::Transport)?;
        let reply_blob = match open_reply(context) {
            Ok(blob) => blob,
            Err(error) => {
                transport::destroy_context(context);
                return Err(error);
            }
        };
        let Some(reply_va) = transport::map_kernel(&reply_blob) else {
            transport::unmap_blob(context, reply_blob);
            transport::destroy_resource(reply_blob.resource);
            transport::destroy_context(context);
            return Err(Error::Transport("fenetre noyau du flux de reponse"));
        };
        let mut session = Session {
            context,
            reply_blob,
            reply_va,
            ring: None,
            next_id: 1,
            instance: 0,
            physical: 0,
            device: 0,
            queue: 0,
            pool: 0,
            info: DeviceInfo {
                name: String::new(),
                vendor_id: 0,
                device_id: 0,
                device_type: 0,
                api_version: 0,
                driver_version: 0,
                queue_family: 0,
                memory: VkPhysicalDeviceMemoryProperties::default(),
                limits: VkPhysicalDeviceLimits::default(),
                push_descriptor_extension: false,
                dma_buf: false,
                robust_buffer_access: false,
            },
            lost: false,
        };
        if let Err(error) = session.bring_up() {
            session.close();
            return Err(error);
        }
        Ok(session)
    }

    /// The virtio-gpu context of the session.
    pub fn context(&self) -> u32 {
        self.context
    }

    /// A fresh object id. Ids never repeat within a session: the host
    /// refuses an id still in use, and a recycled one would let a late
    /// command name the wrong object.
    pub fn id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn is_lost(&self) -> bool {
        self.lost
    }

    pub fn mark_lost(&mut self) {
        self.lost = true;
    }

    /// Sends a batch without waiting: object creations and destructions,
    /// which answer nothing.
    pub fn send(&mut self, enc: &Enc) -> Result<()> {
        if self.lost {
            return Err(Error::Lost);
        }
        if enc.error() {
            return Err(Error::Encoding);
        }
        if enc.is_empty() {
            return Ok(());
        }
        if self.ring.is_some() {
            return self.ring_write(enc.bytes()).map(|_| ());
        }
        let submission = transport::submit(self.context, enc.bytes(), Fence::None).map_err(Error::Transport)?;
        transport::forget(submission);
        Ok(())
    }

    /// Does the session speak through a ring?
    pub fn has_ring(&self) -> bool {
        self.ring.is_some()
    }

    /// Sends a batch whose commands carry `GENERATE_REPLY`, waits for the
    /// host to decode it, and returns the first 4 KiB of the reply stream
    /// -- what every reply but the long enumerations fits in.
    pub fn call(&mut self, enc: &Enc) -> Result<Vec<u8>> {
        self.call_sized(enc, SMALL_REPLY)
    }

    /// `call`, returning `size` bytes of replies. The batch is preceded by
    /// a rewind of the stream.
    ///
    /// Only what the replies can occupy is copied out of the window: a
    /// copy of the whole stream for every object created was a quarter
    /// of a megabyte of kernel heap per call.
    pub fn call_sized(&mut self, enc: &Enc, size: usize) -> Result<Vec<u8>> {
        if self.lost {
            return Err(Error::Lost);
        }
        if enc.error() {
            return Err(Error::Encoding);
        }
        let mut stream = Enc::new();
        seek_reply_command_stream_mesa(&mut stream, 0, 0);
        let mut bytes = stream.into_bytes();
        bytes.extend_from_slice(enc.bytes());
        // Zeroed first: a reply the host did not write reads as a command
        // type of 0, which no decoder of the batch accepts.
        let used = size.min(REPLY_SIZE as usize);
        unsafe { core::ptr::write_bytes(self.reply_va as *mut u8, 0, used.min(SMALL_REPLY)) };
        if self.ring.is_some() {
            let tail = self.ring_write(&bytes)?;
            self.ring_wait(|ring| ring.head().wrapping_sub(tail) as i32 >= 0)?;
        } else {
            let submission = transport::submit(self.context, &bytes, Fence::Decoded).map_err(Error::Transport)?;
            transport::wait(submission, REPLY_TIMEOUT_MS).map_err(|reason| {
                self.lost = true;
                Error::Transport(reason)
            })?;
        }
        let mut reply = alloc::vec![0u8; used];
        // SAFETY: the reply window maps REPLY_SIZE bytes for the session's
        // life, and the host has finished writing (the fence retired).
        unsafe { core::ptr::copy_nonoverlapping(self.reply_va as *const u8, reply.as_mut_ptr(), used) };
        Ok(reply)
    }

    /// Submits recorded work. Through the ring when there is one --
    /// `None`: the end of the work is the completion word's to tell --, or
    /// on the queue's timeline, answered when the GPU has finished it.
    ///
    /// `completion_word`: the batch ends with the completion word's fill.
    /// Without it the work needs the fence, so it takes the control queue
    /// -- once the ring is drained, since it names objects the ring made.
    pub fn submit_work(&mut self, enc: &Enc, completion_word: bool) -> Result<Option<Submission>> {
        if self.lost {
            return Err(Error::Lost);
        }
        if enc.error() {
            return Err(Error::Encoding);
        }
        if self.ring.is_some() {
            if completion_word {
                return self.ring_write(enc.bytes()).map(|_| None);
            }
            let tail = self.ring.as_ref().map(|r| r.tail).unwrap_or(0);
            self.ring_wait(|ring| ring.head() == tail)?;
        }
        transport::submit(self.context, enc.bytes(), Fence::Queue(QUEUE_RING)).map(Some).map_err(Error::Transport)
    }

    /// Instance, physical device, device, queue, command pool.
    fn bring_up(&mut self) -> Result<()> {
        // --- the reply stream, then the instance ---
        let mut enc = Enc::new();
        set_reply_command_stream_mesa(
            &mut enc,
            0,
            &VkCommandStreamDescriptionMESA { resource_id: self.reply_blob.resource, offset: 0, size: REPLY_SIZE },
        );
        self.send(&enc)?;
        // The ring, then its own copy of the reply stream's settings: from
        // here on every batch goes through it.
        if let Some(ring) = self.open_ring() {
            self.ring = Some(ring);
            let mut enc = Enc::new();
            set_reply_command_stream_mesa(
                &mut enc,
                0,
                &VkCommandStreamDescriptionMESA { resource_id: self.reply_blob.resource, offset: 0, size: REPLY_SIZE },
            );
            if self.send(&enc).is_err() {
                self.ring = None;
            }
        }
        if self.ring.is_none() {
            RING_REFUSED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }

        self.instance = self.id();
        let app = VkApplicationInfo {
            application_name: Some("Nova OS"),
            application_version: 1,
            engine_name: Some("nova-gfx"),
            engine_version: 1,
            api_version: API_VERSION_1_3,
            next: &[],
        };
        let create = VkInstanceCreateInfo { application_info: Some(&app), ..Default::default() };
        let mut enc = Enc::new();
        create_instance(&mut enc, GENERATE_REPLY, &create, self.instance);
        let reply = self.call(&enc)?;
        let answer = reply_create_instance(&mut Dec::new(&reply)).ok_or(Error::Reply)?;
        check(answer.ret)?;

        // --- physical devices ---
        let mut enc = Enc::new();
        enumerate_physical_devices(&mut enc, GENERATE_REPLY, self.instance, Some(0), None);
        let reply = self.call(&enc)?;
        let answer = reply_enumerate_physical_devices(&mut Dec::new(&reply)).ok_or(Error::Reply)?;
        check(answer.ret)?;
        let count = answer.physical_device_count.unwrap_or(0).min(8);
        if count == 0 {
            return Err(Error::Vulkan(vk::VK_ERROR_INITIALIZATION_FAILED));
        }
        let ids: Vec<u64> = (0..count).map(|_| self.id()).collect();
        let mut enc = Enc::new();
        enumerate_physical_devices(&mut enc, GENERATE_REPLY, self.instance, Some(count), Some(&ids));
        for id in &ids {
            get_physical_device_properties(&mut enc, GENERATE_REPLY, *id);
        }
        let reply = self.call_sized(&enc, 64 + ids.len() * 2048)?;
        let mut dec = Dec::new(&reply);
        let answer = reply_enumerate_physical_devices(&mut dec).ok_or(Error::Reply)?;
        if answer.ret < 0 {
            return Err(Error::Vulkan(answer.ret));
        }
        let mut best: Option<(u64, VkPhysicalDeviceProperties, u32)> = None;
        for id in &ids {
            let properties = reply_get_physical_device_properties(&mut dec)
                .and_then(|answer| answer.properties)
                .ok_or(Error::Reply)?;
            if properties.api_version < API_VERSION_1_3 {
                continue;
            }
            let rank = match properties.device_type {
                vk::VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU => 4,
                vk::VK_PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU => 3,
                vk::VK_PHYSICAL_DEVICE_TYPE_VIRTUAL_GPU => 2,
                vk::VK_PHYSICAL_DEVICE_TYPE_CPU => 1,
                _ => 0,
            };
            if best.as_ref().is_none_or(|(_, _, r)| rank > *r) {
                best = Some((*id, properties, rank));
            }
        }
        let (physical, properties, _) = best.ok_or(Error::Vulkan(vk::VK_ERROR_INCOMPATIBLE_DRIVER))?;
        self.physical = physical;
        self.info.name = c_string(&properties.device_name);
        self.info.vendor_id = properties.vendor_id;
        self.info.device_id = properties.device_id;
        self.info.device_type = properties.device_type;
        self.info.api_version = properties.api_version;
        self.info.driver_version = properties.driver_version;
        self.info.limits = properties.limits;

        // --- what the chosen device offers ---
        let mut enc = Enc::new();
        get_physical_device_queue_family_properties(&mut enc, GENERATE_REPLY, physical, Some(16), true);
        get_physical_device_memory_properties(&mut enc, GENERATE_REPLY, physical);
        get_physical_device_features(&mut enc, GENERATE_REPLY, physical);
        let reply = self.call(&enc)?;
        let mut dec = Dec::new(&reply);
        let families = reply_get_physical_device_queue_family_properties(&mut dec).ok_or(Error::Reply)?;
        let memory = reply_get_physical_device_memory_properties(&mut dec)
            .and_then(|answer| answer.memory_properties)
            .ok_or(Error::Reply)?;
        let features = reply_get_physical_device_features(&mut dec)
            .and_then(|answer| answer.features)
            .ok_or(Error::Reply)?;
        let wanted = vk::VK_QUEUE_GRAPHICS_BIT | vk::VK_QUEUE_TRANSFER_BIT;
        let family = families
            .queue_family_properties
            .iter()
            .position(|family| family.queue_flags & wanted == wanted && family.queue_count > 0)
            .ok_or(Error::Vulkan(vk::VK_ERROR_FEATURE_NOT_PRESENT))?;
        self.info.queue_family = family as u32;
        self.info.memory = memory;
        self.info.robust_buffer_access = features.robust_buffer_access != 0;

        let mut enc = Enc::new();
        enumerate_device_extension_properties(&mut enc, GENERATE_REPLY, physical, None, Some(0), false);
        let reply = self.call(&enc)?;
        let count = reply_enumerate_device_extension_properties(&mut Dec::new(&reply))
            .and_then(|answer| answer.property_count)
            .unwrap_or(0)
            .min(512);
        let mut enc = Enc::new();
        enumerate_device_extension_properties(&mut enc, GENERATE_REPLY, physical, None, Some(count), true);
        let reply = self.call_sized(&enc, 64 + count as usize * 272)?;
        let extensions = reply_enumerate_device_extension_properties(&mut Dec::new(&reply)).ok_or(Error::Reply)?;
        let has = |name: &str| extensions.properties.iter().any(|p| c_string(&p.extension_name) == name);
        // The version a device offers is the smaller of the card's and
        // the instance's: this instance asks for 1.3, so the core 1.4
        // `vkCmdPushDescriptorSet` exists only through the extension --
        // even on a 1.4 card. Without it the host has no function behind
        // the command, and drops the stream at the first push.
        let effective = self.info.api_version.min(API_VERSION_1_3);
        let core_1_4 = effective >= (1 << 22) | (4 << 12);
        self.info.push_descriptor_extension = !core_1_4;
        if !core_1_4 && !has("VK_KHR_push_descriptor") {
            return Err(Error::Vulkan(vk::VK_ERROR_EXTENSION_NOT_PRESENT));
        }
        self.info.dma_buf = has("VK_EXT_external_memory_dma_buf") && has("VK_KHR_external_memory_fd");

        // --- the device ---
        self.device = self.id();
        let priority = [0x3F80_0000u32]; // 1.0
        let queue_info = [VkDeviceQueueCreateInfo {
            queue_family_index: self.info.queue_family,
            queue_priorities: &priority,
            ..Default::default()
        }];
        let mut device_extensions: Vec<&str> = Vec::new();
        if self.info.push_descriptor_extension {
            device_extensions.push("VK_KHR_push_descriptor");
        }
        if self.info.dma_buf {
            device_extensions.push("VK_KHR_external_memory_fd");
            device_extensions.push("VK_EXT_external_memory_dma_buf");
        }
        // Robustness is what makes a program's own out-of-range vertex
        // fetch or texel read harmless: the GPU returns zeros instead of
        // reading whatever lies next to the buffer.
        let enabled = VkPhysicalDeviceFeatures { robust_buffer_access: features.robust_buffer_access, ..Default::default() };
        let next = [VkNext::PhysicalDeviceVulkan13Features(VkPhysicalDeviceVulkan13Features {
            robust_image_access: 1,
            synchronization2: 1,
            dynamic_rendering: 1,
            ..Default::default()
        })];
        let create = VkDeviceCreateInfo {
            next: &next,
            queue_create_infos: &queue_info,
            enabled_extension_names: &device_extensions,
            enabled_features: Some(&enabled),
            ..Default::default()
        };
        let mut enc = Enc::new();
        create_device(&mut enc, GENERATE_REPLY, self.physical, &create, self.device);
        let reply = self.call(&enc)?;
        let answer = reply_create_device(&mut Dec::new(&reply)).ok_or(Error::Reply)?;
        check(answer.ret)?;

        // --- the queue, bound to its timeline, and the command pool ---
        self.queue = self.id();
        self.pool = self.id();
        let timeline = [VkNext::DeviceQueueTimelineInfoMESA(VkDeviceQueueTimelineInfoMESA { ring_idx: QUEUE_RING as u32 })];
        let queue_info = VkDeviceQueueInfo2 { next: &timeline, queue_family_index: self.info.queue_family, ..Default::default() };
        let pool_info = VkCommandPoolCreateInfo {
            flags: vk::VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT,
            queue_family_index: self.info.queue_family,
            ..Default::default()
        };
        let mut enc = Enc::new();
        get_device_queue2(&mut enc, 0, self.device, &queue_info, self.queue);
        create_command_pool(&mut enc, GENERATE_REPLY, self.device, &pool_info, self.pool);
        let reply = self.call(&enc)?;
        let answer = reply_create_command_pool(&mut Dec::new(&reply)).ok_or(Error::Reply)?;
        check(answer.ret)?;
        Ok(())
    }

    /// Destroys everything on the host: the context goes, and with it the
    /// device, the instance and every object. The reply stream is unmapped
    /// first -- nothing may reach its range once the host unmaps it.
    pub fn close(&mut self) {
        // The ring goes with the context on the host; its window here goes
        // first.
        if let Some(ring) = self.ring.take() {
            transport::unmap_kernel(&ring.blob);
            transport::unmap_blob(self.context, ring.blob);
            transport::destroy_resource(ring.blob.resource);
        }
        transport::unmap_kernel(&self.reply_blob);
        transport::unmap_blob(self.context, self.reply_blob);
        transport::destroy_resource(self.reply_blob.resource);
        transport::destroy_context(self.context);
        self.lost = true;
    }
}

/// The reply stream: a blob of shared memory, mapped.
fn open_reply(context: u32) -> Result<Mapping> {
    let resource = transport::create_blob(context, 0, REPLY_SIZE, transport::BlobUse::Mapped).map_err(Error::Transport)?;
    match transport::map_blob(context, resource, REPLY_SIZE) {
        Ok(mapping) => Ok(mapping),
        Err(reason) => {
            transport::destroy_resource(resource);
            Err(Error::Transport(reason))
        }
    }
}

/// A `VkResult` below zero is an error.
pub fn check(result: i32) -> Result<()> {
    if result < 0 { Err(Error::Vulkan(result)) } else { Ok(()) }
}

/// The bytes of a fixed character array up to its terminator, kept ASCII.
pub fn c_string(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take_while(|b| **b != 0)
        .map(|b| if (0x20..0x7F).contains(b) { *b as char } else { '?' })
        .collect()
}

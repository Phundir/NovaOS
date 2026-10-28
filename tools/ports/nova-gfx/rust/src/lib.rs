//! nova-gfx -- the Nova graphics interface (spec 6.8, domain 0C) for Rust.
//!
//! The Rust side of `SYS_GPU_*`, as `nova_gfx.h` is the C side, and the
//! same contract: every call is one `syscall` instruction with a Nova
//! number; no library of the host's graphics stack is involved. `core`
//! only, so a `#![no_std]` program uses it as well as one with `std`.
//!
//! ## What the types add to the header
//!
//! Handles are typed (`Context`, `Memory`, `Buffer`, `Image`...), so an
//! image cannot be passed where a buffer is wanted; every object gives
//! itself back to the kernel when dropped, a context with everything in
//! it; errors are an enum rather than a negative number. The command
//! buffer is written by `Commands` into a slice the caller owns -- no
//! allocation, as in C.
//!
//! The layouts the kernel reads (`src/gpu/api.rs`, `src/gpu/cmd.rs`) are
//! built here word by word, little-endian, exactly as the header lays its
//! structures out; the three move together.

#![no_std]

use core::marker::PhantomData;

// ---- call numbers ----
const INFO: u64 = 0x0C01;
const CONTEXT_CREATE: u64 = 0x0C02;
const CONTEXT_DESTROY: u64 = 0x0C03;
const MEM_ALLOC: u64 = 0x0C04;
const MEM_FREE: u64 = 0x0C05;
const MEM_MAP: u64 = 0x0C06;
const MEM_UNMAP: u64 = 0x0C07;
const OBJECT_CREATE: u64 = 0x0C08;
const OBJECT_DESTROY: u64 = 0x0C09;
const SUBMIT: u64 = 0x0C0A;
const FENCE_CREATE: u64 = 0x0C0B;
const FENCE_WAIT: u64 = 0x0C0C;
const FENCE_EXPORT: u64 = 0x0C0D;
const SHARE: u64 = 0x0C0E;
const IMPORT: u64 = 0x0C0F;
const PRESENT: u64 = 0x0C10;

// ---- object kinds ----
const OBJ_BUFFER: u64 = 1;
const OBJ_IMAGE: u64 = 2;
const OBJ_SAMPLER: u64 = 3;
const OBJ_SHADER: u64 = 4;
const OBJ_LAYOUT: u64 = 5;
const OBJ_PIPELINE: u64 = 6;

/// The interface version this crate speaks (`NOVA_GPU_INTERFACE`).
pub const INTERFACE: u32 = 0x0001_0000;

/// A timeout that never runs out.
pub const WAIT_FOREVER: u64 = u64::MAX;

/// One `syscall` instruction: number in `rax`, arguments in `rdi`, `rsi`,
/// `rdx`, `r10`, `r8`, `r9`, result in `rax`.
///
/// # Safety
/// The arguments must be what the call expects: pointers to readable (or
/// writable) memory of the sizes passed.
unsafe fn call(number: u64, a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64 {
    let result: i64;
    // SAFETY: the caller vouches for the arguments; `syscall` clobbers
    // `rcx` and `r11` and nothing else the compiler keeps.
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") number as i64 => result,
            in("rdi") a,
            in("rsi") b,
            in("rdx") c,
            in("r10") d,
            in("r8") e,
            in("r9") f,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    result
}

// ============================================================
//  Errors
// ============================================================

/// What a call can answer, beside success (`nova_gfx.h`, "Conventions").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// No GPU (`ENODEV`).
    NoDevice,
    /// A handle not the caller's, or not of that kind (`EBADF`).
    BadHandle,
    /// Arguments, or a command buffer, the kernel refused (`EINVAL`).
    Invalid,
    /// Out of memory (`ENOMEM`).
    NoMemory,
    /// The process's GPU quota (`EDQUOT`).
    Quota,
    /// The context is lost: the watchdog, or the GPU refused the work
    /// (`ENOTRECOVERABLE`). Every later call on it fails the same way.
    Lost,
    /// A wait ran out (`ETIMEDOUT`).
    TimedOut,
    /// Interrupted by a signal (`EINTR`).
    Interrupted,
    /// Any other errno.
    Other(i32),
}

impl Error {
    fn from_errno(errno: i64) -> Error {
        match errno {
            19 => Error::NoDevice,
            9 => Error::BadHandle,
            22 => Error::Invalid,
            12 => Error::NoMemory,
            122 => Error::Quota,
            131 => Error::Lost,
            110 => Error::TimedOut,
            4 => Error::Interrupted,
            other => Error::Other(other as i32),
        }
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// A call's result: values at or above zero, or a negative errno.
fn check(value: i64) -> Result<u64> {
    if (-4095..0).contains(&value) { Err(Error::from_errno(-value)) } else { Ok(value as u64) }
}

// ============================================================
//  Enumerations
// ============================================================

/// Where memory lives (`NOVA_MEM_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemKind {
    /// On the GPU, not mappable.
    Local = 0,
    /// Mappable, written by the CPU, read by the GPU.
    Visible = 1,
    /// Mappable, cached: written by the GPU, read back.
    Readback = 2,
}

/// Image formats (`NOVA_FORMAT_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Bgra8 = 1,
    Rgba8 = 2,
    R8 = 3,
    Rgba16F = 4,
    Depth32F = 5,
    Rgba8Srgb = 6,
    Bgra8Srgb = 7,
}

/// Usage bits of an image (`NOVA_IMAGE_*`); copies are always allowed.
pub mod image_usage {
    pub const RENDER: u32 = 1;
    pub const SAMPLED: u32 = 2;
    pub const DEPTH: u32 = 4;
}

/// Usage bits of a buffer (`NOVA_BUFFER_*`); copies are always allowed.
pub mod buffer_usage {
    pub const VERTEX: u32 = 1;
    pub const INDEX: u32 = 2;
    pub const UNIFORM: u32 = 4;
    pub const STORAGE: u32 = 8;
}

/// Shader stages (`NOVA_STAGE_*`), also bits of a binding's `stages`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Vertex = 1,
    Fragment = 2,
}

/// What a layout binding holds (`NOVA_BIND_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingKind {
    SampledImage = 1,
    UniformBuffer = 2,
    StorageBuffer = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topology {
    Triangles = 0,
    Strip = 1,
    Lines = 2,
    Points = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blend {
    None = 0,
    Premultiplied = 1,
    Alpha = 2,
    Additive = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertexFormat {
    Float1 = 1,
    Float2 = 2,
    Float3 = 3,
    Float4 = 4,
    Ubyte4Norm = 5,
    Uint1 = 6,
}

/// What a render pass does with an image's pixels first (`NOVA_LOAD_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Load {
    Clear = 0,
    Keep = 1,
    DontCare = 2,
}

// ============================================================
//  The device
// ============================================================

/// What `SYS_GPU_INFO` says of the GPU (`struct nova_gpu_info`).
#[derive(Debug, Clone, Copy)]
pub struct Info {
    pub interface: u32,
    pub vendor_id: u32,
    pub device_id: u32,
    pub max_image_size: u32,
    pub max_push_constants: u32,
    /// Formats rendered to and sampled, bit `1 << Format`.
    pub formats: u32,
    pub local_memory: u64,
    pub visible_memory: u64,
    /// GPU memory the process may hold.
    pub quota: u64,
    /// Images can be shared between processes.
    pub sharing: bool,
    name: [u8; 64],
}

impl Info {
    /// The device's name, as the host's driver gives it.
    pub fn name(&self) -> &str {
        let end = self.name.iter().position(|b| *b == 0).unwrap_or(self.name.len());
        core::str::from_utf8(&self.name[..end]).unwrap_or("?")
    }
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u32_at(bytes, at) as u64 | (u32_at(bytes, at + 4) as u64) << 32
}

/// Describes the GPU. `Err(NoDevice)` on a machine without one.
pub fn info() -> Result<Info> {
    let mut raw = [0u8; 128];
    // SAFETY: 128 writable bytes, as the size says.
    check(unsafe { call(INFO, raw.as_mut_ptr() as u64, raw.len() as u64, 0, 0, 0, 0) })?;
    if u32_at(&raw, 4) != 1 {
        return Err(Error::NoDevice);
    }
    let mut name = [0u8; 64];
    name.copy_from_slice(&raw[64..128]);
    Ok(Info {
        interface: u32_at(&raw, 8),
        vendor_id: u32_at(&raw, 12),
        device_id: u32_at(&raw, 16),
        max_image_size: u32_at(&raw, 20),
        max_push_constants: u32_at(&raw, 24),
        formats: u32_at(&raw, 28),
        local_memory: u64_at(&raw, 32),
        visible_memory: u64_at(&raw, 40),
        quota: u64_at(&raw, 48),
        sharing: u32_at(&raw, 56) & 1 != 0,
        name,
    })
}

// ============================================================
//  Context and objects
// ============================================================

/// A GPU context: its own queue, objects, memory, and -- on the host --
/// its own device. Closed, with everything in it, when dropped.
pub struct Context {
    handle: u32,
}

/// Memory of a context. Freed when dropped; must outlive the objects
/// placed in it and every submission using them, as in C.
pub struct Memory<'c> {
    context: &'c Context,
    handle: u32,
    size: u64,
}

/// An object of a context: a buffer, an image, a sampler, a shader, a
/// layout or a pipeline -- the marker says which.
pub struct Object<'c, Kind> {
    context: &'c Context,
    handle: u32,
    kind: PhantomData<Kind>,
}

pub enum BufferKind {}
pub enum ImageKind {}
pub enum SamplerKind {}
pub enum ShaderKind {}
pub enum LayoutKind {}
pub enum PipelineKind {}

pub type Buffer<'c> = Object<'c, BufferKind>;
pub type Image<'c> = Object<'c, ImageKind>;
pub type Sampler<'c> = Object<'c, SamplerKind>;
pub type Shader<'c> = Object<'c, ShaderKind>;
pub type Layout<'c> = Object<'c, LayoutKind>;
pub type Pipeline<'c> = Object<'c, PipelineKind>;

/// A fence of a context: set by a submission, waited on by anyone.
pub struct Fence<'c> {
    context: &'c Context,
    handle: u32,
}

impl<Kind> Object<'_, Kind> {
    /// The kernel's handle, for a command buffer.
    pub fn handle(&self) -> u32 {
        self.handle
    }
}

impl<Kind> Drop for Object<'_, Kind> {
    fn drop(&mut self) {
        // SAFETY: two integers.
        unsafe { call(OBJECT_DESTROY, self.context.handle as u64, self.handle as u64, 0, 0, 0, 0) };
    }
}

impl Drop for Memory<'_> {
    fn drop(&mut self) {
        // SAFETY: two integers. Freeing unmaps it as well.
        unsafe { call(MEM_FREE, self.context.handle as u64, self.handle as u64, 0, 0, 0, 0) };
    }
}

impl Drop for Fence<'_> {
    fn drop(&mut self) {
        // SAFETY: two integers; a fence is destroyed like an object.
        unsafe { call(OBJECT_DESTROY, self.context.handle as u64, self.handle as u64, 0, 0, 0, 0) };
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: one integer.
        unsafe { call(CONTEXT_DESTROY, self.handle as u64, 0, 0, 0, 0, 0) };
    }
}

/// A binding of a layout.
#[derive(Debug, Clone, Copy)]
pub struct Binding {
    pub binding: u32,
    pub kind: BindingKind,
    /// `Stage` bits.
    pub stages: u32,
}

/// A vertex buffer slot of a pipeline.
#[derive(Debug, Clone, Copy)]
pub struct VertexBinding {
    pub stride: u32,
    pub per_instance: bool,
}

/// A vertex attribute of a pipeline.
#[derive(Debug, Clone, Copy)]
pub struct VertexAttribute {
    pub location: u32,
    pub binding: u32,
    pub format: VertexFormat,
    pub offset: u32,
}

/// Everything a pipeline is made of (`struct nova_pipeline_desc`).
pub struct PipelineDesc<'a, 'c> {
    pub layout: &'a Layout<'c>,
    pub vertex: &'a Shader<'c>,
    pub fragment: &'a Shader<'c>,
    pub topology: Topology,
    /// 0 none, 1 back, 2 front.
    pub cull: u32,
    pub front_ccw: bool,
    pub blend: Blend,
    pub color_format: Format,
    pub depth_format: Option<Format>,
    pub depth_test: bool,
    pub depth_write: bool,
    pub bindings: &'a [VertexBinding],
    pub attributes: &'a [VertexAttribute],
}

/// Words of a description, little-endian, as the kernel reads them.
struct Words<const N: usize> {
    words: [u32; N],
    count: usize,
}

impl<const N: usize> Words<N> {
    fn new() -> Self {
        Words { words: [0; N], count: 0 }
    }

    fn push(&mut self, value: u32) -> &mut Self {
        self.words[self.count] = value;
        self.count += 1;
        self
    }

    fn push64(&mut self, value: u64) -> &mut Self {
        self.push(value as u32).push((value >> 32) as u32)
    }

    fn bytes(&self) -> (u64, u64) {
        (self.words.as_ptr() as u64, (self.count * 4) as u64)
    }
}

impl Context {
    /// Opens a context.
    pub fn create() -> Result<Context> {
        // SAFETY: no pointer.
        let handle = check(unsafe { call(CONTEXT_CREATE, 0, 0, 0, 0, 0, 0) })? as u32;
        Ok(Context { handle })
    }

    pub fn handle(&self) -> u32 {
        self.handle
    }

    /// Allocates `size` bytes of memory of `kind`, charged to the process.
    pub fn memory(&self, size: u64, kind: MemKind) -> Result<Memory<'_>> {
        // SAFETY: integers.
        let handle = check(unsafe { call(MEM_ALLOC, self.handle as u64, size, kind as u64, 0, 0, 0) })? as u32;
        Ok(Memory { context: self, handle, size })
    }

    fn make_object<Kind>(&self, kind: u64, desc: (u64, u64)) -> Result<Object<'_, Kind>> {
        // SAFETY: `desc` points at `desc.1` readable bytes, the caller's
        // description kept alive for the call.
        let handle = check(unsafe { call(OBJECT_CREATE, self.handle as u64, kind, desc.0, desc.1, 0, 0) })? as u32;
        Ok(Object { context: self, handle, kind: PhantomData })
    }

    /// A buffer of `size` bytes at `offset` (a multiple of 256) in `memory`.
    pub fn buffer(&self, memory: &Memory<'_>, offset: u64, size: u64, usage: u32) -> Result<Buffer<'_>> {
        let mut w = Words::<6>::new();
        w.push64(size).push(usage).push(memory.handle).push64(offset);
        self.make_object(OBJ_BUFFER, w.bytes())
    }

    /// An image, in memory of its own (`memory: None`) or placed.
    pub fn image(
        &self,
        format: Format,
        width: u32,
        height: u32,
        usage: u32,
        memory: Option<(&Memory<'_>, u64)>,
        shareable: bool,
    ) -> Result<Image<'_>> {
        let (handle, offset) = memory.map_or((0, 0), |(m, o)| (m.handle, o));
        let mut w = Words::<8>::new();
        w.push(format as u32).push(width).push(height).push(usage).push(handle).push(shareable as u32).push64(offset);
        self.make_object(OBJ_IMAGE, w.bytes())
    }

    pub fn sampler(&self, linear: bool, address: u32) -> Result<Sampler<'_>> {
        let mut w = Words::<2>::new();
        w.push(linear as u32).push(address);
        self.make_object(OBJ_SAMPLER, w.bytes())
    }

    /// A shader from its SPIR-V; the kernel checks it (spec 6.8).
    pub fn shader(&self, spirv: &[u8], stage: Stage) -> Result<Shader<'_>> {
        let mut w = Words::<4>::new();
        w.push64(spirv.as_ptr() as u64).push(spirv.len() as u32).push(stage as u32);
        self.make_object(OBJ_SHADER, w.bytes())
    }

    /// A resource layout: at most 16 bindings, and `push_constants` bytes.
    pub fn layout(&self, bindings: &[Binding], push_constants: u32) -> Result<Layout<'_>> {
        if bindings.len() > 16 {
            return Err(Error::Invalid);
        }
        let mut w = Words::<50>::new();
        w.push(bindings.len() as u32).push(push_constants);
        for binding in bindings {
            w.push(binding.binding).push(binding.kind as u32).push(binding.stages);
        }
        self.make_object(OBJ_LAYOUT, w.bytes())
    }

    /// A pipeline: at most 4 vertex bindings and 8 attributes.
    pub fn pipeline(&self, desc: &PipelineDesc<'_, '_>) -> Result<Pipeline<'_>> {
        if desc.bindings.len() > 4 || desc.attributes.len() > 8 {
            return Err(Error::Invalid);
        }
        let mut w = Words::<53>::new();
        w.push(desc.layout.handle).push(desc.vertex.handle).push(desc.fragment.handle);
        w.push(desc.topology as u32).push(desc.cull).push(desc.front_ccw as u32).push(desc.blend as u32);
        w.push(desc.color_format as u32).push(desc.depth_format.map_or(0, |f| f as u32));
        w.push(desc.depth_test as u32).push(desc.depth_write as u32);
        w.push(desc.bindings.len() as u32);
        for slot in 0..4 {
            let binding = desc.bindings.get(slot);
            w.push(binding.map_or(0, |b| b.stride)).push(binding.map_or(0, |b| b.per_instance as u32));
        }
        w.push(desc.attributes.len() as u32);
        for slot in 0..8 {
            match desc.attributes.get(slot) {
                Some(a) => w.push(a.location).push(a.binding).push(a.format as u32).push(a.offset),
                None => w.push(0).push(0).push(0).push(0),
            };
        }
        self.make_object(OBJ_PIPELINE, w.bytes())
    }

    pub fn fence(&self) -> Result<Fence<'_>> {
        // SAFETY: an integer.
        let handle = check(unsafe { call(FENCE_CREATE, self.handle as u64, 0, 0, 0, 0, 0) })? as u32;
        Ok(Fence { context: self, handle })
    }

    /// Submits a command buffer. `signal` is set to it; it runs once every
    /// fence of `waits` has signalled -- fences of other contexts too.
    pub fn submit(&self, commands: &Commands<'_>, signal: Option<&Fence<'_>>, waits: &[&Fence<'_>]) -> Result<()> {
        if commands.overflow {
            return Err(Error::Invalid);
        }
        let mut handles = [0u32; 16];
        if waits.len() > handles.len() {
            return Err(Error::Invalid);
        }
        for (slot, fence) in handles.iter_mut().zip(waits) {
            *slot = fence.handle;
        }
        let words = &commands.words[..commands.count];
        // SAFETY: the words and the wait list are readable for the sizes
        // passed, and kept alive for the call.
        check(unsafe {
            call(
                SUBMIT,
                self.handle as u64,
                words.as_ptr() as u64,
                (words.len() * 4) as u64,
                signal.map_or(0, |f| f.handle as u64),
                handles.as_ptr() as u64,
                waits.len() as u64,
            )
        })
        .map(|_| ())
    }

    /// A token another process imports `image` by (the image must have
    /// been made shareable).
    pub fn share(&self, image: &Image<'_>) -> Result<u64> {
        // SAFETY: integers.
        check(unsafe { call(SHARE, self.handle as u64, image.handle as u64, 0, 0, 0, 0) })
    }

    /// The image another process shared.
    pub fn import(&self, token: u64) -> Result<Image<'_>> {
        // SAFETY: integers.
        let handle = check(unsafe { call(IMPORT, self.handle as u64, token, 0, 0, 0, 0) })? as u32;
        Ok(Object { context: self, handle, kind: PhantomData })
    }

    /// Shows `image` in the `/dev/nova-gui` window of descriptor `window`,
    /// once `after` (if any) has signalled.
    pub fn present(&self, image: &Image<'_>, window: i32, after: Option<&Fence<'_>>) -> Result<()> {
        // SAFETY: integers.
        check(unsafe {
            call(PRESENT, self.handle as u64, image.handle as u64, window as u64, after.map_or(0, |f| f.handle as u64), 0, 0)
        })
        .map(|_| ())
    }
}

impl<'c> Memory<'c> {
    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn handle(&self) -> u32 {
        self.handle
    }

    /// Maps the memory in the process: its bytes, until `unmap` or the
    /// memory is dropped. `Local` memory does not map.
    pub fn map(&mut self) -> Result<&mut [u8]> {
        // SAFETY: integers.
        let address = check(unsafe { call(MEM_MAP, self.context.handle as u64, self.handle as u64, 0, 0, 0, 0) })?;
        // SAFETY: the kernel mapped `size` bytes at `address` for this
        // process; the borrow of `self` keeps the memory alive and mapped.
        Ok(unsafe { core::slice::from_raw_parts_mut(address as *mut u8, self.size as usize) })
    }

    pub fn unmap(&mut self) -> Result<()> {
        // SAFETY: integers; `&mut self` ends every borrow of the mapping.
        check(unsafe { call(MEM_UNMAP, self.context.handle as u64, self.handle as u64, 0, 0, 0, 0) }).map(|_| ())
    }
}

impl Fence<'_> {
    pub fn handle(&self) -> u32 {
        self.handle
    }

    /// Waits until the fence signals, `timeout_ns` at most.
    pub fn wait(&self, timeout_ns: u64) -> Result<()> {
        wait(&[self], true, timeout_ns)
    }

    /// A descriptor that becomes readable when the fence signals, for
    /// `poll`/`epoll`.
    pub fn export(&self) -> Result<i32> {
        // SAFETY: an integer.
        check(unsafe { call(FENCE_EXPORT, self.handle as u64, 0, 0, 0, 0, 0) }).map(|fd| fd as i32)
    }
}

/// Waits for all (or one) of `fences`, of any contexts.
pub fn wait(fences: &[&Fence<'_>], all: bool, timeout_ns: u64) -> Result<()> {
    let mut handles = [0u32; 16];
    if fences.len() > handles.len() {
        return Err(Error::Invalid);
    }
    for (slot, fence) in handles.iter_mut().zip(fences) {
        *slot = fence.handle;
    }
    // SAFETY: the handle list is readable for the count passed.
    check(unsafe { call(FENCE_WAIT, handles.as_ptr() as u64, fences.len() as u64, timeout_ns, all as u64, 0, 0) })
        .map(|_| ())
}

// ============================================================
//  Command buffer
// ============================================================

/// A command buffer being written into the caller's words. A command that
/// does not fit is dropped and marks the buffer `overflow`: `submit` then
/// refuses it whole, as the kernel would refuse half a buffer.
pub struct Commands<'w> {
    words: &'w mut [u32],
    count: usize,
    overflow: bool,
}

// Opcodes (`NOVA_CMD_*`).
const BEGIN_RENDER: u32 = 1;
const END_RENDER: u32 = 2;
const BIND_PIPELINE: u32 = 3;
const BIND_VERTEX: u32 = 4;
const BIND_INDEX: u32 = 5;
const BIND_RESOURCE: u32 = 6;
const VIEWPORT: u32 = 7;
const SCISSOR: u32 = 8;
const PUSH_CONSTANTS: u32 = 9;
const DRAW: u32 = 10;
const DRAW_INDEXED: u32 = 11;
const COPY_BUFFER: u32 = 12;
const COPY_BUFFER_TO_IMAGE: u32 = 13;
const COPY_IMAGE_TO_BUFFER: u32 = 14;
const COPY_IMAGE: u32 = 15;
const BLIT_IMAGE: u32 = 16;
const CLEAR_IMAGE: u32 = 17;
const FILL_BUFFER: u32 = 18;
const UPDATE_BUFFER: u32 = 19;

/// A rectangle, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Rect {
        Rect { x, y, width, height }
    }
}

impl<'w> Commands<'w> {
    pub fn new(words: &'w mut [u32]) -> Commands<'w> {
        Commands { words, count: 0, overflow: false }
    }

    /// Words written so far.
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Did a command not fit?
    pub fn overflowed(&self) -> bool {
        self.overflow
    }

    pub fn clear(&mut self) {
        self.count = 0;
        self.overflow = false;
    }

    fn command(&mut self, opcode: u32, operands: &[u32]) -> &mut Self {
        let length = operands.len() + 1;
        if self.overflow || self.count + length > self.words.len() || length > 0xFFFF {
            self.overflow = true;
            return self;
        }
        self.words[self.count] = opcode | (length as u32) << 16;
        self.words[self.count + 1..self.count + length].copy_from_slice(operands);
        self.count += length;
        self
    }

    /// Starts a render pass on `image` over `area`, loading it by `load`
    /// (cleared to `clear`, RGBA). No depth image.
    pub fn begin_render(&mut self, image: &Image<'_>, load: Load, clear: [f32; 4], area: Rect) -> &mut Self {
        let c = clear.map(f32::to_bits);
        self.command(
            BEGIN_RENDER,
            &[image.handle, load as u32, c[0], c[1], c[2], c[3], 0, Load::DontCare as u32, 0, area.x as u32, area.y as u32, area.width, area.height],
        )
    }

    /// `begin_render` with a depth image, cleared to `depth_clear` or kept.
    pub fn begin_render_depth(
        &mut self,
        image: &Image<'_>,
        load: Load,
        clear: [f32; 4],
        depth: &Image<'_>,
        depth_load: Load,
        depth_clear: f32,
        area: Rect,
    ) -> &mut Self {
        let c = clear.map(f32::to_bits);
        self.command(
            BEGIN_RENDER,
            &[
                image.handle,
                load as u32,
                c[0],
                c[1],
                c[2],
                c[3],
                depth.handle,
                depth_load as u32,
                depth_clear.to_bits(),
                area.x as u32,
                area.y as u32,
                area.width,
                area.height,
            ],
        )
    }

    pub fn end_render(&mut self) -> &mut Self {
        self.command(END_RENDER, &[])
    }

    pub fn bind_pipeline(&mut self, pipeline: &Pipeline<'_>) -> &mut Self {
        self.command(BIND_PIPELINE, &[pipeline.handle])
    }

    pub fn bind_vertex(&mut self, slot: u32, buffer: &Buffer<'_>, offset: u64) -> &mut Self {
        self.command(BIND_VERTEX, &[slot, buffer.handle, offset as u32, (offset >> 32) as u32])
    }

    /// An index buffer: 32-bit indices when `wide`, 16-bit otherwise.
    pub fn bind_index(&mut self, buffer: &Buffer<'_>, offset: u64, wide: bool) -> &mut Self {
        self.command(BIND_INDEX, &[buffer.handle, offset as u32, (offset >> 32) as u32, wide as u32])
    }

    pub fn bind_image(&mut self, binding: u32, image: &Image<'_>, sampler: &Sampler<'_>) -> &mut Self {
        self.command(BIND_RESOURCE, &[binding, image.handle, sampler.handle, 0, 0, 0, 0])
    }

    /// A buffer range (`range` 0: to the end).
    pub fn bind_buffer(&mut self, binding: u32, buffer: &Buffer<'_>, offset: u64, range: u64) -> &mut Self {
        self.command(
            BIND_RESOURCE,
            &[binding, buffer.handle, 0, offset as u32, (offset >> 32) as u32, range as u32, (range >> 32) as u32],
        )
    }

    pub fn viewport(&mut self, x: f32, y: f32, width: f32, height: f32, min_depth: f32, max_depth: f32) -> &mut Self {
        self.command(VIEWPORT, &[x.to_bits(), y.to_bits(), width.to_bits(), height.to_bits(), min_depth.to_bits(), max_depth.to_bits()])
    }

    pub fn scissor(&mut self, rect: Rect) -> &mut Self {
        self.command(SCISSOR, &[rect.x as u32, rect.y as u32, rect.width, rect.height])
    }

    /// Push constants: at most 256 bytes, padded to whole words.
    pub fn push_constants(&mut self, offset: u32, data: &[u8]) -> &mut Self {
        let mut operands = [0u32; 65];
        let words = data.len().div_ceil(4);
        if words > 64 {
            self.overflow = true;
            return self;
        }
        operands[0] = offset;
        for (index, chunk) in data.chunks(4).enumerate() {
            let mut word = [0u8; 4];
            word[..chunk.len()].copy_from_slice(chunk);
            operands[index + 1] = u32::from_le_bytes(word);
        }
        self.command(PUSH_CONSTANTS, &operands[..words + 1])
    }

    pub fn draw(&mut self, vertices: u32, instances: u32, first_vertex: u32, first_instance: u32) -> &mut Self {
        self.command(DRAW, &[vertices, instances, first_vertex, first_instance])
    }

    pub fn draw_indexed(&mut self, indices: u32, instances: u32, first_index: u32, vertex_offset: i32, first_instance: u32) -> &mut Self {
        self.command(DRAW_INDEXED, &[indices, instances, first_index, vertex_offset as u32, first_instance])
    }

    pub fn copy_buffer(&mut self, src: &Buffer<'_>, dst: &Buffer<'_>, src_offset: u64, dst_offset: u64, size: u64) -> &mut Self {
        self.command(
            COPY_BUFFER,
            &[
                src.handle,
                dst.handle,
                src_offset as u32,
                (src_offset >> 32) as u32,
                dst_offset as u32,
                (dst_offset >> 32) as u32,
                size as u32,
                (size >> 32) as u32,
            ],
        )
    }

    /// Rows of `row_pixels` pixels (0: the rectangle's width).
    pub fn copy_buffer_to_image(&mut self, buffer: &Buffer<'_>, offset: u64, row_pixels: u32, image: &Image<'_>, rect: Rect) -> &mut Self {
        self.command(
            COPY_BUFFER_TO_IMAGE,
            &[buffer.handle, offset as u32, (offset >> 32) as u32, row_pixels, image.handle, rect.x as u32, rect.y as u32, rect.width, rect.height],
        )
    }

    pub fn copy_image_to_buffer(&mut self, image: &Image<'_>, rect: Rect, buffer: &Buffer<'_>, offset: u64, row_pixels: u32) -> &mut Self {
        self.command(
            COPY_IMAGE_TO_BUFFER,
            &[image.handle, rect.x as u32, rect.y as u32, rect.width, rect.height, buffer.handle, offset as u32, (offset >> 32) as u32, row_pixels],
        )
    }

    pub fn copy_image(&mut self, src: &Image<'_>, src_x: i32, src_y: i32, dst: &Image<'_>, dst_x: i32, dst_y: i32, width: u32, height: u32) -> &mut Self {
        self.command(
            COPY_IMAGE,
            &[src.handle, src_x as u32, src_y as u32, dst.handle, dst_x as u32, dst_y as u32, width, height],
        )
    }

    pub fn blit(&mut self, src: &Image<'_>, src_rect: Rect, dst: &Image<'_>, dst_rect: Rect, linear: bool) -> &mut Self {
        self.command(
            BLIT_IMAGE,
            &[
                src.handle,
                src_rect.x as u32,
                src_rect.y as u32,
                src_rect.width,
                src_rect.height,
                dst.handle,
                dst_rect.x as u32,
                dst_rect.y as u32,
                dst_rect.width,
                dst_rect.height,
                linear as u32,
            ],
        )
    }

    pub fn clear_image(&mut self, image: &Image<'_>, color: [f32; 4]) -> &mut Self {
        let c = color.map(f32::to_bits);
        self.command(CLEAR_IMAGE, &[image.handle, c[0], c[1], c[2], c[3]])
    }

    pub fn fill_buffer(&mut self, buffer: &Buffer<'_>, offset: u64, size: u64, value: u32) -> &mut Self {
        self.command(FILL_BUFFER, &[buffer.handle, offset as u32, (offset >> 32) as u32, size as u32, (size >> 32) as u32, value])
    }

    /// At most 65536 bytes, a multiple of 4.
    pub fn update_buffer(&mut self, buffer: &Buffer<'_>, offset: u64, data: &[u32]) -> &mut Self {
        let head = [buffer.handle, offset as u32, (offset >> 32) as u32];
        let length = head.len() + data.len() + 1;
        if self.overflow || self.count + length > self.words.len() || length > 0xFFFF {
            self.overflow = true;
            return self;
        }
        self.words[self.count] = UPDATE_BUFFER | (length as u32) << 16;
        self.words[self.count + 1..self.count + 4].copy_from_slice(&head);
        self.words[self.count + 4..self.count + length].copy_from_slice(data);
        self.count += length;
        self
    }
}

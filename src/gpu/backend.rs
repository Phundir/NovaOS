//! The backend interface: what a GPU driver provides to the core (spec 8.5).
//!
//! ## Where the line is drawn
//!
//! The core owns everything a program can get wrong: its handles, its
//! memory quota, the validation of every command it submits, its fences,
//! the watchdog, the sharing of images between processes. A backend
//! receives only what has passed all of that -- objects described in Nova
//! terms, command lists whose every handle, range, format and state has
//! been checked -- and turns them into its hardware's language. The
//! built-in backend turns them into Venus (Vulkan for the host's GPU); a
//! vendor's backend, loaded as a signed module (spec 3.6), would turn them
//! into its card's command rings. Neither ever sees a program's pointer.
//!
//! ## Versioning
//!
//! `INTERFACE_VERSION` names this set of types and calls. A module built
//! against another version is refused at load time (spec 3.6): there is
//! no negotiation, only equality.
//!
//! ## Floating point
//!
//! Colours, depths and viewports cross the interface as the bit patterns
//! of IEEE-754 single-precision numbers (`u32`). The kernel is built
//! soft-float and never computes with them; it copies them.

use alloc::boxed::Box;
use alloc::vec::Vec;

/// Version of the backend interface, major in the high half.
pub const INTERFACE_VERSION: u32 = 0x0001_0000;

/// A backend's name for one of its objects. The core never interprets it.
pub type Id = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A description the backend cannot honour (a format it lacks, a
    /// size beyond its limits).
    Unsupported,
    /// Device or host memory exhausted.
    NoMemory,
    /// The context is lost: every later call fails the same way.
    Lost,
    /// Anything else, named for the log.
    Device(&'static str),
}

impl Error {
    pub fn describe(&self) -> &'static str {
        match self {
            Error::Unsupported => "non pris en charge par le GPU",
            Error::NoMemory => "memoire du GPU epuisee",
            Error::Lost => "contexte GPU perdu",
            Error::Device(reason) => reason,
        }
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// Image formats of the Nova interface (`NOVA_FORMAT_*`).
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

impl Format {
    pub fn from_u32(value: u32) -> Option<Format> {
        Some(match value {
            1 => Format::Bgra8,
            2 => Format::Rgba8,
            3 => Format::R8,
            4 => Format::Rgba16F,
            5 => Format::Depth32F,
            6 => Format::Rgba8Srgb,
            7 => Format::Bgra8Srgb,
            _ => return None,
        })
    }

    pub fn bytes_per_pixel(self) -> u64 {
        match self {
            Format::R8 => 1,
            Format::Rgba16F => 8,
            _ => 4,
        }
    }

    pub fn is_depth(self) -> bool {
        self == Format::Depth32F
    }

    /// Every format, for `SYS_GPU_INFO`'s mask.
    pub const ALL: [Format; 7] =
        [Format::Bgra8, Format::Rgba8, Format::R8, Format::Rgba16F, Format::Depth32F, Format::Rgba8Srgb, Format::Bgra8Srgb];
}

/// What memory is for (`NOVA_MEM_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemKind {
    /// On the GPU; not mappable.
    Local,
    /// Mappable, coherent, written by the program and read by the GPU.
    Visible,
    /// Mappable, cached: written by the GPU and read back by the program.
    Readback,
}

/// Where a mappable allocation shows in physical memory.
#[derive(Debug, Clone, Copy)]
pub struct Window {
    pub phys: u64,
    /// Whole pages.
    pub size: u64,
    /// Page-table cache bits (0: write-back).
    pub cache_flags: u64,
}

/// Image usage bits (`NOVA_IMAGE_*`). Copies to and from any image are
/// always allowed.
pub mod image_usage {
    pub const RENDER: u32 = 1;
    pub const SAMPLED: u32 = 2;
    pub const DEPTH: u32 = 4;
    pub const ALL: u32 = RENDER | SAMPLED | DEPTH;
}

/// Buffer usage bits (`NOVA_BUFFER_*`). Copies always allowed.
pub mod buffer_usage {
    pub const VERTEX: u32 = 1;
    pub const INDEX: u32 = 2;
    pub const UNIFORM: u32 = 4;
    pub const STORAGE: u32 = 8;
    pub const ALL: u32 = VERTEX | INDEX | UNIFORM | STORAGE;
}

#[derive(Debug, Clone, Copy)]
pub struct ImageDesc {
    pub format: Format,
    pub width: u32,
    pub height: u32,
    pub usage: u32,
    /// Its memory may be exported to another context (`SYS_GPU_SHARE`).
    pub shareable: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct BufferDesc {
    pub size: u64,
    pub usage: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct SamplerDesc {
    pub linear: bool,
    /// 0 clamp to edge, 1 repeat, 2 mirrored repeat.
    pub address: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingKind {
    /// An image and its sampler (`NOVA_BIND_SAMPLED_IMAGE`).
    SampledImage,
    UniformBuffer,
    StorageBuffer,
}

/// Shader stage bits.
pub mod stage {
    pub const VERTEX: u32 = 1;
    pub const FRAGMENT: u32 = 2;
    pub const ALL: u32 = VERTEX | FRAGMENT;
}

#[derive(Debug, Clone, Copy)]
pub struct LayoutBinding {
    pub binding: u32,
    pub kind: BindingKind,
    pub stages: u32,
}

#[derive(Debug, Clone)]
pub struct LayoutDesc {
    pub bindings: Vec<LayoutBinding>,
    pub push_constants: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topology {
    Triangles = 0,
    TriangleStrip = 1,
    Lines = 2,
    Points = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blend {
    None = 0,
    /// Colour already multiplied by alpha: `src + dst * (1 - src.a)`.
    Premultiplied,
    /// `src * src.a + dst * (1 - src.a)`.
    Alpha,
    Additive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertexFormat {
    Float1 = 1,
    Float2 = 2,
    Float3 = 3,
    Float4 = 4,
    /// Four bytes, normalized to 0..1.
    Ubyte4Norm = 5,
    Uint1 = 6,
}

impl VertexFormat {
    pub fn from_u32(value: u32) -> Option<VertexFormat> {
        Some(match value {
            1 => VertexFormat::Float1,
            2 => VertexFormat::Float2,
            3 => VertexFormat::Float3,
            4 => VertexFormat::Float4,
            5 => VertexFormat::Ubyte4Norm,
            6 => VertexFormat::Uint1,
            _ => return None,
        })
    }

    pub fn size(self) -> u32 {
        match self {
            VertexFormat::Float1 | VertexFormat::Ubyte4Norm | VertexFormat::Uint1 => 4,
            VertexFormat::Float2 => 8,
            VertexFormat::Float3 => 12,
            VertexFormat::Float4 => 16,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VertexBinding {
    pub stride: u32,
    pub per_instance: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct VertexAttribute {
    pub location: u32,
    pub binding: u32,
    pub format: VertexFormat,
    pub offset: u32,
}

#[derive(Debug, Clone)]
pub struct PipelineDesc {
    pub layout: Id,
    pub vertex: Id,
    pub fragment: Id,
    pub topology: Topology,
    /// 0 none, 1 back, 2 front.
    pub cull: u32,
    pub front_ccw: bool,
    pub blend: Blend,
    pub color_format: Format,
    pub depth_format: Option<Format>,
    pub depth_test: bool,
    pub depth_write: bool,
    pub bindings: Vec<VertexBinding>,
    pub attributes: Vec<VertexAttribute>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Buffer,
    Image,
    Sampler,
    Shader,
    Layout,
    Pipeline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadOp {
    Clear,
    Load,
    DontCare,
}

/// An image as a command uses it.
#[derive(Debug, Clone, Copy)]
pub struct ImageRef {
    pub id: Id,
    pub format: Format,
    pub width: u32,
    pub height: u32,
    /// First use since creation: its contents are undefined, and the
    /// backend brings it to the layout it keeps images in.
    pub first_use: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum ResourceWrite {
    Image { binding: u32, image: Id, sampler: Id },
    Buffer { binding: u32, kind: BindingKind, buffer: Id, offset: u64, range: u64 },
}

/// A validated command. Every id names an object of the context, of the
/// right kind; every range lies within its object; every state a command
/// needs was set before it.
#[derive(Debug, Clone)]
pub enum Command {
    BeginRender { color: ImageRef, load: LoadOp, clear: [u32; 4], depth: Option<(ImageRef, LoadOp, u32)>, area: Rect },
    EndRender,
    BindPipeline { pipeline: Id },
    BindVertex { slot: u32, buffer: Id, offset: u64 },
    BindIndex { buffer: Id, offset: u64, wide: bool },
    PushResources { layout: Id, writes: Vec<ResourceWrite> },
    Viewport { x: u32, y: u32, width: u32, height: u32, min_depth: u32, max_depth: u32 },
    Scissor(Rect),
    PushConstants { layout: Id, offset: u32, data: Vec<u8> },
    Draw { vertices: u32, instances: u32, first_vertex: u32, first_instance: u32 },
    DrawIndexed { indices: u32, instances: u32, first_index: u32, vertex_offset: i32, first_instance: u32 },
    CopyBuffer { src: Id, dst: Id, src_offset: u64, dst_offset: u64, size: u64 },
    CopyBufferToImage { buffer: Id, offset: u64, row_pixels: u32, image: ImageRef, rect: Rect },
    CopyImageToBuffer { image: ImageRef, rect: Rect, buffer: Id, offset: u64, row_pixels: u32 },
    CopyImage { src: ImageRef, dst: ImageRef, src_x: i32, src_y: i32, dst_x: i32, dst_y: i32, width: u32, height: u32 },
    Blit { src: ImageRef, src_rect: Rect, dst: ImageRef, dst_rect: Rect, linear: bool },
    ClearImage { image: ImageRef, color: [u32; 4] },
    FillBuffer { buffer: Id, offset: u64, size: u64, value: u32 },
    UpdateBuffer { buffer: Id, offset: u64, data: Vec<u8> },
}

/// A submission handed to the backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Work(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkState {
    Pending,
    Done,
    /// The GPU or the host refused it; the context is lost.
    Failed,
}

/// What the core shows programs of the device (`SYS_GPU_INFO`).
#[derive(Debug, Clone)]
pub struct Description {
    pub name: alloc::string::String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub max_image_size: u32,
    pub max_push_constants: u32,
    pub local_memory: u64,
    pub visible_memory: u64,
    /// Formats the device renders to and samples, as `1 << Format`.
    pub formats: u32,
    /// Images can be shared between contexts.
    pub sharing: bool,
    /// Alignment a buffer bound in a program's memory must respect.
    pub buffer_alignment: u64,
}

/// A GPU the core can open contexts on.
pub trait Device: Send + Sync {
    fn describe(&self) -> Description;
    fn open(&self, name: &str) -> Result<Box<dyn Context>>;
}

/// One context on the device: its objects, its queue.
///
/// Every call is made under the core's lock of the context: a backend
/// context is never entered twice at once. Calls may sleep.
pub trait Context: Send {
    /// Allocates memory; returns its id and its real size (rounded up).
    fn alloc(&mut self, size: u64, kind: MemKind) -> Result<(Id, u64)>;
    fn free(&mut self, mem: Id);
    /// Maps a `Visible` or `Readback` allocation where the CPU reaches it.
    fn map(&mut self, mem: Id) -> Result<Window>;
    /// Undoes `map`, once nothing maps the window any more.
    fn unmap(&mut self, mem: Id);
    fn create_buffer(&mut self, desc: &BufferDesc, mem: Id, offset: u64) -> Result<Id>;
    /// Creates an image in `memory` at an offset, or in memory of its own
    /// (`None`). Returns its id and the bytes of memory it took of its own.
    fn create_image(&mut self, desc: &ImageDesc, memory: Option<(Id, u64)>) -> Result<(Id, u64)>;
    fn create_sampler(&mut self, desc: &SamplerDesc) -> Result<Id>;
    /// SPIR-V the core validated.
    fn create_shader(&mut self, code: &[u32]) -> Result<Id>;
    fn create_layout(&mut self, desc: &LayoutDesc) -> Result<Id>;
    fn create_pipeline(&mut self, desc: &PipelineDesc) -> Result<Id>;
    /// Destroys an object no submission in flight uses any more.
    fn destroy(&mut self, kind: ObjectKind, id: Id);
    fn submit(&mut self, commands: &[Command]) -> Result<Work>;
    fn poll(&mut self, work: Work) -> WorkState;
    /// Makes a shareable image's memory importable by another context of
    /// this device. Returns the name the other context imports it by.
    fn export_image(&mut self, image: Id) -> Result<u32>;
    /// Imports an image another context exported, with its description.
    fn import_image(&mut self, shared: u32, desc: &ImageDesc) -> Result<Id>;
    /// Ends the context: everything in it goes.
    fn close(&mut self);
}

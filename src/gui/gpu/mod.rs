//! The compositor on the GPU (spec 8.5, phase 13.8).
//!
//! ## What changes, and what does not
//!
//! The compositor decides what the screen shows exactly as before: the
//! same damaged rectangle, the same surfaces in the same order, the same
//! painter with the same clipping. Only the target differs. Instead of
//! laying pixels in the back buffer, the painter records **instances**
//! (`record`), and the GPU draws them into the composed image; the damaged
//! rectangle is then copied, on the GPU, into the memory the screen shows
//! (`gpu::screen`). The processor no longer touches a pixel of the frame:
//! no fill, no glyph, no copy of a canvas into a back buffer, no copy of
//! the back buffer to the screen.
//!
//! ## Where the pixels live
//!
//! ```text
//!   canvases (client memory) --copy--> upload area --GPU copy--> canvas images
//!   instances ----------------------------> composed image (GPU memory)
//!                                              | GPU copy of the damage
//!                                              v
//!   output memory (host memory, cached) = backing of the scanout's 2D resource
//! ```
//!
//! A canvas is copied once by the processor, into memory the GPU reads --
//! the copy the painter used to make into the back buffer. Everything
//! after that is the GPU's.
//!
//! ## The accessibility transforms (16.1, 16.2) stay on the processor
//!
//! Step 13.8.2 left the choice open. The magnifier and the colour filters
//! are integer formulas (`access::map_pixel`), tested against the spec's
//! numbers. With them on, the GPU copies the damage into a second memory
//! (`back`) and the processor maps it into the output, pixel by pixel --
//! the very code of the software path. A shader would redo the same
//! arithmetic in floats, with its own roundings, for a feature whose
//! users would then see two slightly different screens depending on the
//! machine. The cost is paid only by who switches a transform on, as
//! before.
//!
//! ## When the GPU is slow, or lost
//!
//! The host's GPU does not preempt a long draw of another program: a
//! shader that loops holds it until the kernel's watchdog gives up on
//! that program (spec 6.8), two seconds -- and in the meantime nothing
//! else it was given runs, the compositor's frame included. The screen
//! must not freeze for that (13.11.3). So a frame is waited for
//! `FRAME_WAIT_MS` at most; past that the compositor gives the screen to
//! the processor and keeps painting (`drive`), and takes the GPU back
//! once the late frame is done. A context lost for good is closed, and
//! another one opened a moment later.

pub mod drive;
mod frame;
pub mod record;
pub mod selftest;

use alloc::vec::Vec;

use crate::gpu::backend::{self, BindingKind, Blend, Format, ImageDesc, LayoutBinding, MemKind, Topology, VertexFormat};
use crate::gpu::cmd::{float_bits, op};
use crate::gpu::{self, Create, Owner, PipelineRequest};
use crate::gui::canvas::CanvasRef;
use crate::gui::surface::Rect;

use record::{Batch, IconCache, Instance, Recorder};

/// The last frame's counters, for `gui::dump_state` (another task).
pub static STATS: crate::vga::SpinLock<Option<(Stats, (usize, usize))>> = crate::vga::SpinLock::new(None);

static VERTEX: &[u8] = include_bytes!("../../gpu/shaders/compose.vert.spv");
static FRAGMENT: &[u8] = include_bytes!("../../gpu/shaders/compose.frag.spv");

/// Longest wait for one frame before the processor takes the screen over.
const FRAME_WAIT_MS: u64 = 100;

/// Longest wait for the setup's own copies.
const SETUP_WAIT_MS: u64 = 2000;

/// Why a frame did not reach the screen.
#[derive(Debug, Clone, Copy)]
pub enum DrawError {
    /// The GPU has not finished it yet: it is left pending.
    Slow,
    /// The context is gone.
    Lost(&'static str),
}

/// What became of a pending frame.
pub enum Settled {
    Pending,
    Done,
    Lost,
}

/// Alignment of a buffer in its memory (`SYS_GPU_INFO`), and of each
/// upload inside the upload area.
const ALIGN: u64 = 256;

/// A mapped memory with one buffer over the whole of it.
struct Area {
    memory: u32,
    buffer: u32,
    address: u64,
    size: u64,
}

/// A canvas as the GPU holds it.
struct Texture {
    canvas: CanvasRef,
    image: u32,
    width: u32,
    height: u32,
    /// `published_ns` of the frame last copied in (0: none yet).
    stamp: u64,
}

/// What one frame draws, recorded under the compositor's lock.
pub struct Frame {
    pub damage: Rect,
    pub instances: Vec<Instance>,
    pub batches: Vec<Batch>,
    pub canvases: Vec<CanvasRef>,
}

/// The accessibility transforms a frame goes through (16.1, 16.2), read
/// once per frame.
#[derive(Clone, Copy)]
pub struct Transforms {
    pub zoom_on: bool,
    pub zoom: crate::access::zoom::Settings,
    pub centre: (i32, i32),
    pub mode: crate::access::contrast::Mode,
    pub matrix: crate::access::contrast::Matrix,
}

impl Transforms {
    /// The system's, when any transform is on.
    pub fn current() -> Option<Transforms> {
        crate::access::transforms_pixels().then(|| Transforms {
            zoom_on: crate::access::is_on(crate::access::Feature::Zoom),
            zoom: crate::access::zoom_settings(),
            centre: crate::access::zoom_centre(),
            mode: crate::access::contrast_mode(),
            matrix: crate::access::custom_matrix(),
        })
    }
}

/// Counters for the state dump and the 13.8.3 measure.
#[derive(Clone, Copy, Default)]
pub struct Stats {
    pub frames: u64,
    pub instances: u64,
    /// Processor time spent recording, uploading and submitting.
    pub cpu_ns: u64,
    /// Time waiting for the GPU to finish frames.
    pub wait_ns: u64,
    pub uploaded: u64,
    /// Frames that went through the processor transforms.
    pub transformed: u64,
}

pub struct Renderer {
    owner: Owner,
    context: u32,
    pipeline: u32,
    sampler: u32,
    glyphs: u32,
    icon_atlas: u32,
    /// A 1 x 1 image bound where no canvas is.
    blank: u32,
    width: u32,
    height: u32,
    composed: u32,
    output: Area,
    back: Option<Area>,
    instances: Option<Area>,
    upload: Option<Area>,
    fence: u32,
    icons: IconCache,
    textures: Vec<Texture>,
    /// Does the composed image hold a whole frame? Until then, a frame
    /// clears it rather than loading undefined pixels.
    composed_valid: bool,
    /// Is the output the screen's? False for the self-test's renderer.
    on_screen: bool,
    /// A frame the GPU did not finish within `FRAME_WAIT_MS`.
    pending: Option<gpu::FenceView>,
    pub stats: Stats,
}

/// Inside `draw`: a frame's own failure, or one of the setup it needed.
enum Failure {
    Draw(DrawError),
    Setup(&'static str),
}

impl From<&'static str> for Failure {
    fn from(reason: &'static str) -> Failure {
        Failure::Setup(reason)
    }
}

/// A command buffer being written: header words and operands.
#[derive(Default)]
struct Words(Vec<u32>);

impl Words {
    fn cmd(&mut self, opcode: u32, operands: &[u32]) {
        self.0.push(opcode | ((operands.len() as u32 + 1) << 16));
        self.0.extend_from_slice(operands);
    }

    fn copy_to_image(&mut self, buffer: u32, offset: u64, row_pixels: u32, image: u32, rect: Rect) {
        self.cmd(
            op::COPY_BUFFER_TO_IMAGE,
            &[buffer, offset as u32, (offset >> 32) as u32, row_pixels, image, rect.x as u32, rect.y as u32, rect.width as u32, rect.height as u32],
        );
    }

    fn copy_to_buffer(&mut self, image: u32, rect: Rect, buffer: u32, offset: u64, row_pixels: u32) {
        self.cmd(
            op::COPY_IMAGE_TO_BUFFER,
            &[image, rect.x as u32, rect.y as u32, rect.width as u32, rect.height as u32, buffer, offset as u32, (offset >> 32) as u32, row_pixels],
        );
    }
}

fn fail(what: &'static str) -> impl Fn(gpu::Error) -> &'static str {
    move |error| {
        crate::serial_println!("[gui] compositeur GPU : {} - {:?}", what, error);
        what
    }
}

impl Renderer {
    /// Opens the compositor's context and everything that does not depend
    /// on the screen size, then makes the screen's images.
    pub fn new(width: u32, height: u32) -> Result<Renderer, &'static str> {
        Self::open(width, height, true)
    }

    /// A renderer whose output is not shown: the self-test's.
    pub fn offscreen(width: u32, height: u32) -> Result<Renderer, &'static str> {
        Self::open(width, height, false)
    }

    fn open(width: u32, height: u32, on_screen: bool) -> Result<Renderer, &'static str> {
        let owner = Owner::current();
        let name = if on_screen { "compositeur" } else { "compositeur-essai" };
        let context = gpu::context_create(&owner, name).map_err(fail("contexte refuse"))?;
        let built = Self::build(owner.clone(), context, width, height, on_screen);
        if built.is_err() {
            let _ = gpu::context_destroy(&owner, context);
        }
        built
    }

    fn build(owner: Owner, context: u32, width: u32, height: u32, on_screen: bool) -> Result<Renderer, &'static str> {
        let create = |what: Create, name: &'static str| gpu::object_create(&owner, context, what).map_err(fail(name));
        let vertex_code = gpu::spirv::words(VERTEX).ok_or("shader de sommets illisible")?;
        let fragment_code = gpu::spirv::words(FRAGMENT).ok_or("shader de fragments illisible")?;
        let vertex = create(Create::Shader { code: &vertex_code, stage: gpu::spirv::Stage::Vertex }, "shader de sommets refuse")?;
        let fragment =
            create(Create::Shader { code: &fragment_code, stage: gpu::spirv::Stage::Fragment }, "shader de fragments refuse")?;
        let bindings = (0..3)
            .map(|binding| LayoutBinding { binding, kind: BindingKind::SampledImage, stages: backend::stage::FRAGMENT })
            .collect();
        let layout = create(Create::Layout(backend::LayoutDesc { bindings, push_constants: 8 }), "disposition refusee")?;
        let attributes = (0..8)
            .map(|index| backend::VertexAttribute { location: index, binding: 0, format: VertexFormat::Uint1, offset: index * 4 })
            .collect();
        let pipeline = create(
            Create::Pipeline(PipelineRequest {
                layout,
                vertex,
                fragment,
                desc: backend::PipelineDesc {
                    layout: 0,
                    vertex: 0,
                    fragment: 0,
                    topology: Topology::Triangles,
                    cull: 0,
                    front_ccw: false,
                    blend: Blend::None,
                    color_format: Format::Bgra8,
                    depth_format: None,
                    depth_test: false,
                    depth_write: false,
                    bindings: alloc::vec![backend::VertexBinding { stride: 32, per_instance: true }],
                    attributes,
                },
            }),
            "pipeline refuse",
        )?;
        let sampler = create(Create::Sampler(backend::SamplerDesc { linear: false, address: 0 }), "echantillonneur refuse")?;
        let image = |format: Format, width: u32, height: u32, usage: u32, name: &'static str| {
            create(
                Create::Image {
                    desc: ImageDesc { format, width, height, usage, shareable: false },
                    memory: 0,
                    offset: 0,
                },
                name,
            )
        };
        let glyphs = image(Format::R8, 128, 256, backend::image_usage::SAMPLED, "atlas des glyphes refuse")?;
        let icon_atlas = image(Format::R8, 1024, 1024, backend::image_usage::SAMPLED, "atlas des icones refuse")?;
        let blank = image(Format::Bgra8, 1, 1, backend::image_usage::SAMPLED, "image vide refusee")?;
        let fence = gpu::fence_create(&owner, context).map_err(fail("barriere refusee"))?;
        let composed = image(Format::Bgra8, width, height, backend::image_usage::RENDER, "image composee refusee")?;
        let output = Self::area(&owner, context, width as u64 * height as u64 * 4, MemKind::Readback)?;

        let mut renderer = Renderer {
            owner,
            context,
            pipeline,
            sampler,
            glyphs,
            icon_atlas,
            blank,
            width,
            height,
            composed,
            output,
            back: None,
            instances: None,
            upload: None,
            fence,
            icons: IconCache::default(),
            textures: Vec::new(),
            composed_valid: false,
            on_screen,
            pending: None,
            stats: Stats::default(),
        };
        renderer.upload_glyphs()?;
        if on_screen {
            gpu::screen::show(&renderer.owner, context, renderer.output.memory, width, height).map_err(fail("ecran refuse"))?;
        }
        Ok(renderer)
    }

    /// A memory of `size` bytes, mapped in the kernel, with a buffer over
    /// all of it.
    fn area(owner: &Owner, context: u32, size: u64, kind: MemKind) -> Result<Area, &'static str> {
        let size = size.div_ceil(ALIGN) * ALIGN;
        let memory = gpu::mem_alloc(owner, context, size, kind).map_err(fail("memoire refusee"))?;
        let address = match gpu::mem_map(owner, context, memory) {
            Ok(address) => address,
            Err(error) => {
                let _ = gpu::mem_free(owner, context, memory);
                return Err(fail("projection refusee")(error));
            }
        };
        let buffer = gpu::object_create(
            owner,
            context,
            Create::Buffer { size, usage: backend::buffer_usage::VERTEX | backend::buffer_usage::STORAGE, memory, offset: 0 },
        );
        match buffer {
            Ok(buffer) => Ok(Area { memory, buffer, address, size }),
            Err(error) => {
                let _ = gpu::mem_free(owner, context, memory);
                Err(fail("tampon refuse")(error))
            }
        }
    }

    fn free_area(&self, area: Area) {
        let _ = gpu::object_destroy(&self.owner, self.context, area.buffer);
        let _ = gpu::mem_free(&self.owner, self.context, area.memory);
    }

    /// An area of at least `size` bytes in `slot`, remade larger if needed.
    fn ensure(&mut self, which: fn(&mut Renderer) -> &mut Option<Area>, size: u64, kind: MemKind) -> Result<(), &'static str> {
        if which(self).as_ref().is_some_and(|area| area.size >= size) {
            return Ok(());
        }
        if let Some(old) = which(self).take() {
            self.free_area(old);
        }
        // Grown by half again: a canvas that grows a little each frame
        // must not remake the area each frame.
        let area = Self::area(&self.owner, self.context, size + size / 2, kind)?;
        *which(self) = Some(area);
        Ok(())
    }

    /// Submits `words` and waits for the GPU, `wait_ms` at most. A wait
    /// that runs out leaves the work pending (`Slow`).
    fn run(&mut self, words: &Words, wait_ms: u64) -> Result<(), DrawError> {
        let lost = |error| DrawError::Lost(fail("soumission refusee")(error));
        gpu::submit(&self.owner, self.context, &words.0, self.fence, &[]).map_err(lost)?;
        let view = gpu::fence_view(&self.owner, self.fence).map_err(|e| DrawError::Lost(fail("barriere perdue")(e)))?;
        let started = crate::posix::time::monotonic_ns();
        let waited = gpu::fence_wait(core::slice::from_ref(&view), true, Some(wait_ms));
        self.stats.wait_ns += crate::posix::time::monotonic_ns() - started;
        if view.context.is_lost() {
            return Err(DrawError::Lost("contexte GPU perdu"));
        }
        if waited.is_err() {
            self.pending = Some(view);
            return Err(DrawError::Slow);
        }
        Ok(())
    }

    /// Has the pending frame finished? Never waits.
    pub fn settle(&mut self) -> Settled {
        let Some(view) = self.pending.as_ref() else {
            return Settled::Done;
        };
        if view.context.is_lost() {
            return Settled::Lost;
        }
        if gpu::fence_wait(core::slice::from_ref(view), true, Some(0)).is_ok() {
            self.pending = None;
            return Settled::Done;
        }
        Settled::Pending
    }

    /// The system font, in the glyph atlas: 256 cells of 8 x 16, one byte a
    /// pixel, 255 where the glyph's bit is set.
    fn upload_glyphs(&mut self) -> Result<(), &'static str> {
        const BYTES: u64 = 128 * 256;
        self.ensure(|r| &mut r.upload, BYTES, MemKind::Visible)?;
        let Some(upload) = self.upload.as_ref() else { return Err("zone de copie absente") };
        let base = upload.address as *mut u8;
        for code in 0..=255u32 {
            let glyph = crate::graphics::font::glyph(code as u8);
            let (cell_x, cell_y) = ((code % 16) * 8, (code / 16) * 16);
            for (row, bits) in glyph.iter().enumerate() {
                for column in 0..8u32 {
                    let lit = bits & (0x80 >> column) != 0;
                    let at = (cell_y + row as u32) * 128 + cell_x + column;
                    // SAFETY: inside the upload area, mapped for `BYTES` at
                    // least; the GPU reads it only in the submission below.
                    unsafe { base.add(at as usize).write_volatile(if lit { 255 } else { 0 }) };
                }
            }
        }
        let mut words = Words::default();
        let buffer = upload.buffer;
        words.copy_to_image(buffer, 0, 128, self.glyphs, Rect::new(0, 0, 128, 256));
        // The blank canvas: black, so a batch that binds it samples a
        // defined texel even though no instance ever reads it.
        words.cmd(op::CLEAR_IMAGE, &[self.blank, 0, 0, 0, float_bits(1)]);
        self.run(&words, SETUP_WAIT_MS).map_err(|error| match error {
            DrawError::Slow => "copie de l'atlas trop lente",
            DrawError::Lost(reason) => reason,
        })
    }

    /// Remakes the screen's images at a new size and shows the new output.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), &'static str> {
        if (width, height) == (self.width, self.height) {
            return Ok(());
        }
        let composed = gpu::object_create(
            &self.owner,
            self.context,
            Create::Image {
                desc: ImageDesc { format: Format::Bgra8, width, height, usage: backend::image_usage::RENDER, shareable: false },
                memory: 0,
                offset: 0,
            },
        )
        .map_err(fail("image composee refusee"))?;
        let output = match Self::area(&self.owner, self.context, width as u64 * height as u64 * 4, MemKind::Readback) {
            Ok(output) => output,
            Err(reason) => {
                let _ = gpu::object_destroy(&self.owner, self.context, composed);
                return Err(reason);
            }
        };
        if self.on_screen
            && let Err(error) = gpu::screen::show(&self.owner, self.context, output.memory, width, height)
        {
            let _ = gpu::object_destroy(&self.owner, self.context, composed);
            self.free_area(output);
            return Err(fail("ecran refuse")(error));
        }
        // The old output is no longer shown: `show` retired its resource.
        let old_output = core::mem::replace(&mut self.output, output);
        self.free_area(old_output);
        let old_composed = core::mem::replace(&mut self.composed, composed);
        let _ = gpu::object_destroy(&self.owner, self.context, old_composed);
        if let Some(back) = self.back.take() {
            self.free_area(back);
        }
        self.width = width;
        self.height = height;
        self.composed_valid = false;
        Ok(())
    }

    /// Gives the screen back to the processor and closes the context:
    /// everything the GPU held for the compositor goes with it.
    pub fn teardown(self) {
        if self.on_screen {
            gpu::screen::release();
        }
        let _ = gpu::context_destroy(&self.owner, self.context);
    }

    /// Canvases held as textures, and icons in the atlas.
    pub fn held(&self) -> (usize, usize) {
        (self.textures.len(), self.icons.len())
    }

    /// Reads a pixel of the output back (`0x00RRGGBB`), for the self-test.
    pub fn output_pixel(&self, x: u32, y: u32) -> Option<u32> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let at = self.output.address + (y as u64 * self.width as u64 + x as u64) * 4;
        // SAFETY: inside the output memory, mapped for the whole image.
        Some(unsafe { (at as *const u32).read_volatile() } & 0x00FF_FFFF)
    }
}


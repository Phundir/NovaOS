//! The Nova command buffer: its format, and its validation (spec 6.8).
//!
//! ## Format
//!
//! A flat array of 32-bit little-endian words, in the program's memory. A
//! command is a header word -- opcode in the low half, length in words
//! (header included) in the high half -- followed by its operands. Objects
//! are named by their handles; 64-bit offsets take two words, low first;
//! colours, depths and viewports are the bit patterns of 32-bit floats.
//! The opcodes and their operands are listed at `op` and repeated in
//! `nova_gfx.h`; the two move together.
//!
//! ## Validation
//!
//! The whole buffer is checked before anything is sent, and one invalid
//! command refuses it whole, with no partial effect: a program never learns
//! half its work was done. Checked, command by command:
//!
//! - every handle names an object of this context, of the kind the operand
//!   wants, created with the usage the command makes of it;
//! - every offset, size and rectangle lies inside its buffer or image, with
//!   the arithmetic checked for overflow;
//! - the state a command depends on exists: a draw has a render pass, a
//!   pipeline whose formats match the pass's images, a vertex buffer per
//!   vertex binding, a resource per binding of the layout; a copy is made
//!   outside any render pass;
//! - counts are bounded, so that one submission cannot ask the GPU for an
//!   unbounded amount of work (the watchdog bounds the rest).
//!
//! What the Vulkan rules would leave to the program -- viewport and scissor
//! unset, image layouts, barriers between passes -- is supplied by the
//! translation: the render area is the default viewport and scissor, every
//! image lives in the general layout from its first use, and a full memory
//! barrier separates passes. A program cannot write a hazard.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use super::backend::{
    buffer_usage, image_usage, BindingKind, Command, Format, Id, ImageRef, LayoutBinding, LoadOp, Rect,
    ResourceWrite,
};

/// Opcodes (`NOVA_CMD_*`).
pub mod op {
    pub const BEGIN_RENDER: u32 = 1;
    pub const END_RENDER: u32 = 2;
    pub const BIND_PIPELINE: u32 = 3;
    pub const BIND_VERTEX: u32 = 4;
    pub const BIND_INDEX: u32 = 5;
    pub const BIND_RESOURCE: u32 = 6;
    pub const VIEWPORT: u32 = 7;
    pub const SCISSOR: u32 = 8;
    pub const PUSH_CONSTANTS: u32 = 9;
    pub const DRAW: u32 = 10;
    pub const DRAW_INDEXED: u32 = 11;
    pub const COPY_BUFFER: u32 = 12;
    pub const COPY_BUFFER_TO_IMAGE: u32 = 13;
    pub const COPY_IMAGE_TO_BUFFER: u32 = 14;
    pub const COPY_IMAGE: u32 = 15;
    pub const BLIT_IMAGE: u32 = 16;
    pub const CLEAR_IMAGE: u32 = 17;
    pub const FILL_BUFFER: u32 = 18;
    pub const UPDATE_BUFFER: u32 = 19;
}

/// Largest command buffer, in bytes.
pub const MAX_SIZE: usize = 1 << 20;
/// Largest draw: vertices (or indices), and instances.
const MAX_VERTICES: u32 = 1 << 24;
const MAX_INSTANCES: u32 = 1 << 16;
/// `vkCmdUpdateBuffer` takes at most 64 KiB.
const MAX_UPDATE: usize = 65536;
/// Vertex buffer slots.
pub const VERTEX_SLOTS: usize = 4;

/// What validation needs to know of an object.
#[derive(Debug, Clone)]
pub enum Info {
    Buffer { size: u64, usage: u32 },
    Image { format: Format, width: u32, height: u32, usage: u32, initialized: bool },
    Sampler,
    Shader,
    Layout { bindings: Vec<LayoutBinding>, push_constants: u32 },
    Pipeline { layout: u32, color: Format, depth: Option<Format>, vertex_bindings: u32 },
}

#[derive(Debug, Clone)]
pub struct Object {
    pub backend: Id,
    pub info: Info,
}

pub type Refusal = &'static str;

struct Reader<'a> {
    words: &'a [u32],
    at: usize,
}

impl Reader<'_> {
    fn u32(&mut self) -> Result<u32, Refusal> {
        let value = *self.words.get(self.at).ok_or("commande tronquee")?;
        self.at += 1;
        Ok(value)
    }
    fn i32(&mut self) -> Result<i32, Refusal> {
        Ok(self.u32()? as i32)
    }
    fn u64(&mut self) -> Result<u64, Refusal> {
        let low = self.u32()? as u64;
        let high = self.u32()? as u64;
        Ok(low | high << 32)
    }
    fn rest(&self) -> &[u32] {
        &self.words[self.at..]
    }
}

struct Render {
    color: Format,
    depth: Option<Format>,
}

struct Bound {
    pipeline: u32,
    layout: u32,
    layout_id: Id,
    vertex_bindings: u32,
}

struct State<'a> {
    objects: &'a BTreeMap<u32, Object>,
    out: Vec<Command>,
    render: Option<Render>,
    bound: Option<Bound>,
    vertex: [bool; VERTEX_SLOTS],
    index: Option<(u64, bool)>,
    resources: BTreeMap<u32, ResourceWrite>,
    resources_dirty: bool,
    first_use: Vec<u32>,
}

impl State<'_> {
    fn object(&self, handle: u32) -> Result<&Object, Refusal> {
        self.objects.get(&handle).ok_or("objet inconnu dans ce contexte")
    }

    fn buffer(&self, handle: u32, usage: u32) -> Result<(Id, u64), Refusal> {
        match &self.object(handle)?.info {
            Info::Buffer { size, usage: have } => {
                if have & usage != usage {
                    return Err("tampon cree sans cet usage");
                }
                Ok((self.object(handle)?.backend, *size))
            }
            _ => Err("cet objet n'est pas un tampon"),
        }
    }

    fn image(&mut self, handle: u32, usage: u32) -> Result<ImageRef, Refusal> {
        let object = self.object(handle)?;
        match &object.info {
            Info::Image { format, width, height, usage: have, initialized } => {
                if have & usage != usage {
                    return Err("image creee sans cet usage");
                }
                let first_use = !initialized && !self.first_use.contains(&handle);
                let image = ImageRef { id: object.backend, format: *format, width: *width, height: *height, first_use };
                if first_use {
                    self.first_use.push(handle);
                }
                Ok(image)
            }
            _ => Err("cet objet n'est pas une image"),
        }
    }

    fn outside(&self) -> Result<(), Refusal> {
        if self.render.is_some() { Err("commande interdite dans une passe de rendu") } else { Ok(()) }
    }

    fn inside(&self) -> Result<&Render, Refusal> {
        self.render.as_ref().ok_or("commande hors d'une passe de rendu")
    }
}

/// A byte range inside an object of `size` bytes.
fn within(offset: u64, length: u64, size: u64) -> Result<(), Refusal> {
    match offset.checked_add(length) {
        Some(end) if end <= size => Ok(()),
        _ => Err("plage hors de l'objet"),
    }
}

/// A rectangle inside an image.
fn inside_image(rect: Rect, image: &ImageRef) -> Result<(), Refusal> {
    if rect.x < 0 || rect.y < 0 || rect.width == 0 || rect.height == 0 {
        return Err("rectangle vide ou negatif");
    }
    let right = (rect.x as u64) + rect.width as u64;
    let bottom = (rect.y as u64) + rect.height as u64;
    if right > image.width as u64 || bottom > image.height as u64 {
        return Err("rectangle hors de l'image");
    }
    Ok(())
}

fn read_rect(reader: &mut Reader) -> Result<Rect, Refusal> {
    Ok(Rect { x: reader.i32()?, y: reader.i32()?, width: reader.u32()?, height: reader.u32()? })
}

/// Bytes a buffer-image copy reads or writes, from `offset`.
fn copy_extent(offset: u64, row_pixels: u32, rect: Rect, format: Format) -> Result<u64, Refusal> {
    if offset % 4 != 0 {
        return Err("decalage de copie non aligne sur 4");
    }
    let row = if row_pixels == 0 { rect.width } else { row_pixels };
    if row < rect.width {
        return Err("ligne de copie plus courte que le rectangle");
    }
    let bpp = format.bytes_per_pixel();
    let stride = (row as u64).checked_mul(bpp).ok_or("copie trop grande")?;
    let body = stride.checked_mul(rect.height as u64 - 1).ok_or("copie trop grande")?;
    body.checked_add(rect.width as u64 * bpp).ok_or("copie trop grande")
}

fn load_op(value: u32) -> Result<LoadOp, Refusal> {
    match value {
        0 => Ok(LoadOp::Clear),
        1 => Ok(LoadOp::Load),
        2 => Ok(LoadOp::DontCare),
        _ => Err("operation de chargement inconnue"),
    }
}

/// Validates and translates a command buffer. On success, also returns
/// the images used for the first time -- the caller marks them
/// initialized once the submission is accepted.
pub fn translate(words: &[u32], objects: &BTreeMap<u32, Object>) -> Result<(Vec<Command>, Vec<u32>), Refusal> {
    let mut state = State {
        objects,
        out: Vec::new(),
        render: None,
        bound: None,
        vertex: [false; VERTEX_SLOTS],
        index: None,
        resources: BTreeMap::new(),
        resources_dirty: false,
        first_use: Vec::new(),
    };
    let mut position = 0;
    // Each turn advances by the command's length, checked non-zero.
    while position < words.len() {
        let header = words[position];
        let opcode = header & 0xFFFF;
        let length = (header >> 16) as usize;
        if length == 0 || position + length > words.len() {
            return Err("longueur de commande invalide");
        }
        let mut reader = Reader { words: &words[position + 1..position + length], at: 0 };
        command(&mut state, opcode, &mut reader)?;
        if reader.at != reader.words.len() && !matches!(opcode, op::PUSH_CONSTANTS | op::UPDATE_BUFFER) {
            return Err("operandes en trop");
        }
        position += length;
    }
    if state.render.is_some() {
        return Err("passe de rendu non terminee");
    }
    Ok((state.out, state.first_use))
}

fn command(state: &mut State, opcode: u32, reader: &mut Reader) -> Result<(), Refusal> {
    match opcode {
        op::BEGIN_RENDER => {
            state.outside()?;
            let color_handle = reader.u32()?;
            let load = load_op(reader.u32()?)?;
            let clear = [reader.u32()?, reader.u32()?, reader.u32()?, reader.u32()?];
            let depth_handle = reader.u32()?;
            let depth_load = load_op(reader.u32()?)?;
            let depth_clear = reader.u32()?;
            let area = read_rect(reader)?;
            let color = state.image(color_handle, image_usage::RENDER)?;
            if color.format.is_depth() {
                return Err("une image de profondeur n'est pas une cible de couleur");
            }
            inside_image(area, &color)?;
            let depth = if depth_handle == 0 {
                None
            } else {
                let depth = state.image(depth_handle, image_usage::DEPTH)?;
                if !depth.format.is_depth() {
                    return Err("image de profondeur sans format de profondeur");
                }
                inside_image(area, &depth)?;
                Some((depth, depth_load, depth_clear))
            };
            state.render = Some(Render { color: color.format, depth: depth.map(|(d, _, _)| d.format) });
            // A pass starts with nothing bound: what a program relies on
            // it binds again, inside the pass.
            state.bound = None;
            state.vertex = [false; VERTEX_SLOTS];
            state.index = None;
            state.resources.clear();
            state.resources_dirty = false;
            state.out.push(Command::BeginRender { color, load, clear, depth, area });
            // The default viewport and scissor: the render area.
            state.out.push(Command::Viewport {
                x: float_bits(area.x.max(0) as u32),
                y: float_bits(area.y.max(0) as u32),
                width: float_bits(area.width),
                height: float_bits(area.height),
                min_depth: 0,
                max_depth: 0x3F80_0000,
            });
            state.out.push(Command::Scissor(area));
        }
        op::END_RENDER => {
            state.inside()?;
            state.render = None;
            state.out.push(Command::EndRender);
        }
        op::BIND_PIPELINE => {
            let render = state.inside()?;
            let (color, depth) = (render.color, render.depth);
            let handle = reader.u32()?;
            let object = state.object(handle)?;
            let Info::Pipeline { layout, color: pc, depth: pd, vertex_bindings } = object.info else {
                return Err("cet objet n'est pas un pipeline");
            };
            if pc != color || pd != depth {
                return Err("formats du pipeline differents de ceux de la passe");
            }
            let layout_id = state.object(layout)?.backend;
            let pipeline = object.backend;
            // A layout change invalidates the resources pushed for the old.
            if state.bound.as_ref().is_none_or(|b| b.layout != layout) {
                state.resources.clear();
                state.resources_dirty = false;
            }
            state.bound = Some(Bound { pipeline: handle, layout, layout_id, vertex_bindings });
            state.out.push(Command::BindPipeline { pipeline });
        }
        op::BIND_VERTEX => {
            state.inside()?;
            let slot = reader.u32()?;
            let (buffer, size) = state.buffer(reader.u32()?, buffer_usage::VERTEX)?;
            let offset = reader.u64()?;
            if slot as usize >= VERTEX_SLOTS {
                return Err("emplacement de sommets hors limites");
            }
            if offset >= size {
                return Err("decalage hors du tampon de sommets");
            }
            state.vertex[slot as usize] = true;
            state.out.push(Command::BindVertex { slot, buffer, offset });
        }
        op::BIND_INDEX => {
            state.inside()?;
            let (buffer, size) = state.buffer(reader.u32()?, buffer_usage::INDEX)?;
            let offset = reader.u64()?;
            let wide = match reader.u32()? {
                0 => false,
                1 => true,
                _ => return Err("type d'indice inconnu"),
            };
            if offset >= size || offset % if wide { 4 } else { 2 } != 0 {
                return Err("decalage d'indices invalide");
            }
            state.index = Some((size - offset, wide));
            state.out.push(Command::BindIndex { buffer, offset, wide });
        }
        op::BIND_RESOURCE => {
            state.inside()?;
            let binding = reader.u32()?;
            let object = reader.u32()?;
            let sampler = reader.u32()?;
            let offset = reader.u64()?;
            let range = reader.u64()?;
            let bound = state.bound.as_ref().ok_or("ressource liee sans pipeline")?;
            let layout = state.object(bound.layout)?;
            let Info::Layout { bindings, .. } = &layout.info else {
                return Err("disposition introuvable");
            };
            let kind = bindings.iter().find(|b| b.binding == binding).ok_or("liaison absente de la disposition")?.kind;
            let write = match kind {
                BindingKind::SampledImage => {
                    let image = state.image(object, image_usage::SAMPLED)?;
                    if image.format.is_depth() {
                        return Err("une image de profondeur ne s'echantillonne pas ici");
                    }
                    let sampler_object = state.object(sampler)?;
                    if !matches!(sampler_object.info, Info::Sampler) {
                        return Err("cet objet n'est pas un echantillonneur");
                    }
                    ResourceWrite::Image { binding, image: image.id, sampler: sampler_object.backend }
                }
                BindingKind::UniformBuffer | BindingKind::StorageBuffer => {
                    let usage = if kind == BindingKind::UniformBuffer { buffer_usage::UNIFORM } else { buffer_usage::STORAGE };
                    let (buffer, size) = state.buffer(object, usage)?;
                    let range = if range == 0 { size.checked_sub(offset).ok_or("plage hors du tampon")? } else { range };
                    within(offset, range, size)?;
                    if range == 0 || offset % 256 != 0 {
                        return Err("plage de tampon vide ou non alignee sur 256");
                    }
                    ResourceWrite::Buffer { binding, kind, buffer, offset, range }
                }
            };
            state.resources.insert(binding, write);
            state.resources_dirty = true;
        }
        op::VIEWPORT => {
            state.inside()?;
            state.out.push(Command::Viewport {
                x: reader.u32()?,
                y: reader.u32()?,
                width: reader.u32()?,
                height: reader.u32()?,
                min_depth: reader.u32()?,
                max_depth: reader.u32()?,
            });
        }
        op::SCISSOR => {
            state.inside()?;
            let rect = read_rect(reader)?;
            if rect.x < 0 || rect.y < 0 {
                return Err("rectangle de decoupe negatif");
            }
            state.out.push(Command::Scissor(rect));
        }
        op::PUSH_CONSTANTS => {
            state.inside()?;
            let offset = reader.u32()?;
            let data = reader.rest();
            let bound = state.bound.as_ref().ok_or("constantes sans pipeline")?;
            let Info::Layout { push_constants, .. } = state.object(bound.layout)?.info else {
                return Err("disposition introuvable");
            };
            let size = data.len() as u32 * 4;
            if offset % 4 != 0 || size == 0 || offset.checked_add(size).is_none_or(|end| end > push_constants) {
                return Err("constantes hors de la disposition");
            }
            let mut bytes = Vec::with_capacity(size as usize);
            for word in data {
                bytes.extend_from_slice(&word.to_le_bytes());
            }
            reader.at = reader.words.len();
            state.out.push(Command::PushConstants { layout: bound.layout_id, offset, data: bytes });
        }
        op::DRAW | op::DRAW_INDEXED => {
            state.inside()?;
            let indexed = opcode == op::DRAW_INDEXED;
            let count = reader.u32()?;
            let instances = reader.u32()?;
            let first = reader.u32()?;
            let vertex_offset = if indexed { reader.i32()? } else { 0 };
            let first_instance = reader.u32()?;
            ready_to_draw(state)?;
            if count == 0 || instances == 0 || count > MAX_VERTICES || instances > MAX_INSTANCES {
                return Err("nombre de sommets ou d'instances hors limites");
            }
            if indexed {
                let (available, wide) = state.index.ok_or("dessin indexe sans tampon d'indices")?;
                let size = if wide { 4 } else { 2 };
                let end = (first as u64 + count as u64) * size;
                if end > available {
                    return Err("indices hors du tampon");
                }
                state.out.push(Command::DrawIndexed { indices: count, instances, first_index: first, vertex_offset, first_instance });
            } else {
                state.out.push(Command::Draw { vertices: count, instances, first_vertex: first, first_instance });
            }
        }
        op::COPY_BUFFER => {
            state.outside()?;
            let (src, src_size) = state.buffer(reader.u32()?, 0)?;
            let (dst, dst_size) = state.buffer(reader.u32()?, 0)?;
            let src_offset = reader.u64()?;
            let dst_offset = reader.u64()?;
            let size = reader.u64()?;
            if size == 0 {
                return Err("copie vide");
            }
            within(src_offset, size, src_size)?;
            within(dst_offset, size, dst_size)?;
            if src == dst && src_offset < dst_offset + size && dst_offset < src_offset + size {
                return Err("copie sur elle-meme avec recouvrement");
            }
            state.out.push(Command::CopyBuffer { src, dst, src_offset, dst_offset, size });
        }
        op::COPY_BUFFER_TO_IMAGE => {
            state.outside()?;
            let (buffer, size) = state.buffer(reader.u32()?, 0)?;
            let offset = reader.u64()?;
            let row_pixels = reader.u32()?;
            let image = state.image(reader.u32()?, 0)?;
            let rect = read_rect(reader)?;
            inside_image(rect, &image)?;
            within(offset, copy_extent(offset, row_pixels, rect, image.format)?, size)?;
            state.out.push(Command::CopyBufferToImage { buffer, offset, row_pixels, image, rect });
        }
        op::COPY_IMAGE_TO_BUFFER => {
            state.outside()?;
            let image = state.image(reader.u32()?, 0)?;
            let rect = read_rect(reader)?;
            let (buffer, size) = state.buffer(reader.u32()?, 0)?;
            let offset = reader.u64()?;
            let row_pixels = reader.u32()?;
            inside_image(rect, &image)?;
            within(offset, copy_extent(offset, row_pixels, rect, image.format)?, size)?;
            state.out.push(Command::CopyImageToBuffer { image, rect, buffer, offset, row_pixels });
        }
        op::COPY_IMAGE => {
            state.outside()?;
            let src = state.image(reader.u32()?, 0)?;
            let src_x = reader.i32()?;
            let src_y = reader.i32()?;
            let dst = state.image(reader.u32()?, 0)?;
            let dst_x = reader.i32()?;
            let dst_y = reader.i32()?;
            let width = reader.u32()?;
            let height = reader.u32()?;
            if src.format != dst.format {
                return Err("copie entre formats differents");
            }
            inside_image(Rect { x: src_x, y: src_y, width, height }, &src)?;
            inside_image(Rect { x: dst_x, y: dst_y, width, height }, &dst)?;
            if src.id == dst.id {
                return Err("copie d'une image sur elle-meme");
            }
            state.out.push(Command::CopyImage { src, dst, src_x, src_y, dst_x, dst_y, width, height });
        }
        op::BLIT_IMAGE => {
            state.outside()?;
            let src = state.image(reader.u32()?, 0)?;
            let src_rect = read_rect(reader)?;
            let dst = state.image(reader.u32()?, 0)?;
            let dst_rect = read_rect(reader)?;
            let linear = match reader.u32()? {
                0 => false,
                1 => true,
                _ => return Err("filtre inconnu"),
            };
            if src.format.is_depth() || dst.format.is_depth() {
                return Err("mise a l'echelle d'une image de profondeur");
            }
            inside_image(src_rect, &src)?;
            inside_image(dst_rect, &dst)?;
            if src.id == dst.id {
                return Err("mise a l'echelle d'une image sur elle-meme");
            }
            state.out.push(Command::Blit { src, src_rect, dst, dst_rect, linear });
        }
        op::CLEAR_IMAGE => {
            state.outside()?;
            let image = state.image(reader.u32()?, 0)?;
            if image.format.is_depth() {
                return Err("effacement de couleur d'une image de profondeur");
            }
            let color = [reader.u32()?, reader.u32()?, reader.u32()?, reader.u32()?];
            state.out.push(Command::ClearImage { image, color });
        }
        op::FILL_BUFFER => {
            state.outside()?;
            let (buffer, size) = state.buffer(reader.u32()?, 0)?;
            let offset = reader.u64()?;
            let length = reader.u64()?;
            let value = reader.u32()?;
            if offset % 4 != 0 || length % 4 != 0 || length == 0 {
                return Err("remplissage non aligne sur 4");
            }
            within(offset, length, size)?;
            state.out.push(Command::FillBuffer { buffer, offset, size: length, value });
        }
        op::UPDATE_BUFFER => {
            state.outside()?;
            let (buffer, size) = state.buffer(reader.u32()?, 0)?;
            let offset = reader.u64()?;
            let data = reader.rest();
            let bytes_len = data.len() * 4;
            if offset % 4 != 0 || bytes_len == 0 || bytes_len > MAX_UPDATE {
                return Err("mise a jour vide, trop longue ou non alignee");
            }
            within(offset, bytes_len as u64, size)?;
            let mut bytes = Vec::with_capacity(bytes_len);
            for word in data {
                bytes.extend_from_slice(&word.to_le_bytes());
            }
            reader.at = reader.words.len();
            state.out.push(Command::UpdateBuffer { buffer, offset, data: bytes });
        }
        _ => return Err("commande inconnue"),
    }
    Ok(())
}

/// Everything a draw needs: a pipeline, its vertex buffers, a resource
/// per binding of its layout. Pushes the resources when they changed.
fn ready_to_draw(state: &mut State) -> Result<(), Refusal> {
    let bound = state.bound.as_ref().ok_or("dessin sans pipeline")?;
    for slot in 0..bound.vertex_bindings as usize {
        if !state.vertex.get(slot).copied().unwrap_or(false) {
            return Err("dessin sans tampon de sommets pour une liaison du pipeline");
        }
    }
    let Info::Layout { bindings, .. } = &state.object(bound.layout)?.info else {
        return Err("disposition introuvable");
    };
    for binding in bindings {
        if !state.resources.contains_key(&binding.binding) {
            return Err("dessin sans ressource pour une liaison de la disposition");
        }
    }
    if state.resources_dirty && !bindings.is_empty() {
        let writes: Vec<ResourceWrite> = state.resources.values().copied().collect();
        let layout = bound.layout_id;
        state.out.push(Command::PushResources { layout, writes });
        state.resources_dirty = false;
    }
    let _ = bound.pipeline;
    Ok(())
}

/// The IEEE-754 single-precision pattern of a small unsigned integer --
/// exact below 2^24, which bounds every image size. Computed on the bits:
/// the kernel is soft-float and takes no float instruction.
pub fn float_bits(value: u32) -> u32 {
    if value == 0 {
        return 0;
    }
    let exponent = 31 - value.leading_zeros();
    let mantissa = if exponent >= 23 { value >> (exponent - 23) } else { value << (23 - exponent) };
    ((exponent + 127) << 23) | (mantissa & 0x7F_FFFF)
}

//! The adapter from the core's backend `Context` to a module's context:
//! translates validated core commands into the C command array the
//! module's `submit` takes.

use super::*;

pub struct ModuleContext {
    pub(super) table: &'static KmdGpu,
    pub(super) context: *mut u8,
}

// SAFETY: the context pointer is the module's; the core never enters one
// context twice at once.
unsafe impl Send for ModuleContext {}

pub(super) fn image_ref(image: &backend::ImageRef) -> KmdImageRef {
    KmdImageRef { id: image.id, format: image.format as u32, width: image.width, height: image.height, first_use: image.first_use as u32 }
}

pub(super) fn rect(r: &backend::Rect) -> KmdRect {
    KmdRect { x: r.x, y: r.y, width: r.width, height: r.height }
}

pub(super) fn load_op(op: backend::LoadOp) -> u32 {
    match op {
        backend::LoadOp::Clear => 0,
        backend::LoadOp::Load => 1,
        backend::LoadOp::DontCare => 2,
    }
}

pub(super) fn binding_kind(kind: backend::BindingKind) -> u32 {
    match kind {
        backend::BindingKind::SampledImage => 1,
        backend::BindingKind::UniformBuffer => 2,
        backend::BindingKind::StorageBuffer => 3,
    }
}

pub(super) fn image_desc(desc: &ImageDesc) -> KmdImageDesc {
    KmdImageDesc { format: desc.format as u32, width: desc.width, height: desc.height, usage: desc.usage, shareable: desc.shareable as u32 }
}

/// A command list in the module's terms. The resource writes live in
/// `writes`, which must outlive the call.
pub(super) fn commands(list: &[Command], writes: &mut Vec<Vec<KmdWrite>>) -> Vec<KmdCommand> {
    use crate::gpu::cmd::op;
    let mut out = Vec::with_capacity(list.len());
    for command in list {
        let (opcode, body) = match command {
            Command::BeginRender { color, load, clear, depth, area } => {
                let (has_depth, depth_ref, depth_load, depth_clear) = match depth {
                    Some((image, op, value)) => (1, image_ref(image), load_op(*op), *value),
                    None => (0, KmdImageRef::default(), 2, 0),
                };
                (op::BEGIN_RENDER, Body {
                    begin_render: BeginRender {
                        color: image_ref(color),
                        load: load_op(*load),
                        clear: *clear,
                        has_depth,
                        depth: depth_ref,
                        depth_load,
                        depth_clear,
                        area: rect(area),
                    },
                })
            }
            Command::EndRender => (op::END_RENDER, Body { one: 0 }),
            Command::BindPipeline { pipeline } => (op::BIND_PIPELINE, Body { one: *pipeline }),
            Command::BindVertex { slot, buffer, offset } => (op::BIND_VERTEX, Body { bind_vertex: BindVertex { slot: *slot, pad: 0, buffer: *buffer, offset: *offset } }),
            Command::BindIndex { buffer, offset, wide } => (op::BIND_INDEX, Body { bind_index: BindIndex { buffer: *buffer, offset: *offset, wide: *wide as u32 } }),
            Command::PushResources { layout, writes: list } => {
                let converted: Vec<KmdWrite> = list
                    .iter()
                    .map(|w| match w {
                        backend::ResourceWrite::Image { binding, image, sampler } => {
                            KmdWrite { binding: *binding, kind: 1, object: *image, sampler: *sampler, offset: 0, range: 0 }
                        }
                        backend::ResourceWrite::Buffer { binding, kind, buffer, offset, range } => {
                            KmdWrite { binding: *binding, kind: binding_kind(*kind), object: *buffer, sampler: 0, offset: *offset, range: *range }
                        }
                    })
                    .collect();
                let pointer = converted.as_ptr();
                let count = converted.len() as u32;
                writes.push(converted);
                (20, Body { push: Push { layout: *layout, count, writes: pointer } })
            }
            Command::Viewport { x, y, width, height, min_depth, max_depth } => {
                (op::VIEWPORT, Body { viewport: [*x, *y, *width, *height, *min_depth, *max_depth] })
            }
            Command::Scissor(r) => (op::SCISSOR, Body { rect: rect(r) }),
            Command::PushConstants { layout, offset, data } => {
                (op::PUSH_CONSTANTS, Body { constants: Constants { layout: *layout, offset: *offset, size: data.len() as u32, data: data.as_ptr() } })
            }
            Command::Draw { vertices, instances, first_vertex, first_instance } => {
                (op::DRAW, Body { draw: Draw { count: *vertices, instances: *instances, first: *first_vertex, first_instance: *first_instance, vertex_offset: 0 } })
            }
            Command::DrawIndexed { indices, instances, first_index, vertex_offset, first_instance } => {
                (op::DRAW_INDEXED, Body { draw: Draw { count: *indices, instances: *instances, first: *first_index, first_instance: *first_instance, vertex_offset: *vertex_offset } })
            }
            Command::CopyBuffer { src, dst, src_offset, dst_offset, size } => {
                (op::COPY_BUFFER, Body { copy_buffer: [*src, *dst, *src_offset, *dst_offset, *size] })
            }
            Command::CopyBufferToImage { buffer, offset, row_pixels, image, rect: r } => (op::COPY_BUFFER_TO_IMAGE, Body {
                buffer_image: BufferImage { buffer: *buffer, offset: *offset, row_pixels: *row_pixels, image: image_ref(image), rect: rect(r) },
            }),
            Command::CopyImageToBuffer { image, rect: r, buffer, offset, row_pixels } => (op::COPY_IMAGE_TO_BUFFER, Body {
                buffer_image: BufferImage { buffer: *buffer, offset: *offset, row_pixels: *row_pixels, image: image_ref(image), rect: rect(r) },
            }),
            Command::CopyImage { src, dst, src_x, src_y, dst_x, dst_y, width, height } => (op::COPY_IMAGE, Body {
                copy_image: CopyImage {
                    src: image_ref(src),
                    dst: image_ref(dst),
                    src_x: *src_x,
                    src_y: *src_y,
                    dst_x: *dst_x,
                    dst_y: *dst_y,
                    width: *width,
                    height: *height,
                },
            }),
            Command::Blit { src, src_rect, dst, dst_rect, linear } => (op::BLIT_IMAGE, Body {
                blit: Blit { src: image_ref(src), dst: image_ref(dst), src_rect: rect(src_rect), dst_rect: rect(dst_rect), linear: *linear as u32 },
            }),
            Command::ClearImage { image, color } => (op::CLEAR_IMAGE, Body { clear: Clear { image: image_ref(image), color: *color } }),
            Command::FillBuffer { buffer, offset, size, value } => (op::FILL_BUFFER, Body { fill: Fill { buffer: *buffer, offset: *offset, size: *size, value: *value } }),
            Command::UpdateBuffer { buffer, offset, data } => {
                (op::UPDATE_BUFFER, Body { update: Update { buffer: *buffer, offset: *offset, size: data.len() as u64, data: data.as_ptr() } })
            }
        };
        out.push(KmdCommand { opcode, pad: 0, body });
    }
    out
}

impl backend::Context for ModuleContext {
    fn alloc(&mut self, size: u64, kind: MemKind) -> backend::Result<(Id, u64)> {
        let kind = match kind {
            MemKind::Local => 0,
            MemKind::Visible => 1,
            MemKind::Readback => 2,
        };
        let (mut id, mut real) = (0u64, 0u64);
        let code = (self.table.alloc)(self.context, size, kind, &mut id, &mut real);
        if code != 0 { Err(error(code)) } else { Ok((id, real.max(size))) }
    }

    fn free(&mut self, mem: Id) {
        (self.table.free)(self.context, mem);
    }

    fn map(&mut self, mem: Id) -> backend::Result<Window> {
        let mut window = KmdWindow::default();
        let code = (self.table.map)(self.context, mem, &mut window);
        if code != 0 || window.physical % PAGE != 0 || window.size == 0 {
            return Err(error(code.min(-5)));
        }
        let cache = window.cache_flags & (flags::NO_CACHE | flags::WRITE_THROUGH | memory::PAT_4K);
        Ok(Window { phys: window.physical, size: (window.size + PAGE - 1) & !(PAGE - 1), cache_flags: cache })
    }

    fn unmap(&mut self, mem: Id) {
        (self.table.unmap)(self.context, mem);
    }

    fn create_buffer(&mut self, desc: &backend::BufferDesc, mem: Id, offset: u64) -> backend::Result<Id> {
        let d = KmdBufferDesc { size: desc.size, usage: desc.usage };
        let mut id = 0;
        let code = (self.table.create_buffer)(self.context, &d, mem, offset, &mut id);
        if code != 0 { Err(error(code)) } else { Ok(id) }
    }

    fn create_image(&mut self, desc: &ImageDesc, memory: Option<(Id, u64)>) -> backend::Result<(Id, u64)> {
        let d = image_desc(desc);
        let (mem, offset, placed) = memory.map(|(m, o)| (m, o, 1)).unwrap_or((0, 0, 0));
        let (mut id, mut own) = (0, 0);
        let code = (self.table.create_image)(self.context, &d, mem, offset, placed, &mut id, &mut own);
        if code != 0 { Err(error(code)) } else { Ok((id, own)) }
    }

    fn create_sampler(&mut self, desc: &backend::SamplerDesc) -> backend::Result<Id> {
        let d = KmdSamplerDesc { linear: desc.linear as u32, address: desc.address };
        let mut id = 0;
        let code = (self.table.create_sampler)(self.context, &d, &mut id);
        if code != 0 { Err(error(code)) } else { Ok(id) }
    }

    fn create_shader(&mut self, code_words: &[u32]) -> backend::Result<Id> {
        let mut id = 0;
        let code = (self.table.create_shader)(self.context, code_words.as_ptr(), code_words.len() as u32, &mut id);
        if code != 0 { Err(error(code)) } else { Ok(id) }
    }

    fn create_layout(&mut self, desc: &backend::LayoutDesc) -> backend::Result<Id> {
        let mut d = KmdLayoutDesc { count: desc.bindings.len().min(16) as u32, push_constants: desc.push_constants, ..Default::default() };
        for (slot, binding) in d.bindings.iter_mut().zip(desc.bindings.iter()) {
            *slot = KmdLayoutBinding { binding: binding.binding, kind: binding_kind(binding.kind), stages: binding.stages };
        }
        let mut id = 0;
        let code = (self.table.create_layout)(self.context, &d, &mut id);
        if code != 0 { Err(error(code)) } else { Ok(id) }
    }

    fn create_pipeline(&mut self, desc: &backend::PipelineDesc) -> backend::Result<Id> {
        let mut d = KmdPipelineDesc {
            layout: desc.layout,
            vertex: desc.vertex,
            fragment: desc.fragment,
            topology: desc.topology as u32,
            cull: desc.cull,
            front_ccw: desc.front_ccw as u32,
            blend: desc.blend as u32,
            color_format: desc.color_format as u32,
            depth_format: desc.depth_format.map(|f| f as u32).unwrap_or(0),
            depth_test: desc.depth_test as u32,
            depth_write: desc.depth_write as u32,
            binding_count: desc.bindings.len().min(4) as u32,
            attribute_count: desc.attributes.len().min(8) as u32,
            ..Default::default()
        };
        for (slot, b) in d.bindings.iter_mut().zip(desc.bindings.iter()) {
            *slot = [b.stride, b.per_instance as u32];
        }
        for (slot, a) in d.attributes.iter_mut().zip(desc.attributes.iter()) {
            *slot = [a.location, a.binding, a.format as u32, a.offset];
        }
        let mut id = 0;
        let code = (self.table.create_pipeline)(self.context, &d, &mut id);
        if code != 0 { Err(error(code)) } else { Ok(id) }
    }

    fn destroy(&mut self, kind: ObjectKind, id: Id) {
        let kind = match kind {
            ObjectKind::Buffer => 1,
            ObjectKind::Image => 2,
            ObjectKind::Sampler => 3,
            ObjectKind::Shader => 4,
            ObjectKind::Layout => 5,
            ObjectKind::Pipeline => 6,
        };
        (self.table.destroy)(self.context, kind, id);
    }

    fn submit(&mut self, list: &[Command]) -> backend::Result<Work> {
        let mut writes = Vec::new();
        let converted = commands(list, &mut writes);
        let mut work = 0;
        let code = (self.table.submit)(self.context, converted.as_ptr(), converted.len() as u32, &mut work);
        drop(writes);
        if code != 0 { Err(error(code)) } else { Ok(Work(work)) }
    }

    fn poll(&mut self, work: Work) -> WorkState {
        match (self.table.poll)(self.context, work.0) {
            0 => WorkState::Pending,
            1 => WorkState::Done,
            _ => WorkState::Failed,
        }
    }

    fn export_image(&mut self, image: Id) -> backend::Result<u32> {
        let mut name = 0;
        let code = (self.table.export_image)(self.context, image, &mut name);
        if code != 0 { Err(error(code)) } else { Ok(name) }
    }

    fn import_image(&mut self, shared: u32, desc: &ImageDesc) -> backend::Result<Id> {
        let d = image_desc(desc);
        let mut id = 0;
        let code = (self.table.import_image)(self.context, shared, &d, &mut id);
        if code != 0 { Err(error(code)) } else { Ok(id) }
    }

    fn close(&mut self) {
        (self.table.close)(self.context);
    }
}

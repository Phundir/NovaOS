//! Objects, fences and sharing: what a program creates in a context, the
//! fences it waits on, and the images it hands to another process.

use super::*;

/// Object descriptions as the core receives them.
pub enum Create<'a> {
    Buffer { size: u64, usage: u32, memory: u32, offset: u64 },
    Image { desc: ImageDesc, memory: u32, offset: u64 },
    Sampler(backend::SamplerDesc),
    Shader { code: &'a [u32], stage: spirv::Stage },
    Layout(backend::LayoutDesc),
    Pipeline(PipelineRequest),
}

/// A pipeline, by handles.
pub struct PipelineRequest {
    pub layout: u32,
    pub vertex: u32,
    pub fragment: u32,
    pub desc: backend::PipelineDesc,
}

/// `SYS_GPU_OBJECT_CREATE`.
pub fn object_create(owner: &Owner, context: u32, create: Create) -> Result<u32> {
    let shared = context_of(owner, context)?;
    let mut ctx = shared.state.lock();
    if ctx.closed || shared.is_lost() {
        return Err(Error::Lost);
    }
    let description = description().ok_or(Error::Absent)?;
    let (kind, backend_id, info, memory_handle, own_bytes) = match create {
        Create::Buffer { size, usage, memory, offset } => {
            if size == 0 || usage & !backend::buffer_usage::ALL != 0 {
                return Err(Error::Invalid("tampon vide ou usage inconnu"));
            }
            check_handle(owner, memory, HandleKind::Memory, context)?;
            let mem = ctx.memory.get(&memory).ok_or(Error::BadHandle)?;
            if offset % description.buffer_alignment != 0 || offset.checked_add(size).is_none_or(|end| end > mem.size) {
                return Err(Error::Invalid("tampon hors de sa memoire ou mal aligne"));
            }
            let mem_id = mem.backend;
            let id = ctx.backend.create_buffer(&backend::BufferDesc { size, usage }, mem_id, offset)?;
            (ObjectKind::Buffer, id, cmd::Info::Buffer { size, usage }, memory, 0)
        }
        Create::Image { desc, memory, offset } => {
            if desc.width == 0
                || desc.height == 0
                || desc.width > description.max_image_size
                || desc.height > description.max_image_size
                || desc.usage & !backend::image_usage::ALL != 0
                || description.formats & (1 << desc.format as u32) == 0
            {
                return Err(Error::Invalid("image de taille, d'usage ou de format invalide"));
            }
            if (desc.usage & backend::image_usage::DEPTH != 0) != desc.format.is_depth()
                || (desc.format.is_depth() && desc.usage & backend::image_usage::RENDER != 0)
            {
                return Err(Error::Invalid("usage de profondeur et format incoherents"));
            }
            let placed = if memory == 0 {
                None
            } else {
                check_handle(owner, memory, HandleKind::Memory, context)?;
                let mem = ctx.memory.get(&memory).ok_or(Error::BadHandle)?;
                Some((mem.backend, offset))
            };
            if placed.is_none() {
                // Charged before the host allocates, from the pixels: the
                // real size, known after, is charged in addition if larger.
                let estimate = desc.width as u64 * desc.height as u64 * desc.format.bytes_per_pixel();
                charge(owner, estimate)?;
                let result = ctx.backend.create_image(&desc, None);
                match result {
                    Ok((id, bytes)) => {
                        if bytes > estimate && charge(owner, bytes - estimate).is_err() {
                            ctx.backend.destroy(ObjectKind::Image, id);
                            refund(owner, estimate);
                            return Err(Error::Limit);
                        }
                        if bytes < estimate {
                            refund(owner, estimate - bytes);
                        }
                        let info = cmd::Info::Image {
                            format: desc.format,
                            width: desc.width,
                            height: desc.height,
                            usage: desc.usage,
                            initialized: false,
                        };
                        (ObjectKind::Image, id, info, 0, bytes)
                    }
                    Err(error) => {
                        refund(owner, estimate);
                        return Err(error.into());
                    }
                }
            } else {
                let (id, _) = ctx.backend.create_image(&desc, placed)?;
                let info = cmd::Info::Image {
                    format: desc.format,
                    width: desc.width,
                    height: desc.height,
                    usage: desc.usage,
                    initialized: false,
                };
                (ObjectKind::Image, id, info, memory, 0)
            }
        }
        Create::Sampler(desc) => {
            if desc.address > 2 {
                return Err(Error::Invalid("mode d'adressage inconnu"));
            }
            let id = ctx.backend.create_sampler(&desc)?;
            (ObjectKind::Sampler, id, cmd::Info::Sampler, 0, 0)
        }
        Create::Shader { code, stage } => {
            spirv::validate(code, stage).map_err(Error::Invalid)?;
            let id = ctx.backend.create_shader(code)?;
            let _ = stage;
            (ObjectKind::Shader, id, cmd::Info::Shader, 0, 0)
        }
        Create::Layout(desc) => {
            if desc.bindings.len() > 16 || desc.push_constants > description.max_push_constants || desc.push_constants % 4 != 0 {
                return Err(Error::Invalid("disposition trop grande"));
            }
            for (index, binding) in desc.bindings.iter().enumerate() {
                if binding.stages == 0 || binding.stages & !backend::stage::ALL != 0 || binding.binding >= 32 {
                    return Err(Error::Invalid("liaison invalide"));
                }
                if desc.bindings[..index].iter().any(|b| b.binding == binding.binding) {
                    return Err(Error::Invalid("liaison en double"));
                }
            }
            let id = ctx.backend.create_layout(&desc)?;
            let info = cmd::Info::Layout { bindings: desc.bindings.clone(), push_constants: desc.push_constants };
            (ObjectKind::Layout, id, info, 0, 0)
        }
        Create::Pipeline(mut request) => {
            let layout = ctx.objects.get(&request.layout).ok_or(Error::BadHandle)?;
            if !matches!(layout.info, cmd::Info::Layout { .. }) {
                return Err(Error::BadHandle);
            }
            let layout_id = layout.backend;
            let vertex = ctx.objects.get(&request.vertex).ok_or(Error::BadHandle)?;
            let fragment = ctx.objects.get(&request.fragment).ok_or(Error::BadHandle)?;
            if !matches!(vertex.info, cmd::Info::Shader) || !matches!(fragment.info, cmd::Info::Shader) {
                return Err(Error::BadHandle);
            }
            request.desc.layout = layout_id;
            request.desc.vertex = vertex.backend;
            request.desc.fragment = fragment.backend;
            let desc = &request.desc;
            if desc.bindings.len() > cmd::VERTEX_SLOTS || desc.attributes.len() > 8 {
                return Err(Error::Invalid("trop de liaisons ou d'attributs de sommets"));
            }
            for attribute in &desc.attributes {
                let binding = desc.bindings.get(attribute.binding as usize).ok_or(Error::Invalid("attribut sans liaison"))?;
                if attribute.location >= 16 || attribute.offset.saturating_add(attribute.format.size()) > binding.stride.max(attribute.offset + attribute.format.size()) {
                    return Err(Error::Invalid("attribut de sommet invalide"));
                }
            }
            if desc.color_format.is_depth() || desc.depth_format.is_some_and(|f| !f.is_depth()) {
                return Err(Error::Invalid("formats du pipeline incoherents"));
            }
            let id = ctx.backend.create_pipeline(desc)?;
            let info = cmd::Info::Pipeline {
                layout: request.layout,
                color: desc.color_format,
                depth: desc.depth_format,
                vertex_bindings: desc.bindings.len() as u32,
            };
            (ObjectKind::Pipeline, id, info, 0, 0)
        }
    };
    let handle = match new_handle(owner, HandleKind::Object, context) {
        Ok(handle) => handle,
        Err(error) => {
            ctx.backend.destroy(kind, backend_id);
            if own_bytes != 0 {
                refund(owner, own_bytes);
            }
            return Err(error);
        }
    };
    if memory_handle != 0
        && let Some(memory) = ctx.memory.get_mut(&memory_handle)
    {
        memory.users += 1;
    }
    ctx.objects.insert(handle, cmd::Object { backend: backend_id, info });
    ctx.extras.insert(handle, ObjectExtra { kind, memory: memory_handle, own_bytes });
    Ok(handle)
}

/// `SYS_GPU_OBJECT_DESTROY`: objects and fences.
pub fn object_destroy(owner: &Owner, context: u32, handle: u32) -> Result<()> {
    let shared = context_of(owner, context)?;
    if check_handle(owner, handle, HandleKind::Fence, context).is_ok() {
        shared.state.lock().fences.remove(&handle);
        drop_handle(handle);
        return Ok(());
    }
    check_handle(owner, handle, HandleKind::Object, context)?;
    let mut ctx = shared.state.lock();
    // A layout or shader a pipeline still names stays: the pipeline holds
    // it in the backend, but the validation of a later draw reads it.
    if ctx.objects.values().any(|o| matches!(o.info, cmd::Info::Pipeline { layout, .. } if layout == handle)) {
        return Err(Error::Invalid("disposition encore utilisee par un pipeline"));
    }
    let object = ctx.objects.remove(&handle).ok_or(Error::BadHandle)?;
    let extra = ctx.extras.remove(&handle).ok_or(Error::BadHandle)?;
    drop_handle(handle);
    if extra.memory != 0
        && let Some(memory) = ctx.memory.get_mut(&extra.memory)
    {
        memory.users = memory.users.saturating_sub(1);
    }
    defer(&shared, &mut ctx, Deferred::Object(extra.kind, object.backend));
    drop(ctx);
    if extra.own_bytes != 0 {
        refund(owner, extra.own_bytes);
    }
    TABLES.lock().shares.retain(|_, (_, _, image)| *image != handle);
    Ok(())
}

/// `SYS_GPU_FENCE_CREATE`: a fence already signalled.
pub fn fence_create(owner: &Owner, context: u32) -> Result<u32> {
    let shared = context_of(owner, context)?;
    // A lost context refuses everything, this too (spec 6.8).
    if shared.is_lost() {
        return Err(Error::Lost);
    }
    let handle = new_handle(owner, HandleKind::Fence, context)?;
    shared.state.lock().fences.insert(handle, Fence { target: 0 });
    Ok(handle)
}

/// A fence, as waiters see it: a context and a submission number.
#[derive(Clone)]
pub struct FenceView {
    pub context: Arc<Shared>,
    pub target: u64,
}

impl FenceView {
    pub fn signalled(&self) -> bool {
        self.context.reached(self.target)
    }
}

/// The current state of the caller's fence `handle`.
pub fn fence_view(owner: &Owner, handle: u32) -> Result<FenceView> {
    let context = {
        let tables = TABLES.lock();
        match tables.handles.get(&handle) {
            Some((key, HandleKind::Fence, context)) if *key == owner.key() => *context,
            _ => return Err(Error::BadHandle),
        }
    };
    let shared = context_of(owner, context)?;
    let target = shared.state.lock().fences.get(&handle).map(|f| f.target).ok_or(Error::BadHandle)?;
    Ok(FenceView { context: shared, target })
}

/// How long a wait watches the GPU before it sleeps: the work of a frame
/// is done in a fraction of a millisecond, a sleep lasts a tick (10 ms).
const SPIN_NS: u64 = 3_000_000;

/// `SYS_GPU_FENCE_WAIT`: until all (or one) of `fences` signal, at most
/// `timeout_ms` (`None`: no limit).
pub fn fence_wait(fences: &[FenceView], all: bool, timeout_ms: Option<u64>) -> Result<()> {
    let deadline = timeout_ms.map(|ms| crate::idt::fine_ticks() + ms);
    let ready = || {
        let done = if all { fences.iter().all(|f| f.signalled()) } else { fences.iter().any(|f| f.signalled()) };
        done.then_some(())
    };
    if ready().is_some() {
        return Ok(());
    }
    if timeout_ms == Some(0) {
        return Err(Error::TimedOut);
    }
    // First a short watch, polling the backend between two yields: a
    // frame's work ends well within it, and a sleep lasts a whole tick.
    let started = crate::posix::time::monotonic_ns();
    while crate::posix::time::monotonic_ns() - started < SPIN_NS {
        if service_contexts() {
            EVENTS.notify_all();
        }
        if ready().is_some() {
            return Ok(());
        }
        if deadline.is_some_and(|d| crate::idt::fine_ticks() >= d) {
            return Err(Error::TimedOut);
        }
        crate::scheduler::yield_now();
    }
    match crate::posix::wait::wait_until(&[&EVENTS], deadline, ready) {
        Ok(()) => Ok(()),
        Err(crate::posix::wait::WaitError::TimedOut) => Err(Error::TimedOut),
        Err(crate::posix::wait::WaitError::Interrupted) => Err(Error::Interrupted),
    }
}

/// `SYS_GPU_SHARE`: a token another process imports the image by.
pub fn share(owner: &Owner, context: u32, image: u32) -> Result<u64> {
    check_handle(owner, image, HandleKind::Object, context)?;
    let shared = context_of(owner, context)?;
    let mut ctx = shared.state.lock();
    let object = ctx.objects.get(&image).ok_or(Error::BadHandle)?;
    let cmd::Info::Image { format, width, height, usage, .. } = object.info else {
        return Err(Error::BadHandle);
    };
    let id = object.backend;
    let name = ctx.backend.export_image(id)?;
    let desc = ImageDesc { format, width, height, usage, shareable: true };
    let mut token = [0u8; 8];
    crate::random::fill(&mut token);
    let token = u64::from_le_bytes(token) | 1;
    TABLES.lock().shares.insert(token, (name, desc, context));
    let _ = image;
    Ok(token)
}

/// `SYS_GPU_IMPORT`: the image a token names, in the caller's context.
pub fn import(owner: &Owner, context: u32, token: u64) -> Result<u32> {
    let (name, desc, source) = TABLES.lock().shares.get(&token).copied().ok_or(Error::BadHandle)?;
    if source == context {
        return Err(Error::Invalid("import dans le contexte d'origine"));
    }
    let shared = context_of(owner, context)?;
    let mut ctx = shared.state.lock();
    let id = ctx.backend.import_image(name, &desc)?;
    let handle = match new_handle(owner, HandleKind::Object, context) {
        Ok(handle) => handle,
        Err(error) => {
            ctx.backend.destroy(ObjectKind::Image, id);
            return Err(error);
        }
    };
    let info = cmd::Info::Image { format: desc.format, width: desc.width, height: desc.height, usage: desc.usage, initialized: true };
    ctx.objects.insert(handle, cmd::Object { backend: id, info });
    ctx.extras.insert(handle, ObjectExtra { kind: ObjectKind::Image, memory: 0, own_bytes: 0 });
    Ok(handle)
}

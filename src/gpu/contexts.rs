//! Contexts and their memory: opening and closing a context, allocating,
//! mapping and freeing GPU memory, and the deferred destruction that keeps
//! the GPU from reading what a program already gave back.

use super::*;

// ============================================================
//  Calls (spec 6.8), in kernel terms
// ============================================================

/// `SYS_GPU_CONTEXT_CREATE`.
pub fn context_create(owner: &Owner, name: &str) -> Result<u32> {
    let device = device().ok_or(Error::Absent)?;
    {
        let tables = TABLES.lock();
        let held = tables.contexts.values().filter(|c| c.owner.key() == owner.key()).count();
        if held >= MAX_CONTEXTS {
            return Err(Error::Limit);
        }
    }
    let backend = device.open(name)?;
    let handle = match new_handle(owner, HandleKind::Context, 0) {
        Ok(handle) => handle,
        Err(error) => {
            let mut backend = backend;
            backend.close();
            return Err(error);
        }
    };
    let shared = Arc::new(Shared {
        handle,
        owner: owner.clone(),
        name: String::from(name),
        submitted: AtomicU64::new(0),
        completed: AtomicU64::new(0),
        lost: AtomicBool::new(false),
        state: SleepLock::new(Ctx {
            backend,
            objects: BTreeMap::new(),
            extras: BTreeMap::new(),
            memory: BTreeMap::new(),
            fences: BTreeMap::new(),
            in_flight: VecDeque::new(),
            held: VecDeque::new(),
            deferred: Vec::new(),
            closed: false,
            present: None,
        }),
    });
    TABLES.lock().contexts.insert(handle, shared);
    Ok(handle)
}

/// `SYS_GPU_CONTEXT_DESTROY`: waits for the context's work, then closes it.
pub fn context_destroy(owner: &Owner, handle: u32) -> Result<()> {
    let shared = context_of(owner, handle)?;
    close_context(&shared);
    Ok(())
}

/// Ends a context: every handle in it goes, its memory is unmapped from
/// its owner and refunded, and the backend context is closed -- on the
/// host, the device and everything in it.
pub(super) fn close_context(shared: &Arc<Shared>) {
    TABLES.lock().contexts.remove(&shared.handle);
    // Work in flight is let finish, bounded by the watchdog: closing the
    // host context under a running submission would be the host's problem,
    // but its memory is what the next context gets.
    let deadline = crate::idt::fine_ticks() + WATCHDOG_MS;
    let _ = crate::posix::wait::wait_until(&[&EVENTS], Some(deadline), || {
        (shared.reached(shared.submitted.load(Ordering::Acquire))).then_some(())
    });
    let mut ctx = shared.state.lock();
    if ctx.closed {
        return;
    }
    ctx.closed = true;
    let mut refund_bytes = 0;
    let space = shared.owner.space();
    for (_, memory) in core::mem::take(&mut ctx.memory) {
        if let (Some((pager, _)), Some(space)) = (memory.user.as_ref(), space.as_ref()) {
            space.unmap_pager(pager);
        }
        refund_bytes += memory.size;
    }
    for (_, extra) in core::mem::take(&mut ctx.extras) {
        refund_bytes += extra.own_bytes;
    }
    ctx.backend.close();
    shared.lost.store(true, Ordering::Release);
    drop(ctx);
    refund(&shared.owner, refund_bytes);
    let mut tables = TABLES.lock();
    tables.handles.retain(|handle, (_, _, context)| *context != shared.handle && *handle != shared.handle);
    tables.shares.retain(|_, (_, _, context)| *context != shared.handle);
    drop(tables);
    EVENTS.notify_all();
}

/// `SYS_GPU_MEM_ALLOC`.
pub fn mem_alloc(owner: &Owner, context: u32, size: u64, kind: MemKind) -> Result<u32> {
    if size == 0 || size > QUOTA {
        return Err(Error::Invalid("taille de memoire nulle ou hors quota"));
    }
    let shared = context_of(owner, context)?;
    let rounded = (size + 4095) & !4095;
    charge(owner, rounded)?;
    let mut ctx = shared.state.lock();
    if ctx.closed || shared.is_lost() {
        drop(ctx);
        refund(owner, rounded);
        return Err(Error::Lost);
    }
    let (id, real) = match ctx.backend.alloc(size, kind) {
        Ok(result) => result,
        Err(error) => {
            drop(ctx);
            refund(owner, rounded);
            return Err(error.into());
        }
    };
    // The real size is what is held: charged up or refunded down.
    if real > rounded && charge(owner, real - rounded).is_err() {
        ctx.backend.free(id);
        drop(ctx);
        refund(owner, rounded);
        return Err(Error::Limit);
    }
    if real < rounded {
        refund(owner, rounded - real);
    }
    let handle = match new_handle(owner, HandleKind::Memory, context) {
        Ok(handle) => handle,
        Err(error) => {
            ctx.backend.free(id);
            drop(ctx);
            refund(owner, real);
            return Err(error);
        }
    };
    ctx.memory.insert(handle, Memory { backend: id, size: real, kind, user: None, kernel: None, window: None, users: 0 });
    Ok(handle)
}

/// `SYS_GPU_MEM_FREE`.
pub fn mem_free(owner: &Owner, context: u32, handle: u32) -> Result<()> {
    check_handle(owner, handle, HandleKind::Memory, context)?;
    let shared = context_of(owner, context)?;
    let mut ctx = shared.state.lock();
    let memory = ctx.memory.get(&handle).ok_or(Error::BadHandle)?;
    if memory.users != 0 {
        return Err(Error::Invalid("memoire encore occupee par des objets"));
    }
    let memory = ctx.memory.remove(&handle).ok_or(Error::BadHandle)?;
    drop_handle(handle);
    unmap_views(&shared, &mut ctx, &memory);
    let size = memory.size;
    defer(&shared, &mut ctx, Deferred::Memory(memory.backend));
    drop(ctx);
    refund(owner, size);
    Ok(())
}

/// Takes the CPU views of a memory away: from the program, from the
/// kernel window.
pub(super) fn unmap_views(shared: &Shared, ctx: &mut Ctx, memory: &Memory) {
    if let Some((pager, _)) = memory.user.as_ref()
        && let Some(space) = shared.owner.space()
    {
        space.unmap_pager(pager);
    }
    if let (Some(address), Some(window)) = (memory.kernel, memory.window) {
        for page in 0..window.size / 4096 {
            unsafe { crate::memory::unmap_page(address + page * 4096) };
        }
    }
    if memory.window.is_some() {
        ctx.backend.unmap(memory.backend);
    }
}

/// Destroys now if nothing is in flight, else once the work in flight
/// at this moment has completed.
pub(super) fn defer(shared: &Shared, ctx: &mut Ctx, what: Deferred) {
    let serial = shared.submitted.load(Ordering::Acquire);
    if shared.completed() >= serial {
        run_deferred(ctx, what);
    } else {
        ctx.deferred.push((serial, what));
    }
}

pub(super) fn run_deferred(ctx: &mut Ctx, what: Deferred) {
    match what {
        Deferred::Object(kind, id) => ctx.backend.destroy(kind, id),
        Deferred::Memory(id) => ctx.backend.free(id),
    }
}

/// The pages of a GPU memory, lent to a program's address space. Its
/// regions are removed (`Space::unmap_pager`) before the memory goes, so
/// no fault can reach the window after that.
struct GpuPager {
    window: backend::Window,
}

impl crate::vm::Pager for GpuPager {
    fn fill(&self, _offset: u64, _page: &mut [u8]) -> bool {
        false
    }
    fn shared_frame(&self, offset: u64) -> Option<u64> {
        (offset < self.window.size).then_some(self.window.phys + offset)
    }
    fn page_flags(&self) -> u64 {
        self.window.cache_flags
    }
    fn fork_shares(&self) -> bool {
        false
    }
}

/// `SYS_GPU_MEM_MAP`: returns the address of the memory in the caller's
/// space (the kernel window, for a kernel caller without a space).
pub fn mem_map(owner: &Owner, context: u32, handle: u32) -> Result<u64> {
    check_handle(owner, handle, HandleKind::Memory, context)?;
    let shared = context_of(owner, context)?;
    let mut ctx = shared.state.lock();
    let backend_id = {
        let memory = ctx.memory.get(&handle).ok_or(Error::BadHandle)?;
        if memory.kind == MemKind::Local {
            return Err(Error::Invalid("memoire locale au GPU, non projetable"));
        }
        if let Some((_, address)) = memory.user {
            return Ok(address);
        }
        if let Some(address) = memory.kernel {
            return Ok(address);
        }
        memory.backend
    };
    let window = ctx.backend.map(backend_id)?;
    let space = shared.owner.space();
    let result = match space {
        Some(space) => {
            let dyn_pager: Arc<dyn crate::vm::Pager> = Arc::new(GpuPager { window });
            let prot = crate::vm::Prot::RW;
            match space.map(0, window.size, prot, crate::vm::Backing::File { pager: dyn_pager.clone(), offset: 0, shared: true }, false, "[gpu]") {
                Ok(address) => {
                    let memory = ctx.memory.get_mut(&handle).ok_or(Error::BadHandle)?;
                    memory.user = Some((dyn_pager, address));
                    memory.window = Some(window);
                    Ok(address)
                }
                Err(_) => Err(Error::NoMemory),
            }
        }
        None => match kernel_map(window) {
            Some(address) => {
                let memory = ctx.memory.get_mut(&handle).ok_or(Error::BadHandle)?;
                memory.kernel = Some(address);
                memory.window = Some(window);
                Ok(address)
            }
            None => Err(Error::NoMemory),
        },
    };
    if result.is_err() {
        ctx.backend.unmap(backend_id);
    }
    result
}

/// Where a mapped memory shows in physical memory -- mapped first if it
/// is not yet. For the core's own users (`screen`), which hand the device
/// the range itself.
pub(super) fn mem_window(owner: &Owner, context: u32, handle: u32) -> Result<backend::Window> {
    mem_map(owner, context, handle)?;
    let shared = context_of(owner, context)?;
    let ctx = shared.state.lock();
    let memory = ctx.memory.get(&handle).ok_or(Error::BadHandle)?;
    memory.window.ok_or(Error::Invalid("memoire non projetee"))
}

/// The kernel's view of a backend window: the hostmem window of the
/// virtio backend when the window lies in its region, which it always
/// does for the built-in backend.
pub(super) fn kernel_map(window: backend::Window) -> Option<u64> {
    let region = crate::graphics::virtio_gpu::context::region()?;
    let offset = window.phys.checked_sub(region.0)?;
    if offset + window.size > region.1 {
        return None;
    }
    let cache = crate::graphics::virtio_gpu::context::cache_of_flags(window.cache_flags);
    // SAFETY: the backend maps this window for as long as the memory is
    // mapped; the kernel view goes first in `unmap_views`.
    unsafe { crate::graphics::virtio_gpu::hostmem::map_kernel(window.phys, offset, window.size, cache) }
}

/// `SYS_GPU_MEM_UNMAP`.
pub fn mem_unmap(owner: &Owner, context: u32, handle: u32) -> Result<()> {
    check_handle(owner, handle, HandleKind::Memory, context)?;
    let shared = context_of(owner, context)?;
    let mut ctx = shared.state.lock();
    let memory = ctx.memory.get_mut(&handle).ok_or(Error::BadHandle)?;
    let taken = Memory {
        backend: memory.backend,
        size: memory.size,
        kind: memory.kind,
        user: memory.user.take(),
        kernel: memory.kernel.take(),
        window: memory.window.take(),
        users: 0,
    };
    if taken.window.is_none() {
        return Ok(());
    }
    unmap_views(&shared, &mut ctx, &taken);
    Ok(())
}

//! Submissions: validation, launch, submissions held on another context's
//! fence, completion, the watchdog, and the sweep of dead owners' contexts.

use super::*;



/// `SYS_GPU_SUBMIT`: validates `words`, waits for room in the queue, and
/// submits -- or holds the submission until `waits` signal. `signal`, if
/// non-zero, is the fence set to this submission.
pub fn submit(owner: &Owner, context: u32, words: &[u32], signal: u32, waits: &[u32]) -> Result<()> {
    let shared = context_of(owner, context)?;
    if signal != 0 {
        check_handle(owner, signal, HandleKind::Fence, context)?;
    }
    let mut views = Vec::new();
    for wait in waits {
        let view = fence_view(owner, *wait)?;
        if !view.signalled() {
            views.push((view.context, view.target));
        }
    }
    // Room in the queue: waited for without the context lock.
    let deadline = crate::idt::fine_ticks() + WATCHDOG_MS * 2;
    let room = crate::posix::wait::wait_until(&[&EVENTS], Some(deadline), || {
        let in_flight = shared.submitted.load(Ordering::Acquire).saturating_sub(shared.completed());
        (in_flight < MAX_IN_FLIGHT as u64 || shared.is_lost()).then_some(())
    });
    match room {
        Err(crate::posix::wait::WaitError::Interrupted) => return Err(Error::Interrupted),
        Err(crate::posix::wait::WaitError::TimedOut) => return Err(Error::TimedOut),
        Ok(()) => {}
    }
    let mut ctx = shared.state.lock();
    if ctx.closed || shared.is_lost() {
        return Err(Error::Lost);
    }
    let (commands, first_use) = match cmd::translate(words, &ctx.objects) {
        Ok(result) => result,
        Err(reason) => {
            REFUSED.fetch_add(1, Ordering::Relaxed);
            return Err(Error::Invalid(reason));
        }
    };
    for handle in first_use {
        if let Some(cmd::Object { info: cmd::Info::Image { initialized, .. }, .. }) = ctx.objects.get_mut(&handle) {
            *initialized = true;
        }
    }
    let serial = shared.submitted.load(Ordering::Acquire) + 1;
    shared.submitted.store(serial, Ordering::Release);
    if signal != 0
        && let Some(fence) = ctx.fences.get_mut(&signal)
    {
        fence.target = serial;
    }
    if !views.is_empty() || !ctx.held.is_empty() {
        ctx.held.push_back(Held { serial, commands, waits: views });
        drop(ctx);
        release_held(&shared);
        return Ok(());
    }
    launch(&shared, &mut ctx, serial, &commands)
}

/// Hands a submission to the backend. A refusal loses the context: its
/// numbering can no longer be honoured.
pub(super) fn launch(shared: &Shared, ctx: &mut Ctx, serial: u64, commands: &[backend::Command]) -> Result<()> {
    match ctx.backend.submit(commands) {
        Ok(work) => {
            ctx.in_flight.push_back((serial, work, crate::posix::time::monotonic_ns() / 1_000_000));
            SUBMISSIONS.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        Err(error) => {
            lose(shared, "soumission refusee par le GPU");
            Err(error.into())
        }
    }
}

/// Launches the held submissions whose waits have signalled, in order.
pub(super) fn release_held(shared: &Arc<Shared>) {
    let Some(mut ctx) = shared.state.try_lock() else {
        return;
    };
    while let Some(front) = ctx.held.front() {
        if !front.waits.iter().all(|(context, target)| context.reached(*target)) {
            break;
        }
        let held = ctx.held.pop_front().expect("front vu");
        if launch(shared, &mut ctx, held.serial, &held.commands).is_err() {
            break;
        }
    }
}

/// Marks a context lost: every call on it fails from now on, its fences
/// read as signalled (nothing will ever complete them otherwise).
pub(super) fn lose(shared: &Shared, reason: &str) {
    if !shared.lost.swap(true, Ordering::AcqRel) {
        LOST_CONTEXTS.fetch_add(1, Ordering::Relaxed);
        crate::serial_println!("[gpu] contexte {} \"{}\" perdu - {}", shared.handle, shared.name, reason);
        EVENTS.notify_all();
    }
}

/// Polls every context: completions, deferred destructions, held
/// submissions, the watchdog. Returns whether anything moved.
pub(super) fn service_contexts() -> bool {
    // The watchdog's clock is the TSC, not the timer's ticks: while the
    // host's GPU is held, a core that touches the device waits in QEMU
    // with its timer, and the ticks stand still for seconds (13.11.3).
    let now = crate::posix::time::monotonic_ns() / 1_000_000;
    let contexts: Vec<Arc<Shared>> = TABLES.lock().contexts.values().cloned().collect();
    let mut moved = false;
    for shared in contexts {
        let Some(mut ctx) = shared.state.try_lock() else {
            continue;
        };
        if ctx.closed {
            continue;
        }
        while let Some(&(serial, work, started)) = ctx.in_flight.front() {
            match ctx.backend.poll(work) {
                WorkState::Pending => {

                    if now.saturating_sub(started) > WATCHDOG_MS {
                        lose(&shared, "delai de garde depasse");
                        ctx.in_flight.clear();
                        moved = true;
                    }
                    break;
                }
                WorkState::Done => {
                    ctx.in_flight.pop_front();
                    shared.completed.store(serial, Ordering::Release);
                    moved = true;
                }
                WorkState::Failed => {
                    ctx.in_flight.pop_front();
                    lose(&shared, "le GPU a refuse le travail");
                    moved = true;
                }
            }
        }
        let completed = shared.completed();
        let lost = shared.is_lost();
        if !ctx.deferred.is_empty() {
            let deferred = core::mem::take(&mut ctx.deferred);
            for (serial, what) in deferred {
                if serial <= completed || lost {
                    run_deferred(&mut ctx, what);
                } else {
                    ctx.deferred.push((serial, what));
                }
            }
        }
        drop(ctx);
        release_held(&shared);
    }
    moved
}

/// Closes the contexts of owners that died.
pub(super) fn sweep() {
    let dead: Vec<Arc<Shared>> =
        TABLES.lock().contexts.values().filter(|c| !c.owner.alive()).cloned().collect();
    for shared in dead {
        crate::serial_println!("[gpu] contexte {} \"{}\" ferme - proprietaire disparu", shared.handle, shared.name);
        close_context(&shared);
        TABLES.lock().usage.remove(&shared.owner.key());
    }
}

//! Preemptive multi-core scheduler with fixed priorities (spec 7).
//!
//! The task table is unique and shared, but each core owns its current
//! task and elects only among the tasks its affinity allows (spec 7.2). A
//! task moved to the RUNNING state under the lock is no longer eligible:
//! two cores can therefore never fight over the same task.
//!
//! Round-robin applies inside one priority level, and the
//! anti-starvation mechanism (spec 7.5) prevents a low priority task from
//! being ignored indefinitely on its core.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::fs::perm::{Credentials, Gid, Role, Uid};
use crate::gdt::MAX_CPUS;
use crate::process::{self, BinaryType, Pid, Process, State};
use crate::vga::SpinLock;

/// Anti-starvation threshold: 500 ms with no CPU slice (spec 7.5).
/// Converted into ticks: 100 Hz x 0.5 s = 50 ticks.
const STARVATION_THRESHOLD: u64 = 50;

unsafe extern "C" {
    fn switch_context(old_rsp: *mut u64, new_rsp: u64);
    fn start_first_task(new_rsp: u64) -> !;
}

pub struct Scheduler {
    /// Task table. **`Box` and not `Process` directly**, and that is not
    /// a matter of style.
    ///
    /// `switch` reads `&raw mut sched.tasks[previous].rsp`, **releases
    /// the lock**, then switches -- that is the rule of
    /// `docs/DECISIONS.md`: a lock is never held across
    /// `switch_context`. So the pointer outlives the critical section.
    /// With a `Vec<Process>` it names the vector buffer: let another core
    /// create a task during that window, let the `Vec` reallocate, and
    /// `switch_context` writes the outgoing RSP into freed memory. The
    /// task then resumes on a phantom stack, several switches later, with
    /// nothing to name the cause.
    ///
    /// A `Box` never moves: reallocating the vector moves pointers, not
    /// the structures they name. Field access stays identical thanks to
    /// automatic dereferencing.
    tasks: Vec<Box<Process>>,
    /// Index of the task running on each core.
    current: [Option<usize>; MAX_CPUS],
    /// Does the core take part in scheduling?
    started: [bool; MAX_CPUS],
    next_pid: Pid,
    switches: u64,
    starvation_rescues: u64,
}

impl Scheduler {
    const fn new() -> Self {
        Scheduler {
            tasks: Vec::new(),
            current: [None; MAX_CPUS],
            started: [false; MAX_CPUS],
            next_pid: 1,
            switches: 0,
            starvation_rescues: 0,
        }
    }

    /// Chooses the least loaded core among those the affinity of the
    /// binary type allows.
    fn choose_cpu(&self, binary_type: BinaryType) -> u8 {
        let cores = crate::smp_core_count().min(MAX_CPUS);
        let mut best = 0usize;
        let mut best_load = usize::MAX;

        for cpu in 0..cores {
            if !process::affinity_allows(binary_type, cpu, cores) {
                continue;
            }
            let load = self
                .tasks
                .iter()
                .filter(|t| t.cpu_affinity as usize == cpu && t.state.is_schedulable())
                .count();
            if load < best_load {
                best_load = load;
                best = cpu;
            }
        }

        best as u8
    }

    fn spawn_on(
        &mut self,
        name: &str,
        binary_type: BinaryType,
        entry: fn() -> !,
        cpu: u8,
        state: State,
    ) -> Pid {
        self.spawn_sized(name, binary_type, entry, cpu, state, process::STACK_SIZE)
    }

    fn spawn_sized(
        &mut self,
        name: &str,
        binary_type: BinaryType,
        entry: fn() -> !,
        cpu: u8,
        state: State,
        stack_size: usize,
    ) -> Pid {
        let pid = self.next_pid;
        self.next_pid += 1;
        let mut task = Process::with_stack(pid, name, binary_type, entry, cpu, stack_size);
        task.state = state;

        // Reuse of a freed slot rather than a systematic append.
        //
        // A task that is dead and reaped has neither stack nor image any
        // more: the rest of the system already considers it absent --
        // `exists` and `task_list` filter it out. So it occupied nothing
        // but a table row, which nothing ever gave back. The table grew
        // with every task created since boot, and the scheduler walks it
        // **on every election**: the cost of a switch grew without bound.
        //
        // Reuse rather than compact, because `current[cpu]` names
        // **positions**: removing an entry would shift those of every
        // other core. A reused slot, on the other hand, keeps its index.
        match self
            .tasks
            .iter()
            .position(|slot| slot.state == State::Dead && slot.reaped)
        {
            Some(index) => {
                // The content is replaced, not the `Box`: the existing
                // allocation is reused, and the address of the structure
                // does not change -- which is exactly the property the
                // `Box` is there for.
                *self.tasks[index] = task;
            }
            None => self.tasks.push(Box::new(task)),
        }
        pid
    }

    /// Elects the next runnable task on the given core.
    ///
    /// **A single walk of the table.** The previous version did three:
    /// one for anti-starvation, one to find the best priority, one to
    /// resume the round-robin at that priority. It is the hottest
    /// function of the system -- it runs on every switch, several hundred
    /// times a second -- and its cost grew with the number of tasks times
    /// three.
    ///
    /// The result is identical, and it must be: in one pass in
    /// round-robin order we keep the **first** starving task met and the
    /// **first** of lowest priority. Anti-starvation wins at the end,
    /// exactly as the order of the three loops used to make it.
    fn pick_next(&mut self, cpu: usize, now: u64) -> Option<usize> {
        let count = self.tasks.len();
        if count == 0 {
            return None;
        }

        let start = self.current[cpu].unwrap_or(0);

        // First starving task met in round-robin order (spec 7.5).
        let mut starving: Option<usize> = None;
        // Best ordinary candidate: lowest priority, and at equal priority
        // the first met -- that is the round-robin.
        let mut best: Option<(usize, u8)> = None;

        for offset in 1..=count {
            let index = (start + offset) % count;
            let task = &self.tasks[index];

            if task.state != State::Ready || task.cpu_affinity as usize != cpu {
                continue;
            }

            // The idle task is never "starving": it has nothing to do,
            // and rescuing it took a tick from a working task every half
            // second.
            if starving.is_none()
                && task.binary_type != BinaryType::Idle
                && now.saturating_sub(task.last_scheduled) >= STARVATION_THRESHOLD
            {
                starving = Some(index);
            }

            let priority = task.priority();
            match best {
                // Strictly lower: at equal priority the first met keeps
                // its place, failing which the round-robin would start
                // again from the same side on every election.
                Some((_, current)) if priority >= current => {}
                _ => best = Some((index, priority)),
            }
        }

        if let Some(index) = starving {
            self.starvation_rescues += 1;
            return Some(index);
        }

        best.map(|(index, _)| index)
    }

    /// Decrements the slice of the current task of a core. Returns true if
    /// a switch is needed.
    fn tick(&mut self, cpu: usize) -> bool {
        if !self.started[cpu] {
            return false;
        }
        let Some(index) = self.current[cpu] else {
            return false;
        };

        let task = &mut self.tasks[index];
        task.ticks_used += 1;

        if task.slice_remaining > 0 {
            task.slice_remaining -= 1;
        }

        task.slice_remaining == 0 || !task.state.is_schedulable()
    }

    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }

    pub fn switch_count(&self) -> u64 {
        self.switches
    }

    pub fn starvation_count(&self) -> u64 {
        self.starvation_rescues
    }
}

pub static SCHEDULER: SpinLock<Scheduler> = SpinLock::new(Scheduler::new());

/// One rescheduling flag per core.
static NEED_RESCHEDULE: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];

static TOTAL_SWITCHES: AtomicU64 = AtomicU64::new(0);

// ============================================================
//  Task creation
// ============================================================

/// Creates a task on the least loaded core compatible with its type.
pub fn spawn(name: &str, binary_type: BinaryType, entry: fn() -> !) -> Pid {
    let (pid, cpu) = {
        let mut sched = SCHEDULER.lock();
        let cpu = sched.choose_cpu(binary_type);
        (
            sched.spawn_on(name, binary_type, entry, cpu, State::Ready),
            cpu,
        )
    };
    crate::ipc::register(pid);
    crate::serial_println!(
        "[sched] tache {} \"{}\" ({}) assignee au coeur {} - {}",
        pid,
        name,
        binary_type.as_str(),
        cpu,
        process::affinity_label(binary_type, crate::smp_core_count())
    );
    pid
}

/// Creates a task with a stack of `stack_size` bytes rather than
/// `process::STACK_SIZE`, on the least loaded compatible core.
pub fn spawn_with_stack(name: &str, binary_type: BinaryType, entry: fn() -> !, stack_size: usize) -> Pid {
    let (pid, cpu) = {
        let mut sched = SCHEDULER.lock();
        let cpu = sched.choose_cpu(binary_type);
        (
            sched.spawn_sized(name, binary_type, entry, cpu, State::Ready, stack_size),
            cpu,
        )
    };
    crate::ipc::register(pid);
    crate::serial_println!(
        "[sched] tache {} \"{}\" ({}) assignee au coeur {} - pile de {} Kio",
        pid,
        name,
        binary_type.as_str(),
        cpu,
        stack_size.max(process::STACK_SIZE) / 1024
    );
    pid
}

/// Creates a task pinned to a precise core, with no regard to affinity.
/// Reserved to the idle tasks, one per core.
pub fn spawn_pinned(name: &str, binary_type: BinaryType, entry: fn() -> !, cpu: u8) -> Pid {
    let pid = SCHEDULER
        .lock()
        .spawn_on(name, binary_type, entry, cpu, State::Ready);
    crate::ipc::register(pid);
    pid
}

/// Creates a **suspended** task, ready to be woken by `resume`.
///
/// The binary loader (3.2) uses it to publish the ring 3 entry point of
/// the image before the task can be elected: without that, another core
/// could start it before its parameters are laid down.
///
/// Returns the PID and the core kept by the affinity (spec 7.2).
pub fn spawn_dormant(name: &str, binary_type: BinaryType, entry: fn() -> !) -> (Pid, u8) {
    let (pid, cpu) = {
        let mut sched = SCHEDULER.lock();
        let cpu = sched.choose_cpu(binary_type);
        (
            sched.spawn_on(name, binary_type, entry, cpu, State::Suspended),
            cpu,
        )
    };
    crate::ipc::register(pid);
    (pid, cpu)
}

/// Makes a suspended task eligible (basis of SYS_PROC_RESUME).
pub fn resume(pid: Pid) -> bool {
    let target = {
        let mut sched = SCHEDULER.lock();
        let now = crate::idt::ticks();
        match sched.tasks.iter_mut().find(|t| t.pid == pid) {
            Some(task) if task.state == State::Suspended => {
                task.state = State::Ready;
                task.last_scheduled = now;
                Some(task.cpu_affinity as usize)
            }
            _ => None,
        }
    };

    match target {
        Some(cpu) => {
            if cpu != crate::smp::current_cpu_id() {
                crate::smp::send_reschedule(cpu);
            }
            true
        }
        None => false,
    }
}

/// 3.2.4.3 -- Foreground / background switch of a task.
///
/// Only the `.gs` change state automatically (spec 7.1): their slice
/// falls from 20 to 2 ticks, that is the 2 % of CPU announced by spec
/// 3.4. The new slice only takes effect at the next election, the current
/// slice not being truncated.
pub fn set_foreground(pid: Pid, foreground: bool) -> bool {
    let mut sched = SCHEDULER.lock();
    match sched.tasks.iter_mut().find(|t| t.pid == pid) {
        Some(task) => {
            task.foreground = foreground;
            if task.slice_remaining > task.binary_type.time_slice(foreground) {
                task.slice_remaining = task.binary_type.time_slice(foreground);
            }
            true
        }
        None => false,
    }
}

/// Binary type and current slice of a task -- diagnostic 3.4.
pub fn task_profile(pid: Pid) -> Option<(BinaryType, u8, u8, u32, bool)> {
    let sched = SCHEDULER.lock();
    sched.tasks.iter().find(|t| t.pid == pid).map(|t| {
        (
            t.binary_type,
            t.priority(),
            t.cpu_affinity,
            t.binary_type.time_slice(t.foreground),
            t.foreground,
        )
    })
}

// ============================================================
//  Queries
// ============================================================

/// PID of the task running on the calling core.
pub fn current_pid() -> Pid {
    CURRENT_PID[crate::smp::current_cpu_id()].load(Ordering::Relaxed) as Pid
}

/// PID of the task running on each core, kept beside `current` by the
/// switch itself. `current_pid` took the scheduler lock -- the one lock
/// of every election, masking interrupts on whichever core held it -- and
/// it runs in every wait, every Nova system call, every audio write: on
/// eight cores it was a queue in front of that lock. Only this core
/// changes its own entry, at a switch, so reading it here needs no lock.
static CURRENT_PID: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// Changes the state of a task named by its PID.
pub fn set_state(pid: Pid, state: State) -> bool {
    let mut sched = SCHEDULER.lock();
    if let Some(task) = sched.tasks.iter_mut().find(|t| t.pid == pid) {
        task.state = state;
        true
    } else {
        false
    }
}

// ============================================================
//  Preemption
// ============================================================

/// Called from the timer handler of each core.
pub fn on_timer_tick(cpu: usize) {
    let need = {
        let mut sched = SCHEDULER.lock();
        if !sched.started[cpu] {
            return;
        }
        sched.tick(cpu)
    };

    if need {
        NEED_RESCHEDULE[cpu].store(true, Ordering::Relaxed);
    }
}

/// Called from the rescheduling IPI handler.
pub fn on_reschedule_ipi(cpu: usize) {
    NEED_RESCHEDULE[cpu].store(true, Ordering::Relaxed);
}

/// Is the current core asked to elect again? Read without consuming it:
/// what the idle loop watches while it polls (`lib::task_idle`).
pub fn reschedule_pending() -> bool {
    NEED_RESCHEDULE[crate::smp::current_cpu_id()].load(Ordering::Relaxed)
}

/// Times each core's idle task was elected: the idle loop polls only
/// after coming back from real work, not after every timer tick of a core
/// with nothing to do.
static IDLE_ELECTIONS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

pub fn idle_elections() -> u64 {
    IDLE_ELECTIONS[crate::smp::current_cpu_id()].load(Ordering::Relaxed)
}

/// Performs the switch if the flag of the current core is raised.
pub fn preempt_if_needed() {
    let cpu = crate::smp::current_cpu_id();
    if NEED_RESCHEDULE[cpu].swap(false, Ordering::Relaxed) {
        yield_now();
    }
}

/// Declares the current task asleep **without** making it yield the CPU.
///
/// To be called **before** testing the condition that must wake it. It is
/// the order that matters, and it is not intuitive: setting the state
/// after the test leaves a window during which another core can satisfy
/// the condition, call `wake` on a still-RUNNING task -- hence with no
/// effect at all -- then see that task go to sleep for good when it had
/// what it needed to continue.
///
/// Masking interrupts does not close that window: it is opened by
/// parallelism, not by preemption. Two cores are enough, and the symptom
/// is a sleeping task that only wakes on the next message -- or never.
pub fn arm_sleep() {
    let cpu = crate::smp::current_cpu_id();
    let mut sched = SCHEDULER.lock();
    if !sched.started[cpu] {
        return;
    }
    if let Some(index) = sched.current[cpu] {
        sched.tasks[index].state = State::Sleeping;
    }
}

/// Cancels an armed sleep: the condition was already satisfied.
pub fn cancel_sleep() {
    let cpu = crate::smp::current_cpu_id();
    let mut sched = SCHEDULER.lock();
    if let Some(index) = sched.current[cpu] {
        if sched.tasks[index].state == State::Sleeping {
            sched.tasks[index].state = State::Running;
        }
    }
}

/// Puts the current task to sleep and yields the CPU.
///
/// Reserved to the cases **with no condition to retest**: everywhere a
/// wake-up can happen concurrently, `arm_sleep` then `yield_now` is what
/// must be used.
pub fn block_current() {
    let cpu = crate::smp::current_cpu_id();
    {
        let mut sched = SCHEDULER.lock();
        if !sched.started[cpu] {
            return;
        }
        if let Some(index) = sched.current[cpu] {
            sched.tasks[index].state = State::Sleeping;
        }
    }
    yield_now();
}

/// Wakes a sleeping task, wherever it is.
///
/// If it belongs to another core, an IPI forces the latter to reconsider
/// its election without waiting for its next tick -- failing which an
/// idle core could ignore the wake-up for 10 ms.
///
/// If it belongs to **this** core, the same thing is asked of this core:
/// its rescheduling flag is raised, and the next preemption point (the
/// end of the waker's system call, the idle loop, an interrupt taken in
/// ring 3) elects again. The IPI was the only path, and it skipped the
/// local case: a thread woken by a neighbour of its own core -- a futex,
/// a pipe, a socket between two threads of the browser -- waited for the
/// waker's slice to run out, up to 100 ms for a `.cus`; a driver woken by
/// an interrupt on an idle core 0 waited for the next tick. Measured
/// before the change: 30 to 40 wake-ups over 20 ms every five seconds
/// while a video played, the longest 100 ms -- one whole slice.
pub fn wake(pid: Pid) {
    let target = {
        let mut sched = SCHEDULER.lock();
        let now = crate::idt::ticks();

        match sched.tasks.iter_mut().find(|t| t.pid == pid) {
            Some(task) if task.state == State::Sleeping => {
                task.state = State::Ready;
                // A task coming out of sleep is not starving: without
                // this reset it would trigger a rescue.
                task.last_scheduled = now;
                if task.linux.is_some() {
                    task.woken_ns = crate::posix::time::monotonic_ns();
                }
                Some(task.cpu_affinity as usize)
            }
            _ => None,
        }
    };

    if let Some(cpu) = target {
        if cpu != crate::smp::current_cpu_id() {
            crate::smp::send_reschedule(cpu);
        } else {
            NEED_RESCHEDULE[cpu].store(true, Ordering::Relaxed);
        }
    }
}

/// Ends the current task and switches away for good.
///
/// The task goes DEAD, so `pick_next` stops considering it: the
/// `switch_context` that follows will never come back. Its stack stays
/// allocated -- cleaning up dead tasks is the business of SYS_PROC_WAIT.
pub fn exit_current() -> ! {
    let cpu = crate::smp::current_cpu_id();
    let mut pid = 0;
    {
        let mut sched = SCHEDULER.lock();
        if let Some(index) = sched.current[cpu] {
            sched.tasks[index].state = State::Dead;
            pid = sched.tasks[index].pid;
            crate::serial_println!(
                "[sched] tache {} terminee sur le coeur {} apres {} tics",
                sched.tasks[index].pid,
                cpu,
                sched.tasks[index].ticks_used
            );
        }
    }

    // The mailbox disappears with the task: without that, a send to a
    // dead PID would succeed indefinitely and the message would be lost
    // with no word to the sender.
    crate::ipc::unregister(pid);

    yield_now();

    // Unreachable in theory: the idle task of the core takes over.
    crate::halt_loop()
}

/// Wake-up latency of the Linux threads: from `wake` to the switch that
/// runs the thread.
///
/// A browser is a chain of threads handing work to one another -- input
/// thread, main thread, compositor, content process, socket thread -- and
/// every hand-over is a `wake`. What the thread waits between its wake-up
/// and its first instruction is paid again at every link of the chain,
/// so it is measured here rather than guessed from a page's total.
pub mod wake_latency {
    use core::sync::atomic::{AtomicU64, Ordering};

    static COUNT: AtomicU64 = AtomicU64::new(0);
    static SUM_NS: AtomicU64 = AtomicU64::new(0);
    static MAX_NS: AtomicU64 = AtomicU64::new(0);
    /// Task that waited the longest since the last `take`.
    static MAX_PID: AtomicU64 = AtomicU64::new(0);
    /// Wake-ups that waited more than 1, 5 and 20 ms.
    static OVER: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
    const THRESHOLDS_NS: [u64; 3] = [1_000_000, 5_000_000, 20_000_000];

    pub fn record(pid: crate::process::Pid, ns: u64) {
        COUNT.fetch_add(1, Ordering::Relaxed);
        SUM_NS.fetch_add(ns, Ordering::Relaxed);
        if MAX_NS.fetch_max(ns, Ordering::Relaxed) < ns {
            MAX_PID.store(pid as u64, Ordering::Relaxed);
        }
        for (slot, threshold) in OVER.iter().zip(THRESHOLDS_NS) {
            if ns > threshold {
                slot.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Statistics since the previous call: (wake-ups, mean us, max us,
    /// task of the max, over 1 ms, over 5 ms, over 20 ms). Resets them.
    pub fn take() -> (u64, u64, u64, crate::process::Pid, [u64; 3]) {
        let count = COUNT.swap(0, Ordering::Relaxed);
        let sum = SUM_NS.swap(0, Ordering::Relaxed);
        let max = MAX_NS.swap(0, Ordering::Relaxed);
        let pid = MAX_PID.swap(0, Ordering::Relaxed) as crate::process::Pid;
        let over: [u64; 3] = core::array::from_fn(|i| OVER[i].swap(0, Ordering::Relaxed));
        (count, sum / count.max(1) / 1000, max / 1000, pid, over)
    }
}

/// Yields the CPU voluntarily. Basis of SYS_PROC_YIELD.
pub fn yield_now() {
    let cpu = crate::smp::current_cpu_id();
    let now = crate::idt::ticks();

    // The pointers are read under the lock, then the lock is released
    // before the switch: holding it across switch_context would block
    // every other core until the outgoing task returns.
    let (old_rsp_ptr, new_rsp, kernel_stack, context) = {
        let mut sched = SCHEDULER.lock();

        if !sched.started[cpu] {
            return;
        }
        let Some(previous) = sched.current[cpu] else {
            return;
        };

        let mut next = match sched.pick_next(cpu, now) {
            Some(index) => index,
            None => return,
        };

        // A task that yields (or whose slice ran out) while still wanting
        // the CPU is not candidate to `pick_next`, which only considers
        // READY tasks: alone on its core, it was handed to the idle task,
        // which halted until the next tick. Every `sched_yield`, and the
        // end of every slice, cost up to 10 ms -- a fifth of a core for a
        // `.cus` in the background, and twenty seconds for the SSE
        // self-test's two thousand yields. The idle task runs only when
        // nothing else can.
        if sched.tasks[next].binary_type == BinaryType::Idle
            && sched.tasks[previous].state == State::Running
            && sched.tasks[previous].binary_type != BinaryType::Idle
        {
            next = previous;
        }

        if next == previous {
            sched.tasks[previous].reset_slice();
            sched.tasks[previous].last_scheduled = now;
            return;
        }

        if sched.tasks[previous].state == State::Running {
            sched.tasks[previous].state = State::Ready;
        }
        let tsc = crate::random::rdtsc();
        let outgoing = &mut sched.tasks[previous];
        if outgoing.run_since != 0 {
            outgoing.run_cycles += tsc.wrapping_sub(outgoing.run_since);
        }
        sched.tasks[next].run_since = tsc;

        sched.current[cpu] = Some(next);
        CURRENT_PID[cpu].store(sched.tasks[next].pid as u64, Ordering::Relaxed);
        sched.switches += 1;

        let entering = &mut sched.tasks[next];
        if entering.binary_type == BinaryType::Idle {
            IDLE_ELECTIONS[cpu].fetch_add(1, Ordering::Relaxed);
        }
        entering.state = State::Running;
        entering.schedule_count += 1;
        entering.last_scheduled = now;
        entering.reset_slice();
        if entering.woken_ns != 0 {
            wake_latency::record(entering.pid, crate::posix::time::monotonic_ns().saturating_sub(entering.woken_ns));
            entering.woken_ns = 0;
        }

        let new_rsp = entering.rsp;
        let kernel_stack = (entering.kernel_stack_top, entering.stack_size);
        let context = UserContext { space: entering.space.clone(), fs_base: entering.fs_base, gs_base: entering.gs_base, linux: entering.linux.clone() };
        let old_rsp_ptr = &raw mut sched.tasks[previous].rsp;

        (old_rsp_ptr, new_rsp, kernel_stack, context)
    };

    // Outside the lock, but before the switch: the CPU must know the
    // kernel stack of the incoming task before it can take an interrupt
    // or a system call from ring 3.
    install_kernel_stack(cpu, kernel_stack);
    install_user_context(context);

    TOTAL_SWITCHES.fetch_add(1, Ordering::Relaxed);

    unsafe { switch_context(old_rsp_ptr, new_rsp) };
}

/// Loads the address space and TLS base of the incoming task (11.3).
///
/// The `Arc` clone taken under the lock keeps the space alive across
/// the window where the lock is released; the task itself holds another
/// reference for as long as it runs, so dropping this one here is never
/// the last.
fn install_user_context(context: UserContext) {
    crate::vm::activate(context.space.as_ref());
    crate::posix::activate(context.linux.as_ref());
    set_fs_base(context.fs_base);
    // The switch happens in the kernel, GS base = per-core structure: the
    // user base of the incoming task goes where swapgs will fetch it.
    write_msr(MSR_KERNEL_GS_BASE, context.gs_base);
}

/// What `install_user_context` loads for the incoming task. The clones
/// taken under the scheduler lock keep the space and the thread alive
/// while the lock is released; the task holds its own references.
struct UserContext {
    space: Option<alloc::sync::Arc<crate::vm::Space>>,
    fs_base: u64,
    gs_base: u64,
    linux: Option<alloc::sync::Arc<crate::posix::Thread>>,
}

/// IA32_FS_BASE: the TLS base of ring 3 code. The kernel never uses FS.
const MSR_FS_BASE: u32 = 0xC000_0100;
/// IA32_KERNEL_GS_BASE: the base `swapgs` exchanges with GS -- the user
/// one while the kernel runs.
const MSR_KERNEL_GS_BASE: u32 = 0xC000_0102;

fn write_msr(msr: u32, value: u64) {
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nomem, nostack, preserves_flags)
        );
    }
}

fn set_fs_base(value: u64) {
    write_msr(MSR_FS_BASE, value);
}

/// Changes the user GS base of the current task (`arch_prctl`). Called
/// from a system call: the user base is in IA32_KERNEL_GS_BASE now.
pub fn set_current_gs_base(value: u64) {
    let cpu = crate::smp::current_cpu_id();
    {
        let mut sched = SCHEDULER.lock();
        if let Some(index) = sched.current[cpu] {
            sched.tasks[index].gs_base = value;
        }
    }
    write_msr(MSR_KERNEL_GS_BASE, value);
}

/// User GS base of a task, for `clone` and `fork` to inherit.
pub fn gs_base_of(pid: Pid) -> u64 {
    SCHEDULER.lock().tasks.iter().find(|t| t.pid == pid).map_or(0, |t| t.gs_base)
}

/// Sets the user GS base of a dormant task.
pub fn set_gs_base(pid: Pid, value: u64) {
    if let Some(task) = SCHEDULER.lock().tasks.iter_mut().find(|t| t.pid == pid) {
        task.gs_base = value;
    }
}

/// Gives a dormant task its address space and TLS base, before it is
/// resumed (`vm::spawn_user`).
pub fn set_user_context(pid: Pid, space: Option<alloc::sync::Arc<crate::vm::Space>>, fs_base: u64) -> bool {
    let mut sched = SCHEDULER.lock();
    match sched.tasks.iter_mut().find(|t| t.pid == pid) {
        Some(task) => {
            task.space = space;
            task.fs_base = fs_base;
            true
        }
        None => false,
    }
}

/// Renames a task, after `execve` replaced the program it runs.
///
/// The task keeps the name of the program that was spawned; after an
/// exec that name is a lie, and the profile then credits the browser's
/// time to the `sh` that started it.
pub fn set_name(pid: Pid, name: &str) -> bool {
    let mut sched = SCHEDULER.lock();
    match sched.tasks.iter_mut().find(|t| t.pid == pid) {
        Some(task) => {
            task.name = alloc::string::String::from(name);
            true
        }
        None => false,
    }
}

/// Attaches a Linux thread to a dormant task (`linux::spawn`, `clone`).
pub fn set_linux(pid: Pid, thread: Option<alloc::sync::Arc<crate::posix::Thread>>) -> bool {
    let mut sched = SCHEDULER.lock();
    match sched.tasks.iter_mut().find(|t| t.pid == pid) {
        Some(task) => {
            task.linux = thread;
            true
        }
        None => false,
    }
}

/// The Linux thread a task is, if any (signals sent by TID).
pub fn linux_of(pid: Pid) -> Option<alloc::sync::Arc<crate::posix::Thread>> {
    SCHEDULER
        .lock()
        .tasks
        .iter()
        .find(|t| t.pid == pid && t.state != State::Dead)
        .and_then(|t| t.linux.clone())
}

/// Changes the TLS base of the current task (`arch_prctl(ARCH_SET_FS)`),
/// in the task and in the MSR at once.
pub fn set_current_fs_base(value: u64) {
    let cpu = crate::smp::current_cpu_id();
    {
        let mut sched = SCHEDULER.lock();
        if let Some(index) = sched.current[cpu] {
            sched.tasks[index].fs_base = value;
        }
    }
    set_fs_base(value);
}

/// Address space of a task, if it has one.
pub fn space_of(pid: Pid) -> Option<alloc::sync::Arc<crate::vm::Space>> {
    SCHEDULER.lock().tasks.iter().find(|t| t.pid == pid).and_then(|t| t.space.clone())
}

/// Publishes the kernel stack of the incoming task to the hardware.
///
/// Two distinct recipients, and both mandatory:
///   - `TSS.RSP0` -- stack adopted by the CPU on an **interrupt**
///     occurring in ring 3;
///   - `PerCpu.kernel_rsp` -- stack adopted by hand by the entry point of
///     `syscall`, which gets no hardware switch at all.
fn install_kernel_stack(cpu: usize, (stack_top, stack_size): (u64, usize)) {
    if stack_top == 0 || cpu >= MAX_CPUS {
        return;
    }
    STACK_SIZES[cpu].store(stack_size as u64, Ordering::Relaxed);
    unsafe {
        crate::gdt::set_kernel_stack(cpu, stack_top);
        crate::syscall::set_kernel_stack(cpu, stack_top);
    }
}

/// Lowest address of the kernel stack of the current task, or 0 if no task
/// has yet been elected on this core.
///
/// **A task stack has no guard page.** It is allocated in the heap by
/// `Process::new`, and overflowing it does not write into the void: it
/// overwrites the stack of the neighbouring task, then the next, with no
/// fault occurring -- the heap is mapped from end to end. The system only
/// stops much later, on a corrupted return address, tens of thousands of
/// instructions from the cause.
///
/// Any kernel code whose recursion depth depends on outside data -- a
/// script, a file, a network frame -- must therefore compare its `rsp` to
/// this bound, rather than trust a level counter: a counter bounds
/// levels, not bytes, and it is the compiler that decides the bytes.
///
/// The value is stable for the whole life of the task: reading the bound
/// once on entry is enough, there is no need to ask again at every step.
pub fn current_stack_bottom() -> u64 {
    let cpu = crate::smp::current_cpu_id();
    let top = crate::syscall::kernel_stack(cpu);
    if top == 0 {
        return 0;
    }
    // `kernel_stack_top` is the 16-byte aligned top of the stack; the real
    // base is at most fifteen bytes under the exact count, which is of no
    // consequence next to the margins the callers demand.
    let size = match STACK_SIZES[cpu.min(MAX_CPUS - 1)].load(Ordering::Relaxed) {
        0 => process::STACK_SIZE as u64,
        size => size,
    };
    top.saturating_sub(size)
}

/// Stack size of the task running on each core, published with its top.
static STACK_SIZES: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// Current stack pointer. A pure register read.
pub fn stack_pointer() -> u64 {
    let rsp: u64;
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack)) };
    rsp
}

/// Bytes of kernel stack still available under the current pointer.
///
/// Returns `usize::MAX` as long as no task has been elected on this core:
/// the stack is then that of `boot.asm`, whose bounds are not known here,
/// and no caller has a decision to take.
pub fn stack_headroom() -> usize {
    let bottom = current_stack_bottom();
    if bottom == 0 {
        return usize::MAX;
    }
    let rsp = stack_pointer();
    if rsp <= bottom {
        0
    } else {
        (rsp - bottom) as usize
    }
}

// ============================================================
//  Startup
// ============================================================

/// Elects the first task of a core and transfers control to it.
fn first_task_rsp(cpu: usize) -> (u64, (u64, usize)) {
    let mut sched = SCHEDULER.lock();

    let index = sched
        .tasks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.state == State::Ready && t.cpu_affinity as usize == cpu)
        .min_by_key(|(_, t)| t.priority())
        .map(|(i, _)| i)
        .expect("aucune tache prete pour ce coeur");

    sched.current[cpu] = Some(index);
    CURRENT_PID[cpu].store(sched.tasks[index].pid as u64, Ordering::Relaxed);
    sched.started[cpu] = true;

    let task = &mut sched.tasks[index];
    task.state = State::Running;
    task.schedule_count = 1;
    task.reset_slice();
    (task.rsp, (task.kernel_stack_top, task.stack_size))
}

/// Starts scheduling on the boot core. Never returns.
///
/// # Safety
/// At least one task assigned to core 0 must exist, and the IDT be in
/// place.
pub unsafe fn start() -> ! {
    let (rsp, kernel_stack) = first_task_rsp(0);
    install_kernel_stack(0, kernel_stack);
    unsafe { start_first_task(rsp) }
}

/// Starts scheduling on a secondary core. Never returns.
///
/// # Safety
/// The core must have its GDT, its IDT and its timer configured, and at
/// least one task must be assigned to it -- at minimum its idle task.
pub unsafe fn start_ap(cpu: usize) -> ! {
    let (rsp, kernel_stack) = first_task_rsp(cpu);
    install_kernel_stack(cpu, kernel_stack);
    unsafe { start_first_task(rsp) }
}

// ============================================================
//  Diagnostics
// ============================================================

/// Logs the process table, core by core.
pub fn dump_tasks() {
    // Copied under the lock, printed after it. The table used to be
    // printed with the scheduler lock held -- interrupts masked -- and
    // every serial byte is an exit to the hypervisor: some 270 lines every
    // thirty seconds froze every election and every wake-up of every core
    // for tens of milliseconds. A video lost eight frames at once there,
    // the sound card ran dry, and a thread woken meanwhile waited 300 ms.
    let (tasks, switches, rescues, running) = {
        let sched = SCHEDULER.lock();
        let running: Vec<Option<Pid>> = (0..MAX_CPUS)
            .map(|cpu| sched.current[cpu].and_then(|i| sched.tasks.get(i)).map(|t| t.pid))
            .collect();
        let tasks: Vec<TaskInfo> = sched.tasks.iter().map(|task| snapshot(task)).collect();
        (tasks, sched.switches, sched.starvation_rescues, running)
    };

    crate::serial_println!(
        "[sched] {} taches - {} commutations - {} sauvetages anti-famine",
        tasks.len(),
        switches,
        rescues
    );
    crate::serial_println!("[sched]  PID  CPU  TYPE     PRIO  ETAT       TICS  ELECTIONS  NOM");

    for task in tasks.iter() {
        crate::serial_println!(
            "[sched]  {:3}  {:3}  {:<7}  {:4}  {:<9}  {:5}  {:9}  {}",
            task.pid,
            task.cpu,
            task.binary_type.as_str(),
            task.priority,
            task.state.as_str(),
            task.ticks_used,
            task.schedule_count,
            task.name
        );
        // The caller is a kernel task, which the timer does not preempt:
        // three hundred lines at a hypervisor exit per byte kept a core of
        // the browser's for tens of milliseconds. A line at a time, the
        // core goes back to whoever was woken on it meanwhile.
        preempt_if_needed();
    }

    // Load distribution: what we want to check is that each core really
    // has work and that the affinity is respected.
    let cores = crate::smp_core_count().min(MAX_CPUS);
    for cpu in 0..cores {
        let name = running[cpu]
            .and_then(|pid| tasks.iter().find(|t| t.pid == pid))
            .map(|t| t.name.as_str())
            .unwrap_or("-");
        let assigned = tasks
            .iter()
            .filter(|t| t.cpu as usize == cpu && t.state.is_schedulable())
            .count();
        crate::serial_println!(
            "[sched]  coeur {} : {} taches actives - execute \"{}\"",
            cpu,
            assigned,
            name
        );
    }
}

/// Statistics: (tasks, switches, anti-starvation rescues).
pub fn stats() -> (usize, u64, u64) {
    let sched = SCHEDULER.lock();
    (
        sched.task_count(),
        sched.switch_count(),
        sched.starvation_count(),
    )
}

// ============================================================
//  5.3 -- Support of the SYS_PROC_* domain
// ============================================================

/// Snapshot of a task, as `SYS_PROC_STAT` returns it.
///
/// A copy, and not a reference: the task table is protected by a lock
/// that masks interrupts, and the `SYS_PROC_*` domain then formats the
/// answer towards user space -- an operation far too long to be done with
/// the lock held.
#[derive(Debug, Clone)]
pub struct TaskInfo {
    pub pid: Pid,
    pub parent: Pid,
    pub name: String,
    pub binary_type: BinaryType,
    pub state: State,
    pub priority: u8,
    pub cpu: u8,
    pub foreground: bool,
    pub slice: u32,
    pub ticks_used: u64,
    pub schedule_count: u64,
    pub uid: Uid,
    pub gid: Gid,
    pub role: Role,
    pub app_flags: u32,
    pub exit_code: i64,
    /// Kernel stack memory tied up by the task, in bytes.
    pub stack_bytes: u64,
    /// Processor time consumed, in TSC cycles (see `Process::run_cycles`).
    pub run_cycles: u64,
    /// Is it a Linux thread?
    pub linux: bool,
}

fn snapshot(task: &Process) -> TaskInfo {
    TaskInfo {
        pid: task.pid,
        parent: task.parent,
        name: task.name.clone(),
        binary_type: task.binary_type,
        state: task.state,
        priority: task.priority(),
        cpu: task.cpu_affinity,
        foreground: task.foreground,
        slice: task.binary_type.time_slice(task.foreground),
        ticks_used: task.ticks_used,
        schedule_count: task.schedule_count,
        uid: task.uid,
        gid: task.gid,
        role: task.role,
        app_flags: task.app_flags,
        exit_code: task.exit_code,
        stack_bytes: if task.reaped {
            0
        } else {
            process::STACK_SIZE as u64
        },
        run_cycles: task.run_cycles
            + if task.state == State::Running && task.run_since != 0 {
                crate::random::rdtsc().wrapping_sub(task.run_since)
            } else {
                0
            },
        linux: task.linux.is_some(),
    }
}

/// Snapshot of a task named by its PID.
pub fn task_info(pid: Pid) -> Option<TaskInfo> {
    let sched = SCHEDULER.lock();
    sched.tasks.iter().find(|t| t.pid == pid).map(|task| snapshot(task))
}

/// Snapshot of the current task.
pub fn current_info() -> Option<TaskInfo> {
    let cpu = crate::smp::current_cpu_id();
    let sched = SCHEDULER.lock();
    sched.current[cpu]
        .and_then(|index| sched.tasks.get(index))
        .map(|task| snapshot(task))
}

/// Every task still present in the table.
///
/// The tombstones -- dead tasks whose exit code has already been claimed
/// -- are set aside: they describe nothing living any more.
pub fn task_list() -> Vec<TaskInfo> {
    let sched = SCHEDULER.lock();
    sched
        .tasks
        .iter()
        .filter(|t| !(t.state == State::Dead && t.reaped))
        .map(|task| snapshot(task))
        .collect()
}

// ============================================================
//  5.6.1 -- Task identity
// ============================================================

/// Makes a task inherit the identity of the current task.
///
/// Called by the loader right after creation: a program runs under the
/// account of whoever launched it, never under an account it chose for
/// itself.
pub fn adopt(pid: Pid, app_flags: u32) {
    let cpu = crate::smp::current_cpu_id();
    let mut sched = SCHEDULER.lock();

    let parent = sched.current[cpu]
        .and_then(|index| sched.tasks.get(index))
        .map(|t| (t.pid, t.uid, t.gid, t.role))
        .unwrap_or((0, crate::fs::perm::ROOT_UID, crate::fs::perm::SYSTEM_GID, Role::Root));

    if let Some(task) = sched.tasks.iter_mut().find(|t| t.pid == pid) {
        task.parent = parent.0;
        task.uid = parent.1;
        task.gid = parent.2;
        task.role = parent.3;
        task.app_flags = app_flags;
    }
}

/// Access context of the current task.
///
/// This is what the `SYS_FS_*` domain uses in place of the `Credentials`
/// the caller supplied until now. A task absent from the table -- the
/// case of the boot code, before the scheduler runs -- counts as the
/// kernel itself.
pub fn current_credentials() -> Credentials {
    let cpu = crate::smp::current_cpu_id();
    let sched = SCHEDULER.lock();
    sched.current[cpu]
        .and_then(|index| sched.tasks.get(index))
        .map(|t| t.credentials())
        .unwrap_or_else(Credentials::kernel)
}

/// Changes the account a task runs under.
///
/// The legitimacy check is not here: it belongs to `SYS_USER_SETID`,
/// which only allows a privilege **drop**.
pub fn set_credentials(pid: Pid, who: Credentials) -> bool {
    let mut sched = SCHEDULER.lock();
    match sched.tasks.iter_mut().find(|t| t.pid == pid) {
        Some(task) => {
            task.uid = who.uid;
            task.gid = who.gid;
            task.role = who.role;
            true
        }
        None => false,
    }
}

// ============================================================
//  5.3.1 -- Suspend and resume
// ============================================================

/// Suspends an eligible task (SYS_PROC_SUSPEND).
///
/// A task running on another core goes SUSPENDED without being evicted
/// immediately: it will yield the CPU at the end of its slice or at the
/// next system call, and will not be re-elected afterwards. Evicting it
/// by force would mean interrupting a core in the middle of a critical
/// section, which would bring nothing but risk.
pub fn suspend(pid: Pid) -> bool {
    let mut sched = SCHEDULER.lock();
    match sched.tasks.iter_mut().find(|t| t.pid == pid) {
        Some(task) if task.state == State::Ready || task.state == State::Running => {
            task.state = State::Suspended;
            true
        }
        _ => false,
    }
}

/// Is the task dead, but not yet claimed?
pub fn is_dying(pid: Pid) -> bool {
    let sched = SCHEDULER.lock();
    sched
        .tasks
        .iter()
        .any(|t| t.pid == pid && t.state == State::Dead && !t.reaped)
}

/// Does the task still exist, in a useful form?
pub fn exists(pid: Pid) -> bool {
    let sched = SCHEDULER.lock();
    sched
        .tasks
        .iter()
        .any(|t| t.pid == pid && !(t.state == State::Dead && t.reaped))
}

// ============================================================
//  5.3.3 -- Termination and reaping
// ============================================================

/// Tasks waiting for the death of another.
///
/// A global list rather than a field per task: waits are rare, and one
/// more field in `Process` would weigh down a structure copied on every
/// snapshot.
static WAITERS: SpinLock<Vec<(Pid, Pid)>> = SpinLock::new(Vec::new());

/// Ends the current task, publishing an exit code.
pub fn exit_current_with(code: i64) -> ! {
    let cpu = crate::smp::current_cpu_id();
    let mut pid = 0;
    {
        let mut sched = SCHEDULER.lock();
        if let Some(index) = sched.current[cpu] {
            sched.tasks[index].state = State::Dead;
            sched.tasks[index].exit_code = code;
            pid = sched.tasks[index].pid;
            crate::serial_println!(
                "[sched] tache {} terminee sur le coeur {} apres {} tics - code {}",
                pid,
                cpu,
                sched.tasks[index].ticks_used,
                code
            );
        }
    }

    crate::ipc::unregister(pid);
    wake_waiters(pid);

    yield_now();
    crate::halt_loop()
}

/// Wakes everything that was waiting for the death of `pid`.
fn wake_waiters(pid: Pid) {
    let woken: Vec<Pid> = {
        let mut waiters = WAITERS.lock();
        let mut woken = Vec::new();
        waiters.retain(|(waiter, target)| {
            if *target == pid {
                woken.push(*waiter);
                false
            } else {
                true
            }
        });
        woken
    };

    for waiter in woken {
        wake(waiter);
    }
}

/// Ends a named task, without being it (SYS_PROC_KILL).
pub fn kill(pid: Pid, code: i64) -> bool {
    let killed = {
        let mut sched = SCHEDULER.lock();
        match sched.tasks.iter_mut().find(|t| t.pid == pid) {
            Some(task) if task.state != State::Dead => {
                task.state = State::Dead;
                task.exit_code = code;
                true
            }
            _ => false,
        }
    };

    if killed {
        crate::ipc::unregister(pid);
        wake_waiters(pid);
    }
    killed
}

/// Reaps the exit code of a dead task and returns its resources.
///
/// This is where the limit left open by phase 3 closes: the kernel stack
/// is released, and the loader image -- if there is one -- given back to
/// the frame allocator. An already claimed task stays in the table as a
/// tombstone: removing it would shift the indexes each core holds on its
/// current task.
pub fn try_reap(pid: Pid) -> Option<i64> {
    let code = {
        let mut sched = SCHEDULER.lock();

        // **A task marked dead can still be running.** `kill` sets the
        // state from another core, but the core running it will only
        // abandon it at its next election. Taking its stack back now
        // would amount to pulling it out from under its feet: the task
        // runs on a few instructions, writes into memory given back to
        // the allocator, and corrupts whatever reoccupies it. As long as
        // a core names it as current, it is not reapable.
        let index = sched.tasks.iter().position(|t| t.pid == pid)?;
        if sched.current.iter().any(|slot| *slot == Some(index)) {
            return None;
        }

        let task = &mut sched.tasks[index];
        if task.state != State::Dead || task.reaped {
            return None;
        }
        task.reaped = true;
        task.release_stack();
        (task.exit_code, task.take_space(), task.take_linux())
    };
    let (code, space, linux) = code;
    // Last reference to a private space: its pages go back to the frame
    // allocator here, outside the scheduler lock. The thread first: it
    // holds its process, which holds the space too.
    drop(linux);
    drop(space);

    // The image is returned **outside the scheduler lock**: `unload` takes
    // the loader's and handles the page tables, and nesting the two locks
    // would open an acquisition order the reverse of the loading one.
    let image_pages = unsafe { crate::binfmt::loader::release_for(pid) };

    // The `SYS_MEM_ALLOC` regions the task did not free, and its possible
    // sleep deadline: without this cleanup, a program dying while asleep
    // would leave an entry pointing at a PID that will never answer.
    let arena_pages = crate::syscall::domain_mem::release_for(pid);
    crate::syscall::domain_time::forget(pid);

    if image_pages.is_some() || arena_pages > 0 {
        crate::serial_println!(
            "[sched] tache {} recuperee - {} pages d'image, {} pages d'arene rendues",
            pid,
            image_pages.unwrap_or(0),
            arena_pages
        );
    }

    Some(code)
}

/// Waits for the end of a task and reaps its exit code.
///
/// Blocking: it is the only call of the process domain that puts its
/// caller to sleep, which ranks it among the asynchronous calls of spec
/// 6.3. Returns `None` if the PID names nothing, or if it names the
/// caller -- waiting for oneself would never end.
pub fn wait_for(pid: Pid) -> Option<i64> {
    let me = current_pid();
    if pid == me || pid == 0 {
        return None;
    }

    // Registration comes first: a target dying from here on will find the
    // entry and emit the wake-up.
    WAITERS.lock().push((me, pid));

    let result = loop {
        // The sleep is armed **before** the test. If the target dies
        // between the two, `wake` finds it asleep and makes it eligible
        // again; if it is already dead, the test sees it and the sleep is
        // cancelled without any CPU having been yielded.
        arm_sleep();

        if let Some(code) = try_reap(pid) {
            cancel_sleep();
            break Some(code);
        }
        if !exists(pid) {
            cancel_sleep();
            break None;
        }

        // Target dead but not yet reapable: it still occupies a core.
        // Nobody will announce it a second time, so going to sleep here
        // would be going to sleep for good. We yield the CPU without
        // sleeping, long enough for that core to switch -- at most one
        // slice.
        if is_dying(pid) {
            cancel_sleep();
            yield_now();
            continue;
        }

        yield_now();
    };

    WAITERS.lock().retain(|(waiter, _)| *waiter != me);
    result
}

/// Number of tasks waiting on another -- diagnostics.
pub fn waiting_count() -> usize {
    WAITERS.lock().len()
}

/// Is anyone waiting for the end of this task?
///
/// `init.dsa` (6.5.1) uses it to adopt only the **orphans**: reaping a
/// task whose parent is waiting for the exit code would steal it, and its
/// `SYS_PROC_WAIT` would return "unknown" where it expected a code.
pub fn is_awaited(pid: Pid) -> bool {
    WAITERS.lock().iter().any(|(_, target)| *target == pid)
}

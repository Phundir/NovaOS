//! Process and thread structures.
//!
//! In phase 2, a "process" is a plain kernel thread: same address space,
//! same privilege level. Only the `userland` task switches to ring 3,
//! through the phase 2.8 mechanism.

use alloc::string::String;

use crate::frame::FRAME_SIZE;
use crate::fs::perm::{Credentials, Gid, Role, Uid};

unsafe extern "C" {
    /// First code of every new task (boot/switch.asm): calls the entry
    /// function found in R12, with the stack aligned as the ABI wants.
    fn task_trampoline();
}

pub type Pid = u64;

/// Stack size allocated to each task: 128 KiB.
///
/// It was 32 KiB until phase 9 ran a recursive interpreter on it. A
/// single level of `sdk::script` call costs close to 9 KiB of stack in a
/// debug build -- the sum of the frames of `call`, `statement`, `binary`,
/// `eval` and `block`, which the compiler sizes and not us. Twenty-four
/// levels, the bound the interpreter imposes on itself, therefore
/// demanded more than 200 KiB.
///
/// The stacks are physically contiguous frames, reached through the
/// physical window (`KernelStack`) -- not the kernel heap. Taken from the
/// heap, 128 KiB for each of the browser's three hundred threads was 37
/// MiB of a 64 MiB heap; a speed test starting a few more threads ran it
/// out, and the kernel stopped on whatever allocation came next.
///
/// **These stacks have no guard page**: overflowing them does not write
/// into the void but into whatever frame follows, with no fault to
/// signal it. Growing the stack
/// does not remove that risk, it pushes it back; it is
/// `scheduler::stack_headroom` that measures it, and any kernel code
/// whose recursion depth depends on outside data must query it.
pub const STACK_SIZE: usize = 32 * FRAME_SIZE as usize;

/// Frames held by the kernel stacks of every task, alive or not yet
/// released.
static STACK_FRAMES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Frames the kernel stacks hold right now: a leak check counting free
/// frames sets them aside, since a task another self-test starts
/// meanwhile legitimately takes 32 of them.
pub fn kernel_stack_frames() -> usize {
    STACK_FRAMES.load(core::sync::atomic::Ordering::Relaxed)
}

/// A task's kernel stack: `frames` physically consecutive frames, used
/// through the physical window, given back when the task is released.
pub struct KernelStack {
    first: crate::frame::Frame,
    frames: usize,
}

impl KernelStack {
    /// A zeroed stack of `size` bytes (a whole number of frames). Running
    /// out of memory here is running out of it for good: the kernel stops
    /// with the same message as a failed heap allocation.
    fn new(size: usize) -> KernelStack {
        let frames = size / FRAME_SIZE as usize;
        let Some(first) = crate::frame::allocate_contiguous(frames) else {
            alloc::alloc::handle_alloc_error(core::alloc::Layout::from_size_align(size, 16).unwrap_or(core::alloc::Layout::new::<u8>()));
        };
        // SAFETY: frames just taken from the allocator, mapped nowhere
        // else; the physical window covers all RAM.
        unsafe { core::ptr::write_bytes(first.virt_addr() as *mut u8, 0, size) };
        STACK_FRAMES.fetch_add(frames, core::sync::atomic::Ordering::Relaxed);
        KernelStack { first, frames }
    }

    /// Lowest address of the stack, in the physical window.
    fn base(&self) -> u64 {
        self.first.virt_addr()
    }
}

impl Drop for KernelStack {
    fn drop(&mut self) {
        for index in 0..self.frames as u64 {
            // SAFETY: the task is dead and out of every use (`release_stack`);
            // nothing maps these frames but the physical window.
            unsafe { crate::frame::deallocate_frame(crate::frame::Frame(self.first.0 + index)) };
        }
        STACK_FRAMES.fetch_sub(self.frames, core::sync::atomic::Ordering::Relaxed);
    }
}

// ============================================================
//  Binary types and priorities (spec 3.1 and 7.3)
// ============================================================

/// Binary format. Determines the priority, the CPU affinity and the time
/// slice -- see spec 3.1, 7.2 and 7.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BinaryType {
    /// Direct System Access -- official system apps.
    Dsa,
    /// System Software -- explorer, task manager, compositor.
    Ss,
    /// Game Software -- automatic foreground/background switch.
    Gs,
    /// Common Utility Software -- ordinary applications.
    Cus,
    /// Random Access Software -- total access, not recommended.
    Ras,
    /// Internal kernel tasks: maximum priority, outside the hierarchy.
    Kernel,
    /// Idle. One per core, of lower priority than everything else: it
    /// runs only when its core has nothing else to do.
    Idle,
}

impl BinaryType {
    /// Fixed priority, determined at launch and never modified while
    /// running (spec 7.1).
    pub fn priority(&self) -> u8 {
        match self {
            BinaryType::Kernel => 0,
            BinaryType::Dsa => 1,
            BinaryType::Ss => 2,
            BinaryType::Gs => 3,
            BinaryType::Cus => 4,
            BinaryType::Ras => 5,
            BinaryType::Idle => 6,
        }
    }

    /// CPU slice in timer ticks, according to the foreground or
    /// background state (spec 7.3).
    pub fn time_slice(&self, foreground: bool) -> u32 {
        match (self, foreground) {
            (BinaryType::Kernel, _) => 40,
            (BinaryType::Dsa, _) => 40, // never reduced
            (BinaryType::Ss, true) => 25,
            (BinaryType::Ss, false) => 15,
            (BinaryType::Gs, true) => 20,
            (BinaryType::Gs, false) => 2, // automatic pause mode
            (BinaryType::Cus, true) => 10,
            (BinaryType::Cus, false) => 4,
            (BinaryType::Ras, true) => 5,
            (BinaryType::Ras, false) => 1,
            (BinaryType::Idle, _) => 1,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            BinaryType::Kernel => "kernel",
            BinaryType::Dsa => ".dsa",
            BinaryType::Ss => ".ss",
            BinaryType::Gs => ".gs",
            BinaryType::Cus => ".cus",
            BinaryType::Ras => ".ras",
            BinaryType::Idle => "idle",
        }
    }
}

// ============================================================
//  CPU affinity (spec 7.2)
// ============================================================

/// May this binary type run on this core?
///
/// Distribution from the specification:
///
/// ```text
///   Core 0          .dsa exclusively -- never shared
///   Cores 1..N/2    .ss and the GUI compositor
///   Cores N/2..N-1  .gs, .cus and .ras
///   All             kernel tasks
/// ```
///
/// Below four cores, the three-class partition no longer makes sense: we
/// merely reserve core 0 for the system.
pub fn affinity_allows(binary_type: BinaryType, cpu: usize, cores: usize) -> bool {
    if cores <= 1 {
        return cpu == 0;
    }

    match binary_type {
        // Internal tasks and idle run wherever they are put.
        BinaryType::Kernel | BinaryType::Idle => true,
        // Core 0 reserved: always available for the system.
        BinaryType::Dsa => cpu == 0,
        // Simplified partition on small configurations.
        _ if cores < 4 => cpu >= 1,
        BinaryType::Ss => cpu >= 1 && cpu < cores / 2,
        _ => cpu >= cores / 2,
    }
}

/// Describes the allowed core range in words -- diagnostics.
pub fn affinity_label(binary_type: BinaryType, cores: usize) -> &'static str {
    if cores <= 1 {
        return "coeur 0 (monoprocesseur)";
    }
    match binary_type {
        BinaryType::Kernel | BinaryType::Idle => "tous les coeurs",
        BinaryType::Dsa => "coeur 0 exclusivement",
        _ if cores < 4 => "coeurs 1 et suivants",
        BinaryType::Ss => "moitie basse (1..N/2)",
        _ => "moitie haute (N/2..N-1)",
    }
}

// ============================================================
//  States (spec 7.4)
// ============================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Waiting for its CPU turn.
    Ready,
    /// Using the CPU right now.
    Running,
    /// Waiting for an event -- IPC wake-up or expiry of a delay.
    Sleeping,
    /// Paused by hand through SYS_PROC_SUSPEND.
    Suspended,
    /// Finished, awaiting cleanup by the kernel.
    Dead,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Ready => "READY",
            State::Running => "RUNNING",
            State::Sleeping => "SLEEPING",
            State::Suspended => "SUSPENDED",
            State::Dead => "DEAD",
        }
    }

    /// The scheduler only considers tasks in this state.
    pub fn is_schedulable(&self) -> bool {
        matches!(self, State::Ready | State::Running)
    }
}

// ============================================================
//  Task structure
// ============================================================

pub struct Process {
    pub pid: Pid,
    pub name: String,
    pub binary_type: BinaryType,
    pub state: State,

    /// Saved stack top. That is the **whole** CPU context: the registers
    /// themselves are pushed onto it by switch_context.
    pub rsp: u64,

    /// **Fixed** top of the task stack, loaded into `TSS.RSP0` and into
    /// `PerCpu.kernel_rsp` on every switch.
    ///
    /// It is the stack the CPU switches to when an interrupt or a
    /// `syscall` occurs while the task runs in ring 3. One stack per
    /// task, and not per core: two ring 3 tasks of the same core yielding
    /// the CPU in the middle of a system call would otherwise share the
    /// same area and overwrite each other's frame.
    ///
    /// A task in ring 3 has nothing useful on its kernel stack -- the
    /// system call handler empties it entirely before handing back -- so
    /// it can start again from the top on every entry.
    pub kernel_stack_top: u64,

    /// Size of that stack, published with its top so that
    /// `scheduler::stack_headroom` knows where it ends.
    pub stack_size: usize,

    /// Task stack, kept so that it can be freed at its death.
    stack: Option<KernelStack>,

    /// Is the application in the foreground? Only the .gs switch
    /// automatically (spec 7.1).
    pub foreground: bool,

    /// Ticks left on the current slice.
    pub slice_remaining: u32,

    /// Core this task runs on (spec 7.2). Assigned at creation according
    /// to the type, and not changed afterwards. Spec 7.2 would allow a
    /// move within the type's range; measured, moving woken threads to
    /// idle cores cost the Web Browser more video frames than it saved
    /// (see DECISIONS.md).
    pub cpu_affinity: u8,

    // --- statistics ---
    /// CPU ticks consumed since launch.
    pub ticks_used: u64,
    /// Number of times the task has been elected.
    pub schedule_count: u64,
    /// Tick at which the task last received CPU -- the basis of the
    /// anti-starvation computation.
    pub last_scheduled: u64,
    /// Instant of the last `wake` of a Linux thread, in nanoseconds, until
    /// it runs: what the wake-up latency statistics measure. Zero when the
    /// task was not woken or is not a Linux thread.
    pub woken_ns: u64,
    /// Processor time consumed, in TSC cycles, counted at every switch:
    /// the tick count above has a 10 ms grain, far too coarse for what a
    /// page load spends in each of its threads.
    pub run_cycles: u64,
    /// TSC when the task last got its core.
    pub run_since: u64,

    // --- 5.6.1: process identity ---
    /// Account the task runs under.
    ///
    /// Until phase 5, every call to the filesystem carried `Credentials`
    /// supplied by the caller: the kernel did not know who was behind a
    /// request. Attaching them to the process is what lets `SYS_FS_*`
    /// derive them instead of receiving them -- and therefore refuse a
    /// ring 3 program the right to declare itself ROOT.
    pub uid: Uid,
    pub gid: Gid,
    pub role: Role,

    /// Process that asked for this creation. Zero for the tasks born with
    /// the system.
    pub parent: Pid,

    /// Flags declared by the binary header (spec 3.4). They condition
    /// access to certain domains: `SYS_NET_*` is only open to a binary
    /// that has declared the network.
    pub app_flags: u32,

    /// Code returned on termination, readable by `SYS_PROC_WAIT`.
    pub exit_code: i64,

    /// Private address space (phase 11.3), `None` for the tasks that run
    /// in the boot PML4 -- the kernel tasks and the Nova images. Loaded
    /// into CR3 by the scheduler before the task runs.
    pub space: Option<alloc::sync::Arc<crate::vm::Space>>,

    /// TLS base of a ring 3 thread (the FS.base MSR), set through
    /// `arch_prctl` or `clone`. Restored on every switch to this task.
    pub fs_base: u64,

    /// User GS base of a ring 3 thread (`arch_prctl(ARCH_SET_GS)`), which
    /// Firefox's wasm2c sandboxes use for their memory base. While the
    /// kernel runs it sits in IA32_KERNEL_GS_BASE, and `swapgs` puts it
    /// back on the way to ring 3.
    pub gs_base: u64,

    /// Linux thread this task is (phase 11.5), `None` for every other
    /// task. Published per core on every switch, like the space.
    pub linux: Option<alloc::sync::Arc<crate::posix::Thread>>,

    /// Has the exit code already been collected?
    ///
    /// A dead task stays in the table until someone claims it: without
    /// that, a parent calling `wait` after its child's death would find
    /// nothing left to wait for.
    pub reaped: bool,
}

impl Process {
    /// Creates a task ready to be elected on the given core.
    ///
    /// The stack is prepared so that the first `switch_context` to this
    /// task ends in running `entry`: a dummy context is pushed onto it
    /// whose return address is the entry point.
    pub fn new(pid: Pid, name: &str, binary_type: BinaryType, entry: fn() -> !, cpu: u8) -> Self {
        Self::with_stack(pid, name, binary_type, entry, cpu, STACK_SIZE)
    }

    /// Same, with a stack of `stack_size` bytes (at least `STACK_SIZE`):
    /// for the few tasks whose recursion is deep by nature -- the
    /// browser, which interprets the scripts of pages.
    pub fn with_stack(pid: Pid, name: &str, binary_type: BinaryType, entry: fn() -> !, cpu: u8, stack_size: usize) -> Self {
        let stack_size = stack_size.max(STACK_SIZE).next_multiple_of(FRAME_SIZE as usize);
        let stack = KernelStack::new(stack_size);
        let stack_top = stack.base() + stack_size as u64;

        // 16-byte alignment demanded by the System V ABI.
        let stack_top = stack_top & !0xF;

        // Building of the initial context, in the exact order
        // switch_context expects to pop it.
        let rsp = unsafe {
            let mut sp = stack_top as *mut u64;

            // Return address: the final `ret` of switch_context jumps to
            // the trampoline, which calls the entry held in R12. Jumping
            // to the entry directly would leave its stack misaligned by
            // 8 (see boot/switch.asm).
            sp = sp.sub(1);
            sp.write(task_trampoline as *const () as u64);

            // RFLAGS: bit 1 always set, bit 9 (IF) so that the task runs
            // with interrupts enabled -- otherwise preemption would stop
            // working once the task is elected.
            sp = sp.sub(1);
            sp.write(0x202);

            // rbp, rbx, r12, r13, r14, r15, in push order: null, except
            // R12 which carries the entry to the trampoline.
            for register in 0..6 {
                sp = sp.sub(1);
                sp.write(if register == 2 { entry as *const () as u64 } else { 0 });
            }

            // FXSAVE image restored by the switch (phase 11.2). The stack
            // top is 16-aligned and eight words sit above, so the image
            // is 16-aligned as FXRSTOR demands.
            let area = (sp as *mut u8).sub(crate::fpu::AREA_SIZE);
            core::ptr::copy_nonoverlapping(crate::fpu::INITIAL.as_ptr(), area, crate::fpu::AREA_SIZE);

            area as u64
        };

        Process {
            pid,
            name: String::from(name),
            binary_type,
            state: State::Ready,
            rsp,
            kernel_stack_top: stack_top,
            stack_size,
            stack: Some(stack),
            foreground: true,
            slice_remaining: binary_type.time_slice(true),
            cpu_affinity: cpu,
            ticks_used: 0,
            schedule_count: 0,
            last_scheduled: 0,
            woken_ns: 0,
            run_cycles: 0,
            run_since: 0,
            // A task is born with the kernel identity; the scheduler
            // replaces it at once with that of its creator.
            uid: crate::fs::perm::ROOT_UID,
            gid: crate::fs::perm::SYSTEM_GID,
            role: Role::Root,
            parent: 0,
            app_flags: 0,
            exit_code: 0,
            space: None,
            fs_base: 0,
            gs_base: 0,
            linux: None,
            reaped: false,
        }
    }

    /// Access context of the process, as the filesystem expects it.
    pub fn credentials(&self) -> Credentials {
        Credentials::new(self.uid, self.gid, self.role)
    }

    /// Reloads the time slice according to the type and display state.
    pub fn reset_slice(&mut self) {
        self.slice_remaining = self.binary_type.time_slice(self.foreground);
    }

    pub fn priority(&self) -> u8 {
        self.binary_type.priority()
    }

    /// Frees the stack. To be called only once the task is really dead
    /// and out of all use.
    pub fn release_stack(&mut self) {
        self.stack = None;
    }

    /// Detaches the address space, to be dropped outside the scheduler
    /// lock: dropping the last reference frees every page of it.
    pub fn take_space(&mut self) -> Option<alloc::sync::Arc<crate::vm::Space>> {
        self.space.take()
    }

    pub fn take_linux(&mut self) -> Option<alloc::sync::Arc<crate::posix::Thread>> {
        self.linux.take()
    }
}

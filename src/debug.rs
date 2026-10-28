//! Tracing of ring 3 programs (16.4): what a debugger needs from the
//! kernel -- breakpoints, single steps, registers, memory.
//!
//! ## How a stop happens
//!
//! A breakpoint is an `int3` (0xCC) written over the first byte of an
//! instruction; a step is the trap flag (TF) set in the program's RFLAGS,
//! which raises a debug exception (#DB) after one instruction. Both
//! vectors enter through `debug_trap_entry` (`boot/fault.asm`), which
//! saves every register so that the handler can read them **and change
//! them** -- the debugger writes a register, the program resumes with it.
//!
//! When the task is traced, the handler records the stop (the registers,
//! the reason), wakes the debugger, and puts the task to sleep **inside
//! the exception**, on its kernel stack, until the debugger says what to
//! do: continue, step, or end. The task never runs a user instruction
//! while stopped, so its memory can be patched without a race. An
//! untraced task that reaches `int3` dies of it, as before.
//!
//! ## Going past a breakpoint
//!
//! Resuming at an address that holds a breakpoint would trap again at
//! once. The original byte goes back, the program runs one instruction
//! with TF, and the #DB that follows puts the `int3` back -- then the
//! program goes on without TF if it was told to continue, or stops if it
//! was told to step. The debugger never sees that extra trap.
//!
//! ## Memory
//!
//! Programs loaded by `binfmt::loader` live in the arena of the shared
//! page hierarchy: the debugger reads and writes them through the
//! physical window, after a page walk, which reaches read-only code as
//! well (that is how an `int3` gets into a text page).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::process::Pid;
use crate::scheduler::WaitOutcome;
use crate::vga::SpinLock;
use crate::vm::FaultFrame;

/// The trap flag of RFLAGS.
const TF: u64 = 1 << 8;

/// Registers of a stopped program, as the debugger sees and edits them.
#[derive(Debug, Clone, Copy, Default)]
pub struct Registers {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub rsp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rip: u64,
    pub rflags: u64,
}

/// The names `get`/`set` know, in display order.
pub const REGISTER_NAMES: &[&str] = &[
    "rax", "rbx", "rcx", "rdx", "rsi", "rdi", "rbp", "rsp", "r8", "r9", "r10", "r11", "r12", "r13", "r14", "r15", "rip",
    "rflags",
];

impl Registers {
    fn from_frame(frame: &FaultFrame) -> Registers {
        Registers {
            rax: frame.rax,
            rbx: frame.rbx,
            rcx: frame.rcx,
            rdx: frame.rdx,
            rsi: frame.rsi,
            rdi: frame.rdi,
            rbp: frame.rbp,
            rsp: frame.rsp,
            r8: frame.r8,
            r9: frame.r9,
            r10: frame.r10,
            r11: frame.r11,
            r12: frame.r12,
            r13: frame.r13,
            r14: frame.r14,
            r15: frame.r15,
            rip: frame.rip,
            rflags: frame.rflags,
        }
    }

    /// Back into the frame: what the program resumes with. RFLAGS keeps
    /// what ring 3 may not change (IF on, IOPL 0), whatever was written.
    fn to_frame(&self, frame: &mut FaultFrame) {
        frame.rax = self.rax;
        frame.rbx = self.rbx;
        frame.rcx = self.rcx;
        frame.rdx = self.rdx;
        frame.rsi = self.rsi;
        frame.rdi = self.rdi;
        frame.rbp = self.rbp;
        frame.rsp = self.rsp;
        frame.r8 = self.r8;
        frame.r9 = self.r9;
        frame.r10 = self.r10;
        frame.r11 = self.r11;
        frame.r12 = self.r12;
        frame.r13 = self.r13;
        frame.r14 = self.r14;
        frame.r15 = self.r15;
        frame.rip = self.rip;
        frame.rflags = (self.rflags | 0x200) & !0x3000;
    }

    pub fn get(&self, name: &str) -> Option<u64> {
        Some(match name {
            "rax" => self.rax,
            "rbx" => self.rbx,
            "rcx" => self.rcx,
            "rdx" => self.rdx,
            "rsi" => self.rsi,
            "rdi" => self.rdi,
            "rbp" => self.rbp,
            "rsp" => self.rsp,
            "r8" => self.r8,
            "r9" => self.r9,
            "r10" => self.r10,
            "r11" => self.r11,
            "r12" => self.r12,
            "r13" => self.r13,
            "r14" => self.r14,
            "r15" => self.r15,
            "rip" => self.rip,
            "rflags" => self.rflags,
            _ => return None,
        })
    }

    pub fn set(&mut self, name: &str, value: u64) -> bool {
        let slot = match name {
            "rax" => &mut self.rax,
            "rbx" => &mut self.rbx,
            "rcx" => &mut self.rcx,
            "rdx" => &mut self.rdx,
            "rsi" => &mut self.rsi,
            "rdi" => &mut self.rdi,
            "rbp" => &mut self.rbp,
            "rsp" => &mut self.rsp,
            "r8" => &mut self.r8,
            "r9" => &mut self.r9,
            "r10" => &mut self.r10,
            "r11" => &mut self.r11,
            "r12" => &mut self.r12,
            "r13" => &mut self.r13,
            "r14" => &mut self.r14,
            "r15" => &mut self.r15,
            "rip" => &mut self.rip,
            "rflags" => &mut self.rflags,
            _ => return false,
        };
        *slot = value;
        true
    }
}

/// Why a program stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// On one of the debugger's breakpoints, before the instruction at
    /// this address runs.
    Breakpoint(u64),
    /// After one instruction, as asked.
    Step,
    /// On an `int3` the program has in its own code.
    Trap(u64),
}

/// What the debugger tells a stopped program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resume {
    Continue,
    Step,
    Kill,
}

/// What waiting for a traced program gives.
#[derive(Debug, Clone)]
pub enum Event {
    Stopped(Stop, Registers),
    /// The program ended, with its exit code.
    Exited(i64),
}

struct Trace {
    debugger: Pid,
    /// Set while the program sleeps in its exception.
    stopped: Option<(Stop, Registers)>,
    resume: Option<Resume>,
    /// Address of each breakpoint, and the byte the `int3` covers.
    breakpoints: BTreeMap<u64, u8>,
    /// The breakpoint stepped over, to be put back on the next #DB.
    reinsert: Option<u64>,
    /// That step was asked by the debugger (stop after it), not taken
    /// to get past a breakpoint (go on after it).
    user_step: bool,
}

static TRACES: SpinLock<BTreeMap<Pid, Trace>> = SpinLock::new(BTreeMap::new());

/// Traps taken by traced programs, since boot (diagnostics).
static TRAPS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

pub fn traps() -> u64 {
    TRAPS.load(core::sync::atomic::Ordering::Relaxed)
}

/// `pid` is traced by `debugger` from now on. To be called before the
/// task first runs (`binfmt::loader::spawn_with`), or the first trap
/// might find nobody.
pub fn attach(pid: Pid, debugger: Pid) {
    TRACES.lock().insert(
        pid,
        Trace { debugger, stopped: None, resume: None, breakpoints: BTreeMap::new(), reinsert: None, user_step: false },
    );
}

pub fn is_traced(pid: Pid) -> bool {
    TRACES.lock().contains_key(&pid)
}

// ------------------------------------------------------------
//  Memory of the traced program
// ------------------------------------------------------------

/// The kernel address of the byte at user address `addr`, through the
/// physical window. None when it is not a mapped user page.
fn window(addr: u64) -> Option<*mut u8> {
    // SAFETY: the page walk only reads the tables of the active space;
    // the physical window exists from boot on.
    let translation = unsafe { crate::memory::translate(addr) }?;
    if !translation.user {
        return None;
    }
    Some(crate::memory::phys_to_virt(translation.phys) as *mut u8)
}

/// Reads `len` bytes at `addr` of the traced program, the debugger's
/// own `int3` hidden (what is shown is the program's code).
pub fn read_memory(pid: Pid, addr: u64, len: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(len);
    for offset in 0..len as u64 {
        let at = addr.wrapping_add(offset);
        let Some(byte) = window(at) else {
            return Err(alloc::format!("adresse 0x{:x} non lisible", at));
        };
        // SAFETY: `window` returned the kernel alias of a mapped byte.
        out.push(unsafe { core::ptr::read_volatile(byte) });
    }
    if let Some(trace) = TRACES.lock().get(&pid) {
        for (address, original) in &trace.breakpoints {
            if *address >= addr && *address < addr + len as u64 {
                out[(*address - addr) as usize] = *original;
            }
        }
    }
    Ok(out)
}

/// Writes one byte of the program's memory, read-only code included.
fn poke(addr: u64, value: u8) -> Result<u8, String> {
    let Some(byte) = window(addr) else {
        return Err(alloc::format!("adresse 0x{:x} non accessible", addr));
    };
    // SAFETY: as in `read_memory`; the traced program is stopped or not
    // started, so it is not executing this byte.
    unsafe {
        let old = core::ptr::read_volatile(byte);
        core::ptr::write_volatile(byte, value);
        Ok(old)
    }
}

/// Writes `data` at `addr` of a stopped program (the breakpoints keep
/// their `int3`; the bytes they cover change underneath).
pub fn write_memory(pid: Pid, addr: u64, data: &[u8]) -> Result<(), String> {
    let mut traces = TRACES.lock();
    let trace = traces.get_mut(&pid).ok_or_else(|| String::from("programme non suivi"))?;
    for (offset, value) in data.iter().enumerate() {
        let at = addr + offset as u64;
        match trace.breakpoints.get_mut(&at) {
            Some(original) => *original = *value,
            None => {
                poke(at, *value)?;
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------
//  Breakpoints
// ------------------------------------------------------------

pub fn set_breakpoint(pid: Pid, addr: u64) -> Result<(), String> {
    let mut traces = TRACES.lock();
    let trace = traces.get_mut(&pid).ok_or_else(|| String::from("programme non suivi"))?;
    if trace.breakpoints.contains_key(&addr) {
        return Ok(());
    }
    let original = poke(addr, 0xCC)?;
    trace.breakpoints.insert(addr, original);
    Ok(())
}

pub fn clear_breakpoint(pid: Pid, addr: u64) -> Result<(), String> {
    let mut traces = TRACES.lock();
    let trace = traces.get_mut(&pid).ok_or_else(|| String::from("programme non suivi"))?;
    let Some(original) = trace.breakpoints.remove(&addr) else {
        return Err(alloc::format!("aucun point d'arret en 0x{:x}", addr));
    };
    // Stepping over it right now: it is simply not put back.
    if trace.reinsert == Some(addr) {
        trace.reinsert = None;
        return Ok(());
    }
    poke(addr, original).map(|_| ())
}

pub fn breakpoints(pid: Pid) -> Vec<u64> {
    TRACES.lock().get(&pid).map(|t| t.breakpoints.keys().copied().collect()).unwrap_or_default()
}

// ------------------------------------------------------------
//  Stopping and resuming
// ------------------------------------------------------------

/// The stop of a stopped program.
pub fn stopped(pid: Pid) -> Option<(Stop, Registers)> {
    TRACES.lock().get(&pid).and_then(|t| t.stopped)
}

/// Changes the registers the stopped program will resume with.
pub fn set_registers(pid: Pid, registers: Registers) -> Result<(), String> {
    let mut traces = TRACES.lock();
    let trace = traces.get_mut(&pid).ok_or_else(|| String::from("programme non suivi"))?;
    match &mut trace.stopped {
        Some((_, saved)) => {
            *saved = registers;
            Ok(())
        }
        None => Err(String::from("le programme n'est pas arrete")),
    }
}

/// Lets a stopped program go on.
pub fn resume(pid: Pid, how: Resume) -> Result<(), String> {
    {
        let mut traces = TRACES.lock();
        let trace = traces.get_mut(&pid).ok_or_else(|| String::from("programme non suivi"))?;
        if trace.stopped.is_none() {
            return Err(String::from("le programme n'est pas arrete"));
        }
        trace.resume = Some(how);
    }
    crate::scheduler::wake(pid);
    Ok(())
}

/// Waits until the traced program stops or ends.
pub fn wait(pid: Pid) -> Event {
    let stopped = || TRACES.lock().get(&pid).and_then(|t| if t.resume.is_none() { t.stopped } else { None });
    match crate::scheduler::wait_for_or(pid, &|| stopped().is_some()) {
        WaitOutcome::Condition => match stopped() {
            Some((stop, registers)) => Event::Stopped(stop, registers),
            None => Event::Exited(-1),
        },
        WaitOutcome::Exited(code) => {
            TRACES.lock().remove(&pid);
            Event::Exited(code)
        }
        WaitOutcome::Gone => {
            TRACES.lock().remove(&pid);
            Event::Exited(-1)
        }
    }
}

/// Ends the traced program (from a stop, or not started), its
/// breakpoints with it.
pub fn kill(pid: Pid) {
    let stopped = TRACES.lock().get(&pid).is_some_and(|t| t.stopped.is_some());
    if stopped {
        let _ = resume(pid, Resume::Kill);
    } else {
        crate::scheduler::kill(pid, -9);
    }
    let _ = crate::scheduler::wait_for(pid);
    TRACES.lock().remove(&pid);
}

/// Called by `debug_trap_entry` for #DB (1) and #BP (3), the vector in
/// the error slot of the frame.
#[unsafe(no_mangle)]
pub extern "C" fn debug_trap_handler(frame: &mut FaultFrame) {
    let vector = frame.error as u8;
    let from_user = frame.cs & 3 == 3;
    if !from_user {
        kernel_trap(vector, frame);
        return;
    }
    let pid = crate::scheduler::current_pid();
    let debugger = {
        let mut traces = TRACES.lock();
        let Some(trace) = traces.get_mut(&pid) else {
            drop(traces);
            let name = if vector == 3 { "Point d'arret" } else { "Debug" };
            crate::idt::user_exception_kernel_gs(vector, name, frame.rip, None);
        };
        let stop = if vector == 3 {
            let addr = frame.rip.wrapping_sub(1);
            if trace.breakpoints.contains_key(&addr) {
                frame.rip = addr;
                Stop::Breakpoint(addr)
            } else {
                Stop::Trap(addr)
            }
        } else {
            clear_dr6();
            frame.rflags &= !TF;
            match trace.reinsert.take() {
                Some(addr) => {
                    // Past the breakpoint: it goes back.
                    let _ = poke(addr, 0xCC);
                    if !trace.user_step {
                        return;
                    }
                    Stop::Step
                }
                None => Stop::Step,
            }
        };
        trace.stopped = Some((stop, Registers::from_frame(frame)));
        trace.resume = None;
        trace.debugger
    };
    TRAPS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    // Interrupts on while the task sleeps in its exception: the handler
    // was entered through an interrupt gate.
    // SAFETY: kernel stack, kernel GS base (the entry swapped it).
    unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
    crate::scheduler::wake(debugger);
    let how = loop {
        crate::scheduler::arm_sleep();
        let order = TRACES.lock().get_mut(&pid).and_then(|t| t.resume);
        match order {
            Some(how) => {
                crate::scheduler::cancel_sleep();
                break how;
            }
            None if !TRACES.lock().contains_key(&pid) => {
                // The debugger is gone without a word: the program ends.
                crate::scheduler::cancel_sleep();
                break Resume::Kill;
            }
            None => crate::scheduler::yield_now(),
        }
    };
    if how == Resume::Kill {
        TRACES.lock().remove(&pid);
        crate::scheduler::exit_current_with(-9);
    }
    {
        let mut traces = TRACES.lock();
        if let Some(trace) = traces.get_mut(&pid) {
            if let Some((_, registers)) = trace.stopped.take() {
                registers.to_frame(frame);
            }
            trace.resume = None;
            // Resuming on a breakpoint: its byte back for one step.
            if let Some(original) = trace.breakpoints.get(&frame.rip).copied() {
                let _ = poke(frame.rip, original);
                trace.reinsert = Some(frame.rip);
                trace.user_step = how == Resume::Step;
                frame.rflags |= TF;
            } else if how == Resume::Step {
                frame.rflags |= TF;
            }
        }
    }
    // Back to the entry, which swaps GS and returns with `iretq`: no
    // interrupt may come between the two.
    // SAFETY: restores the state the entry expects.
    unsafe { core::arch::asm!("cli", options(nomem, nostack)) };
}

/// A trap in ring 0: `int3` is the IDT's own test (`idt::test_breakpoint`),
/// a #DB is a kernel bug.
fn kernel_trap(vector: u8, frame: &mut FaultFrame) {
    if vector == 3 {
        crate::idt::kernel_breakpoint(frame.rip);
        return;
    }
    crate::idt::kernel_debug_exception(frame.rip, frame.rsp, frame.rflags)
}

fn clear_dr6() {
    // SAFETY: DR6 holds status bits only; clearing them is what the
    // manual asks of the #DB handler.
    unsafe { core::arch::asm!("xor {0:e}, {0:e}", "mov dr6, {0}", out(reg) _, options(nomem, nostack)) };
}

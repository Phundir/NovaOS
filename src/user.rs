//! Entry into user space (phase 2.8.3).
//!
//! The demonstration program lives in the `.user` section, linked at
//! 0x400000 but loaded with the kernel. It is copied into a fresh page
//! marked USER, given a stack, and then we switch to ring 3.
//!
//! The crossing is done by `iretq` rather than `sysret`: the latter
//! assumes a return from a system call, hence a pre-existing user
//! context. `iretq` builds that context from scratch out of a frame
//! pushed by hand.
//!
//! The kernel stack -- TSS.RSP0 and `PerCpu.kernel_rsp` -- is that of the
//! current task, reinstalled on every switch by the scheduler: two ring 3
//! tasks on the same core therefore cannot overwrite each other's system
//! call frame.

use core::arch::asm;

use crate::frame;
use crate::gdt::{USER_CODE_SELECTOR, USER_DATA_SELECTOR};
use crate::memory::{self, phys_to_virt};

/// Virtual address of the user code -- must match USER_VMA in the linker
/// script.
pub const USER_CODE_ADDR: u64 = 0x0000_0000_0040_0000;
/// Base of the user stack.
pub const USER_STACK_ADDR: u64 = 0x0000_0000_0080_0000;
/// 16 KiB of user stack.
pub const USER_STACK_SIZE: u64 = 4096 * 4;

unsafe extern "C" {
    static __user_load: u8;
    static user_program_start: u8;
    static user_program_end: u8;
}

/// Bytes of the demonstration program, as linked into the kernel.
///
/// The `.user` section is linked for 0x400000 but loaded with the kernel
/// at `__user_load`: it is that copy the sample generator packs into a
/// Nova OS binary, to give the phase 3.2 loader code that really runs
/// rather than a run of `nop`. The program uses RIP-relative addressing
/// only: it runs at any base, which the loader exploits by placing it in
/// its arena.
pub fn demo_program() -> &'static [u8] {
    let start = &raw const user_program_start as u64;
    let end = &raw const user_program_end as u64;
    let load = &raw const __user_load as u64;
    let size = (end - start) as usize;

    unsafe { core::slice::from_raw_parts(phys_to_virt(load) as *const u8, size) }
}

/// Installs the user program and switches to ring 3.
///
/// Never returns: control passes to the user code, which will end with a
/// call to SYS_PROC_KILL.
///
/// # Safety
/// The GDT must contain the ring 3 descriptors, and `syscall::init` must
/// have been called on the current core.
pub unsafe fn launch() -> ! {
    let cpu = crate::smp::current_cpu_id();

    let start = &raw const user_program_start as u64;
    let end = &raw const user_program_end as u64;
    let load = &raw const __user_load as u64;
    let size = (end - start) as usize;

    crate::serial_println!(
        "[user] programme de {} octets - charge a 0x{:x}, execute a 0x{:x} sur le coeur {}",
        size,
        load,
        USER_CODE_ADDR,
        cpu
    );

    // --- code pages ---
    let code_pages = (size as u64 + 4095) / 4096;
    for i in 0..code_pages {
        let f = frame::allocate_zeroed_frame().expect("plus de frames pour le code utilisateur");

        // The copy goes through the physical window: we write into the
        // frame without depending on the not-yet-existing user mapping.
        let src = phys_to_virt(load + i * 4096) as *const u8;
        let dst = phys_to_virt(f.start_addr()) as *mut u8;
        let chunk = (size as u64 - i * 4096).min(4096) as usize;
        unsafe { core::ptr::copy_nonoverlapping(src, dst, chunk) };

        // Executable but not writable code: an attempt to write will
        // produce a page fault.
        unsafe {
            memory::map_user_page(USER_CODE_ADDR + i * 4096, f.start_addr(), false, true)
                .expect("mapping du code utilisateur")
        };
    }

    // --- stack pages ---
    let stack_pages = USER_STACK_SIZE / 4096;
    for i in 0..stack_pages {
        let f = frame::allocate_zeroed_frame().expect("plus de frames pour la pile utilisateur");
        unsafe {
            // Non-executable stack: W^X demands that what is written does
            // not execute.
            memory::map_user_page(USER_STACK_ADDR + i * 4096, f.start_addr(), true, false)
                .expect("mapping de la pile utilisateur")
        };
    }

    let user_stack_top = (USER_STACK_ADDR + USER_STACK_SIZE - 16) & !0xF;

    // Check of the mappings before the jump -- an error here would show as
    // an immediate page fault and be hard to interpret.
    unsafe {
        memory::report_translation("code user", USER_CODE_ADDR);
        memory::report_translation("pile user", USER_STACK_ADDR);
    }

    crate::vga::set_color(crate::vga::Color::Yellow, crate::vga::Color::Black);
    crate::kprintln!("[user] bascule en ring 3 depuis le coeur {}...", cpu);
    crate::vga::set_color(crate::vga::Color::LightGray, crate::vga::Color::Black);

    unsafe { enter_ring3(USER_CODE_ADDR, user_stack_top) }
}

/// Builds an interrupt return frame and executes it.
///
/// `iretq` pops RIP, CS, RFLAGS, RSP and SS. Since the pushed CS carries
/// an RPL of 3, the CPU performs a complete privilege change: it loads
/// the new stack and switches to ring 3.
///
/// # Safety
/// The selectors must exist in the GDT with DPL 3, and the addresses be
/// mapped with the USER bit.
pub unsafe fn enter_ring3(entry: u64, stack_top: u64) -> ! {
    unsafe {
        asm!(
            // Kernel -> user crossing: the GS convention wants GS.base to
            // carry the per-core structure in ring 0 and zero in ring 3
            // (see `syscall::init`). `syscall` and `sysret` do the round
            // trip on their own; `iretq` does not -- so it has to be done
            // here.
            //
            // Interrupts masked for the duration of the window: a handler
            // running between the two would run in ring 0 with the user
            // GS. None of ours reads `gs:`, but the window costs nothing
            // to close. `iretq` re-arms IF from the pushed RFLAGS.
            "cli",
            "swapgs",
            "push {ss}",        // user SS
            "push {rsp}",       // user RSP
            "push {rflags}",    // RFLAGS -- bit 9 set to keep the IRQs
            "push {cs}",        // user CS, RPL 3
            "push {rip}",       // point d'entree
            "iretq",
            ss = in(reg) USER_DATA_SELECTOR as u64,
            rsp = in(reg) stack_top,
            rflags = in(reg) 0x202u64,
            cs = in(reg) USER_CODE_SELECTOR as u64,
            rip = in(reg) entry,
            options(noreturn)
        )
    }
}

//! Global Descriptor Table and Task State Segment, per core.
//!
//! The order of the descriptors is imposed by `sysret`, which derives its
//! selectors from `STAR[63:48]` by arithmetic: user code at base+16, user
//! data at base+8. Hence the layout below, with an unused but mandatory
//! 32-bit code descriptor:
//!
//! ```text
//!   0x00  null descriptor
//!   0x08  kernel code      (ring 0)   <- STAR[47:32]
//!   0x10  kernel data      (ring 0)      = 0x08 + 8
//!   0x18  32-bit code      (ring 3)   <- STAR[63:48]
//!   0x20  data             (ring 3)      = 0x18 + 8
//!   0x28  64-bit code      (ring 3)      = 0x18 + 16
//!   0x30  TSS (two slots)
//! ```
//!
//! GDT and TSS are duplicated per core: two processors loading the same
//! TSS descriptor would share their exception stacks, and a simultaneous
//! double fault would corrupt both contexts.

use core::arch::asm;
use core::mem::size_of;

/// Cap on the cores handled. Beyond it, the extra cores are ignored.
pub const MAX_CPUS: usize = 8;

/// Index in the IST of the stack reserved for the double fault.
pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

/// 20 KiB per core: enough to absorb a stack overflow without falling
/// back on the faulty stack.
const STACK_SIZE: usize = 4096 * 5;

pub const KERNEL_CODE_SELECTOR: u16 = 0x08;
pub const KERNEL_DATA_SELECTOR: u16 = 0x10;
/// Base used by sysret to compute the user selectors.
pub const USER_BASE_SELECTOR: u16 = 0x18;
/// User selectors, RPL 3 included.
pub const USER_DATA_SELECTOR: u16 = 0x20 | 3;
pub const USER_CODE_SELECTOR: u16 = 0x28 | 3;
pub const TSS_SELECTOR: u16 = 0x30;

/// Number of entries: 6 descriptors plus the 2 slots of the TSS.
const GDT_ENTRIES: usize = 8;

/// The System V ABI demands a 16-byte aligned stack.
#[repr(C, align(16))]
struct Stack([u8; STACK_SIZE]);

static mut DOUBLE_FAULT_STACKS: [Stack; MAX_CPUS] =
    [const { Stack([0; STACK_SIZE]) }; MAX_CPUS];

// ============================================================
//  Task State Segment
// ============================================================

/// Layout imposed by Intel: 104 bytes, 64-bit fields aligned on 4 bytes
/// -- hence `packed(4)`.
#[repr(C, packed(4))]
#[derive(Clone, Copy)]
pub struct TaskStateSegment {
    reserved_1: u32,
    /// RSP0..RSP2 -- stacks loaded on a privilege change. RSP0 is
    /// indispensable as soon as ring 3 code runs: it is the stack the CPU
    /// switches to when an interrupt occurs.
    pub privilege_stack_table: [u64; 3],
    reserved_2: u64,
    /// IST1..IST7 -- unconditional stacks for the critical exceptions.
    pub interrupt_stack_table: [u64; 7],
    reserved_3: u64,
    reserved_4: u16,
    /// Pointed past the TSS limit: no I/O permission granted.
    pub iomap_base: u16,
}

impl TaskStateSegment {
    pub const fn new() -> Self {
        TaskStateSegment {
            reserved_1: 0,
            privilege_stack_table: [0; 3],
            reserved_2: 0,
            interrupt_stack_table: [0; 7],
            reserved_3: 0,
            reserved_4: 0,
            iomap_base: size_of::<TaskStateSegment>() as u16,
        }
    }
}

static mut TSS: [TaskStateSegment; MAX_CPUS] =
    [const { TaskStateSegment::new() }; MAX_CPUS];

static mut GDT: [[u64; GDT_ENTRIES]; MAX_CPUS] = [[0; GDT_ENTRIES]; MAX_CPUS];

/// Structure expected by `lgdt` and `lidt`.
#[repr(C, packed(2))]
pub struct DescriptorTablePointer {
    pub limit: u16,
    pub base: u64,
}

// Bits of a segment descriptor.
const SEG_WRITABLE: u64 = 1 << 41;
const SEG_EXECUTABLE: u64 = 1 << 43;
const SEG_USER_TYPE: u64 = 1 << 44;
const SEG_PRESENT: u64 = 1 << 47;
const SEG_LONG_MODE: u64 = 1 << 53;
const SEG_DEFAULT_32: u64 = 1 << 54;
/// DPL 3 -- reachable from user space.
const SEG_DPL3: u64 = 3 << 45;

/// Builds and loads the GDT of the given core.
///
/// # Safety
/// `cpu_id` must be unique per core and below MAX_CPUS.
pub unsafe fn init_cpu(cpu_id: usize) {
    assert!(cpu_id < MAX_CPUS, "cpu_id hors limites");

    unsafe {
        // --- stack dedicated to the double fault, private to this core ---
        let stacks = &raw const DOUBLE_FAULT_STACKS as *const Stack;
        let stack_start = stacks.add(cpu_id) as u64;
        let stack_end = stack_start + STACK_SIZE as u64; // the stack grows downwards

        let tss = (&raw mut TSS as *mut TaskStateSegment).add(cpu_id);
        let ist = (&raw mut (*tss).interrupt_stack_table) as *mut u64;
        ist.add(DOUBLE_FAULT_IST_INDEX as usize)
            .write_unaligned(stack_end);

        // --- descriptors ---
        let tss_addr = tss as u64;
        let tss_limit = (size_of::<TaskStateSegment>() - 1) as u64;

        let gdt = (&raw mut GDT as *mut [u64; GDT_ENTRIES]).add(cpu_id);

        (*gdt)[0] = 0; // mandatory null descriptor

        // 0x08 -- kernel code, ring 0, 64-bit.
        (*gdt)[1] = SEG_EXECUTABLE | SEG_USER_TYPE | SEG_PRESENT | SEG_LONG_MODE;

        // 0x10 -- kernel data, ring 0.
        (*gdt)[2] = SEG_WRITABLE | SEG_USER_TYPE | SEG_PRESENT;

        // 0x18 -- 32-bit ring 3 code. Never used, but sysret computes its
        // selectors from this position: the slot must exist and carry
        // DPL 3.
        (*gdt)[3] =
            SEG_EXECUTABLE | SEG_USER_TYPE | SEG_PRESENT | SEG_DPL3 | SEG_DEFAULT_32;

        // 0x20 -- ring 3 data.
        (*gdt)[4] = SEG_WRITABLE | SEG_USER_TYPE | SEG_PRESENT | SEG_DPL3;

        // 0x28 -- 64-bit ring 3 code.
        (*gdt)[5] =
            SEG_EXECUTABLE | SEG_USER_TYPE | SEG_PRESENT | SEG_LONG_MODE | SEG_DPL3;

        // 0x30 -- TSS system descriptor, spread over 16 bytes.
        // 0x89 = present, DPL 0, type 9 (64-bit TSS available).
        (*gdt)[6] = (tss_limit & 0xFFFF)
            | ((tss_addr & 0xFF_FFFF) << 16)
            | (0x89 << 40)
            | (((tss_limit >> 16) & 0xF) << 48)
            | (((tss_addr >> 24) & 0xFF) << 56);
        (*gdt)[7] = tss_addr >> 32;

        // --- loading ---
        let pointer = DescriptorTablePointer {
            limit: (size_of::<[u64; GDT_ENTRIES]>() - 1) as u16,
            base: gdt as u64,
        };
        asm!(
            "lgdt [{}]",
            in(reg) &pointer,
            options(readonly, nostack, preserves_flags)
        );

        reload_code_segment(KERNEL_CODE_SELECTOR);
        reload_data_segments(KERNEL_DATA_SELECTOR);

        asm!(
            "ltr ax",
            in("ax") TSS_SELECTOR,
            options(nostack, preserves_flags)
        );
    }
}

/// Initialisation of the boot core.
pub fn init() {
    unsafe { init_cpu(0) };
}

/// Fills in RSP0: the stack the CPU switches to when an interrupt or an
/// exception occurs while ring 3 code is running. Indispensable before
/// any entry into user space.
///
/// # Safety
/// `stack_top` must name the top of a valid, aligned kernel stack.
pub unsafe fn set_kernel_stack(cpu_id: usize, stack_top: u64) {
    assert!(cpu_id < MAX_CPUS, "cpu_id hors limites");
    unsafe {
        let tss = (&raw mut TSS as *mut TaskStateSegment).add(cpu_id);
        let rsp = (&raw mut (*tss).privilege_stack_table) as *mut u64;
        rsp.write_unaligned(stack_top);
    }
}

/// CS cannot be loaded by a `mov` in long mode. A far return is simulated:
/// the selector and the resume address are pushed, then `retfq` pops them
/// -- which updates CS along the way.
unsafe fn reload_code_segment(selector: u16) {
    unsafe {
        asm!(
            "push {sel}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            sel = in(reg) u64::from(selector),
            tmp = lateout(reg) _,
        );
    }
}

/// FS and GS are left untouched: GS carries the base of the per-core
/// structure used by the system call entry point.
unsafe fn reload_data_segments(selector: u16) {
    unsafe {
        asm!(
            "mov ds, {0:x}",
            "mov es, {0:x}",
            "mov ss, {0:x}",
            in(reg) selector,
            options(nostack, preserves_flags)
        );
    }
}

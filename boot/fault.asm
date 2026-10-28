; ============================================================
;  Nova OS -- Page fault entry, user copies, full-state ring 3 entry
;  (phase 11.3)
;
;  Three pieces of assembly that Rust cannot express:
;
;  * page_fault_entry: the fault handler must be able to *change* the
;    interrupted RIP (to recover from a kernel copy that touched a bad
;    user address) and to be preempted. An `extern "x86-interrupt"`
;    function gets its frame by value: writes to it are not guaranteed
;    to reach the stack. So the entry saves every general register,
;    hands a pointer to the whole frame to Rust, and returns through
;    whatever Rust left in it.
;
;  * user_copy: the one routine allowed to touch user memory from ring 0.
;    If a fault inside it cannot be resolved, the handler moves RIP to
;    user_copy_fixup, which returns the number of bytes not copied --
;    the classic exception-table recovery, with a table of one entry.
;
;  * enter_user: first entry into ring 3 with a complete register set,
;    for threads born from `clone` (every register of the parent, RAX 0)
;    and for processes whose ABI wants values in registers at start.
; ============================================================

section .text
bits 64

global page_fault_entry
global user_copy
global user_copy_end
global user_copy_fixup
global enter_user
extern page_fault_handler

; ------------------------------------------------------------
;  page_fault_entry
;
;  On entry the CPU has pushed SS, RSP, RFLAGS, CS, RIP and the error
;  code. The layout built below is `vm::FaultFrame`:
;
;    [rsp+0]   r15 ... [rsp+112] rax   (15 registers)
;    [rsp+120] error code
;    [rsp+128] rip, cs, rflags, rsp, ss
;
;  15 pushes plus the error code: 128 bytes on top of the 40-byte
;  hardware frame, which the CPU aligns on 16. 168 bytes in all, so RSP
;  is 8 mod 16 here and one more word re-aligns it before the call.
;
;  swapgs when the fault comes from ring 3: the handler may switch
;  tasks (a fault resolved by reading a file sleeps, a fatal one ends
;  the task), and every switch assumes the kernel GS base is loaded --
;  the next task may return to ring 3 through sysret, which swaps back.
;  The CS pushed by the CPU is at [rsp+16] here (error code, RIP, CS).
; ------------------------------------------------------------
page_fault_entry:
    test qword [rsp + 16], 3
    jz .kernel_gs
    swapgs
.kernel_gs:
    ; The direction flag is the faulting program's: musl's memmove copies
    ; backwards (std; rep movsb) and faults on the first page it touches.
    ; The ABI promises DF clear at every call; the handler's memset and
    ; memcpy (rep stos/movs) would otherwise run backwards, 4 KiB into
    ; the physical frame *below* the one being filled -- the kernel heap,
    ; a page table, another process's page. The interrupt handlers
    ; written in Rust get their `cld` from the compiler, the syscall
    ; entry from IA32_FMASK; this entry is the one written by hand. The
    ; program's own DF comes back with its RFLAGS at iretq.
    cld
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15

    mov rdi, rsp
    sub rsp, 8
    call page_fault_handler
    add rsp, 8

    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax
    add rsp, 8                  ; error code
    test qword [rsp + 8], 3     ; CS of the interrupted code
    jz .to_kernel
    swapgs
.to_kernel:
    iretq

; ------------------------------------------------------------
;  user_copy(dst: RDI, src: RSI, len: RDX) -> RAX = bytes NOT copied
;
;  Either side may be a user address. The caller holds no lock: a fault
;  in here is resolved by the page fault handler like any user fault,
;  which may allocate, read a file, or sleep.
; ------------------------------------------------------------
user_copy:
    mov rcx, rdx
    rep movsb
user_copy_end:
    xor eax, eax
    ret

; Reached only when page_fault_handler gives up on a fault whose RIP lies
; in [user_copy, user_copy_end): RCX still counts what was left.
user_copy_fixup:
    mov rax, rcx
    ret

; ------------------------------------------------------------
;  enter_user(regs: RDI) -> !
;
;  `regs` points at a `vm::UserRegs`, in this order:
;    rax rbx rcx rdx rsi rdi rbp r8 r9 r10 r11 r12 r13 r14 r15
;    rip rsp rflags
;  (18 words). The iretq frame is built on the kernel stack first, the
;  registers are loaded last -- RDI, which points at the structure, is
;  loaded at the very end.
; ------------------------------------------------------------
USER_DS     equ 0x20 | 3
USER_CS     equ 0x28 | 3

enter_user:
    cli
    push USER_DS                ; SS
    push qword [rdi + 16*8]     ; RSP
    mov rax, [rdi + 17*8]       ; RFLAGS, IF forced on, IOPL 0
    or rax, 0x200
    and rax, ~0x3000
    push rax
    push USER_CS                ; CS
    push qword [rdi + 15*8]     ; RIP

    mov rax, [rdi + 0*8]
    mov rbx, [rdi + 1*8]
    mov rcx, [rdi + 2*8]
    mov rdx, [rdi + 3*8]
    mov rsi, [rdi + 4*8]
    mov rbp, [rdi + 6*8]
    mov r8,  [rdi + 7*8]
    mov r9,  [rdi + 8*8]
    mov r10, [rdi + 9*8]
    mov r11, [rdi + 10*8]
    mov r12, [rdi + 11*8]
    mov r13, [rdi + 12*8]
    mov r14, [rdi + 13*8]
    mov r15, [rdi + 14*8]
    mov rdi, [rdi + 5*8]

    swapgs                      ; ring 3 runs with the user GS base
    iretq

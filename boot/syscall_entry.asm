; ============================================================
;  Nova OS -- System call entry point
;
;  The `syscall` instruction does not change stack: the CPU stays on the
;  user stack, puts the return RIP in RCX and RFLAGS in R11. It is up to
;  the kernel to switch to a safe stack itself.
;
;  `swapgs` swaps GS.base with IA32_KERNEL_GS_BASE. In ring 3, GS.base
;  is 0 and the MSR points at the per-core structure; the swap therefore
;  gives access to that structure without overwriting a single register.
;
;  Calling convention on the user side:
;    RAX = call number
;    RDI, RSI, RDX, R10, R8, R9 = arguments 1 to 6
;    RAX = return value (negative on error)
;
;  R10 replaces RCX for the fourth argument, RCX being overwritten by
;  the `syscall` instruction itself.
; ============================================================

section .text
bits 64

global syscall_entry
extern syscall_dispatch

; Offsets in the PerCpu structure.
PERCPU_KERNEL_RSP   equ 0
PERCPU_USER_RSP     equ 8

syscall_entry:
    swapgs
    mov [gs:PERCPU_USER_RSP], rsp       ; puts the user stack aside
    mov rsp, [gs:PERCPU_KERNEL_RSP]     ; adopts the kernel stack

    ; --- building of the SyscallFrame ---
    ; The push order is the reverse of the field order of the Rust
    ; structure: the last pushed occupies the lowest address.
    push qword [gs:PERCPU_USER_RSP]
    push r11                            ; user RFLAGS
    push rcx                            ; user RIP
    push r15
    push r14
    push r13
    push r12
    push r11
    push r10
    push r9
    push r8
    push rbp
    push rdi
    push rsi
    push rdx
    push rcx
    push rbx
    push rax

    ; 18 pushes of 8 bytes = 144, a multiple of 16: the alignment the
    ; System V ABI demands is preserved at the moment of the `call`.

    ; The stack is now safe: the interrupts can be re-armed, which
    ; IA32_FMASK had masked on entry.
    sti

    mov rdi, rsp                        ; &SyscallFrame
    call syscall_dispatch
    mov [rsp], rax                      ; return value into frame.rax

    cli

    ; --- restore ---
    pop rax
    pop rbx
    pop rcx
    pop rdx
    pop rsi
    pop rdi
    pop rbp
    pop r8
    pop r9
    pop r10
    pop r11
    pop r12
    pop r13
    pop r14
    pop r15

    pop rcx                             ; sysret expects the RIP in RCX
    pop r11                             ; and RFLAGS in R11
    mov rsp, [rsp]                      ; user stack

    swapgs
    o64 sysret

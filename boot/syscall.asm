; ============================================================
;  Nova OS -- System call gate
;
;  The `syscall` instruction does not change stack: it merely loads CS
;  and SS from STAR, files RIP into RCX and RFLAGS into R11, then jumps
;  to the address held in LSTAR. All the rest -- switching to the kernel
;  stack, saving the registers -- is up to us.
;
;  RCX and R11 are therefore reserved by the hardware: the calling
;  convention uses R10 in place of RCX for the fourth argument.
; ============================================================

section .text
bits 64

global syscall_entry
global enter_user_mode
extern syscall_dispatch

; ------------------------------------------------------------
;  Entry point of `syscall`.
;
;  GS points at the per-core structure:
;    [gs:0]  = top of the kernel stack of the current task
;    [gs:8]  = save slot of the user RSP
; ------------------------------------------------------------
syscall_entry:
    ; Swaps GS_BASE and KERNEL_GS_BASE: GS names the per-core kernel
    ; data again.
    swapgs

    mov [gs:8], rsp             ; puts the user stack aside
    mov rsp, [gs:0]             ; adopts the kernel stack

    ; Building of the frame, in the exact order the SyscallFrame
    ; structure expects on the Rust side.
    push qword [gs:8]           ; user rsp
    push r11                    ; rflags saved by syscall
    push rcx                    ; return rip saved by syscall
    push rax                    ; call number, then return value
    push rdi
    push rsi
    push rdx
    push r10
    push r8
    push r9
    push rbx
    push rbp
    push r12
    push r13
    push r14
    push r15

    ; Address of the frame as the only argument.
    mov rdi, rsp
    call syscall_dispatch

    pop r15
    pop r14
    pop r13
    pop r12
    pop rbp
    pop rbx
    pop r9
    pop r8
    pop r10
    pop rdx
    pop rsi
    pop rdi
    pop rax                     ; return value written by the dispatcher
    pop rcx                     ; rip
    pop r11                     ; rflags
    pop rsp                     ; user stack

    swapgs
    o64 sysret

; ------------------------------------------------------------
;  enter_user_mode(rip: u64, rsp: u64) -> !
;
;  RDI = user entry point
;  RSI = top of the user stack
;
;  `iretq` is used rather than `sysret` for the first transition: it
;  allows setting RFLAGS explicitly, in particular the IF bit, which
;  sysret only does by copying R11.
; ------------------------------------------------------------
enter_user_mode:
    ; KERNEL_GS_BASE takes the current value: the next swapgs, on entry
    ; to the first syscall, will restore it.
    swapgs

    mov rcx, rdi                ; target rip
    mov rdx, rsi                ; target rsp

    ; Registers zeroed: no kernel data must leak into user space.
    ;
    xor rax, rax
    xor rbx, rbx
    xor rsi, rsi
    xor rdi, rdi
    xor rbp, rbp
    xor r8, r8
    xor r9, r9
    xor r10, r10
    xor r11, r11
    xor r12, r12
    xor r13, r13
    xor r14, r14
    xor r15, r15

    push qword 0x2B             ; SS  -- ring 3 data
    push rdx                    ; user RSP
    push qword 0x202            ; RFLAGS -- bit 1 reserved, IF set
    push qword 0x33             ; CS  -- ring 3 code
    push rcx                    ; RIP

    iretq

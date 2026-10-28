; ============================================================
;  Nova OS -- Context switch
;
;  The System V ABI distinguishes two categories of registers:
;    - volatile (rax, rcx, rdx, rsi, rdi, r8-r11) -- the caller saves
;      them itself if it needs them;
;    - preserved (rbx, rbp, r12-r15) -- it is up to the callee to restore.
;
;  Since switch_context is called from Rust, the compiler has already
;  saved the volatile ones at the call site. So we preserve only the
;  six callee-saved registers, plus RFLAGS.
;
;  Plus the x87/SSE state (phase 11.2): the kernel never touches it, but
;  ring 3 programs do, and it belongs to the task, not to the core. The
;  512-byte FXSAVE image goes on the outgoing stack under the general
;  registers. FXSAVE wants 16-byte alignment: the caller's stack is
;  16-aligned at the call (System V), the call pushes 8 bytes and the
;  seven pushes below 56 more -- 64 in all, so RSP is aligned again
;  when the image is written. process.rs builds initial frames with the
;  same layout and the same alignment.
; ============================================================

section .text
bits 64

global switch_context
global start_first_task
global task_trampoline
extern switched_in

; ------------------------------------------------------------
;  switch_context(old_rsp: *mut u64, new_rsp: u64)
;
;  RDI = address where to store the RSP of the outgoing context
;  RSI = RSP of the incoming context
;
;  The stack becomes the medium of the save: we push the state,
;  note the top, then switch to the other stack and pop.
; ------------------------------------------------------------
switch_context:
    ; --- save of the current context ---
    pushfq                      ; RFLAGS -- carries IF, DF and the flags
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    sub rsp, 512
    fxsave [rsp]

    ; Note the stack top reached in the structure of the outgoing process.
    mov [rdi], rsp

    ; --- switch ---
    mov rsp, rsi

    ; --- restore of the incoming context ---
    fxrstor [rsp]
    add rsp, 512
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    popfq

    ; The return address at the top of the new stack is that of the
    ; incoming context: ret takes us there.
    ret

; ------------------------------------------------------------
;  start_first_task(new_rsp: u64) -> !
;
;  First start: no context to save, we merely adopt the stack of
;  the task and pop its initial state.
; ------------------------------------------------------------
start_first_task:
    mov rsp, rdi

    fxrstor [rsp]
    add rsp, 512
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    popfq

    ret

; ------------------------------------------------------------
;  task_trampoline -- first code of every new task
;
;  The initial frame built by process.rs "returns" here, with the entry
;  function in R12. Returning straight into the entry function would
;  start it with RSP 16-aligned, where the System V ABI promises
;  RSP = 8 mod 16 at a function's first instruction (the return address
;  just pushed). The CALL restores that: every task used to run with its
;  stack off by 8, harmless until FXSAVE (phase 11.2) demanded alignment.
;
;  The entry function never returns (fn() -> !): UD2 turns a broken
;  promise into an immediate, visible fault.
; ------------------------------------------------------------
task_trampoline:
    ; The core that switched here has stopped writing the task it left
    ; (scheduler::LEAVING): that one may be reaped from now on. R12 is
    ; callee-saved, and RSP keeps the alignment the comment above needs.
    call switched_in
    call r12
    ud2

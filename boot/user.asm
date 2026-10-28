; ============================================================
;  Nova OS -- Demonstration user program
;
;  This code runs in ring 3, in a memory space distinct from the kernel.
;  No direct hardware access is possible: the only interface is the
;  `syscall` instruction.
;
;  It is linked at virtual address 0x400000 but loaded with the kernel:
;  the BSP copies the block into a dedicated frame before mapping it.
;
;  Calling convention (spec 6.1):
;    RAX = syscall number -- SYS_{DOMAIN}_{ACTION}
;    RDI, RSI, RDX, R10, R8, R9 = arguments
;    RAX = return value, negative on error
; ============================================================

SYS_DEBUG_PRINT equ 0x0001
SYS_PROC_KILL   equ 0x0202
SYS_PROC_LIST   equ 0x0206
SYS_PROC_SELF   equ 0x020A
SYS_TIME_TICKS  equ 0x0701

section .usercode
bits 64

global user_code_start
global user_code_end

user_code_start:

_user_entry:
    ; --- start-up announcement ---
    mov rax, SYS_DEBUG_PRINT
    lea rdi, [rel msg_hello]
    mov rsi, msg_hello_len
    syscall

    ; --- fetch our own PID ---
    mov rax, SYS_PROC_SELF
    syscall
    mov r12, rax                ; kept for the final display

    ; --- number of active processes ---
    mov rax, SYS_PROC_LIST
    syscall
    mov r13, rax

    ; --- system clock ---
    mov rax, SYS_TIME_TICKS
    syscall
    mov r14, rax

    ; --- work loop: a few iterations interspersed with calls ---
    xor r15, r15
.loop:
    inc r15

    mov rax, SYS_DEBUG_PRINT
    lea rdi, [rel msg_tick]
    mov rsi, msg_tick_len
    syscall

    ; Artificial CPU load between two calls, to leave the timer
    ; time to trigger a preemption.
    mov rcx, 3000000
.spin:
    dec rcx
    jnz .spin

    cmp r15, 5
    jl .loop

    ; --- attempt at a forbidden access ---
    ; Reading a kernel address from ring 3: the USER bit being absent
    ; from the page tables, the CPU must raise a page fault.
    ; It is the proof that the isolation really works.
    mov rax, SYS_DEBUG_PRINT
    lea rdi, [rel msg_violation]
    mov rsi, msg_violation_len
    syscall

    mov rax, 0xFFFFFFFF80000000
    mov rbx, [rax]              ; must cause a page fault

    ; --- never reached if the isolation is correct ---
    mov rax, SYS_DEBUG_PRINT
    lea rdi, [rel msg_leak]
    mov rsi, msg_leak_len
    syscall

.exit:
    mov rax, SYS_PROC_KILL
    xor rdi, rdi                ; 0 = oneself
    syscall

.halt:
    jmp .halt

; ------------------------------------------------------------
msg_hello:      db "programme ring 3 demarre"
msg_hello_len   equ $ - msg_hello

msg_tick:       db "iteration en espace utilisateur"
msg_tick_len    equ $ - msg_tick

msg_violation:  db "tentative de lecture d'une adresse kernel..."
msg_violation_len equ $ - msg_violation

msg_leak:       db "ALERTE : l'isolation a echoue"
msg_leak_len    equ $ - msg_leak

align 16
user_code_end:

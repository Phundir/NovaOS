; ============================================================
;  Nova OS -- Ring 3 test program for private address spaces (11.3)
;
;  Position-independent: vm::selftest copies these bytes into a fresh
;  address space at an address of its choosing and starts one of the
;  entry points below, with its arguments in RSI and RDX. Nova system
;  calls only: the Linux subsystem is not what is under test here.
;
;  Every mode ends with SYS_PROC_EXIT, code 0 on success, the number of
;  the failed check otherwise -- except those whose point is to fault or
;  never to end.
; ============================================================

section .rodata
bits 64
default rel

global vmtest_start
global vmtest_end
global vmtest_zero
global vmtest_isolation
global vmtest_spin
global vmtest_null
global vmtest_write_ro
global vmtest_sse

SYS_PROC_YIELD  equ 0x0208
SYS_PROC_EXIT   equ 0x020B

align 16
vmtest_start:

; ------------------------------------------------------------
;  exit(code in RDI)
; ------------------------------------------------------------
exit:
    mov eax, SYS_PROC_EXIT
    syscall
    ud2

yield:
    push rcx
    push r11
    mov eax, SYS_PROC_YIELD
    syscall
    pop r11
    pop rcx
    ret

; ------------------------------------------------------------
;  vmtest_zero(RSI = base of an anonymous RW region, RDX = pages)
;  Every page must read zero before its first write (demand-zero), then
;  hold what was written into it.
; ------------------------------------------------------------
vmtest_zero:
    mov r8, rsi                 ; base
    mov r9, rdx                 ; pages
    xor rcx, rcx
.first_pass:
    cmp rcx, r9
    je .second
    mov rax, rcx
    shl rax, 12
    lea rbx, [r8 + rax]
    cmp qword [rbx], 0          ; never touched: must be zero
    jne .fail1
    cmp qword [rbx + 4088], 0
    jne .fail1
    mov rax, rcx
    imul rax, rax, 0x1234567
    mov [rbx], rax
    not rax
    mov [rbx + 4088], rax
    inc rcx
    jmp .first_pass
.second:
    xor rcx, rcx
.check:
    cmp rcx, r9
    je .ok
    mov rax, rcx
    shl rax, 12
    lea rbx, [r8 + rax]
    mov rax, rcx
    imul rax, rax, 0x1234567
    cmp [rbx], rax
    jne .fail2
    not rax
    cmp [rbx + 4088], rax
    jne .fail2
    inc rcx
    jmp .check
.ok:
    xor edi, edi
    jmp exit
.fail1:
    mov edi, 1
    jmp exit
.fail2:
    mov edi, 2
    jmp exit

; ------------------------------------------------------------
;  vmtest_isolation(RSI = address, RDX = value)
;  Two processes run this with the same address and different values.
;  With separate spaces, each keeps reading its own value.
; ------------------------------------------------------------
vmtest_isolation:
    mov r12, rsi
    mov r13, rdx
    mov [r12], r13
    mov r14, 200
.loop:
    call yield
    cmp [r12], r13
    jne .bad
    dec r14
    jnz .loop
    xor edi, edi
    jmp exit
.bad:
    mov edi, 3
    jmp exit

; ------------------------------------------------------------
;  vmtest_spin: computes forever, no system call. Only preemption by the
;  timer lets anything else run on its core; the self-test ends it.
; ------------------------------------------------------------
vmtest_spin:
    xor eax, eax
.forever:
    inc rax
    jmp .forever

; ------------------------------------------------------------
;  vmtest_null: reads address 0. Must end this task, and only it.
; ------------------------------------------------------------
vmtest_null:
    xor eax, eax
    mov rax, [rax]
    mov edi, 4                  ; not reached
    jmp exit

; ------------------------------------------------------------
;  vmtest_write_ro(RSI = address in a read-only region)
; ------------------------------------------------------------
vmtest_write_ro:
    mov rax, [rsi]              ; reading is allowed
    mov qword [rsi], 1          ; writing must fault
    mov edi, 5
    jmp exit

; ------------------------------------------------------------
;  vmtest_sse(RDX = pattern): SSE in ring 3, across switches. Loads the
;  pattern into four XMM registers, yields two hundred times, checks.
; ------------------------------------------------------------
vmtest_sse:
    mov r12, rdx
    movq xmm0, r12
    lea rax, [r12 + 1]
    movq xmm1, rax
    lea rax, [r12 + 2]
    movq xmm2, rax
    lea rax, [r12 + 3]
    movq xmm15, rax
    mov r14, 200
.loop:
    call yield
    movq rax, xmm0
    cmp rax, r12
    jne .bad
    movq rax, xmm1
    lea rbx, [r12 + 1]
    cmp rax, rbx
    jne .bad
    movq rax, xmm2
    lea rbx, [r12 + 2]
    cmp rax, rbx
    jne .bad
    movq rax, xmm15
    lea rbx, [r12 + 3]
    cmp rax, rbx
    jne .bad
    dec r14
    jnz .loop
    xor edi, edi
    jmp exit
.bad:
    mov edi, 6
    jmp exit

vmtest_end:

; ============================================================
;  Nova OS -- User-space demonstration program
;
;  This code runs in ring 3, in a page marked USER. It has access to
;  nothing but its own pages and the system calls: any attempt to reach
;  the kernel would produce a page fault.
;
;  It is written position-independent -- relative jumps and RIP-relative
;  addressing only -- although the section is linked at 0x400000, the
;  address the kernel copies it to.
; ============================================================

section .user
bits 64

global user_program_start
global user_program_end

user_program_start:
    ; --- SYS_PROC_SELF: what is our PID? ---
    mov rax, 0x020A
    syscall
    mov r15, rax

    xor r14, r14                        ; lap counter

.loop:
    ; --- SYS_DEBUG_PRINT: writing to the console ---
    mov rax, 0x0001
    lea rdi, [rel msg]
    mov rsi, msg_len
    syscall

    ; --- SYS_TIME_TICKS: reading the system clock ---
    mov rax, 0x0701
    syscall

    ; --- SYS_PROC_YIELD: voluntary CPU yield ---
    mov rax, 0x0208
    syscall

    inc r14
    cmp r14, 5
    jb .loop

    ; --- SYS_IPC_SEND: message addressed to oneself ---
    ; The recipient is the PID read at start-up (R15), and not a
    ; hard-coded number: the order in which the tasks are created depends
    ; on the number of cores and of binaries loaded, and a fixed PID would
    ; name anything at all. Resolving a service by its name will come with
    ; SYS_IPC_OPEN, phase 5.5.
    mov rax, 0x0401
    mov rdi, r15
    lea rsi, [rel bye]
    mov rdx, bye_len
    mov r10, 42
    syscall

    ; --- SYS_FS_DISK_LIST: how many volumes are mounted? ---
    ; The buffer is taken from the stack: the kernel checks it really is
    ; in user space and writable before writing anything into it. The
    ; return is the number of volumes, not a number of bytes.
    sub rsp, 256
    mov rax, 0x0120
    mov rdi, rsp
    mov rsi, 256
    syscall
    add rsp, 256

    test rax, rax
    jle .no_disk

    ; --- SYS_FS_DISK_MOUNT: does the boot disk answer? ---
    mov rax, 0x0121
    mov rdi, 0x2F                       ; '/'
    syscall
    test rax, rax
    jle .no_disk

    mov rax, 0x0001
    lea rdi, [rel disks]
    mov rsi, disks_len
    syscall

.no_disk:

    ; ========================================================
    ;  Phase 5 -- the system API seen from ring 3
    ; ========================================================

    ; --- SYS_MEM_ALLOC: eight kibibytes of heap ---
    ; The kernel returns an address in its user arena. The page is marked
    ; USER and writable: writing it then reading it back proves the
    ; mapping is real, and not an address returned at random.
    mov rax, 0x0301
    mov rdi, 8192
    xor rsi, rsi
    syscall
    test rax, rax
    js .no_mem
    mov r13, rax

    mov rbx, 0x4E4F5641564F4E41         ; "NOVA" in two words
    mov [r13], rbx
    mov [r13 + 4088], rbx
    cmp qword [r13], rbx
    jne .no_mem
    cmp qword [r13 + 4088], rbx
    jne .no_mem

    ; --- SYS_MEM_FREE: and everything is given back ---
    mov rax, 0x0302
    mov rdi, r13
    syscall
    test rax, rax
    js .no_mem

    mov rax, 0x0001
    lea rdi, [rel mem_ok]
    mov rsi, mem_ok_len
    syscall
.no_mem:

    ; --- SYS_TIME_NOW: the wall clock, if it exists ---
    mov rax, 0x0703
    syscall
    test rax, rax
    js .no_clock
    mov rax, 0x0001
    lea rdi, [rel clock_ok]
    mov rsi, clock_ok_len
    syscall
.no_clock:

    ; --- SYS_USER_SELF: which account are we running under? ---
    ; The identity is derived from the process by the kernel: the program
    ; does not declare it, it reads it.
    sub rsp, 128
    mov rax, 0x0501
    mov rdi, rsp
    mov rsi, 128
    syscall
    add rsp, 128

    ; --- structured error: a deliberately unknown number ---
    ; Then SYS_ERROR_LAST, which must return the name, the message and
    ; the context RAX alone could not carry.
    mov rax, 0x09FF
    syscall

    sub rsp, 256
    mov rax, 0x0002
    mov rdi, rsp
    mov rsi, 256
    syscall
    add rsp, 256
    test rax, rax
    jle .no_error

    mov rax, 0x0001
    lea rdi, [rel err_ok]
    mov rsi, err_ok_len
    syscall
.no_error:

    ; --- attempt at a forbidden access: must produce a page fault ---
    ; Uncomment to check the ring 3 / ring 0 isolation.
    ; mov rax, 0xFFFFFFFF80000000
    ; mov rbx, [rax]

    ; --- SYS_PROC_EXIT: termination with a recognisable code ---
    ; KILL(0) terminated without saying anything; the exit code is what
    ; SYS_PROC_WAIT will return to the parent.
    mov rax, 0x020B
    mov rdi, 7
    syscall

.hang:
    jmp .hang

msg:        db "[ring3] appel systeme depuis l'espace utilisateur"
msg_len     equ $ - msg

bye:        db "la tache utilisateur a termine son travail"
bye_len     equ $ - bye

disks:      db "[ring3] SYS_FS_DISK_LIST et DISK_MOUNT repondent"
disks_len   equ $ - disks

mem_ok:     db "[ring3] SYS_MEM_ALLOC, ecriture, relecture et FREE"
mem_ok_len  equ $ - mem_ok

clock_ok:   db "[ring3] SYS_TIME_NOW rend l'heure murale"
clock_ok_len equ $ - clock_ok

err_ok:     db "[ring3] SYS_ERROR_LAST rend l'erreur structuree"
err_ok_len  equ $ - err_ok

align 16
user_program_end:

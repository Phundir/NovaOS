; ============================================================
;  Nova OS -- Startup trampoline of the secondary cores
;
;  An AP answering a SIPI starts in 16-bit real mode, with
;  CS = vector << 8 and IP = 0. So this code must live under 1 MiB,
;  at a 4 KiB aligned address.
;
;  The linker script gives it VMA 0x8000: the internal labels therefore
;  resolve directly to the final physical addresses, once the block has
;  been copied to that location by the BSP.
;
;  Path taken: 16-bit real -> 64-bit long mode in a single transition,
;  by setting PE and PG in the same write to CR0.
; ============================================================

section .trampoline
bits 16

global trampoline_start
global trampoline_end
global tramp_cr3
global tramp_stack
global tramp_entry
global tramp_flag

trampoline_start:
    cli
    cld

    ; The segment registers are undetermined: they are zeroed so that
    ; the absolute addresses 0x8000+ are read correctly.
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax

    ; PAE -- demanded by long mode.
    mov eax, cr4
    or  eax, 1 << 5
    mov cr4, eax

    ; Page hierarchy supplied by the BSP: the same as its own.
    mov eax, [tramp_cr3]
    mov cr3, eax

    ; EFER.LME
    mov ecx, 0xC0000080
    rdmsr
    or  eax, 1 << 8
    wrmsr

    lgdt [tramp_gdt_ptr]

    ; PE and PG set at the same time: with LME already active, the CPU
    ; switches straight to long mode. The far jump that follows loads a
    ; 64-bit descriptor and completes the transition.
    mov eax, cr0
    or  eax, (1 << 31) | 1
    mov cr0, eax

    jmp dword 0x08:tramp_long_mode

; ------------------------------------------------------------
bits 64
; The trampoline runs at the physical address it is linked for (0x8000):
; its labels are therefore valid absolute addresses, and that is the
; addressing mode we want. NASM refuses to guess it in 64 bits and warns
; until it is declared.
default abs
tramp_long_mode:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov fs, ax
    mov gs, ax

    ; Stack private to this core, written by the BSP before the SIPI.
    mov rsp, [tramp_stack]

    ; Signals to the BSP that the core is alive: it waits for this flag
    ; before moving to the next, which serialises the startups.
    mov byte [tramp_flag], 1

    ; Jump to the Rust code, in higher-half.
    mov rax, [tramp_entry]
    jmp rax

.halt:
    hlt
    jmp .halt

; ------------------------------------------------------------
;  Minimal GDT: for the duration of the transition only. Each core
;  will load its own final GDT from Rust.
; ------------------------------------------------------------
align 8
tramp_gdt:
    dq 0                                            ; null descriptor
    dq (1 << 43) | (1 << 44) | (1 << 47) | (1 << 53) ; 0x08: 64-bit code
    dq (1 << 41) | (1 << 44) | (1 << 47)             ; 0x10: data
tramp_gdt_ptr:
    dw $ - tramp_gdt - 1
    dq tramp_gdt

; ------------------------------------------------------------
;  Parameter block, filled in by the BSP before each SIPI.
; ------------------------------------------------------------
align 8
tramp_cr3:      dq 0
tramp_stack:    dq 0
tramp_entry:    dq 0
tramp_flag:     db 0

align 16
trampoline_end:

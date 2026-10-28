; ============================================================
;  Nova OS -- Low-level entry point
;  Multiboot2 -> 32-bit protected mode -> long mode -> higher-half
; ============================================================

MB2_MAGIC       equ 0xE85250D6
MB2_ARCH_I386   equ 0
MB2_BOOTMAGIC   equ 0x36D76289

; Virtual base of the kernel: last 2 gibibytes of the address space.
KERNEL_VMA      equ 0xFFFFFFFF80000000
; Window on physical memory: virtual = physical + PHYS_OFFSET.
PHYS_OFFSET     equ 0xFFFF800000000000

; Table indexes derived from the constants above.
PML4_KERNEL     equ 511         ; (KERNEL_VMA  >> 39) & 0x1FF
PDPT_KERNEL     equ 510         ; (KERNEL_VMA  >> 30) & 0x1FF
PML4_PHYSMAP    equ 256         ; (PHYS_OFFSET >> 39) & 0x1FF

; ------------------------------------------------------------
;  Multiboot2 header
; ------------------------------------------------------------
section .multiboot_header
align 8
header_start:
    dd  MB2_MAGIC
    dd  MB2_ARCH_I386
    dd  header_end - header_start
    dd  0x100000000 - (MB2_MAGIC + MB2_ARCH_I386 + (header_end - header_start))

    ; --- 7.1.1: request for a graphics mode (tag 5) ---
    ;
    ; Without this tag, GRUB leaves the card in 80x25 text mode and emits
    ; no tag 8: the kernel then has no framebuffer address at all. With
    ; it, GRUB programs the mode through VBE and describes the result.
    ;
    ; Bit 0 of the flags means "optional": if the firmware cannot produce
    ; this mode, the boot carries on in text mode rather than failing. So
    ; the kernel must handle both cases -- which is what graphics::init
    ; does, falling back on the VGA writer.
    ;
    ; The dimensions are a wish, not a constraint: GRUB chooses the
    ; closest available mode and returns its real values in tag 8. Nothing
    ; in the kernel assumes 1024x768.
    align 8
fb_tag_start:
    dw  5                               ; type: framebuffer
    dw  1                               ; flags: optional
    dd  fb_tag_end - fb_tag_start
    dd  1024                            ; wanted width
    dd  768                             ; wanted height
    dd  32                              ; wanted bits per pixel
fb_tag_end:

    align 8
    dw  0
    dw  0
    dd  8
header_end:

; ============================================================
;  Section .boot -- identity mapped, low physical addresses.
;  Holds the 32-bit code, the GDT, the page tables and the
;  temporary stack: everything that must be addressable before
;  the higher-half exists.
; ============================================================
section .boot progbits alloc exec write align=4096
bits 32
default abs
global _start

_start:
    mov esp, boot_stack_top

    call check_multiboot

    ; EBX points at the Multiboot2 info structure (physical address).
    ; It is filed in EDI: first integer argument of the System V ABI, so
    ; it will arrive in RDI for kernel_main.
    mov edi, ebx

    call check_cpuid
    call check_long_mode

    call setup_page_tables
    call enable_paging

    lgdt [gdt64.pointer]
    jmp  gdt64.code:long_mode_low

; ------------------------------------------------------------
check_multiboot:
    cmp eax, MB2_BOOTMAGIC
    jne .fail
    ret
.fail:
    mov al, '0'
    jmp error

; ------------------------------------------------------------
check_cpuid:
    pushfd
    pop eax
    mov ecx, eax
    xor eax, 1 << 21
    push eax
    popfd
    pushfd
    pop eax
    push ecx
    popfd
    cmp eax, ecx
    je .fail
    ret
.fail:
    mov al, '1'
    jmp error

; ------------------------------------------------------------
check_long_mode:
    mov eax, 0x80000000
    cpuid
    cmp eax, 0x80000001
    jb .fail

    mov eax, 0x80000001
    cpuid
    test edx, 1 << 29
    jz .fail
    ret
.fail:
    mov al, '2'
    jmp error

; ------------------------------------------------------------
;  2.2.2 -- 4-level page tables.
;
;  p2_table covers the first physical gibibyte in 2 MiB pages.
;  It is referenced through three different paths, which gives
;  three views of the same gibibyte:
;
;    PML4[0]   -> p3_table      -> p2_table   : identity (temporary)
;    PML4[256] -> p3_table      -> p2_table   : physical window
;    PML4[511] -> p3_table_high -> p2_table   : kernel higher-half
; ------------------------------------------------------------
setup_page_tables:
    ; --- identity: indispensable while low code runs ---
    mov eax, p3_table
    or  eax, 0b11                       ; present + write
    mov [p4_table], eax

    ; --- window on physical memory ---
    mov eax, p3_table
    or  eax, 0b11
    mov [p4_table + PML4_PHYSMAP * 8], eax

    ; --- kernel in higher-half ---
    mov eax, p3_table_high
    or  eax, 0b11
    mov [p4_table + PML4_KERNEL * 8], eax

    ; The kernel stays on 2 MiB pages through p2_table.
    mov eax, p2_table
    or  eax, 0b11
    mov [p3_table_high + PDPT_KERNEL * 8], eax

    ; p2_table covers the first gibibyte in 2 MiB pages.
    mov ecx, 0
.map_p2:
    mov eax, 0x200000
    mul ecx
    or  eax, 0b10000011
    mov [p2_table + ecx * 8], eax

    inc ecx
    cmp ecx, 512
    jne .map_p2

    ; --- physical window: 4 pages of 1 GiB ---
    ; Covers the first 4 GiB, which includes the LAPIC (0xFEE00000)
    ; and the I/O APIC (0xFEC00000). RAM above 4 GiB is added by
    ; memory::extend_window once the memory map is read.
    mov ecx, 0
.map_p3:
    mov eax, 0x40000000
    mul ecx
    or  eax, 0b10000011
    mov [p3_table + ecx * 8], eax

    inc ecx
    cmp ecx, 4
    jne .map_p3
    ret

; ------------------------------------------------------------
;  2.2.4 -- PAE, long mode, paging
; ------------------------------------------------------------
enable_paging:
    mov eax, p4_table
    mov cr3, eax

    mov eax, cr4
    or  eax, 1 << 5                     ; PAE
    mov cr4, eax

    mov ecx, 0xC0000080                 ; MSR EFER
    rdmsr
    or  eax, 1 << 8                     ; LME
    wrmsr

    mov eax, cr0
    or  eax, 1 << 31                    ; PG
    mov cr0, eax
    ret

; ------------------------------------------------------------
error:
    mov dword [0xb8000], 0x4f524f45
    mov dword [0xb8004], 0x4f3a4f52
    mov dword [0xb8008], 0x4f204f20
    mov byte  [0xb800a], al
    hlt
    jmp $

; ------------------------------------------------------------
;  64-bit trampoline, still at a low address.
;  The 32-bit far jump can only target a 32-bit offset:
;  hence this relay to reach the higher-half.
; ------------------------------------------------------------
bits 64
long_mode_low:
    mov ax, 0
    mov ss, ax
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax

    ; Visual marker: "OK" in green through the identity mapping.
    mov word [0xb8000], 0x2f4f
    mov word [0xb8002], 0x2f4b

    mov rax, higher_half_entry
    jmp rax

; ------------------------------------------------------------
;  Boot data
; ------------------------------------------------------------
align 4096
p4_table:
    times 4096 db 0
p3_table:
    times 4096 db 0
p3_table_high:
    times 4096 db 0
p2_table:
    times 4096 db 0

align 16
boot_stack_bottom:
    times 16384 db 0
boot_stack_top:

align 8
gdt64:
    dq 0
.code: equ $ - gdt64
    dq (1 << 43) | (1 << 44) | (1 << 47) | (1 << 53)
.pointer:
    dw $ - gdt64 - 1
    dq gdt64

; ============================================================
;  Section .text -- linked in higher-half
; ============================================================
section .text
bits 64
extern kernel_main
global higher_half_entry

higher_half_entry:
    ; Final kernel stack, in higher-half.
    mov rsp, kernel_stack_top

    ; The identity mapping has no reason to exist any more: PML4[0] is
    ; cleared. The write goes through the physical window, still valid.
    mov rax, PHYS_OFFSET
    add rax, p4_table
    mov qword [rax], 0

    ; Reloading CR3 empties the TLB and makes the removal effective.
    mov rax, cr3
    mov cr3, rax

    ; RDI still holds the physical address of the Multiboot2 structure.
    call kernel_main

.halt:
    hlt
    jmp .halt

; ============================================================
section .bss
alignb 16
kernel_stack_bottom:
    resb 65536                          ; 64 KiB
kernel_stack_top:

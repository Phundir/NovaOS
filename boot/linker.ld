/* Nova OS -- linker script (higher-half)

   .boot stays at its physical load address: the 32-bit code can only
   address 4 GiB, and the identity mapping exists only there.

   .trampoline and .user receive low VMAs while being loaded with the
   rest of the kernel: their labels therefore resolve to the final
   addresses, and the kernel copies those blocks at run time. */

ENTRY(_start)

KERNEL_VMA = 0xFFFFFFFF80000000;
TRAMPOLINE_VMA = 0x8000;
USER_VMA = 0x400000;

SECTIONS {
    . = 1M;

    __boot_start = .;

    .boot ALIGN(4K) :
    {
        KEEP(*(.multiboot_header))
        *(.boot)
        *(.boot.*)
    }

    . = ALIGN(4K);

    /* AP startup code: linked for 0x8000, loaded here. */
    __trampoline_load = .;
    .trampoline TRAMPOLINE_VMA : AT(__trampoline_load)
    {
        *(.trampoline)
    }
    . = __trampoline_load + SIZEOF(.trampoline);

    . = ALIGN(4K);

    /* User program: linked for 0x400000, loaded here.
       The kernel copies it into a page marked USER at launch. */
    __user_load = .;
    .user USER_VMA : AT(__user_load)
    {
        *(.user)
    }
    . = __user_load + SIZEOF(.user);

    . = ALIGN(4K);
    __boot_end = .;

    /* Bascule en higher-half */
    . += KERNEL_VMA;

    __kernel_start = .;

    .text ALIGN(4K) : AT(ADDR(.text) - KERNEL_VMA)
    {
        *(.text .text.*)
    }
    __text_end = .;

    .rodata ALIGN(4K) : AT(ADDR(.rodata) - KERNEL_VMA)
    {
        *(.rodata .rodata.*)
    }

    .data ALIGN(4K) : AT(ADDR(.data) - KERNEL_VMA)
    {
        *(.data .data.*)
    }

    .bss ALIGN(4K) : AT(ADDR(.bss) - KERNEL_VMA)
    {
        *(COMMON)
        *(.bss .bss.*)
    }

    . = ALIGN(4K);
    __kernel_end = .;

    /DISCARD/ :
    {
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.comment)
        *(.note.*)
    }
}

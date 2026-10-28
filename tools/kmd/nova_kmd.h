/*
 * nova_kmd.h -- writing a GPU driver module for Nova (spec 3.6, 8.5).
 *
 * A vendor's GPU driver is a kernel module: a position-independent ELF
 * shared object, wrapped in a signed .kmd container (tools/nova-kmd.py),
 * installed in /sys/kernel/modules/gpu/ and loaded at boot, before the
 * compositor. Its signature is verified before any of its instructions
 * runs; it then runs in ring 0, and the signature is its only barrier.
 *
 * What the module sees of the kernel is `struct nova_kmd_kernel` and
 * nothing else: no symbol of the kernel is resolved into it. What the
 * kernel sees of the module is the table `nova_kmd_init` returns. Both
 * carry the interface version; a module of another version is refused.
 *
 * Building (gcc or clang, x86-64):
 *   -ffreestanding -fPIC -fvisibility=hidden -fno-plt -nostdlib -shared
 *   -mno-red-zone -mgeneral-regs-only -fno-stack-protector -O2
 *   -Wl,-e,nova_kmd_init -Wl,-z,max-page-size=4096
 * The kernel is built without SSE and interrupts may land on its stack at
 * any instruction: no floating point, no red zone. Only R_X86_64_RELATIVE
 * relocations are applied; a module needing any other is refused.
 *
 * Every object a program creates reaches the module already validated by
 * the core: handles resolved to the module's own ids, ranges inside their
 * objects, formats matched (see the comments of struct nova_kmd_command).
 * Floating point values cross as the bits of 32-bit floats.
 */
#ifndef NOVA_KMD_H
#define NOVA_KMD_H

#include <stdint.h>
#include <stddef.h>

#define NOVA_KMD_INTERFACE 0x00010000u   /* equals the core's INTERFACE_VERSION */

/* Results: 0, or a negative value. */
#define NOVA_KMD_OK           0
#define NOVA_KMD_UNSUPPORTED (-1)
#define NOVA_KMD_NO_MEMORY   (-2)
#define NOVA_KMD_LOST        (-3)
#define NOVA_KMD_NO_DEVICE   (-4)
#define NOVA_KMD_FAILED      (-5)

/* ---- what the kernel offers ---- */
struct nova_kmd_kernel {
    uint32_t interface;                 /* NOVA_KMD_INTERFACE */
    uint32_t size;                      /* sizeof, for later additions */
    void (*log)(const char *ascii_line);
    void *(*alloc)(size_t size);        /* kernel heap, 16-byte aligned */
    void (*free)(void *pointer, size_t size);
    /* Physically contiguous zeroed pages, and their kernel address. */
    uint64_t (*pages_alloc)(size_t count, void **virtual_address);
    void (*pages_free)(uint64_t physical, size_t count);
    /* Device registers, mapped uncached. */
    void *(*mmio_map)(uint64_t physical, size_t size);
    /* PCI: bus/device/function packed as bus << 8 | device << 3 | function. */
    int (*pci_find)(uint16_t vendor, uint16_t device, uint32_t index, uint32_t *bdf);
    uint32_t (*pci_read32)(uint32_t bdf, uint32_t offset);
    void (*pci_write32)(uint32_t bdf, uint32_t offset, uint32_t value);
    uint64_t (*pci_bar)(uint32_t bdf, uint32_t index);
    uint64_t (*time_ns)(void);
    void (*sleep_ms)(uint32_t milliseconds);
};

/* ---- descriptions, as in src/gpu/backend.rs ---- */
struct nova_kmd_description {
    char name[64];
    uint32_t vendor_id, device_id;
    uint32_t max_image_size, max_push_constants;
    uint64_t local_memory, visible_memory;
    uint32_t formats;                   /* 1 << NOVA_FORMAT_x */
    uint32_t sharing;                   /* images can be shared */
    uint64_t buffer_alignment;
};

struct nova_kmd_window { uint64_t physical, size, cache_flags; };

struct nova_kmd_image_desc { uint32_t format, width, height, usage, shareable; };
struct nova_kmd_buffer_desc { uint64_t size; uint32_t usage; };
struct nova_kmd_sampler_desc { uint32_t linear, address; };
struct nova_kmd_layout_binding { uint32_t binding, kind, stages; };  /* kind: 1 image, 2 uniform, 3 storage */
struct nova_kmd_layout_desc { uint32_t count, push_constants; struct nova_kmd_layout_binding bindings[16]; };
struct nova_kmd_pipeline_desc {
    uint64_t layout, vertex, fragment;  /* the module's own ids */
    uint32_t topology, cull, front_ccw, blend;
    uint32_t color_format, depth_format; /* depth 0: none */
    uint32_t depth_test, depth_write;
    uint32_t binding_count;
    struct { uint32_t stride, per_instance; } bindings[4];
    uint32_t attribute_count;
    struct { uint32_t location, binding, format, offset; } attributes[8];
};

/* Object kinds of destroy(). */
#define NOVA_KMD_BUFFER   1
#define NOVA_KMD_IMAGE    2
#define NOVA_KMD_SAMPLER  3
#define NOVA_KMD_SHADER   4
#define NOVA_KMD_LAYOUT   5
#define NOVA_KMD_PIPELINE 6

/* An image as a command uses it. first_use: its contents are undefined,
 * bring it to the state images are kept in. */
struct nova_kmd_image_ref { uint64_t id; uint32_t format, width, height, first_use; };
struct nova_kmd_rect { int32_t x, y; uint32_t width, height; };

/* Opcodes of struct nova_kmd_command: those of the Nova command buffer
 * (NOVA_CMD_x in nova_gfx.h), plus 20 for the resources the core pushes
 * before a draw. */
#define NOVA_KMD_PUSH_RESOURCES 20

struct nova_kmd_write {                 /* one resource of PUSH_RESOURCES */
    uint32_t binding, kind;             /* kind: 1 image, 2 uniform, 3 storage */
    uint64_t object, sampler;           /* image and sampler, or buffer */
    uint64_t offset, range;
};

struct nova_kmd_command {
    uint32_t opcode;
    uint32_t pad;
    union {
        struct { struct nova_kmd_image_ref color; uint32_t load; uint32_t clear[4];
                 uint32_t has_depth; struct nova_kmd_image_ref depth; uint32_t depth_load, depth_clear;
                 struct nova_kmd_rect area; } begin_render;
        struct { uint64_t pipeline; } bind_pipeline;
        struct { uint32_t slot, pad; uint64_t buffer, offset; } bind_vertex;
        struct { uint64_t buffer, offset; uint32_t wide; } bind_index;
        struct { uint64_t layout; uint32_t count; const struct nova_kmd_write *writes; } push_resources;
        struct { uint32_t x, y, width, height, min_depth, max_depth; } viewport;
        struct nova_kmd_rect scissor;
        struct { uint64_t layout; uint32_t offset, size; const uint8_t *data; } push_constants;
        struct { uint32_t count, instances, first, first_instance; int32_t vertex_offset; } draw;
        struct { uint64_t src, dst, src_offset, dst_offset, size; } copy_buffer;
        struct { uint64_t buffer, offset; uint32_t row_pixels; struct nova_kmd_image_ref image;
                 struct nova_kmd_rect rect; } buffer_image;   /* both directions */
        struct { struct nova_kmd_image_ref src, dst; int32_t src_x, src_y, dst_x, dst_y;
                 uint32_t width, height; } copy_image;
        struct { struct nova_kmd_image_ref src, dst; struct nova_kmd_rect src_rect, dst_rect;
                 uint32_t linear; } blit;
        struct { struct nova_kmd_image_ref image; uint32_t color[4]; } clear_image;
        struct { uint64_t buffer, offset, size; uint32_t value; } fill_buffer;
        struct { uint64_t buffer, offset, size; const uint8_t *data; } update_buffer;
    } u;
};

/* Work states of poll(). */
#define NOVA_KMD_PENDING 0
#define NOVA_KMD_DONE    1
#define NOVA_KMD_FAILED_WORK 2

/* ---- what the module offers ---- */
struct nova_kmd_gpu {
    uint32_t interface;                 /* NOVA_KMD_INTERFACE */
    uint32_t size;                      /* sizeof (struct nova_kmd_gpu) */
    const char *name;                   /* ASCII, for the log */
    /* Is the module's card on this machine? 0 yes, NOVA_KMD_NO_DEVICE no:
     * the module is then unloaded. */
    int (*probe)(void);
    int (*describe)(struct nova_kmd_description *out);
    int (*open)(const char *name, void **context);
    int (*alloc)(void *ctx, uint64_t size, uint32_t kind, uint64_t *id, uint64_t *real_size);
    void (*free)(void *ctx, uint64_t id);
    int (*map)(void *ctx, uint64_t id, struct nova_kmd_window *out);
    void (*unmap)(void *ctx, uint64_t id);
    int (*create_buffer)(void *ctx, const struct nova_kmd_buffer_desc *d, uint64_t memory, uint64_t offset, uint64_t *id);
    int (*create_image)(void *ctx, const struct nova_kmd_image_desc *d, uint64_t memory, uint64_t offset,
                        int placed, uint64_t *id, uint64_t *own_bytes);
    int (*create_sampler)(void *ctx, const struct nova_kmd_sampler_desc *d, uint64_t *id);
    int (*create_shader)(void *ctx, const uint32_t *spirv, uint32_t words, uint64_t *id);
    int (*create_layout)(void *ctx, const struct nova_kmd_layout_desc *d, uint64_t *id);
    int (*create_pipeline)(void *ctx, const struct nova_kmd_pipeline_desc *d, uint64_t *id);
    void (*destroy)(void *ctx, uint32_t kind, uint64_t id);
    int (*submit)(void *ctx, const struct nova_kmd_command *commands, uint32_t count, uint64_t *work);
    int (*poll)(void *ctx, uint64_t work);
    int (*export_image)(void *ctx, uint64_t image, uint32_t *name);
    int (*import_image)(void *ctx, uint32_t name, const struct nova_kmd_image_desc *d, uint64_t *id);
    void (*close)(void *ctx);
};

/* The module's one entry point. Returns its table, or NULL to decline. */
const struct nova_kmd_gpu *nova_kmd_init(const struct nova_kmd_kernel *kernel);

#endif /* NOVA_KMD_H */

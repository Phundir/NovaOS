/*
 * nova_gfx.h -- the Nova graphics interface (spec 6.8, domain 0C).
 *
 * The C side of `SYS_GPU_*`, header only: every call is a `syscall`
 * instruction with a Nova number, for any Nova program -- one linked with
 * the Nova C library as well as a bare one. No library of the host's
 * graphics stack is involved: this header, the kernel, and the GPU.
 *
 * Conventions
 *   - Every call returns a value >= 0, or a negative errno: -ENODEV no
 *     GPU, -EBADF a handle that is not the caller's or not of that kind,
 *     -EINVAL arguments or a command buffer the kernel refused, -ENOMEM,
 *     -EDQUOT the process's GPU quota, -ENOTRECOVERABLE the context is
 *     lost (the watchdog, or the GPU refused the work), -ETIMEDOUT.
 *     MEM_MAP returns an address: an error is -1 to -4095 only.
 *   - Handles are 32-bit, never 0, the process's own.
 *   - Colours, depths and viewports are 32-bit floats: the kernel copies
 *     their bits and never computes with them.
 *
 * The layouts below are read by the kernel field by field
 * (src/gpu/api.rs, src/gpu/cmd.rs); the two move together.
 */
#ifndef NOVA_GFX_H
#define NOVA_GFX_H

#include <stddef.h>
#include <stdint.h>
#include <string.h>

/* ---- call numbers ---- */
#define NOVA_GPU_INFO            0x0C01
#define NOVA_GPU_CONTEXT_CREATE  0x0C02
#define NOVA_GPU_CONTEXT_DESTROY 0x0C03
#define NOVA_GPU_MEM_ALLOC       0x0C04
#define NOVA_GPU_MEM_FREE        0x0C05
#define NOVA_GPU_MEM_MAP         0x0C06
#define NOVA_GPU_MEM_UNMAP       0x0C07
#define NOVA_GPU_OBJECT_CREATE   0x0C08
#define NOVA_GPU_OBJECT_DESTROY  0x0C09
#define NOVA_GPU_SUBMIT          0x0C0A
#define NOVA_GPU_FENCE_CREATE    0x0C0B
#define NOVA_GPU_FENCE_WAIT      0x0C0C
#define NOVA_GPU_FENCE_EXPORT    0x0C0D
#define NOVA_GPU_SHARE           0x0C0E
#define NOVA_GPU_IMPORT          0x0C0F
#define NOVA_GPU_PRESENT         0x0C10

/* ---- memory kinds (MEM_ALLOC) ---- */
#define NOVA_MEM_LOCAL    0   /* on the GPU, not mappable */
#define NOVA_MEM_VISIBLE  1   /* mappable, written by the CPU, read by the GPU */
#define NOVA_MEM_READBACK 2   /* mappable, cached: written by the GPU, read back */

/* ---- object kinds (OBJECT_CREATE) ---- */
#define NOVA_OBJ_BUFFER   1
#define NOVA_OBJ_IMAGE    2
#define NOVA_OBJ_SAMPLER  3
#define NOVA_OBJ_SHADER   4
#define NOVA_OBJ_LAYOUT   5
#define NOVA_OBJ_PIPELINE 6

/* ---- image formats ---- */
#define NOVA_FORMAT_BGRA8      1
#define NOVA_FORMAT_RGBA8      2
#define NOVA_FORMAT_R8         3
#define NOVA_FORMAT_RGBA16F    4
#define NOVA_FORMAT_DEPTH32F   5
#define NOVA_FORMAT_RGBA8_SRGB 6
#define NOVA_FORMAT_BGRA8_SRGB 7

/* ---- usages (copies are always allowed) ---- */
#define NOVA_IMAGE_RENDER   1
#define NOVA_IMAGE_SAMPLED  2
#define NOVA_IMAGE_DEPTH    4
#define NOVA_IMAGE_SHAREABLE 1   /* nova_image_desc.flags */
#define NOVA_BUFFER_VERTEX  1
#define NOVA_BUFFER_INDEX   2
#define NOVA_BUFFER_UNIFORM 4
#define NOVA_BUFFER_STORAGE 8

/* ---- shaders, bindings ---- */
#define NOVA_STAGE_VERTEX   1
#define NOVA_STAGE_FRAGMENT 2
#define NOVA_BIND_SAMPLED_IMAGE  1
#define NOVA_BIND_UNIFORM_BUFFER 2
#define NOVA_BIND_STORAGE_BUFFER 3

/* ---- pipeline state ---- */
#define NOVA_TOPOLOGY_TRIANGLES 0
#define NOVA_TOPOLOGY_STRIP     1
#define NOVA_TOPOLOGY_LINES     2
#define NOVA_TOPOLOGY_POINTS    3
#define NOVA_BLEND_NONE          0
#define NOVA_BLEND_PREMULTIPLIED 1
#define NOVA_BLEND_ALPHA         2
#define NOVA_BLEND_ADDITIVE      3
#define NOVA_VERTEX_FLOAT1 1
#define NOVA_VERTEX_FLOAT2 2
#define NOVA_VERTEX_FLOAT3 3
#define NOVA_VERTEX_FLOAT4 4
#define NOVA_VERTEX_UBYTE4_NORM 5
#define NOVA_VERTEX_UINT1  6

/* ---- load operations of a render pass ---- */
#define NOVA_LOAD_CLEAR     0
#define NOVA_LOAD_KEEP      1
#define NOVA_LOAD_DONT_CARE 2

/* ---- command buffer opcodes: header = opcode | words << 16 ---- */
#define NOVA_CMD_BEGIN_RENDER         1
#define NOVA_CMD_END_RENDER           2
#define NOVA_CMD_BIND_PIPELINE        3
#define NOVA_CMD_BIND_VERTEX          4
#define NOVA_CMD_BIND_INDEX           5
#define NOVA_CMD_BIND_RESOURCE        6
#define NOVA_CMD_VIEWPORT             7
#define NOVA_CMD_SCISSOR              8
#define NOVA_CMD_PUSH_CONSTANTS       9
#define NOVA_CMD_DRAW                 10
#define NOVA_CMD_DRAW_INDEXED         11
#define NOVA_CMD_COPY_BUFFER          12
#define NOVA_CMD_COPY_BUFFER_TO_IMAGE 13
#define NOVA_CMD_COPY_IMAGE_TO_BUFFER 14
#define NOVA_CMD_COPY_IMAGE           15
#define NOVA_CMD_BLIT_IMAGE           16
#define NOVA_CMD_CLEAR_IMAGE          17
#define NOVA_CMD_FILL_BUFFER          18
#define NOVA_CMD_UPDATE_BUFFER        19

#define NOVA_GPU_INTERFACE 0x00010000u
#define NOVA_WAIT_FOREVER  UINT64_MAX

/* ---- structures (little-endian, no padding) ---- */
struct nova_gpu_info {           /* 128 bytes */
    uint32_t size;
    uint32_t present;
    uint32_t interface;          /* NOVA_GPU_INTERFACE */
    uint32_t vendor_id;
    uint32_t device_id;
    uint32_t max_image_size;
    uint32_t max_push_constants;
    uint32_t formats;            /* bit (1 << NOVA_FORMAT_x) */
    uint64_t local_memory;
    uint64_t visible_memory;
    uint64_t quota;              /* GPU memory the process may hold */
    uint32_t flags;              /* 1: images can be shared */
    uint32_t reserved;
    char     device[64];
};

struct nova_buffer_desc {        /* 24 bytes */
    uint64_t size;
    uint32_t usage;
    uint32_t memory;             /* a memory handle */
    uint64_t offset;             /* multiple of 256 */
};

struct nova_image_desc {         /* 32 bytes */
    uint32_t format, width, height, usage;
    uint32_t memory;             /* 0: memory of its own, charged to the process */
    uint32_t flags;              /* NOVA_IMAGE_SHAREABLE */
    uint64_t offset;
};

struct nova_sampler_desc {       /* 8 bytes */
    uint32_t filter;             /* 0 nearest, 1 linear */
    uint32_t address;            /* 0 clamp, 1 repeat, 2 mirror */
};

struct nova_shader_desc {        /* 16 bytes */
    uint64_t code;               /* address of the SPIR-V */
    uint32_t size;               /* bytes, a multiple of 4 */
    uint32_t stage;              /* NOVA_STAGE_x */
};

struct nova_layout_desc {        /* 8 + 12 x count bytes */
    uint32_t count;              /* <= 16 */
    uint32_t push_constants;     /* bytes, multiple of 4 */
    struct { uint32_t binding, type, stages; } bindings[16];
};

struct nova_pipeline_desc {      /* 212 bytes */
    uint32_t layout, vertex, fragment;
    uint32_t topology, cull, front_ccw, blend;
    uint32_t color_format, depth_format, depth_test, depth_write;
    uint32_t binding_count;
    struct { uint32_t stride, per_instance; } bindings[4];
    uint32_t attribute_count;
    struct { uint32_t location, binding, format, offset; } attributes[8];
};

/* ---- the call ---- */
static inline long nova_gpu_call(long n, long a, long b, long c, long d, long e, long f)
{
    register long r10 __asm__("r10") = d;
    register long r8 __asm__("r8") = e;
    register long r9 __asm__("r9") = f;
    long ret;
    __asm__ volatile ("syscall"
                      : "=a"(ret)
                      : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9)
                      : "rcx", "r11", "memory");
    return ret;
}

static inline int nova_gpu_failed(long value) { return value < 0 && value >= -4095; }

static inline long nova_gpu_info(struct nova_gpu_info *info)
{ return nova_gpu_call(NOVA_GPU_INFO, (long)info, sizeof *info, 0, 0, 0, 0); }
static inline long nova_gpu_context_create(void)
{ return nova_gpu_call(NOVA_GPU_CONTEXT_CREATE, 0, 0, 0, 0, 0, 0); }
static inline long nova_gpu_context_destroy(uint32_t ctx)
{ return nova_gpu_call(NOVA_GPU_CONTEXT_DESTROY, ctx, 0, 0, 0, 0, 0); }
static inline long nova_gpu_mem_alloc(uint32_t ctx, uint64_t size, uint32_t kind)
{ return nova_gpu_call(NOVA_GPU_MEM_ALLOC, ctx, (long)size, kind, 0, 0, 0); }
static inline long nova_gpu_mem_free(uint32_t ctx, uint32_t mem)
{ return nova_gpu_call(NOVA_GPU_MEM_FREE, ctx, mem, 0, 0, 0, 0); }
static inline void *nova_gpu_mem_map(uint32_t ctx, uint32_t mem)
{ return (void *)nova_gpu_call(NOVA_GPU_MEM_MAP, ctx, mem, 0, 0, 0, 0); }
static inline long nova_gpu_mem_unmap(uint32_t ctx, uint32_t mem)
{ return nova_gpu_call(NOVA_GPU_MEM_UNMAP, ctx, mem, 0, 0, 0, 0); }
static inline long nova_gpu_create(uint32_t ctx, uint32_t kind, const void *desc, size_t size)
{ return nova_gpu_call(NOVA_GPU_OBJECT_CREATE, ctx, kind, (long)desc, (long)size, 0, 0); }
static inline long nova_gpu_destroy(uint32_t ctx, uint32_t handle)
{ return nova_gpu_call(NOVA_GPU_OBJECT_DESTROY, ctx, handle, 0, 0, 0, 0); }
static inline long nova_gpu_submit(uint32_t ctx, const uint32_t *words, size_t count, uint32_t signal,
                                   const uint32_t *waits, uint32_t wait_count)
{ return nova_gpu_call(NOVA_GPU_SUBMIT, ctx, (long)words, (long)(count * 4), signal, (long)waits, wait_count); }
static inline long nova_gpu_fence_create(uint32_t ctx)
{ return nova_gpu_call(NOVA_GPU_FENCE_CREATE, ctx, 0, 0, 0, 0, 0); }
static inline long nova_gpu_fence_wait(const uint32_t *fences, uint32_t count, uint64_t timeout_ns, int all)
{ return nova_gpu_call(NOVA_GPU_FENCE_WAIT, (long)fences, count, (long)timeout_ns, all, 0, 0); }
static inline long nova_gpu_fence_export(uint32_t fence)
{ return nova_gpu_call(NOVA_GPU_FENCE_EXPORT, fence, 0, 0, 0, 0, 0); }
static inline long nova_gpu_share(uint32_t ctx, uint32_t image)
{ return nova_gpu_call(NOVA_GPU_SHARE, ctx, image, 0, 0, 0, 0); }
static inline long nova_gpu_import(uint32_t ctx, uint64_t token)
{ return nova_gpu_call(NOVA_GPU_IMPORT, ctx, (long)token, 0, 0, 0, 0); }

/* ---- command buffers ---- */
struct nova_cmds {
    uint32_t *words;
    size_t count, capacity;
    int overflow;                /* set when a command did not fit */
};

static inline void nova_cmds_init(struct nova_cmds *c, uint32_t *storage, size_t capacity)
{ c->words = storage; c->count = 0; c->capacity = capacity; c->overflow = 0; }

static inline void nova_cmd(struct nova_cmds *c, uint32_t opcode, const uint32_t *operands, size_t n)
{
    if (c->count + n + 1 > c->capacity) { c->overflow = 1; return; }
    c->words[c->count++] = opcode | (uint32_t)(n + 1) << 16;
    memcpy(c->words + c->count, operands, n * 4);
    c->count += n;
}

static inline uint32_t nova_f32(float value) { uint32_t bits; memcpy(&bits, &value, 4); return bits; }

static inline void nova_cmd_begin_render(struct nova_cmds *c, uint32_t image, uint32_t load, const float clear[4],
                                         int32_t x, int32_t y, uint32_t w, uint32_t h)
{
    uint32_t o[13] = { image, load, nova_f32(clear[0]), nova_f32(clear[1]), nova_f32(clear[2]), nova_f32(clear[3]),
                       0, NOVA_LOAD_DONT_CARE, 0, (uint32_t)x, (uint32_t)y, w, h };
    nova_cmd(c, NOVA_CMD_BEGIN_RENDER, o, 13);
}
static inline void nova_cmd_begin_render_depth(struct nova_cmds *c, uint32_t image, uint32_t load, const float clear[4],
                                               uint32_t depth, uint32_t depth_load, float depth_clear,
                                               int32_t x, int32_t y, uint32_t w, uint32_t h)
{
    uint32_t o[13] = { image, load, nova_f32(clear[0]), nova_f32(clear[1]), nova_f32(clear[2]), nova_f32(clear[3]),
                       depth, depth_load, nova_f32(depth_clear), (uint32_t)x, (uint32_t)y, w, h };
    nova_cmd(c, NOVA_CMD_BEGIN_RENDER, o, 13);
}
static inline void nova_cmd_end_render(struct nova_cmds *c) { nova_cmd(c, NOVA_CMD_END_RENDER, 0, 0); }
static inline void nova_cmd_bind_pipeline(struct nova_cmds *c, uint32_t pipeline)
{ nova_cmd(c, NOVA_CMD_BIND_PIPELINE, &pipeline, 1); }
static inline void nova_cmd_bind_vertex(struct nova_cmds *c, uint32_t slot, uint32_t buffer, uint64_t offset)
{ uint32_t o[4] = { slot, buffer, (uint32_t)offset, (uint32_t)(offset >> 32) }; nova_cmd(c, NOVA_CMD_BIND_VERTEX, o, 4); }
static inline void nova_cmd_bind_index(struct nova_cmds *c, uint32_t buffer, uint64_t offset, int wide)
{ uint32_t o[4] = { buffer, (uint32_t)offset, (uint32_t)(offset >> 32), wide ? 1u : 0u }; nova_cmd(c, NOVA_CMD_BIND_INDEX, o, 4); }
static inline void nova_cmd_bind_image(struct nova_cmds *c, uint32_t binding, uint32_t image, uint32_t sampler)
{ uint32_t o[7] = { binding, image, sampler, 0, 0, 0, 0 }; nova_cmd(c, NOVA_CMD_BIND_RESOURCE, o, 7); }
static inline void nova_cmd_bind_buffer(struct nova_cmds *c, uint32_t binding, uint32_t buffer, uint64_t offset, uint64_t range)
{
    uint32_t o[7] = { binding, buffer, 0, (uint32_t)offset, (uint32_t)(offset >> 32), (uint32_t)range, (uint32_t)(range >> 32) };
    nova_cmd(c, NOVA_CMD_BIND_RESOURCE, o, 7);
}
static inline void nova_cmd_viewport(struct nova_cmds *c, float x, float y, float w, float h, float min_depth, float max_depth)
{
    uint32_t o[6] = { nova_f32(x), nova_f32(y), nova_f32(w), nova_f32(h), nova_f32(min_depth), nova_f32(max_depth) };
    nova_cmd(c, NOVA_CMD_VIEWPORT, o, 6);
}
static inline void nova_cmd_scissor(struct nova_cmds *c, int32_t x, int32_t y, uint32_t w, uint32_t h)
{ uint32_t o[4] = { (uint32_t)x, (uint32_t)y, w, h }; nova_cmd(c, NOVA_CMD_SCISSOR, o, 4); }
static inline void nova_cmd_push_constants(struct nova_cmds *c, uint32_t offset, const void *data, size_t bytes)
{
    uint32_t o[65]; size_t n = (bytes + 3) / 4;
    if (n > 64) { c->overflow = 1; return; }
    o[0] = offset; memset(o + 1, 0, n * 4); memcpy(o + 1, data, bytes);
    nova_cmd(c, NOVA_CMD_PUSH_CONSTANTS, o, n + 1);
}
static inline void nova_cmd_draw(struct nova_cmds *c, uint32_t vertices, uint32_t instances, uint32_t first, uint32_t first_instance)
{ uint32_t o[4] = { vertices, instances, first, first_instance }; nova_cmd(c, NOVA_CMD_DRAW, o, 4); }
static inline void nova_cmd_draw_indexed(struct nova_cmds *c, uint32_t indices, uint32_t instances, uint32_t first,
                                         int32_t vertex_offset, uint32_t first_instance)
{ uint32_t o[5] = { indices, instances, first, (uint32_t)vertex_offset, first_instance }; nova_cmd(c, NOVA_CMD_DRAW_INDEXED, o, 5); }
static inline void nova_cmd_copy_buffer(struct nova_cmds *c, uint32_t src, uint32_t dst, uint64_t src_offset, uint64_t dst_offset, uint64_t size)
{
    uint32_t o[8] = { src, dst, (uint32_t)src_offset, (uint32_t)(src_offset >> 32), (uint32_t)dst_offset,
                      (uint32_t)(dst_offset >> 32), (uint32_t)size, (uint32_t)(size >> 32) };
    nova_cmd(c, NOVA_CMD_COPY_BUFFER, o, 8);
}
static inline void nova_cmd_copy_buffer_to_image(struct nova_cmds *c, uint32_t buffer, uint64_t offset, uint32_t row_pixels,
                                                 uint32_t image, int32_t x, int32_t y, uint32_t w, uint32_t h)
{
    uint32_t o[9] = { buffer, (uint32_t)offset, (uint32_t)(offset >> 32), row_pixels, image, (uint32_t)x, (uint32_t)y, w, h };
    nova_cmd(c, NOVA_CMD_COPY_BUFFER_TO_IMAGE, o, 9);
}
static inline void nova_cmd_copy_image_to_buffer(struct nova_cmds *c, uint32_t image, int32_t x, int32_t y, uint32_t w, uint32_t h,
                                                 uint32_t buffer, uint64_t offset, uint32_t row_pixels)
{
    uint32_t o[9] = { image, (uint32_t)x, (uint32_t)y, w, h, buffer, (uint32_t)offset, (uint32_t)(offset >> 32), row_pixels };
    nova_cmd(c, NOVA_CMD_COPY_IMAGE_TO_BUFFER, o, 9);
}
static inline void nova_cmd_blit(struct nova_cmds *c, uint32_t src, int32_t sx, int32_t sy, uint32_t sw, uint32_t sh,
                                 uint32_t dst, int32_t dx, int32_t dy, uint32_t dw, uint32_t dh, int linear)
{
    uint32_t o[11] = { src, (uint32_t)sx, (uint32_t)sy, sw, sh, dst, (uint32_t)dx, (uint32_t)dy, dw, dh, linear ? 1u : 0u };
    nova_cmd(c, NOVA_CMD_BLIT_IMAGE, o, 11);
}
static inline void nova_cmd_clear_image(struct nova_cmds *c, uint32_t image, const float color[4])
{
    uint32_t o[5] = { image, nova_f32(color[0]), nova_f32(color[1]), nova_f32(color[2]), nova_f32(color[3]) };
    nova_cmd(c, NOVA_CMD_CLEAR_IMAGE, o, 5);
}
static inline void nova_cmd_fill_buffer(struct nova_cmds *c, uint32_t buffer, uint64_t offset, uint64_t size, uint32_t value)
{
    uint32_t o[6] = { buffer, (uint32_t)offset, (uint32_t)(offset >> 32), (uint32_t)size, (uint32_t)(size >> 32), value };
    nova_cmd(c, NOVA_CMD_FILL_BUFFER, o, 6);
}

#endif /* NOVA_GFX_H */

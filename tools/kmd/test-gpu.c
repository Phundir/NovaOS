/*
 * test-gpu.c -- the test module of the GPU module loader (phase 13.4.1b).
 *
 * Two things in one module, both for the loader's self-test:
 *
 *   - probe() looks for a card that does not exist and declines: what the
 *     loader must then do at boot is unload the module and carry on;
 *   - behind it, a GPU made of CPU and memory: allocations in pages the
 *     kernel lends, buffers placed in them, and the three commands that
 *     need no rasterizer (fill, update, copy). The self-test drives it
 *     through the same calls the core makes -- a context, memory, a
 *     buffer, a submission, a poll -- and reads the result back: the
 *     interface of nova_kmd.h crossed for real, both ways.
 *
 * Nothing here pretends to drive hardware.
 */
#include "nova_kmd.h"

#define TEST_VENDOR 0x4E4Fu   /* "NO": no such vendor */
#define TEST_DEVICE 0x5641u
#define SLOTS 16

static const struct nova_kmd_kernel *kernel;

/* The compiler may call these for structure copies: the module brings its
 * own, local, so that no symbol of the kernel is ever needed. */
void *memset(void *d, int c, size_t n)
{
    unsigned char *p = d;
    while (n--) *p++ = (unsigned char)c;
    return d;
}

void *memcpy(void *d, const void *s, size_t n)
{
    unsigned char *p = d;
    const unsigned char *q = s;
    while (n--) *p++ = *q++;
    return d;
}

struct memory { uint64_t physical; unsigned char *address; uint64_t size; int used; };
struct buffer { uint64_t memory; uint64_t offset, size; int used; };
struct context { struct memory memories[SLOTS]; struct buffer buffers[SLOTS]; };

static int probe(void)
{
    uint32_t bdf;
    if (kernel->pci_find(TEST_VENDOR, TEST_DEVICE, 0, &bdf) == 0)
        return 0;
    kernel->log("[kmd] test-gpu : aucune carte 4e4f:5641 - le module se retire");
    return NOVA_KMD_NO_DEVICE;
}

static int describe(struct nova_kmd_description *out)
{
    memset(out, 0, sizeof *out);
    memcpy(out->name, "GPU d'essai (processeur)", 25);
    out->vendor_id = TEST_VENDOR;
    out->device_id = TEST_DEVICE;
    out->max_image_size = 1;
    out->buffer_alignment = 256;
    return NOVA_KMD_OK;
}

static int open_context(const char *name, void **out)
{
    (void)name;
    struct context *ctx = kernel->alloc(sizeof *ctx);
    if (!ctx)
        return NOVA_KMD_NO_MEMORY;
    memset(ctx, 0, sizeof *ctx);
    *out = ctx;
    return NOVA_KMD_OK;
}

static int alloc(void *c, uint64_t size, uint32_t kind, uint64_t *id, uint64_t *real)
{
    struct context *ctx = c;
    (void)kind;
    for (int i = 0; i < SLOTS; i++) {
        if (ctx->memories[i].used)
            continue;
        uint64_t pages = (size + 4095) / 4096;
        void *address = 0;
        uint64_t physical = kernel->pages_alloc(pages, &address);
        if (!physical)
            return NOVA_KMD_NO_MEMORY;
        ctx->memories[i] = (struct memory){ physical, address, pages * 4096, 1 };
        *id = (uint64_t)i + 1;
        *real = pages * 4096;
        return NOVA_KMD_OK;
    }
    return NOVA_KMD_NO_MEMORY;
}

static struct memory *memory_of(struct context *ctx, uint64_t id)
{
    if (id == 0 || id > SLOTS || !ctx->memories[id - 1].used)
        return 0;
    return &ctx->memories[id - 1];
}

static void release(void *c, uint64_t id)
{
    struct memory *m = memory_of(c, id);
    if (m) {
        kernel->pages_free(m->physical, m->size / 4096);
        m->used = 0;
    }
}

static int map(void *c, uint64_t id, struct nova_kmd_window *out)
{
    struct memory *m = memory_of(c, id);
    if (!m)
        return NOVA_KMD_FAILED;
    *out = (struct nova_kmd_window){ m->physical, m->size, 0 };
    return NOVA_KMD_OK;
}

static void unmap(void *c, uint64_t id) { (void)c; (void)id; }

static int create_buffer(void *c, const struct nova_kmd_buffer_desc *d, uint64_t memory, uint64_t offset, uint64_t *id)
{
    struct context *ctx = c;
    struct memory *m = memory_of(ctx, memory);
    if (!m || offset + d->size > m->size)
        return NOVA_KMD_FAILED;
    for (int i = 0; i < SLOTS; i++) {
        if (!ctx->buffers[i].used) {
            ctx->buffers[i] = (struct buffer){ memory, offset, d->size, 1 };
            *id = (uint64_t)i + 1;
            return NOVA_KMD_OK;
        }
    }
    return NOVA_KMD_NO_MEMORY;
}

static unsigned char *bytes_of(struct context *ctx, uint64_t id, uint64_t offset, uint64_t size)
{
    if (id == 0 || id > SLOTS || !ctx->buffers[id - 1].used)
        return 0;
    struct buffer *b = &ctx->buffers[id - 1];
    struct memory *m = memory_of(ctx, b->memory);
    if (!m || offset + size > b->size)
        return 0;
    return m->address + b->offset + offset;
}

static void destroy(void *c, uint32_t kind, uint64_t id)
{
    struct context *ctx = c;
    if (kind == NOVA_KMD_BUFFER && id > 0 && id <= SLOTS)
        ctx->buffers[id - 1].used = 0;
}

static int submit(void *c, const struct nova_kmd_command *commands, uint32_t count, uint64_t *work)
{
    struct context *ctx = c;
    for (uint32_t i = 0; i < count; i++) {
        const struct nova_kmd_command *k = &commands[i];
        if (k->opcode == 18) {          /* FILL_BUFFER */
            uint32_t *p = (uint32_t *)bytes_of(ctx, k->u.fill_buffer.buffer, k->u.fill_buffer.offset, k->u.fill_buffer.size);
            if (!p)
                return NOVA_KMD_FAILED;
            for (uint64_t w = 0; w < k->u.fill_buffer.size / 4; w++)
                p[w] = k->u.fill_buffer.value;
        } else if (k->opcode == 19) {   /* UPDATE_BUFFER */
            unsigned char *p = bytes_of(ctx, k->u.update_buffer.buffer, k->u.update_buffer.offset, k->u.update_buffer.size);
            if (!p)
                return NOVA_KMD_FAILED;
            memcpy(p, k->u.update_buffer.data, k->u.update_buffer.size);
        } else if (k->opcode == 12) {   /* COPY_BUFFER */
            unsigned char *s = bytes_of(ctx, k->u.copy_buffer.src, k->u.copy_buffer.src_offset, k->u.copy_buffer.size);
            unsigned char *d = bytes_of(ctx, k->u.copy_buffer.dst, k->u.copy_buffer.dst_offset, k->u.copy_buffer.size);
            if (!s || !d)
                return NOVA_KMD_FAILED;
            memcpy(d, s, k->u.copy_buffer.size);
        } else {
            return NOVA_KMD_UNSUPPORTED;
        }
    }
    *work = 1;
    return NOVA_KMD_OK;
}

static int poll(void *c, uint64_t work) { (void)c; (void)work; return NOVA_KMD_DONE; }

static void close_context(void *c)
{
    struct context *ctx = c;
    for (uint64_t i = 1; i <= SLOTS; i++)
        release(ctx, i);
    kernel->free(ctx, sizeof *ctx);
}

static int no_image(void *c, const struct nova_kmd_image_desc *d, uint64_t m, uint64_t o, int p, uint64_t *id, uint64_t *own)
{ (void)c; (void)d; (void)m; (void)o; (void)p; (void)id; (void)own; return NOVA_KMD_UNSUPPORTED; }
static int no_sampler(void *c, const struct nova_kmd_sampler_desc *d, uint64_t *id) { (void)c; (void)d; (void)id; return NOVA_KMD_UNSUPPORTED; }
static int no_shader(void *c, const uint32_t *s, uint32_t w, uint64_t *id) { (void)c; (void)s; (void)w; (void)id; return NOVA_KMD_UNSUPPORTED; }
static int no_layout(void *c, const struct nova_kmd_layout_desc *d, uint64_t *id) { (void)c; (void)d; (void)id; return NOVA_KMD_UNSUPPORTED; }
static int no_pipeline(void *c, const struct nova_kmd_pipeline_desc *d, uint64_t *id) { (void)c; (void)d; (void)id; return NOVA_KMD_UNSUPPORTED; }
static int no_export(void *c, uint64_t i, uint32_t *n) { (void)c; (void)i; (void)n; return NOVA_KMD_UNSUPPORTED; }
static int no_import(void *c, uint32_t n, const struct nova_kmd_image_desc *d, uint64_t *id) { (void)c; (void)n; (void)d; (void)id; return NOVA_KMD_UNSUPPORTED; }

static const struct nova_kmd_gpu table = {
    .interface = NOVA_KMD_INTERFACE,
    .size = sizeof(struct nova_kmd_gpu),
    .name = "test-gpu",
    .probe = probe,
    .describe = describe,
    .open = open_context,
    .alloc = alloc,
    .free = release,
    .map = map,
    .unmap = unmap,
    .create_buffer = create_buffer,
    .create_image = no_image,
    .create_sampler = no_sampler,
    .create_shader = no_shader,
    .create_layout = no_layout,
    .create_pipeline = no_pipeline,
    .destroy = destroy,
    .submit = submit,
    .poll = poll,
    .export_image = no_export,
    .import_image = no_import,
    .close = close_context,
};

__attribute__((visibility("default"))) const struct nova_kmd_gpu *nova_kmd_init(const struct nova_kmd_kernel *k)
{
    if (k->interface != NOVA_KMD_INTERFACE || k->size < sizeof *k)
        return 0;
    kernel = k;
    return &table;
}

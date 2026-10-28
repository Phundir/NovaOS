/*
 * gpu.c -- a native Nova program drawing on the GPU (phase 13.7.1).
 *
 * Nothing but nova_gfx.h and the kernel: a context, memory the program
 * maps and writes, a vertex buffer, two SPIR-V shaders, a pipeline, a
 * triangle drawn into an image, the image copied back to memory the
 * program reads -- and the pixels checked. Then what must fail, and a
 * child that dies holding GPU objects: the kernel must take them back.
 *
 * With the argument "fenetre", the triangle is also shown in a window of
 * /dev/nova-gui through SYS_GPU_PRESENT, and the window's pixels read
 * back (not at boot: a window would open over the login screen).
 *
 * With the argument "garde", the watchdog instead (spec 6.8, 13.11.3): a
 * draw is timed on a short load, then sized to hold the GPU about three
 * seconds -- past the kernel's two. The context must be lost at about two
 * seconds, and a new one must work at once. Not at boot either: it
 * borrows the host's GPU for those seconds.
 *
 * Exit code: 0 when everything held, 77 when there is no GPU (the test
 * is then not run), otherwise the number of the first check that failed.
 */
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include "nova_gfx.h"
#include "gpu_shaders.h"

#define W 128
#define H 96

static int check = 0;
static int failures = 0;

static int ok(int condition, const char *what)
{
    check++;
    if (!condition) {
        failures++;
        printf("gpu: ECHEC %d - %s\n", check, what);
        return 0;
    }
    return 1;
}

static long shader(uint32_t ctx, const unsigned char *code, unsigned size, uint32_t stage)
{
    struct nova_shader_desc desc = { (uint64_t)(uintptr_t)code, size, stage };
    return nova_gpu_create(ctx, NOVA_OBJ_SHADER, &desc, sizeof desc);
}

/* The child's life: a context, memory, and death without a word. */
static void abandon(void)
{
    long ctx = nova_gpu_context_create();
    long mem = ctx > 0 ? nova_gpu_mem_alloc(ctx, 1 << 20, NOVA_MEM_LOCAL) : -1;
    _exit(ctx > 0 && mem > 0 ? 0 : 3);
}

/* SYS_GPU_PRESENT into a window of /dev/nova-gui, read back. */
static void window_test(long ctx, long image, long fence)
{
    int fd = open("/dev/nova-gui", O_RDWR);
    if (!ok(fd >= 0, "fenetre /dev/nova-gui ouverte"))
        return;
    struct { uint32_t w, h; char title[128]; } create = { W, H, "Essai GPU" };
    if (!ok(ioctl(fd, 0x4E01, &create) == 0, "fenetre creee"))
        return;
    uint32_t *window = mmap(0, W * H * 4, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (!ok(window != MAP_FAILED, "pixels de la fenetre projetes"))
        return;
    long r = nova_gpu_call(NOVA_GPU_PRESENT, ctx, image, fd, fence, 0, 0);
    uint32_t centre = window[(H / 2 + 4) * W + W / 2];
    uint32_t corner = window[1 * W + 1];
    printf("gpu: fenetre centre %08x coin %08x\n", centre, corner);
    /* The window is B, G, R, X in memory: green is 0x00FF00, the clear
     * colour (half blue) 0x7F or 0x80. */
    uint32_t blue = corner & 0xFFFFFF;
    ok(r == 0 && (centre & 0xFFFFFF) == 0x00FF00 && (blue == 0x7F || blue == 0x80),
       "SYS_GPU_PRESENT montre le triangle dans la fenetre");
    sleep(2);
    close(fd);
}

static double now(void)
{
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

/* A context drawing `iterations` of busy.frag on every pixel of a
 * 1024 x 1024 image. Returns the seconds the fence took, or -1; `lost`
 * says whether the context was lost meanwhile. */
static double busy(uint32_t iterations, int *lost)
{
    long ctx = nova_gpu_context_create();
    struct nova_image_desc id = { NOVA_FORMAT_RGBA8, 1024, 1024, NOVA_IMAGE_RENDER, 0, 0, 0 };
    long image = ctx > 0 ? nova_gpu_create(ctx, NOVA_OBJ_IMAGE, &id, sizeof id) : -1;
    long vert = ctx > 0 ? shader(ctx, busy_vert_spv, sizeof busy_vert_spv, NOVA_STAGE_VERTEX) : -1;
    long frag = ctx > 0 ? shader(ctx, busy_frag_spv, sizeof busy_frag_spv, NOVA_STAGE_FRAGMENT) : -1;
    struct nova_layout_desc ld = { 0, 4 };
    long layout = ctx > 0 ? nova_gpu_create(ctx, NOVA_OBJ_LAYOUT, &ld, 8) : -1;
    struct nova_pipeline_desc pd;
    memset(&pd, 0, sizeof pd);
    pd.layout = layout;
    pd.vertex = vert;
    pd.fragment = frag;
    pd.color_format = NOVA_FORMAT_RGBA8;
    long pipeline = layout > 0 ? nova_gpu_create(ctx, NOVA_OBJ_PIPELINE, &pd, sizeof pd) : -1;
    long fence = ctx > 0 ? nova_gpu_fence_create(ctx) : -1;
    if (image <= 0 || vert <= 0 || frag <= 0 || pipeline <= 0 || fence <= 0) {
        printf("gpu: garde - objets refuses (%ld %ld %ld %ld %ld)\n", image, vert, frag, pipeline, fence);
        return -1;
    }
    uint32_t words[64];
    struct nova_cmds cmds;
    const float clear[4] = { 0, 0, 0, 1 };
    nova_cmds_init(&cmds, words, 64);
    nova_cmd_begin_render(&cmds, image, NOVA_LOAD_CLEAR, clear, 0, 0, 1024, 1024);
    nova_cmd_bind_pipeline(&cmds, pipeline);
    nova_cmd_push_constants(&cmds, 0, &iterations, 4);
    nova_cmd_draw(&cmds, 3, 1, 0, 0);
    nova_cmd_end_render(&cmds);
    double start = now();
    if (nova_gpu_submit(ctx, words, cmds.count, fence, 0, 0) != 0)
        return -1;
    uint32_t fences[1] = { (uint32_t)fence };
    long waited = nova_gpu_fence_wait(fences, 1, 20000000000ull, 1);
    double seconds = now() - start;
    /* A lost context answers ENOTRECOVERABLE to everything after. */
    *lost = waited == -ENOTRECOVERABLE || nova_gpu_fence_create(ctx) == -ENOTRECOVERABLE;
    nova_gpu_context_destroy(ctx);
    return seconds;
}

static int watchdog(void)
{
    /* Calibration: grow the load until it takes a measurable time. */
    uint32_t iterations = 1000;
    int lost = 0;
    double seconds = 0;
    while (iterations < (1u << 28)) {
        seconds = busy(iterations, &lost);
        if (seconds < 0 || lost)
            break;
        if (seconds >= 0.05)
            break;
        iterations *= 4;
    }
    if (!ok(seconds > 0 && !lost, "une charge courte passe sous le delai de garde"))
        return check;
    double rate = iterations / seconds;
    uint32_t long_run = rate * 3.0 > 4e9 ? 4000000000u : (uint32_t)(rate * 3.0);
    printf("gpu: garde - %u iterations en %.3f s, charge longue de %u iterations (environ 3 s)\n", iterations, seconds,
           long_run);
    seconds = busy(long_run, &lost);
    printf("gpu: garde - contexte %s au bout de %.3f s\n", lost ? "perdu" : "intact", seconds);
    ok(lost && seconds > 1.8 && seconds < 3.0, "le delai de garde coupe le travail au bout de deux secondes");
    int again = 0;
    double short_run = busy(1000, &again);
    printf("gpu: garde - un nouveau contexte rend en %.3f s\n", short_run);
    ok(short_run >= 0 && !again, "un nouveau contexte fonctionne ensuite");
    printf("gpu: %d verifications\n", check);
    return failures ? check : 0;
}

int main(int argc, char **argv)
{
    int with_window = argc > 1 && strcmp(argv[1], "fenetre") == 0;
    if (argc > 1 && strcmp(argv[1], "garde") == 0)
        return watchdog();
    struct nova_gpu_info info;
    memset(&info, 0, sizeof info);
    long r = nova_gpu_info(&info);
    if (r == -ENODEV) {
        printf("gpu: pas de GPU - essai ignore\n");
        return 77;
    }
    if (!ok(r == 0 && info.present == 1 && info.interface == NOVA_GPU_INTERFACE, "SYS_GPU_INFO"))
        return check;
    printf("gpu: %s - interface %x, %llu Mio locaux, quota %llu Mio\n", info.device, info.interface,
           (unsigned long long)(info.local_memory >> 20), (unsigned long long)(info.quota >> 20));

    long ctx = nova_gpu_context_create();
    if (!ok(ctx > 0, "contexte"))
        return check;

    long vmem = nova_gpu_mem_alloc(ctx, 65536, NOVA_MEM_VISIBLE);
    long rmem = nova_gpu_mem_alloc(ctx, W * H * 4, NOVA_MEM_READBACK);
    float *vertices = nova_gpu_mem_map(ctx, vmem);
    unsigned char *pixels = nova_gpu_mem_map(ctx, rmem);
    if (!ok(vmem > 0 && rmem > 0 && !nova_gpu_failed((long)vertices) && !nova_gpu_failed((long)pixels),
            "memoire allouee et projetee dans le programme"))
        return check;
    const float triangle[6] = { 0.0f, -0.8f, 0.8f, 0.8f, -0.8f, 0.8f };
    memcpy(vertices, triangle, sizeof triangle);
    memset(pixels, 0x55, W * H * 4);

    struct nova_buffer_desc vb = { 65536, NOVA_BUFFER_VERTEX, (uint32_t)vmem, 0 };
    struct nova_buffer_desc rb = { W * H * 4, 0, (uint32_t)rmem, 0 };
    struct nova_image_desc id = { NOVA_FORMAT_RGBA8, W, H, NOVA_IMAGE_RENDER, 0, 0, 0 };
    long vertex = nova_gpu_create(ctx, NOVA_OBJ_BUFFER, &vb, sizeof vb);
    long readback = nova_gpu_create(ctx, NOVA_OBJ_BUFFER, &rb, sizeof rb);
    long image = nova_gpu_create(ctx, NOVA_OBJ_IMAGE, &id, sizeof id);
    long vert = shader(ctx, triangle_vert_spv, sizeof triangle_vert_spv, NOVA_STAGE_VERTEX);
    long frag = shader(ctx, triangle_frag_spv, sizeof triangle_frag_spv, NOVA_STAGE_FRAGMENT);
    struct nova_layout_desc ld = { 0, 16 };
    long layout = nova_gpu_create(ctx, NOVA_OBJ_LAYOUT, &ld, 8);
    struct nova_pipeline_desc pd;
    memset(&pd, 0, sizeof pd);
    pd.layout = layout;
    pd.vertex = vert;
    pd.fragment = frag;
    pd.color_format = NOVA_FORMAT_RGBA8;
    pd.binding_count = 1;
    pd.bindings[0].stride = 8;
    pd.attribute_count = 1;
    pd.attributes[0].format = NOVA_VERTEX_FLOAT2;
    long pipeline = nova_gpu_create(ctx, NOVA_OBJ_PIPELINE, &pd, sizeof pd);
    long fence = nova_gpu_fence_create(ctx);
    if (!ok(vertex > 0 && readback > 0 && image > 0 && vert > 0 && frag > 0 && layout > 0 && pipeline > 0 && fence > 0,
            "objets crees"))
        return check;

    /* --- what must fail --- */
    uint32_t words[256];
    struct nova_cmds cmds;
    nova_cmds_init(&cmds, words, 256);
    nova_cmd_draw(&cmds, 3, 1, 0, 0);
    ok(nova_gpu_submit(ctx, words, cmds.count, 0, 0, 0) == -EINVAL, "un dessin hors d'une passe est refuse");
    ok(nova_gpu_destroy(ctx, 0x7FFFFFF0) == -EBADF, "une poignee jamais donnee est inconnue");
    ok(nova_gpu_mem_alloc(ctx, info.quota, NOVA_MEM_LOCAL) == -EDQUOT, "le quota refuse ce qui le depasse");

    /* --- the triangle --- */
    const float clear[4] = { 0.0f, 0.0f, 0.5f, 1.0f };
    const float green[4] = { 0.0f, 1.0f, 0.0f, 1.0f };
    nova_cmds_init(&cmds, words, 256);
    nova_cmd_begin_render(&cmds, image, NOVA_LOAD_CLEAR, clear, 0, 0, W, H);
    nova_cmd_bind_pipeline(&cmds, pipeline);
    nova_cmd_bind_vertex(&cmds, 0, vertex, 0);
    nova_cmd_push_constants(&cmds, 0, green, sizeof green);
    nova_cmd_draw(&cmds, 3, 1, 0, 0);
    nova_cmd_end_render(&cmds);
    nova_cmd_copy_image_to_buffer(&cmds, image, 0, 0, W, H, readback, 0, 0);
    long fd = nova_gpu_fence_export(fence);
    ok(fd >= 0, "la barriere devient un descripteur");
    ok(nova_gpu_submit(ctx, words, cmds.count, fence, 0, 0) == 0, "le triangle est soumis");
    struct pollfd p = { (int)fd, POLLIN, 0 };
    ok(poll(&p, 1, 3000) == 1 && (p.revents & POLLIN), "le descripteur devient lisible quand le GPU a fini");
    uint32_t fences[1] = { (uint32_t)fence };
    ok(nova_gpu_fence_wait(fences, 1, 3000000000ull, 1) == 0, "la barriere est levee");
    close((int)fd);
    const unsigned char *centre = pixels + ((H / 2 + 4) * W + W / 2) * 4;
    const unsigned char *corner = pixels + (1 * W + 1) * 4;
    printf("gpu: centre %u %u %u %u, coin %u %u %u %u\n", centre[0], centre[1], centre[2], centre[3], corner[0],
           corner[1], corner[2], corner[3]);
    ok(centre[0] == 0 && centre[1] == 255 && centre[2] == 0 && centre[3] == 255, "le centre du triangle est vert");
    ok(corner[0] == 0 && corner[1] == 0 && (corner[2] == 127 || corner[2] == 128) && corner[3] == 255,
       "le coin a la couleur d'effacement");

    if (with_window)
        window_test(ctx, image, fence);

    /* --- a child dies holding GPU objects --- */
    pid_t child = fork();
    if (child == 0)
        abandon();
    int status = 0;
    ok(child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0,
       "un fils ouvre un contexte et meurt sans le fermer");

    /* --- everything goes --- */
    long ids[] = { vertex, readback, image, pipeline, layout, vert, frag, fence };
    int destroyed = 1;
    for (unsigned i = 0; i < sizeof ids / sizeof ids[0]; i++)
        destroyed &= nova_gpu_destroy(ctx, ids[i]) == 0;
    ok(destroyed, "objets detruits");
    ok(nova_gpu_mem_unmap(ctx, vmem) == 0 && nova_gpu_mem_free(ctx, vmem) == 0 && nova_gpu_mem_free(ctx, rmem) == 0,
       "memoires rendues");
    ok(nova_gpu_context_destroy(ctx) == 0, "contexte ferme");
    ok(nova_gpu_mem_free(ctx, rmem) == -EBADF, "une poignee d'un contexte ferme est inconnue");

    printf("gpu: %d verifications\n", check);
    return 0;
}

/* Memory: malloc, mmap, mprotect, mremap, munmap, brk (phase 11.5). */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    /* A large reservation, touched sparsely: demand paging. */
    size_t big = 1UL << 30;
    char *p = mmap(0, big, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
    if (p == MAP_FAILED) return 1;
    for (size_t off = 0; off < big; off += 64 << 20) p[off] = (char)(off >> 20);
    for (size_t off = 0; off < big; off += 64 << 20) if (p[off] != (char)(off >> 20)) return 2;
    if (p[4096] != 0) return 3;
    if (munmap(p, big) != 0) return 4;

    /* mprotect down and back up keeps the contents. */
    char *q = mmap(0, 3 * 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    memset(q, 0x5A, 3 * 4096);
    if (mprotect(q + 4096, 4096, PROT_NONE) != 0) return 5;
    if (mprotect(q + 4096, 4096, PROT_READ | PROT_WRITE) != 0) return 6;
    if (q[4096 + 17] != 0x5A) return 7;

    /* mremap grows, moving if needed, and keeps the bytes. */
    char *r = mremap(q, 3 * 4096, 64 * 4096, MREMAP_MAYMOVE);
    if (r == MAP_FAILED) return 8;
    if (r[2 * 4096 + 5] != 0x5A || r[40 * 4096] != 0) return 9;

    /* malloc of many sizes, freed in another order. */
    void *blocks[1000];
    for (int i = 0; i < 1000; i++) {
        blocks[i] = malloc((i * 7919) % 70000 + 1);
        if (!blocks[i]) return 10;
        memset(blocks[i], i, (i * 7919) % 70000 + 1);
    }
    for (int i = 999; i >= 0; i -= 2) free(blocks[i]);
    for (int i = 0; i < 1000; i += 2) free(blocks[i]);

    /* brk, through the raw call: musl's sbrk refuses any increment. */
    char *before = (char *)syscall(SYS_brk, 0);
    if ((char *)syscall(SYS_brk, before + (1 << 20)) != before + (1 << 20)) return 11;
    memset(before, 1, 1 << 20);
    if ((char *)syscall(SYS_brk, before) != before) return 12;

    /* The direction flag belongs to the program. musl's memmove copies
     * backwards (std; rep movsb) and may take a page fault meanwhile: the
     * kernel must not run its own copies with the program's DF, or they
     * go backwards too -- 4 KiB into the frame below the one filled. A
     * copy-on-write page written with DF set must come out whole, and a
     * fresh page read with DF set must read as zeros. */
    unsigned char *cow = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (cow == MAP_FAILED) return 13;
    memset(cow, 0xC3, 4096);
    pid_t child = fork();
    if (child < 0) return 14;
    if (child == 0) {
        __asm__ volatile("std\n\tmovb $0x11, (%0)\n\tcld" : : "r"(cow + 2048) : "memory");
        for (int i = 0; i < 4096; i++)
            if (cow[i] != (i == 2048 ? 0x11 : 0xC3)) _exit(1);
        unsigned char *fresh = mmap(0, 16 * 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (fresh == MAP_FAILED) _exit(2);
        for (int page = 0; page < 16; page++) {
            unsigned char v;
            __asm__ volatile("std\n\tmovb (%1), %0\n\tcld" : "=q"(v) : "r"(fresh + page * 4096 + 100) : "memory");
            if (v != 0) _exit(3);
        }
        for (int i = 0; i < 16 * 4096; i++)
            if (fresh[i] != 0) _exit(4);
        _exit(0);
    }
    int status;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status)) return 15;
    if (WEXITSTATUS(status) != 0) {
        printf("memory: drapeau de direction - cas %d\n", WEXITSTATUS(status));
        return 16;
    }
    for (int i = 0; i < 4096; i++)
        if (cow[i] != 0xC3) return 17;

    printf("memory: 1 Gio reserve, mprotect, mremap, 1000 malloc, brk, drapeau de direction conformes\n");
    return 0;
}

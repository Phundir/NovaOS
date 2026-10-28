/* Address space under concurrency (phase 11.3 / 11.10).
 *
 * The Web Browser died at random in musl's malloc, reading zeros where it
 * had written a group header, once KVM gave the processors real TLBs.
 * The pattern it needs: one thread unmaps a range, another maps the same
 * addresses at once, and a third reads them. This reproduces it on
 * purpose: threads map, fill, hand over, check and unmap pages through a
 * shared ring, and do the same with malloc'd blocks, checking every word.
 *
 * Exit 0 when every word read back is the one written. */
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <unistd.h>

#define THREADS 4
#define ROUNDS 6000
#define SLOTS 64

struct slot { uint64_t *p; size_t words; uint64_t tag; int mapped; };

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static struct slot ring[SLOTS];
static int head, tail, count;
static volatile int failed;
/* 'm': mappings only, 'b': malloc'd blocks only, else both. */
static int mode;
static int threads = THREADS;

static void fill(uint64_t *p, size_t words, uint64_t tag) {
    for (size_t i = 0; i < words; i++) p[i] = tag ^ (i * 0x9E3779B97F4A7C15ull);
}

static int check(const uint64_t *p, size_t words, uint64_t tag) {
    for (size_t i = 0; i < words; i++)
        if (p[i] != (tag ^ (i * 0x9E3779B97F4A7C15ull))) {
            printf("mmstress: %p mot %zu : 0x%llx au lieu de 0x%llx\n", (void *)p, i,
                   (unsigned long long)p[i], (unsigned long long)(tag ^ (i * 0x9E3779B97F4A7C15ull)));
            return 0;
        }
    return 1;
}

static void *worker(void *arg) {
    uint64_t id = (uint64_t)(uintptr_t)arg;
    uint64_t seed = id * 7919 + 1;
    for (uint64_t n = 0; n < ROUNDS && !failed; n++) {
        seed = seed * 6364136223846793005ull + 1442695040888963407ull;
        int give = (seed >> 33) & 1;
        struct slot s = {0};
        pthread_mutex_lock(&lock);
        if (!give && count > 0) {
            s = ring[tail];
            tail = (tail + 1) % SLOTS;
            count--;
        }
        pthread_mutex_unlock(&lock);
        if (s.p) {
            if (!check(s.p, s.words, s.tag)) { failed = 1; break; }
            if (s.mapped) munmap(s.p, s.words * 8);
            else free(s.p);
            continue;
        }
        /* Produce: a mapping of 1 to 4 pages, or a malloc'd block. */
        s.mapped = mode == 'm' ? 1 : mode == 'b' ? 0 : (seed >> 40) & 1;
        s.tag = (id << 56) | (n << 8) | 0x5A;
        if (s.mapped) {
            s.words = (((seed >> 44) & 3) + 1) * 512;
            s.p = mmap(0, s.words * 8, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            if (s.p == MAP_FAILED) { printf("mmstress: mmap a echoue\n"); failed = 1; break; }
        } else {
            s.words = ((seed >> 44) & 1023) + 2;
            s.p = malloc(s.words * 8);
            if (!s.p) { printf("mmstress: malloc a echoue\n"); failed = 1; break; }
        }
        fill(s.p, s.words, s.tag);
        pthread_mutex_lock(&lock);
        if (count < SLOTS) {
            ring[head] = s;
            head = (head + 1) % SLOTS;
            count++;
            s.p = 0;
        }
        pthread_mutex_unlock(&lock);
        if (s.p) {
            if (!check(s.p, s.words, s.tag)) { failed = 1; break; }
            if (s.mapped) munmap(s.p, s.words * 8);
            else free(s.p);
        }
    }
    return 0;
}

int main(int argc, char **argv) {
    pthread_t t[THREADS];
    for (int a = 1; a < argc; a++) {
        if (argv[a][0] == '1') threads = 1;
        else mode = argv[a][0];
    }
    for (long i = 0; i < threads; i++)
        if (pthread_create(&t[i], 0, worker, (void *)i) != 0) return 2;
    for (int i = 0; i < threads; i++) pthread_join(t[i], 0);
    if (failed) return 1;
    for (int i = 0; i < count; i++) {
        struct slot *s = &ring[(tail + i) % SLOTS];
        if (!check(s->p, s->words, s->tag)) return 1;
    }
    printf("mmstress: %d thread(s), mode %c, %d tours, pages et blocs passes de main en main intacts\n",
           threads, mode ? mode : '*', ROUNDS);
    return 0;
}

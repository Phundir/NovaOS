/* Cost of the kernel paths a browser leans on, in nanoseconds per
 * operation: the same static binary runs on the Linux host and under
 * Nova, and the two columns say where Nova is slower.
 *
 * Not one of the boot self-tests (it proves nothing, it measures); run
 * it by hand, or from /nova/autostart.sh on a test image.
 *
 *   sysbench            every measure
 *   sysbench tube       the pipe round trips only, long enough (300 000
 *                       rounds) for the kernel monitor's five-second
 *                       wake-up statistics to cover them alone
 */
#define _GNU_SOURCE
#include <pthread.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static uint64_t now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
}

static void report(const char *what, uint64_t start, long ops) {
    uint64_t ns = now_ns() - start;
    printf("sysbench: %-28s %8llu ns/op (%ld ops)\n", what, (unsigned long long)(ns / (uint64_t)ops), ops);
    fflush(stdout);
}

static int ping[2], pong[2];

static void *echo(void *arg) {
    long rounds = (long)arg;
    char c;
    for (long i = 0; i < rounds; i++) {
        if (read(ping[0], &c, 1) != 1 || write(pong[1], &c, 1) != 1)
            break;
    }
    return NULL;
}

static void ping_pong(const char *what, long rounds) {
    char c = 'x';
    uint64_t t = now_ns();
    for (long i = 0; i < rounds; i++) {
        if (write(ping[1], &c, 1) != 1 || read(pong[0], &c, 1) != 1)
            break;
    }
    report(what, t, rounds);
}

static int pipes_only(void) {
    const long R = 300000;
    if (pipe(ping) || pipe(pong))
        return 1;
    pthread_t th;
    pthread_create(&th, NULL, echo, (void *)R);
    ping_pong("aller-retour tube, threads", R);
    pthread_join(th, NULL);
    printf("sysbench: fin\n");
    return 0;
}

int main(int argc, char **argv) {
    const long N = 200000;
    uint64_t t;
    if (argc > 1 && strcmp(argv[1], "tube") == 0)
        return pipes_only();

    t = now_ns();
    for (long i = 0; i < N; i++)
        syscall(SYS_getppid);
    report("getppid (appel nu)", t, N);

    struct timespec ts;
    t = now_ns();
    for (long i = 0; i < N; i++)
        clock_gettime(CLOCK_MONOTONIC, &ts);
    report("clock_gettime (libc)", t, N);

    t = now_ns();
    for (long i = 0; i < N; i++)
        syscall(SYS_clock_gettime, CLOCK_MONOTONIC, &ts);
    report("clock_gettime (appel)", t, N);

    int word = 0;
    t = now_ns();
    for (long i = 0; i < N; i++)
        syscall(SYS_futex, &word, 1 /* FUTEX_WAKE */, 1, NULL, NULL, 0);
    report("futex wake sans attente", t, N);

    t = now_ns();
    for (long i = 0; i < N / 4; i++)
        sched_yield();
    report("sched_yield", t, N / 4);

    const long M = 20000;
    t = now_ns();
    for (long i = 0; i < M; i++) {
        char *p = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        p[0] = 1;
        munmap(p, 4096);
    }
    report("mmap+faute+munmap 4 Kio", t, M);

    char *region = mmap(NULL, 64 << 20, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    t = now_ns();
    for (long i = 0; i < (64 << 20) / 4096; i++)
        region[i * 4096] = 1;
    report("faute de page (64 Mio)", t, (64 << 20) / 4096);

    t = now_ns();
    for (long i = 0; i < M; i++) {
        mprotect(region, 4096, PROT_READ);
        mprotect(region, 4096, PROT_READ | PROT_WRITE);
    }
    report("mprotect RW->R->RW", t, M);

    t = now_ns();
    for (long i = 0; i < M; i++)
        madvise(region + (i % 1024) * 4096, 4096, MADV_DONTNEED);
    report("madvise DONTNEED 4 Kio", t, M);

    /* Touching 64 MiB again: a TLB-hungry pass. */
    t = now_ns();
    uint64_t sum = 0;
    for (int pass = 0; pass < 8; pass++)
        for (long i = 0; i < (64 << 20) / 4096; i++)
            sum += (unsigned char)region[i * 4096 + (pass * 64)];
    report("lecture 1 octet/page 64 Mio", t, 8 * ((64 << 20) / 4096));
    if (sum == 42)
        puts("");

    const long R = 20000;
    if (pipe(ping) || pipe(pong))
        return 1;
    pthread_t th;
    pthread_create(&th, NULL, echo, (void *)R);
    ping_pong("aller-retour tube, threads", R);
    pthread_join(th, NULL);

    pid_t child = fork();
    if (child == 0) {
        echo((void *)R);
        _exit(0);
    }
    ping_pong("aller-retour tube, processus", R);
    waitpid(child, NULL, 0);

    printf("sysbench: fin\n");
    return 0;
}

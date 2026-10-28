/* Processes: fork (copy-on-write), execve, posix_spawn, waitpid, kill,
 * SIGCHLD, pipes and shared memory across processes (phase 11.5). */
#define _GNU_SOURCE
/* The program exec'd: the Linux build's hello, or the native one
 * (phase 12: a native program execs native programs). */
#ifndef HELLO_PATH
#define HELLO_PATH "/nova-tests/hello"
#endif
#include <signal.h>
#include <spawn.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;
static volatile int chld;
static void on_chld(int s) { (void)s; chld++; }

int main(void) {
    signal(SIGCHLD, on_chld);
    int private_value = 11;
    char *heap = malloc(1 << 20);
    memset(heap, 'p', 1 << 20);
    int m = memfd_create("partage", 0);
    ftruncate(m, 4096);
    char *shared = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, m, 0);
    strcpy(shared, "avant");
    int p[2];
    pipe(p);

    /* The kernel writes into a page still shared copy-on-write: the
     * child's sigprocmask stores the old mask into `marker`. */
    volatile unsigned long marker = 0x1111;
    pid_t child = fork();
    if (child < 0) return 1;
    if (child == 0) {
        unsigned long block = 1UL << (SIGUSR2 - 1);
        syscall(SYS_rt_sigprocmask, SIG_BLOCK, &block, &marker, 8);
        private_value = 99;          /* copy-on-write: parent must not see it */
        memset(heap, 'c', 1 << 20);
        strcpy(shared, "apres");     /* shared: parent must see it */
        write(p[1], "fils", 4);
        _exit(3);
    }
    int status;
    if (waitpid(child, &status, 0) != child) return 2;
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 3) { printf("spawn: statut %x\n", status); return 3; }
    if (private_value != 11 || heap[12345] != 'p') return 4;
    if (marker != 0x1111) { printf("spawn: ecriture noyau visible du parent (%lx)\n", marker); return 13; }
    if (strcmp(shared, "apres") != 0) return 5;
    char buf[8] = {0};
    if (read(p[0], buf, 4) != 4 || strcmp(buf, "fils") != 0) return 6;

    /* fork + execve */
    child = fork();
    if (child == 0) {
        char *argv[] = {HELLO_PATH, 0};
        execve(argv[0], argv, environ);
        _exit(100);
    }
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 7) return 7;

    /* posix_spawn (vfork + execve in musl) */
    char *argv2[] = {HELLO_PATH, 0};
    if (posix_spawn(&child, argv2[0], 0, 0, argv2, environ) != 0) return 8;
    if (waitpid(child, &status, 0) != child || WEXITSTATUS(status) != 7) return 9;

    /* kill a child that computes forever */
    child = fork();
    if (child == 0) { for (volatile long i = 0;; i++); }
    usleep(20000);
    kill(child, SIGKILL);
    if (waitpid(child, &status, 0) != child || !WIFSIGNALED(status) || WTERMSIG(status) != SIGKILL) return 10;

    if (waitpid(-1, &status, WNOHANG) != -1) return 11;  /* no children left */
    if (chld < 3) { printf("spawn: %d SIGCHLD recus\n", chld); return 12; }
    printf("spawn: fork COW, memfd partage, execve, posix_spawn, kill, waitpid, %d SIGCHLD conformes\n", chld);
    return 0;
}

/* Signals: handlers, masks, EINTR, pthread_kill, sigaltstack (11.5). */
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static volatile int got;
static volatile int on_alt;
static char altstack[16384];

static void handler(int sig, siginfo_t *info, void *ctx) {
    (void)info; (void)ctx;
    got = sig;
    char probe;
    on_alt = (&probe >= altstack && &probe < altstack + sizeof altstack);
}

static int pipefd[2];
static volatile int read_result, read_errno;
static void *reader(void *arg) {
    (void)arg;
    char c;
    read_result = read(pipefd[0], &c, 1);
    read_errno = errno;
    return 0;
}

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = handler;
    sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
    if (sigaction(SIGUSR1, &sa, 0) != 0) return 1;
    stack_t ss = {.ss_sp = altstack, .ss_size = sizeof altstack};
    if (sigaltstack(&ss, 0) != 0) return 2;

    raise(SIGUSR1);
    if (got != SIGUSR1) return 3;
    if (!on_alt) return 4;

    /* Blocked, then delivered on unblock. */
    got = 0;
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    pthread_sigmask(SIG_BLOCK, &set, 0);
    raise(SIGUSR1);
    if (got != 0) return 5;
    pthread_sigmask(SIG_UNBLOCK, &set, 0);
    if (got != SIGUSR1) return 6;

    /* A thread blocked in read is interrupted with EINTR. */
    sa.sa_flags = SA_SIGINFO; /* no SA_RESTART */
    sigaction(SIGUSR2, &sa, 0);
    pipe(pipefd);
    pthread_t t;
    pthread_create(&t, 0, reader, 0);
    usleep(50000);
    pthread_kill(t, SIGUSR2);
    pthread_join(t, 0);
    if (read_result != -1 || read_errno != EINTR) { printf("signals: read=%d errno=%d\n", read_result, read_errno); return 7; }

    /* SIGPIPE ignored: EPIPE instead of death. */
    signal(SIGPIPE, SIG_IGN);
    close(pipefd[0]);
    if (write(pipefd[1], "x", 1) != -1 || errno != EPIPE) return 8;

    printf("signals: gestionnaire, pile alternative, masque, EINTR, EPIPE conformes\n");
    return 0;
}

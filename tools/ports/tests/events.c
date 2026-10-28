/* Pipes, eventfd, epoll, poll timeouts, socketpair + SCM_RIGHTS, a large
 * message through a socket pair (11.5, 11.15). */
#define _GNU_SOURCE
#include <poll.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

static int efd;
static void *poke(void *arg) {
    (void)arg;
    usleep(30000);
    uint64_t one = 1;
    write(efd, &one, 8);
    return 0;
}

/* A large message through a socket pair: 8 MiB sent by one sendmsg and
 * received by recvmsg calls with an 8 MiB buffer, as Firefox's IPC does
 * with a large image. The kernel copies it in pieces; it once gathered it
 * whole in its heap and stopped there ("ALLOCATION ECHOUEE"). */
#define BIG (8 << 20)
static int big_sv[2];
static void *big_reader(void *arg) {
    unsigned char *in = malloc(BIG);
    size_t got = 0;
    while (got < BIG) {
        struct iovec iov = {in + got, BIG - got};
        struct msghdr m = {.msg_iov = &iov, .msg_iovlen = 1};
        ssize_t n = recvmsg(big_sv[1], &m, 0);
        if (n <= 0) break;
        got += (size_t)n;
    }
    long bad = got != BIG;
    for (size_t i = 0; i < got && !bad; i++)
        bad = in[i] != (unsigned char)(i * 7 + i / 4096);
    free(in);
    return (void *)bad;
}

static long ms_since(struct timespec *a) {
    struct timespec b;
    clock_gettime(CLOCK_MONOTONIC, &b);
    return (b.tv_sec - a->tv_sec) * 1000 + (b.tv_nsec - a->tv_nsec) / 1000000;
}

int main(void) {
    int p[2];
    if (pipe2(p, O_CLOEXEC) != 0) return 1;
    if (write(p[1], "abc", 3) != 3) return 2;
    char buf[16];
    if (read(p[0], buf, 16) != 3) return 3;

    /* epoll wakes on an eventfd written by another thread. */
    efd = eventfd(0, EFD_NONBLOCK);
    int ep = epoll_create1(EPOLL_CLOEXEC);
    struct epoll_event ev = {.events = EPOLLIN, .data.u64 = 42};
    if (epoll_ctl(ep, EPOLL_CTL_ADD, efd, &ev) != 0) return 4;
    pthread_t t;
    pthread_create(&t, 0, poke, 0);
    struct timespec start;
    clock_gettime(CLOCK_MONOTONIC, &start);
    struct epoll_event out;
    int n = epoll_wait(ep, &out, 1, 5000);
    long waited = ms_since(&start);
    pthread_join(t, 0);
    if (n != 1 || out.data.u64 != 42) return 5;
    if (waited < 20 || waited > 2000) { printf("events: epoll a attendu %ld ms\n", waited); return 6; }

    /* poll times out when nothing comes. */
    struct pollfd pf = {.fd = p[0], .events = POLLIN};
    clock_gettime(CLOCK_MONOTONIC, &start);
    if (poll(&pf, 1, 50) != 0) return 7;
    waited = ms_since(&start);
    if (waited < 45 || waited > 1000) { printf("events: poll a expire apres %ld ms\n", waited); return 8; }

    /* A descriptor passed through a socketpair. */
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) return 9;
    char c = 'x';
    struct iovec iov = {&c, 1};
    char control[CMSG_SPACE(sizeof(int))];
    struct msghdr msg = {.msg_iov = &iov, .msg_iovlen = 1, .msg_control = control, .msg_controllen = sizeof control};
    struct cmsghdr *cm = CMSG_FIRSTHDR(&msg);
    cm->cmsg_level = SOL_SOCKET;
    cm->cmsg_type = SCM_RIGHTS;
    cm->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(cm), &p[1], sizeof(int));
    if (sendmsg(sv[0], &msg, 0) != 1) return 10;
    char r;
    struct iovec riov = {&r, 1};
    char rcontrol[CMSG_SPACE(sizeof(int))];
    struct msghdr rmsg = {.msg_iov = &riov, .msg_iovlen = 1, .msg_control = rcontrol, .msg_controllen = sizeof rcontrol};
    if (recvmsg(sv[1], &rmsg, 0) != 1 || r != 'x') return 11;
    struct cmsghdr *rc = CMSG_FIRSTHDR(&rmsg);
    if (!rc || rc->cmsg_type != SCM_RIGHTS) return 12;
    int passed;
    memcpy(&passed, CMSG_DATA(rc), sizeof(int));
    if (write(passed, "via", 3) != 3) return 13;
    if (read(p[0], buf, 16) != 3 || memcmp(buf, "via", 3) != 0) return 14;

    /* End of file once every write end is closed. */
    close(p[1]);
    close(passed);
    if (read(p[0], buf, 16) != 0) return 15;

    if (socketpair(AF_UNIX, SOCK_STREAM, 0, big_sv) != 0) return 16;
    unsigned char *outbuf = malloc(BIG);
    for (size_t i = 0; i < BIG; i++)
        outbuf[i] = (unsigned char)(i * 7 + i / 4096);
    pthread_t reader;
    pthread_create(&reader, NULL, big_reader, NULL);
    struct iovec biov = {outbuf, BIG};
    struct msghdr bmsg = {.msg_iov = &biov, .msg_iovlen = 1};
    if (sendmsg(big_sv[0], &bmsg, 0) != BIG) return 17;
    void *bad;
    pthread_join(reader, &bad);
    if (bad) return 18;
    free(outbuf);

    printf("events: pipe, eventfd+epoll (%ld ms), poll, SCM_RIGHTS, message de 8 Mio conformes\n", waited);
    return 0;
}

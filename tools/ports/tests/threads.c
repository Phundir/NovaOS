/* Threads, mutexes, condition variables, TLS, join (phase 11.4).
 * Exit 0 when every check holds; the failed check's number otherwise. */
#include <pthread.h>
#include <stdio.h>
#include <unistd.h>

#define THREADS 8
#define ROUNDS 20000

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t cond = PTHREAD_COND_INITIALIZER;
static long counter;
static int turn;
static __thread long mine;

static void *worker(void *arg) {
    long id = (long)arg;
    mine = id * 1000;
    for (int i = 0; i < ROUNDS; i++) {
        pthread_mutex_lock(&lock);
        counter++;
        pthread_mutex_unlock(&lock);
        mine++;
    }
    /* Each thread waits for its turn: a chain of condition signals. */
    pthread_mutex_lock(&lock);
    while (turn != id)
        pthread_cond_wait(&cond, &lock);
    turn++;
    pthread_cond_broadcast(&cond);
    pthread_mutex_unlock(&lock);
    return (void *)(mine - id * 1000);
}

int main(void) {
    pthread_t t[THREADS];
    mine = -1;
    for (long i = 0; i < THREADS; i++)
        if (pthread_create(&t[i], 0, worker, (void *)i) != 0) {
            printf("threads: pthread_create %ld a echoue\n", i);
            return 1;
        }
    for (int i = 0; i < THREADS; i++) {
        void *r;
        if (pthread_join(t[i], &r) != 0) return 2;
        if ((long)r != ROUNDS) { printf("threads: TLS du thread %d = %ld\n", i, (long)r); return 3; }
    }
    if (counter != (long)THREADS * ROUNDS) { printf("threads: compteur %ld\n", counter); return 4; }
    if (mine != -1) return 5;
    if (turn != THREADS) return 6;
    printf("threads: %d threads, %ld increments sous verrou, TLS et conditions conformes\n", THREADS, counter);
    return 0;
}

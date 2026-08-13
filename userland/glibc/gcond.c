#define _GNU_SOURCE
#include <stdio.h>
#include <pthread.h>
#include <unistd.h>

/* CONDITION-VARIABLE test. A broadcast has to reach EVERY waiter. glibc implements
   it with futex REQUEUE/WAKE_OP, not plain WAKE: a kernel that answers those with
   "0 waiters" drops the wakeup in silence and the waiters sleep forever. That is
   what left chrome's raster workers idle while tiles piled up.

   Exit 157 = all waiters woke from one broadcast, and a signal wakes one more. */
#define N 6
static pthread_mutex_t m = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t cv = PTHREAD_COND_INITIALIZER;
static int go = 0, awake = 0;

static void *worker(void *arg) {
    (void)arg;
    pthread_mutex_lock(&m);
    while (!go) pthread_cond_wait(&cv, &m);
    awake++;
    pthread_mutex_unlock(&m);
    return 0;
}

int main(void) {
    pthread_t t[N];
    for (int i = 0; i < N; i++) {
        if (pthread_create(&t[i], 0, worker, 0) != 0) {
            printf("GCOND: pthread_create FAILED\n"); fflush(stdout); return 1;
        }
    }
    usleep(200000);                 /* let them all reach the wait */
    pthread_mutex_lock(&m);
    go = 1;
    pthread_cond_broadcast(&cv);    /* REQUEUE path: every waiter must come back */
    pthread_mutex_unlock(&m);

    for (int i = 0; i < N; i++) {
        if (pthread_join(t[i], 0) != 0) {
            printf("GCOND: join FAILED (a waiter never woke)\n"); fflush(stdout); return 2;
        }
    }
    printf("GCOND: broadcast woke %d of %d waiters\n", awake, N);
    if (awake != N) { fflush(stdout); return 3; }

    printf("GCOND: a broadcast reaches every waiter -> PASS\n");
    fflush(stdout);
    return 157;
}

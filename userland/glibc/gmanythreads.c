/* gmanythreads: [many-threads] (BUG-147) - 320 threads of one process alive at
 * the same time, as a browser session with 16 processes has (214 live tasks of
 * 256 in one W23 run, while the kernel had 256 task slots and 224 thread kernel
 * stacks). Every thread waits on one condition variable until all exist; then
 * all are released and joined. Exit 77 = all 320 created and joined. */
#include <pthread.h>
#include <stdio.h>
#include <string.h>

#define N 320
static pthread_mutex_t mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t go = PTHREAD_COND_INITIALIZER;
static int released = 0, arrived = 0;

static void *waiter(void *arg) {
    pthread_mutex_lock(&mu);
    arrived++;
    while (!released) pthread_cond_wait(&go, &mu);
    pthread_mutex_unlock(&mu);
    return arg;
}

int main(void) {
    static pthread_t t[N];
    pthread_attr_t at;
    pthread_attr_init(&at);
    pthread_attr_setstacksize(&at, 64 * 1024);
    int made = 0, err = 0;
    for (int i = 0; i < N; i++) {
        int e = pthread_create(&t[i], &at, waiter, (void *)(long)i);
        if (e) { err = e; break; }
        made++;
    }
    pthread_mutex_lock(&mu);
    released = 1;
    pthread_cond_broadcast(&go);
    pthread_mutex_unlock(&mu);
    int joined = 0;
    for (int i = 0; i < made; i++) {
        void *r = 0;
        if (pthread_join(t[i], &r) == 0 && (long)r == i) joined++;
    }
    int ok = made == N && joined == N;
    printf("GMANYTHREADS: %d of %d threads alive at once%s%s, %d joined -> %s\n", made, N,
           err ? ", pthread_create: " : "", err ? strerror(err) : "", joined, ok ? "PASS" : "FAIL");
    fflush(stdout);
    return ok ? 77 : 1;
}

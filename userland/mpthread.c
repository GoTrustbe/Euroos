/* EuroOS — REAL musl pthreads: pthread_create + pthread_join, linked unchanged
 * against musl. The worker thread (started by musl via clone()) increments a
 * shared counter; the main thread joins and reads the result. If this runs,
 * then the full pthread layer works on EuroOS' own clone + futex(busy-poll)
 * + CLONE_CHILD_CLEARTID mechanism. */
#include <pthread.h>
#include <unistd.h>

volatile long shared = 0;

static void *worker(void *arg) {
    (void)arg;
    for (long i = 0; i < 500000; i++) shared++;
    return 0;
}

static int slen(const char *s) { int n = 0; while (s[n]) n++; return n; }
static void emit(const char *s) { write(1, s, slen(s)); }

int main(void) {
    emit("mpthread: pthread_create + pthread_join (real musl pthreads)\n");
    pthread_t t;
    if (pthread_create(&t, 0, worker, 0) != 0) {
        emit("  pthread_create failed\n");
        return 1;
    }
    pthread_join(t, 0);

    char msg[72];
    int o = 0;
    const char *m = "  shared counter via pthread: ";
    while (*m) msg[o++] = *m++;
    long v = shared;
    char tmp[24];
    int n = 0;
    if (!v) tmp[n++] = '0';
    else while (v) { tmp[n++] = '0' + (v % 10); v /= 10; }
    for (int i = 0; i < n; i++) msg[o++] = tmp[n - 1 - i];
    msg[o++] = '\n';
    write(1, msg, o);
    return 0;
}

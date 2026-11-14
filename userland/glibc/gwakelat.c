/* gwakelat: [wakelat] (docs/design/SMP-SCHEDULER.md, step S2). Two threads pass a
 * turn back and forth on a futex while the kernel runs CPU-bound hogs beside them.
 * Each round the main thread sleeps 3 ms, stamps the TSC, wakes the partner and
 * then keeps working for 1 ms before it waits for the answer (a waker that goes on
 * running, as a browser thread that posts a task does). The partner records how
 * long it waited for the CPU after the wake (wake to run) and answers at once.
 * argv[1] = rounds, argv[2] = TSC cycles per microsecond (given by the kernel: the
 * vDSO clock moves in 10 ms steps). Prints one WAKELAT line, exit 77; 78 = a turn
 * was not answered within 5 s (lost wake). */
#include <linux/futex.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>
#include <x86intrin.h>

static _Atomic int turn;                 /* 0: the main thread's turn, 1: the partner's */
static volatile unsigned long long stamp; /* TSC at the last wake */
static int rounds;
static double cpu_us;                    /* TSC cycles per microsecond */
static unsigned *wake_us, *rt_us;
static volatile int lost;

static void fwake(void) { syscall(SYS_futex, &turn, FUTEX_WAKE_PRIVATE, 1, 0, 0, 0); }

/* Wait until the turn is `mine`; false after 5 s. */
static int fwait(int mine) {
    unsigned long long t0 = __rdtsc();
    while (atomic_load(&turn) != mine) {
        struct timespec to = {0, 100 * 1000 * 1000};
        syscall(SYS_futex, &turn, FUTEX_WAIT_PRIVATE, !mine, &to, 0, 0);
        if ((__rdtsc() - t0) / cpu_us > 5e6) return 0;
    }
    return 1;
}

static void work_us(unsigned us) {
    unsigned long long t0 = __rdtsc();
    while ((__rdtsc() - t0) / cpu_us < us) __asm__ volatile("pause");
}

static void *partner(void *arg) {
    (void)arg;
    for (int i = 0; i < rounds && !lost; i++) {
        if (!fwait(1)) { lost = 1; break; }
        wake_us[i] = (unsigned)((__rdtsc() - stamp) / cpu_us);
        atomic_store(&turn, 0);
        fwake();
    }
    return 0;
}

static int cmp(const void *a, const void *b) {
    unsigned x = *(const unsigned *)a, y = *(const unsigned *)b;
    return x < y ? -1 : x > y;
}

int main(int argc, char **argv) {
    rounds = argc > 1 ? atoi(argv[1]) : 400;
    cpu_us = argc > 2 ? atof(argv[2]) : 2000.0;
    if (rounds < 10) rounds = 10;
    if (cpu_us < 1) cpu_us = 1;
    wake_us = calloc(rounds, sizeof *wake_us);
    rt_us = calloc(rounds, sizeof *rt_us);
    pthread_t th;
    if (!wake_us || !rt_us || pthread_create(&th, 0, partner, 0)) { puts("WAKELAT: setup failed"); return 1; }
    int done = 0;
    for (int i = 0; i < rounds; i++) {
        struct timespec nap = {0, 3 * 1000 * 1000};
        nanosleep(&nap, 0);
        unsigned long long t = __rdtsc();
        stamp = t;
        atomic_store(&turn, 1);
        fwake();
        work_us(1000);
        if (!fwait(0)) { lost = 1; break; }
        rt_us[i] = (unsigned)((__rdtsc() - t) / cpu_us);
        done++;
    }
    if (lost) { atomic_store(&turn, 1); fwake(); }
    pthread_join(th, 0);
    qsort(wake_us, done, sizeof *wake_us, cmp);
    qsort(rt_us, done, sizeof *rt_us, cmp);
    int n = done ? done : 1;
    printf("WAKELAT: rounds=%d wake_p50_us=%u wake_p99_us=%u wake_max_us=%u rt_p50_us=%u rt_p99_us=%u rt_max_us=%u lost=%d\n",
           done, wake_us[n / 2], wake_us[n * 99 / 100], wake_us[n - 1], rt_us[n / 2], rt_us[n * 99 / 100], rt_us[n - 1], lost);
    fflush(stdout);
    return lost ? 78 : 77;
}

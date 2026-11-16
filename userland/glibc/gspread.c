/* gspread: [spread] (docs/design/SMP-SCHEDULER.md, step S5). argv[1] threads,
 * argv[2] work units per thread. Every thread does the same floating-point work
 * without system calls; the main thread waits for all of them. Prints
 * "SPREAD threads=N wall_ms=W sum=S" (W from CLOCK_MONOTONIC); exit 77. */
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static long units;
static double out[64];

static void *work(void *a) {
    long k = (long)a;
    double x = 1.0 + k * 1e-9, y = 0.5;
    for (long u = 0; u < units; u++)
        for (int i = 0; i < 100000; i++) {
            x = x * 1.0000001 + y * 1e-7;
            y = y * 0.9999999 + 1e-9;
        }
    out[k] = x + y;
    return NULL;
}

int main(int argc, char **argv) {
    int n = argc > 1 ? atoi(argv[1]) : 8;
    units = argc > 2 ? atol(argv[2]) : 100;
    if (n < 1) n = 1;
    if (n > 64) n = 64;
    struct timespec t0, t1;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    pthread_t t[64];
    for (long i = 0; i < n; i++) pthread_create(&t[i], NULL, work, (void *)i);
    for (int i = 0; i < n; i++) pthread_join(t[i], NULL);
    clock_gettime(CLOCK_MONOTONIC, &t1);
    double s = 0;
    for (int i = 0; i < n; i++) s += out[i];
    long ms = (t1.tv_sec - t0.tv_sec) * 1000 + (t1.tv_nsec - t0.tv_nsec) / 1000000;
    printf("SPREAD threads=%d wall_ms=%ld sum=%.6f\n", n, ms, s);
    fflush(stdout);
    return 77;
}

#define _GNU_SOURCE
#include <stdio.h>
#include <time.h>
#include <errno.h>

/* SLEEP test. nanosleep() that returns immediately is the same lie poll() used to
   tell: the caller asked for time to pass and it did not, so every paced loop
   becomes a spin and anything scheduled on a deadline never settles — which is
   what a compositor does between frames.

   Checks: a relative sleep really takes about as long as asked, and an ABSOLUTE
   deadline (clock_nanosleep TIMER_ABSTIME, what timer code actually uses) waits
   until that point and not past it. Exit 149 = time passes when asked. */
static long ms_since(struct timespec *t0) {
    struct timespec t1;
    clock_gettime(CLOCK_MONOTONIC, &t1);
    return (t1.tv_sec - t0->tv_sec) * 1000 + (t1.tv_nsec - t0->tv_nsec) / 1000000;
}

int main(void) {
    struct timespec t0, req;

    clock_gettime(CLOCK_MONOTONIC, &t0);
    req.tv_sec = 0; req.tv_nsec = 60 * 1000 * 1000;   /* 60 ms */
    if (nanosleep(&req, NULL) != 0) { printf("GSLEEP: nanosleep FAILED\n"); fflush(stdout); return 1; }
    long waited = ms_since(&t0);
    printf("GSLEEP: nanosleep(60ms) took %ld ms\n", waited);
    if (waited < 40) { printf("GSLEEP: returned early (a spin, not a sleep) FAILED\n"); fflush(stdout); return 2; }
    if (waited > 400) { printf("GSLEEP: massively overslept FAILED\n"); fflush(stdout); return 3; }

    clock_gettime(CLOCK_MONOTONIC, &t0);
    struct timespec at = t0;
    at.tv_nsec += 80 * 1000 * 1000;                   /* 80 ms from now, absolute */
    if (at.tv_nsec >= 1000000000) { at.tv_nsec -= 1000000000; at.tv_sec++; }
    int r = clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &at, NULL);
    waited = ms_since(&t0);
    printf("GSLEEP: clock_nanosleep(abs +80ms) -> %d after %ld ms\n", r, waited);
    if (r != 0) { printf("GSLEEP: absolute sleep FAILED\n"); fflush(stdout); return 4; }
    if (waited < 55) { printf("GSLEEP: absolute deadline ignored FAILED\n"); fflush(stdout); return 5; }

    printf("GSLEEP: time passes when a program asks for it -> PASS\n");
    fflush(stdout);
    return 149;
}

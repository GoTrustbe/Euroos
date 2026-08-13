#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <poll.h>
#include <time.h>

/* poll() TIMEOUT test. A poll that returns 0 says "your timeout expired", so a
   kernel that answers 0 straight away turns every wait into a spin: chrome's
   compositor thread polled millions of times per second while waiting for a frame,
   so no frame was ever produced and no screenshot came back.

   Checks:
     1. poll(no data, 60 ms) returns 0 only AFTER the time really passed,
     2. poll(data waiting, 60 ms) returns 1 immediately,
     3. poll(nfds=0, 40 ms) sleeps too (the plain "wait a while" idiom).
   Exit 143 = a timeout is a duration, not an instant answer. */
static long ms_since(struct timespec *t0) {
    struct timespec t1;
    clock_gettime(CLOCK_MONOTONIC, &t1);
    return (t1.tv_sec - t0->tv_sec) * 1000 + (t1.tv_nsec - t0->tv_nsec) / 1000000;
}

int main(void) {
    printf("GPOLL: start\n"); fflush(stdout);
    int fds[2];
    if (pipe(fds) != 0) { printf("GPOLL: pipe FAILED\n"); fflush(stdout); return 1; }

    struct pollfd p = { .fd = fds[0], .events = POLLIN };
    struct timespec t0;

    /* 1. Nothing to read: the call must take about as long as it promised. */
    clock_gettime(CLOCK_MONOTONIC, &t0);
    int r = poll(&p, 1, 60);
    long waited = ms_since(&t0);
    printf("GPOLL: empty poll(60ms) -> %d after %ld ms\n", r, waited);
    if (r != 0) { printf("GPOLL: expected 0 FAILED\n"); fflush(stdout); return 2; }
    if (waited < 40) { printf("GPOLL: returned instantly (a spin, not a wait) FAILED\n"); fflush(stdout); return 3; }

    /* 2. Data waiting: return at once, no waiting. */
    if (write(fds[1], "x", 1) != 1) { printf("GPOLL: write FAILED\n"); fflush(stdout); return 4; }
    clock_gettime(CLOCK_MONOTONIC, &t0);
    p.revents = 0;
    r = poll(&p, 1, 60);
    waited = ms_since(&t0);
    if (r != 1 || !(p.revents & POLLIN)) { printf("GPOLL: ready poll -> %d FAILED\n", r); fflush(stdout); return 5; }
    if (waited > 40) { printf("GPOLL: ready poll waited %ld ms FAILED\n", waited); fflush(stdout); return 6; }

    /* 3. poll(NULL, 0, ms) is the plain "sleep a while" idiom. */
    clock_gettime(CLOCK_MONOTONIC, &t0);
    r = poll(NULL, 0, 40);
    waited = ms_since(&t0);
    printf("GPOLL: poll(0 fds, 40ms) -> %d after %ld ms\n", r, waited);
    if (r != 0 || waited < 25) { printf("GPOLL: zero-fd poll did not wait FAILED\n"); fflush(stdout); return 7; }

    printf("GPOLL: a timeout is a duration, not an instant answer -> PASS\n");
    fflush(stdout);
    return 143;
}

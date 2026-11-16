/* gtlbsmp: [tlb-smp] (docs/design/SMP-SCHEDULER.md, step S4). A reader thread (the
 * kernel places it on an application processor) reads one page in a loop. The
 * main thread (on the BSP) changes what that address means and tells the reader;
 * after that call has returned the reader must never read the old contents (a
 * stale TLB entry); a page fault on the way is fine, its time is reported. Modes, argv[2] rounds each:
 *   dontneed  madvise(MADV_DONTNEED): the page reads zero afterwards
 *   remap     mmap(MAP_FIXED) of a fresh page over the old one, with a new value
 *   cowbreak  fork; the child keeps the page shared; the parent writes it
 *             (copy-on-write break in the parent: a new frame for the same address)
 * argv[1] = TSC cycles per microsecond. Prints "TLBSMP mode=... rounds=N stale=S
 * max_us=M" (the longest time to the new contents) per mode and "TLBSMP all ok" or "TLBSMP FAILED"; exit 77 ok, 1 failed. */
#define _GNU_SOURCE
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>
#include <x86intrin.h>

static volatile uint32_t *P;
static _Atomic long flag = -1, done = -1;
static _Atomic uint32_t want, last_seen, old_val;
static _Atomic uint64_t t_flag, lat_max, stale;
static _Atomic int stop;
static uint64_t cyc_us = 1000;

static void *reader(void *a) {
    (void)a;
    long handled = -1;
    while (!atomic_load(&stop)) {
        uint32_t v = *P;
        atomic_store(&last_seen, v);
        long f = atomic_load(&flag);
        if (f != handled && f >= 0) {
            uint32_t w = atomic_load(&want), old = atomic_load(&old_val);
            uint64_t t0 = atomic_load(&t_flag), t;
            int saw_old = 0;
            for (;;) {
                /* After the flag the main thread's call has returned: a read
                 * of the old value went through a stale TLB entry. A fault is
                 * fine (the BSP serves it; its time is reported, not judged). */
                v = *P;
                t = __rdtsc();
                if (v == w) break;
                if (v == old) saw_old = 1;
                if (t - t0 > 2000000 * cyc_us) { saw_old = 1; break; }
            }
            uint64_t us = (t - t0) / cyc_us;
            if (saw_old) atomic_fetch_add(&stale, 1);
            if (us > atomic_load(&lat_max)) atomic_store(&lat_max, us);
            handled = f;
            atomic_store(&done, f);
        }
    }
    return NULL;
}

static void wait_seen(uint32_t v) {
    uint64_t t0 = __rdtsc();
    while (atomic_load(&last_seen) != v && __rdtsc() - t0 < 2000000 * cyc_us) {}
}

static void wait_done(long k) {
    uint64_t t0 = __rdtsc();
    while (atomic_load(&done) != k && __rdtsc() - t0 < 2000000 * cyc_us) {}
}

static int run(const char *mode, long rounds, long *round_base) {
    atomic_store(&stale, 0);
    atomic_store(&lat_max, 0);
    for (long r = 0; r < rounds; r++) {
        long k = (*round_base)++;
        uint32_t a = (uint32_t)(2 * k + 1), b = (uint32_t)(2 * k + 2);
        P[0] = a;
        wait_seen(a);
        int fds[2] = {-1, -1};
        pid_t child = -1;
        if (!strcmp(mode, "dontneed")) {
            madvise((void *)P, 4096, MADV_DONTNEED);
            b = 0;
        } else if (!strcmp(mode, "remap")) {
            /* MAP_FIXED over the mapping replaces it in one call (no hole the reader could hit) */
            void *q = mmap((void *)P, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
            if (q != (void *)P) { printf("TLBSMP remap: mmap fixed failed\n"); return 1; }
            P[0] = b;
        } else {
            if (pipe(fds) != 0) return 1;
            child = fork();
            if (child == 0) {
                char c;
                if (read(fds[0], &c, 1) < 0) _exit(4); /* keep the page shared until the parent wrote */
                _exit(P[0] == a ? 0 : 3);
            }
            P[0] = b; /* copy-on-write break in the parent */
        }
        atomic_store(&old_val, a);
        atomic_store(&want, b);
        atomic_store(&t_flag, __rdtsc());
        atomic_store(&flag, k);
        wait_done(k);
        if (child > 0) {
            int st = 0;
            if (write(fds[1], "x", 1) != 1) atomic_fetch_add(&stale, 1);
            waitpid(child, &st, 0);
            close(fds[0]);
            close(fds[1]);
            if (!WIFEXITED(st) || WEXITSTATUS(st) != 0) atomic_fetch_add(&stale, 1); /* the child saw the parent write */
        }
    }
    printf("TLBSMP mode=%s rounds=%ld stale=%lu max_us=%lu\n", mode, rounds,
           (unsigned long)atomic_load(&stale), (unsigned long)atomic_load(&lat_max));
    return atomic_load(&stale) != 0;
}

int main(int argc, char **argv) {
    cyc_us = argc > 1 ? strtoull(argv[1], 0, 10) : 1000;
    long rounds = argc > 2 ? atol(argv[2]) : 1000;
    if (cyc_us == 0) cyc_us = 1000;
    /* The first page of a large mapping: demand-paged (a small one would come
     * from the arena window, where MADV_DONTNEED is a no-op and a fork copies). */
    P = mmap(NULL, 64 << 20, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (P == MAP_FAILED) { printf("TLBSMP mmap failed\n"); return 1; }
    P[0] = 1;
    pthread_t t;
    pthread_create(&t, NULL, reader, NULL);
    long base = 0;
    int bad = 0;
    bad += run("dontneed", rounds, &base);
    bad += run("remap", rounds, &base);
    bad += run("cowbreak", rounds, &base);
    atomic_store(&stop, 1);
    pthread_join(t, NULL);
    printf("TLBSMP %s\n", bad ? "FAILED" : "all ok");
    fflush(stdout);
    return bad ? 1 : 77;
}

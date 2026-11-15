/* gapuser: [ap-user] and [cow-smp] (docs/design/SMP-SCHEDULER.md, steps S3 and S4).
 * The main thread does the work of one worker itself first (the reference: the
 * main thread is the foreground program and runs on the BSP), then starts
 * argv[1] worker threads (the kernel pins them to the application processors
 * in turn) that each do the same work: a floating-point checksum (SSE, and AVX2
 * when the CPU has it), argv[2] raw system calls and argv[3] pages of fresh
 * anonymous memory touched one by one (demand faults). Every 512 iterations a
 * worker reads the APIC id of the CPU it runs on with CPUID (ring 3) into a mask.
 * Mode "cow" (argv[4]): the process forks while its workers run; the child
 * rewrites every page of a buffer it shares copy-on-write with the parent while
 * the parent's workers keep reading that buffer on the APs. The parent must still
 * see its own bytes, the child its new ones (exit 0 of the child).
 * Prints one APUSER line per thread and "APUSER ... all ok" or "FAILED"; exit 77 ok, 1 failed. */
#define _GNU_SOURCE
#include <cpuid.h>
#include <immintrin.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define MAXT 32
static long nsys, npages;
static int have_avx2;
static volatile unsigned char *shared; /* cow mode: a buffer shared with a forked child */
static long shared_len;
static int cow_mode;
static volatile int cow_done; /* cow mode: the child has exited, the workers may stop writing */

struct res { uint64_t sum; uint64_t apic_mask; long sys_ok; long faults; long cow_bad; void *mem; };
static struct res R[MAXT + 1];

static unsigned apic_id(void) {
    unsigned a, b, c, d;
    __cpuid(1, a, b, c, d);
    return b >> 24;
}

__attribute__((target("avx2"))) static uint64_t avx_part(uint64_t seed) {
    __m256d acc = _mm256_set1_pd(1.0);
    __m256d step = _mm256_set_pd(1.0000001, 0.9999999, 1.0000003, 0.9999997);
    for (int i = 0; i < 20000; i++) {
        acc = _mm256_mul_pd(acc, step);
        acc = _mm256_add_pd(acc, _mm256_set1_pd((double)((seed + i) & 7) * 1e-9));
    }
    double o[4];
    _mm256_storeu_pd(o, acc);
    uint64_t r = 0, x;
    for (int i = 0; i < 4; i++) { memcpy(&x, &o[i], 8); r = r * 31 + x; }
    return r;
}

static void work(struct res *r) {
    double a = 1.0, b = 0.5;
    uint64_t sum = 0, x;
    long pid = syscall(SYS_getpid);
    for (long i = 0; i < 400000; i++) {
        a = a * 1.0000001 + b * 1e-7;
        b = b * 0.9999999 + (double)(i & 15) * 1e-9;
        if ((i & 511) == 0) r->apic_mask |= 1ull << (apic_id() & 63);
        if (i % 20 == 0 && r->sys_ok < nsys && syscall(SYS_getpid) == pid) r->sys_ok++;
        if (cow_mode && shared && (i & 1023) == 0)
            for (long p = 0; p < shared_len; p += 4096) {
                if (shared[p] != (unsigned char)(p >> 12)) r->cow_bad++;
                shared[p + 2048] = (unsigned char)(i >> 10); /* our writes: never seen by the child */
            }
    }
    /* cow mode: keep writing until the child is done (writes through a stale TLB entry would reach it) */
    for (long k = 0; cow_mode && shared && !cow_done; k++)
        for (long p = 0; p < shared_len; p += 4096) {
            if (shared[p] != (unsigned char)(p >> 12)) r->cow_bad++;
            shared[p + 2048] = (unsigned char)k;
        }
    while (r->sys_ok < nsys && syscall(SYS_getpid) == pid) r->sys_ok++;
    memcpy(&x, &a, 8); sum = sum * 31 + x;
    memcpy(&x, &b, 8); sum = sum * 31 + x;
    if (have_avx2) sum = sum * 31 + avx_part(7);
    unsigned char *m = mmap(NULL, npages * 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) { r->faults = -1; r->sum = sum; return; }
    for (long p = 0; p < npages; p++) {
        m[p * 4096] = (unsigned char)(p * 7 + 3);
        m[p * 4096 + 4095] = (unsigned char)(p * 13 + 1);
        r->faults++;
    }
    for (long p = 0; p < npages; p++) sum = sum * 31 + m[p * 4096] + m[p * 4096 + 4095];
    r->mem = m;
    r->sum = sum;
}

static void *thr(void *arg) {
    work((struct res *)arg);
    return NULL;
}

int main(int argc, char **argv) {
    int n = argc > 1 ? atoi(argv[1]) : 2;
    nsys = argc > 2 ? atol(argv[2]) : 10000;
    npages = argc > 3 ? atol(argv[3]) : 1000;
    cow_mode = argc > 4 && strcmp(argv[4], "cow") == 0;
    if (n < 1) n = 1;
    if (n > MAXT) n = MAXT;
    __builtin_cpu_init();
    have_avx2 = __builtin_cpu_supports("avx2");
    unsigned bsp = apic_id();
    work(&R[MAXT]);
    uint64_t ref = R[MAXT].sum;
    munmap(R[MAXT].mem, npages * 4096);
    if (cow_mode) {
        shared_len = 256 * 4096;
        unsigned char *b = malloc(shared_len + 4096);
        b = (unsigned char *)(((uintptr_t)b + 4095) & ~(uintptr_t)4095);
        for (long p = 0; p < shared_len; p += 4096) memset(b + p, (int)(p >> 12) & 0xff, 4096);
        shared = b;
    }
    pthread_t t[MAXT];
    for (int i = 0; i < n; i++) pthread_create(&t[i], NULL, thr, &R[i]);
    pid_t child = -1;
    int cst = -1;
    if (cow_mode) {
        child = fork();
        if (child == 0) {
            int bad = 0;
            unsigned char v1[256];
            for (long p = 0; p < shared_len; p += 4096) v1[p >> 12] = shared[p + 2048];
            for (int k = 0; k < 200; k++) syscall(SYS_sched_yield);
            for (long p = 0; p < shared_len; p += 4096)
                if (shared[p + 2048] != v1[p >> 12]) bad++; /* the parent wrote into our copy */
            for (int round = 0; round < 20; round++)
                for (long p = 0; p < shared_len; p += 4096) {
                    shared[p] = (unsigned char)(0xA5 ^ round);
                    if (shared[p] != (unsigned char)(0xA5 ^ round)) bad++;
                }
            _exit(bad ? 3 : 0);
        }
        if (child > 0) waitpid(child, &cst, 0);
        cow_done = 1;
    }
    int bad = 0;
    for (int i = 0; i < n; i++) {
        pthread_join(t[i], NULL);
        int ok = R[i].sum == ref && R[i].sys_ok == nsys && R[i].faults == npages && R[i].cow_bad == 0;
        if (!ok) bad++;
        printf("APUSER thread %d apic-mask=%#llx sum=%#llx ref=%#llx syscalls=%ld faults=%ld cow-bad=%ld %s\n", i,
               (unsigned long long)R[i].apic_mask, (unsigned long long)R[i].sum, (unsigned long long)ref,
               R[i].sys_ok, R[i].faults, R[i].cow_bad, ok ? "ok" : "WRONG");
        munmap(R[i].mem, npages * 4096);
    }
    if (cow_mode) {
        int pbad = 0;
        for (long p = 0; p < shared_len; p += 4096)
            if (shared[p] != (unsigned char)(p >> 12)) pbad++;
        int cok = child > 0 && WIFEXITED(cst) && WEXITSTATUS(cst) == 0;
        if (!cok || pbad) bad++;
        printf("APUSER cow child=%s parent-pages-changed=%d\n", cok ? "ok" : "WRONG", pbad);
    }
    printf("APUSER bsp-apic=%u avx2=%d threads=%d %s\n", bsp, have_avx2, n, bad ? "FAILED" : "all ok");
    fflush(stdout);
    return bad ? 1 : 77;
}

#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <stdint.h>
#include <immintrin.h>
#include <pthread.h>

/* Do VECTOR REGISTERS survive a task switch? Every crypto library computes in
   xmm/ymm, and a kernel that fails to preserve that state across a switch
   produces wrong answers with no fault anywhere - which is exactly what a
   failed self-test inside a crypto token looks like from outside.
   NSS's software token refuses to initialise here with CKR_DEVICE_ERROR, and
   its power-up tests are the obvious suspect.

   Each thread fills ymm0-ymm7 with a pattern of its own, makes syscalls so the
   scheduler can preempt it, and checks the registers still hold that pattern.

   Exit 159 = every thread's registers survived every round. */

#define ROUNDS 200

static volatile int failures = 0;

static void *worker(void *arg) {
    long id = (long)arg;
    for (int r = 0; r < ROUNDS; r++) {
        __m256i v0 = _mm256_set1_epi32((int)(id * 1000 + r));
        __m256i v1 = _mm256_set1_epi32((int)(id * 1000 + r + 1));
        __m256i v2 = _mm256_set1_epi32((int)(id * 1000 + r + 2));
        __m256i v3 = _mm256_set1_epi32((int)(id * 1000 + r + 3));
        /* Syscalls in between: the scheduler preempts and other threads run,
           each with their own vector state. */
        (void)getpid();
        (void)getppid();
        __m256i s = _mm256_add_epi32(_mm256_add_epi32(v0, v1), _mm256_add_epi32(v2, v3));
        int out[8];
        memcpy(out, &s, sizeof out);
        int want = 4 * (int)(id * 1000 + r) + 6;
        for (int i = 0; i < 8; i++) {
            if (out[i] != want) {
                printf("GVEC: thread %ld round %d lane %d: %d != %d\n", id, r, i, out[i], want);
                fflush(stdout);
                __atomic_fetch_add(&failures, 1, __ATOMIC_SEQ_CST);
                return NULL;
            }
        }
    }
    return NULL;
}

int main(void) {
    pthread_t t[4];
    for (long i = 0; i < 4; i++) {
        if (pthread_create(&t[i], NULL, worker, (void *)i) != 0) {
            printf("GVEC: pthread_create FAILED\n"); fflush(stdout); return 1;
        }
    }
    for (int i = 0; i < 4; i++) pthread_join(t[i], NULL);
    if (failures) { printf("GVEC: %d corrupted vector results\n", failures); fflush(stdout); return 2; }
    printf("GVEC: vector registers survive task switches (4 threads x %d rounds) -> PASS\n", ROUNDS);
    fflush(stdout);
    return 159;
}

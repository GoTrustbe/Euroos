#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>

/* BRK test: memory gained through the program break reads as ZEROS, every time.
   glibc's calloc skips its own memset for chunks fresh from the kernel, so a brk
   that re-exposes old bytes turns calloc into an uninitialized-memory generator —
   fontconfig walked a 0xFF… "pointer" out of exactly such a hash table.

   Grow, poison, shrink, regrow: the regrown window must be zeros again, as it is
   on Linux (shrink unmaps; regrowth maps fresh pages). Exit 163 = clean. */
int main(void) {
    const long N = 1 << 20;
    void *start = sbrk(0);
    if (sbrk(N) == (void *)-1) { printf("GBRK: grow FAILED\n"); fflush(stdout); return 1; }
    unsigned char *p = start;
    for (long i = 0; i < N; i += 4096)
        if (p[i]) { printf("GBRK: fresh brk memory not zero at +%ld\n", i); fflush(stdout); return 2; }

    memset(p, 0xFF, N);                    /* poison */
    if (sbrk(-N) == (void *)-1) { printf("GBRK: shrink FAILED\n"); fflush(stdout); return 3; }
    if (sbrk(N) == (void *)-1) { printf("GBRK: regrow FAILED\n"); fflush(stdout); return 4; }
    for (long i = 0; i < N; i += 4096)
        if (p[i]) { printf("GBRK: regrown brk memory not zero at +%ld (old bytes leak through)\n", i); fflush(stdout); return 5; }

    printf("GBRK: brk memory is zero on every growth -> PASS\n");
    fflush(stdout);
    return 163;
}

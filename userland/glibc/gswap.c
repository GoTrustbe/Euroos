// gswap: swap under memory pressure, measured from inside a process (plan J3/N2).
// Allocates MORE anonymous memory than the kernel's demand pool holds (the
// kernel passes the amount in MiB as argv[1]), fills it with an index-based
// pattern, then verifies the whole range twice. With swap, the kernel pages
// the oldest pages out to the EuroSwap partition when the pool runs dry and
// brings them back on access; the pattern survives. Without swap the process
// is killed at the first page past the pool, and that is the failure signature.
// exit 0 = verified; 3 = mismatch; 2 = usage/malloc.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>

int main(int argc, char **argv) {
    unsigned long mib = argc > 1 ? strtoul(argv[1], 0, 10) : 64;
    if (mib < 1 || mib > 4096) { printf("gswap: bad size\n"); return 2; }
    size_t n = (size_t)mib << 20;
    uint32_t *p = malloc(n);
    if (!p) { printf("gswap: malloc(%lu MiB) failed\n", mib); return 2; }
    size_t words = n / 4;
    for (size_t i = 0; i < words; i += 1024) p[i] = (uint32_t)(i * 2654435761u) ^ 0xA5A5A5A5u; /* one word per page */
    printf("gswap: %lu MiB touched (%lu pages)\n", mib, (unsigned long)(n / 4096));
    fflush(stdout);
    for (int pass = 0; pass < 2; pass++) {
        size_t bad = 0;
        for (size_t i = 0; i < words; i += 1024)
            if (p[i] != ((uint32_t)(i * 2654435761u) ^ 0xA5A5A5A5u)) bad++;
        printf("gswap: pass %d: %lu bad pages\n", pass, (unsigned long)bad);
        if (bad) return 3;
    }
    printf("gswap: %lu MiB verified twice, OK\n", mib);
    return 0;
}

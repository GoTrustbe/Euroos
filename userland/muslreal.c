/* EuroOS — a REAL binary linked against musl libc (static-PIE). No custom
 * syscall stubs: this uses printf/malloc/strlen from musl, which talk to
 * EuroKernel via the Linux syscall ABI. If this runs, then EuroKernel
 * runs unmodified musl userspace. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(int argc, char **argv) {
    printf("Hello from a REAL musl-libc binary on EuroKernel!\n");
    printf("  argc=%d, argv[0]=%s\n", argc, argv[0]);

    char *p = malloc(64);
    strcpy(p, "malloc + strcpy + printf via musl libc");
    size_t n = strlen(p);
    printf("  buffer (%zu bytes): %s\n", n, p);
    free(p);

    int sum = 0;
    for (int i = 1; i <= 10; i++) sum += i;
    printf("  sum(1..10) = %d (musl runs ordinary C runtime)\n", sum);
    return 0;
}

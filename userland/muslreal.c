/* EuroOS — een ECHTE binary gelinkt tegen musl libc (static-PIE). Geen eigen
 * syscall-stubs: dit gebruikt printf/malloc/strlen uit musl, die via de Linux
 * syscall-ABI met EuroKernel praten. Draait dit, dan draait EuroKernel
 * ongewijzigde musl-userspace. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(int argc, char **argv) {
    printf("Hallo vanuit een ECHTE musl-libc binary op EuroKernel!\n");
    printf("  argc=%d, argv[0]=%s\n", argc, argv[0]);

    char *p = malloc(64);
    strcpy(p, "malloc + strcpy + printf via musl libc");
    size_t n = strlen(p);
    printf("  buffer (%zu bytes): %s\n", n, p);
    free(p);

    int sum = 0;
    for (int i = 1; i <= 10; i++) sum += i;
    printf("  som(1..10) = %d (musl draait gewone C-runtime)\n", sum);
    return 0;
}

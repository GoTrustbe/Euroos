/* EuroOS — a musl program that is NOT part of the boot set. It is installed
 * by name via the shell (`install msum`), where the kernel verifies its
 * Ed25519 signature before writing it into EuroFS and registering it.
 * Sums the integers from argv. */
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
    long sum = 0;
    for (int i = 1; i < argc; i++) {
        sum += atol(argv[i]);
    }
    printf("sum of %d numbers = %ld\n", argc - 1, sum);
    return 0;
}

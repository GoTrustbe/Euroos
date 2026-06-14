/* EuroOS — a real musl binary that does FILE I/O via the C standard library:
 * fopen/fgets/fclose. musl translates this to openat/fstat/read/close (Linux ABI),
 * which EuroKernel handles against its EuroFS VFS. Proves that unmodified musl
 * programs can read the real filesystem. */
#include <stdio.h>

int main(void) {
    const char *path = "/etc/eurokernel.conf";
    printf("musl reads %s via fopen/fgets:\n", path);

    FILE *f = fopen(path, "r");
    if (!f) {
        printf("  fopen() FAILED\n");
        return 1;
    }
    char line[128];
    int n = 0;
    while (fgets(line, sizeof line, f)) {
        printf("  | %s", line);
        n++;
    }
    fclose(f);
    printf("  (%d lines read, fclose OK)\n", n);
    return 0;
}

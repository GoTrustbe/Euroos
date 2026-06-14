/* EuroOS — real musl binary that WRITES A FILE: fopen("w") + fprintf +
 * fclose. musl translates this into openat(O_CREAT|O_TRUNC) + writev + close,
 * which EuroKernel writes to its VFS (and the shell syncs back to EuroFS).
 * Proves writable userspace files. Usage: mwrite <file> <text> */
#include <stdio.h>
#include <string.h>

int main(int argc, char **argv) {
    if (argc < 3) {
        printf("usage: mwrite <file> <text>\n");
        return 1;
    }
    FILE *f = fopen(argv[1], "w");
    if (!f) {
        printf("mwrite: cannot create '%s'\n", argv[1]);
        return 1;
    }
    fprintf(f, "%s\n", argv[2]);
    fclose(f);
    printf("mwrite: wrote %zu bytes to %s\n", strlen(argv[2]) + 1, argv[1]);
    return 0;
}

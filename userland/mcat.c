/* EuroOS — a real `cat` linked against musl: opens the file from argv[1] and
 * prints the contents. Proves that the shell's ARGUMENTS flow via the SysV stack
 * all the way into main(argc, argv) of an unmodified musl binary. */
#include <stdio.h>

int main(int argc, char **argv) {
    if (argc < 2) {
        printf("usage: mcat <file>\n");
        return 1;
    }
    FILE *f = fopen(argv[1], "r");
    if (!f) {
        printf("mcat: cannot open '%s'\n", argv[1]);
        return 1;
    }
    printf("mcat %s (argc=%d):\n", argv[1], argc);
    char buf[256];
    size_t n;
    while ((n = fread(buf, 1, sizeof buf, f)) > 0) {
        fwrite(buf, 1, n, stdout);
    }
    fclose(f);
    return 0;
}

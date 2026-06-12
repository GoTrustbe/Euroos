/* EuroOS — echte musl-binary die BESTANDS-I/O doet via de C-standaardbibliotheek:
 * fopen/fgets/fclose. musl vertaalt dit naar openat/fstat/read/close (Linux-ABI),
 * die EuroKernel tegen zijn EuroFS-VFS afhandelt. Bewijst dat ongewijzigde musl-
 * programma's het echte filesysteem kunnen lezen. */
#include <stdio.h>

int main(void) {
    const char *path = "/etc/eurokernel.conf";
    printf("musl leest %s via fopen/fgets:\n", path);

    FILE *f = fopen(path, "r");
    if (!f) {
        printf("  fopen() MISLUKT\n");
        return 1;
    }
    char line[128];
    int n = 0;
    while (fgets(line, sizeof line, f)) {
        printf("  | %s", line);
        n++;
    }
    fclose(f);
    printf("  (%d regels gelezen, fclose OK)\n", n);
    return 0;
}

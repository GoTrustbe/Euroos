/* EuroOS — echte musl-binary die een BESTAND SCHRIJFT: fopen("w") + fprintf +
 * fclose. musl vertaalt dit naar openat(O_CREAT|O_TRUNC) + writev + close, die
 * EuroKernel naar zijn VFS schrijft (en de shell synct terug naar EuroFS).
 * Bewijst schrijfbare userspace-bestanden. Gebruik: mwrite <bestand> <tekst> */
#include <stdio.h>
#include <string.h>

int main(int argc, char **argv) {
    if (argc < 3) {
        printf("gebruik: mwrite <bestand> <tekst>\n");
        return 1;
    }
    FILE *f = fopen(argv[1], "w");
    if (!f) {
        printf("mwrite: kan '%s' niet maken\n", argv[1]);
        return 1;
    }
    fprintf(f, "%s\n", argv[2]);
    fclose(f);
    printf("mwrite: %zu bytes naar %s geschreven\n", strlen(argv[2]) + 1, argv[1]);
    return 0;
}

/* EuroOS — een echte `cat` gelinkt tegen musl: opent het bestand uit argv[1] en
 * print de inhoud. Bewijst dat ARGUMENTEN van de shell via de SysV-stack tot in
 * main(argc, argv) van een ongewijzigde musl-binary doorstromen. */
#include <stdio.h>

int main(int argc, char **argv) {
    if (argc < 2) {
        printf("gebruik: mcat <bestand>\n");
        return 1;
    }
    FILE *f = fopen(argv[1], "r");
    if (!f) {
        printf("mcat: kan '%s' niet openen\n", argv[1]);
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

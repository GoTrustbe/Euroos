/* EuroOS — musl `echo`: print de argumenten (argv[1..]) gescheiden door spaties.
 * Schone bron voor pipe-demo's (geen ruis-header). */
#include <stdio.h>

int main(int argc, char **argv) {
    for (int i = 1; i < argc; i++) {
        fputs(argv[i], stdout);
        if (i < argc - 1) {
            fputc(' ', stdout);
        }
    }
    fputc('\n', stdout);
    return 0;
}

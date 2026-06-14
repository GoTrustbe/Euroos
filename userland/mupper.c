/* EuroOS — musl filter: read STDIN and write everything in UPPERCASE to stdout.
 * Reads fd 0 (getchar -> read(0)); demonstrates pipes: `mecho ... | mupper`. */
#include <stdio.h>
#include <ctype.h>

int main(void) {
    int c;
    while ((c = getchar()) != EOF) {
        putchar(toupper(c));
    }
    return 0;
}

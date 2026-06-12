/* EuroOS — musl-filter: lees STDIN en schrijf alles in HOOFDLETTERS naar stdout.
 * Leest fd 0 (getchar -> read(0)); bewijst pipes: `mecho ... | mupper`. */
#include <stdio.h>
#include <ctype.h>

int main(void) {
    int c;
    while ((c = getchar()) != EOF) {
        putchar(toupper(c));
    }
    return 0;
}

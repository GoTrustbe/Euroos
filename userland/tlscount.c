/* EuroOS — bewijs van het PREEMPTIEVE per-proces-model. Dit is een gewone musl
 * static-PIE binary met een THREAD-LOCAL teller (`__thread`). musl zet bij start
 * een eigen TLS-blok op en laadt FS_BASE; de teller wordt dus FS-relatief
 * benaderd. Draaien er twee instanties tegelijk en wisselt de scheduler ze
 * preemptief af, dan blijven hun tellers ALLEEN onafhankelijk als de kernel
 * FS_BASE per proces bewaart/herstelt bij elke context-switch.
 *
 * Geen printf/malloc — alleen getpid() + write() — om de syscall-set klein te
 * houden. De lus eindigt nooit (een gescheduelde achtergrondtaak). */
#include <unistd.h>

__thread volatile unsigned long counter = 0;

static int utoa(unsigned long v, char *b) {
    char t[24];
    int n = 0;
    if (v == 0) {
        b[0] = '0';
        return 1;
    }
    while (v) {
        t[n++] = '0' + (v % 10);
        v /= 10;
    }
    for (int i = 0; i < n; i++) {
        b[i] = t[n - 1 - i];
    }
    return n;
}

int main(void) {
    long pid = getpid();
    char pbuf[24];
    int pn = utoa((unsigned long)pid, pbuf);

    unsigned long i = 0;
    for (;;) {
        counter++; /* THREAD-LOCAL (fs-relatief) */
        i++;
        if ((i & 0x1FFFF) == 0) {
            char msg[64];
            int o = 0;
            const char *p = "tls-proc ";
            while (*p) msg[o++] = *p++;
            for (int k = 0; k < pn; k++) msg[o++] = pbuf[k];
            const char *q = ": teller=";
            while (*q) msg[o++] = *q++;
            o += utoa(counter, msg + o);
            msg[o++] = '\n';
            write(1, msg, o);
        }
    }
}

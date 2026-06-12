/* EuroOS — een ECHT 'job'-proces: het doet rekenwerk (priemgetallen tellen),
 * rapporteert het resultaat en sluit NETJES af met exit(0). De kernel ruimt het
 * daarna op (frames vrij). Dit is de nette exit-route van de proces-levenscyclus
 * (naast de isolatie-kill van isotest). Een gewone musl static-PIE binary. */
#include <unistd.h>
#include <stdlib.h>

static int slen(const char *s) {
    int n = 0;
    while (s[n]) n++;
    return n;
}
static void emit(const char *s) {
    write(1, s, slen(s));
}
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
    for (int i = 0; i < n; i++) b[i] = t[n - 1 - i];
    return n;
}

static int is_prime(int n) {
    if (n < 2) return 0;
    for (int i = 2; (long)i * i <= n; i++) {
        if (n % i == 0) return 0;
    }
    return 1;
}

int main(void) {
    long pid = getpid();
    char pb[24];
    int pn = utoa((unsigned long)pid, pb);

    char hdr[64];
    int o = 0;
    const char *h = "worker pid ";
    while (*h) hdr[o++] = *h++;
    for (int k = 0; k < pn; k++) hdr[o++] = pb[k];
    const char *h2 = ": telt priemgetallen tot 30000...\n";
    while (*h2) hdr[o++] = *h2++;
    write(1, hdr, o);

    int count = 0;
    for (int i = 2; i < 30000; i++) {
        if (is_prime(i)) count++;
    }

    char msg[80];
    o = 0;
    const char *m = "worker pid ";
    while (*m) msg[o++] = *m++;
    for (int k = 0; k < pn; k++) msg[o++] = pb[k];
    const char *m2 = ": resultaat=";
    while (*m2) msg[o++] = *m2++;
    o += utoa((unsigned long)count, msg + o);
    const char *m3 = " priemgetallen, exit(0)\n";
    while (*m3) msg[o++] = *m3++;
    write(1, msg, o);

    exit(0); /* nette beëindiging -> de kernel ruimt dit proces op */
}

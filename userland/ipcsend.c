/* EuroOS — EuroIPC-zender. Wacht even (zodat de ontvanger poort 42 kan claimen)
 * en stuurt dan een bericht naar poort 42. Gebruikt de eigen EuroIPC-syscalls.
 * Een gewone musl-binary. */
#include <unistd.h>

static long ipc(long n, long a, long b, long c) {
    long r;
    asm volatile("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c) : "rcx", "r11", "memory");
    return r;
}

static int slen(const char *s) { int n = 0; while (s[n]) n++; return n; }

int main(void) {
    /* geef de ontvanger tijd om poort 42 te claimen */
    for (volatile long i = 0; i < 8000000; i++) {
    }
    const char *msg = "Hallo van proces A via EuroIPC!";
    long r = ipc(501, 42, (long)msg, slen(msg)); /* send naar port 42 */

    char out[80];
    int o = 0;
    const char *s = "ipcsend: verzonden naar poort 42 (kernel: ";
    while (*s) out[o++] = *s++;
    long v = r;
    if (v < 0) { out[o++] = '-'; v = -v; }
    char t[20];
    int tn = 0;
    if (!v) t[tn++] = '0';
    else while (v) { t[tn++] = '0' + (v % 10); v /= 10; }
    for (int i = 0; i < tn; i++) out[o++] = t[tn - 1 - i];
    const char *e = " bytes)\n";
    while (*e) out[o++] = *e++;
    write(1, out, o);
    return 0;
}

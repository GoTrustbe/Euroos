/* EuroOS — EuroIPC sender. Waits a moment (so the receiver can claim port 42)
 * and then sends a message to port 42. Uses the native EuroIPC syscalls.
 * A plain musl binary. */
#include <unistd.h>

static long ipc(long n, long a, long b, long c) {
    long r;
    asm volatile("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c) : "rcx", "r11", "memory");
    return r;
}

static int slen(const char *s) { int n = 0; while (s[n]) n++; return n; }

int main(void) {
    /* give the receiver time to claim port 42 */
    for (volatile long i = 0; i < 8000000; i++) {
    }
    const char *msg = "Hello from process A via EuroIPC!";
    long r = ipc(501, 42, (long)msg, slen(msg)); /* send to port 42 */

    char out[80];
    int o = 0;
    const char *s = "ipcsend: sent to port 42 (kernel: ";
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

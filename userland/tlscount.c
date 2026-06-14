/* EuroOS — proof of the PREEMPTIVE per-process model. This is an ordinary musl
 * static-PIE binary with a THREAD-LOCAL counter (`__thread`). At startup musl
 * sets up its own TLS block and loads FS_BASE; the counter is therefore accessed
 * FS-relative. If two instances run at the same time and the scheduler swaps
 * them preemptively, their counters stay independent ONLY if the kernel
 * saves/restores FS_BASE per process on every context switch.
 *
 * No printf/malloc — only getpid() + write() — to keep the syscall set small.
 * The loop never ends (a scheduled background task). */
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
        counter++; /* THREAD-LOCAL (fs-relative) */
        i++;
        if ((i & 0x1FFFF) == 0) {
            char msg[64];
            int o = 0;
            const char *p = "tls-proc ";
            while (*p) msg[o++] = *p++;
            for (int k = 0; k < pn; k++) msg[o++] = pbuf[k];
            const char *q = ": counter=";
            while (*q) msg[o++] = *q++;
            o += utoa(counter, msg + o);
            msg[o++] = '\n';
            write(1, msg, o);
        }
    }
}

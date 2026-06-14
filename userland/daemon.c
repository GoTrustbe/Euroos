/* EuroOS — a BACKGROUND DAEMON: a ring-3 program that runs PREEMPTIVELY
 * scheduled (not synchronously) and periodically writes a heartbeat via a
 * syscall. Proves that a loaded program runs as a real, interruptible task
 * alongside the desktop and other tasks. Never terminates. */

static long sys(long n, long a1, long a2, long a3) {
    long r;
    __asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a1), "S"(a2), "d"(a3)
                     : "rcx", "r11", "memory");
    return r;
}
#define SYS_WRITE 1
static void put(const char *s) { sys(SYS_WRITE, (long)s, 0, 0); }

static char *utoa(unsigned long v, char *end) {
    *end = 0;
    char *p = end;
    do {
        *--p = (char)('0' + v % 10);
        v /= 10;
    } while (v);
    return p;
}

__attribute__((section(".text.start"))) void _start(void) {
    unsigned long n = 0;
    char b[24];
    for (;;) {
        put("EuroMonitor heartbeat #");
        put(utoa(n, b + 23));
        put(": kernel + scheduler running\n");
        n++;
        /* Delay so the heartbeat appears ~1x per quantum (does not flood). */
        for (volatile long i = 0; i < 4000000; i++) {
        }
    }
}

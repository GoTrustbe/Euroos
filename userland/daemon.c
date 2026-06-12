/* EuroOS — een ACHTERGROND-DAEMON: een ring-3 programma dat PREEMPTIEF
 * gescheduled draait (niet synchroon) en periodiek een hartslag schrijft via een
 * syscall. Bewijst dat een geladen programma als echte, onderbreekbare taak
 * naast de desktop en andere taken draait. Eindigt nooit. */

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
        put("EuroMonitor hartslag #");
        put(utoa(n, b + 23));
        put(": kernel + scheduler draaien\n");
        n++;
        /* Vertraging zodat de hartslag ~1x per kwantum verschijnt (niet floodt). */
        for (volatile long i = 0; i < 4000000; i++) {
        }
    }
}

/* EuroOS — S4 demo-service: schrijft een hartslag en sluit af. EuroInit (de
 * supervisor) herstart het volgens het 'always'-beleid tot het plafond — wat de
 * service-supervisie zichtbaar maakt in de kernel-log. */

static long sys(long n, long a1, long a2, long a3) {
    long ret;
    __asm__ volatile("syscall" : "=a"(ret) : "a"(n), "D"(a1), "S"(a2), "d"(a3) : "rcx", "r11", "memory");
    return ret;
}
#define L_WRITE 1
#define L_EXIT 60

static long slen(const char *s) {
    long n = 0;
    while (s[n]) n++;
    return n;
}

__attribute__((section(".text.start"))) void _start(void) {
    const char *m = "ticker: hartslag -> exit(0)\n";
    sys(L_WRITE, 1, (long)m, slen(m));
    sys(L_EXIT, 0, 0, 0);
    for (;;) {
    }
}

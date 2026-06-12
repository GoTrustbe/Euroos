/* EuroOS — S3-test: ECHTE fork() + waitpid() via de Linux-syscall-ABI.
 * De ouder forkt een kind; het kind print z'n pid en exit(7); de ouder wacht met
 * waitpid tot het kind klaar is en leest de exitstatus. Bewijst proces-creatie met
 * gekopieerde adresruimte + zombie-reaping op EuroKernel. */

static long sys(long n, long a1, long a2, long a3) {
    long ret;
    __asm__ volatile("syscall" : "=a"(ret) : "a"(n), "D"(a1), "S"(a2), "d"(a3) : "rcx", "r11", "memory");
    return ret;
}

#define L_WRITE 1
#define L_FORK 57
#define L_GETPID 39
#define L_EXECVE 59
#define L_EXIT 60
#define L_WAIT4 61

static long slen(const char *s) {
    long n = 0;
    while (s[n]) n++;
    return n;
}
static void put(const char *s) { sys(L_WRITE, 1, (long)s, slen(s)); }
static const char *utoa(long v, char *end) {
    *end = 0;
    char *p = end;
    unsigned long u = (v < 0) ? (unsigned long)(-v) : (unsigned long)v;
    do {
        *--p = (char)('0' + (u % 10));
        u /= 10;
    } while (u);
    return p;
}

__attribute__((section(".text.start"))) void _start(void) {
    char num[24];
    put("forktest: fork() + waitpid() op EuroKernel\n");
    long pid = sys(L_FORK, 0, 0, 0);
    if (pid == 0) {
        /* kind: eigen adresruimte (kopie), eigen pid. execve() vervangt nu het
         * image door /bin/execee (dat exit(9) doet). Op succes keert execve nooit
         * terug; komen we hierna, dan faalde het. */
        long cp = sys(L_GETPID, 0, 0, 0);
        put("  [child]  getpid=");
        put(utoa(cp, num + 23));
        put(" -> execve(/bin/execee)\n");
        char *av[2];
        av[0] = "/bin/execee";
        av[1] = 0;
        sys(L_EXECVE, (long)"/bin/execee", (long)av, 0);
        put("  [child]  execve faalde -> exit(1)\n");
        sys(L_EXIT, 1, 0, 0);
        for (;;) {
        }
    } else {
        /* ouder: krijgt de kind-pid, wacht (pollend) tot het kind een zombie is. */
        put("  [parent] fork gaf kind-pid ");
        put(utoa(pid, num + 23));
        put("\n");
        int status = 0;
        long w;
        do {
            w = sys(L_WAIT4, -1, (long)&status, 0);
        } while (w == 0);
        put("  [parent] waitpid reapte ");
        put(utoa(w, num + 23));
        put(", WEXITSTATUS=");
        put(utoa((status >> 8) & 0xff, num + 23));
        put("\n");
        sys(L_EXIT, 0, 0, 0);
        for (;;) {
        }
    }
}

/* EuroOS — fd-space test: a background process that keeps a pipe and a file
 * open while another program is launched in the foreground.
 *
 * Phase 1: pipe2(), open the control file, one round trip through the pipe.
 * Then it polls the control file by path until the kernel flips its first
 * byte to '1' (the kernel does that after a foreground launch).
 * Phase 2: the same pipe and the same file descriptor must still work.
 * Used by the [fdspace] boot self-test (kernel/src/ring3.rs). Linux ABI. */

static long sys(long n, long a1, long a2, long a3) {
    long ret;
    __asm__ volatile("syscall" : "=a"(ret) : "a"(n), "D"(a1), "S"(a2), "d"(a3) : "rcx", "r11", "memory");
    return ret;
}
#define L_READ 0
#define L_WRITE 1
#define L_OPEN 2
#define L_CLOSE 3
#define L_LSEEK 8
#define L_GETPID 39
#define L_EXIT 60
#define L_PIPE2 293

#define CTL "/tmp/kdebug-pipehold"

static char line[160];
static int pos;
static void s(const char *t) {
    while (*t && pos < 150) line[pos++] = *t++;
}
static void num(long v) {
    char b[24];
    int i = 0;
    if (v < 0) {
        if (pos < 150) line[pos++] = '-';
        v = -v;
    }
    do {
        b[i++] = (char)('0' + v % 10);
        v /= 10;
    } while (v && i < 23);
    while (i && pos < 150) line[pos++] = b[--i];
}
static void flush(void) {
    line[pos++] = '\n';
    sys(L_WRITE, 1, (long)line, pos);
    pos = 0;
}

/* One token through the pipe and back. 1 = it came back intact. */
static int round_trip(int *fds, const char *tok, long *wr, long *rd) {
    char buf[8];
    buf[0] = buf[1] = 0;
    *wr = sys(L_WRITE, fds[1], (long)tok, 2);
    *rd = sys(L_READ, fds[0], (long)buf, 8);
    return *wr == 2 && *rd == 2 && buf[0] == tok[0] && buf[1] == tok[1];
}

__attribute__((section(".text.start"))) void _start(void) {
    int fds[2];
    long wr, rd;
    char c = 0;
    fds[0] = fds[1] = -1;
    long pr = sys(L_PIPE2, (long)fds, 0, 0);
    long hfd = sys(L_OPEN, (long)CTL, 0, 0);
    int p1 = pr == 0 && round_trip(fds, "A1", &wr, &rd);
    long fr = sys(L_READ, hfd, (long)&c, 1);
    int f1 = hfd >= 3 && fr == 1;
    s("pipehold: phase1 pipe=");
    s(p1 ? "ok" : "BAD");
    s(" file=");
    s(f1 ? "ok" : "BAD");
    s(" fds r=");
    num(fds[0]);
    s(" w=");
    num(fds[1]);
    s(" f=");
    num(hfd);
    flush();

    /* Wait for the kernel: the control file's first byte becomes '1'. */
    for (;;) {
        long fd = sys(L_OPEN, (long)CTL, 0, 0);
        if (fd >= 0) {
            char g = 0;
            sys(L_READ, fd, (long)&g, 1);
            sys(L_CLOSE, fd, 0, 0);
            if (g == '1') break;
        }
        for (int i = 0; i < 200; i++) sys(L_GETPID, 0, 0, 0);
    }

    int p2 = round_trip(fds, "B2", &wr, &rd);
    c = 0;
    long sk = sys(L_LSEEK, hfd, 0, 0);
    fr = sys(L_READ, hfd, (long)&c, 1);
    int f2 = sk == 0 && fr == 1 && c == '1';
    s("pipehold: phase2 pipe=");
    s(p2 ? "ok" : "BAD");
    s(" (write=");
    num(wr);
    s(" read=");
    num(rd);
    s(") file=");
    s(f2 ? "ok" : "BAD");
    s(" (lseek=");
    num(sk);
    s(" read=");
    num(fr);
    s(")");
    flush();
    s(p1 && f1 && p2 && f2 ? "pipehold: PASS" : "pipehold: FAIL");
    flush();
    sys(L_EXIT, p1 && f1 && p2 && f2 ? 0 : 1, 0, 0);
    for (;;) {
    }
}

/* EuroOS - fd semantics on the background (musl) process path.
 *
 * The same rules as userland/glibc/gfdsem.c, for a process that is scheduled
 * as a background process and forks there: one descriptor table per process,
 * a fork copies it, both copies name the same open file descriptions; a pipe
 * ends when the LAST descriptor of its other end closes; close-on-exec.
 *
 * Freestanding (no libc), Linux ABI by number, like forkpipe.c and
 * pipehold.c. Used by the [fdsem] boot self-test (kernel/src/ring3.rs),
 * which puts "abcdefgh" in DATA before it starts this.
 *
 * Nothing here waits without a bound. */

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
#define L_DUP 32
#define L_DUP2 33
#define L_NANOSLEEP 35
#define L_FORK 57
#define L_EXECVE 59
#define L_EXIT 60
#define L_WAIT4 61
#define L_FCNTL 72
#define L_CLOCK_GETTIME 228
#define L_DUP3 292
#define L_PIPE2 293
#define L_CLOSE_RANGE 436

#define F_DUPFD 0
#define F_GETFD 1
#define F_SETFD 2
#define F_DUPFD_CLOEXEC 1030
#define O_CLOEXEC 02000000
#define EAGAIN 11
#define EBADF 9
#define EPIPE 32

#define DATA "/tmp/fdsem-data"

static char line[200];
static int pos;
static int total, fails;

static void s(const char *t) {
    while (*t && pos < 190) line[pos++] = *t++;
}
static void num(long v) {
    char b[24];
    int i = 0;
    if (v < 0) {
        if (pos < 190) line[pos++] = '-';
        v = -v;
    }
    do {
        b[i++] = (char)('0' + v % 10);
        v /= 10;
    } while (v && i < 23);
    while (i && pos < 190) line[pos++] = b[--i];
}
static void flush(void) {
    line[pos++] = '\n';
    sys(L_WRITE, 1, (long)line, pos);
    pos = 0;
}
/* One result line: "fdsem: ok   name" or "fdsem: FAIL name: a b c". */
static void check(const char *name, int ok, long a, long b, long c) {
    total++;
    if (!ok) fails++;
    s(ok ? "fdsem: ok   " : "fdsem: FAIL ");
    s(name);
    if (!ok) {
        s(": ");
        num(a); s(" "); num(b); s(" "); num(c);
    }
    flush();
}
static void nap(void) { sys(L_NANOSLEEP, 0, 0, 0); }
/* Milliseconds of the kernel's clock. Every wait below is bounded by TIME: a
 * nap gives the CPU away at the next tick, it does not last a known while. */
static long now_ms(void) {
    long ts[2] = { 0, 0 };
    sys(L_CLOCK_GETTIME, 1, (long)ts, 0);
    return ts[0] * 1000 + ts[1] / 1000000;
}

/* Read with a bound: -2 when nothing came within `ms` milliseconds. */
static long read_to(int fd, char *buf, long n, long ms) {
    long end = now_ms() + ms;
    for (;;) {
        long r = sys(L_READ, fd, (long)buf, n);
        if (r != -EAGAIN) return r;
        if (now_ms() >= end) return -2;
        nap();
    }
}
static long reap(void) {
    int st = -1;
    long end = now_ms() + 20000;
    while (now_ms() < end) {
        long w = sys(L_WAIT4, -1, (long)&st, 0);
        if (w > 0) return (st >> 8) & 0xff;
        nap();
    }
    return -1;
}
static void quit(long code) {
    sys(L_EXIT, code, 0, 0);
    for (;;) {
    }
}

static void one_process(void) {
    char b[8];
    for (int i = 0; i < 8; i++) b[i] = 0;
    int fd = (int)sys(L_OPEN, (long)DATA, 0, 0);
    int d = (int)sys(L_DUP, fd, 0, 0);
    long r1 = sys(L_READ, fd, (long)b, 2);
    long r2 = sys(L_READ, d, (long)(b + 2), 2);
    check("dup gives a second number for the same open file (shared offset)",
          fd >= 0 && d >= 0 && d != fd && r1 == 2 && r2 == 2 && b[0] == 'a' && b[2] == 'c' && b[3] == 'd', fd, d, b[2]);
    sys(L_CLOSE, fd, 0, 0);
    long r3 = sys(L_READ, d, (long)b, 2);
    check("the open file lives until its last descriptor closes", r3 == 2 && b[0] == 'e' && b[1] == 'f', r3, b[0], b[1]);
    int again = (int)sys(L_OPEN, (long)DATA, 0, 0);
    check("open returns the lowest free number", again == fd, fd, again, 0);
    sys(L_CLOSE, again, 0, 0);

    int e = (int)sys(L_FCNTL, d, F_DUPFD, 50);
    int c = (int)sys(L_FCNTL, d, F_DUPFD_CLOEXEC, 100);
    long ge = sys(L_FCNTL, e, F_GETFD, 0), gc = sys(L_FCNTL, c, F_GETFD, 0);
    check("F_DUPFD and F_DUPFD_CLOEXEC", e >= 50 && c >= 100 && ge == 0 && gc == 1, e, c, gc);
    long t = sys(L_DUP3, d, 200, O_CLOEXEC);
    long gt = sys(L_FCNTL, 200, F_GETFD, 0);
    sys(L_FCNTL, e, F_SETFD, 1);
    check("dup3 with O_CLOEXEC, F_SETFD", t == 200 && gt == 1 && sys(L_FCNTL, e, F_GETFD, 0) == 1 && sys(L_FCNTL, d, F_GETFD, 0) == 0, t, gt, 0);
    long cr = sys(L_CLOSE_RANGE, 50, 0xffffffffL, 0);
    check("close_range", cr == 0 && sys(L_FCNTL, e, F_GETFD, 0) == -EBADF && sys(L_FCNTL, 200, F_GETFD, 0) == -EBADF
          && sys(L_FCNTL, d, F_GETFD, 0) == 0, cr, sys(L_FCNTL, e, F_GETFD, 0), sys(L_FCNTL, d, F_GETFD, 0));
    sys(L_CLOSE, d, 0, 0);
    check("a closed number is EBADF", sys(L_FCNTL, d, F_GETFD, 0) == -EBADF && sys(L_DUP, 999, 0, 0) == -EBADF, sys(L_FCNTL, d, F_GETFD, 0), sys(L_DUP, 999, 0, 0), 0);

    int p[2] = { -1, -1 };
    sys(L_PIPE2, (long)p, 0, 0);
    int w2 = (int)sys(L_DUP, p[1], 0, 0);
    sys(L_CLOSE, p[1], 0, 0);
    long e1 = sys(L_READ, p[0], (long)b, 1);
    long wr = sys(L_WRITE, w2, (long)"k", 1);
    sys(L_CLOSE, w2, 0, 0);
    long g1 = sys(L_READ, p[0], (long)b, 1);
    long g2 = sys(L_READ, p[0], (long)b, 1);
    check("pipe: no end-of-file while a duplicate of the write end is open", e1 == -EAGAIN && w2 >= 0 && w2 != p[1], e1, w2, p[1]);
    check("pipe: data, then end-of-file when the last write end closes", wr == 1 && g1 == 1 && g2 == 0, wr, g1, g2);
    sys(L_CLOSE, p[0], 0, 0);
    int q[2] = { -1, -1 };
    sys(L_PIPE2, (long)q, 0, 0);
    sys(L_CLOSE, q[0], 0, 0);
    long ep = sys(L_WRITE, q[1], (long)"z", 1);
    check("pipe: EPIPE when the last read end closes", ep == -EPIPE, ep, 0, 0);
    sys(L_CLOSE, q[1], 0, 0);
}

static void across_fork(void) {
    char b[8];
    int fd = (int)sys(L_OPEN, (long)DATA, 0, 0);
    int p[2], back[2], hold[2];
    sys(L_PIPE2, (long)p, 0, 0);
    sys(L_PIPE2, (long)back, 0, 0);
    sys(L_PIPE2, (long)hold, 0, 0);
    long pid = sys(L_FORK, 0, 0, 0);
    if (pid == 0) {
        char c[4];
        long r = sys(L_READ, fd, (long)c, 3); /* moves the parent's offset too */
        sys(L_CLOSE, fd, 0, 0);
        int n = (int)sys(L_OPEN, (long)DATA, 0, 0); /* takes the number just closed */
        sys(L_CLOSE, p[0], 0, 0);
        sys(L_CLOSE, back[0], 0, 0);
        sys(L_CLOSE, hold[1], 0, 0);
        char rep[2] = { (char)(r == 3 && c[0] == 'a' ? 'y' : 'n'), (char)(n == fd ? 'y' : 'n') };
        sys(L_WRITE, back[1], (long)rep, 2);
        sys(L_WRITE, p[1], (long)"xy", 2);
        sys(L_CLOSE, p[1], 0, 0);
        read_to(hold[0], c, 1, 30000); /* alive, with the write end closed */
        quit(0);
    }
    sys(L_CLOSE, p[1], 0, 0);
    sys(L_CLOSE, back[1], 0, 0);
    sys(L_CLOSE, hold[0], 0, 0);
    char rep[2] = { 0, 0 };
    long got = read_to(back[0], rep, 2, 15000);
    for (int i = 0; i < 8; i++) b[i] = 0;
    long r = sys(L_READ, fd, (long)b, 3);
    check("fork shares the offset", got == 2 && rep[0] == 'y' && r == 3 && b[0] == 'd' && b[2] == 'f', got, r, b[0]);
    check("a close in the child closes nothing in the parent; the child reuses the number", rep[1] == 'y' && r == 3, rep[1], r, 0);
    long d1 = read_to(p[0], b, 2, 15000);
    long d2 = read_to(p[0], b, 2, 5000);
    check("pipe across fork: data, then end-of-file when the child closed the last write end (child alive)",
          d1 == 2 && d2 == 0, d1, d2, 0);
    sys(L_WRITE, hold[1], (long)"h", 1);
    long st = reap();
    check("child of the fork test", st == 0, st, 0, 0);
    sys(L_CLOSE, p[0], 0, 0);
    sys(L_CLOSE, back[0], 0, 0);
    sys(L_CLOSE, hold[1], 0, 0);

    int q[2];
    sys(L_PIPE2, (long)q, 0, 0);
    pid = sys(L_FORK, 0, 0, 0);
    if (pid == 0) {
        sys(L_WRITE, q[1], (long)"q", 1);
        quit(0); /* exits with both ends open */
    }
    sys(L_CLOSE, q[1], 0, 0);
    st = reap();
    long e1 = read_to(q[0], b, 1, 5000);
    long e2 = read_to(q[0], b, 1, 5000);
    check("exit closes the descriptors of the process", st == 0 && e1 == 1 && e2 == 0, st, e1, e2);
    sys(L_CLOSE, q[0], 0, 0);

    /* exec: fd 40 stays (with its offset), fd 41 is close-on-exec. The image
     * that finds fd 40 open at its start is the one after the exec. (Without
     * descriptor flags the image after the exec could not tell: not tried.) */
    if (sys(L_FCNTL, 0, F_GETFD, 0) != 0) {
        check("exec closes close-on-exec descriptors and keeps the others (with their offset)", 0, sys(L_FCNTL, 0, F_GETFD, 0), 0, 0);
        sys(L_CLOSE, fd, 0, 0);
        return;
    }
    sys(L_LSEEK, fd, 2, 0);
    sys(L_DUP2, fd, 40, 0);
    sys(L_FCNTL, fd, F_DUPFD_CLOEXEC, 41);
    pid = sys(L_FORK, 0, 0, 0);
    if (pid == 0) {
        sys(L_EXECVE, (long)"/bin/fdsem", 0, 0);
        quit(90);
    }
    st = reap();
    check("exec closes close-on-exec descriptors and keeps the others (with their offset)", st == 0, st, 0, 0);
    sys(L_CLOSE, 40, 0, 0);
    sys(L_CLOSE, 41, 0, 0);
    sys(L_CLOSE, fd, 0, 0);
}

__attribute__((section(".text.start"))) void _start(void) {
    if (sys(L_FCNTL, 40, F_GETFD, 0) == 0) {
        /* After the exec. */
        char b[2] = { 0, 0 };
        long gone = sys(L_FCNTL, 41, F_GETFD, 0);
        long r = sys(L_READ, 40, (long)b, 2);
        quit(gone != -EBADF ? 32 : (r == 2 && b[0] == 'c' && b[1] == 'd') ? 0 : 33);
    }
    one_process();
    across_fork();
    s(fails ? "fdsem: FAILED " : "fdsem: PASS ");
    num(total - fails); s("/"); num(total);
    flush();
    quit(fails ? 1 : 0);
}

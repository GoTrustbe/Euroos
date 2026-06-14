/* EuroOS — S3 test: pipe() + fork() IPC. The parent makes a pipe, forks, and the
 * child writes a message into the pipe that the parent reads back. Proves inter-process
 * communication between two REAL processes via an in-kernel FIFO. */

static long sys(long n, long a1, long a2, long a3) {
    long ret;
    __asm__ volatile("syscall" : "=a"(ret) : "a"(n), "D"(a1), "S"(a2), "d"(a3) : "rcx", "r11", "memory");
    return ret;
}
#define L_READ 0
#define L_WRITE 1
#define L_FORK 57
#define L_EXIT 60
#define L_WAIT4 61
#define L_PIPE2 293

static long slen(const char *s) {
    long n = 0;
    while (s[n]) n++;
    return n;
}
static void put(const char *s) { sys(L_WRITE, 1, (long)s, slen(s)); }

__attribute__((section(".text.start"))) void _start(void) {
    put("forkpipe: pipe() + fork() IPC\n");
    int fds[2];
    fds[0] = fds[1] = -1;
    if (sys(L_PIPE2, (long)fds, 0, 0) != 0) {
        put("  pipe2 failed\n");
        sys(L_EXIT, 1, 0, 0);
        for (;;) {
        }
    }
    long pid = sys(L_FORK, 0, 0, 0);
    if (pid == 0) {
        /* child: write a message into the write end and exit. */
        const char *m = "hello-from-child-via-pipe";
        sys(L_WRITE, fds[1], (long)m, slen(m));
        sys(L_EXIT, 0, 0, 0);
        for (;;) {
        }
    } else {
        /* parent: read (polling) from the read end, then reap the child. */
        char buf[64];
        long n;
        do {
            n = sys(L_READ, fds[0], (long)buf, 64);
        } while (n <= 0); /* -EAGAIN (empty) or 0 -> try again */
        put("  [parent] from pipe: ");
        sys(L_WRITE, 1, (long)buf, n);
        put("\n");
        int st;
        long w;
        do {
            w = sys(L_WAIT4, -1, (long)&st, 0);
        } while (w == 0);
        put("  [parent] child reaped\n");
        sys(L_EXIT, 0, 0, 0);
        for (;;) {
        }
    }
}

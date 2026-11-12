/* gfdsem: descriptor semantics per process, as Linux has them.
 *
 * One process has one descriptor table. fork copies the table and the two
 * copies name the same open file descriptions (offset and status flags are
 * shared, a close in one process closes nothing for the other). A pipe or a
 * socket reports end-of-file, EPIPE and POLLHUP when the LAST descriptor of
 * the other end is closed, in whatever process that is. close-on-exec is a
 * property of the descriptor (not of the description), SCM_RIGHTS installs
 * the sender's description under a new number in the receiver, and numbers
 * are handed out lowest-first for every kind of object.
 *
 * Built twice: against glibc (the whole file) and, with -DNOFORK, against
 * musl as a static PIE for the single-process half.
 *
 * Every check prints one line; the last line is the count. Nothing in here
 * waits without a bound: on a kernel that lacks one of these semantics the
 * check fails, it does not hang.
 */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdarg.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <dirent.h>
#include <stdint.h>
#include <sys/types.h>
#include <sys/stat.h>
#include <sys/socket.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#ifndef NOFORK
#include <sys/eventfd.h>
#include <sys/epoll.h>
#endif

#ifndef TAG
#define TAG "GFDSEM"
#endif
#ifndef SYS_close_range
#define SYS_close_range 436
#endif

static int total, fails;

static void check(const char *name, int ok, const char *fmt, ...) __attribute__((format(printf, 3, 4)));
static void check(const char *name, int ok, const char *fmt, ...) {
    char detail[200];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(detail, sizeof detail, fmt, ap);
    va_end(ap);
    total++;
    if (!ok) fails++;
    printf(TAG ": %s %s%s%s\n", ok ? "ok  " : "FAIL", name, detail[0] ? ": " : "", detail);
    fflush(stdout);
}

static int mkfile(const char *path, const char *content) {
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0600);
    if (fd >= 0 && content) {
        if (write(fd, content, strlen(content)) < 0) { close(fd); return -1; }
        lseek(fd, 0, SEEK_SET);
    }
    return fd;
}

/* Wait until `fd` reports something; the revents (0 = nothing within `ms`). */
static int wait_ev(int fd, short ev, int ms) {
    struct pollfd p = { .fd = fd, .events = ev, .revents = 0 };
    for (int waited = 0; waited <= ms; waited += 50) {
        p.revents = 0;
        int r = poll(&p, 1, 50);
        if (r > 0 && p.revents) return p.revents;
    }
    return 0;
}

static void set_nb(int fd) { fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK); }

/* Read with a bound: -2 when nothing came within `ms`. */
static long read_to(int fd, void *buf, size_t n, int ms) {
    set_nb(fd);
    for (int waited = 0; ; waited += 20) {
        long r = read(fd, buf, n);
        if (r >= 0) return r;
        if (errno != EAGAIN && errno != EINTR) return -1;
        if (waited >= ms) return -2;
        usleep(20000);
    }
}

/* -- one process ---------------------------------------------------------- */

static void t_dup_offset(void) {
    char b[8] = {0};
    int fd = mkfile("/tmp/gfdsem-a", "abcdefgh");
    int d = dup(fd);
    long r1 = read(fd, b, 2);
    long r2 = read(d, b + 2, 2);
    long o1 = lseek(fd, 0, SEEK_CUR), o2 = lseek(d, 0, SEEK_CUR);
    check("dup shares the offset", fd >= 0 && d >= 0 && r1 == 2 && r2 == 2 && !memcmp(b, "abcd", 4) && o1 == 4 && o2 == 4,
          "read via the dup gave \"%c%c\" (want \"cd\"), offsets %ld/%ld", b[2] ? b[2] : '?', b[3] ? b[3] : '?', o1, o2);
    int fl = fcntl(fd, F_GETFL);
    fcntl(fd, F_SETFL, fl | O_NONBLOCK);
    int dfl = fcntl(d, F_GETFL);
    check("dup shares the status flags", dfl >= 0 && (dfl & O_NONBLOCK) != 0,
          "F_GETFL of the dup = %#x after O_NONBLOCK on the original", dfl);
    fcntl(fd, F_SETFL, fl);
    close(fd);
    memset(b, 0, sizeof b);
    long r3 = read(d, b, 2);
    check("the description lives until its last descriptor closes", r3 == 2 && !memcmp(b, "ef", 2),
          "read after closing the original = %ld", r3);
    close(d);
}

static void t_lowest(void) {
    int a = mkfile("/tmp/gfdsem-l1", "1");
    int b = mkfile("/tmp/gfdsem-l2", "2");
    close(a);
    int c = mkfile("/tmp/gfdsem-l3", "3");
    check("open returns the lowest free number", a >= 0 && c == a, "closed %d, next open gave %d", a, c);
    close(b); close(c);
}

static void t_pipe_one_process(void) {
    int p[2];
    char c = 0;
    if (pipe(p)) { check("pipe", 0, "errno %d", errno); return; }
    int w2 = dup(p[1]);
    close(p[1]);
    set_nb(p[0]);
    long r = read(p[0], &c, 1);
    int e = errno;
    check("no end-of-file while a duplicate of the write end is open", r == -1 && e == EAGAIN,
          "read = %ld errno %d (want -1 EAGAIN)", r, r < 0 ? e : 0);
    long w = write(w2, "k", 1);
    close(w2);
    r = read(p[0], &c, 1);
    check("data written before the close is still read", w == 1 && r == 1 && c == 'k', "write %ld read %ld", w, r);
    int re = wait_ev(p[0], POLLIN, 500);
    check("POLLHUP on the read end when the last write end closes", (re & POLLHUP) != 0, "revents %#x", re);
    r = read(p[0], &c, 1);
    e = errno;
    check("end-of-file when the last write end closes", r == 0, "read = %ld errno %d (want 0)", r, r < 0 ? e : 0);
    close(p[0]);

    int q[2];
    if (pipe(q)) { check("pipe", 0, "errno %d", errno); return; }
    close(q[0]);
    re = wait_ev(q[1], POLLOUT, 200);
    check("POLLERR on the write end when the last read end closes", (re & POLLERR) != 0, "revents %#x", re);
    r = write(q[1], "z", 1);
    e = errno;
    check("EPIPE when the last read end closes", r == -1 && e == EPIPE, "write = %ld errno %d (want -1, EPIPE = %d)", r, r < 0 ? e : 0, EPIPE);
    close(q[1]);
}

static void t_cloexec_flags(void) {
    int fd = open("/tmp/gfdsem-a", O_RDONLY | O_CLOEXEC);
    int d = dup(fd);
    int g1 = fcntl(fd, F_GETFD), g2 = fcntl(d, F_GETFD);
    check("O_CLOEXEC sets FD_CLOEXEC, dup clears it", fd >= 0 && d >= 0 && g1 == FD_CLOEXEC && g2 == 0, "F_GETFD original %d, dup %d", g1, g2);
    int e = fcntl(fd, F_DUPFD, 50);
    check("F_DUPFD returns a number at or above its argument", e >= 50 && fcntl(e, F_GETFD) == 0, "got %d (want >= 50)", e);
    int c = fcntl(fd, F_DUPFD_CLOEXEC, 100);
    int gc = fcntl(c, F_GETFD);
    check("F_DUPFD_CLOEXEC", c >= 100 && gc == FD_CLOEXEC, "got %d, F_GETFD %d", c, gc);
    int t = dup3(fd, 200, O_CLOEXEC);
    int gt = fcntl(200, F_GETFD);
    check("dup3 with O_CLOEXEC", t == 200 && gt == FD_CLOEXEC, "got %d, F_GETFD %d", t, gt);
    fcntl(d, F_SETFD, FD_CLOEXEC);
    int gd = fcntl(d, F_GETFD), ge = fcntl(e, F_GETFD);
    check("FD_CLOEXEC belongs to the descriptor, not to the file", gd == FD_CLOEXEC && ge == 0, "after F_SETFD on one dup: %d, on another: %d", gd, ge);
    int r1 = dup3(fd, fd, 0), e1 = errno;
    int r2 = dup2(fd, fd);
    int r3 = dup(9999), e3 = errno;
    int r4 = dup2(9999, 60), e4 = errno;
    check("dup errors", r1 == -1 && e1 == EINVAL && r2 == fd && r3 == -1 && e3 == EBADF && r4 == -1 && e4 == EBADF,
          "dup3(fd,fd)=%d/%d dup2(fd,fd)=%d dup(9999)=%d/%d dup2(9999,60)=%d/%d", r1, e1, r2, r3, e3, r4, e4);
    /* dup2 onto an open number closes what was there. */
    int p[2];
    if (pipe(p) == 0) {
        int r = dup2(fd, p[1]);
        char ch;
        long n = read_to(p[0], &ch, 1, 300);
        check("dup2 onto an open descriptor closes it", r == p[1] && n == 0, "dup2 = %d, read on the pipe = %ld (want 0)", r, n);
        close(p[0]); close(p[1]);
    }
    close(fd); close(d); close(e); close(c); close(200);
    int after = fcntl(200, F_GETFD), ea = errno;
    check("a closed number is EBADF", after == -1 && ea == EBADF, "F_GETFD = %d errno %d", after, after < 0 ? ea : 0);
}

static void t_limit(void) {
    struct rlimit rl = {0, 0};
    getrlimit(RLIMIT_NOFILE, &rl);
    check("RLIMIT_NOFILE is at least 4096", rl.rlim_cur >= 4096 && rl.rlim_max >= rl.rlim_cur,
          "soft %lu hard %lu", (unsigned long)rl.rlim_cur, (unsigned long)rl.rlim_max);
    static int fds[1300];
    int base = open("/tmp/gfdsem-a", O_RDONLY);
    int n = 0, top = -1;
    for (; n < 1200; n++) {
        fds[n] = dup(base);
        if (fds[n] < 0) break;
        if (fds[n] > top) top = fds[n];
    }
    int en = errno;
    check("1200 duplicates of one descriptor", n == 1200 && top >= 1200, "%d made, highest number %d, errno %d", n, top, n < 1200 ? en : 0);
    for (int i = 0; i < n; i++) close(fds[i]);
    close(base);
    int m = 0;
    char path[48];
    for (; m < 700; m++) {
        snprintf(path, sizeof path, "/tmp/gfdsem-m%d", m);
        fds[m] = open(path, O_CREAT | O_RDWR, 0600);
        if (fds[m] < 0) break;
    }
    en = errno;
    /* One of them is really its own file. */
    int own = 0;
    if (m > 650) {
        char b = 0;
        if (write(fds[650], "Q", 1) == 1 && pread(fds[650], &b, 1, 0) == 1 && b == 'Q' && pread(fds[649], &b, 1, 0) == 0) own = 1;
    }
    check("700 files open at once in one process", m == 700 && own, "%d opened, errno %d", m, m < 700 ? en : 0);
    for (int i = 0; i < m; i++) {
        close(fds[i]);
        snprintf(path, sizeof path, "/tmp/gfdsem-m%d", i);
        unlink(path);
    }
}

static void t_proc_self_fd(void) {
    int x = open("/tmp/gfdsem-a", O_RDONLY);
    int y = fcntl(x, F_DUPFD, 90); /* not the number opendir is about to get */
    close(y);
    DIR *d = opendir("/proc/self/fd");
    int saw_x = 0, saw_y = 0, saw_std = 0, n = 0;
    if (d) {
        struct dirent *de;
        while ((de = readdir(d)) != NULL) {
            if (de->d_name[0] == '.') continue;
            int v = atoi(de->d_name);
            n++;
            if (v == x) saw_x = 1;
            if (v == y) saw_y = 1;
            if (v == 0 || v == 1 || v == 2) saw_std++;
        }
        closedir(d);
    }
    check("/proc/self/fd lists this process's descriptors", d && saw_x && !saw_y && saw_std == 3,
          "opendir %s, %d entries, open fd %d listed=%d, closed fd %d listed=%d, std=%d", d ? "ok" : "failed", n, x, saw_x, y, saw_y, saw_std);
    close(x);
}

static void t_close_range(void) {
    int base = open("/tmp/gfdsem-a", O_RDONLY);
    int a = fcntl(base, F_DUPFD, 300), b = fcntl(base, F_DUPFD, 300), c = fcntl(base, F_DUPFD, 400);
    long r = syscall(SYS_close_range, 300U, 350U, 4U /* CLOSE_RANGE_CLOEXEC */);
    int ce = fcntl(a, F_GETFD), cc = fcntl(c, F_GETFD);
    long r2 = syscall(SYS_close_range, 300U, ~0U, 0U);
    int ga = fcntl(a, F_GETFD), gb = fcntl(b, F_GETFD), gc = fcntl(c, F_GETFD), gbase = fcntl(base, F_GETFD);
    check("close_range", a >= 300 && b > a && c >= 400 && r == 0 && ce == FD_CLOEXEC && cc == 0 && r2 == 0
          && ga == -1 && gb == -1 && gc == -1 && gbase == 0,
          "fds %d %d %d, CLOEXEC call %ld (flags %d/%d), close call %ld, after: %d %d %d, below the range: %d",
          a, b, c, r, ce, cc, r2, ga, gb, gc, gbase);
    close(base);
}

static void t_numbers(void) {
    int probe = dup(0);
    close(probe);
    int s = socket(AF_UNIX, SOCK_STREAM, 0);
    int sv[2] = {-1, -1};
    int sp = socketpair(AF_UNIX, SOCK_STREAM, 0, sv);
    check("sockets are numbered like every other descriptor", s == probe && sp == 0 && sv[0] == probe + 1 && sv[1] == probe + 2,
          "lowest free %d: socket %d, socketpair %d,%d", probe, s, sv[0], sv[1]);
#ifndef NOFORK
    int ev = eventfd(0, 0);
    int ep = epoll_create1(0);
    check("eventfd and epoll are numbered like every other descriptor", ev == probe + 3 && ep == probe + 4,
          "eventfd %d (want %d), epoll %d (want %d)", ev, probe + 3, ep, probe + 4);
    close(ev); close(ep);
#endif
    close(s); close(sv[0]); close(sv[1]);
}

#ifndef NOFORK
/* -- across fork ---------------------------------------------------------- */

static int reap(pid_t pid) {
    int st = 0;
    for (int i = 0; i < 400; i++) {
        pid_t r = waitpid(pid, &st, WNOHANG);
        if (r == pid) return WIFEXITED(st) ? WEXITSTATUS(st) : 200 + WTERMSIG(st);
        usleep(20000);
    }
    kill(pid, SIGKILL);
    return -1;
}

static void t_fork_offset(void) {
    int fd = mkfile("/tmp/gfdsem-a", "abcdefgh");
    pid_t c = fork();
    if (c == 0) {
        char b[3];
        _exit(read(fd, b, 3) == 3 && !memcmp(b, "abc", 3) ? 0 : 9);
    }
    int st = reap(c);
    char b[4] = {0};
    long r = read(fd, b, 3);
    check("fork shares the offset", st == 0 && r == 3 && !memcmp(b, "def", 3), "child %d, parent then read \"%s\" (want \"def\")", st, b);
    close(fd);
}

static void t_fork_close(void) {
    int fd = mkfile("/tmp/gfdsem-a", "abcdefgh");
    int p[2], back[2];
    if (pipe(p) || pipe(back)) { check("pipe", 0, "errno %d", errno); return; }
    pid_t c = fork();
    if (c == 0) {
        close(fd);
        int n = open("/tmp/gfdsem-b", O_CREAT | O_RDWR | O_TRUNC, 0600);
        int w = (int)write(n, "CHILD", 5);
        close(p[0]); close(p[1]);
        /* Enough new files to get numbers the parent does not use itself. */
        int n2 = -1, n3 = -1;
        for (int i = 0; i < 6; i++) { n2 = n3; n3 = mkfile(i & 1 ? "/tmp/gfdsem-c" : "/tmp/gfdsem-d", "x"); }
        int v[4] = { n, n2, n3, w };
        if (write(back[1], v, sizeof v) != sizeof v) _exit(8);
        usleep(600000); /* alive while the parent looks */
        _exit(n == fd ? 0 : 7);
    }
    int v[4] = { -1, -1, -1, -1 };
    long got = read_to(back[0], v, sizeof v, 4000);
    char b[6] = {0};
    long r = read(fd, b, 5);
    check("a close in the child closes nothing in the parent", got == (long)sizeof v && r == 5 && !memcmp(b, "abcde", 5),
          "parent read %ld bytes \"%s\" from its descriptor (want \"abcde\")", r, b);
    check("the child reuses the number it closed", v[0] == fd, "child closed %d and opened %d", fd, v[0]);
    /* The child is still alive and has p closed; the parent's pipe works. */
    char ch = 0;
    int pw = (int)write(p[1], "p", 1);
    long pr = read_to(p[0], &ch, 1, 500);
    check("the parent's pipe survives the child closing both ends", pw == 1 && pr == 1 && ch == 'p', "write %d read %ld", pw, pr);
    /* Descriptors the child opened are not descriptors of the parent. */
    int vis = 0;
    for (int i = 1; i < 3; i++) if (v[i] < 0 || fcntl(v[i], F_GETFD) != -1) vis++;
    check("descriptors opened by the child do not exist in the parent", got == (long)sizeof v && vis == 0,
          "child opened %d and %d; %d of them answer in the parent", v[1], v[2], vis);
    int st = reap(c);
    check("child of the close test", st == 0, "exit %d", st);
    close(fd); close(p[0]); close(p[1]); close(back[0]); close(back[1]);
}

static void t_fork_pipe_eof(void) {
    int p[2], go[2], hold[2];
    if (pipe(p) || pipe(go) || pipe(hold)) { check("pipe", 0, "errno %d", errno); return; }
    pid_t c = fork();
    if (c == 0) {
        char g;
        close(p[0]); close(go[1]); close(hold[1]);
        if (read_to(go[0], &g, 1, 5000) != 1) _exit(5);
        if (write(p[1], "xy", 2) != 2) _exit(6);
        close(p[1]);
        read_to(hold[0], &g, 1, 6000); /* stay alive with the write end closed */
        _exit(0);
    }
    close(p[1]); close(go[0]); close(hold[0]);
    char b[4] = {0};
    set_nb(p[0]);
    long r = read(p[0], b, 2);
    int e = errno;
    check("no end-of-file while the child holds the write end", r == -1 && e == EAGAIN, "read = %ld errno %d (want -1 EAGAIN)", r, r < 0 ? e : 0);
    long gw = write(go[1], "g", 1);
    r = read_to(p[0], b, 2, 3000);
    check("the parent reads what the child wrote", gw == 1 && r == 2 && !memcmp(b, "xy", 2), "read = %ld", r);
    int re = wait_ev(p[0], POLLIN | POLLHUP, 2000);
    check("POLLHUP when the child closed the last write end", (re & POLLHUP) != 0, "revents %#x", re);
    r = read_to(p[0], b, 2, 2000);
    check("end-of-file when the child closed the last write end (the child is alive)", r == 0, "read = %ld (want 0; -2 = nothing within 2 s)", r);
    gw = write(hold[1], "h", 1);
    int st = reap(c);
    check("child of the pipe test", st == 0, "exit %d", st);
    close(p[0]); close(go[1]); close(hold[1]);

    /* A process that exits closes its descriptors. */
    int q[2];
    if (pipe(q)) return;
    c = fork();
    if (c == 0) { if (write(q[1], "q", 1) != 1) _exit(4); _exit(0); }
    close(q[1]);
    st = reap(c);
    r = read_to(q[0], b, 1, 1000);
    long r2 = read_to(q[0], b, 1, 2000);
    check("exit closes the descriptors of the process", st == 0 && r == 1 && r2 == 0, "child %d, read %ld then %ld (want 1 then 0)", st, r, r2);
    close(q[0]);
}

static void t_fork_socket_eof(void) {
    int sv[2], go[2], hold[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) || pipe(go) || pipe(hold)) { check("socketpair", 0, "errno %d", errno); return; }
    pid_t c = fork();
    if (c == 0) {
        char g;
        close(sv[0]); close(go[1]); close(hold[1]);
        if (read_to(go[0], &g, 1, 5000) != 1) _exit(5);
        if (write(sv[1], "s", 1) != 1) _exit(6);
        close(sv[1]);
        read_to(hold[0], &g, 1, 6000);
        _exit(0);
    }
    close(sv[1]); close(go[0]); close(hold[0]);
    char b = 0;
    set_nb(sv[0]);
    long r = read(sv[0], &b, 1);
    int e = errno;
    check("no end-of-file on a socket while the child holds the other end", r == -1 && e == EAGAIN, "read = %ld errno %d", r, r < 0 ? e : 0);
    long gw = write(go[1], "g", 1);
    r = read_to(sv[0], &b, 1, 3000);
    long r2 = read_to(sv[0], &b, 1, 2000);
    check("end-of-file on a socket when the child closed the last other end (the child is alive)", gw == 1 && r == 1 && r2 == 0,
          "read %ld then %ld (want 1 then 0; -2 = nothing within 2 s)", r, r2);
    int re = wait_ev(sv[0], POLLIN | POLLHUP, 300);
    check("POLLHUP on a socket whose other end is closed", (re & POLLHUP) != 0, "revents %#x", re);
    gw = write(hold[1], "h", 1);
    int st = reap(c);
    check("child of the socket test", st == 0, "exit %d", st);
    close(sv[0]); close(go[1]); close(hold[1]);
}

static void t_fork_eventfd_epoll(void) {
    int ev = eventfd(0, EFD_NONBLOCK);
    int ep = epoll_create1(0);
    int r[2];
    if (ev < 0 || ep < 0 || pipe(r)) { check("eventfd/epoll", 0, "errno %d", errno); return; }
    struct epoll_event ee = { .events = EPOLLIN, .data.u64 = 77 };
    epoll_ctl(ep, EPOLL_CTL_ADD, r[0], &ee);
    pid_t c = fork();
    if (c == 0) {
        uint64_t five = 5;
        int ok = write(ev, &five, 8) == 8;
        close(ev); close(ep); close(r[0]); close(r[1]);
        _exit(ok ? 0 : 3);
    }
    int st = reap(c);
    uint64_t v = 0, two = 2, v2 = 0;
    long a = read(ev, &v, 8);
    long b = write(ev, &two, 8);
    long d = read(ev, &v2, 8);
    check("an eventfd is shared across fork and survives the child's close", st == 0 && a == 8 && v == 5 && b == 8 && d == 8 && v2 == 2,
          "child %d, read %ld value %lu, then write %ld read %ld value %lu", st, a, (unsigned long)v, b, d, (unsigned long)v2);
    long w = write(r[1], "e", 1);
    struct epoll_event out;
    memset(&out, 0, sizeof out);
    int n = epoll_wait(ep, &out, 1, 1000);
    check("an epoll instance survives the child's close of its copy", w == 1 && n == 1 && out.data.u64 == 77, "epoll_wait = %d", n);
    close(ev); close(ep); close(r[0]); close(r[1]);
}

static int send_fd(int sock, int fd) {
    char c = 'F';
    struct iovec iov = { .iov_base = &c, .iov_len = 1 };
    char cbuf[CMSG_SPACE(sizeof(int))];
    memset(cbuf, 0, sizeof cbuf);
    struct msghdr msg = { .msg_iov = &iov, .msg_iovlen = 1, .msg_control = cbuf, .msg_controllen = sizeof cbuf };
    struct cmsghdr *cm = CMSG_FIRSTHDR(&msg);
    cm->cmsg_level = SOL_SOCKET; cm->cmsg_type = SCM_RIGHTS; cm->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(cm), &fd, sizeof(int));
    return sendmsg(sock, &msg, 0) == 1 ? 0 : -1;
}

static int recv_fd(int sock, int flags) {
    char c = 0;
    struct iovec iov = { .iov_base = &c, .iov_len = 1 };
    char cbuf[CMSG_SPACE(sizeof(int))];
    for (int i = 0; i < 250; i++) {
        memset(cbuf, 0, sizeof cbuf);
        struct msghdr msg = { .msg_iov = &iov, .msg_iovlen = 1, .msg_control = cbuf, .msg_controllen = sizeof cbuf };
        long r = recvmsg(sock, &msg, flags | MSG_DONTWAIT);
        if (r == 1) {
            struct cmsghdr *cm = CMSG_FIRSTHDR(&msg);
            if (!cm || cm->cmsg_type != SCM_RIGHTS) return -1;
            int fd; memcpy(&fd, CMSG_DATA(cm), sizeof(int));
            return fd;
        }
        if (r == 0) return -3;
        usleep(20000);
    }
    return -2;
}

static void t_scm(void) {
    int sv[2], res[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) || pipe(res)) { check("socketpair", 0, "errno %d", errno); return; }
    pid_t c = fork();
    if (c == 0) {
        close(sv[0]); close(res[0]);
        int probe = dup(0);
        close(probe);
        int f = recv_fd(sv[1], 0);
        int v[6] = { probe, f, -1, -1, -1, -1 };
        char b[3] = {0};
        v[2] = (int)read(f, b, 2);
        v[3] = (b[0] << 8) | b[1];
        int w = recv_fd(sv[1], MSG_CMSG_CLOEXEC);
        v[4] = w >= 0 ? fcntl(w, F_GETFD) : -9;
        v[5] = w;
        if (write(res[1], v, sizeof v) != sizeof v) _exit(8);
        if (w >= 0) { if (write(w, "P", 1) != 1) _exit(7); close(w); }
        usleep(800000); /* alive, with the received write end closed */
        _exit(0);
    }
    close(sv[1]); close(res[1]);
    /* Opened after the fork: the child can only get these over the socket. */
    int F = mkfile("/tmp/gfdsem-s", "abcdefgh");
    char b[4] = {0};
    int pw[2];
    if (read(F, b, 2) != 2 || pipe(pw)) { check("scm setup", 0, "errno %d", errno); return; }
    int s1 = send_fd(sv[0], F);
    close(F); /* the sender's only descriptor: the one in flight keeps the file open */
    int s2 = send_fd(sv[0], pw[1]);
    close(pw[1]);
    int v[6] = { -1, -1, -1, -1, -1, -1 };
    long got = read_to(res[0], v, sizeof v, 8000);
    check("SCM_RIGHTS: the descriptor arrives under the receiver's lowest free number",
          s1 == 0 && got == (long)sizeof v && v[1] >= 0 && v[1] == v[0], "receiver's lowest free %d, got %d", v[0], v[1]);
    check("SCM_RIGHTS: the description (offset) is the sender's, and outlives the sender's close",
          v[2] == 2 && v[3] == (('c' << 8) | 'd'), "receiver read %d bytes, %#x (want \"cd\" = %#x)", v[2], v[3], ('c' << 8) | 'd');
    check("SCM_RIGHTS: MSG_CMSG_CLOEXEC marks the received descriptor", s2 == 0 && v[5] >= 0 && v[4] == FD_CLOEXEC, "fd %d F_GETFD %d", v[5], v[4]);
    char ch = 0;
    long r = read_to(pw[0], &ch, 1, 3000);
    long r2 = read_to(pw[0], &ch, 1, 2000);
    check("SCM_RIGHTS: a passed pipe end is an end (data, then end-of-file when the receiver closes it)",
          r == 1 && r2 == 0, "read %ld then %ld (want 1 then 0)", r, r2);
    int st = reap(c);
    check("child of the SCM_RIGHTS test", st == 0, "exit %d", st);
    close(sv[0]); close(res[0]); close(pw[0]);
}

static void t_exec(const char *self) {
    int base = mkfile("/tmp/gfdsem-a", "abcdefgh");
    int keep = dup2(base, 40);
    int gone = fcntl(base, F_DUPFD_CLOEXEC, 41);
    char b[3];
    if (read(base, b, 2) != 2) { check("exec setup", 0, "errno %d", errno); return; }
    pid_t c = fork();
    if (c == 0) {
        char *argv[] = { (char *)"gfdsem", (char *)"--after-exec", (char *)"40", (char *)"41", NULL };
        char *envp[] = { (char *)"PATH=/bin", NULL };
        execve(self, argv, envp);
        _exit(90);
    }
    int st = reap(c);
    check("exec closes close-on-exec descriptors and keeps the others (with their offset)", keep == 40 && gone == 41 && st == 0,
          "the exec'd image reported %d (0 = fd 40 open at offset 2 and fd 41 closed; 90 = execve failed)", st);
    close(base); close(40); close(41);
}

/* The image after exec: argv = --after-exec KEEP GONE. */
static int after_exec(int keep, int gone) {
    char b[3] = {0};
    if (fcntl(keep, F_GETFD) != 0) return 31;
    if (fcntl(gone, F_GETFD) != -1 || errno != EBADF) return 32;
    if (read(keep, b, 2) != 2 || memcmp(b, "cd", 2)) return 33;
    return 0;
}
#endif /* !NOFORK */

int main(int argc, char **argv) {
#ifndef NOFORK
    if (argc == 4 && !strcmp(argv[1], "--after-exec")) return after_exec(atoi(argv[2]), atoi(argv[3]));
#endif
    (void)argc; (void)argv;
    signal(SIGPIPE, SIG_IGN);
    setvbuf(stdout, NULL, _IOLBF, 0);
    t_dup_offset();
    t_lowest();
    t_pipe_one_process();
    t_cloexec_flags();
    t_numbers();
    t_proc_self_fd();
    t_close_range();
    t_limit();
#ifndef NOFORK
    t_fork_offset();
    t_fork_close();
    t_fork_pipe_eof();
    t_fork_socket_eof();
    t_fork_eventfd_epoll();
    t_scm();
    /* "--exec": the kernel can exec this program again (a glibc fork child on
       EuroOS re-executes only the hosted browser; the exec rule is covered
       there by userland/fdsem.c on the background path). */
    if (argc > 1 && !strcmp(argv[1], "--exec")) t_exec("/proc/self/exe");
#endif
    printf(TAG ": %s %d/%d\n", fails ? "FAILED" : "PASS", total - fails, total);
    fflush(stdout);
    return fails ? (fails > 100 ? 100 : fails) : 171;
}

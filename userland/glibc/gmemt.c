/* gmemt: memory that comes back, and what happens when it runs out.
 *
 *   gmemt forkfail   a fork the kernel refuses must give back all it took
 *   gmemt forkmany   270 children one after the other, every second one ended by a signal:
 *                    the slots of ended processes are used again (the task table has 256)
 *   gmemt forkidle   40 children that sit idle at the same time, as the utility processes of
 *                    a browser do: the arena of a child costs what the child has in it, not
 *                    its full size (BUG-156); a child sees the memory of its parent, zeros
 *                    where nobody wrote, and the kernel can write into such a page
 *   gmemt madvise    madvise(MADV_DONTNEED) gives the frames back; the pages read as zeros
 *   gmemt memfd      a memory file is made of frames, not of kernel heap; one memory for
 *                    read/write and every mapping; ENOSPC at its limit, no kill
 *   gmemt oom        the main process needs a page while a child holds the pool: the
 *                    child is ended, the main process goes on
 *
 * The kernel's account is read from /proc/euroos/mem (kB per line). Every check
 * prints one line; the last line is "GMEMT <mode>: PASS n/n" or "FAILED".
 * Build: gcc -O1 -o gmemt gmemt.c   (dynamic, glibc)
 */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <sys/syscall.h>

static int checks, passed;
static void check(int ok, const char *fmt, long a, long b, long c)
{
    char line[200];
    snprintf(line, sizeof line, fmt, a, b, c);
    checks++;
    passed += !!ok;
    printf("%s %s\n", ok ? "ok  " : "FAIL", line);
    fflush(stdout);
}

/* One value of /proc/euroos/mem in kB, -1 when it is not there. */
static long kacct(const char *key)
{
    char buf[1024];
    int fd = open("/proc/euroos/mem", O_RDONLY);
    if (fd < 0)
        return -1;
    int n = read(fd, buf, sizeof buf - 1);
    close(fd);
    if (n <= 0)
        return -1;
    buf[n] = 0;
    char *p = strstr(buf, key);
    if (!p)
        return -1;
    return atol(p + strlen(key));
}

#define MIB (1024L * 1024L)

static void touch(volatile char *p, long len, char v)
{
    for (long i = 0; i < len; i += 4096)
        p[i] = v;
}

static int mode_forkfail(void)
{
    /* Something in the demand region for the fork to copy. */
    long len = 8 * MIB;
    char *p = mmap(0, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED)
        return 1;
    touch(p, len, 1);
    long d0 = kacct("demand_free_kb "), p0 = kacct("procpool_free_kb ");
    int refused = 0, made = 0;
    for (int i = 0; i < 3; i++) {
        pid_t c = fork();
        if (c == 0)
            _exit(0);
        if (c < 0) {
            refused++;
        } else {
            made++;
            waitpid(c, 0, 0);
        }
    }
    long d1 = kacct("demand_free_kb "), p1 = kacct("procpool_free_kb ");
    check(refused == 3, "three forks refused by the kernel (%ld refused, %ld made)", refused, made, 0);
    check(d0 >= 0 && d1 >= d0 - 64, "demand pool after the refused forks: %ld kB free, %ld before", d1, d0, 0);
    check(p0 >= 0 && p1 >= p0 - 64, "process pool after the refused forks: %ld kB free, %ld before", p1, p0, 0);
    /* And a fork that is allowed still works afterwards. */
    pid_t c = fork();
    if (c == 0)
        _exit(7);
    int st = 0;
    if (c > 0)
        waitpid(c, &st, 0);
    check(c > 0 && WIFEXITED(st) && WEXITSTATUS(st) == 7, "a fork after them works (pid %ld, status 0x%lx)", c, st, 0);
    return 0;
}

static int mode_forkmany(void)
{
    long d0 = kacct("demand_free_kb "), p0 = kacct("procpool_free_kb ");
    int made = 0, refused = 0, wrong = 0, first_errno = 0;
    for (int i = 0; i < 270; i++) {
        pid_t c = fork();
        if (c == 0) {
            if (i & 1)
                for (;;)
                    pause();
            _exit(i & 0x7f);
        }
        if (c < 0) {
            if (!refused)
                first_errno = errno;
            refused++;
            continue;
        }
        made++;
        if (i & 1)
            kill(c, SIGTERM);
        int st = 0;
        if (waitpid(c, &st, 0) != c)
            wrong++;
        else if (i & 1)
            wrong += !(WIFSIGNALED(st) && WTERMSIG(st) == SIGTERM);
        else
            wrong += !(WIFEXITED(st) && WEXITSTATUS(st) == (i & 0x7f));
    }
    check(made == 270 && refused == 0, "270 forks one after the other: %ld made, %ld refused (first errno %ld)", made, refused, first_errno);
    check(wrong == 0, "every child reported the way it ended (%ld did not)", wrong, 0, 0);
    long d1 = kacct("demand_free_kb "), p1 = kacct("procpool_free_kb ");
    check(d1 >= d0 - 64 && p1 >= p0 - 64, "pools afterwards: demand %ld kB free, process %ld kB free (demand %ld before)", d1, p1, d0);
    return 0;
}

#include <poll.h>
static char pat(long off)
{
    return (char)((off >> 12) | 1);
}

static int mode_forkidle(void)
{
    enum { MAXKIDS = 40 };
    long p0 = kacct("procpool_free_kb ");
    /* Arena memory (the brk heap): 1 MiB with something in every page, then
     * 1 MiB nobody wrote. */
    long len = 1 * MIB;
    char *a = sbrk(2 * len);
    if (a == (char *)-1)
        return 1;
    for (long j = 0; j < len; j += 4096)
        a[j] = pat(j);
    /* A machine with a small pool: as many as fit at 8 MiB each. */
    int want = MAXKIDS;
    if (p0 > 0 && p0 / 8192 < want)
        want = p0 / 8192 < 4 ? 4 : p0 / 8192;
    int pfd[2];
    if (pipe(pfd))
        return 1;
    pid_t kids[MAXKIDS];
    int made = 0, refused = 0, first_errno = 0;
    for (int i = 0; i < want; i++) {
        pid_t c = fork();
        if (c == 0) {
            char res = 'k';
            close(pfd[0]);
            for (long j = 0; j < len; j += 4096)
                if (a[j] != pat(j))
                    res = 'P';
            for (long j = len; j < 2 * len; j += 4096)
                if (a[j] != 0)
                    res = 'Z';
            /* A page of its own in both halves. */
            a[i * 4096L] = 'c';
            a[len + i * 4096L] = 'c';
            /* The kernel writes into an arena page nobody touched yet. */
            char *k = a + len + (i + 64) * 4096L + 100;
            int fd = open("/proc/euroos/mem", O_RDONLY);
            if (fd < 0 || read(fd, k, 512) <= 0 || k[0] == 0)
                res = 'K';
            if (fd >= 0)
                close(fd);
            if (write(pfd[1], &res, 1) != 1)
                _exit(3);
            for (;;)
                pause();
        }
        if (c < 0) {
            if (!refused)
                first_errno = errno;
            refused++;
            continue;
        }
        kids[made++] = c;
    }
    int good = 0, bad = 0;
    for (int i = 0; i < made; i++) {
        struct pollfd pf = { pfd[0], POLLIN, 0 };
        char r = 0;
        if (poll(&pf, 1, 20000) != 1 || read(pfd[0], &r, 1) != 1)
            break;
        if (r == 'k')
            good++;
        else
            bad++;
    }
    long p1 = kacct("procpool_free_kb ");
    check(made == want && refused == 0, "%ld idle children at the same time: %ld made (first errno of a refused one %ld)", want, made, first_errno);
    check(made > 0 && good == made && bad == 0,
          "each sees the arena of its parent, zeros where nobody wrote, and the kernel wrote into a page it never touched (%ld of %ld, %ld wrong)",
          good, made, bad);
    long per = made ? (p0 - p1) / made : -1;
    check(made > 0 && per <= 8192, "an idle child costs %ld kB of the process pool (at most 8192; %ld kB were free)", per, p0, 0);
    int own = 1;
    for (long j = 0; j < len; j += 4096)
        own &= a[j] == pat(j);
    for (long j = len; j < 2 * len; j += 4096)
        own &= a[j] == 0;
    check(own, "the memory of the parent is as it was", 0, 0, 0);
    /* A child that forks hands on its own arena pages, not those of the first parent. */
    pid_t c = fork();
    if (c == 0) {
        a[len] = 'g';
        a[0] = 'h';
        pid_t g = fork();
        if (g == 0)
            _exit(a[len] == 'g' && a[0] == 'h' && a[4096] == pat(4096) && a[len + 4096] == 0 ? 0 : 1);
        int gs = 0;
        if (g > 0)
            waitpid(g, &gs, 0);
        _exit(g > 0 && WIFEXITED(gs) && WEXITSTATUS(gs) == 0 ? 0 : 1);
    }
    int st = -1;
    if (c > 0)
        waitpid(c, &st, 0);
    check(c > 0 && WIFEXITED(st) && WEXITSTATUS(st) == 0, "a child that forks hands on its own arena (pid %ld, status 0x%lx)", c, st, 0);
    for (int i = 0; i < made; i++) {
        kill(kids[i], SIGKILL);
        waitpid(kids[i], 0, 0);
    }
    long p2 = kacct("procpool_free_kb ");
    check(p2 >= p0 - 64, "process pool after they ended: %ld kB free, %ld before", p2, p0, 0);
    return 0;
}

static int mode_madvise(void)
{
    long len = 32 * MIB;
    long d0 = kacct("demand_free_kb ");
    char *p = mmap(0, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED)
        return 1;
    touch(p, len, 0x5a);
    long d1 = kacct("demand_free_kb ");
    check(d0 - d1 >= 30 * 1024, "32 MiB touched: the pool went from %ld to %ld kB free", d0, d1, 0);
    int r = madvise(p, len, MADV_DONTNEED);
    long d2 = kacct("demand_free_kb ");
    check(r == 0 && d2 >= d0 - 1024, "after MADV_DONTNEED: %ld kB free (%ld before the mapping)", d2, d0, 0);
    long bad = 0;
    for (long i = 0; i < len; i += 4096)
        bad += p[i] != 0 || p[i + 4095] != 0;
    check(bad == 0, "the pages read as zeros afterwards (%ld do not)", bad, 0, 0);
    /* Half of it, with MADV_FREE, in the middle of the mapping. */
    touch(p, len, 0x33);
    long d3 = kacct("demand_free_kb ");
    r = madvise(p + 8 * MIB, 16 * MIB, 8 /* MADV_FREE */);
    long d4 = kacct("demand_free_kb ");
    check(r == 0 && d4 - d3 >= 15 * 1024, "MADV_FREE of 16 MiB: %ld -> %ld kB free", d3, d4, 0);
    check(p[0] == 0x33 && p[8 * MIB - 1 - 4095] == 0x33 && p[24 * MIB] == 0x33 && (p[8 * MIB] == 0 || p[8 * MIB] == 0x33),
          "the pages around it keep their bytes", 0, 0, 0);
    munmap(p, len);
    long d5 = kacct("demand_free_kb ");
    check(d5 >= d0 - 1024, "after munmap: %ld kB free (%ld at the start)", d5, d0, 0);
    return 0;
}

static int mode_memfd(void)
{
    long len = 16 * MIB;
    int fd = syscall(SYS_memfd_create, "gmemt", 0);
    if (fd < 0)
        return 1;
    long h0 = kacct("heap_used_kb "), d0 = kacct("demand_free_kb "), f0 = kacct("ramfile_frames_kb ");
    static char chunk[65536];
    for (long off = 0; off < len; off += sizeof chunk) {
        for (unsigned i = 0; i < sizeof chunk; i += 4096)
            *(long *)(chunk + i) = off + i;
        if (write(fd, chunk, sizeof chunk) != (long)sizeof chunk)
            break;
    }
    long h1 = kacct("heap_used_kb "), f1 = kacct("ramfile_frames_kb ");
    check(h1 - h0 < 2048, "16 MiB written to a memfd: the kernel heap grew by %ld kB", h1 - h0, 0, 0);
    check(f1 - f0 >= 16 * 1024, "the file holds %ld kB of frames (%ld before)", f1, f0, 0);
    long *m = mmap(0, len, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    long *m2 = mmap(0, len, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (m == MAP_FAILED || m2 == MAP_FAILED)
        return 1;
    long bad = 0;
    for (long off = 0; off < len; off += 4096)
        bad += m[off / 8] != off;
    check(bad == 0, "a mapping shows what write() wrote, all 16 MiB (%ld pages differ)", bad, 0, 0);
    long f2 = kacct("ramfile_frames_kb ");
    check(f2 - f1 < 64, "mapping and reading it took %ld kB more frames (one memory, not a copy)", f2 - f1, 0, 0);
    m[12 * MIB / 8] = 0x1122334455667788L;
    long v = 0;
    pread(fd, &v, 8, 12 * MIB);
    check(v == 0x1122334455667788L && m2[12 * MIB / 8] == v, "a store through one mapping is read by pread and the other mapping (%lx)", v, 0, 0);
    v = 0x99;
    pwrite(fd, &v, 8, 5 * MIB);
    check(m[5 * MIB / 8] == 0x99, "pwrite is seen through the mapping", 0, 0, 0);
    check(ftruncate(fd, 4 * MIB) == 0 && kacct("ramfile_frames_kb ") - f0 <= 4 * 1024 + 64, "ftruncate to 4 MiB gives 12 MiB of frames back (%ld kB held)",
          kacct("ramfile_frames_kb ") - f0, 0, 0);
    munmap(m, len);
    munmap(m2, len);
    close(fd);
    long d3 = kacct("demand_free_kb "), f3 = kacct("ramfile_frames_kb ");
    check(f3 <= f0 + 64 && d3 >= d0 - 1024, "after the last close and unmap: %ld kB of file frames, pool %ld kB free (%ld at the start)", f3, d3, d0);
    /* The limit: a file that grows until the kernel says no. */
    long limit = kacct("ramfile_limit_kb ");
    fd = syscall(SYS_memfd_create, "gmemt-big", 0);
    long wrote = 0, cap = (limit > 0 ? limit : 48 * 1024) + 16 * 1024;
    int err = 0;
    memset(chunk, 0x77, sizeof chunk);
    while (wrote < cap) {
        long k = write(fd, chunk, sizeof chunk);
        if (k <= 0) {
            err = errno;
            break;
        }
        wrote += k / 1024;
    }
    check(err == ENOSPC && limit > 0 && wrote <= limit, "a file that keeps growing gets ENOSPC (errno %ld) after %ld kB; the limit is %ld kB", err, wrote, limit);
    check(ftruncate(fd, 1L << 41) != 0, "ftruncate to 2 TiB is refused (errno %ld)", errno, 0, 0);
    /* The program itself still gets memory. */
    char *a = mmap(0, 4 * MIB, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    touch(a, 4 * MIB, 1);
    check(a[4 * MIB - 4096] == 1, "with the file at its limit the program still gets 4 MiB of its own", 0, 0, 0);
    close(fd);
    long d4 = kacct("demand_free_kb ");
    check(d4 >= d0 - 6 * 1024, "the big file is gone after its close: pool %ld kB free (%ld at the start)", d4, d0, 0);
    return 0;
}

static int mode_oom(void)
{
    long freekb = kacct("demand_free_kb ");
    if (freekb <= 0 || freekb > 512 * 1024) {
        printf("FAIL the test needs a small pool (demand_free_kb %ld)\n", freekb);
        return 1;
    }
    long hog = (freekb * 1024 / 10 * 6) & ~4095L; /* 60% of what is free, each */
    int pfd[2];
    if (pipe(pfd))
        return 1;
    pid_t c = fork();
    if (c == 0) {
        char *p = mmap(0, hog, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        touch(p, hog, 2);
        write(pfd[1], "r", 1);
        for (;;)
            pause();
    }
    check(c > 0, "a child is forked (pid %ld) and takes %ld kB", c, hog / 1024, 0);
    if (c <= 0)
        return 1;
    char b;
    read(pfd[0], &b, 1);
    char *p = mmap(0, hog, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    touch(p, hog, 3); /* the pool cannot hold both */
    check(p[hog - 4096] == 3, "the main process got its %ld kB although the pool was full", hog / 1024, 0, 0);
    int st = 0;
    pid_t w = waitpid(c, &st, WNOHANG);
    for (int i = 0; w == 0 && i < 200; i++) {
        usleep(10000);
        w = waitpid(c, &st, WNOHANG);
    }
    check(w == c && WIFSIGNALED(st) && WTERMSIG(st) == SIGKILL, "the child was ended for it (wait status 0x%lx)", st, 0, 0);
    if (w == 0)
        kill(c, SIGKILL);
    munmap(p, hog);
    /* A child that takes more than there is: it is the one that goes. */
    long pool = kacct("demand_total_kb ");
    c = fork();
    if (c == 0) {
        long big = pool * 1024 * 2;
        char *q = mmap(0, big, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        touch(q, big, 4);
        _exit(0); /* not reached */
    }
    st = 0;
    w = 0;
    for (int i = 0; c > 0 && w == 0 && i < 3000; i++) {
        usleep(10000);
        w = waitpid(c, &st, WNOHANG);
    }
    check(w == c && WIFSIGNALED(st) && WTERMSIG(st) == SIGKILL, "a child that touches twice the pool is ended (wait status 0x%lx, pid %ld)", st, w, 0);
    if (w == 0)
        kill(c, SIGKILL);
    char *r = mmap(0, 4 * MIB, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    touch(r, 4 * MIB, 5);
    check(r[4 * MIB - 4096] == 5, "and the main process still gets memory afterwards", 0, 0, 0);
    return 0;
}

int main(int argc, char **argv)
{
    const char *mode = argc > 1 ? argv[1] : "";
    int r = 1;
    if (!strcmp(mode, "forkfail"))
        r = mode_forkfail();
    else if (!strcmp(mode, "forkmany"))
        r = mode_forkmany();
    else if (!strcmp(mode, "forkidle"))
        r = mode_forkidle();
    else if (!strcmp(mode, "madvise"))
        r = mode_madvise();
    else if (!strcmp(mode, "memfd"))
        r = mode_memfd();
    else if (!strcmp(mode, "oom"))
        r = mode_oom();
    int ok = r == 0 && checks > 0 && passed == checks;
    printf("GMEMT %s: %s %d/%d\n", mode, ok ? "PASS" : "FAILED", passed, checks);
    return ok ? 0 : 1;
}

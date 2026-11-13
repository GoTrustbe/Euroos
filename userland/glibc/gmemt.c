/* gmemt: memory that comes back, and what happens when it runs out.
 *
 *   gmemt forkfail   a fork the kernel refuses must give back all it took
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
    long hog = freekb * 1024 / 10 * 6; /* 60% of what is free, each */
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
    return 0;
}

int main(int argc, char **argv)
{
    const char *mode = argc > 1 ? argv[1] : "";
    int r = 1;
    if (!strcmp(mode, "forkfail"))
        r = mode_forkfail();
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

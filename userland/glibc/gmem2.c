/* gmem2: memory of fork children, files and the machine, second round.
 *
 *   gmem2 cowfork    fork of a 64 MiB process (8 MiB of arena, 56 MiB of demand pages):
 *                    the time and the frames the fork takes; the child and the parent see
 *                    each other's memory as it was at the fork and never what the other
 *                    writes afterwards, also when the kernel writes (read() into a page);
 *                    MADV_DONTNEED and munmap on one side leave the other side alone;
 *                    a grandchild; every frame back after the end (copy-on-write)
 *   gmem2 ramnamed   a file with a name in /tmp is frames of the demand pool, not kernel heap:
 *                    read back after its close, stat, mmap, rename, O_APPEND, ftruncate,
 *                    O_TRUNC, ENOSPC at the limit, unlink gives the frames back
 *   gmem2 sysinfo    sysinfo() and /proc/meminfo report the pools of the machine
 *
 * The kernel's account is read from /proc/euroos/mem (kB per line). Every check
 * prints one line; the last line is "GMEM2 <mode>: PASS n/n" or "FAILED".
 * Build: gcc -O1 -o gmem2 gmem2.c   (dynamic, glibc)
 */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <time.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <sys/syscall.h>

static int checks, passed;
static void check(int ok, const char *fmt, long a, long b, long c)
{
    char line[220];
    snprintf(line, sizeof line, fmt, a, b, c);
    checks++;
    passed += !!ok;
    printf("%s %s\n", ok ? "ok  " : "FAIL", line);
    fflush(stdout);
}

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
#define PG 4096L

static long now_us(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000000L + ts.tv_nsec / 1000;
}

/* The value page i of region r holds at the fork. */
static long pat(int r, long i)
{
    return ((i * 2654435761L) ^ ((long)r << 40)) | 1;
}
static void fill(char *p, long len, int r)
{
    for (long i = 0; i < len / PG; i++) {
        *(long *)(p + i * PG) = pat(r, i);
        *(long *)(p + i * PG + PG - 8) = pat(r, i);
    }
}
/* Pages [from, n) step `step` hold pat(r) (want == 0) or `want`. Returns the pages that differ. */
static long differ(char *p, long from, long n, long step, int r, long want)
{
    long bad = 0;
    for (long i = from; i < n; i += step) {
        long v = want ? want : pat(r, i);
        bad += *(long *)(p + i * PG) != v || *(long *)(p + i * PG + PG - 8) != v;
    }
    return bad;
}
static void put(char *p, long from, long n, long step, long v)
{
    for (long i = from; i < n; i += step) {
        *(long *)(p + i * PG) = v;
        *(long *)(p + i * PG + PG - 8) = v;
    }
}

static long taken(long p0, long d0)
{
    return (p0 - kacct("procpool_free_kb ")) + (d0 - kacct("demand_free_kb "));
}

static int to_child[2], to_parent[2];
static void say(int fd, char c)
{
    if (write(fd, &c, 1) != 1)
        _exit(90);
}
static char hear(int fd)
{
    char c = 0;
    if (read(fd, &c, 1) != 1)
        return 0;
    return c;
}

/* Pages 0..3 of each region are for the kernel-write checks; the even/odd
 * checks start at page 4. */
#define FIRST 4

static char *a, *d, *m;
/* 8 MiB of arena (the brk window of the 96 MiB test arena is 12 MiB) and
 * 56 MiB of demand pages: a process of 64 MiB. */
static long alen = 8 * MIB, dlen = 56 * MIB, mlen = 4 * MIB, an, dn;

/* The child of cowfork: answers each step of the parent with one byte. */
static void cow_child(void)
{
    long bad = differ(a, 0, an, 1, 1, 0) + differ(d, 0, dn, 1, 2, 0) + differ(m, 0, mlen / PG, 1, 3, 0);
    say(to_parent[1], bad ? 'X' : 'r');
    /* The kernel writes into shared pages of the child: read() from a pipe. */
    hear(to_child[0]);
    if (read(to_child[0], a + 1 * PG + 64, 8) != 8 || read(to_child[0], d + 1 * PG + 64, 8) != 8)
        say(to_parent[1], 'X');
    long k1 = *(long *)(a + 1 * PG + 64), k2 = *(long *)(d + 1 * PG + 64);
    say(to_parent[1], k1 == 0x1111 && k2 == 0x2222 ? 'k' : 'K');
    /* Then the parent has done the same with pages 2: not seen here. */
    hear(to_child[0]);
    bad = *(long *)(a + 2 * PG + 64) != 0 || *(long *)(d + 2 * PG + 64) != 0
        || differ(a, 2, 3, 1, 1, 0) || differ(d, 2, 3, 1, 2, 0);
    say(to_parent[1], bad ? 'K' : 'k');
    /* MADV_DONTNEED on the first half of m: zeros here, not in the parent. */
    hear(to_child[0]);
    bad = madvise(m, mlen / 2, MADV_DONTNEED) != 0;
    for (long i = 0; i < mlen / 2 / PG; i++)
        bad += *(long *)(m + i * PG) != 0 || *(long *)(m + i * PG + PG - 8) != 0;
    say(to_parent[1], bad ? 'M' : 'm');
    /* The parent unmapped the second half of m: still here. */
    hear(to_child[0]);
    say(to_parent[1], differ(m, mlen / 2 / PG, mlen / PG, 1, 3, 0) ? 'U' : 'u');
    /* The child writes the even pages. */
    hear(to_child[0]);
    put(a, FIRST, an, 2, 0x63);
    put(d, FIRST, dn, 2, 0x63);
    say(to_parent[1], 'w');
    /* The parent wrote the odd pages: the child still sees the fork there. */
    hear(to_child[0]);
    bad = differ(a, FIRST, an, 2, 1, 0x63) + differ(d, FIRST, dn, 2, 2, 0x63)
        + differ(a, FIRST + 1, an, 2, 1, 0) + differ(d, FIRST + 1, dn, 2, 2, 0);
    say(to_parent[1], bad ? 'V' : 'v');
    /* A grandchild writes a page of each kind; the child does not see it. */
    hear(to_child[0]);
    pid_t g = fork();
    if (g == 0) {
        int ok = *(long *)(a + 5 * PG) == pat(1, 5) && *(long *)(d + 4 * PG) == 0x63;
        *(long *)(a + 5 * PG) = 0x67;
        *(long *)(d + 4 * PG) = 0x67;
        *(long *)(a + 7 * PG) = 0x67;
        _exit(ok ? 0 : 1);
    }
    int gs = -1;
    if (g > 0)
        waitpid(g, &gs, 0);
    bad = !(g > 0 && WIFEXITED(gs) && WEXITSTATUS(gs) == 0) || *(long *)(a + 5 * PG) != pat(1, 5)
        || *(long *)(d + 4 * PG) != 0x63 || *(long *)(a + 7 * PG) != pat(1, 7);
    say(to_parent[1], bad ? 'G' : 'g');
    hear(to_child[0]);
    _exit(0);
}

static int mode_cowfork(void)
{
    an = alen / PG;
    dn = dlen / PG;
    long b0p = kacct("procpool_free_kb "), b0d = kacct("demand_free_kb ");
    a = sbrk(alen);
    d = mmap(0, dlen, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    m = mmap(0, mlen, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (a == (char *)-1 || d == MAP_FAILED || m == MAP_FAILED) {
        printf("FAIL no memory for the test (sbrk %p, mmap %p %p)\n", a, d, m);
        return 1;
    }
    fill(a, alen, 1);
    fill(d, dlen, 2);
    fill(m, mlen, 3);
    if (pipe(to_child) || pipe(to_parent))
        return 1;
    long p0 = kacct("procpool_free_kb "), d0 = kacct("demand_free_kb ");
    long t0 = now_us();
    pid_t c = fork();
    if (c == 0)
        cow_child();
    long t1 = now_us();
    if (c < 0) {
        printf("FAIL fork refused (errno %d)\n", errno);
        return 1;
    }
    long t_fork = taken(p0, d0);
    printf("info fork of a 64 MiB process: %ld us in the parent, %ld kB of frames\n", t1 - t0, t_fork);
    check(t_fork <= 4096, "the fork took %ld kB of frames for a process of 64 MiB (at most 4096)", t_fork, 0, 0);
    char r = hear(to_parent[0]);
    check(r == 'r', "the child sees the memory of its parent as it was at the fork (%ld)", r, 0, 0);
    long t_read = taken(p0, d0);
    check(t_read <= 4096, "after the child read all of it the two hold %ld kB more than the parent alone (at most 4096)", t_read, 0, 0);
    /* The kernel writes into shared pages: of the child, then of the parent. */
    long v1 = 0x1111, v2 = 0x2222, v3 = 0x3333, v4 = 0x4444;
    say(to_child[1], 'k');
    if (write(to_child[1], &v1, 8) != 8 || write(to_child[1], &v2, 8) != 8)
        return 1;
    r = hear(to_parent[0]);
    check(r == 'k', "read() by the child into two of its shared pages lands there (%ld)", r, 0, 0);
    check(*(long *)(a + 1 * PG + 64) == 0 && *(long *)(d + 1 * PG + 64) == 0 && !differ(a, 1, 2, 1, 1, 0) && !differ(d, 1, 2, 1, 2, 0),
          "and not in the parent", 0, 0, 0);
    int pp[2];
    if (pipe(pp))
        return 1;
    if (write(pp[1], &v3, 8) != 8 || write(pp[1], &v4, 8) != 8 || read(pp[0], a + 2 * PG + 64, 8) != 8 || read(pp[0], d + 2 * PG + 64, 8) != 8)
        return 1;
    check(*(long *)(a + 2 * PG + 64) == v3 && *(long *)(d + 2 * PG + 64) == v4, "read() by the parent into two of its shared pages lands there", 0, 0, 0);
    say(to_child[1], 'k');
    r = hear(to_parent[0]);
    check(r == 'k', "and not in the child (%ld)", r, 0, 0);
    /* MADV_DONTNEED and munmap. */
    say(to_child[1], 'm');
    r = hear(to_parent[0]);
    check(r == 'm' && !differ(m, 0, mlen / 2 / PG, 1, 3, 0), "MADV_DONTNEED in the child: zeros there, the parent keeps its bytes (%ld)", r, 0, 0);
    munmap(m + mlen / 2, mlen / 2);
    say(to_child[1], 'u');
    r = hear(to_parent[0]);
    check(r == 'u', "munmap in the parent: the child keeps its bytes (%ld)", r, 0, 0);
    /* The child writes the even pages: it gets frames for those only. */
    long before = taken(p0, d0);
    say(to_child[1], 'w');
    r = hear(to_parent[0]);
    long grew = taken(p0, d0) - before;
    long half = ((an - FIRST) / 2 + (dn - FIRST) / 2) * 4;
    check(r == 'w' && grew >= half - 1024 && grew <= half + 4096,
          "the child wrote %ld kB of pages and took %ld kB of frames for it (%ld)", half, grew, r);
    /* The parent writes the odd pages and keeps seeing its own even ones. */
    long t2 = now_us();
    put(a, FIRST + 1, an, 2, 0x70);
    put(d, FIRST + 1, dn, 2, 0x70);
    long t3 = now_us();
    long bad = differ(a, FIRST, an, 2, 1, 0) + differ(d, FIRST, dn, 2, 2, 0)
        + differ(a, FIRST + 1, an, 2, 1, 0x70) + differ(d, FIRST + 1, dn, 2, 2, 0x70);
    check(bad == 0, "the parent sees its own writes and not the child's (%ld pages differ; writing %ld kB took %ld us)", bad, half, t3 - t2);
    say(to_child[1], 'v');
    r = hear(to_parent[0]);
    check(r == 'v', "the child sees its own writes and not the parent's (%ld)", r, 0, 0);
    say(to_child[1], 'g');
    r = hear(to_parent[0]);
    check(r == 'g', "a grandchild writes pages shared by three: only it sees them (%ld)", r, 0, 0);
    check(*(long *)(a + 5 * PG) == 0x70 && *(long *)(a + 7 * PG) == 0x70 && *(long *)(d + 4 * PG) == pat(2, 4),
          "nor does the parent", 0, 0, 0);
    say(to_child[1], 'x');
    int st = -1;
    waitpid(c, &st, 0);
    check(WIFEXITED(st) && WEXITSTATUS(st) == 0, "the child ended (status 0x%lx)", st, 0, 0);
    long after_child = taken(p0, d0);
    check(after_child <= 2048, "after its end the two regions cost %ld kB more than before the fork (at most 2048)", after_child, 0, 0);
    munmap(d, dlen);
    munmap(m, mlen / 2);
    long b1p = kacct("procpool_free_kb "), b1d = kacct("demand_free_kb ");
    check(b1p >= b0p - 256 && b1d >= b0d - 1024, "every frame back at the end: process pool %ld kB free (%ld at the start), demand pool %ld kB",
          b1p, b0p, b1d);
    printf("info demand pool %ld kB free at the start\n", b0d);
    return 0;
}

/* gmem2 ramnamed: a file with a name in a RAM file system is frames of the
 * demand pool, as a file without a name is (BUG-154), not bytes in the heap. */
#include <sys/stat.h>
static long fpat(long off) { return off * 7 + 3; }
static int mode_ramnamed(void)
{
    const char *path = "/tmp/gmem2-named", *path2 = "/tmp/gmem2-named2";
    long len = 16 * MIB;
    static char chunk[65536];
    unlink(path);
    unlink(path2);
    long h0 = kacct("heap_used_kb "), f0 = kacct("ramfile_frames_kb ");
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0600);
    if (fd < 0)
        return 1;
    for (long off = 0; off < len; off += sizeof chunk) {
        for (unsigned i = 0; i < sizeof chunk; i += 4096)
            *(long *)(chunk + i) = fpat(off + i);
        if (write(fd, chunk, sizeof chunk) != (long)sizeof chunk)
            break;
    }
    long h1 = kacct("heap_used_kb "), f1 = kacct("ramfile_frames_kb ");
    check(h1 - h0 < 2048, "16 MiB written to /tmp/gmem2-named: the kernel heap grew by %ld kB", h1 - h0, 0, 0);
    check(f1 - f0 >= len / 1024 - 64, "the file holds %ld kB of frames (%ld before)", f1, f0, 0);
    close(fd);
    struct stat st;
    check(stat(path, &st) == 0 && st.st_size == len, "after its close it is still there: stat says %ld bytes", (long)st.st_size, 0, 0);
    fd = open(path, O_RDWR);
    long bad = 0, got = 0;
    for (long off = 0; fd >= 0 && off < len; off += sizeof chunk) {
        long k = read(fd, chunk, sizeof chunk);
        if (k != (long)sizeof chunk)
            break;
        got += k;
        for (unsigned i = 0; i < sizeof chunk; i += 4096)
            bad += *(long *)(chunk + i) != fpat(off + i);
    }
    check(got == len && bad == 0, "opened again it reads back as written (%ld bytes, %ld pages differ)", got, bad, 0);
    struct stat fst;
    check(fstat(fd, &fst) == 0 && fst.st_size == len && lseek(fd, 0, SEEK_END) == len, "fstat and SEEK_END agree (%ld)", (long)fst.st_size, 0, 0);
    long v = 0x5151, w = 0;
    pwrite(fd, &v, 8, 5 * MIB + 8);
    long *mp = mmap(0, len, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    check(mp != MAP_FAILED && mp[(5 * MIB + 8) / 8] == v, "a MAP_SHARED mapping sees what pwrite wrote", 0, 0, 0);
    if (mp != MAP_FAILED) {
        mp[(9 * MIB) / 8] = 0x6262;
        pread(fd, &w, 8, 9 * MIB);
        munmap(mp, len);
    }
    check(w == 0x6262, "pread sees a store through the mapping (%lx)", w, 0, 0);
    close(fd);
    check(rename(path, path2) == 0 && access(path, F_OK) != 0, "rename: the old name is gone", 0, 0, 0);
    fd = open(path2, O_RDWR | O_APPEND);
    w = 0;
    pread(fd, &w, 8, 9 * MIB);
    check(fd >= 0 && w == 0x6262, "the new name has the bytes (%lx)", w, 0, 0);
    v = 0x7373;
    write(fd, &v, 8);
    check(fstat(fd, &fst) == 0 && fst.st_size == len + 8, "O_APPEND writes at the end (size %ld)", (long)fst.st_size, 0, 0);
    check(ftruncate(fd, 4 * MIB) == 0 && kacct("ramfile_frames_kb ") - f0 <= 4 * 1024 + 64, "ftruncate to 4 MiB gives the rest back (%ld kB held)",
          kacct("ramfile_frames_kb ") - f0, 0, 0);
    close(fd);
    fd = open(path2, O_RDWR | O_TRUNC);
    check(fd >= 0 && fstat(fd, &fst) == 0 && fst.st_size == 0 && kacct("ramfile_frames_kb ") - f0 <= 64, "O_TRUNC empties it (%ld kB held)",
          kacct("ramfile_frames_kb ") - f0, 0, 0);
    /* The limit: a named file that grows until the kernel says no, and the heap stays where it is. */
    long limit = kacct("ramfile_limit_kb "), wrote = 0, cap = (limit > 0 ? limit : 48 * 1024) + 16 * 1024;
    int err = 0;
    memset(chunk, 0x77, sizeof chunk);
    while (fd >= 0 && wrote < cap) {
        long k = write(fd, chunk, sizeof chunk);
        if (k <= 0) {
            err = errno;
            break;
        }
        wrote += k / 1024;
    }
    long h2 = kacct("heap_used_kb ");
    check(err == ENOSPC && limit > 0 && wrote <= limit && wrote >= limit / 2, "it gets ENOSPC (errno %ld) after %ld kB; the limit is %ld kB", err, wrote, limit);
    check(h2 - h0 < 2048, "and the kernel heap grew by %ld kB", h2 - h0, 0, 0);
    close(fd);
    unlink(path2);
    long f3 = kacct("ramfile_frames_kb "), h3 = kacct("heap_used_kb ");
    check(f3 <= f0 + 64 && h3 - h0 < 1024, "unlinked: %ld kB of file frames (%ld at the start), heap %+ld kB", f3, f0, h3 - h0);
    return 0;
}

/* gmem2 sysinfo: sysinfo() and /proc/meminfo report the machine, not a fixed 256 MiB. */
#include <sys/sysinfo.h>
static long meminfo(const char *key)
{
    char buf[4096];
    int fd = open("/proc/meminfo", O_RDONLY);
    if (fd < 0)
        return -1;
    int n = read(fd, buf, sizeof buf - 1);
    close(fd);
    if (n <= 0)
        return -1;
    buf[n] = 0;
    char *p = strstr(buf, key);
    return p ? atol(p + strlen(key)) : -1;
}
static int mode_sysinfo(void)
{
    /* Something in a memory file, for sharedram. */
    int mfd = syscall(SYS_memfd_create, "gmem2-si", 0);
    static char chunk[65536];
    memset(chunk, 1, sizeof chunk);
    for (int i = 0; mfd >= 0 && i < 128; i++)
        if (write(mfd, chunk, sizeof chunk) != (long)sizeof chunk)
            break;
    struct sysinfo si;
    memset(&si, 0, sizeof si);
    int r = sysinfo(&si);
    long dt = kacct("demand_total_kb "), df = kacct("demand_free_kb "), pf = kacct("procpool_free_kb "), pc = kacct("pagecache_kb ");
    long u = si.mem_unit ? si.mem_unit : 1;
    long total = si.totalram * u / 1024, freek = si.freeram * u / 1024, shared = si.sharedram * u / 1024, buf = si.bufferram * u / 1024;
    printf("info sysinfo: totalram %ld kB, freeram %ld kB, sharedram %ld kB, bufferram %ld kB, unit %ld\n", total, freek, shared, buf, u);
    check(r == 0 && total > 256 * 1024 && total >= dt + pf, "totalram is the machine: %ld kB (pools %ld kB + %ld kB)", total, dt, pf);
    check(freek >= df + pf - 8192 && freek <= df + pf + 8192, "freeram %ld kB is what the pools have free (%ld kB)", freek, df + pf, 0);
    check(shared >= 8 * 1024, "sharedram %ld kB holds the 8 MiB memory file", shared, 0, 0);
    check(buf >= pc - 4096 && buf <= pc + 4096, "bufferram %ld kB is the page cache (%ld kB)", buf, pc, 0);
    long phys = sysconf(_SC_PHYS_PAGES) * sysconf(_SC_PAGESIZE) / 1024;
    check(phys == total, "sysconf(_SC_PHYS_PAGES) says the same: %ld kB", phys, 0, 0);
    long mt = meminfo("MemTotal:"), mf = meminfo("MemFree:"), ma = meminfo("MemAvailable:"), sh = meminfo("Shmem:"), ca = meminfo("Cached:");
    printf("info /proc/meminfo: MemTotal %ld, MemFree %ld, MemAvailable %ld, Shmem %ld, Cached %ld kB\n", mt, mf, ma, sh, ca);
    check(mt == total, "/proc/meminfo MemTotal %ld kB = totalram", mt, 0, 0);
    check(mf >= freek - 8192 && mf <= freek + 8192 && ma >= mf && sh >= 8 * 1024 && ca >= pc - 4096 && ca <= pc + 4096,
          "MemFree %ld, MemAvailable %ld, Shmem %ld kB agree with sysinfo", mf, ma, sh);
    if (mfd >= 0)
        close(mfd);
    return 0;
}

int main(int argc, char **argv)
{
    const char *mode = argc > 1 ? argv[1] : "";
    int r = 1;
    if (!strcmp(mode, "cowfork"))
        r = mode_cowfork();
    else if (!strcmp(mode, "ramnamed"))
        r = mode_ramnamed();
    else if (!strcmp(mode, "sysinfo"))
        r = mode_sysinfo();
    int ok = r == 0 && checks > 0 && passed == checks;
    printf("GMEM2 %s: %s %d/%d\n", mode, ok ? "PASS" : "FAILED", passed, checks);
    return ok ? 0 : 1;
}

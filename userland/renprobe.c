/* EuroOS — rename over an existing file, as a Linux program sees it (BUG-091).
 *
 * One source, two builds: freestanding (userland/renprobe.elf, built by
 * userland/build.sh; file mappings are copies there) and glibc dynamic
 * (userland/glibc/grenprobe: gcc -O2 -o glibc/grenprobe renprobe.c; with demand
 * paging a mapping of 1 MiB is filled page by page from the file). Used by the [rename] boot self-test
 * (kernel/src/renametest.rs) and, from the Terminal, for the same question
 * across a restart.
 *
 *   renprobe [dir]          the scenario below in dir (default /tmp/renprobe)
 *   renprobe save DIR VER   an atomic save of three documents in DIR: the first
 *                           call creates a.txt, b.txt and c.txt, a later one
 *                           replaces a.txt by renaming a new file over it,
 *                           replaces b.txt by unlink and rename, removes c.txt
 *   renprobe check DIR VER  a.txt and b.txt hold version VER, no temporary
 *                           file is left; c.txt is there for VER 1 only
 *
 * The scenario: sixteen small files and three large ones, descriptors held on
 * the later ones (each at its own offset), a shared mapping and a lazily
 * filled private mapping. Then
 *   A  a new file is renamed over an EARLIER file, twice (how a program saves
 *      a document); every file by path, every held descriptor and both
 *      mappings must still be their own file;
 *   B  the same over a file that is open: the descriptor keeps the old bytes,
 *      the path gives the new ones;
 *   C  six rounds of unlink + rename + rename (save with a backup), and an
 *      unlink of a file in the middle;
 *   D  a file that is mapped and whose descriptor is closed is replaced, and
 *      another one is unlinked: the mappings keep the bytes they were made of.
 *
 * Every line starts with "renprobe:". The last one is "renprobe: PASS" or
 * "renprobe: FAIL n". Exit code 0 or 1. */
#if __STDC_HOSTED__
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
#else
/* Freestanding build (userland/build.sh, no C library): the few calls this
 * program makes, straight on the Linux syscall ABI. */
#include <stddef.h>
static int errno;
static long sys6(long n, long a1, long a2, long a3, long a4, long a5, long a6) {
    long ret;
    register long r10 __asm__("r10") = a4;
    register long r8 __asm__("r8") = a5;
    register long r9 __asm__("r9") = a6;
    __asm__ volatile("syscall" : "=a"(ret) : "a"(n), "D"(a1), "S"(a2), "d"(a3), "r"(r10), "r"(r8), "r"(r9) : "rcx", "r11", "memory");
    return ret;
}
static long ret(long r) {
    if (r < 0 && r > -4096) {
        errno = (int)-r;
        return -1;
    }
    return r;
}
#define O_RDONLY 0
#define O_WRONLY 1
#define O_RDWR 2
#define O_CREAT 0100
#define O_TRUNC 01000
#define SEEK_SET 0
#define SEEK_CUR 1
#define PROT_READ 1
#define PROT_WRITE 2
#define MAP_SHARED 1
#define MAP_PRIVATE 2
#define MAP_FAILED ((void *)-1)
struct stat {
    unsigned long before_size[6];
    long st_size;
    unsigned long after_size[11];
};
static int open(const char *p, int flags, ...) { return (int)ret(sys6(2, (long)p, flags, 0644, 0, 0, 0)); }
static int close(int fd) { return (int)ret(sys6(3, fd, 0, 0, 0, 0, 0)); }
static long read(int fd, void *b, size_t n) { return ret(sys6(0, fd, (long)b, (long)n, 0, 0, 0)); }
static long write(int fd, const void *b, size_t n) { return ret(sys6(1, fd, (long)b, (long)n, 0, 0, 0)); }
static long pread(int fd, void *b, size_t n, long off) { return ret(sys6(17, fd, (long)b, (long)n, off, 0, 0)); }
static long pwrite(int fd, const void *b, size_t n, long off) { return ret(sys6(18, fd, (long)b, (long)n, off, 0, 0)); }
static long lseek(int fd, long off, int whence) { return ret(sys6(8, fd, off, whence, 0, 0, 0)); }
static int fstat(int fd, struct stat *st) { return (int)ret(sys6(5, fd, (long)st, 0, 0, 0, 0)); }
static void *mmap(void *a, size_t n, int prot, int flags, int fd, long off) { return (void *)ret(sys6(9, (long)a, (long)n, prot, flags, fd, off)); }
static int munmap(void *a, size_t n) { return (int)ret(sys6(11, (long)a, (long)n, 0, 0, 0, 0)); }
static int rename(const char *a, const char *b) { return (int)ret(sys6(82, (long)a, (long)b, 0, 0, 0, 0)); }
static int mkdir(const char *p, int mode) { return (int)ret(sys6(83, (long)p, mode, 0, 0, 0, 0)); }
static int unlink(const char *p) { return (int)ret(sys6(87, (long)p, 0, 0, 0, 0, 0)); }
void *memcpy(void *d, const void *s, size_t n) {
    for (size_t i = 0; i < n; i++) ((char *)d)[i] = ((const char *)s)[i];
    return d;
}
void *memset(void *d, int c, size_t n) {
    for (size_t i = 0; i < n; i++) ((char *)d)[i] = (char)c;
    return d;
}
int memcmp(const void *a, const void *b, size_t n) {
    for (size_t i = 0; i < n; i++) {
        if (((const unsigned char *)a)[i] != ((const unsigned char *)b)[i]) return 1;
    }
    return 0;
}
static int strcmp(const char *a, const char *b) {
    while (*a && *a == *b) a++, b++;
    return *a != *b;
}
int main(int argc, char **argv);
/* The kernel enters with rsp at argc (the SysV process stack). */
__asm__(".section .text.start\n"
        ".globl _start\n"
        "_start:\n"
        "  mov %rsp, %rdi\n"
        "  and $-16, %rsp\n"
        "  call real_start\n"
        ".text\n");
void real_start(unsigned long *sp) {
    int r = main((int)sp[0], (char **)(sp + 1));
    for (;;) sys6(60, r, 0, 0, 0, 0, 0);
}
#endif

#define NSMALL 16
#define FIRST_HELD 8
#define BIG (1024 * 1024 + 3 * 4096 + 100) /* past the 1 MiB that makes a mapping lazy */

static char line[400];
static int pos;
static int fails;
static const char *dir; /* set in main: the freestanding build has no relocations */
static unsigned char buf[65536];

static void s(const char *t) {
    while (*t && pos < 380) line[pos++] = *t++;
}
static void num(long v) {
    char b[24];
    int i = 0;
    if (v < 0) {
        if (pos < 380) line[pos++] = '-';
        v = -v;
    }
    do {
        b[i++] = (char)('0' + v % 10);
        v /= 10;
    } while (v && i < 23);
    while (i && pos < 380) line[pos++] = b[--i];
}
static void flush(void) {
    line[pos++] = '\n';
    int off = 0;
    while (off < pos) {
        long w = write(1, line + off, (size_t)(pos - off));
        if (w <= 0) break;
        off += (int)w;
    }
    pos = 0;
}
/* One verdict line per check: "renprobe: <step> <what> ok" or "... FAILED <why>". */
static void verdict(const char *step, const char *what, int bad, const char *why, long a, long b) {
    s("renprobe: ");
    s(step);
    s(" ");
    s(what);
    if (!bad) {
        s(" ok");
    } else {
        fails++;
        s(" FAILED ");
        s(why);
        s(" ");
        num(a);
        s(" ");
        num(b);
    }
    flush();
}

/* Byte i of file `id` in version `ver`. */
static unsigned char want(int id, int ver, long i) {
    return (unsigned char)(id * 131 + ver * 29 + i * 7 + (i >> 8) + (i >> 16) * 3);
}
static long small_size(int k) {
    return 3000 + k * 600; /* 3000 .. 12000: one to three pages */
}
static char pathbuf[4][200];
static const char *path_of(int slot, const char *name) {
    char *p = pathbuf[slot];
    int n = 0;
    for (const char *d = dir; *d && n < 150; d++) p[n++] = *d;
    p[n++] = '/';
    for (; *name && n < 198; name++) p[n++] = *name;
    p[n] = 0;
    return p;
}
static const char *small_name(int k) {
    static char nm[8];
    nm[0] = 'f';
    nm[1] = (char)('0' + k / 10);
    nm[2] = (char)('0' + k % 10);
    nm[3] = 0;
    return nm;
}

static int write_file(const char *path, int id, int ver, long size) {
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return -1;
    long done = 0;
    while (done < size) {
        long n = size - done < (long)sizeof buf ? size - done : (long)sizeof buf;
        for (long i = 0; i < n; i++) buf[i] = want(id, ver, done + i);
        long w = write(fd, buf, (size_t)n);
        if (w <= 0) {
            close(fd);
            return -1;
        }
        done += w;
    }
    close(fd);
    return 0;
}
/* 0 when the open descriptor reads exactly `size` bytes of (id, ver) from offset
 * 0 by pread; else 1 + the first offset that differs, or -1 for a wrong length. */
static long fd_differs(int fd, int id, int ver, long size) {
    long done = 0;
    for (;;) {
        long n = pread(fd, buf, sizeof buf, done);
        if (n < 0) return -1;
        if (n == 0) break;
        for (long i = 0; i < n; i++) {
            if (done + i >= size || buf[i] != want(id, ver, done + i)) return 1 + done + i;
        }
        done += n;
    }
    return done == size ? 0 : -1;
}
static long path_differs(const char *path, int id, int ver, long size) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -2;
    long r = fd_differs(fd, id, ver, size);
    close(fd);
    return r;
}
static long mem_differs(const unsigned char *p, int id, int ver, long from, long size) {
    for (long i = from; i < size; i++) {
        if (p[i] != want(id, ver, i)) return 1 + i;
    }
    return 0;
}
static int gone(const char *path) {
    int fd = open(path, O_RDONLY);
    if (fd >= 0) {
        close(fd);
        return 0;
    }
    return 1;
}

/* What the scenario believes: the version of every small file (-1: unlinked). */
static int ver_of[NSMALL];
static int held[NSMALL];

static void check_paths(const char *step) {
    int bad = 0;
    long first = 0, why = 0;
    for (int k = 0; k < NSMALL; k++) {
        const char *p = path_of(0, small_name(k));
        if (ver_of[k] < 0) {
            if (!gone(p) && !bad) {
                bad = 1;
                first = k;
                why = -3;
            }
            continue;
        }
        long r = path_differs(p, k, ver_of[k], small_size(k));
        if (r != 0 && !bad) {
            bad = 1;
            first = k;
            why = r;
        }
    }
    verdict(step, "every file by its path", bad, "file,where", first, why);
}
static void check_held(const char *step) {
    int bad = 0;
    long first = 0, why = 0;
    for (int k = FIRST_HELD; k < NSMALL; k++) {
        /* A held descriptor is the file it was opened on: version 0. */
        long r = fd_differs(held[k], k, 0, small_size(k));
        if (r == 0 && lseek(held[k], 0, SEEK_CUR) != k * 10) r = -4;
        struct stat st;
        if (r == 0 && (fstat(held[k], &st) != 0 || st.st_size != small_size(k))) r = -5;
        if (r != 0 && !bad) {
            bad = 1;
            first = k;
            why = r;
        }
    }
    verdict(step, "every held descriptor (bytes, offset, size)", bad, "file,where", first, why);
    /* A write through a held descriptor lands in that file and in no neighbour. */
    static const char mark[] = "MARK-BUG-091";
    unsigned char keep[sizeof mark];
    int k = FIRST_HELD + 1;
    long w = pread(held[k], keep, sizeof mark, 100);
    w = w == (long)sizeof mark ? pwrite(held[k], mark, sizeof mark, 100) : -1;
    int wbad = w != (long)sizeof mark;
    if (!wbad) {
        int fd = open(path_of(0, small_name(k)), O_RDONLY);
        unsigned char got[sizeof mark];
        wbad = fd < 0 || pread(fd, got, sizeof mark, 100) != (long)sizeof mark || memcmp(got, mark, sizeof mark) != 0;
        if (fd >= 0) close(fd);
        if (pwrite(held[k], keep, sizeof mark, 100) != (long)sizeof mark) wbad = 1;
    }
    verdict(step, "a write through a held descriptor lands in its own file", wbad, "file,ret", k, w);
}

static int save(const char *name, int id, int ver, long size) {
    char tmp[16];
    int n = 0;
    for (; name[n]; n++) tmp[n] = name[n];
    memcpy(tmp + n, ".tmp", 5);
    if (write_file(path_of(1, tmp), id, ver, size) != 0) return -1;
    return rename(path_of(1, tmp), path_of(2, name)) == 0 ? 0 : -2;
}

static int scenario(void) {
    mkdir(dir, 0755);
    int bad = 0;
    /* Creation order: f00..f07, big0, f08..f15, big1, big2. */
    for (int k = 0; k < NSMALL; k++) {
        if (k == FIRST_HELD) bad |= write_file(path_of(0, "big0"), 100, 0, BIG) != 0;
        bad |= write_file(path_of(0, small_name(k)), k, 0, small_size(k)) != 0;
        ver_of[k] = 0;
    }
    bad |= write_file(path_of(0, "big1"), 101, 0, BIG) != 0;
    bad |= write_file(path_of(0, "big2"), 102, 0, BIG) != 0;
    verdict("setup", "16 small files and 3 of 1 MiB written", bad, "errno", errno, 0);
    if (bad) return 1;

    for (int k = FIRST_HELD; k < NSMALL; k++) {
        held[k] = open(path_of(0, small_name(k)), O_RDWR);
        if (held[k] < 0 || lseek(held[k], k * 10, SEEK_SET) != k * 10) bad = 1;
    }
    /* A shared mapping of f12 (two pages) and a private one of big1 of which only
     * the first page is touched before the renames. */
    unsigned char *shared = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_SHARED, held[12], 0);
    int fd1 = open(path_of(0, "big1"), O_RDONLY);
    unsigned char *lazy = fd1 >= 0 ? mmap(NULL, BIG, PROT_READ, MAP_PRIVATE, fd1, 0) : MAP_FAILED;
    int have_shared = shared != MAP_FAILED, have_lazy = lazy != MAP_FAILED;
    if (have_lazy) bad |= mem_differs(lazy, 101, 0, 0, 4096) != 0;
    if (have_shared) bad |= mem_differs(shared, 12, 0, 0, 8192) != 0;
    s("renprobe: setup mappings shared=");
    num(have_shared);
    s(" big=");
    num(have_lazy);
    s(" big-bytes=");
    num(BIG);
    flush();
    verdict("setup", "descriptors held on f08..f15, mappings read their files", bad, "errno", errno, 0);
    check_paths("before");
    check_held("before");

    /* A: a new file renamed over an earlier one, twice. */
    static const int targets[3] = {3, 5, 3};
    for (int round = 0; round < 3; round++) {
        int k = targets[round];
        int r = save(small_name(k), k, ver_of[k] + 1, small_size(k));
        if (r == 0) ver_of[k]++;
        const char *step = round == 0 ? "A1" : round == 1 ? "A2" : "A3";
        verdict(step, "a new file renamed over an earlier file", r != 0, "ret,errno", r, errno);
        verdict(step, "the temporary name is gone", !gone(path_of(0, k == 3 ? "f03.tmp" : "f05.tmp")), "file", k, 0);
        check_paths(step);
        check_held(step);
        if (have_shared) verdict(step, "the shared mapping of f12", mem_differs(shared, 12, 0, 0, 8192) != 0, "where", mem_differs(shared, 12, 0, 0, 8192), 0);
        if (round == 0) {
            /* Pages of big1 that are filled only now, and a new mapping of a held descriptor. */
            if (have_lazy) verdict(step, "the mapping of big1, pages first touched after the rename", mem_differs(lazy, 101, 0, 4096, BIG) != 0, "where", mem_differs(lazy, 101, 0, 4096, BIG), 0);
            unsigned char *fresh = mmap(NULL, 4096, PROT_READ, MAP_PRIVATE, held[13], 0);
            if (fresh != MAP_FAILED) {
                verdict(step, "a new mapping through a held descriptor (f13)", mem_differs(fresh, 13, 0, 0, 4096) != 0, "where", mem_differs(fresh, 13, 0, 0, 4096), 0);
                munmap(fresh, 4096);
            }
        }
    }

    /* B: the destination is open while it is replaced. */
    {
        int old = open(path_of(0, "f06"), O_RDONLY);
        int r = save("f06", 6, 1, small_size(6) + 777);
        verdict("B", "a new file renamed over an OPEN file", old < 0 || r != 0, "ret,errno", r, errno);
        long d = fd_differs(old, 6, 0, small_size(6));
        verdict("B", "the open descriptor still reads the replaced file", d != 0, "where", d, 0);
        d = path_differs(path_of(0, "f06"), 6, 1, small_size(6) + 777);
        verdict("B", "the path gives the new file", d != 0, "where", d, 0);
        if (old >= 0) close(old);
        /* f06 now has another size than the model: check it apart, then put a
         * version back that the model knows. */
        r = save("f06", 6, 2, small_size(6));
        if (r == 0) ver_of[6] = 2;
        check_paths("B");
        check_held("B");
    }

    /* C: save with a backup, six rounds; then an unlink in the middle. */
    {
        int cbad = 0;
        long where = 0;
        for (int it = 1; it <= 6 && !cbad; it++) {
            if (write_file(path_of(1, "f02.new"), 2, it, small_size(2)) != 0) cbad = 1;
            unlink(path_of(1, "f02.bak"));
            if (rename(path_of(1, "f02"), path_of(2, "f02.bak")) != 0) cbad = 2;
            if (rename(path_of(1, "f02.new"), path_of(2, "f02")) != 0) cbad = 3;
            if (!cbad && path_differs(path_of(1, "f02"), 2, it, small_size(2)) != 0) cbad = 4;
            if (!cbad && path_differs(path_of(1, "f02.bak"), 2, it - 1, small_size(2)) != 0) cbad = 5;
            if (!cbad && !gone(path_of(1, "f02.new"))) cbad = 6;
            where = it;
            if (!cbad) ver_of[2] = it;
        }
        verdict("C", "six saves with a backup (unlink, rename, rename)", cbad, "step,round", cbad, where);
        int u = unlink(path_of(1, "f04"));
        if (u == 0) ver_of[4] = -1;
        u |= unlink(path_of(1, "f02.bak"));
        verdict("C", "unlink of f04 and of the backup", u != 0 || !gone(path_of(1, "f02.bak")), "ret,errno", u, errno);
        check_paths("C");
        check_held("C");
        if (have_shared) verdict("C", "the shared mapping of f12", mem_differs(shared, 12, 0, 0, 8192) != 0, "where", mem_differs(shared, 12, 0, 0, 8192), 0);
        if (have_lazy) verdict("C", "the mapping of big1, all of it", mem_differs(lazy, 101, 0, 0, BIG) != 0, "where", mem_differs(lazy, 101, 0, 0, BIG), 0);
    }

    /* D: mapped, descriptor closed, then replaced (big0) or unlinked (big2). */
    {
        int fd = open(path_of(0, "big0"), O_RDONLY);
        unsigned char *m0 = fd >= 0 ? mmap(NULL, BIG, PROT_READ, MAP_PRIVATE, fd, 0) : MAP_FAILED;
        if (fd >= 0) close(fd);
        fd = open(path_of(0, "big2"), O_RDONLY);
        unsigned char *m2 = fd >= 0 ? mmap(NULL, BIG, PROT_READ, MAP_PRIVATE, fd, 0) : MAP_FAILED;
        if (fd >= 0) close(fd);
        if (m0 == MAP_FAILED || m2 == MAP_FAILED) {
            s("renprobe: D not run: no mapping of 1 MiB here (errno ");
            num(errno);
            s(")");
            flush();
        } else {
            int first = mem_differs(m0, 100, 0, 0, 4096) != 0 || mem_differs(m2, 102, 0, 0, 4096) != 0;
            verdict("D", "big0 and big2 mapped, descriptors closed, first page read", first, "errno", errno, 0);
            int r = save("big0", 100, 1, 5000);
            verdict("D", "a new file renamed over the mapped big0", r != 0, "ret,errno", r, errno);
            long d = mem_differs(m0, 100, 0, 0, BIG);
            verdict("D", "the mapping of the replaced file keeps its bytes", d != 0, "where", d, 0);
            d = path_differs(path_of(0, "big0"), 100, 1, 5000);
            verdict("D", "the path big0 gives the new file", d != 0, "where", d, 0);
            r = unlink(path_of(0, "big2"));
            verdict("D", "unlink of the mapped big2", r != 0 || !gone(path_of(0, "big2")), "ret,errno", r, errno);
            d = mem_differs(m2, 102, 0, 0, BIG);
            verdict("D", "the mapping of the unlinked file keeps its bytes", d != 0, "where", d, 0);
        }
        check_paths("D");
        check_held("D");
    }

    /* Leave nothing behind. */
    for (int k = FIRST_HELD; k < NSMALL; k++) close(held[k]);
    if (fd1 >= 0) close(fd1);
    for (int k = 0; k < NSMALL; k++) unlink(path_of(0, small_name(k)));
    unlink(path_of(0, "big0"));
    unlink(path_of(0, "big1"));
    return 0;
}

/* The documents of `save` and `check`: three short text files. */
static int doc_write(const char *path, char which, int ver) {
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return -1;
    char t[64];
    int n = 0;
    const char *a = "document ";
    while (*a) t[n++] = *a++;
    t[n++] = which;
    a = " version ";
    while (*a) t[n++] = *a++;
    t[n++] = (char)('0' + ver % 10);
    t[n++] = '\n';
    long w = write(fd, t, (size_t)n);
    close(fd);
    return w == n ? 0 : -1;
}
static int doc_is(const char *path, char which, int ver) {
    char t[64];
    int fd = open(path, O_RDONLY);
    if (fd < 0) return 0;
    long n = read(fd, t, sizeof t);
    close(fd);
    return n == 21 && t[9] == which && t[20] == '\n' && t[19] == (char)('0' + ver % 10);
}
static int docs(const char *mode, int ver) {
    const char *a = path_of(0, "a.txt"), *b = path_of(1, "b.txt"), *c = path_of(2, "c.txt");
    if (mode[0] == 's') {
        mkdir(dir, 0755);
        if (gone(a)) {
            int r = doc_write(a, 'a', ver) | doc_write(b, 'b', ver) | doc_write(c, 'c', ver);
            verdict("save", "a.txt, b.txt and c.txt created", r != 0, "errno", errno, 0);
        } else {
            const char *t = path_of(3, "a.txt.tmp");
            int r = doc_write(t, 'a', ver);
            if (r == 0) r = rename(t, a);
            verdict("save", "a.txt replaced by a rename over it", r != 0, "ret,errno", r, errno);
            t = path_of(3, "b.txt.new");
            r = doc_write(t, 'b', ver);
            if (r == 0) r = unlink(b);
            if (r == 0) r = rename(t, b);
            verdict("save", "b.txt replaced by unlink and rename", r != 0, "ret,errno", r, errno);
            r = unlink(c);
            verdict("save", "c.txt removed", r != 0, "ret,errno", r, errno);
        }
    }
    verdict("check", "a.txt holds the version", !doc_is(a, 'a', ver), "version", ver, 0);
    verdict("check", "b.txt holds the version", !doc_is(b, 'b', ver), "version", ver, 0);
    if (ver == 1) verdict("check", "c.txt holds the version", !doc_is(c, 'c', ver), "version", ver, 0);
    else verdict("check", "c.txt is gone", !gone(c), "version", ver, 0);
    verdict("check", "no temporary file left", !gone(path_of(3, "a.txt.tmp")) || !gone(path_of(3, "b.txt.new")), "version", ver, 0);
    return 0;
}

int main(int argc, char **argv) {
    dir = "/tmp/renprobe";
    if (argc >= 4 && (!strcmp(argv[1], "save") || !strcmp(argv[1], "check"))) {
        dir = argv[2];
        docs(argv[1], argv[3][0] - '0');
    } else {
        if (argc >= 2) dir = argv[1];
        scenario();
    }
    if (fails) {
        s("renprobe: FAIL ");
        num(fails);
    } else {
        s("renprobe: PASS");
    }
    flush();
    return fails ? 1 : 0;
}

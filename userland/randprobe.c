/* EuroOS — what a Linux program sees of the kernel's randomness (BUG-080).
 *
 * One source, two builds: musl static-PIE (userland/randprobe.elf) and glibc
 * dynamic (userland/glibc/grandprobe). Used by the [linux-rand] boot self-test
 * (kernel/src/linuxrand.rs) and by scripts/rand-boots.py, which compares the
 * lines of several boots.
 *
 *   randprobe             AT_RANDOM, the stack guard, getrandom with each flag,
 *                         /dev/urandom, /dev/random, pread, short-read rules
 *   randprobe stat [KiB]  the same, then bit balance, byte chi-square and a
 *                         repeated-block search over KiB (default 1024) from
 *                         getrandom and as much from /dev/urandom
 *   randprobe closed      for a pool the kernel reports closed: GRND_NONBLOCK
 *                         gives EAGAIN and leaves the buffer alone, a call
 *                         without flags waits and then returns bytes
 *
 * Every line starts with "randprobe:". The last one is "randprobe: PASS" or
 * "randprobe: FAIL n". Exit code 0 or 1. */
#include <errno.h>
#include <fcntl.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/syscall.h>
#include <unistd.h>

#define GRND_NONBLOCK 1u
#define GRND_RANDOM 2u
#define GRND_INSECURE 4u

static char line[400];
static int pos;
static int fails;

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
static void hex(const unsigned char *p, int n) {
    static const char d[] = "0123456789abcdef";
    for (int i = 0; i < n && pos < 378; i++) {
        line[pos++] = d[p[i] >> 4];
        line[pos++] = d[p[i] & 15];
    }
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
static void check(int ok, const char *what) {
    if (!ok) {
        fails++;
        s("randprobe: CHECK FAILED: ");
        s(what);
        flush();
    }
}

static long grnd(void *buf, unsigned long len, unsigned flags) {
    return syscall(SYS_getrandom, buf, len, flags);
}
static int all_equal(const unsigned char *p, int n, unsigned char v) {
    for (int i = 0; i < n; i++)
        if (p[i] != v) return 0;
    return 1;
}
/* Read exactly n bytes; the number read, or -errno of the first failure. */
static long read_full(int fd, unsigned char *p, long n) {
    long got = 0;
    while (got < n) {
        long r = read(fd, p + got, (size_t)(n - got));
        if (r < 0) return -errno;
        if (r == 0) break;
        got += r;
    }
    return got;
}

/* ── statistics over a stream ─────────────────────────────────────────────── */
static unsigned long long st_n, st_ones, st_hist[256], st_bit[8];
static unsigned long long st_window[8192]; /* last block seen per 13-bit key */
static unsigned char st_used[8192];
static unsigned char st_first[16], st_prev[16];
static unsigned long st_blocks, st_repeats;

static void st_reset(void) {
    st_n = st_ones = 0;
    memset(st_hist, 0, sizeof st_hist);
    memset(st_bit, 0, sizeof st_bit);
    memset(st_used, 0, sizeof st_used);
    st_blocks = st_repeats = 0;
}
static void st_feed(const unsigned char *p, long n) {
    for (long i = 0; i < n; i++) {
        unsigned char c = p[i];
        st_hist[c]++;
        for (int b = 0; b < 8; b++)
            if (c & (1 << b)) {
                st_bit[b]++;
                st_ones++;
            }
    }
    st_n += (unsigned long long)n;
    /* 16-byte blocks: a repeat of the block before, of the first block of the
     * stream, or of the last block with the same 13-bit key. Random data has
     * none; a stream with a period up to 128 KiB has thousands. */
    for (long i = 0; i + 16 <= n; i += 16) {
        const unsigned char *b = p + i;
        unsigned long long lo, hi;
        int counted = 0;
        memcpy(&lo, b, 8);
        memcpy(&hi, b + 8, 8);
        if (st_blocks == 0) {
            memcpy(st_first, b, 16);
        } else if (!memcmp(b, st_prev, 16) || !memcmp(b, st_first, 16)) {
            st_repeats++;
            counted = 1;
        }
        unsigned k = (unsigned)(hi & 8191);
        if (!counted && st_used[k] && st_window[k] == lo) st_repeats++;
        st_window[k] = lo;
        st_used[k] = 1;
        memcpy(st_prev, b, 16);
        st_blocks++;
    }
}
static void st_report(const char *name) {
    /* bit balance: |ones - n/2| against 5 standard deviations (sqrt(bits)/2) */
    unsigned long long bits = st_n * 8, half = bits / 2;
    unsigned long long dev = st_ones > half ? st_ones - half : half - st_ones;
    unsigned long long sd = 1;
    while (sd * sd < bits) sd++; /* sqrt(bits), rounded up */
    unsigned long long limit = sd * 5 / 2;
    /* each bit position on its own, same bound for its n */
    unsigned long long worst = 0, sdp = 1;
    while (sdp * sdp < st_n) sdp++;
    for (int b = 0; b < 8; b++) {
        unsigned long long h = st_n / 2, d = st_bit[b] > h ? st_bit[b] - h : h - st_bit[b];
        if (d > worst) worst = d;
    }
    /* chi-square of the byte histogram, 255 degrees of freedom: mean 255,
     * standard deviation 22.6; accepted 140 to 370 (about 5 sd each way) */
    unsigned long long e = st_n / 256, sq = 0;
    for (int i = 0; i < 256; i++) {
        unsigned long long d = st_hist[i] > e ? st_hist[i] - e : e - st_hist[i];
        sq += d * d;
    }
    unsigned long long chi = e ? sq / e : 0;
    int ok_bal = dev <= limit, ok_pos = worst <= sdp * 5 / 2, ok_chi = chi >= 140 && chi <= 370, ok_rep = st_repeats == 0;
    s("randprobe: stat ");
    s(name);
    s(" bytes=");
    num((long)st_n);
    s(" ones=");
    num((long)st_ones);
    s(" of ");
    num((long)bits);
    s(" (off by ");
    num((long)dev);
    s(", limit ");
    num((long)limit);
    s(") worst-bit-position-off=");
    num((long)worst);
    s(" (limit ");
    num((long)(sdp * 5 / 2));
    s(") chi2=");
    num((long)chi);
    s(" (140..370) blocks16=");
    num((long)st_blocks);
    s(" repeated=");
    num((long)st_repeats);
    s(ok_bal && ok_pos && ok_chi && ok_rep ? " ok" : " BAD");
    flush();
    check(ok_bal, "bit balance");
    check(ok_pos, "bit position balance");
    check(ok_chi, "byte chi-square");
    check(ok_rep, "repeated 16-byte blocks");
}

static unsigned char big[32768];

static void stat_mode(long kib) {
    long total = kib * 1024;
    st_reset();
    long done = 0;
    int short_calls = 0;
    while (done < total) {
        long want = total - done < (long)sizeof big ? total - done : (long)sizeof big;
        long r = grnd(big, (unsigned long)want, 0);
        if (r <= 0) {
            check(0, "getrandom failed in the stream");
            break;
        }
        if (r != want) short_calls++;
        st_feed(big, r);
        done += r;
    }
    s("randprobe: stream getrandom short-calls-of-32768=");
    num(short_calls);
    flush();
    st_report("getrandom");

    int fd = open("/dev/urandom", O_RDONLY);
    check(fd >= 0, "open /dev/urandom for the stream");
    if (fd >= 0) {
        st_reset();
        done = 0;
        while (done < total) {
            long want = total - done < (long)sizeof big ? total - done : (long)sizeof big;
            long r = read_full(fd, big, want);
            if (r <= 0) {
                check(0, "/dev/urandom read failed in the stream");
                break;
            }
            st_feed(big, r);
            done += r;
        }
        close(fd);
        st_report("urandom");
    }
}

static void closed_mode(void) {
    unsigned char b[32];
    memset(b, 0xA7, sizeof b);
    errno = 0;
    long r = grnd(b, sizeof b, GRND_NONBLOCK);
    int e = errno;
    int untouched = all_equal(b, sizeof b, 0xA7);
    s("randprobe: closed nonblock ret=");
    num(r);
    s(" errno=");
    num(e);
    s(e == EAGAIN ? " (EAGAIN)" : "");
    s(" buffer-untouched=");
    num(untouched);
    flush();
    check(r == -1 && e == EAGAIN, "GRND_NONBLOCK on a closed pool is EAGAIN");
    check(untouched, "a refused request wrote nothing");
    memset(b, 0xA7, sizeof b);
    r = grnd(b, sizeof b, 0);
    s("randprobe: closed blocking ret=");
    num(r);
    s(" bytes=");
    hex(b, 32);
    flush();
    check(r == 32 && !all_equal(b, sizeof b, 0xA7), "a call without flags waited and returned bytes");
}

int main(int argc, char **argv) {
    unsigned char a[32], b[32];
    const char *mode = argc > 1 ? argv[1] : "";

    s("randprobe: start pid=");
    num((long)getpid());
    s(" mode=");
    s(*mode ? mode : "quick");
    flush();
    if (!strcmp(mode, "closed")) {
        closed_mode();
        goto done;
    }

    /* AT_RANDOM: 16 bytes the kernel puts on the stack of every new image. */
    const unsigned char *ar = (const unsigned char *)getauxval(AT_RANDOM);
    s("randprobe: at_random=");
    if (ar) hex(ar, 16);
    else s("(none)");
    flush();
    check(ar != 0, "AT_RANDOM present");
    /* What the libc made of it: the stack guard (and glibc's pointer guard). */
    unsigned long guard, pguard;
    __asm__("mov %%fs:0x28, %0" : "=r"(guard));
    __asm__("mov %%fs:0x30, %0" : "=r"(pguard));
    s("randprobe: stack_guard=");
    hex((const unsigned char *)&guard, 8);
    s(" fs30=");
    hex((const unsigned char *)&pguard, 8);
    flush();

    /* getrandom, each flag. */
    memset(a, 0, sizeof a);
    long r = grnd(a, 32, 0);
    s("randprobe: getrandom=");
    hex(a, 32);
    s(" ret=");
    num(r);
    flush();
    check(r == 32, "getrandom(32, 0) returns 32");
    check(!all_equal(a, 32, 0), "getrandom wrote bytes");
    memset(b, 0, sizeof b);
    r = grnd(b, 32, 0);
    s("randprobe: getrandom2=");
    hex(b, 32);
    flush();
    check(r == 32 && memcmp(a, b, 32), "two getrandom calls differ");

    memset(b, 0, sizeof b);
    r = grnd(b, 32, GRND_NONBLOCK);
    s("randprobe: getrandom_nonblock=");
    hex(b, 32);
    s(" ret=");
    num(r);
    flush();
    check(r == 32 && !all_equal(b, 32, 0), "GRND_NONBLOCK on a seeded pool returns bytes");
    memset(b, 0, sizeof b);
    r = grnd(b, 32, GRND_RANDOM);
    s("randprobe: getrandom_random=");
    hex(b, 32);
    s(" ret=");
    num(r);
    flush();
    check(r == 32 && !all_equal(b, 32, 0), "GRND_RANDOM accepted");
    memset(b, 0, sizeof b);
    r = grnd(b, 32, GRND_INSECURE);
    s("randprobe: getrandom_insecure=");
    hex(b, 32);
    s(" ret=");
    num(r);
    flush();
    check(r == 32 && !all_equal(b, 32, 0), "GRND_INSECURE accepted");
    errno = 0;
    long r1 = grnd(b, 32, 0x100);
    int e1 = errno;
    errno = 0;
    long r2 = grnd(b, 32, GRND_RANDOM | GRND_INSECURE);
    int e2 = errno;
    errno = 0;
    long r3 = grnd((void *)8, 32, 0);
    int e3 = errno;
    s("randprobe: getrandom_errors unknown-flag=");
    num(r1);
    s("/");
    num(e1);
    s(" random+insecure=");
    num(r2);
    s("/");
    num(e2);
    s(" bad-pointer=");
    num(r3);
    s("/");
    num(e3);
    flush();
    check(r1 == -1 && e1 == EINVAL, "an unknown flag is EINVAL");
    check(r2 == -1 && e2 == EINVAL, "GRND_RANDOM|GRND_INSECURE is EINVAL");
    check(r3 == -1 && e3 == EFAULT, "a bad pointer is EFAULT");
    r = grnd(b, 0, 0);
    check(r == 0, "getrandom(0) returns 0");

    /* 256 bytes or fewer are never short. */
    int shorts = 0;
    for (int i = 0; i < 16; i++)
        if (grnd(big, 256, 0) != 256) shorts++;
    for (int n = 1; n <= 255; n += 37)
        if (grnd(big, (unsigned long)n, 0) != n) shorts++;
    s("randprobe: short_reads_le_256=");
    num(shorts);
    flush();
    check(shorts == 0, "requests of 256 bytes or fewer are never short");

    /* The two character devices. */
    static const char *const dev[2] = { "/dev/urandom", "/dev/random" };
    static const char *const key[2] = { "urandom", "random" };
    for (int d = 0; d < 2; d++) {
        int fd = open(dev[d], O_RDONLY);
        memset(a, 0, sizeof a);
        long n = fd >= 0 ? read_full(fd, a, 32) : -errno;
        s("randprobe: ");
        s(key[d]);
        s("=");
        hex(a, 32);
        s(" ret=");
        num(n);
        flush();
        check(fd >= 0 && n == 32 && !all_equal(a, 32, 0), dev[d]);
        if (fd >= 0) {
            /* a character device never ends: read past any "file size" */
            long m = read_full(fd, big, 8192);
            check(m == 8192, "8192 bytes in one go from the device");
            memset(a, 0, sizeof a);
            memset(b, 0, sizeof b);
            long p1 = pread(fd, a, 32, 0), p2 = pread(fd, b, 32, 0);
            s("randprobe: ");
            s(key[d]);
            s("_pread0=");
            hex(a, 32);
            s(" again=");
            hex(b, 32);
            flush();
            check(p1 == 32 && p2 == 32 && memcmp(a, b, 32), "two preads at offset 0 differ");
            close(fd);
        }
    }

    if (!strcmp(mode, "stat")) {
        long kib = 1024;
        if (argc > 2) {
            kib = 0;
            for (const char *p = argv[2]; *p >= '0' && *p <= '9'; p++) kib = kib * 10 + (*p - '0');
            if (kib <= 0) kib = 1024;
        }
        stat_mode(kib);
    }
done:
    if (fails) {
        s("randprobe: FAIL ");
        num(fails);
    } else {
        s("randprobe: PASS");
    }
    flush();
    return fails ? 1 : 0;
}

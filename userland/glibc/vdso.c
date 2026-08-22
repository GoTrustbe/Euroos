/* EuroOS minimal vDSO — clock reads without a syscall.
 *
 * WHY: the syscall histogram of a chrome run put clock_gettime at 67% of ALL
 * syscalls (123k of 182k): without a vDSO every TimeTicks::Now() is a full
 * syscall+emulation round trip, and chrome asks for the time after nearly every
 * task. These functions read a kernel-updated data page instead.
 *
 * The DATA PAGE is the page of __vdso_data (pinned at +0x1000 by vdso.lds); the
 * kernel maps a single shared frame there and refreshes it from the timer tick:
 *   [0] seq (odd while the kernel is mid-update)
 *   [1] mono_sec  [2] mono_nsec   (CLOCK_MONOTONIC family)
 *   [3] real_sec  [4] real_nsec   (CLOCK_REALTIME family)
 * Readers retry on a torn/odd sequence, exactly like the Linux vDSO does.
 */
typedef long time_t;
struct timespec { time_t tv_sec; long tv_nsec; };
struct timeval  { time_t tv_sec; long tv_usec; };

/* STATIC on purpose: an exported data symbol is addressed through the GOT, and a
 * vDSO is never relocated by ld.so -- the GOT entry stays 0 and the first clock
 * read dereferences null (measured: rip=vdso+0x1053, addr=0). A static symbol is
 * addressed rip-relative, which needs no relocation at all. The kernel finds the
 * page by its fixed alignment, not by name. */
static __attribute__((aligned(4096)))
volatile unsigned long __vdso_data[512];

static int read_clock(int clk, unsigned long *sec, unsigned long *nsec)
{
    unsigned long s1;
    int real;
    switch (clk) {
    case 0: case 5: case 11: real = 1; break;      /* REALTIME / _COARSE / TAI */
    case 1: case 4: case 6: case 7: case 9: real = 0; break; /* MONOTONIC family / BOOTTIME */
    default: return -38;                            /* -ENOSYS: glibc falls back to the syscall */
    }
    do {
        s1 = __vdso_data[0];
        *sec  = __vdso_data[real ? 3 : 1];
        *nsec = __vdso_data[real ? 4 : 2];
    } while (__vdso_data[0] != s1 || (s1 & 1));
    return 0;
}

__attribute__((visibility("default")))
int __vdso_clock_gettime(int clk, struct timespec *ts)
{
    unsigned long sec, nsec;
    int r = read_clock(clk, &sec, &nsec);
    if (r) return r;
    ts->tv_sec = (time_t)sec;
    ts->tv_nsec = (long)nsec;
    return 0;
}

__attribute__((visibility("default")))
int __vdso_gettimeofday(struct timeval *tv, void *tz)
{
    (void)tz;
    if (tv) {
        unsigned long sec, nsec;
        read_clock(0, &sec, &nsec);
        tv->tv_sec = (time_t)sec;
        tv->tv_usec = (long)(nsec / 1000);
    }
    return 0;
}

__attribute__((visibility("default")))
time_t __vdso_time(time_t *t)
{
    unsigned long sec, nsec;
    read_clock(0, &sec, &nsec);
    if (t) *t = (time_t)sec;
    return (time_t)sec;
}

__attribute__((visibility("default")))
int __vdso_clock_getres(int clk, struct timespec *ts)
{
    (void)clk;
    if (ts) { ts->tv_sec = 0; ts->tv_nsec = 10000000; } /* 10 ms — the tick */
    return 0;
}

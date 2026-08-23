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

/* The data page lives OUTSIDE the image, one page past its end — the vvar idea.
 * __ehdr_start is the linker's own "image base" symbol; hidden visibility forces a
 * PC-relative reference, so this needs no relocation (an exported data symbol went
 * through the GOT, which nobody relocates in a vDSO — that was a null deref), and
 * the image itself can stay a single R+X load exactly like the real Linux vDSO. */
extern char __ehdr_start[] __attribute__((visibility("hidden")));
#define __vdso_data ((volatile unsigned long *)(__ehdr_start + 4096))

/* Data page layout (all unsigned long):
 *   [0] seq (odd while the kernel updates)
 *   [1] mono_ns at the anchor    [3] real_ns at the anchor
 *   [5] tsc at the anchor        [6] ns per tsc, <<20 fixed point (kernel-calibrated)
 * now = anchor + (rdtsc - anchor_tsc) scaled -- REAL sub-tick time, the way the
 * Linux vDSO does it. A clock that is flat for 10 ms between ticks broke chrome's
 * delay-until-deadline math; rdtsc interpolation gives monotonic microseconds. */
static inline unsigned long rdtsc_(void)
{
    unsigned int lo, hi;
    __asm__ __volatile__("rdtsc" : "=a"(lo), "=d"(hi));
    return ((unsigned long)hi << 32) | lo;
}

static int read_clock(int clk, unsigned long *sec, unsigned long *nsec)
{
    unsigned long s1, base_ns, tsc0, per10ms, dt_ns, ns;
    int real;
    switch (clk) {
    case 0: case 5: case 11: real = 1; break;      /* REALTIME / _COARSE / TAI */
    case 1: case 4: case 6: case 7: case 9: real = 0; break; /* MONOTONIC family / BOOTTIME */
    default: return -38;                            /* -ENOSYS: glibc falls back to the syscall */
    }
    do {
        s1 = __vdso_data[0];
        base_ns = __vdso_data[real ? 3 : 1];
        tsc0    = __vdso_data[5];
        per10ms = __vdso_data[6];
    } while (__vdso_data[0] != s1 || (s1 & 1));
    dt_ns = 0;
    if (per10ms) { /* [6]: ns-per-tsc in <<20 fixed point — mul+shift only, no
                      128-bit division (that pulls __udivti3 into a vDSO that must
                      not have relocations or libcalls) */
        unsigned long dt = rdtsc_() - tsc0;
        if (dt < (unsigned long)1 << 40) /* sane window; a stale anchor stays flat */
            dt_ns = (unsigned long)(((unsigned __int128)dt * per10ms) >> 20);
        if (dt_ns > 20000000ul)
            dt_ns = 20000000ul; /* cap at 2 ticks: never run ahead of the kernel clock */
    }
    ns = base_ns + dt_ns;
    *sec = ns / 1000000000ul;
    *nsec = ns % 1000000000ul;
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
    /* 1 ns, NOT the 10 ms tick. The kernel's syscall clock_getres already learned
     * this the hard way (its comment says so): chrome sizes its timers off the
     * REPORTED resolution during time-subsystem init, and a coarse answer makes
     * TimeTicks low-resolution and degrades frame scheduling. The vDSO shadowing
     * that syscall with 10 ms silently undid the fix — one present, then no frame
     * loop, ever. The rdtsc interpolation makes fine resolution honest anyway. */
    if (ts) { ts->tv_sec = 0; ts->tv_nsec = 1; }
    return 0;
}

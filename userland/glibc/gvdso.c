/* gvdso: does the process see AT_SYSINFO_EHDR, and does glibc's clock_gettime
 * actually go through the vDSO? Prints the auxv value, the vDSO's own ELF magic,
 * a direct call through the vDSO symbol (resolved by hand from its dynsym), and
 * the syscall-vs-vdso timing ratio glibc achieves. */
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <sys/time.h>
#include <sys/auxv.h>
#include <sys/syscall.h>
#include <unistd.h>
#include <pthread.h>
#include <elf.h>

typedef int (*cg_t)(clockid_t, struct timespec *);

static cg_t find_vdso_clock_gettime(unsigned long base)
{
    Elf64_Ehdr *eh = (Elf64_Ehdr *)base;
    Elf64_Phdr *ph = (Elf64_Phdr *)(base + eh->e_phoff);
    Elf64_Dyn *dyn = 0;
    for (int i = 0; i < eh->e_phnum; i++)
        if (ph[i].p_type == PT_DYNAMIC)
            dyn = (Elf64_Dyn *)(base + ph[i].p_vaddr);
    if (!dyn) { printf("gvdso: no PT_DYNAMIC\n"); return 0; }
    const char *strtab = 0; Elf64_Sym *symtab = 0; unsigned *hash = 0;
    for (Elf64_Dyn *d = dyn; d->d_tag != DT_NULL; d++) {
        if (d->d_tag == DT_STRTAB) strtab = (const char *)(base + d->d_un.d_ptr);
        if (d->d_tag == DT_SYMTAB) symtab = (Elf64_Sym *)(base + d->d_un.d_ptr);
        if (d->d_tag == DT_HASH)   hash = (unsigned *)(base + d->d_un.d_ptr);
    }
    if (!strtab || !symtab || !hash) { printf("gvdso: missing dynsym tables (str=%p sym=%p hash=%p)\n", (void*)strtab, (void*)symtab, (void*)hash); return 0; }
    unsigned nsym = hash[1];
    for (unsigned i = 0; i < nsym; i++)
        if (!strcmp(strtab + symtab[i].st_name, "__vdso_clock_gettime"))
            return (cg_t)(base + symtab[i].st_value);
    printf("gvdso: __vdso_clock_gettime not in dynsym (%u syms)\n", nsym);
    return 0;
}

int main(void)
{
    unsigned long v = getauxval(AT_SYSINFO_EHDR);
    printf("gvdso: AT_SYSINFO_EHDR = %#lx\n", v);
    if (v) {
        printf("gvdso: magic = %.4s\n", (const char *)v);
        cg_t cg = find_vdso_clock_gettime(v);
        if (cg) {
            struct timespec ts = {0, 0};
            int r = cg(CLOCK_MONOTONIC, &ts);
            printf("gvdso: direct __vdso_clock_gettime -> r=%d mono=%ld.%09ld\n", r, ts.tv_sec, ts.tv_nsec);
            r = cg(CLOCK_REALTIME, &ts);
            printf("gvdso: direct realtime -> r=%d %ld.%09ld\n", r, ts.tv_sec, ts.tv_nsec);
        }
    }
    /* Does GLIBC route through it? 200k calls: through the vDSO this is user-space
     * only; through the syscall it is 200k kernel round trips and visibly slower. */
    struct timespec a, b, t;
    clock_gettime(CLOCK_MONOTONIC, &a);
    for (int i = 0; i < 200000; i++)
        clock_gettime(CLOCK_MONOTONIC, &t);
    clock_gettime(CLOCK_MONOTONIC, &b);
    long ms = (b.tv_sec - a.tv_sec) * 1000 + (b.tv_nsec - a.tv_nsec) / 1000000;
    printf("gvdso: 200000 glibc clock_gettime calls took %ld ms\n", ms);
    /* Same loop through gettimeofday: glibc resolves that via a DIFFERENT vdso
     * symbol (__vdso_gettimeofday). Fast gettimeofday + slow clock_gettime = a
     * symbol-level problem; both slow = the whole image was rejected. */
    struct timeval tv;
    clock_gettime(CLOCK_MONOTONIC, &a);
    for (int i = 0; i < 200000; i++)
        gettimeofday(&tv, 0);
    clock_gettime(CLOCK_MONOTONIC, &b);
    ms = (b.tv_sec - a.tv_sec) * 1000 + (b.tv_nsec - a.tv_nsec) / 1000000;
    printf("gvdso: 200000 glibc gettimeofday calls took %ld ms\n", ms);
    /* Sub-tick progress: two back-to-back reads must differ by MORE than zero and
     * LESS than a full 10 ms tick — proof the rdtsc interpolation works. */
    struct timespec u, w;
    clock_gettime(CLOCK_MONOTONIC, &u);
    for (volatile int i = 0; i < 50000; i++) ;
    clock_gettime(CLOCK_MONOTONIC, &w);
    long dns = (w.tv_sec - u.tv_sec) * 1000000000L + (w.tv_nsec - u.tv_nsec);
    printf("gvdso: sub-tick delta over a short spin = %ld ns (%s)\n", dns,
           dns > 0 && dns < 10000000 ? "INTERPOLATING" : dns == 0 ? "FLAT (no sub-tick)" : "coarse");
    /* vDSO vs raw syscall, same instant: any offset or scale bug shows here. */
    for (int i = 0; i < 3; i++) {
        struct timespec sv, sy;
        clock_gettime(CLOCK_MONOTONIC, &sv);            /* glibc -> vDSO */
        syscall(228 /*SYS_clock_gettime*/, 1, &sy);     /* forced kernel path */
        long d = (sv.tv_sec - sy.tv_sec) * 1000000000L + (sv.tv_nsec - sy.tv_nsec);
        printf("gvdso: mono vdso=%ld.%09ld syscall=%ld.%09ld delta=%ld ns\n",
               sv.tv_sec, sv.tv_nsec, sy.tv_sec, sy.tv_nsec, d);
        for (volatile int j = 0; j < 200000; j++) ;
    }
    /* The primitive chrome's frame loop lives on: a timed condvar wait. If the
     * vDSO's presence breaks glibc's absolute-deadline math, a 50 ms wait here
     * takes seconds (or forever) and the whole browser mystery reproduces in a
     * five-second probe. Measured with BOTH clock attrs. */
    {
        pthread_mutex_t m = PTHREAD_MUTEX_INITIALIZER;
        pthread_cond_t c1; pthread_condattr_t at;
        pthread_condattr_init(&at);
        pthread_condattr_setclock(&at, CLOCK_MONOTONIC);
        pthread_cond_init(&c1, &at);
        struct timespec dl, t0, t1;
        clock_gettime(CLOCK_MONOTONIC, &t0);
        dl = t0; dl.tv_nsec += 50000000; if (dl.tv_nsec >= 1000000000) { dl.tv_sec++; dl.tv_nsec -= 1000000000; }
        pthread_mutex_lock(&m);
        int r = pthread_cond_timedwait(&c1, &m, &dl);
        pthread_mutex_unlock(&m);
        clock_gettime(CLOCK_MONOTONIC, &t1);
        long ms = (t1.tv_sec - t0.tv_sec) * 1000 + (t1.tv_nsec - t0.tv_nsec) / 1000000;
        printf("gvdso: cond_timedwait(MONO, 50ms) -> r=%d after %ld ms (%s)\n", r, ms,
               ms < 200 ? "ok" : "BROKEN");
        pthread_cond_t c2 = PTHREAD_COND_INITIALIZER; /* default clock = REALTIME */
        struct timespec rt;
        clock_gettime(CLOCK_REALTIME, &rt);
        rt.tv_nsec += 50000000; if (rt.tv_nsec >= 1000000000) { rt.tv_sec++; rt.tv_nsec -= 1000000000; }
        clock_gettime(CLOCK_MONOTONIC, &t0);
        pthread_mutex_lock(&m);
        r = pthread_cond_timedwait(&c2, &m, &rt);
        pthread_mutex_unlock(&m);
        clock_gettime(CLOCK_MONOTONIC, &t1);
        ms = (t1.tv_sec - t0.tv_sec) * 1000 + (t1.tv_nsec - t0.tv_nsec) / 1000000;
        printf("gvdso: cond_timedwait(REAL, 50ms) -> r=%d after %ld ms (%s)\n", r, ms,
               ms < 200 ? "ok" : "BROKEN");
        /* And a plain 30 ms usleep for the nanosleep path. */
        clock_gettime(CLOCK_MONOTONIC, &t0);
        usleep(30000);
        clock_gettime(CLOCK_MONOTONIC, &t1);
        ms = (t1.tv_sec - t0.tv_sec) * 1000 + (t1.tv_nsec - t0.tv_nsec) / 1000000;
        printf("gvdso: usleep(30ms) took %ld ms (%s)\n", ms, ms < 200 ? "ok" : "BROKEN");
    }
    printf("gvdso: OK\n");
    return 0;
}

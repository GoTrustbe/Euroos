/* gvdso: does the process see AT_SYSINFO_EHDR, and does glibc's clock_gettime
 * actually go through the vDSO? Prints the auxv value, the vDSO's own ELF magic,
 * a direct call through the vDSO symbol (resolved by hand from its dynsym), and
 * the syscall-vs-vdso timing ratio glibc achieves. */
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <sys/auxv.h>
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
    printf("gvdso: 200000 glibc clock_gettime calls took %ld ms (vdso => ~0-30ms; syscalls => seconds)\n", ms);
    printf("gvdso: OK\n");
    return 0;
}

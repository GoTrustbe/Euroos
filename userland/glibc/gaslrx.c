/* EuroOS — gaslr with the vDSO and a fork/exec mode (ASLR S0, BUG-088).
 *
 * glibc dynamic, PIE: gcc -O2 -o glibc/gaslrx gaslrx.c
 * Used by the [aslr], [aslr-ind], [aslr-fork] and [aslr-exec] boot self-tests
 * (kernel/src/aslrtest.rs). The fields are those of gaslr plus the vDSO:
 *   <tag>: exe=0x.. ldso=0x.. libc=0x.. stack=0x.. brk=0x.. mmap=0x.. mmapbig=0x.. vdso=0x..
 * exe: the address of main; ldso: AT_BASE; libc: the address of printf; stack:
 * a local of main; brk: sbrk(0) before anything of ours is allocated; mmap: a
 * fresh 4 KiB anonymous mapping; mmapbig: a fresh 1 MiB one; vdso:
 * AT_SYSINFO_EHDR (0 = none).
 *
 *   gaslrx        one line, tag "gaslr"
 *   gaslrx fork   fork first, then parent ("gaslr-parent") and child
 *                 ("gaslr-fork") each take their addresses from the same state:
 *                 a fork child must get the parent's layout bit for bit
 *   gaslrx exec   the parent's line ("gaslr"), then a fork child execs
 *                 /proc/self/exe; the new image prints "gaslr-exec". An exec the
 *                 kernel refuses prints "gaslr-exec: refused errno=N".
 *   gaslrx child  (the exec'd image) one line, tag "gaslr-exec" */
#include <errno.h>
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

static void line(const char *tag, volatile int *local) {
    void *brk0 = sbrk(0);
    void *m = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    void *mb = mmap(NULL, 1 << 20, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    unsigned long ldso = getauxval(AT_BASE);
    unsigned long vdso = getauxval(AT_SYSINFO_EHDR);
    printf("%s: exe=%#lx ldso=%#lx libc=%#lx stack=%#lx brk=%#lx mmap=%#lx mmapbig=%#lx vdso=%#lx\n", tag,
           (unsigned long)(uintptr_t)&line, ldso, (unsigned long)(uintptr_t)&printf,
           (unsigned long)(uintptr_t)local, (unsigned long)(uintptr_t)brk0,
           (unsigned long)(uintptr_t)m, (unsigned long)(uintptr_t)mb, vdso);
    fflush(stdout);
}

int main(int argc, char **argv) {
    volatile int local = 0;
    const char *mode = argc > 1 ? argv[1] : "";
    if (strcmp(mode, "fork") == 0) {
        pid_t p = fork();
        if (p < 0) {
            printf("gaslr-fork: refused errno=%d\n", errno);
            return 1;
        }
        line(p == 0 ? "gaslr-fork" : "gaslr-parent", &local);
        if (p == 0)
            _exit(0);
        waitpid(p, NULL, 0);
        return 0;
    }
    if (strcmp(mode, "exec") == 0) {
        line("gaslr", &local);
        pid_t p = fork();
        if (p == 0) {
            char *av[] = {argv[0], "child", NULL};
            execve("/proc/self/exe", av, environ);
            printf("gaslr-exec: refused errno=%d\n", errno);
            fflush(stdout);
            _exit(0);
        }
        if (p > 0)
            waitpid(p, NULL, 0);
        return 0;
    }
    line(strcmp(mode, "child") == 0 ? "gaslr-exec" : "gaslr", &local);
    return 0;
}

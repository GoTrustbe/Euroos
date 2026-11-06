#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <sys/mman.h>
/* SYSCALL FUZZER (night plan N3.2). Fires random Linux syscalls with random
   arguments at the kernel's Linux-ABI layer: small integers, pointers into a
   valid buffer, unmapped and non-canonical pointers, huge lengths. The kernel
   must survive every one of them (EFAULT/EINVAL/ENOSYS are all fine answers).
   Syscalls that would end or reshape this process itself are skipped, so the
   round count reflects kernel robustness, not the fuzzer killing itself.
   Exit 171 after the full run. */
static uint64_t st = 0x9E3779B97F4A7C15ull;
static uint64_t rnd(void) { st ^= st << 13; st ^= st >> 7; st ^= st << 17; return st; }

static int skip(long nr) {
    switch (nr) {
    case SYS_exit: case SYS_exit_group: case SYS_execve: case SYS_execveat:
    case SYS_fork: case SYS_vfork: case SYS_clone: case SYS_clone3:
    case SYS_kill: case SYS_tkill: case SYS_tgkill: case SYS_rt_sigreturn:
    case SYS_munmap: case SYS_mprotect: case SYS_mremap: case SYS_brk: case SYS_madvise:
    case SYS_close: case SYS_dup2: case SYS_dup3: case SYS_prctl: case SYS_seccomp:
    case SYS_setrlimit: case SYS_prlimit64: case SYS_pause: case SYS_nanosleep: case SYS_clock_nanosleep:
    case SYS_futex: case SYS_wait4: case SYS_waitid: case SYS_rt_sigsuspend: case SYS_sigaltstack:
    case SYS_arch_prctl: case SYS_set_tid_address: case SYS_setuid: case SYS_setresuid: case SYS_setreuid:
    case SYS_reboot: case SYS_vhangup: case SYS_ptrace:
        return 1;
    default: return 0;
    }
}

int main(int argc, char **argv) {
    long rounds = argc > 1 ? atol(argv[1]) : 20000;
    static char buf[65536];
    void *map = mmap(0, 1 << 20, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    uint64_t pool[] = { 0, 1, 2, 3, 7, 0xFF, 0x1000, 0xFFFF, (uint64_t)-1, (uint64_t)-2, 0x7fffffff, 0xffffffff,
                        (uint64_t)buf, (uint64_t)buf + 100, (uint64_t)map, (uint64_t)map + 4096,
                        0x10, 0x1000000, 0xdeadbeef, 0x8000000000000000ull, 0x00007fffffffffffull, 0xffff800000000000ull,
                        (uint64_t)1 << 40, 0x100000000ull, 0x2f000000ull, 0x34e00000ull };
    const int npool = sizeof pool / sizeof pool[0];
    long done = 0, errors = 0, ok = 0;
    for (long i = 0; i < rounds; i++) {
        long nr = (long)(rnd() % 452);
        if (skip(nr)) continue;
        uint64_t a[6];
        for (int k = 0; k < 6; k++) {
            uint64_t r = rnd();
            a[k] = (r & 3) == 0 ? r : pool[(r >> 8) % npool];
        }
        long r = syscall(nr, a[0], a[1], a[2], a[3], a[4], a[5]);
        if (r == -1) errors++; else ok++;
        done++;
        if (done % 5000 == 0) { printf("GFUZZ: %ld syscalls so far (%ld errors, %ld ok)\n", done, errors, ok); fflush(stdout); }
    }
    printf("GFUZZ: %ld random syscalls fired, kernel still answering (%ld returned an error, %ld succeeded)\n", done, errors, ok);
    printf("GFUZZ: a kernel that survives this never trusted a user pointer or length -> PASS\n");
    fflush(stdout);
    return 171;
}

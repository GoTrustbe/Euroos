/* EuroOS userspace program — freestanding (no libc), compiled by the
 * EuroToolchain (Track 6) and run in ring 3 on EuroKernel.
 *
 * Growing POSIX-like syscall set (rax=nr, rdi/rsi/rdx=args; `syscall`):
 *   0 exit(code) · 1 write(NUL-string) · 2 getpid() · 4 uname(buf,size)
 *   20 open(path) · 21 close(fd) · 22 read(fd,buf,len)
 */

static long sys(long n, long a1, long a2, long a3) {
    long ret;
    __asm__ volatile("syscall"
                     : "=a"(ret)
                     : "a"(n), "D"(a1), "S"(a2), "d"(a3)
                     : "rcx", "r11", "memory");
    return ret;
}

#define SYS_EXIT 0
#define SYS_WRITE 1
#define SYS_GETPID 2
#define SYS_UNAME 4
#define SYS_SBRK 12
#define SYS_OPEN 20
#define SYS_CLOSE 21
#define SYS_READ 22
#define SYS_NET 60

static void put(const char *s) { sys(SYS_WRITE, (long)s, 0, 0); }

/* Minimal malloc on top of sys_sbrk (bump allocation on the userspace heap). */
static void *malloc_(unsigned long n) {
    n = (n + 15) & ~15UL;
    long p = sys(SYS_SBRK, (long)n, 0, 0);
    return (p < 0) ? (void *)0 : (void *)p;
}

static const char *utoa(unsigned long v, char *end) {
    *end = 0;
    char *p = end;
    do {
        *--p = (char)('0' + (v % 10));
        v /= 10;
    } while (v);
    return p;
}

__attribute__((section(".text.start"))) void _start(void) {
    put("EuroOS userspace program (C, ring 3) — via syscalls:\n");

    char num[24];
    long pid = sys(SYS_GETPID, 0, 0, 0);
    put("  getpid() = ");
    put(utoa((unsigned long)pid, num + 23));
    put("\n");

    char uname[48];
    sys(SYS_UNAME, (long)uname, (long)sizeof(uname), 0);
    put("  uname()  = ");
    put(uname);
    put("\n");

    /* Open and read a file from EuroFS via syscalls. */
    long fd = sys(SYS_OPEN, (long)"/etc/hostname", 0, 0);
    if (fd >= 0) {
        char fbuf[64];
        long m = sys(SYS_READ, fd, (long)fbuf, (long)sizeof(fbuf) - 1);
        if (m < 0) m = 0;
        fbuf[m] = 0;
        put("  read(/etc/hostname, ");
        put(utoa((unsigned long)m, num + 23));
        put(" bytes) = ");
        put(fbuf);
        sys(SYS_CLOSE, fd, 0, 0);
    } else {
        put("  open(/etc/hostname) failed\n");
    }

    /* Try network access — this process has NO CAP_NET, so the kernel
     * should deny this (capability-based least privilege). */
    long net = sys(SYS_NET, 0, 0, 0);
    if (net < 0)
        put("  net()    = DENIED by kernel (process lacks CAP_NET)\n");
    else
        put("  net()    = allowed\n");

    /* Dynamic memory: malloc on top of sys_sbrk (userspace heap). */
    char *mem = (char *)malloc_(64);
    if (mem) {
        const char *src = "dynamic memory via sbrk()/malloc()!";
        char *d = mem;
        const char *q = src;
        while (*q) *d++ = *q++;
        *d = 0;
        put("  malloc() = ");
        put(mem);
        put("\n");
    }

    sys(SYS_EXIT, 0, 0, 0);
    __builtin_unreachable();
}

/* EuroOS 'cat' — displays a file via syscalls. Second independently
 * compiled userspace program (proves the toolchain + loader are generic,
 * not hardcoded to one program). Freestanding, no libc. */

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
#define SYS_OPEN 20
#define SYS_CLOSE 21
#define SYS_READ 22

static void put(const char *s) { sys(SYS_WRITE, (long)s, 0, 0); }

__attribute__((section(".text.start"))) void _start(void) {
    const char *path = "/etc/eurokernel.conf";
    long fd = sys(SYS_OPEN, (long)path, 0, 0);
    if (fd < 0) {
        put("cat: cannot open file\n");
        sys(SYS_EXIT, 1, 0, 0);
    }
    put("cat /etc/eurokernel.conf:\n");
    char buf[256];
    for (;;) {
        long m = sys(SYS_READ, fd, (long)buf, (long)sizeof(buf) - 1);
        if (m <= 0) break;
        buf[m] = 0;
        put(buf);
    }
    sys(SYS_CLOSE, fd, 0, 0);
    sys(SYS_EXIT, 0, 0, 0);
    __builtin_unreachable();
}

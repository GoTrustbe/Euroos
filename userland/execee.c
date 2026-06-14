/* EuroOS — S3 execve target: a MINIMAL program that replaces the child image.
 * Prints a line and exits with code 9, so the parent (via waitpid) can see
 * that execve actually replaced the image. */

static long sys(long n, long a1, long a2, long a3) {
    long ret;
    __asm__ volatile("syscall" : "=a"(ret) : "a"(n), "D"(a1), "S"(a2), "d"(a3) : "rcx", "r11", "memory");
    return ret;
}
#define L_WRITE 1
#define L_EXIT 60

static long slen(const char *s) {
    long n = 0;
    while (s[n]) n++;
    return n;
}

__attribute__((section(".text.start"))) void _start(void) {
    const char *m = "  [execee] new image running after execve -> exit(9)\n";
    sys(L_WRITE, 1, (long)m, slen(m));
    sys(L_EXIT, 9, 0, 0);
    for (;;) {
    }
}

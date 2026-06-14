/* EuroOS — standalone dynamic PIE with a THREAD-LOCAL variable (__thread).
 * No musl: the program sets up NO TLS of its own. The kernel (acting as ld.so) must
 * set up the static TLS block + FS_BASE (variant-II), otherwise the %fs access crashes.
 * `tls_value` starts at 41, becomes 42, and the program calls exit(42). Proves the
 * kernel TLS setup end-to-end. (Sprint 1 / H3.) */

__thread long tls_value = 41;

static void sys_exit(long code) {
    __asm__ volatile("syscall" ::"a"(60), "D"(code) : "rcx", "r11", "memory");
}

void _start(void) {
    tls_value += 1;        /* 41 -> 42, FS-relative (%fs:0x0 → TP, then TP-8) */
    sys_exit(tls_value);   /* exit(42) */
    __builtin_unreachable();
}

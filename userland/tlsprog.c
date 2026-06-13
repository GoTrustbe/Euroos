/* EuroOS — vrijstaande dynamische PIE met een THREAD-LOCAL variabele (__thread).
 * Geen musl: het programma zet GEEN eigen TLS op. De kernel (als ld.so) moet het
 * statische TLS-blok + FS_BASE opzetten (variant-II), anders crasht de %fs-toegang.
 * `tls_value` start op 41, wordt 42, en het programma exit(42). Bewijst de
 * kernel-TLS-setup end-to-end. (Sprint 1 / H3.) */

__thread long tls_value = 41;

static void sys_exit(long code) {
    __asm__ volatile("syscall" ::"a"(60), "D"(code) : "rcx", "r11", "memory");
}

void _start(void) {
    tls_value += 1;        /* 41 -> 42, FS-relatief (%fs:0x0 → TP, dan TP-8) */
    sys_exit(tls_value);   /* exit(42) */
    __builtin_unreachable();
}

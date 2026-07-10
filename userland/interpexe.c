/* EuroOS 3C-3 — a DYNAMICALLY-LINKED executable that names an interpreter
 * (PT_INTERP = /lib/ld-euro.so). It calls euroc_answer(), which lives in
 * libc-euro.so; the call goes through the PLT to a GOT slot that the USERSPACE
 * interpreter (ld-euro.so) fills in at startup (R_X86_64_JUMP_SLOT) — the real
 * Linux dynamic-linking flow, not the in-kernel linker. Prints "3C3: 42". */

extern long euroc_answer(void);

static long sys(long n, long a1, long a2, long a3) {
    long r;
    __asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a1), "S"(a2), "d"(a3)
                     : "rcx", "r11", "memory");
    return r;
}

void _start(void) {
    long v = euroc_answer(); /* cross-module call, resolved by ld-euro.so */
    char msg[8] = {'3', 'C', '3', ':', ' ', '0' + (char)(v / 10), '0' + (char)(v % 10), '\n'};
    sys(1, 1, (long)msg, 8); /* write(1, msg, 8) */
    sys(60, v, 0, 0);        /* exit(v) */
    for (;;) {
    }
}

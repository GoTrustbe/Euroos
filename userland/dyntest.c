/* EuroOS H3 — a DYNAMICALLY LINKED executable (PIE/ET_DYN). Calls euro_answer(),
 * which is NOT in this binary but in libeuro.so: the call goes through the PLT
 * to a GOT slot that the KERNEL DYNLINKER fills in at load time with the real
 * address of euro_answer in the loaded .so (R_X86_64_JUMP_SLOT). If the linking
 * succeeds, the program writes "H3: 42" and exit(42). */

extern long euro_answer(void);

static long sys(long n, long a1, long a2, long a3) {
    long r;
    __asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a1), "S"(a2), "d"(a3)
                     : "rcx", "r11", "memory");
    return r;
}

void _start(void) {
    long v = euro_answer(); /* cross-module call → JUMP_SLOT (resolved by the kernel) */
    char msg[8];
    msg[0] = 'H';
    msg[1] = '3';
    msg[2] = ':';
    msg[3] = ' ';
    msg[4] = '0' + (char)((v / 10) % 10);
    msg[5] = '0' + (char)(v % 10);
    msg[6] = '\n';
    msg[7] = 0;
    sys(1, 1, (long)msg, 7); /* write(1, msg, 7) — Linux ABI */
    sys(60, v, 0, 0);        /* exit(42) */
    for (;;) {
    }
}

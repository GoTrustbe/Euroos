/* EuroOS H3 — een DYNAMISCH GELINKTE executable (PIE/ET_DYN). Roept euro_answer()
 * aan, dat NIET in deze binary zit maar in libeuro.so: de aanroep loopt via de PLT
 * naar een GOT-slot dat de KERNEL-DYNLINKER bij het laden invult met het echte
 * adres van euro_answer in de geladen .so (R_X86_64_JUMP_SLOT). Slaagt de linking,
 * dan schrijft het programma "H3: 42" en exit(42). */

extern long euro_answer(void);

static long sys(long n, long a1, long a2, long a3) {
    long r;
    __asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a1), "S"(a2), "d"(a3)
                     : "rcx", "r11", "memory");
    return r;
}

void _start(void) {
    long v = euro_answer(); /* cross-module call → JUMP_SLOT (door de kernel resolved) */
    char msg[8];
    msg[0] = 'H';
    msg[1] = '3';
    msg[2] = ':';
    msg[3] = ' ';
    msg[4] = '0' + (char)((v / 10) % 10);
    msg[5] = '0' + (char)(v % 10);
    msg[6] = '\n';
    msg[7] = 0;
    sys(1, 1, (long)msg, 7); /* write(1, msg, 7) — Linux-ABI */
    sys(60, v, 0, 0);        /* exit(42) */
    for (;;) {
    }
}

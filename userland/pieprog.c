/* EuroOS — een ECHTE positie-onafhankelijke executable (PIE / ET_DYN) met
 * R_X86_64_RELATIVE-relocaties. De pointer-array hieronder dwingt relocaties af:
 * elke pointer is opgeslagen als een offset-vanaf-0 en moet door de loader met
 * de load-bias worden gecorrigeerd. Verschijnen de woorden correct, dan heeft de
 * kernel de relocaties toegepast — exact wat een musl static-PIE binary vereist. */

static long sys(long n, long a1, long a2, long a3) {
    long ret;
    __asm__ volatile("syscall" : "=a"(ret) : "a"(n), "D"(a1), "S"(a2), "d"(a3)
                     : "rcx", "r11", "memory");
    return ret;
}
#define SYS_EXIT 0
#define SYS_WRITE 1
static long slen(const char *s) { long n = 0; while (s[n]) n++; return n; }
static void put(const char *s) { sys(SYS_WRITE, (long)s, 0, 0); }

/* Array van pointers -> R_X86_64_RELATIVE relocaties in .rela.dyn. */
static const char *const woorden[] = {
    "soeverein", "Europees", "besturingssysteem",
};
#define N (sizeof(woorden) / sizeof(woorden[0]))

void real_start(void) {
    put("PIE met relocaties draait. Gerelokeerde pointers:\n");
    for (unsigned i = 0; i < N; i++) {
        unsigned j = i;
        /* Ondoorzichtige index: dwingt runtime-indexering af zodat de compiler de
         * pointer-array NIET wegoptimaliseert -> echte R_X86_64_RELATIVE relocs. */
        __asm__ volatile("" : "+r"(j));
        put("  - ");
        put(woorden[j]); /* garbage zonder relocatie, correct mét */
        put("\n");
    }
    sys(SYS_EXIT, 0, 0, 0);
    __builtin_unreachable();
}

__asm__(".section .text.start\n"
        ".globl _start\n"
        "_start:\n"
        "  and $-16, %rsp\n"
        "  call real_start\n");

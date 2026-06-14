/* EuroOS — a REAL position-independent executable (PIE / ET_DYN) with
 * R_X86_64_RELATIVE relocations. The pointer array below forces relocations:
 * each pointer is stored as an offset-from-0 and must be corrected by the loader
 * with the load bias. If the words appear correctly, then the kernel has applied
 * the relocations — exactly what a musl static-PIE binary requires. */

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

/* Array of pointers -> R_X86_64_RELATIVE relocations in .rela.dyn. */
static const char *const woorden[] = {
    "sovereign", "European", "operating system",
};
#define N (sizeof(woorden) / sizeof(woorden[0]))

void real_start(void) {
    put("PIE with relocations is running. Relocated pointers:\n");
    for (unsigned i = 0; i < N; i++) {
        unsigned j = i;
        /* Opaque index: forces runtime indexing so the compiler does NOT
         * optimize away the pointer array -> real R_X86_64_RELATIVE relocs. */
        __asm__ volatile("" : "+r"(j));
        put("  - ");
        put(woorden[j]); /* garbage without relocation, correct with it */
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

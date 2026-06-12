/* EuroOS — programma dat zijn INITIËLE STACK leest: argc, argv[], envp[] en de
 * auxiliary vector (auxv). Dit is het SysV-x86-64 proces-entry-contract dat een
 * echte musl/glibc `_start` van de kernel verwacht. Bewijst dat EuroKernel een
 * conforme stack opbouwt — de andere helft (naast de syscall-ABI) van het
 * draaien van ongewijzigde Linux-binaries. */

static long sys(long n, long a1, long a2, long a3) {
    long ret;
    __asm__ volatile("syscall" : "=a"(ret) : "a"(n), "D"(a1), "S"(a2), "d"(a3)
                     : "rcx", "r11", "memory");
    return ret;
}
#define SYS_EXIT 0
#define SYS_WRITE 1
static void put(const char *s) { sys(SYS_WRITE, (long)s, 0, 0); }

static char *utoa(unsigned long v, char *end) {
    *end-- = 0;
    char *p = end + 1;
    do { *--p = (char)('0' + (v % 10)); v /= 10; } while (v);
    return p;
}

/* _start zonder prologue: pak rsp (wijst naar argc) vóór elke stackmanipulatie
 * en geef het door aan real_start. */
__asm__(".section .text.start\n"
        ".globl _start\n"
        "_start:\n"
        "  mov %rsp, %rdi\n"
        "  and $-16, %rsp\n"
        "  call real_start\n");

#define AT_NULL 0
#define AT_PAGESZ 6
#define AT_RANDOM 25

void real_start(unsigned long *sp) {
    char nb[24];
    unsigned long argc = sp[0];
    char **argv = (char **)(sp + 1);
    char **envp = argv + argc + 1;

    put("SysV-stack gelezen door het proces:\n");
    put("  argc = "); put(utoa(argc, nb + 23)); put("\n");
    put("  argv[0] = "); put(argv[0] ? argv[0] : "(null)"); put("\n");

    /* envp doorlopen tot de NULL-terminator, dan begint auxv. */
    char **e = envp;
    unsigned long envc = 0;
    while (*e) { e++; envc++; }
    put("  envc = "); put(utoa(envc, nb + 23)); put("\n");

    /* auxv: paren (type, waarde) tot AT_NULL. */
    unsigned long *aux = (unsigned long *)(e + 1);
    unsigned long pagesz = 0, *rnd = 0;
    for (; aux[0] != AT_NULL; aux += 2) {
        if (aux[0] == AT_PAGESZ) pagesz = aux[1];
        else if (aux[0] == AT_RANDOM) rnd = (unsigned long *)aux[1];
    }
    put("  auxv AT_PAGESZ = "); put(utoa(pagesz, nb + 23)); put("\n");
    if (rnd) {
        put("  auxv AT_RANDOM[0..1] = ");
        put(utoa((unsigned long)((unsigned char *)rnd)[0], nb + 23)); put(",");
        put(utoa((unsigned long)((unsigned char *)rnd)[1], nb + 23)); put("\n");
    }
    sys(SYS_EXIT, 0, 0, 0);
    __builtin_unreachable();
}

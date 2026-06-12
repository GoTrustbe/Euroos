/* EuroOS — programma dat de Linux-startup-syscalls van een echte musl-binary
 * nabootst: arch_prctl(SET_FS) voor TLS, set_tid_address, mmap voor een buffer,
 * en writev (musl-stdio). Bewijst dat de Linux-compat-laag een musl-achtige
 * opstartsequentie aankan — de stap vóór ONGEWIJZIGDE musl-binaries draaien. */

static long sys(long n, long a1, long a2, long a3) {
    long ret;
    __asm__ volatile("syscall" : "=a"(ret) : "a"(n), "D"(a1), "S"(a2), "d"(a3)
                     : "rcx", "r11", "memory");
    return ret;
}
static long sys6(long n, long a1, long a2, long a3, long a4, long a5, long a6) {
    long ret;
    register long r10 __asm__("r10") = a4;
    register long r8  __asm__("r8")  = a5;
    register long r9  __asm__("r9")  = a6;
    __asm__ volatile("syscall" : "=a"(ret)
                     : "a"(n), "D"(a1), "S"(a2), "d"(a3), "r"(r10), "r"(r8), "r"(r9)
                     : "rcx", "r11", "memory");
    return ret;
}

#define L_WRITE 1
#define L_MMAP 9
#define L_WRITEV 20
#define L_GETPID 39
#define L_EXIT 60
#define L_ARCH_PRCTL 158
#define L_SET_TID_ADDRESS 218
#define ARCH_SET_FS 0x1002

struct iovec { const void *base; unsigned long len; };

static long slen(const char *s) { long n = 0; while (s[n]) n++; return n; }
static void put(const char *s) { sys(L_WRITE, 1, (long)s, slen(s)); }
static const char *utoa(unsigned long v, char *end) {
    *end = 0; char *p = end;
    do { *--p = (char)('0' + (v % 10)); v /= 10; } while (v);
    return p;
}

/* TLS-blok waar FS naar wijst; musl leest thread-pointer via %fs:0. */
static long tls_area[16];

__attribute__((section(".text.start"))) void _start(void) {
    /* 1. TLS opzetten zoals musl: FS_BASE -> ons TLS-blok, self-pointer op offset 0. */
    tls_area[0] = (long)tls_area;
    long r = sys(L_ARCH_PRCTL, ARCH_SET_FS, (long)tls_area, 0);
    put(r == 0 ? "arch_prctl(SET_FS): TLS ingesteld OK\n"
              : "arch_prctl(SET_FS): MISLUKT\n");

    /* 2. Lees de thread-pointer terug via %fs:0 — bewijst dat FS_BASE werkt. */
    long tp;
    __asm__ volatile("mov %%fs:0, %0" : "=r"(tp));
    put(tp == (long)tls_area ? "  %fs:0 leest thread-pointer terug: OK\n"
                            : "  %fs:0 fout\n");

    /* 3. set_tid_address (musl roept dit in __init_tp). */
    static long tid;
    long t = sys(L_SET_TID_ADDRESS, (long)&tid, 0, 0);
    char nb[24];
    put("  set_tid_address -> tid "); put(utoa((unsigned long)t, nb + 23)); put("\n");

    /* 4. mmap een anonieme buffer (MAP_PRIVATE|MAP_ANONYMOUS = 0x22). */
    long page = sys6(L_MMAP, 0, 4096, 0x3 /*RW*/, 0x22, -1, 0);
    if (page > 0) {
        char *buf = (char *)page;
        buf[0] = 'H'; buf[1] = 'i'; buf[2] = '\n'; buf[3] = 0;
        put("  mmap(4096) OK, schrijf+lees uit nieuwe pagina: "); put(buf);
    } else {
        put("  mmap MISLUKT\n");
    }

    /* 5. writev — zoals musl's gebufferde stdio onder de motorkap. */
    struct iovec iov[2] = {
        { "  writev: deel-1 ", 16 },
        { "+ deel-2 (musl-stdio pad)\n", 26 },
    };
    sys(L_WRITEV, 1, (long)iov, 2);

    sys(L_EXIT, 0, 0, 0);
    __builtin_unreachable();
}

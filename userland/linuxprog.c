/* EuroOS — programma dat de LINUX x86-64 syscall-ABI gebruikt (andere nummers +
 * semantiek dan onze native ABI). Draait via de Linux-compat-laag van de kernel.
 * Dit is de opstap naar het draaien van ONGEWIJZIGDE musl/Linux-binaries. */

static long sys(long n, long a1, long a2, long a3) {
    long ret;
    __asm__ volatile("syscall"
                     : "=a"(ret)
                     : "a"(n), "D"(a1), "S"(a2), "d"(a3)
                     : "rcx", "r11", "memory");
    return ret;
}

/* Linux x86-64 syscall-nummers */
#define L_READ 0
#define L_WRITE 1
#define L_OPEN 2
#define L_CLOSE 3
#define L_GETDENTS64 217
#define L_READLINK 89
#define L_GETPID 39
#define L_GETUID 102
#define L_GETTIMEOFDAY 96
#define L_UNAME 63
#define L_EXIT 60

static long slen(const char *s) {
    long n = 0;
    while (s[n]) n++;
    return n;
}
/* Linux write(fd, buf, count) — count-gebaseerd, niet NUL-getermineerd. */
static void put(const char *s) { sys(L_WRITE, 1, (long)s, slen(s)); }

static const char *utoa(unsigned long v, char *end) {
    *end = 0;
    char *p = end;
    do {
        *--p = (char)('0' + (v % 10));
        v /= 10;
    } while (v);
    return p;
}

/* Open een /proc-bestand, lees het, en print de EERSTE regel ("  <pad>: <regel>"). */
static void cat_proc_line(const char *path) {
    long fd = sys(L_OPEN, (long)path, 0 /*O_RDONLY*/, 0);
    if (fd < 0) {
        put("  (kon ");
        put(path);
        put(" niet openen)\n");
        return;
    }
    char buf[256];
    long n = sys(L_READ, fd, (long)buf, sizeof(buf) - 1);
    sys(L_CLOSE, fd, 0, 0);
    if (n <= 0) {
        return;
    }
    for (long i = 0; i < n; i++) {
        if (buf[i] == '\n') {
            buf[i] = 0; /* kap af op de eerste regel */
            break;
        }
    }
    buf[n < 255 ? n : 255] = 0;
    put("  ");
    put(path);
    put(": ");
    put(buf);
    put("\n");
}

__attribute__((section(".text.start"))) void _start(void) {
    put("Hallo via de LINUX syscall-ABI (write=1, getpid=39, exit=60)!\n");
    char num[24];
    long pid = sys(L_GETPID, 0, 0, 0);
    put("  Linux getpid()      = ");
    put(utoa((unsigned long)pid, num + 23));
    put("\n");

    /* uname(2): de kernel spiegelt een Linux-utsname zodat echte Linux-binaries
     * de kernelversie kunnen inspecteren. Veld 0 = sysname, veld 2 = release. */
    char uts[6 * 65];
    if (sys(L_UNAME, (long)uts, 0, 0) == 0) {
        put("  Linux uname.sysname = ");
        put(uts);            /* sysname  (offset 0)   */
        put("  release = ");
        put(uts + 2 * 65);   /* release  (offset 130) */
        put("\n");
    }

    /* getuid(2): EuroOS-voorgrondproces draait als root (0). */
    long uid = sys(L_GETUID, 0, 0, 0);
    put("  Linux getuid()      = ");
    put(utoa((unsigned long)uid, num + 23));
    put("\n");

    /* gettimeofday(2): echte wandklok (Unix-epoch) uit de RTC. */
    long tv[2] = {0, 0};
    if (sys(L_GETTIMEOFDAY, (long)tv, 0, 0) == 0) {
        put("  Linux gettimeofday  = ");
        put(utoa((unsigned long)tv[0], num + 23));
        put(" (Unix-epoch seconden)\n");
    }

    /* /proc-synthese (Track 8.2): lees echte kernel-info via het Linux-VFS-pad. */
    put("  --- /proc (live kernel-info) ---\n");
    cat_proc_line("/proc/version");
    cat_proc_line("/proc/cpuinfo");
    cat_proc_line("/proc/meminfo");
    cat_proc_line("/proc/uptime");
    cat_proc_line("/proc/loadavg");

    /* readlink(/proc/self/exe): runtimes vinden zo hun eigen binarypad. */
    {
        char ex[64];
        long m = sys(L_READLINK, (long)"/proc/self/exe", (long)ex, sizeof(ex) - 1);
        if (m > 0) {
            ex[m] = 0;
            put("  readlink(/proc/self/exe) = ");
            put(ex);
            put("\n");
        }
    }

    /* /etc: echte configbestanden, nu zichtbaar voor Linux-programma's via de VFS. */
    put("  --- /etc (systeemconfig) ---\n");
    cat_proc_line("/etc/os-release");
    cat_proc_line("/etc/passwd");

    /* Maplisting via openat + getdents64 (zoals ls/find): som /etc op. */
    put("  --- getdents64(/etc) ---\n");
    long dfd = sys(L_OPEN, (long)"/etc", 0 /*O_RDONLY*/, 0);
    if (dfd >= 0) {
        char dbuf[1024];
        long n = sys(L_GETDENTS64, dfd, (long)dbuf, sizeof(dbuf));
        long off = 0;
        while (off < n) {
            /* linux_dirent64: d_ino(8) d_off(8) d_reclen(2)@16 d_type(1)@18 name@19 */
            unsigned short reclen = *(unsigned short *)(dbuf + off + 16);
            unsigned char dtype = (unsigned char)dbuf[off + 18];
            const char *name = dbuf + off + 19;
            put(dtype == 4 ? "    [d] " : "    [f] ");
            put(name);
            put("\n");
            if (reclen == 0) break;
            off += reclen;
        }
        sys(L_CLOSE, dfd, 0, 0);
    }

    sys(L_EXIT, 0, 0, 0);
    __builtin_unreachable();
}

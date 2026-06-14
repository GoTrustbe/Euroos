/* EuroOS — proof of THREADS (kernel `clone`, CLONE_VM). The main thread creates
 * with a raw clone() syscall a second thread that shares the SAME address space
 * (a shared counter `shared`), but has its own stack. The thread increments the
 * shared counter; the main thread then reads it out.
 *
 * The child path lives ENTIRELY in rip-relative asm: the child resumes with
 * cleared registers and its own stack, so no function call (PLT), no
 * rbp frame and no register pointers — only rip-relative access to the
 * shared globals. This makes it fully position- and register-independent. */
#include <unistd.h>
#include <stdlib.h>

#define CLONE_VM      0x00000100
#define CLONE_FS      0x00000200
#define CLONE_FILES   0x00000400
#define CLONE_SIGHAND 0x00000800
#define CLONE_THREAD  0x00010000

volatile long shared = 0; /* SHARED between the threads (same address space) */
volatile int done = 0;

static int slen(const char *s) { int n = 0; while (s[n]) n++; return n; }
static void emit(const char *s) { write(1, s, slen(s)); }
static int utoa(unsigned long v, char *b) {
    char t[24]; int n = 0;
    if (!v) { b[0] = '0'; return 1; }
    while (v) { t[n++] = '0' + (v % 10); v /= 10; }
    for (int i = 0; i < n; i++) b[i] = t[n - 1 - i];
    return n;
}

int main(void) {
    emit("mthread: main thread starts a 2nd thread (clone, CLONE_VM)...\n");
    long sz = 32768;
    char *stack = malloc(sz);
    char *child_sp = stack + sz; /* 16-aligned; stack grows downward */
    long flags = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD;

    long ret;
    /* clone(); in the child (rax==0) we increment `shared` 500000x, set `done`,
     * and terminate the thread — all rip-relative, no call/PLT/rbp. */
    asm volatile(
        "syscall\n\t"
        "test %%rax, %%rax\n\t"
        "jnz 2f\n\t"                 /* parent: skip the child path */
        "mov $500000, %%rcx\n\t"
        "1:\n\t"
        "incq shared(%%rip)\n\t"     /* increment shared counter */
        "dec %%rcx\n\t"
        "jnz 1b\n\t"
        "movl $1, done(%%rip)\n\t"   /* done signal for the main thread */
        "3:\n\t"                      /* terminate thread (exit loop until the scheduler switches away) */
        "mov $60, %%rax\n\t"
        "xor %%edi, %%edi\n\t"
        "syscall\n\t"
        "jmp 3b\n\t"
        "2:\n\t"
        : "=a"(ret)
        : "a"(56), "D"(flags), "S"(child_sp), "d"(0)
        : "rcx", "r11", "memory", "cc");

    /* main thread: wait until the 2nd thread is done, read the shared counter */
    while (!done) {
        for (volatile int i = 0; i < 2000; i++) {
        }
    }
    char msg[80];
    int o = 0;
    const char *m = "mthread: shared counter (by the 2nd thread): ";
    while (*m) msg[o++] = *m++;
    o += utoa((unsigned long)shared, msg + o);
    msg[o++] = '\n';
    write(1, msg, o);
    return 0;
}

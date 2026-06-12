/* EuroOS — bewijs van THREADS (kernel `clone`, CLONE_VM). De hoofd-thread maakt
 * met een rauwe clone()-syscall een tweede thread die DEZELFDE adresruimte deelt
 * (een gedeelde teller `shared`), maar een eigen stack heeft. De thread hoogt de
 * gedeelde teller op; de hoofd-thread leest hem daarna uit.
 *
 * Het child-pad staat VOLLEDIG in rip-relatieve asm: de child hervat met
 * gewiste registers en een eigen stack, dus geen functie-call (PLT), geen
 * rbp-frame en geen register-pointers — alleen rip-relatieve toegang tot de
 * gedeelde globals. Zo is het volledig positie- en register-onafhankelijk. */
#include <unistd.h>
#include <stdlib.h>

#define CLONE_VM      0x00000100
#define CLONE_FS      0x00000200
#define CLONE_FILES   0x00000400
#define CLONE_SIGHAND 0x00000800
#define CLONE_THREAD  0x00010000

volatile long shared = 0; /* GEDEELD tussen de threads (zelfde adresruimte) */
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
    emit("mthread: hoofd-thread start een 2e thread (clone, CLONE_VM)...\n");
    long sz = 32768;
    char *stack = malloc(sz);
    char *child_sp = stack + sz; /* 16-uitgelijnd; stack groeit omlaag */
    long flags = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD;

    long ret;
    /* clone(); in de child (rax==0) verhogen we `shared` 500000x, zetten `done`,
     * en beëindigen de thread — alles rip-relatief, geen call/PLT/rbp. */
    asm volatile(
        "syscall\n\t"
        "test %%rax, %%rax\n\t"
        "jnz 2f\n\t"                 /* ouder: sla het child-pad over */
        "mov $500000, %%rcx\n\t"
        "1:\n\t"
        "incq shared(%%rip)\n\t"     /* gedeelde teller ophogen */
        "dec %%rcx\n\t"
        "jnz 1b\n\t"
        "movl $1, done(%%rip)\n\t"   /* klaar-signaal voor de hoofd-thread */
        "3:\n\t"                      /* thread beëindigen (exit-lus tot de scheduler wegschakelt) */
        "mov $60, %%rax\n\t"
        "xor %%edi, %%edi\n\t"
        "syscall\n\t"
        "jmp 3b\n\t"
        "2:\n\t"
        : "=a"(ret)
        : "a"(56), "D"(flags), "S"(child_sp), "d"(0)
        : "rcx", "r11", "memory", "cc");

    /* hoofd-thread: wacht tot de 2e thread klaar is, lees de gedeelde teller */
    while (!done) {
        for (volatile int i = 0; i < 2000; i++) {
        }
    }
    char msg[80];
    int o = 0;
    const char *m = "mthread: gedeelde teller (door de 2e thread): ";
    while (*m) msg[o++] = *m++;
    o += utoa((unsigned long)shared, msg + o);
    msg[o++] = '\n';
    write(1, msg, o);
    return 0;
}

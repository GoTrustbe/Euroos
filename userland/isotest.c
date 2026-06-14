/* EuroOS — proves the MEMORY ISOLATION of the per-process model. This process
 * attempts, from ring 3, to read kernel memory (0x100000 = 1 MiB). In its
 * own page tables that address is supervisor-only (no USER bit), so the CPU
 * raises a page fault. The kernel THEN terminates only this process; the rest
 * of the system keeps running normally. If we get past the read, the
 * isolation would LEAK — and we report that. */
#include <unistd.h>

static unsigned slen(const char *s) {
    unsigned n = 0;
    while (s[n]) n++;
    return n;
}
static void emit(const char *s) {
    write(1, s, slen(s));
}

int main(void) {
    emit("isotest: reading kernel memory 0x100000 from ring 3...\n");
    volatile unsigned char *p = (volatile unsigned char *)0x100000;
    unsigned char v = *p; /* <-- page fault if isolation works */
    /* Unreachable with correct isolation: */
    char m[2] = {(char)('0' + (v % 10)), '\n'};
    emit("isotest: ISOLATION LEAK - read allowed, byte=");
    write(1, m, 2);
    for (;;) {
    }
}

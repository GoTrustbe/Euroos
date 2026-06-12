/* EuroOS — bewijst de GEHEUGENISOLATIE van het per-proces-model. Dit proces
 * probeert vanuit ring 3 kernelgeheugen (0x100000 = 1 MiB) te lezen. In zijn
 * eigen page tables is dat adres supervisor-only (geen USER-bit), dus de CPU
 * geeft een page fault. De kernel beëindigt DAARop alleen dit proces; de rest
 * van het systeem draait gewoon door. Komen we voorbij de lezing, dan zou de
 * isolatie LEKKEN — en dat melden we. */
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
    emit("isotest: leest kernelgeheugen 0x100000 vanuit ring 3...\n");
    volatile unsigned char *p = (volatile unsigned char *)0x100000;
    unsigned char v = *p; /* <-- page fault als de isolatie werkt */
    /* Onbereikbaar bij correcte isolatie: */
    char m[2] = {(char)('0' + (v % 10)), '\n'};
    emit("isotest: ISOLATIE LEK - lezing toegestaan, byte=");
    write(1, m, 2);
    for (;;) {
    }
}

/* EuroOS — EuroIPC-ontvanger. Claimt poort 42 en wacht op een bericht van een
 * ander proces, en print het. Gebruikt de eigen EuroIPC-syscalls (500/501/502)
 * via rauwe syscall-asm. Een gewone musl-binary. */
#include <unistd.h>

static long ipc(long n, long a, long b, long c) {
    long r;
    asm volatile("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c) : "rcx", "r11", "memory");
    return r;
}

int main(void) {
    write(1, "ipcrecv: claim poort 42, wacht op bericht...\n", 44);
    ipc(500, 42, 0, 0); /* register port 42 */

    char buf[128];
    long n = -11;
    for (long tries = 0; tries < 4000000; tries++) {
        n = ipc(502, (long)buf, sizeof buf, 0); /* recv */
        if (n > 0) break;
        for (volatile int i = 0; i < 200; i++) {
        }
    }
    if (n > 0) {
        write(1, "ipcrecv: ontvangen via EuroIPC: ", 32);
        write(1, buf, n);
        write(1, "\n", 1);
    } else {
        write(1, "ipcrecv: geen bericht ontvangen\n", 32);
    }
    return 0;
}

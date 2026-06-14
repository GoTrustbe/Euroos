/* EuroOS — EuroIPC receiver. Claims port 42 and waits for a message from
 * another process, then prints it. Uses the native EuroIPC syscalls (500/501/502)
 * via raw syscall asm. An ordinary musl binary. */
#include <unistd.h>

static long ipc(long n, long a, long b, long c) {
    long r;
    asm volatile("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c) : "rcx", "r11", "memory");
    return r;
}

int main(void) {
    write(1, "ipcrecv: claim port 42, wait for message...\n", 44);
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
        write(1, "ipcrecv: received via EuroIPC: ", 31);
        write(1, buf, n);
        write(1, "\n", 1);
    } else {
        write(1, "ipcrecv: no message received\n", 29);
    }
    return 0;
}

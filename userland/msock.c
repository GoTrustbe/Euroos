/* EuroOS — a REAL musl-libc binary that does NETWORKING via the ordinary POSIX
 * socket API. No custom stubs: socket()/connect()/write()/read() from musl
 * talk via the Linux syscall ABI (socket=41, connect=42, sendto=44,
 * recvfrom=45) to EuroKernel, which connects them to the native TCP/IP stack
 * (EuroNet) on top of the virtio-net NIC.
 *
 * The kernel resolves the target name during boot via DNS and passes the IP
 * as the environment variable FETCH_IP (host in FETCH_HOST). This keeps it a
 * completely standard sockets program — it does not hardcode a volatile IP. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>

int main(void) {
    const char *ip = getenv("FETCH_IP");
    const char *host = getenv("FETCH_HOST");
    if (!ip) ip = "172.66.147.243";
    if (!host) host = "example.com";

    printf("msock: HTTP GET via POSIX sockets on EuroOS\n");
    printf("  target: %s (%s):80\n", host, ip);

    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) { printf("  socket() failed\n"); return 1; }
    printf("  socket() -> fd %d\n", fd);

    struct sockaddr_in sa;
    memset(&sa, 0, sizeof sa);
    sa.sin_family = AF_INET;
    sa.sin_port = htons(80);
    sa.sin_addr.s_addr = inet_addr(ip);

    if (connect(fd, (struct sockaddr *)&sa, sizeof sa) != 0) {
        printf("  connect() failed\n");
        return 1;
    }
    printf("  connect() OK — TCP handshake complete\n");

    char req[256];
    int rn = snprintf(req, sizeof req,
        "GET / HTTP/1.0\r\nHost: %s\r\nConnection: close\r\n\r\n", host);
    write(fd, req, rn);
    printf("  GET sent (%d bytes)\n", rn);

    char buf[2048];
    int total = 0, n;
    char status[128] = {0};
    while ((n = read(fd, buf, sizeof buf)) > 0) {
        if (total == 0) {
            /* first line = status line */
            int i = 0;
            while (i < n && i < 127 && buf[i] != '\r' && buf[i] != '\n') {
                status[i] = buf[i];
                i++;
            }
        }
        total += n;
    }
    close(fd);

    printf("  response: %d bytes\n", total);
    printf("  status:   %s\n", status);
    return 0;
}

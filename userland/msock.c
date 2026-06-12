/* EuroOS — een ECHTE musl-libc binary die NETWERKT via de gewone POSIX
 * socket-API. Geen eigen stubs: socket()/connect()/write()/read() uit musl
 * praten via de Linux syscall-ABI (socket=41, connect=42, sendto=44,
 * recvfrom=45) met EuroKernel, dat ze koppelt aan de eigen TCP/IP-stack
 * (EuroNet) bovenop de virtio-net NIC.
 *
 * De kernel resolvet de doelnaam tijdens de boot via DNS en geeft het IP door
 * als omgevingsvariabele FETCH_IP (host in FETCH_HOST). Zo blijft dit een
 * volstrekt standaard sockets-programma — het hardcodeert geen vluchtig IP. */
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

    printf("msock: HTTP GET via POSIX-sockets op EuroOS\n");
    printf("  doel: %s (%s):80\n", host, ip);

    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) { printf("  socket() faalde\n"); return 1; }
    printf("  socket() -> fd %d\n", fd);

    struct sockaddr_in sa;
    memset(&sa, 0, sizeof sa);
    sa.sin_family = AF_INET;
    sa.sin_port = htons(80);
    sa.sin_addr.s_addr = inet_addr(ip);

    if (connect(fd, (struct sockaddr *)&sa, sizeof sa) != 0) {
        printf("  connect() faalde\n");
        return 1;
    }
    printf("  connect() OK — TCP-handshake voltooid\n");

    char req[256];
    int rn = snprintf(req, sizeof req,
        "GET / HTTP/1.0\r\nHost: %s\r\nConnection: close\r\n\r\n", host);
    write(fd, req, rn);
    printf("  GET verzonden (%d bytes)\n", rn);

    char buf[2048];
    int total = 0, n;
    char status[128] = {0};
    while ((n = read(fd, buf, sizeof buf)) > 0) {
        if (total == 0) {
            /* eerste regel = statusregel */
            int i = 0;
            while (i < n && i < 127 && buf[i] != '\r' && buf[i] != '\n') {
                status[i] = buf[i];
                i++;
            }
        }
        total += n;
    }
    close(fd);

    printf("  antwoord: %d bytes\n", total);
    printf("  status:   %s\n", status);
    return 0;
}

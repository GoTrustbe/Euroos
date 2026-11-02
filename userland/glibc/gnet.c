/* EuroOS: a real glibc program that fetches a web page over TCP with blocking
 * POSIX sockets (socket/connect/send/recv). It runs as a scheduled Linux-ABI
 * process; with the userspace NIC driver (net.driver=user) every blocking call
 * has to give the CPU away so the driver process e1000d can deliver the frames.
 * Usage: gnet <ipv4> <host>. Exit 78 when an HTTP status line came back. */
#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

int main(int argc, char **argv) {
    const char *ip = argc > 1 ? argv[1] : "172.66.147.243";
    const char *host = argc > 2 ? argv[2] : "example.com";
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) { printf("GNET: socket() failed\n"); fflush(stdout); _exit(1); }
    struct sockaddr_in sa;
    memset(&sa, 0, sizeof sa);
    sa.sin_family = AF_INET;
    sa.sin_port = htons(80);
    sa.sin_addr.s_addr = inet_addr(ip);
    if (connect(fd, (struct sockaddr *)&sa, sizeof sa) != 0) {
        printf("GNET: connect(%s:80) failed\n", ip); fflush(stdout); _exit(2);
    }
    char req[256];
    int n = snprintf(req, sizeof req, "GET / HTTP/1.0\r\nHost: %s\r\nConnection: close\r\n\r\n", host);
    if (send(fd, req, n, 0) != n) { printf("GNET: send failed\n"); fflush(stdout); _exit(3); }
    char buf[2048], status[96] = {0};
    long total = 0;
    ssize_t r;
    while ((r = recv(fd, buf, sizeof buf, 0)) > 0) {
        if (total == 0) {
            int i = 0; /* the status line: up to CR or LF */
            while (i < r && i < 95 && buf[i] != '\r' && buf[i] != '\n') { status[i] = buf[i]; i++; }
        }
        total += r;
    }
    close(fd);
    printf("GNET: glibc blocking sockets: connect+send+recv to %s (%s): %ld bytes, status \"%s\"\n", host, ip, total, status);
    fflush(stdout);
    _exit(strncmp(status, "HTTP/", 5) == 0 ? 78 : 4);
}

/* EuroOS — a REAL musl-libc binary that looks up a DNS name via a
 * UDP socket. socket(AF_INET, SOCK_DGRAM) / connect / write / read from musl
 * talk via the Linux syscall ABI to EuroKernel, which connects them to EuroNet's
 * UDP/IP layer. The program builds the DNS query itself and parses the A record
 * out of the answer — proving that connectionless (UDP) sockets really work.
 *
 * The nameserver comes from the environment variable DNS_IP (set by the kernel after
 * DHCP), the name to resolve from FETCH_HOST. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>

/* Encode "a.b.c" as DNS labels: <len>a<len>b<len>c<0>. */
static int encode_qname(unsigned char *out, const char *host) {
    int o = 0, start = 0, i = 0;
    for (;; i++) {
        if (host[i] == '.' || host[i] == 0) {
            int len = i - start;
            out[o++] = (unsigned char)len;
            for (int j = start; j < i; j++) out[o++] = host[j];
            start = i + 1;
            if (host[i] == 0) break;
        }
    }
    out[o++] = 0;
    return o;
}

/* One DNS lookup via a UDP socket. Prints the A record, or reports "no
 * answer" (e.g. when EuroGuard blocks the query at the DNS level). */
static void lookup(const char *ns, const char *host) {
    printf("  query: A %s\n", host);
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) { printf("    socket() failed\n"); return; }

    struct sockaddr_in sa;
    memset(&sa, 0, sizeof sa);
    sa.sin_family = AF_INET;
    sa.sin_port = htons(53);
    sa.sin_addr.s_addr = inet_addr(ns);
    if (connect(fd, (struct sockaddr *)&sa, sizeof sa) != 0) {
        printf("    connect() failed\n");
        close(fd);
        return;
    }

    unsigned char q[512];
    memset(q, 0, sizeof q);
    q[0] = 0x12; q[1] = 0x34;   /* transaction ID  */
    q[2] = 0x01; q[3] = 0x00;   /* flags: recursion desired */
    q[4] = 0x00; q[5] = 0x01;   /* QDCOUNT = 1   */
    int o = 12;
    o += encode_qname(q + 12, host);
    q[o++] = 0x00; q[o++] = 0x01; /* QTYPE  = A   */
    q[o++] = 0x00; q[o++] = 0x01; /* QCLASS = IN  */
    write(fd, q, o);

    unsigned char r[512];
    int n = read(fd, r, sizeof r);
    if (n <= 0) {
        printf("    no answer (did EuroGuard block the DNS query?)\n");
        close(fd);
        return;
    }
    int answers = (r[6] << 8) | r[7];

    int p = 12;
    while (p < n && r[p] != 0) p += r[p] + 1;
    p += 1 + 4; /* terminating 0 + QTYPE + QCLASS */
    for (int a = 0; a < answers && p + 12 <= n; a++) {
        if ((r[p] & 0xC0) == 0xC0) {
            p += 2;
        } else {
            while (p < n && r[p] != 0) p += r[p] + 1;
            p += 1;
        }
        int type = (r[p] << 8) | r[p + 1];
        int rdlen = (r[p + 8] << 8) | r[p + 9];
        unsigned char *rd = r + p + 10;
        if (type == 1 && rdlen == 4) {
            printf("    -> %d.%d.%d.%d\n", rd[0], rd[1], rd[2], rd[3]);
            close(fd);
            return;
        }
        p += 10 + rdlen;
    }
    printf("    no A record in the answer\n");
    close(fd);
}

int main(void) {
    const char *ns = getenv("DNS_IP");
    const char *host = getenv("FETCH_HOST");
    if (!ns) ns = "10.0.2.3";
    if (!host) host = "example.com";

    printf("mdns: DNS lookups via UDP socket on EuroOS (nameserver %s)\n", ns);
    /* 1) allowed domain -> real resolution */
    lookup(ns, host);
    /* 2) tracker domain on the EuroGuard block list -> no answer */
    lookup(ns, "ads.doubleclick.net");
    return 0;
}

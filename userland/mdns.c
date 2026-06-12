/* EuroOS — een ECHTE musl-libc binary die een DNS-naam opzoekt via een
 * UDP-socket. socket(AF_INET, SOCK_DGRAM) / connect / write / read uit musl
 * praten via de Linux syscall-ABI met EuroKernel, dat ze koppelt aan EuroNet's
 * UDP/IP-laag. Het programma bouwt de DNS-query zelf en parseert het A-record
 * uit het antwoord — bewijst dat verbindingsloze (UDP) sockets echt werken.
 *
 * De nameserver komt uit de omgevingsvariabele DNS_IP (door de kernel gezet na
 * DHCP), de te resolven naam uit FETCH_HOST. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>

/* Codeer "a.b.c" als DNS-labels: <len>a<len>b<len>c<0>. */
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

/* Eén DNS-lookup via een UDP-socket. Print het A-record, of meldt "geen
 * antwoord" (bv. wanneer EuroGuard de query op DNS-niveau blokkeert). */
static void lookup(const char *ns, const char *host) {
    printf("  vraag: A %s\n", host);
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) { printf("    socket() faalde\n"); return; }

    struct sockaddr_in sa;
    memset(&sa, 0, sizeof sa);
    sa.sin_family = AF_INET;
    sa.sin_port = htons(53);
    sa.sin_addr.s_addr = inet_addr(ns);
    if (connect(fd, (struct sockaddr *)&sa, sizeof sa) != 0) {
        printf("    connect() faalde\n");
        close(fd);
        return;
    }

    unsigned char q[512];
    memset(q, 0, sizeof q);
    q[0] = 0x12; q[1] = 0x34;   /* transactie-ID  */
    q[2] = 0x01; q[3] = 0x00;   /* vlaggen: recursie gewenst */
    q[4] = 0x00; q[5] = 0x01;   /* QDCOUNT = 1   */
    int o = 12;
    o += encode_qname(q + 12, host);
    q[o++] = 0x00; q[o++] = 0x01; /* QTYPE  = A   */
    q[o++] = 0x00; q[o++] = 0x01; /* QCLASS = IN  */
    write(fd, q, o);

    unsigned char r[512];
    int n = read(fd, r, sizeof r);
    if (n <= 0) {
        printf("    geen antwoord (EuroGuard blokkeerde de DNS-query?)\n");
        close(fd);
        return;
    }
    int answers = (r[6] << 8) | r[7];

    int p = 12;
    while (p < n && r[p] != 0) p += r[p] + 1;
    p += 1 + 4; /* afsluitende 0 + QTYPE + QCLASS */
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
    printf("    geen A-record in het antwoord\n");
    close(fd);
}

int main(void) {
    const char *ns = getenv("DNS_IP");
    const char *host = getenv("FETCH_HOST");
    if (!ns) ns = "10.0.2.3";
    if (!host) host = "example.com";

    printf("mdns: DNS-lookups via UDP-socket op EuroOS (nameserver %s)\n", ns);
    /* 1) toegestaan domein -> echte resolutie */
    lookup(ns, host);
    /* 2) tracker-domein op de EuroGuard-blokkeerlijst -> geen antwoord */
    lookup(ns, "ads.doubleclick.net");
    return 0;
}

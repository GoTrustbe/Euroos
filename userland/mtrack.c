/* EuroOS — demonstreert EuroGuard (Track 7). Deze app gedraagt zich als een
 * "telemetrie"-component die naar een tracker-endpoint probeert te bellen. De
 * kernel-policy (EuroGuard) blokkeert die verbinding VÓÓR er een pakket vertrekt
 * en logt de poging. Ter contrast maakt de app daarna een TOEGESTANE verbinding.
 *
 * Het is een gewone, ongewijzigde musl-binary die de standaard socket-API
 * gebruikt — de controle zit in de kernel, niet in de app. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>

static int try_connect(const char *ip, int port) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return -1;
    struct sockaddr_in sa;
    memset(&sa, 0, sizeof sa);
    sa.sin_family = AF_INET;
    sa.sin_port = htons(port);
    sa.sin_addr.s_addr = inet_addr(ip);
    int r = connect(fd, (struct sockaddr *)&sa, sizeof sa);
    close(fd);
    return r;
}

int main(void) {
    const char *tracker = "203.0.113.5"; /* bekend tracker-endpoint (geblokkeerd) */
    printf("mtrack: 'telemetrie'-app probeert thuis te bellen\n");
    printf("  doel: %s:80 (tracker)\n", tracker);
    if (try_connect(tracker, 80) != 0)
        printf("  connect() GEWEIGERD — EuroGuard blokkeerde dit \xe2\x9c\x93\n");
    else
        printf("  connect() gelukt — (verwacht: geblokkeerd?!)\n");

    const char *ok = getenv("FETCH_IP");
    if (ok) {
        printf("  ter contrast: %s:80 (toegestaan door policy)\n", ok);
        if (try_connect(ok, 80) == 0)
            printf("  connect() OK — verbinding toegestaan \xe2\x9c\x93\n");
        else
            printf("  connect() faalde\n");
    }
    return 0;
}

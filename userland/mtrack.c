/* EuroOS — demonstrates EuroGuard (Track 7). This app behaves like a
 * "telemetry" component that tries to phone home to a tracker endpoint. The
 * kernel policy (EuroGuard) blocks that connection BEFORE a packet leaves
 * and logs the attempt. For contrast, the app then makes an ALLOWED connection.
 *
 * It is a plain, unmodified musl binary that uses the standard socket API
 * — the control is in the kernel, not in the app. */
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
    const char *tracker = "203.0.113.5"; /* known tracker endpoint (blocked) */
    printf("mtrack: 'telemetry' app tries to phone home\n");
    printf("  target: %s:80 (tracker)\n", tracker);
    if (try_connect(tracker, 80) != 0)
        printf("  connect() DENIED — EuroGuard blocked this \xe2\x9c\x93\n");
    else
        printf("  connect() succeeded — (expected: blocked?!)\n");

    const char *ok = getenv("FETCH_IP");
    if (ok) {
        printf("  for contrast: %s:80 (allowed by policy)\n", ok);
        if (try_connect(ok, 80) == 0)
            printf("  connect() OK — connection allowed \xe2\x9c\x93\n");
        else
            printf("  connect() failed\n");
    }
    return 0;
}

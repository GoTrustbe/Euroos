/* gpolicy: a plain Linux program that touches the world around it; EuroOS puts
 * a policy around it from the OUTSIDE ([app-policy]). Prints one line per
 * attempt so the kernel can check the outcome, exit = bitmask of successes:
 *   1 = read /etc/hostname, 2 = write /home/euro/policy-demo.txt,
 *   4 = read /etc/euroid/users.db (must stay refused under any policy). */
#include <stdio.h>
#include <fcntl.h>
#include <unistd.h>
#include <string.h>
int main(void){
    int r = 0; char buf[64];
    int fd = open("/etc/hostname", O_RDONLY);
    if (fd >= 0) { if (read(fd, buf, sizeof buf) > 0) r |= 1; close(fd); }
    printf("GPOLICY: hostname %s\n", (r & 1) ? "read" : "REFUSED");
    fd = open("/home/euro/policy-demo.txt", O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd >= 0) { if (write(fd, "policy demo\n", 12) == 12) r |= 2; close(fd); }
    printf("GPOLICY: home write %s\n", (r & 2) ? "ok" : "REFUSED");
    fd = open("/etc/euroid/users.db", O_RDONLY);
    if (fd >= 0) { r |= 4; close(fd); }
    printf("GPOLICY: identity store %s\n", (r & 4) ? "READ (bad)" : "refused");
    fflush(stdout);
    _exit(r);
}

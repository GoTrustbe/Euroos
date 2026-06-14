/* EuroOS — musl program that reads the ENVIRONMENT VARIABLES the kernel passes
 * via envp on the SysV stack. Proves that the system environment works:
 * getenv() and the environ table are available as in any POSIX system. */
#include <stdio.h>
#include <stdlib.h>

extern char **environ;

int main(void) {
    printf("getenv via musl libc:\n");
    const char *keys[] = {"EUROOS_VERSION", "LANG", "TERM", "USER", "SHELL"};
    for (int i = 0; i < 5; i++) {
        const char *v = getenv(keys[i]);
        printf("  %-15s = %s\n", keys[i], v ? v : "(not set)");
    }
    int n = 0;
    for (char **e = environ; *e; e++) {
        n++;
    }
    printf("  (%d variables in environ)\n", n);
    return 0;
}

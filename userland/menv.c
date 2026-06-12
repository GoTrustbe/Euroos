/* EuroOS — musl-programma dat de OMGEVINGSVARIABELEN leest die de kernel via
 * envp op de SysV-stack doorgeeft. Bewijst dat het systeemmilieu werkt:
 * getenv() en de environ-tabel zijn beschikbaar zoals in elk POSIX-systeem. */
#include <stdio.h>
#include <stdlib.h>

extern char **environ;

int main(void) {
    printf("getenv via musl libc:\n");
    const char *keys[] = {"EUROOS_VERSION", "LANG", "TERM", "USER", "SHELL"};
    for (int i = 0; i < 5; i++) {
        const char *v = getenv(keys[i]);
        printf("  %-15s = %s\n", keys[i], v ? v : "(niet gezet)");
    }
    int n = 0;
    for (char **e = environ; *e; e++) {
        n++;
    }
    printf("  (%d variabelen in environ)\n", n);
    return 0;
}

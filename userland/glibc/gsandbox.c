#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/prctl.h>
#include <linux/seccomp.h>
/* SYSCALL-PROFILE test (sprint WP1). A process that asks for seccomp strict
   mode may only read, write and exit afterwards; the first other syscall must
   end the process (SIGKILL, exit status 137). Without the request the same
   program opens a file normally and exits 161.
   The open() after strict mode must NEVER return: if it does, the kernel did
   not enforce the profile and the program exits 3 to say so. */
int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "strict") == 0) {
        printf("GSBX: requesting seccomp strict mode\n");
        fflush(stdout);
        if (prctl(PR_SET_SECCOMP, SECCOMP_MODE_STRICT) != 0) {
            printf("GSBX: prctl(PR_SET_SECCOMP) FAILED\n");
            fflush(stdout);
            return 2;
        }
        const char msg[] = "GSBX: write still allowed under strict mode\n";
        write(1, msg, sizeof msg - 1);
        int fd = open("/etc/hostname", O_RDONLY); /* must be fatal */
        const char bad[] = "GSBX: open returned -> profile NOT enforced\n";
        write(1, bad, sizeof bad - 1);
        (void)fd;
        return 3;
    }
    int fd = open("/etc/hostname", O_RDONLY);
    printf("GSBX: unsandboxed open -> fd %d (%s)\n", fd, fd >= 0 ? "ok" : "failed");
    if (fd >= 0) close(fd);
    printf("GSBX: a process keeps full syscalls until it asks for less -> PASS\n");
    fflush(stdout);
    return 161;
}

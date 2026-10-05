#define _GNU_SOURCE
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/mman.h>
#include <linux/seccomp.h>
#include <linux/filter.h>
#include <linux/audit.h>
/* SYSCALL-PROFILE tests (sprint WP1). Each mode has one expected exit status;
   anything else means the kernel did not enforce what it promised.
     (none)            full profile: open works                        -> 161
     strict            seccomp strict: first open() ends the process    -> 137
     filter            cBPF filter: getpid EPERM, socket kills          -> 159
     rlimit            RLIMIT_NOFILE enforced, hard limit not raisable  -> 165
     aslr              prints stack/heap/mmap addresses                 -> 167
     --type=renderer   renderer scope: /etc ok, /home refused, uid drop -> 169 */

static void say(const char *s) { (void)!write(1, s, strlen(s)); }

static int mode_strict(void) {
    say("GSBX: requesting seccomp strict mode\n");
    if (prctl(PR_SET_SECCOMP, SECCOMP_MODE_STRICT) != 0) { say("GSBX: prctl(PR_SET_SECCOMP) FAILED\n"); return 2; }
    say("GSBX: write still allowed under strict mode\n");
    int fd = open("/etc/hostname", O_RDONLY); /* must be fatal */
    say("GSBX: open returned -> profile NOT enforced\n");
    (void)fd;
    return 3;
}

static int mode_filter(void) {
    struct sock_filter prog[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_getpid, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EPERM),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_socket, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog fprog = { .len = sizeof prog / sizeof prog[0], .filter = prog };
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0) { say("GSBX: PR_SET_NO_NEW_PRIVS FAILED\n"); return 2; }
    if (syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &fprog) != 0) {
        printf("GSBX: seccomp(SET_MODE_FILTER) FAILED errno=%d\n", errno); fflush(stdout); return 2;
    }
    say("GSBX: cBPF filter installed\n");
    long pid = syscall(SYS_getpid);
    if (pid == -1 && errno == EPERM) say("GSBX: getpid -> EPERM (filter ERRNO action works)\n");
    else { printf("GSBX: getpid returned %ld errno=%d -> filter NOT enforced\n", pid, errno); fflush(stdout); return 3; }
    say("GSBX: write still allowed under the filter\n");
    int s = socket(AF_UNIX, SOCK_STREAM, 0); /* must be fatal */
    printf("GSBX: socket returned %d -> filter kill NOT enforced\n", s); fflush(stdout);
    return 4;
}

static int mode_rlimit(void) {
    struct rlimit r;
    if (getrlimit(RLIMIT_NOFILE, &r) != 0) { say("GSBX: getrlimit FAILED\n"); return 2; }
    printf("GSBX: nofile cur=%llu max=%llu\n", (unsigned long long)r.rlim_cur, (unsigned long long)r.rlim_max);
    struct rlimit want = { 8, 8 };
    if (setrlimit(RLIMIT_NOFILE, &want) != 0) { printf("GSBX: setrlimit(8,8) FAILED errno=%d\n", errno); fflush(stdout); return 3; }
    if (getrlimit(RLIMIT_NOFILE, &r) != 0 || r.rlim_cur != 8 || r.rlim_max != 8) { say("GSBX: limit not stored\n"); return 4; }
    int opened = 0, last_errno = 0;
    for (int i = 0; i < 20; i++) {
        int fd = open("/etc/hostname", O_RDONLY);
        if (fd < 0) { last_errno = errno; break; }
        opened++;
    }
    printf("GSBX: opened %d files before failure errno=%d (want 5 then EMFILE=%d)\n", opened, last_errno, EMFILE);
    struct rlimit raise = { 8, 64 };
    int rr = setrlimit(RLIMIT_NOFILE, &raise);
    printf("GSBX: raising the hard limit -> %s\n", rr != 0 && errno == EPERM ? "EPERM (correct)" : "ALLOWED (wrong)");
    fflush(stdout);
    if (opened == 5 && last_errno == EMFILE && rr != 0 && errno == EPERM) { say("GSBX: resource limits enforced -> PASS\n"); return 165; }
    return 5;
}

static int mode_aslr(void) {
    int on_stack = 0;
    void *heap = malloc(4096);
    void *map = mmap(0, 65536, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    printf("GSBX-ASLR stack=%p heap=%p mmap=%p\n", (void *)&on_stack, heap, map);
    fflush(stdout);
    return 167;
}

static int mode_renderer(void) {
    int ok = 1;
    int fd = open("/etc/hostname", O_RDONLY);
    printf("GSBX: open(/etc/hostname) -> %d (%s)\n", fd, fd >= 0 ? "ok, in scope" : "FAILED");
    if (fd < 0) ok = 0; else close(fd);
    int fd2 = open("/home/euro/demo/welcome.txt", O_RDONLY);
    int e2 = errno;
    printf("GSBX: open(/home/euro/...) -> %d errno=%d (%s)\n", fd2, e2, fd2 < 0 && e2 == EACCES ? "EACCES, out of scope" : "ALLOWED (wrong)");
    if (!(fd2 < 0 && e2 == EACCES)) ok = 0;
    uid_t before = getuid();
    int su = setuid(65534);
    uid_t after = getuid();
    int su0 = setuid(0);
    printf("GSBX: uid %u -> setuid(65534)=%d -> %u; setuid(0)=%d (%s)\n", before, su, after, su0, su0 != 0 ? "EPERM, correct" : "ALLOWED (wrong)");
    if (!(su == 0 && after == 65534 && su0 != 0)) ok = 0;
    fflush(stdout);
    if (ok) { say("GSBX: renderer scope + identity drop -> PASS\n"); return 169; }
    return 6;
}

int main(int argc, char **argv) {
    const char *m = argc > 1 ? argv[1] : "";
    if (strcmp(m, "strict") == 0) return mode_strict();
    if (strcmp(m, "filter") == 0) return mode_filter();
    if (strcmp(m, "rlimit") == 0) return mode_rlimit();
    if (strcmp(m, "aslr") == 0) return mode_aslr();
    if (strcmp(m, "--type=renderer") == 0) return mode_renderer();
    int fd = open("/etc/hostname", O_RDONLY);
    printf("GSBX: unsandboxed open -> fd %d (%s)\n", fd, fd >= 0 ? "ok" : "failed");
    if (fd >= 0) close(fd);
    printf("GSBX: a process keeps full syscalls until it asks for less -> PASS\n");
    fflush(stdout);
    return 161;
}

#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/wait.h>

/* CROSS-PROCESS shared memory, exactly the way chrome uses it: one process
   creates a memfd and hands the DESCRIPTOR to another process over a socket
   with SCM_RIGHTS, and both then map it MAP_SHARED. Everything chrome does
   between processes rides on this - perfetto gives every child a shared
   ring buffer this way, and a child that cannot map it never acks
   BeginTracing (which is exactly what this kernel's chrome children do).

   gshm covers two mappings inside ONE process. This covers the part that
   crosses a process boundary and a descriptor table:

     1. the child maps the passed memfd and sees the parent's pattern,
     2. the child's writes are visible to the PARENT through its own mapping,
     3. a page the parent writes AFTER the child mapped is visible there too
        (one memory, not a snapshot taken at mmap time).

   Exit 141 = all three hold. */

#define SZ 65536

static int send_fd(int sock, int fd) {
    char c = 'F';
    struct iovec iov = { .iov_base = &c, .iov_len = 1 };
    char cbuf[CMSG_SPACE(sizeof(int))];
    memset(cbuf, 0, sizeof cbuf);
    struct msghdr msg = { .msg_iov = &iov, .msg_iovlen = 1,
                          .msg_control = cbuf, .msg_controllen = sizeof cbuf };
    struct cmsghdr *cm = CMSG_FIRSTHDR(&msg);
    cm->cmsg_level = SOL_SOCKET; cm->cmsg_type = SCM_RIGHTS;
    cm->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(cm), &fd, sizeof(int));
    return sendmsg(sock, &msg, 0) == 1 ? 0 : -1;
}

static int recv_fd(int sock) {
    char c = 0;
    struct iovec iov = { .iov_base = &c, .iov_len = 1 };
    char cbuf[CMSG_SPACE(sizeof(int))];
    memset(cbuf, 0, sizeof cbuf);
    struct msghdr msg = { .msg_iov = &iov, .msg_iovlen = 1,
                          .msg_control = cbuf, .msg_controllen = sizeof cbuf };
    if (recvmsg(sock, &msg, 0) != 1) return -1;
    struct cmsghdr *cm = CMSG_FIRSTHDR(&msg);
    if (!cm || cm->cmsg_type != SCM_RIGHTS) return -1;
    int fd; memcpy(&fd, CMSG_DATA(cm), sizeof(int));
    return fd;
}

static int run_case(int do_close) {
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) {
        printf("GSHM2[%d]: socketpair FAILED\n", do_close); fflush(stdout); return 1;
    }
    int fd = memfd_create("euroshm2", 0);
    if (fd < 0) { printf("GSHM2[%d]: memfd_create FAILED\n", do_close); fflush(stdout); return 2; }
    if (ftruncate(fd, (off_t)SZ) != 0) { printf("GSHM2[%d]: ftruncate FAILED\n", do_close); fflush(stdout); return 3; }

    unsigned char *p = mmap(NULL, SZ, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (p == MAP_FAILED) { printf("GSHM2[%d]: parent mmap FAILED\n", do_close); fflush(stdout); return 4; }
    /* Pattern in the FIRST half; the second half is written later, after the
       child has already mapped, to prove the mapping is live and not a copy. */
    for (size_t i = 0; i < SZ / 2; i += 4096) p[i] = (unsigned char)(0x50 + (i / 4096));

    pid_t kid = fork();
    if (kid < 0) { printf("GSHM2[%d]: fork FAILED\n", do_close); fflush(stdout); return 5; }

    if (kid == 0) {
        if (do_close) close(sv[0]);
        int rfd = recv_fd(sv[1]);
        if (rfd < 0) { printf("GSHM2[%d]: child recv_fd FAILED\n", do_close); fflush(stdout); _exit(11); }
        unsigned char *q = mmap(NULL, SZ, PROT_READ | PROT_WRITE, MAP_SHARED, rfd, 0);
        if (q == MAP_FAILED) { printf("GSHM2[%d]: child mmap FAILED\n", do_close); fflush(stdout); _exit(12); }
        size_t bad = 0;
        for (size_t i = 0; i < SZ / 2; i += 4096)
            if (q[i] != (unsigned char)(0x50 + (i / 4096))) bad++;
        printf("GSHM2[%d]: child sees parent pattern, mismatched=%zu of %zu\n", do_close, bad, (size_t)((SZ / 2) / 4096));
        fflush(stdout);
        if (bad) _exit(13);
        /* Write back, then tell the parent to look. */
        for (size_t i = 0; i < SZ / 2; i += 4096) q[i] = (unsigned char)(0xA0 + (i / 4096));
        char ack = 'C';
        if (write(sv[1], &ack, 1) != 1) _exit(14);
        /* Wait for the parent's late write, then check it is visible here. */
        char go = 0;
        if (read(sv[1], &go, 1) != 1) _exit(15);
        bad = 0;
        for (size_t i = SZ / 2; i < SZ; i += 4096)
            if (q[i] != (unsigned char)(0x70 + ((i - SZ / 2) / 4096))) bad++;
        printf("GSHM2[%d]: child sees parent's LATE write, mismatched=%zu\n", do_close, bad);
        fflush(stdout);
        _exit(bad ? 16 : 0);
    }

    if (do_close) close(sv[1]);
    if (send_fd(sv[0], fd) != 0) { printf("GSHM2[%d]: send_fd FAILED\n", do_close); fflush(stdout); return 6; }
    char ack = 0;
    if (read(sv[0], &ack, 1) != 1) { printf("GSHM2[%d]: no ack from child\n", do_close); fflush(stdout); return 7; }
    size_t bad = 0;
    for (size_t i = 0; i < SZ / 2; i += 4096)
        if (p[i] != (unsigned char)(0xA0 + (i / 4096))) bad++;
    printf("GSHM2[%d]: parent sees child writes, mismatched=%zu of %zu\n", do_close, bad, (size_t)((SZ / 2) / 4096));
    fflush(stdout);
    if (bad) return 8;
    /* Late write, only now, into the second half. */
    for (size_t i = SZ / 2; i < SZ; i += 4096) p[i] = (unsigned char)(0x70 + ((i - SZ / 2) / 4096));
    char go = 'G';
    if (write(sv[0], &go, 1) != 1) return 9;
    int st = 0;
    if (waitpid(kid, &st, 0) != kid) { printf("GSHM2[%d]: waitpid FAILED\n", do_close); fflush(stdout); return 10; }
    if (!WIFEXITED(st) || WEXITSTATUS(st) != 0) {
        printf("GSHM2[%d]: child exit=%d\n", do_close, WIFEXITED(st) ? WEXITSTATUS(st) : -1);
        fflush(stdout);
        return 20 + (WIFEXITED(st) ? WEXITSTATUS(st) : 0);
    }
    printf("GSHM2[%d]: cross-process shared memory OK\n", do_close); fflush(stdout);
    return 0;
}

int main(void) {
    /* Case 1: neither side closes the end it does not use.
       Case 2: both close it, the way chrome's launcher does. If only case 2
       fails, a parent's close of the child's socket end is what breaks
       descriptor passing - not SCM_RIGHTS itself. */
    int a = run_case(0);
    printf("GSHM2: case without closes -> %d\n", a); fflush(stdout);
    int b = run_case(1);
    printf("GSHM2: case with closes -> %d\n", b); fflush(stdout);
    if (a == 0 && b == 0) return 141;
    return 100 + (a ? 1 : 0) * 10 + (b ? 1 : 0);
}

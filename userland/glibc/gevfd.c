#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <stdint.h>
#include <sys/eventfd.h>
#include <sys/socket.h>
#include <sys/wait.h>

/* An eventfd exists so one party can WAKE another. Chrome hands children an
   eventfd over SCM_RIGHTS and signals them through it, so a duplicate must
   share the counter with the original. A duplicate with a counter of its own
   loses every wakeup, silently.

   Exit 153 = a write in the child was read by the parent through its own
   descriptor, and a write in the parent was read by the child. */

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

int main(void) {
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) {
        printf("GEVFD: socketpair FAILED\n"); fflush(stdout); return 1;
    }
    int ev = eventfd(0, 0);
    if (ev < 0) { printf("GEVFD: eventfd FAILED\n"); fflush(stdout); return 2; }

    pid_t kid = fork();
    if (kid < 0) { printf("GEVFD: fork FAILED\n"); fflush(stdout); return 3; }
    if (kid == 0) {
        int rfd = recv_fd(sv[1]);
        if (rfd < 0) { printf("GEVFD: child got no descriptor\n"); fflush(stdout); _exit(11); }
        uint64_t one = 7;
        if (write(rfd, &one, 8) != 8) { printf("GEVFD: child write FAILED\n"); fflush(stdout); _exit(12); }
        char ack = 'C';
        if (write(sv[1], &ack, 1) != 1) _exit(13);
        /* Now the other direction: the parent writes, the child must see it. */
        char go = 0;
        if (read(sv[1], &go, 1) != 1) _exit(14);
        uint64_t v = 0;
        if (read(rfd, &v, 8) != 8) { printf("GEVFD: child read FAILED\n"); fflush(stdout); _exit(15); }
        printf("GEVFD: child read %llu from the parent (want 9)\n", (unsigned long long)v);
        fflush(stdout);
        _exit(v == 9 ? 0 : 16);
    }

    if (send_fd(sv[0], ev) != 0) { printf("GEVFD: send_fd FAILED\n"); fflush(stdout); return 4; }
    char ack = 0;
    if (read(sv[0], &ack, 1) != 1) { printf("GEVFD: no ack\n"); fflush(stdout); return 5; }
    uint64_t v = 0;
    if (read(ev, &v, 8) != 8) { printf("GEVFD: parent read FAILED\n"); fflush(stdout); return 6; }
    printf("GEVFD: parent read %llu from the child (want 7)\n", (unsigned long long)v);
    fflush(stdout);
    if (v != 7) return 7;
    uint64_t nine = 9;
    if (write(ev, &nine, 8) != 8) { printf("GEVFD: parent write FAILED\n"); fflush(stdout); return 8; }
    char go = 'G';
    if (write(sv[0], &go, 1) != 1) return 9;
    int st = 0;
    if (waitpid(kid, &st, 0) != kid) { printf("GEVFD: waitpid FAILED\n"); fflush(stdout); return 10; }
    if (!WIFEXITED(st) || WEXITSTATUS(st) != 0) {
        printf("GEVFD: child exit=%d\n", WIFEXITED(st) ? WEXITSTATUS(st) : -1);
        fflush(stdout); return 20;
    }
    printf("GEVFD: an eventfd passed to another process wakes both ways -> PASS\n");
    fflush(stdout);
    return 153;
}

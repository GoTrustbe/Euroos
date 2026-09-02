#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>
#include <sys/socket.h>
#include <sys/wait.h>

/* CHILD-TO-CHILD, the way Mojo introduces two peers. A broker (the parent)
   makes a socketpair and hands ONE end to each of two children, which then
   talk to each other directly - neither created the socket, and neither is
   the process the descriptor was made in.

   Chrome does exactly this: its tracing service lives in a utility process,
   so a renderer that must ack BeginTracing has to reach a sibling, not its
   parent. On this kernel two of chrome's three children never ack.

   Exit 147 = the siblings exchanged bytes in both directions. */

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
    int toA[2], toB[2], peer[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, toA) || socketpair(AF_UNIX, SOCK_STREAM, 0, toB)
        || socketpair(AF_UNIX, SOCK_STREAM, 0, peer)) {
        printf("GSCM3: socketpair FAILED\n"); fflush(stdout); return 1;
    }

    pid_t a = fork();
    if (a < 0) { printf("GSCM3: fork A FAILED\n"); fflush(stdout); return 2; }
    if (a == 0) {
        int s = recv_fd(toA[1]);
        if (s < 0) { printf("GSCM3: A got no descriptor\n"); fflush(stdout); _exit(11); }
        if (write(s, "A", 1) != 1) { printf("GSCM3: A write FAILED\n"); fflush(stdout); _exit(12); }
        char c = 0;
        if (read(s, &c, 1) != 1 || c != 'B') { printf("GSCM3: A did not hear B (c=%d)\n", c); fflush(stdout); _exit(13); }
        printf("GSCM3: A heard its sibling\n"); fflush(stdout);
        _exit(0);
    }

    pid_t b = fork();
    if (b < 0) { printf("GSCM3: fork B FAILED\n"); fflush(stdout); return 3; }
    if (b == 0) {
        int s = recv_fd(toB[1]);
        if (s < 0) { printf("GSCM3: B got no descriptor\n"); fflush(stdout); _exit(21); }
        char c = 0;
        if (read(s, &c, 1) != 1 || c != 'A') { printf("GSCM3: B did not hear A (c=%d)\n", c); fflush(stdout); _exit(22); }
        if (write(s, "B", 1) != 1) { printf("GSCM3: B write FAILED\n"); fflush(stdout); _exit(23); }
        printf("GSCM3: B heard its sibling\n"); fflush(stdout);
        _exit(0);
    }

    /* The broker hands each child one end of a socket neither of them made. */
    if (send_fd(toA[0], peer[0]) != 0) { printf("GSCM3: hand to A FAILED\n"); fflush(stdout); return 4; }
    if (send_fd(toB[0], peer[1]) != 0) { printf("GSCM3: hand to B FAILED\n"); fflush(stdout); return 5; }

    int sa = 0, sb = 0;
    if (waitpid(a, &sa, 0) != a || waitpid(b, &sb, 0) != b) {
        printf("GSCM3: waitpid FAILED\n"); fflush(stdout); return 6;
    }
    int ea = WIFEXITED(sa) ? WEXITSTATUS(sa) : -1;
    int eb = WIFEXITED(sb) ? WEXITSTATUS(sb) : -1;
    printf("GSCM3: A=%d B=%d\n", ea, eb); fflush(stdout);
    if (ea || eb) return 7;
    printf("GSCM3: two children talked over a socket the broker handed them -> PASS\n");
    fflush(stdout);
    return 147;
}

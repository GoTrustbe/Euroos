#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/socket.h>

/* SCM_RIGHTS test: a descriptor sent over a socketpair must ARRIVE. Dropping the
   control message is silent — the bytes get through, the handle does not, and the
   receiver waits forever for a resource it was never given. Mojo passes handles
   exactly this way, including while chrome produces a frame.

   Exit 151 = the receiver can read through the descriptor it was handed. */
int main(void) {
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) {
        printf("GSCM: socketpair FAILED\n"); fflush(stdout); return 1;
    }

    int f = open("/tmp/gscm_payload", O_RDWR | O_CREAT | O_TRUNC, 0600);
    if (f < 0 || write(f, "handed over", 11) != 11) {
        printf("GSCM: payload FAILED\n"); fflush(stdout); return 2;
    }

    char body[1] = { 'x' };
    struct iovec iov = { .iov_base = body, .iov_len = 1 };
    char cbuf[CMSG_SPACE(sizeof(int))];
    memset(cbuf, 0, sizeof cbuf);
    struct msghdr msg = { .msg_iov = &iov, .msg_iovlen = 1,
                          .msg_control = cbuf, .msg_controllen = sizeof cbuf };
    struct cmsghdr *c = CMSG_FIRSTHDR(&msg);
    c->cmsg_level = SOL_SOCKET; c->cmsg_type = SCM_RIGHTS; c->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(c), &f, sizeof(int));
    if (sendmsg(sv[0], &msg, 0) < 0) { printf("GSCM: sendmsg FAILED\n"); fflush(stdout); return 3; }

    char rbody[1] = { 0 };
    struct iovec riov = { .iov_base = rbody, .iov_len = 1 };
    char rcbuf[CMSG_SPACE(sizeof(int))];
    memset(rcbuf, 0, sizeof rcbuf);
    struct msghdr rmsg = { .msg_iov = &riov, .msg_iovlen = 1,
                           .msg_control = rcbuf, .msg_controllen = sizeof rcbuf };
    if (recvmsg(sv[1], &rmsg, 0) < 0) { printf("GSCM: recvmsg FAILED\n"); fflush(stdout); return 4; }

    struct cmsghdr *rc = CMSG_FIRSTHDR(&rmsg);
    if (!rc || rc->cmsg_level != SOL_SOCKET || rc->cmsg_type != SCM_RIGHTS) {
        printf("GSCM: no descriptor arrived (control message dropped) FAILED\n");
        fflush(stdout); return 5;
    }
    int got; memcpy(&got, CMSG_DATA(rc), sizeof(int));

    char buf[16]; memset(buf, 0, sizeof buf);
    if (pread(got, buf, 11, 0) != 11 || memcmp(buf, "handed over", 11) != 0) {
        printf("GSCM: the received descriptor reads '%s' FAILED\n", buf);
        fflush(stdout); return 6;
    }

    printf("GSCM: descriptor passed over a socket and usable on the other side -> PASS\n");
    fflush(stdout);
    return 151;
}

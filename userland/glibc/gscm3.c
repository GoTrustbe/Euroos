#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <sys/mman.h>
#include <sys/eventfd.h>
#include <sys/epoll.h>
#include <stdint.h>

/* CHILD-TO-CHILD, the way Mojo introduces two peers. A broker (the parent)
   makes a socketpair and hands ONE end to each of two children, which then
   talk to each other directly - neither created the socket, and neither is
   the process the descriptor was made in.

   Chrome does exactly this: its tracing service lives in a utility process,
   so a renderer that must ack BeginTracing has to reach a sibling, not its
   parent. On this kernel two of chrome's three children never ack.

   Then the harder half, and the one chrome's IPC actually needs: child A
   CREATES shared memory and the broker RELAYS that descriptor to child B,
   which maps it and reads what A wrote. Neither the creator nor the reader
   is the process the descriptor travelled through.

   And a third round, because chrome needs that too: A makes an EVENTFD, the
   broker relays it to B, A signals through it and B waits for it with epoll.
   A renderer here receives nothing from a sibling process - not its trace
   buffer, not a resource body - while everything with the browser works, so
   the sibling primitives are worth testing one by one.

   Exit 147 = siblings exchanged bytes both ways, the relayed shared memory
   carried A's bytes to B, and B was woken through A's eventfd. */

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
        /* A makes shared memory and hands it up to the broker to relay to B. */
        int mfd = memfd_create("euro-relay", 0);
        if (mfd < 0 || ftruncate(mfd, 65536) != 0) { printf("GSCM3: A memfd FAILED\n"); fflush(stdout); _exit(14); }
        unsigned char *p = mmap(NULL, 65536, PROT_READ | PROT_WRITE, MAP_SHARED, mfd, 0);
        if (p == MAP_FAILED) { printf("GSCM3: A map FAILED\n"); fflush(stdout); _exit(15); }
        for (int i = 0; i < 16; i++) p[i * 4096] = (unsigned char)(0xC0 + i);
        if (send_fd(toA[1], mfd) != 0) { printf("GSCM3: A relay-send FAILED\n"); fflush(stdout); _exit(16); }
        /* Round three: an eventfd of A's, relayed to B, signalled by A. */
        int ev = eventfd(0, 0);
        if (ev < 0) { printf("GSCM3: A eventfd FAILED\n"); fflush(stdout); _exit(17); }
        if (send_fd(toA[1], ev) != 0) { printf("GSCM3: A eventfd-relay FAILED\n"); fflush(stdout); _exit(18); }
        char go = 0;
        if (read(toA[1], &go, 1) != 1) _exit(19);   /* B is watching */
        uint64_t one = 1;
        if (write(ev, &one, 8) != 8) { printf("GSCM3: A signal FAILED\n"); fflush(stdout); _exit(20); }
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
        int mfd = recv_fd(toB[1]);
        if (mfd < 0) { printf("GSCM3: B got no relayed memfd\n"); fflush(stdout); _exit(24); }
        unsigned char *p = mmap(NULL, 65536, PROT_READ | PROT_WRITE, MAP_SHARED, mfd, 0);
        if (p == MAP_FAILED) { printf("GSCM3: B map FAILED\n"); fflush(stdout); _exit(25); }
        int bad = 0;
        for (int i = 0; i < 16; i++) if (p[i * 4096] != (unsigned char)(0xC0 + i)) bad++;
        printf("GSCM3: B read relayed shared memory, mismatched=%d of 16\n", bad);
        fflush(stdout);
        if (bad) _exit(26);
        int ev = recv_fd(toB[1]);
        if (ev < 0) { printf("GSCM3: B got no relayed eventfd\n"); fflush(stdout); _exit(27); }
        int ep = epoll_create1(0);
        struct epoll_event want = { .events = EPOLLIN, .data = { .fd = ev } };
        if (ep < 0 || epoll_ctl(ep, EPOLL_CTL_ADD, ev, &want) != 0) {
            printf("GSCM3: B epoll setup FAILED\n"); fflush(stdout); _exit(28);
        }
        char ready = 'R';
        if (write(toB[1], &ready, 1) != 1) _exit(29);  /* tell the broker: watching */
        struct epoll_event got;
        int n = epoll_wait(ep, &got, 1, 20000);
        if (n != 1) { printf("GSCM3: B never woken by its sibling (epoll=%d)\n", n); fflush(stdout); _exit(30); }
        uint64_t v = 0;
        if (read(ev, &v, 8) != 8 || v != 1) { printf("GSCM3: B read %llu\n", (unsigned long long)v); fflush(stdout); _exit(31); }
        printf("GSCM3: B was woken through its sibling eventfd\n");
        fflush(stdout);
        _exit(0);
    }

    /* The broker hands each child one end of a socket neither of them made. */
    if (send_fd(toA[0], peer[0]) != 0) { printf("GSCM3: hand to A FAILED\n"); fflush(stdout); return 4; }
    if (send_fd(toB[0], peer[1]) != 0) { printf("GSCM3: hand to B FAILED\n"); fflush(stdout); return 5; }

    /* Relay A's shared memory to B: the broker never made it and never maps it. */
    int relayed = recv_fd(toA[0]);
    if (relayed < 0) { printf("GSCM3: broker got no memfd from A\n"); fflush(stdout); return 8; }
    if (send_fd(toB[0], relayed) != 0) { printf("GSCM3: relay to B FAILED\n"); fflush(stdout); return 9; }

    /* Relay A's eventfd to B, then pass B's "watching" signal back to A. */
    int evfd = recv_fd(toA[0]);
    if (evfd < 0) { printf("GSCM3: broker got no eventfd from A\n"); fflush(stdout); return 11; }
    if (send_fd(toB[0], evfd) != 0) { printf("GSCM3: eventfd relay to B FAILED\n"); fflush(stdout); return 12; }
    char ready = 0;
    if (read(toB[0], &ready, 1) != 1) { printf("GSCM3: B never reported watching\n"); fflush(stdout); return 13; }
    if (write(toA[0], &ready, 1) != 1) return 14;

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

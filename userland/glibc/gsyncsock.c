/* gsyncsock: [syncsock] (BUG-170) — the audio stream of Chromium between two
 * processes. The browser (parent) makes a socketpair and a shared-memory region,
 * hands one end and the region to the renderer (a fork child, over an existing
 * channel socket, SCM_RIGHTS) and closes its copies, as base::CancelableSyncSocket
 * and the audio service do. Then the browser writes 4-byte signals on its end
 * (non-blocking, as CancelableSyncSocket::Send) and the renderer answers on the
 * end it received. Every step is one line "ok"/"FAIL"; exit 77 = all passed. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

static int fails = 0, checks = 0;
static void check(int ok, const char *what, long v) {
    checks++;
    if (!ok) fails++;
    printf("GSYNCSOCK: %s %s (%ld)\n", ok ? "ok  " : "FAIL", what, v);
    fflush(stdout);
}

static int send_fds(int chan, int a, int b) {
    char byte = 'S';
    struct iovec io = {&byte, 1};
    union { char buf[CMSG_SPACE(2 * sizeof(int))]; struct cmsghdr al; } u;
    struct msghdr m = {0};
    m.msg_iov = &io; m.msg_iovlen = 1;
    m.msg_control = u.buf; m.msg_controllen = sizeof u.buf;
    struct cmsghdr *c = CMSG_FIRSTHDR(&m);
    c->cmsg_level = SOL_SOCKET; c->cmsg_type = SCM_RIGHTS; c->cmsg_len = CMSG_LEN(2 * sizeof(int));
    int fds[2] = {a, b};
    memcpy(CMSG_DATA(c), fds, sizeof fds);
    return (int)sendmsg(chan, &m, 0);
}

static int recv_fds(int chan, int *a, int *b) {
    char byte;
    struct iovec io = {&byte, 1};
    union { char buf[CMSG_SPACE(2 * sizeof(int))]; struct cmsghdr al; } u;
    struct msghdr m = {0};
    m.msg_iov = &io; m.msg_iovlen = 1;
    m.msg_control = u.buf; m.msg_controllen = sizeof u.buf;
    int n = (int)recvmsg(chan, &m, 0);
    int e = errno;
    struct cmsghdr *c = CMSG_FIRSTHDR(&m);
    if (n != 1 || !c || c->cmsg_type != SCM_RIGHTS) {
        printf("GSYNCSOCK: child: recvmsg = %d (errno %d), controllen %zu, cmsg %s\n", n, n < 0 ? e : 0,
               (size_t)m.msg_controllen, c ? "present" : "absent");
        fflush(stdout);
        return -1;
    }
    int fds[2];
    memcpy(fds, CMSG_DATA(c), sizeof fds);
    *a = fds[0]; *b = fds[1];
    return 0;
}

static ssize_t send_nb(int fd, const void *p, size_t n) {
    /* base::CancelableSyncSocket::Send: non-blocking for the call */
    int fl = fcntl(fd, F_GETFL);
    fcntl(fd, F_SETFL, fl | O_NONBLOCK);
    ssize_t r = send(fd, p, n, MSG_NOSIGNAL);
    int e = errno;
    fcntl(fd, F_SETFL, fl);
    errno = e;
    return r;
}

int main(void) {
    signal(SIGPIPE, SIG_IGN);
    int chan[2];
    check(socketpair(AF_UNIX, SOCK_STREAM, 0, chan) == 0, "channel socketpair", 0);
    pid_t c = fork();
    if (c < 0) {
        printf("GSYNCSOCK: fork refused (errno %d): not run -> SKIP\n", errno);
        fflush(stdout);
        _exit(78);
    }
    if (c == 0) {
        /* renderer */
        close(chan[0]);
        int s = -1, shm = -1;
        if (recv_fds(chan[1], &s, &shm) != 0) _exit(10);
        uint32_t *mem = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, shm, 0);
        if (mem == MAP_FAILED) _exit(11);
        close(shm);
        for (int i = 0; i < 3; i++) {
            uint32_t v = 0;
            struct pollfd p = {s, POLLIN, 0};
            if (poll(&p, 1, 5000) != 1) _exit(12);
            if (read(s, &v, 4) != 4) _exit(13);
            mem[1 + i] = v * 10;          /* the "audio data" */
            uint32_t ack = v + 100;
            if (write(s, &ack, 4) != 4) _exit(14);
        }
        char b;
        if (read(s, &b, 1) != 0) _exit(15);   /* the browser closes: end of file */
        _exit(0);
    }
    /* browser */
    close(chan[1]);
    /* A non-blocking pair that is gone again: its numbers are free for the next one. */
    int old[2];
    if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK, 0, old) == 0) { close(old[0]); close(old[1]); }
    int pair[2];
    check(socketpair(AF_UNIX, SOCK_STREAM, 0, pair) == 0, "sync socketpair", 0);
    int fl0 = fcntl(pair[0], F_GETFL), fl1 = fcntl(pair[1], F_GETFL);
    check(!(fl0 & O_NONBLOCK) && !(fl1 & O_NONBLOCK), "a new socketpair is blocking (after a closed non-blocking one)", fl0 | fl1);
    int shm = memfd_create("audio", 0);
    check(shm >= 0 && ftruncate(shm, 4096) == 0, "shared memory", shm);
    uint32_t *mem = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, shm, 0);
    check(mem != MAP_FAILED, "browser maps the region", 0);
    check(send_fds(chan[0], pair[1], shm) == 1, "the foreign end and the region sent with SCM_RIGHTS", 0);
    /* the sender's copies go at once: the message holds them now */
    check(close(pair[1]) == 0 && close(shm) == 0, "the sender closes its copies", 0);
    for (int i = 0; i < 3; i++) {
        uint32_t v = (uint32_t)(i + 1);
        errno = 0;
        ssize_t r = send_nb(pair[0], &v, 4);
        check(r == 4, "non-blocking send of a 4-byte signal on the kept end", r < 0 ? -errno : r);
        if (r != 4) break;
        struct pollfd p = {pair[0], POLLIN, 0};
        int pr = poll(&p, 1, 5000);
        uint32_t ack = 0;
        ssize_t n = pr == 1 ? read(pair[0], &ack, 4) : -1;
        check(n == 4 && ack == v + 100, "the renderer answers on the end it received", n == 4 ? (long)ack : -errno);
        check(mem[1 + i] == v * 10, "its data is in the shared region", (long)mem[1 + i]);
    }
    close(pair[0]);
    int st = 0;
    waitpid(c, &st, 0);
    check(WIFEXITED(st) && WEXITSTATUS(st) == 0, "the renderer saw every signal and the end of file", st);
    printf("GSYNCSOCK: %d of %d -> %s\n", checks - fails, checks, fails ? "FAIL" : "PASS");
    fflush(stdout);
    _exit(fails ? 1 : 77);
}

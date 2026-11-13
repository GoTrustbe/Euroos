/* gslots: [churn-forkthreads] (BUG-140) — fork children that start threads and end
 * the way Chromium children end: exit_group from the main thread with its threads
 * still blocked, exit_group from a worker thread, and kill(SIGTERM) by the parent.
 * ROUNDS x (THREADS + 1) tasks is more than the kernel's 256 scheduler slots, so a
 * kernel that keeps the slot of any task of an ended child runs out of slots on
 * the way: pthread_create or fork answers EAGAIN. After the rounds the parent must
 * still create threads and fork a child that creates threads.
 * Exit 88 = PASS. Exit 89 = the first fork was refused for memory (no room for a
 * second arena on this machine): only the single-process part ran. */
#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define ROUNDS 40
#define THREADS 6

static pthread_mutex_t mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t never = PTHREAD_COND_INITIALIZER;
static int worker_exit_code = -1;
static volatile int ready_sent = 0;

/* Blocks for good: in a futex (most threads), or in nanosleep. Thread 0 of a
 * mode-2 child ends the whole child with exit_group once the child has told
 * the parent that all its threads exist. */
static void *blocker(void *arg) {
    long i = (long)arg;
    if (i == 0 && worker_exit_code >= 0) {
        struct timespec ts = {0, 2 * 1000 * 1000};
        while (!ready_sent) nanosleep(&ts, NULL);
        nanosleep(&ts, NULL);
        _exit(worker_exit_code);
    }
    if (i == 1) {
        for (;;) { struct timespec ts = {3600, 0}; nanosleep(&ts, NULL); }
    }
    pthread_mutex_lock(&mu);
    for (;;) pthread_cond_wait(&never, &mu);
    return NULL;
}

static void *adder(void *arg) { return (void *)((long)arg + 1); }

/* The child: THREADS threads, then end as `mode` says (0: exit_group from main
 * with its threads blocked; 1: wait to be killed; 2: a worker calls exit_group).
 * A thread that cannot be created: its error number goes to the parent, exit 100+i. */
static void child(int mode, int code, int ready_fd) {
    if (mode == 2) worker_exit_code = code;
    for (long i = 0; i < THREADS; i++) {
        pthread_t t;
        int e = pthread_create(&t, NULL, blocker, (void *)i);
        if (e) {
            char b = (char)e;
            if (write(ready_fd, &b, 1) < 0) {}
            _exit(100 + (int)i);
        }
    }
    char ok = 0;
    if (write(ready_fd, &ok, 1) < 0) {}
    ready_sent = 1;
    if (mode == 0) _exit(code);
    for (;;) { struct timespec ts = {3600, 0}; nanosleep(&ts, NULL); }
}

int main(void) {
    int bad = 0, done = 0, refused = 0;
    char why[256] = "";
    for (int r = 0; r < ROUNDS; r++) {
        int mode = r % 3, code = 10 + r % 50;
        int p[2];
        if (pipe(p)) { bad++; snprintf(why, sizeof why, "round %d: pipe errno %d", r, errno); break; }
        pid_t c = fork();
        if (c < 0) {
            int e = errno;
            close(p[0]); close(p[1]);
            if (r == 0 && e == ENOMEM) { refused = 1; break; }
            bad++; snprintf(why, sizeof why, "round %d: fork errno %d", r, e);
            break;
        }
        if (c == 0) { close(p[0]); child(mode, code, p[1]); }
        close(p[1]);
        char b = 1;
        ssize_t n = read(p[0], &b, 1);
        close(p[0]);
        if (n != 1 || b != 0) {
            bad++;
            snprintf(why, sizeof why, "round %d (mode %d): the child could not create its threads (pthread_create %d)", r, mode, n == 1 ? b : -1);
        }
        if (mode == 1) kill(c, SIGTERM);
        int st = 0;
        pid_t w = waitpid(c, &st, 0);
        int want_ok = mode == 1 ? (WIFSIGNALED(st) && WTERMSIG(st) == SIGTERM)
                                : (WIFEXITED(st) && WEXITSTATUS(st) == code);
        if (w != c || (!want_ok && b == 0)) {
            bad++;
            if (!why[0]) snprintf(why, sizeof why, "round %d (mode %d): waitpid %d status %#x", r, mode, (int)w, st);
        }
        if (bad) break;
        done++;
    }
    /* The parent itself: threads still come. */
    int tbad = 0;
    for (long i = 0; i < 8; i++) {
        pthread_t t; void *res = 0;
        int e = pthread_create(&t, NULL, adder, (void *)i);
        if (e || pthread_join(t, &res) || (long)res != i + 1) {
            tbad++;
            if (!why[0]) snprintf(why, sizeof why, "parent: pthread_create %d after %d rounds", e, done);
        }
    }
    bad += tbad;
    if (!refused && !bad) {
        /* One more child with threads after all rounds. */
        int p[2];
        if (pipe(p) == 0) {
            pid_t c = fork();
            if (c == 0) { close(p[0]); child(0, 7, p[1]); }
            close(p[1]);
            char b = 1;
            ssize_t n = c > 0 ? read(p[0], &b, 1) : -1;
            close(p[0]);
            int st = 0;
            if (c < 0 || n != 1 || b != 0 || waitpid(c, &st, 0) != c || !WIFEXITED(st) || WEXITSTATUS(st) != 7) {
                bad++;
                snprintf(why, sizeof why, "last child after %d rounds: fork %d, threads %d, status %#x", done, (int)c, b, st);
            }
        }
    }
    if (refused)
        printf("GSLOTS: fork refused for memory: parent threads %s -> %s\n", tbad ? "FAILED" : "ok", tbad ? "FAIL" : "SKIP");
    else
        printf("GSLOTS: %d of %d rounds (%d tasks of ended children), bad=%d%s%s -> %s\n", done, ROUNDS,
               done * (THREADS + 1), bad, why[0] ? ": " : "", why, bad == 0 ? "PASS" : "FAIL");
    fflush(stdout);
    _exit(bad ? 1 : refused ? 89 : 88);
}

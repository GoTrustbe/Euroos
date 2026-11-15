/* gkillsmp: [kill-smp] (docs/design/SMP-SCHEDULER.md, step S4). argv[1] rounds. Each
 * round forks a child that starts 8 threads spinning in user mode (the kernel
 * places them on the application processors) and reports when all 8 spin. Even
 * rounds: the child's main thread ends the process with exit_group while its
 * threads spin; odd rounds: the parent sends SIGKILL. The parent reaps the child
 * every time. Prints "KILLSMP rounds=N exit_group=E sigkill=K reaped=R" and
 * "KILLSMP all ok" / "KILLSMP FAILED"; exit 77 ok, 1 failed. */
#define _GNU_SOURCE
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static _Atomic int running;
static volatile unsigned long spin[8];

static void *spinner(void *a) {
    long i = (long)a;
    atomic_fetch_add(&running, 1);
    for (;;) spin[i]++;
    return NULL;
}

int main(int argc, char **argv) {
    long rounds = argc > 1 ? atol(argv[1]) : 1000;
    long eg = 0, kills = 0, reaped = 0, bad = 0;
    for (long r = 0; r < rounds; r++) {
        int fds[2];
        if (pipe(fds) != 0) { bad++; break; }
        pid_t c = fork();
        if (c < 0) { bad++; break; }
        if (c == 0) {
            pthread_t t[8];
            for (long i = 0; i < 8; i++) pthread_create(&t[i], NULL, spinner, (void *)i);
            while (atomic_load(&running) < 8) {}
            if (write(fds[1], "s", 1) != 1) _exit(5);
            if (r % 2 == 0) {
                for (volatile int k = 0; k < 100000; k++) {}
                syscall(SYS_exit_group, 0); /* the threads still spin */
            }
            for (;;) pause();
        }
        char ch;
        if (read(fds[0], &ch, 1) != 1) bad++;
        if (r % 2 == 1) {
            kill(c, SIGKILL);
            kills++;
        } else {
            eg++;
        }
        int st = 0;
        if (waitpid(c, &st, 0) == c) reaped++;
        close(fds[0]);
        close(fds[1]);
    }
    int ok = !bad && reaped == rounds;
    printf("KILLSMP rounds=%ld exit_group=%ld sigkill=%ld reaped=%ld\nKILLSMP %s\n", rounds, eg, kills, reaped, ok ? "all ok" : "FAILED");
    fflush(stdout);
    return ok ? 77 : 1;
}

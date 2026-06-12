/* EuroOS — pthread_mutex onder CONTENTIE. Twee threads verhogen 15000x dezelfde
 * teller, elk onder een pthread_mutex. Werkt het slot (en de blokkerende futex
 * eronder) correct, dan is de eindstand EXACT 30000 — geen verloren updates door
 * een race. Een gewone, ongewijzigde musl-binary. */
#include <pthread.h>
#include <unistd.h>

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static volatile long counter = 0;

static void *worker(void *arg) {
    (void)arg;
    for (int i = 0; i < 15000; i++) {
        pthread_mutex_lock(&lock);
        counter++;
        pthread_mutex_unlock(&lock);
    }
    return 0;
}

static int slen(const char *s) { int n = 0; while (s[n]) n++; return n; }
static void emit(const char *s) { write(1, s, slen(s)); }

int main(void) {
    emit("mmutex: 2 threads x 15000 onder pthread_mutex...\n");
    pthread_t t1, t2;
    pthread_create(&t1, 0, worker, 0);
    pthread_create(&t2, 0, worker, 0);
    pthread_join(t1, 0);
    pthread_join(t2, 0);

    char msg[80];
    int o = 0;
    const char *m = "  teller = ";
    while (*m) msg[o++] = *m++;
    long v = counter;
    char tmp[24];
    int n = 0;
    if (!v) tmp[n++] = '0';
    else while (v) { tmp[n++] = '0' + (v % 10); v /= 10; }
    for (int i = 0; i < n; i++) msg[o++] = tmp[n - 1 - i];
    const char *m2 = " (verwacht 30000 -> geen race)\n";
    while (*m2) msg[o++] = *m2++;
    write(1, msg, o);
    return 0;
}

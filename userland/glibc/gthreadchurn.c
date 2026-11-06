/* gthreadchurn: [churn-thread] — many pthread create/join cycles in ONE process.
 * Every cycle must join cleanly and compute the right sum; the kernel must give
 * back every thread kernel stack, futex entry and TLS block (checked from the
 * kernel side by churn.rs). Exit 77 = all cycles correct. */
#include <stdio.h>
#include <stdlib.h>
#include <pthread.h>
#include <unistd.h>
#define CYCLES 60
#define PER 4
static void *worker(void *arg){ long n=(long)arg, s=0; for(long i=0;i<n;i++) s+=i; return (void*)s; }
int main(void){
    long bad=0;
    for(int c=0;c<CYCLES;c++){
        pthread_t t[PER]; long a[PER]; long expect=0;
        for(int i=0;i<PER;i++){ a[i]=500+i*100+c; expect+=a[i]*(a[i]-1)/2; }
        for(int i=0;i<PER;i++) if(pthread_create(&t[i],NULL,worker,(void*)a[i])){ bad++; a[i]=0; }
        long tot=0;
        for(int i=0;i<PER;i++){ if(!a[i]) continue; void*r=0; if(pthread_join(t[i],&r)) bad++; else tot+=(long)r; }
        if(tot!=expect) bad++;
    }
    printf("GTHREADCHURN: %d cycles x %d threads, bad=%ld -> %s\n", CYCLES, PER, bad, bad==0?"PASS":"FAIL"); fflush(stdout);
    _exit(bad==0?77:1);
}

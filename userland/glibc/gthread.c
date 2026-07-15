#include <stdio.h>
#include <stdlib.h>
#include <pthread.h>
#include <unistd.h>
static void *worker(void *arg){
    long n=(long)arg, s=0; for(long i=0;i<n;i++) s+=i;
    long *r=malloc(sizeof(long)); *r=s; return r;
}
int main(void){
    printf("GTHREAD: creating 3 pthreads\n"); fflush(stdout);
    pthread_t t[3]; long a[3]={1000,2000,3000};
    for(int i=0;i<3;i++) pthread_create(&t[i],NULL,worker,(void*)a[i]);
    long tot=0;
    for(int i=0;i<3;i++){ void*r; pthread_join(t[i],&r); tot+=*(long*)r; free(r); }
    printf("GTHREAD: 3 threads joined, total=%ld (expect 6497500)\n", tot); fflush(stdout);
    _exit(88);
}

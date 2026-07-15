#include <stdio.h>
#include <stdlib.h>
#include <pthread.h>
#include <unistd.h>
#define CAP 8
#define NITEMS 200          /* per producer */
#define NPROD 2
#define NCONS 2
static int q[CAP], head, tail, count;
static pthread_mutex_t m = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t not_full = PTHREAD_COND_INITIALIZER;
static pthread_cond_t not_empty = PTHREAD_COND_INITIALIZER;
static long produced_sum, consumed_sum;
static int producers_done;

static void *producer(void *arg){
    long base=(long)arg;
    for(int i=0;i<NITEMS;i++){
        int v=(int)(base+i);
        pthread_mutex_lock(&m);
        while(count==CAP) pthread_cond_wait(&not_full,&m);
        q[tail]=v; tail=(tail+1)%CAP; count++;
        produced_sum+=v;
        pthread_cond_signal(&not_empty);
        pthread_mutex_unlock(&m);
    }
    pthread_mutex_lock(&m);
    if(++producers_done==NPROD) pthread_cond_broadcast(&not_empty);
    pthread_mutex_unlock(&m);
    return NULL;
}
static void *consumer(void *arg){
    (void)arg;
    for(;;){
        pthread_mutex_lock(&m);
        while(count==0 && producers_done<NPROD) pthread_cond_wait(&not_empty,&m);
        if(count==0 && producers_done==NPROD){ pthread_mutex_unlock(&m); break; }
        int v=q[head]; head=(head+1)%CAP; count--;
        consumed_sum+=v;
        pthread_cond_signal(&not_full);
        pthread_mutex_unlock(&m);
    }
    return NULL;
}
int main(void){
    printf("GSYNC: %d producers + %d consumers, bounded queue (mutex+condvar)\n",NPROD,NCONS);
    fflush(stdout);
    pthread_t p[NPROD], c[NCONS];
    for(int i=0;i<NPROD;i++) pthread_create(&p[i],NULL,producer,(void*)(long)(i*100000));
    for(int i=0;i<NCONS;i++) pthread_create(&c[i],NULL,consumer,NULL);
    for(int i=0;i<NPROD;i++) pthread_join(p[i],NULL);
    /* wake any consumer still waiting, then join */
    pthread_mutex_lock(&m); pthread_cond_broadcast(&not_empty); pthread_mutex_unlock(&m);
    for(int i=0;i<NCONS;i++) pthread_join(c[i],NULL);
    int ok = (produced_sum==consumed_sum);
    printf("GSYNC: produced=%ld consumed=%ld -> %s\n",produced_sum,consumed_sum,ok?"PASS":"FAIL");
    fflush(stdout);
    _exit(ok?99:1);
}

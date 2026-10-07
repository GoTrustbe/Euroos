/* gforkchurn: [churn-fork] — fork/exit/wait cycles. Every child must be reaped
 * with the status it exited with (no zombies, no lost children, pids reusable).
 * Exit 66 = all cycles correct. */
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <sys/wait.h>
#define CYCLES 6
int main(void){
    int bad=0;
    for(int c=0;c<CYCLES;c++){
        pid_t p=fork();
        if(p<0){ bad++; continue; }
        if(p==0){ _exit((c%50)+1); }
        int st=0; pid_t w=waitpid(p,&st,0);
        if(w!=p || !WIFEXITED(st) || WEXITSTATUS(st)!=(c%50)+1) bad++;
    }
    /* a second reap must find nothing left */
    int st=0; if(waitpid(-1,&st,WNOHANG)>0) bad++;
    printf("GFORKCHURN: %d fork/wait cycles, bad=%d -> %s\n", CYCLES, bad, bad==0?"PASS":"FAIL"); fflush(stdout);
    _exit(bad==0?66:1);
}

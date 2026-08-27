#include <stdio.h>
#include <math.h>
#include <dlfcn.h>
#include <string.h>
#include <unistd.h>
int main(void){
    /* libm: force real dynamic calls (volatile args defeat const-folding) */
    volatile double x=2.0, y=10.0;
    double s=sqrt(x), p=pow(x,y), l=log(y), c=cos(x);
    printf("GMATH: sqrt(2)=%.5f pow(2,10)=%.1f log(10)=%.5f cos(2)=%.5f\n", s,p,l,c);
    /* libdl: open libm at runtime and call a symbol by name */
    void *h = dlopen("libm.so.6", RTLD_NOW);
    if(!h){ printf("GMATH: dlopen(libm) FAILED: %s\n", dlerror()); return 1; }
    double (*fn)(double) = (double(*)(double))dlsym(h, "sin");
    if(!fn){ printf("GMATH: dlsym(sin) FAILED\n"); return 1; }
    double si = fn(x);
    int ok = (si>0.9 && si<0.91);
    printf("GMATH: dlsym sin(2)=%.5f -> %s\n", si, ok ? "PASS":"FAIL");
    dlclose(h);
    fflush(stdout); /* _exit() does NOT flush glibc's stdio buffers */
    _exit(ok ? 77 : 1);
}

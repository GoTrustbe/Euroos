#include <stdio.h>
#include <sys/mman.h>
#include <stdint.h>
#include <unistd.h>
int main(void){
    size_t GB = 1UL<<30;
    size_t SIZE = 4*GB;                 /* reserve 4 GiB virtual — far beyond RAM */
    unsigned char *p = mmap(NULL, SIZE, PROT_READ|PROT_WRITE,
                            MAP_ANONYMOUS|MAP_PRIVATE, -1, 0);
    if(p==MAP_FAILED){ printf("GSPARSE: mmap(4GiB) FAILED\n"); fflush(stdout); return 1; }
    int N=1024;
    size_t stride = SIZE/N;             /* one touch every 16 MiB */
    for(int i=0;i<N;i++) p[(size_t)i*stride] = (unsigned char)(i*7+1);
    int ok=0;
    for(int i=0;i<N;i++) if(p[(size_t)i*stride]==(unsigned char)(i*7+1)) ok++;
    printf("GSPARSE: reserved %zu MiB virtual, touched %d pages, %d/%d verified -> %s\n",
           SIZE/(1024*1024), N, ok, N, ok==N?"PASS":"FAIL");
    fflush(stdout);
    _exit(ok==N?123:1);
}

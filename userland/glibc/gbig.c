#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <unistd.h>
int main(void){
    const size_t MB = 1024*1024;
    const size_t N = 200*MB;           /* 200 MiB heap allocation */
    const size_t PS = 4096;
    unsigned char *buf = malloc(N);
    if(!buf){ printf("GBIG: malloc(%zu) FAILED\n", N); return 1; }
    /* Write a per-page fingerprint across the whole allocation, forcing every
       page to be real, distinct, writable memory. */
    size_t pages = N/PS;
    for(size_t p=0; p<pages; p++) buf[p*PS] = (unsigned char)((p*2654435761u) >> 24);
    /* Read it back and verify. */
    size_t ok=0;
    for(size_t p=0; p<pages; p++)
        if(buf[p*PS] == (unsigned char)((p*2654435761u) >> 24)) ok++;
    printf("GBIG: allocated %zu MiB, %zu/%zu pages verified -> %s\n",
           N/MB, ok, pages, ok==pages ? "PASS":"FAIL");
    fflush(stdout);
    free(buf);
    _exit(ok==pages ? 111 : 1);
}

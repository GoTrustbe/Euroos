/* Minimal zlib declarations (stable ABI). compress2/uncompress round-trip. */
#include <stdio.h>
#include <string.h>
#include <unistd.h>
typedef unsigned long uLong; typedef unsigned char Bytef; typedef unsigned int uInt;
extern int compress2(Bytef*, uLong*, const Bytef*, uLong, int);
extern int uncompress(Bytef*, uLong*, const Bytef*, uLong);
extern uLong compressBound(uLong);
extern const char* zlibVersion(void);
int main(void){
    char src[4096];
    for(int i=0;i<(int)sizeof(src);i++) src[i] = "EuroOS-"[i%7]; /* compressible */
    uLong slen = sizeof(src);
    uLong bound = compressBound(slen);
    static Bytef comp[8192]; uLong clen = bound;
    if(compress2(comp,&clen,(Bytef*)src,slen,9)!=0){ printf("GZLIB: compress FAILED\n"); return 1; }
    static char out[4096]; uLong olen = sizeof(out);
    if(uncompress((Bytef*)out,&olen,comp,clen)!=0){ printf("GZLIB: uncompress FAILED\n"); return 1; }
    int ok = (olen==slen) && (memcmp(out,src,slen)==0);
    printf("GZLIB: zlib %s | %lu bytes -> %lu compressed -> %lu back, roundtrip %s\n",
           zlibVersion(), slen, clen, olen, ok?"PASS":"FAIL");
    fflush(stdout);
    _exit(ok?33:1);
}

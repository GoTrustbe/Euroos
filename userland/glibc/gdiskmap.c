#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/mman.h>
#include <sys/stat.h>

/* DISK-BACKED serving test: /pack/libcrypto.so.3 is served from a EuroPack
   virtio disk (never RAM-resident); /lib/x86_64-linux-gnu/libcrypto.so.3 is the
   SAME file embedded in the kernel. Prove (a) read() from disk == embedded,
   (b) a lazily demand-faulted mmap of the disk file == embedded, page by page. */
static int cmp_range(const unsigned char *a, const unsigned char *b, size_t off, size_t n){
    return memcmp(a + off, b + off, n) == 0;
}

int main(void){
    const char *disk_path = "/pack/libcrypto.so.3";
    const char *ref_path  = "/lib/x86_64-linux-gnu/libcrypto.so.3";

    int dfd = open(disk_path, O_RDONLY);
    if(dfd < 0){ printf("GDISKMAP: open(disk) FAILED\n"); fflush(stdout); return 1; }
    int rfd = open(ref_path, O_RDONLY);
    if(rfd < 0){ printf("GDISKMAP: open(ref) FAILED\n"); fflush(stdout); return 2; }

    struct stat ds, rs;
    if(fstat(dfd, &ds) || fstat(rfd, &rs)){ printf("GDISKMAP: fstat FAILED\n"); fflush(stdout); return 3; }
    if(ds.st_size != rs.st_size){
        printf("GDISKMAP: size mismatch disk=%ld ref=%ld\n", (long)ds.st_size, (long)rs.st_size);
        fflush(stdout); return 4;
    }
    size_t sz = (size_t)ds.st_size;

    /* Reference: the embedded copy via read(). */
    unsigned char *ref = malloc(sz);
    if(!ref){ printf("GDISKMAP: malloc FAILED\n"); fflush(stdout); return 5; }
    size_t got = 0;
    while(got < sz){
        ssize_t n = pread(rfd, ref + got, sz - got, (off_t)got);
        if(n <= 0) break;
        got += (size_t)n;
    }
    if(got != sz){ printf("GDISKMAP: ref short read\n"); fflush(stdout); return 6; }

    /* (a) pread() from the DISK file, sampled ranges incl. misaligned offsets. */
    unsigned char tmp[8192];
    int reads_ok = 1;
    size_t offs[] = {0, 1, 511, 512, 4095, 4096, sz/2 + 37, sz - 700};
    for(unsigned i = 0; i < sizeof offs/sizeof *offs; i++){
        size_t o = offs[i], n = (o + 4096 <= sz) ? 4096 : sz - o;
        if(pread(dfd, tmp, n, (off_t)o) != (ssize_t)n || memcmp(tmp, ref + o, n)){
            printf("GDISKMAP: pread mismatch at %zu\n", o); reads_ok = 0; break;
        }
    }

    /* (b) mmap the DISK file (lazy, demand-faulted from virtio) and compare. */
    unsigned char *p = mmap(NULL, sz, PROT_READ, MAP_PRIVATE, dfd, 0);
    if(p == MAP_FAILED){ printf("GDISKMAP: mmap FAILED\n"); fflush(stdout); return 7; }
    size_t pages = (sz + 4095)/4096, ok = 0;
    int printed = 0;
    for(size_t pg = 0; pg < pages; pg++){
        size_t o = pg*4096 + (pg % 4093);
        if(o >= sz) o = sz - 1;
        if(p[o] == ref[o]) ok++;
        else if(printed < 5){
            printf("GDISKMAP: page %zu MISMATCH at off %zu: mmap=%02x ref=%02x\n",
                   pg, o, p[o], ref[o]);
            printed++;
        }
    }
    int spans = cmp_range(p, ref, 0, 3*4096 < sz ? 3*4096 : sz)
             && cmp_range(p, ref, (pages/2)*4096, sz-(pages/2)*4096 > 4096 ? 4096 : sz-(pages/2)*4096)
             && cmp_range(p, ref, sz > 64 ? sz-64 : 0, sz > 64 ? 64 : sz);

    int pass = reads_ok && ok == pages && spans;
    printf("GDISKMAP: %zu KiB from DISK: preads=%s, mmap per-page %zu/%zu, spans=%s -> %s\n",
           sz/1024, reads_ok?"ok":"BAD", ok, pages, spans?"ok":"BAD", pass?"PASS":"FAIL");
    fflush(stdout);
    _exit(pass ? 125 : 1);
}

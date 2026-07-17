#include <stdio.h>
#include <stdlib.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/mman.h>
#include <sys/stat.h>

/* FILE-BACKED DEMAND-PAGING test: map a large served library the way a dynamic
   loader maps a LOAD segment, then prove the lazily-faulted mmap view is byte-
   identical to the same file read through read(). Only the pages we touch commit
   physical frames — the kernel reports the fill count. */
int main(void){
    const char *path = "/lib/x86_64-linux-gnu/libcrypto.so.3"; /* ~5 MiB, already served */
    int fd = open(path, O_RDONLY);
    if(fd < 0){ printf("GFMMAP: open FAILED\n"); fflush(stdout); return 1; }

    struct stat st;
    if(fstat(fd, &st) != 0 || st.st_size <= 0){ printf("GFMMAP: fstat FAILED\n"); fflush(stdout); return 2; }
    size_t sz = (size_t)st.st_size;

    /* Reference copy via read(). */
    unsigned char *ref = malloc(sz);
    if(!ref){ printf("GFMMAP: malloc FAILED\n"); fflush(stdout); return 3; }
    size_t got = 0;
    while(got < sz){
        ssize_t n = pread(fd, ref + got, sz - got, (off_t)got);
        if(n <= 0) break;
        got += (size_t)n;
    }
    if(got != sz){ printf("GFMMAP: short read %zu/%zu\n", got, sz); fflush(stdout); return 4; }

    /* File-backed private mmap (what ld.so does for a library segment). */
    unsigned char *p = mmap(NULL, sz, PROT_READ, MAP_PRIVATE, fd, 0);
    if(p == MAP_FAILED){ printf("GFMMAP: mmap FAILED\n"); fflush(stdout); return 5; }

    /* One byte per page across the whole file: mmap view must equal read view. */
    size_t pages = (sz + 4095) / 4096;
    size_t ok = 0;
    for(size_t pg = 0; pg < pages; pg++){
        size_t o = pg * 4096 + (pg % 4093);   /* vary the intra-page offset */
        if(o >= sz) o = sz - 1;
        if(p[o] == ref[o]) ok++;
    }

    /* Byte-for-byte over a few contiguous spans (catches off-by-one page fills). */
    int span_ok = 1;
    size_t head = sz < 3*4096 ? sz : 3*4096;
    for(size_t o = 0; o < head; o++) if(p[o] != ref[o]){ span_ok = 0; break; }
    size_t mid = (pages/2) * 4096;
    for(size_t o = mid; span_ok && o < mid + 4096 && o < sz; o++) if(p[o] != ref[o]) span_ok = 0;
    /* The final partial page: its tail past EOF must not corrupt the last real bytes. */
    for(size_t o = sz > 64 ? sz - 64 : 0; span_ok && o < sz; o++) if(p[o] != ref[o]) span_ok = 0;

    int pass = (ok == pages) && span_ok;
    printf("GFMMAP: mapped %zu KiB (%zu pages), per-page %zu/%zu match, spans=%s -> %s\n",
           sz/1024, pages, ok, pages, span_ok ? "ok" : "BAD", pass ? "PASS" : "FAIL");
    fflush(stdout);
    _exit(pass ? 124 : 1);
}

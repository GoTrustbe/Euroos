#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/mman.h>

/* SHARED-MEMORY test: two MAP_SHARED mappings of ONE memfd must be the SAME
   memory, not two private copies. This is the contract every shared-memory user
   relies on: chrome's Mojo data pipes carry resource bodies (the HTML/JS of a
   page) through a memfd ring buffer that the producer and the consumer each map
   separately — even inside a single process. A kernel that answers mmap() with a
   private copy makes the reader see zeros: the page "loads" but its document is
   empty, with no error anywhere.

   Checks, in order:
     1. writes through mapping A are visible through mapping B,
     2. writes through B are visible through A (sharing is symmetric),
     3. a mapping made AFTER the write already sees the data,
     4. writes through the mapping are visible to read() on the fd.
   Exit 131 = all four hold. */
int main(void) {
    const size_t SZ = 65536;

    int fd = memfd_create("euroshm", MFD_CLOEXEC);
    if (fd < 0) { printf("GSHM: memfd_create FAILED\n"); fflush(stdout); return 1; }
    if (ftruncate(fd, (off_t)SZ) != 0) { printf("GSHM: ftruncate FAILED\n"); fflush(stdout); return 2; }

    unsigned char *a = mmap(NULL, SZ, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (a == MAP_FAILED) { printf("GSHM: mmap A FAILED\n"); fflush(stdout); return 3; }
    unsigned char *b = mmap(NULL, SZ, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (b == MAP_FAILED) { printf("GSHM: mmap B FAILED\n"); fflush(stdout); return 4; }

    /* 1. A -> B, spread across pages so a single-page alias can't fake it. */
    for (size_t i = 0; i < SZ; i += 4096) a[i] = (unsigned char)(0x40 + (i / 4096));
    size_t bad = 0;
    for (size_t i = 0; i < SZ; i += 4096)
        if (b[i] != (unsigned char)(0x40 + (i / 4096))) bad++;
    printf("GSHM: A->B pages mismatched=%zu of %zu\n", bad, SZ / 4096);
    if (bad) { fflush(stdout); return 5; }

    /* 2. B -> A (symmetric). */
    memcpy(b + 1024, "EuroOS shared memory", 20);
    if (memcmp(a + 1024, "EuroOS shared memory", 20) != 0) {
        printf("GSHM: B->A FAILED\n"); fflush(stdout); return 6;
    }

    /* 3. A mapping created after the writes must see them (not a stale copy). */
    unsigned char *c = mmap(NULL, SZ, PROT_READ, MAP_SHARED, fd, 0);
    if (c == MAP_FAILED) { printf("GSHM: mmap C FAILED\n"); fflush(stdout); return 7; }
    if (memcmp(c + 1024, "EuroOS shared memory", 20) != 0) {
        printf("GSHM: late mapping C sees stale data FAILED\n"); fflush(stdout); return 8;
    }

    /* 4. The fd itself must observe the writes (shared mapping == the file). */
    char buf[24];
    memset(buf, 0, sizeof buf);
    if (pread(fd, buf, 20, 1024) != 20 || memcmp(buf, "EuroOS shared memory", 20) != 0) {
        printf("GSHM: pread of the shared region FAILED (got '%.20s')\n", buf);
        fflush(stdout); return 9;
    }

    /* 5. A BIG shared region. Anything over a megabyte takes a different path in
          this kernel (its own address range per mapping, faulting onto the file's
          shared frames) than the small one above, and that is the path a browser's
          frame buffers and IPC buffers actually use. Two mappings must still be one
          memory, across many pages, in both directions. */
    const size_t BIG = 4u * 1024 * 1024;
    int big = memfd_create("eurobig", MFD_CLOEXEC);
    if (big < 0 || ftruncate(big, (off_t)BIG) != 0) {
        printf("GSHM: big memfd setup FAILED\n"); fflush(stdout); return 10;
    }
    unsigned char *x = mmap(NULL, BIG, PROT_READ | PROT_WRITE, MAP_SHARED, big, 0);
    unsigned char *y = mmap(NULL, BIG, PROT_READ | PROT_WRITE, MAP_SHARED, big, 0);
    if (x == MAP_FAILED || y == MAP_FAILED) { printf("GSHM: big mmap FAILED\n"); fflush(stdout); return 11; }
    size_t bad_big = 0;
    for (size_t o = 0; o < BIG; o += 4096) x[o] = (unsigned char)(1 + (o / 4096) % 251);
    for (size_t o = 0; o < BIG; o += 4096)
        if (y[o] != (unsigned char)(1 + (o / 4096) % 251)) bad_big++;
    printf("GSHM: big region %zu KiB, X->Y mismatched=%zu of %zu pages\n",
           BIG / 1024, bad_big, BIG / 4096);
    if (bad_big) { fflush(stdout); return 12; }
    memcpy(y + BIG - 4096 + 64, "far end shared", 14);      /* and the other way, last page */
    if (memcmp(x + BIG - 4096 + 64, "far end shared", 14) != 0) {
        printf("GSHM: big region Y->X FAILED\n"); fflush(stdout); return 13;
    }

    printf("GSHM: MAP_SHARED memfd truly shared (small + 4 MiB, both directions) -> PASS\n");
    fflush(stdout);
    return 131;
}

#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/mman.h>

/* UNLINKED-BUT-OPEN test: POSIX keeps a file alive as long as a descriptor is
   open, and unlinking one file must not disturb any OTHER open descriptor.

   This is not academic. The standard way to get anonymous shared memory on Linux
   is create -> unlink -> ftruncate -> mmap(MAP_SHARED), and that is exactly how
   chrome allocates the Mojo buffers that carry a page's bytes to the renderer. A
   VFS that drops the entry (and shifts the descriptors of every file after it)
   turns those buffers into someone else's data, and pages arrive EMPTY with no
   error anywhere.

   Exit 137 = an unlinked file keeps serving its fd, its neighbours are
   undisturbed, and an unlinked file still works as shared memory. */
int main(void) {
    char buf[16];

    int a = open("/tmp/gu_a", O_RDWR | O_CREAT | O_TRUNC, 0600);
    int b = open("/tmp/gu_b", O_RDWR | O_CREAT | O_TRUNC, 0600);
    if (a < 0 || b < 0) { printf("GUNLINK: open FAILED\n"); fflush(stdout); return 1; }
    if (write(a, "AAA", 3) != 3 || write(b, "BBB", 3) != 3) {
        printf("GUNLINK: write FAILED\n"); fflush(stdout); return 2;
    }

    if (unlink("/tmp/gu_a") != 0) { printf("GUNLINK: unlink FAILED\n"); fflush(stdout); return 3; }

    /* 1. The unlinked file is still readable through its open descriptor. */
    memset(buf, 0, sizeof buf);
    if (pread(a, buf, 3, 0) != 3 || memcmp(buf, "AAA", 3) != 0) {
        printf("GUNLINK: unlinked fd lost its data (got '%.3s') FAILED\n", buf);
        fflush(stdout); return 4;
    }
    /* 2. The OTHER descriptor must be untouched — a VFS that removes the entry
          and shifts indices hands this fd the wrong file. */
    memset(buf, 0, sizeof buf);
    if (pread(b, buf, 3, 0) != 3 || memcmp(buf, "BBB", 3) != 0) {
        printf("GUNLINK: neighbour fd now reads '%.3s' instead of BBB FAILED\n", buf);
        fflush(stdout); return 5;
    }
    /* 3. The path is really gone. */
    if (open("/tmp/gu_a", O_RDONLY) >= 0) {
        printf("GUNLINK: unlinked path still openable FAILED\n"); fflush(stdout); return 6;
    }

    /* 4. Anonymous shared memory, chrome's way: create, unlink, size, map twice. */
    int s = open("/tmp/gu_shm", O_RDWR | O_CREAT | O_EXCL, 0600);
    if (s < 0) { printf("GUNLINK: shm open FAILED\n"); fflush(stdout); return 7; }
    unlink("/tmp/gu_shm");
    if (ftruncate(s, 8192) != 0) { printf("GUNLINK: shm ftruncate FAILED\n"); fflush(stdout); return 8; }
    unsigned char *p = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_SHARED, s, 0);
    unsigned char *q = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_SHARED, s, 0);
    if (p == MAP_FAILED || q == MAP_FAILED) {
        printf("GUNLINK: shm mmap FAILED\n"); fflush(stdout); return 9;
    }
    memcpy(p + 4096, "EuroOS anon shm", 15);
    if (memcmp(q + 4096, "EuroOS anon shm", 15) != 0) {
        printf("GUNLINK: unlinked shared mapping is not shared FAILED\n"); fflush(stdout); return 10;
    }

    printf("GUNLINK: unlinked fd alive, neighbours intact, anon shm shared -> PASS\n");
    fflush(stdout);
    return 137;
}

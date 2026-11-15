/* EuroOS — where a Linux program's address space lies, once per start (BUG-088).
 *
 * glibc dynamic, PIE: gcc -O2 -o glibc/gaslr gaslr.c
 * Used by the [aslr] boot self-test (kernel/src/aslrtest.rs), which starts it
 * many times and counts, per region, the address bits that differ between
 * starts. One line:
 *   gaslr: exe=0x.. ldso=0x.. libc=0x.. stack=0x.. brk=0x.. mmap=0x.. mmapbig=0x..
 * exe: the address of main (the PIE base plus a constant); ldso: AT_BASE, where
 * the kernel put the interpreter; libc: the address of printf; stack: a local variable of main; brk: sbrk(0) before any
 * allocation of ours; mmap: a fresh 4 KiB anonymous mapping; mmapbig: a fresh
 * 1 MiB one (on EuroOS with demand paging these two come from different
 * regions). */
#include <stdio.h>
#include <stdint.h>
#include <sys/auxv.h>
#include <sys/mman.h>
#include <unistd.h>

int main(void) {
    volatile int local = 0;
    void *brk0 = sbrk(0);
    void *m = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    void *mb = mmap(NULL, 1 << 20, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    uintptr_t ldso = (uintptr_t)getauxval(AT_BASE);
    printf("gaslr: exe=%#lx ldso=%#lx libc=%#lx stack=%#lx brk=%#lx mmap=%#lx mmapbig=%#lx\n",
           (unsigned long)(uintptr_t)&main, (unsigned long)ldso, (unsigned long)(uintptr_t)&printf,
           (unsigned long)(uintptr_t)&local, (unsigned long)(uintptr_t)brk0,
           (unsigned long)(uintptr_t)m, (unsigned long)(uintptr_t)mb);
    return 0;
}

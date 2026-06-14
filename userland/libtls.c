/* EuroOS — standalone shared library with a THREAD-LOCAL variable (__thread).
 * With -ftls-model=initial-exec this generates an R_X86_64_TPOFF64 relocation against
 * `ctr`; the kernel ld.so patches the GOT slot with the TP offset. (Sprint 1 / H3.) */
__thread long ctr = 41;
long bump(void) { return ++ctr; }

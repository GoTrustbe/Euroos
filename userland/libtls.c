/* EuroOS — vrijstaande shared library met een THREAD-LOCAL variabele (__thread).
 * Met -ftls-model=initial-exec genereert dit een R_X86_64_TPOFF64-relocatie tegen
 * `ctr`; de kernel-ld.so patcht het GOT-slot met de TP-offset. (Sprint 1 / H3.) */
__thread long ctr = 41;
long bump(void) { return ++ctr; }

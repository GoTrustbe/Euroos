/* EuroOS 3C-3 — a tiny "libc-euro.so": a real shared library (ET_DYN) whose
 * exported symbol is resolved by a USERSPACE dynamic linker (ld-euro.so), not by
 * the kernel. Freestanding: no host libc. This stands in for libc.so in the
 * PT_INTERP → userspace-ld.so path. */

/* Exported: the dynamically-linked exe calls this through its PLT/GOT; the
 * userspace ld-euro.so fills the GOT slot with this address (R_X86_64_JUMP_SLOT). */
long euroc_answer(void) {
    return 42;
}

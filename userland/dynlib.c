/* EuroOS H3 — a REAL shared library (.so, ET_DYN). The kernel dynamic linker loads
 * this module alongside a dynamically-linked executable and resolves symbols from it
 * (R_X86_64_JUMP_SLOT). `euro_answer` is the exported symbol that the exe refers to via
 * its PLT/GOT. Freestanding: no libc needed. */

long euro_answer(void) {
    return 42;
}

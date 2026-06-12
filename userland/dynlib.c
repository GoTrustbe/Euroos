/* EuroOS H3 — een ECHTE shared library (.so, ET_DYN). De kernel-dynlinker laadt
 * deze module náást een dynamisch-gelinkte executable en resolved er symbolen uit
 * (R_X86_64_JUMP_SLOT). `euro_answer` is het geëxporteerde symbool waar de exe via
 * z'n PLT/GOT naar verwijst. Vrijstaand: geen libc nodig. */

long euro_answer(void) {
    return 42;
}

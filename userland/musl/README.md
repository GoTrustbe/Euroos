# musl test binaries kept in the tree

The build server has no musl compiler, and `userland/*.elf` is build output
that git ignores. A musl program a self-test embeds is therefore kept here as
a binary with its signature, like the glibc ones in `userland/glibc/`.

| binary | source | built with |
|---|---|---|
| `mfdsem` | `../glibc/gfdsem.c` | `musl-gcc -static-pie -Os -DNOFORK '-DTAG="MFDSEM"' -o mfdsem gfdsem.c && strip --strip-all mfdsem` (musl 1.2.5) |

Sign with `python3 userland/sign.py userland/musl/mfdsem`.

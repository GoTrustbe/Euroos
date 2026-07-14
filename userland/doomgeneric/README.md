# DOOM on EuroOS (vendored doomgeneric)

This directory contains the source of the DOOM port that ships as `/bin/doom`
on EuroOS. It is **third-party GPL-2.0 code** (see `LICENSE`) — unlike the rest
of this repository (EUPL-1.2). It builds a **standalone userspace program**; no
code in this directory is linked into the EuroOS kernel or any EuroOS library.

Provenance:
- Engine: id Software's DOOM (GPL source release), via the
  [doomgeneric](https://github.com/ozkl/doomgeneric) portability layer,
  upstream commit `dcb7a8dbc7a16ce3dda29382ac9aae9d77d21284`.
- Only the sources needed for the EuroOS build are vendored (the canonical
  `SRC_DOOM` list + headers); the SDL/X11/Windows/Allegro backends are omitted.
- `doomgeneric_euroos.c` is the EuroOS platform backend (written for EuroOS,
  same GPL-2.0 license): frames go out through the `fb_present` syscall
  (0x6000), key events come in through `getkey` (0x6001), timing through
  `clock_gettime`, and the WAD is read with plain stdio over the Linux-ABI
  bridge (open/readv/lseek/fstat).

Build: `userland/build.sh` compiles this directory with `musl-gcc -static-pie`
into `doom.elf` (Ed25519-signed as `doom.elf.sig`, embedded by the kernel and
registered as `/bin/doom`).

Game data: `userland/doom1.wad` is the DOOM 1 **shareware** episode, which id
Software's shareware terms allow distributing unmodified. It is served
read-only from the kernel image at `/doom1.wad`. To play the full game, place
a retail `doom.wad` you own on the filesystem and run `doom -iwad <path>`.

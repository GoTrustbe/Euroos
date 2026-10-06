# EuroOS App Developer Guide

How to build an application for EuroOS on an ordinary Linux machine, sign it
with your own key, and let users install and run it. Status 2026-10-06: the
first version of the SDK, verified in QEMU; see the limits at the end.

## 1. What an EuroOS app is

A statically specified, signed package (`.eupkg`) containing one program. The
program is a normal Linux x86-64 executable linked against glibc: EuroOS runs it
on its Linux-ABI layer with the glibc runtime it serves itself (the same
library set its Chromium uses). There is no EuroOS-specific SDK library to link;
the kernel's policy is applied from the outside:

- **Capabilities** (`caps`): `console` (stdout), `file` (open/read files),
  `net` (sockets), `proc` (getpid, uname). What is not listed is refused.
  Read-only access to the system libraries (`/lib`, `/usr/lib`, `/usr/share`,
  the ld.so cache) never needs `file`: a dynamically linked program starts
  without it, and `file` only governs the user's data. The X11 display
  connection (a local AF_UNIX socket) never needs `net`; `net` is for the
  network and for serving sockets.
- **Syscall profile** (`profile`): `utility` (recommended: no new processes,
  no tracing, mounts, modules, identity change), `renderer` (also no inbound
  sockets, no file-system modification, and a file-system scope limited to
  system paths), `strict` (read/write/exit only), `full`.
- **Display** (`display`): `none` for a command-line program, `x11` for a
  program that opens an X11 window on the in-kernel X server (`DISPLAY=:0`).

## 2. Build

```bash
# any C program against glibc; for a window add -lX11
gcc -O2 -o hello hello.c
```

Keep the binary self-contained: dynamic libraries beyond glibc, libX11, cairo,
pango, GTK3 and their dependencies (what EuroOS already serves) are not
available. Static linking of your own libraries works.

## 3. Key and package

```bash
pip install cryptography                 # once
python3 sdk/euroapp.py keygen mykeys/me   # mykeys/me.key (private), mykeys/me.pub
python3 sdk/euroapp.py build --name hello --version 1.0.0 --binary hello \
    --key mykeys/me.key --title "Hello" --caps console,proc --profile utility \
    --display none --out hello-1.0.0.eupkg
python3 sdk/euroapp.py verify hello-1.0.0.eupkg --pub mykeys/me.pub
python3 sdk/euroapp.py info hello-1.0.0.eupkg
```

The package is a plain STORED zip: `MANIFEST.toml`, the binary and a 64-byte
Ed25519 signature over the manifest. The manifest carries the binary's SHA-256,
so the signature covers the binary. Names may contain letters, digits, `-`, `_`.

## 4. Install and run on EuroOS

Copy the `.eupkg` and your `.pub` onto the system (USB stick, `/usb`, or the
file manager). In the Terminal:

```
app trust /usb/me.pub me        # once per developer key (owner only)
app trusted
app install /usb/hello-1.0.0.eupkg
app installed
app run hello                   # a display=none app: output appears in the terminal
```

An `x11` app appears in the launcher (EU button, or the search field) under its
title and opens in a hosted window. Closing the window ends it.

What the kernel checks: the signature against your enrolled key (or the EuroOS
developer key), the binary's hash at install **and again at every launch**, and
the manifest's capabilities and syscall profile at spawn. A tampered package or
store blob is refused with the reason in the log.

What the in-kernel X server offers a core-X program: windows, pixmaps, GCs,
fills, PutImage/GetImage/CopyArea, ClearArea, core text (PolyText8 and
ImageText8 with one fixed 8x8 server font; QueryFont reports its metrics),
Expose/MapNotify/ConfigureNotify, keyboard and mouse events. No XRender,
no Xft server-side glyphs: cairo and Pango draw client-side and upload with
PutImage, which is what GTK and Chromium do. `scripts/app-x11-desktop.py`
is the QEMU harness that launches the windowed example and screendumps it.

## 5. Examples

`sdk/examples/hello-cli` (exits 7, used by the kernel's `[app]` self-test) and
`sdk/examples/hello-x11` (an Xlib window). `sdk/examples/build.sh` builds and
packages both with the example developer key in `sdk/examples/keys/`. That key
is public: do not sign real software with it.

## 6. Limits of this first version

- One hosted X11 app at a time (the same window Chromium uses).
- The whole binary is loaded into memory at launch (not demand-paged); keep
  packages to tens of megabytes.
- No dependencies between packages, no icons yet (the launcher shows the title).
- Only the system's own library set; no package may ship libraries.
- `app run` blocks the desktop until the program exits; use it for short tools.
- Signed indexes (an app store) are the next step (night plan N7).

## 6. Native apps: no Linux library at all (sprint WP6)

Everything above runs a glibc program through the compatibility bridge. A
**native** EuroOS app speaks only the Euro API, the interface the kernel
defines for applications: a window surface you draw into, an event queue,
the system text engine, whole-file reads and writes inside your home, a log
line and a clock. No libc, no X11, no Linux syscall. The binary is a
freestanding static PIE; it runs under the kernel's `euro` syscall profile,
an allow-list of exactly these calls plus exit, memory and sleep. Anything
else ends the process with SIGSYS before any handler sees the arguments.

| call | what | capability |
|------|------|------------|
| `info` | ABI version, screen size, your capabilities | none |
| `surface_create(w, h)` | one window surface per process, up to 1920x1200 | `window` |
| `surface_present(buf, w, h)` | show your XRGB8888 pixels | `window` |
| `event_poll` / `event_wait(ms)` | keys (scancode + character under the active layout), pointer, buttons, resize, close | `window` |
| `text_draw(...)` / `text_measure` | the system font (DM Sans, anti-aliased) rendered by the kernel into your buffer | `window` / none |
| `log(s)` | one line to the system log and your console output | `console` |
| `file_read` / `file_write` | whole files in your home; nothing else (not `/tmp`) | `file` |
| `time_ms` / `sleep_ms` / `exit` | clock, sleep, end | none |

### Rust (recommended)

```bash
cd sdk/rust
cargo build --release                        # target x86_64-unknown-none, static PIE
python3 ../euroapp.py build --abi euro --name hello-euro --version 1.0.0 \
    --binary target/x86_64-unknown-none/release/hello-euro --key mykeys/me.key \
    --title "Hello (native)" --caps console,window --display x11 --out hello-euro-1.0.0.eupkg
```

`sdk/rust/eurosdk` is a `#![no_std]` crate: `Surface` (your pixel buffer,
`present()`, `rect`, `text`), `Event`, `event_wait`, `log`, `file_read`,
`file_write`. `sdk/rust/examples/hello-euro` is the complete example (a window,
the kernel's text, a key and click counter, Escape to quit) in about a hundred
lines; `efuzz` is the fuzzer the kernel's self-test runs.

### C

`sdk/c/euro.h` has the same calls as inline functions for freestanding C:
`gcc -O2 -ffreestanding -nostdlib -fpie -static-pie -fno-stack-protector -o app app.c`.
`sdk/c/hello-euro-c.c` is the C version of the example.

### WASM

Build the same Rust source with `--features wasm` for `wasm32-unknown-unknown`
and package it with `--abi wasm`. The kernel runs it in `eurowasm`, its own
interpreter, with the Euro API as the only import. A WASM app is
**frame-driven**: it exports `init()` (return 0 when ready) and `frame()`
(return 0 to keep running); the desktop calls `frame` once per tick under a
step budget, so a module can never hold the desktop, and `event_wait`
returns at once. The interpreter is a no-JIT interpreter: fine for a tool or a
form, slow for heavy graphics. Same capabilities, same signature checks.

### What a native app gets, stated plainly

One surface per process; events only while its window has focus; files only
inside the session user's home (`..`, `/tmp` and other paths are refused);
text calls up to 512 bytes at up to 96 px, log lines up to 4 KiB, file reads
up to 16 MiB, file writes up to 4 MiB per call and 64 MiB per process; the
program's code, data and bss must fit in 2 MiB (W^X page table); no network,
no IPC, no clipboard yet (planned). The kernel re-verifies the binary at
every launch, as for any app, and checks every pointer you pass against your
own writable memory (a pointer into your code pages is refused, not faulted).

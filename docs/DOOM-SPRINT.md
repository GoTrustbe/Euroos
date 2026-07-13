# Sprint DOOM — run `doomgeneric` on EuroOS

**Goal:** boot EuroOS, type `doom` in the terminal, and play DOOM — an
UNMODIFIED-engine musl static-PIE binary drawing to a real compositor window and
reading the keyboard, over EuroOS's Linux ABI. It runs on the from-scratch
kernel; no Linux underneath.

**Why it's not free today:** userspace can run real musl C binaries (proven:
`muslreal`), but has no way to push *pixels* to the screen or read *keys* — the
display protocol is text-only, there's no framebuffer syscall, no key-read path.
So the work is a small kernel graphics/input API + a doomgeneric port.

**Honesty guardrails:** DOOM is a real game engine (id Software GPL source via
doomgeneric); the shareware `doom1.wad` is freely redistributable. We show it
actually running (title → menu → gameplay), not a mock. If a step only half
works, we say so.

---

## D0 — Kernel: a large-arena scheduled process  ⬜
Discovered during D1 scoping: `spawn_bg_musl` gives each process a fixed **2 MiB
arena** (code+heap+stack), and interactive `run_args` runs to completion — but
DOOM is an infinite game loop needing ~32 MiB (code + 4 MiB WAD + heap). So DOOM
must be a **preemptively-scheduled** process (like the bg-musl demos) with a
**large arena**.
- Add `spawn_bg_app(falloc, prog, pid, argv0, arena_mib)` — same as
  `spawn_bg_musl` but a configurable (e.g. 32 MiB) arena + heap window.
- A shell command spawns DOOM this way and returns immediately (game runs in the
  background, drawing to its window).
- **Verify:** spawn a large-arena test process; it runs + can malloc several MiB.

## D1 — Kernel: userspace framebuffer present syscall  ⬜
Add a syscall `fb_present(buf, w, h)` (new Linux-ABI/native number) that copies a
userspace XRGB8888 pixel buffer into a compositor window dedicated to the calling
app, scaled/centered, then presents. First call creates the window.
- Reuse the centralized user-pointer validation (`ring3.rs` copy_from_user).
- Reuse the fast `present_rect` (memcpy) path.
- **Verify:** a tiny test program (`fbtest.c`) presents a gradient → a window with
  the gradient appears in a boot screenshot.

## D2 — Kernel: userspace key-input syscall  ⬜
Add `getkey()` → next key event `(pressed<<8 | keycode)` or 0 if none
(non-blocking). Route desktop key events into a ring buffer the app drains while
its window is focused.
- **Verify:** `fbtest.c` reads keys, tints the screen per key → screenshot changes
  when a key is injected.

## D3 — doomgeneric port + build  ⬜
Vendor doomgeneric (id GPL DOOM + doomgeneric shim). Implement the platform file
`doomgeneric_euroos.c`:
- `DG_Init` → open the window (first `fb_present`).
- `DG_DrawFrame` → `fb_present(DG_ScreenBuffer, DOOMGENERIC_RESX, RESY)`.
- `DG_GetKey` → `getkey()` mapped to DOOM keycodes.
- `DG_GetTicksMs` / `DG_SleepMs` → `clock_gettime` / `nanosleep`.
Compile as **musl static-PIE** (`musl-gcc -static-pie`), like `muslreal`.
- **Verify:** `doom` links; runs far enough to print its startup banner.

## D4 — WAD + first frame  ⬜
Put `doom1.wad` (shareware) on EuroFS (register at boot / load from FS); DOOM
opens it via `fopen` (proven by `muslfile`). Bump guest RAM to 512M.
- **Verify:** screenshot shows the DOOM title screen / menu.

## D5 — Playable + capture  ⬜
Wire arrow/ctrl/enter keys; confirm menu navigation + in-game movement/fire.
Record a video. Bump guest CPU headroom as needed (TCG is slow — expect low FPS).
- **Verify:** video of DOOM's menu + a few seconds of gameplay on EuroOS.

---

### Risks
- Missing libc syscalls DOOM needs → add stubs/real impls as they surface.
- 4 MB WAD + DOOM working set on a 256 MB RAM root → use `-m 512M`.
- TCG framerate will be low; that's the emulator, not the port.

---

## STATUS (2026-07-13) — SPRINT COMPLETE ✅ DOOM IS PLAYABLE

**D0 — large-arena scheduled process — DONE ✅.** `ring3::spawn_bg_app(…, arena_mib)`
+ `paging::build_address_space_big`: 32 MiB arena (block 0 W^X code, rest RW+NX
huge pages). Boot-verified: `bg-app (pid 90) arena 0x5c00000 span 32 MiB`.

**D1 — userspace framebuffer present — DONE ✅.** Syscall `fb_present(buf,w,h)`
(`0x6000`, bounds-checked vs the process arena). Final design paints **directly
from the presenting syscall** (`main.rs::screen_present_xrgb`, scaled + centered
to the GOP) — the app's own CPU time pays for its own pixels, so a starved desktop
loop can't freeze the picture. `userland/fbtest.c` (animated gradient) validates
the path end-to-end.

**D2 — userspace key input — DONE ✅.** `getkey()` (`0x6001`) delivers raw
make/break scancodes; while an app owns the screen the desktop loop only drains
PS/2-decoded codes into `appgfx::push_key`. The keyboard drove DOOM's menu into a
New Game. (A sandbox-QEMU quirk remains where QMP `send-key` sometimes emits no
HID report while a fullscreen app runs — host-side, diagnosed via QEMU tracing;
irrelevant for real hardware.)

**D3 — doomgeneric port + build — DONE ✅.** `doomgeneric_euroos.c` (~150 lines:
fb_present + getkey + clock_gettime + unbuffered stdout), musl-gcc static-PIE,
552 KB `userland/doom.elf`, Ed25519-signed, embedded + registered as `/bin/doom`.

**D4 — WAD + first frame — DONE ✅.** Shareware `doom1.wad` served straight from
the kernel image (`vfs` special-case, sentinel file-index) — NOT in the RAM FS
(the boot integrity scrub made a 4 MiB file cost >4 min of TCG boot). New in
`bg_dispatch`: `open/openat/close/lseek/fstat/read/readv` (+ `clock_gettime`),
with real-file fds taking precedence over stale global pipe fds. Three WAD
blockers found + fixed: missing `readv` (musl stdio reads with nothing else),
the global `PIPE_FDS` fd-3 collision, and the scrub crawl.

**D5 — playable + capture — DONE ✅.** Boot → shell `doom` → DOOM banner →
`R_Init` → title screen → Enter/arrows navigate the menu → **in-game E1M1 with
live HUD, animating** (unique frame checksums across 9 consecutive screenshots).
Captures: `DOOM-on-EuroOS.png` (title), `DOOM-playable-on-EuroOS.png` (E1M1).
Blog post staged at `web-staging/blog/doom-on-euroos/`.

**Follow-up hardening the sprint triggered (2026-07-13):** the DOOM harness made
the pre-existing flaky boot measurable; RIP-sampling a hung boot (30/30 samples
in the heap-lock spin, IF=0) root-caused it to the two BUG-007-class instances
the June audit missed — the **heap** and **UART** spinlocks, taken from IF=0
contexts (xHCI MSI-X harvest allocates+prints; bg-app syscalls run with FMASK-
cleared IF). Fixed class-wide (`IrqSafeHeap`, irqsave `_print`), plus BUG-011:
`push_scancode`/`push_byte` upgraded from lossy `try_lock` to lossless irqsave —
the "first keystroke after boot goes missing" bug. See
`docs/LOADTEST-TRACKER.md` BUG-010/BUG-011. New tooling: boot-time symbolization
anchor line + `scratchpad/bootdbg.py` QMP RIP-sampling hang catcher.

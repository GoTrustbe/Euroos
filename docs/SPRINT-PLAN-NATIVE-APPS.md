# Sprint plan WP6: native EuroOS apps (no Linux libraries)

Status: ACTIVE, started 2026-10-06. Everything is measured in QEMU and lands
on `feature/app-control` in increments, each with a self-test marker.

## Why

The EuroApps SDK (night of 2026-10-06) lets third parties ship apps, but an
app is a glibc program with Xlib, cairo or GTK: Linux libraries running
through the compatibility bridge. That bridge is how outside software gets on
EuroOS; it is not what EuroOS is. A native app must be able to open a window,
draw, read input, read and write its files and talk to the system with
**zero Linux libraries**, against an interface the kernel defines and keeps
stable, enforced by EuroGuard capabilities only.

What exists today and is reused: the native loader (`ring3::spawn_bg_app`,
static PIE ELF, own address space, 2 MiB blocks), the app-graphics bridge
that DOOM uses (`fb_present`, raw keys), the capability model
(`CAP_CONSOLE/PROC_INFO/FILE/NET`), the per-task syscall profiles
(`eurosandbox`), the hosted-window mechanism of the compositor (a window whose
body is a pixel buffer, today the X server's), the kernel's text engine
(fontdue, DM Sans), the signed `.eupkg` package with manifest caps and
profile, and `eurowasm`, a from-scratch WASM interpreter with cap-gated host
imports.

## Architecture: one Euro API, two bindings

### Euro API v1 (kernel, `kernel/src/euroapi.rs`)

Syscall numbers `0x6100..0x61FF`, the `euro` namespace. Arguments in
rdi, rsi, rdx, r10, r8 (the existing syscall convention); results are
unsigned, errors are negative errno-style values. Every call validates user
pointers through the existing `copy_from_user`/`copy_to_user`/arena checks,
bounds every size, and is gated by a capability:

| nr     | call                                             | cap       |
|--------|--------------------------------------------------|-----------|
| 0x6100 | `info(out, cap)` ABI version, screen w/h, caps   | none      |
| 0x6101 | `surface_create(w, h)` -> 1 (one per process)    | window    |
| 0x6102 | `surface_present(buf, w, h)` copy XRGB8888       | window    |
| 0x6103 | `event_poll(out)` -> 1 if an event was written   | window    |
| 0x6104 | `event_wait(timeout_ms)` -> 1 ready / 0 timeout  | window    |
| 0x6105 | `text_draw(buf, w, h, x, y, s, len, argb, px)`   | window    |
| 0x6106 | `text_measure(s, len, px)` -> w<<32 \| h          | none      |
| 0x6107 | `log(s, len)` serial + app output                | console   |
| 0x6108 | `time_ms()` monotonic                            | none      |
| 0x6109 | `sleep_ms(ms)`                                   | none      |
| 0x610A | `file_read(path, len, buf, cap)` -> n            | file      |
| 0x610B | `file_write(path, len, buf, n)` -> n             | file      |
| 0x610C | `exit(code)`                                     | none      |

Event record (16 bytes): `kind:u32, a:u32, b:u32, c:u32`. Kinds: 1 Key
(a = scancode, b = pressed, c = unicode or 0), 2 PointerMove (a = x, b = y),
3 Button (a = button, b = pressed, c = x<<16|y), 4 Resize (a = w, b = h),
5 Close. Events only reach the process whose surface has focus; the queue
is bounded (256) and never blocks the desktop.

`file_read`/`file_write` are whole-file operations on paths inside the
session user's home or `/tmp` (the scope is checked in the kernel; `..` and
absolute paths elsewhere are refused). Partial I/O, directories and streams
are v2.

### Profile `euro` (eurosandbox)

A native app runs under `Profile::Euro`: an allow-list of exactly the Euro
API plus `exit`, `exit_group`, `brk`, anonymous `mmap`/`munmap`/`mprotect`
(never executable), `nanosleep`, `clock_gettime` and `write` to fd 1/2.
Every other Linux syscall ends the process with SIGSYS and is logged. A
native app has no Linux surface at all: no open, no socket, no fork.

### Capability `window`

New bit `CAP_WINDOW` (1 << 5). The manifest string `caps` accepts `window`.
Without it the surface calls fail with EPERM; an app can still be a
console program.

### Binding A: native ELF

`sdk/rust/eurosdk`: a `#![no_std]` crate with the raw syscalls, a `Surface`
(owns the pixel buffer in the app's memory, `present()`), `Event`, a
`Canvas` with fill/rect/line/text helpers and a `run` loop. Built for the
`x86_64-unknown-none` target as a static PIE; no libc, no allocator required
(an optional bump allocator for `alloc`). `sdk/c/euro.h` gives the same
calls to freestanding C (`gcc -ffreestanding -nostdlib -static-pie`).

Package: `.eupkg` with `abi = "euro"`. The kernel launches it with
`spawn_bg_app` under `Profile::Euro`, the manifest caps and a hosted window
(`SuiteApp::EuroSurface`) titled from the manifest; raw keys and pointer
events are routed to it while its window has focus; when the process exits
the window closes.

### Binding B: WASM

The same calls as imports in the `euro` module for `eurowasm`. A WASM app is
frame-driven: it exports `init() -> i32` and `frame() -> i32`; the kernel
calls `frame` from the desktop loop with a step budget (fuel), so a module
can never block or starve the desktop, and returns when `frame` returns
non-zero. Linear memory stays within the existing 16 MiB bound; every pointer
and length an import receives is checked against it. Package: `abi = "wasm"`,
the binary is the `.wasm`. Built with `wasm32-unknown-unknown`; the
`eurosdk` crate has a `wasm` feature that swaps syscalls for imports, so one
app source builds both ways.

### Not in v1 (stated)

Clipboard, EuroIPC between apps, partial file I/O and directories, more than
one surface per process, images/icons in packages, a widget toolkit, a WASM
compiler (the interpreter is slow for heavy graphics), audio.

## Security requirements (each has a test)

1. No user pointer is dereferenced unchecked: all through
   `copy_from_user`/`copy_to_user`/arena bounds. (`gfuzz` is extended to the
   `0x6100..0x6110` range with the hostile argument pool; the kernel must
   survive 20000 calls.)
2. Sizes bounded: surface <= 1920x1200, strings <= 4 KiB, files <= 16 MiB,
   events queue 256, one surface per process.
3. `Profile::Euro` is an allow-list; host test proves `open`, `socket`,
   `fork`, `execve`, `ptrace` and `mmap PROT_EXEC` are killed.
4. Capabilities: host test for the cap table of every Euro call; QEMU test
   that a package without `window` cannot create a surface.
5. Input isolation: events only to the focused surface; a background app
   gets nothing (QEMU test: two surfaces, keys land in one).
6. File scope: `file_read("/etc/euroid/users.db")` and `..` refused (QEMU
   test).
7. WASM: fuel bound per `frame`, memory bound, pointer checks (host tests in
   `eurowasm`).
8. Signature and hash as before: re-verified at every launch.

## Increments and proofs

- **W1 kernel Euro API + profile + cap** (`euroapi.rs`, dispatcher arms,
  `Profile::Euro`, `CAP_WINDOW`, compositor `EuroSurface` window, input
  routing, launcher/shell integration, `abi` field in europkg).
  Proof: `[euroapp]` self-test launches the embedded native example,
  a frame is presented, text is drawn, a key event arrives, a forbidden
  syscall kills, a surface without `window` is refused.
- **W2 Rust SDK + example** (`sdk/rust/eurosdk`, `sdk/rust/examples/
  hello-euro`, C header + `hello-euro-c`). Proof: the package built from the
  SDK is the binary the `[euroapp]` test embeds; desktop screenshot via
  `scripts/app-euro-desktop.py`.
- **W3 WASM binding** (`EuroHost` in `wasm.rs`, fuel, frame loop, `abi =
  "wasm"` launch). Proof: `[euroapp-wasm]` marker with the same example
  built for wasm32; screenshot.
- **W4 hardening** (gfuzz range, host tests 1..7, docs: developer guide
  section "Native apps", ROADMAP §0b).

## Order

W1 -> W2 (the native example is needed for W1's proof, so they are built
together) -> W4 tests for the native path -> W3 -> W4 remainder -> gate ->
commit.

## Progress, 2026-10-06 (all QEMU-measured)

- **W1 + W2 built and proven.** `kernel/src/euroapi.rs` (surfaces, events,
  focus, text through the kernel's fontdue engine, file scope), the Euro arms
  in `ring3::bg_dispatch`, `Profile::Euro` (host test: allow-list, every
  Linux call killed), `CAP_WINDOW`, `spawn_euro_app` (caps + profile set with
  interrupts off before the first instruction), the `EuroSurface` hosted
  window with key, pointer and close routing, `abi` in the package manifest,
  `sdk/rust/eurosdk` + `hello-euro` (5 KB static PIE, no relocations),
  `sdk/c/euro.h` + `hello-euro-c` (13.7 KB). `[euroapp]` marker: installed,
  abi=euro, frame presented, header blue, body white, kernel text drawn, key
  event redraws, Escape exits 0, without `window` refused (exit 2), a Linux
  `open(2)` kills the probe (SIGSYS, exit 159) before its next instruction,
  file scope refuses `/etc/euroid/users.db` and `..`. Desktop proof:
  `docs/proof/2026-10-06-native-app-euro-api.png` via
  `scripts/app-euro-desktop.py` ("launched natively (pid 400, task 36; caps
  'console,window', profile euro; no Linux library)").
- **W3 built and proven.** `eurowasm` learned what Rust-compiled modules
  need (br_table, narrow loads and stores, i64 load/store, memory.fill and
  memory.copy, sign-extension, i64 div/rem/shr_u/rot/clz/ctz/popcnt) with
  host tests, plus `set_fuel`. `kernel/src/wasm.rs` hosts a frame-driven
  app (`init`/`frame`, `FRAME_FUEL` 4 M steps per call) with `euro.call` as
  its one import, every pointer checked against linear memory. The same
  `hello-euro` source built with `--features wasm` (4 KB) runs: `[euroapp-wasm]`
  marker with the same checks, and a spinning module is ended by the fuel bound.
- **W4 partly.** `efuzz`: 20000 random Euro calls with hostile arguments under
  `console,window,file`; `[eurofuzz]` OK (exit 171, no fault, no panic).
  Host tests: eurosandbox (Euro profile), europkg (abi), eurowasm (opcodes,
  fuel). Developer guide section 6 "Native apps". Not yet: the two-surface
  input-isolation QEMU test (one surface per desktop today; focus is per
  surface in `euroapi`), clipboard, IPC.
- **Defects found and fixed on the way:** background processes had no
  capability or profile check at all in the syscall route (now enforced for
  the euro profile); a killed process ran on until the next timer tick (now
  yields at once); `sleep_ticks` only marks a task sleeping, so a boot
  self-test waiting on an app must yield and bound by the clock; a wait loop
  inside a syscall froze the clock and hung the machine (event_wait now
  sleeps one tick and returns, the SDK loops); the first frame's size did not
  reach the window frame until the fit moved out of the tick-gated repaint
  block.

## Security review of the Euro API (2026-10-06, after the first commit)

Question asked: can an app with this API compromise the OS or reach data it
may not? Method: read every path an app can drive (the syscall route, the
Euro arms, euroapi, the WASM host, the desktop routing, the package launch)
as an attacker, then prove each fix in QEMU.

**Findings, fixed in the same day (all in the tree, re-measured):**

1. **Kernel fault through an app pointer (DoS of the whole machine).**
   `copy_to_user` and the in-place writers (text_draw, file_read) checked
   that a pointer lies inside the process arena, not that the page is
   writable. A native app's code pages are mapped R-X; a kernel write there
   under CR0.WP is a ring-0 page fault, which this kernel treats as fatal.
   Any native app could halt EuroOS with `event_poll(out = its own code)`.
   Fix: a page-table walk (`paging::user_range_writable_in`) before every
   kernel write into app memory under the euro profile; the answer is
   -EFAULT. Proof: `efuzz` now puts its own code page and stack in the
   argument pool; `[eurofuzz]` still ends at exit 171.
2. **Lock discipline (machine hang).** The surface table was taken with
   interrupts enabled in the desktop loop and with interrupts off inside
   syscalls; a timer tick while the desktop held it, followed by an app
   spinning on it in a syscall, hangs the machine (this kernel's IF=0 rule).
   Fix: every access runs with interrupts off (`with_surfaces`).
3. **File scope too wide.** `/tmp` was in the scope: shared by every user
   and process, so a native app could read another app's or user's temp
   files. Fix: the home of the session user only. The self-test proves
   `/tmp/x`, `/home/euroX/f`, `/home/euro` (the directory itself),
   `/etc/euroid/users.db` and `..` are refused.
4. **Kernel heap exhaustion through file_write.** The RAM VFS lives on the
   kernel heap; an app could write files until the allocator aborted. Fix:
   4 MiB per call, 64 MiB per process lifetime (ENOSPC beyond), bounded
   accounting table.
5. **CPU time inside a syscall.** `text_draw` accepted 4 KiB of text at 200
   px; rasterising that with interrupts off is long enough to trip the
   deadman watchdog. Fix: 512 bytes at up to 96 px per call.
6. **A dead process ran on.** After `exit` (and after a profile kill) the
   task returned to user mode until the next timer tick. Fix: the route
   yields at once after marking the task dead, for exit, exit_group and the
   Euro exit alike.
7. **W^X bypass by size.** A program whose code+data+bss exceeds the first
   2 MiB block would get RWX pages (the loader warns and continues). Fix:
   `spawn_euro_app` refuses it.
8. **Pid collision.** Native and WASM pids started at 400 and 600; fork pids
   start at 1000. Fix: own ranges (0x10000, 0x20000).
9. **WASM console apps could hold the shell.** `run_cli` pumped up to 10000
   frames of 4 M steps. Fix: 50 frames.
10. **Close was never graceful.** The desktop's wait loop after the Close
    event did not yield, so the app was always terminated. Fix: yield.

**Checked and found sound:** capability gating of every call (the self-test
runs the binary without `window` and the surface is refused); the euro
profile as an allow-list (host test: every Linux syscall in the probe list
is killed, including the EuroIPC numbers and the legacy app-graphics
bridge); caps and profile set before the first instruction (interrupts off
from spawn to bookkeeping); signature and hash re-verified at every launch;
surfaces bounded (1920x1200, one per process), events bounded (256), log
lines bounded; events only to the focused surface and only while its
window is active; WASM pointers checked against linear memory, memory
bounded to 16 MiB, fuel per frame; `mmap`/`brk` of a native app stay inside
its own arena.

**Residual, stated:** a native app with keyboard focus receives every key
(the desktop's own shortcuts included) until its window is closed with the
mouse; a WASM app's `file_read` goes through EuroFS, which follows
symlinks: a link the user placed in their own home can reach a
world-readable file outside it (EuroFS permissions still apply; an lstat
in the FileSystem trait would close this); the manifest title is free text
(an app may call itself anything, as on every OS); `brk` below the heap
start stays inside the process's own arena (process-local only).

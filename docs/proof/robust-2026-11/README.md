# Stability pass: panics to errors, and a test harness for the desktop (2026-11-10)

Branch `feature/robust`, started on `feature/app-control` at `a36ef4b`. Not merged.
Bugs are in `../../LOADTEST-TRACKER.md` (BUG-041 to BUG-045, BUG-021 closed); the
harness is described in `../../GUI-TESTING.md`.

| Commit | What |
|---|---|
| `43b59b3` | `paging.rs`: `try_` address-space builders (`Err(OutOfFrames)`), guarded stacks; `euromm` `allocate_many` |
| `86c168e` | `xhci.rs`, `hda.rs`: out of frames or an odd device skips the device |
| `d9f3fda` | `nvme.rs`: Identify Namespace data validated (`euronvme::identify`) |
| `e469740` | `net.rs`: the one `expect` in the HTTPS fetch |
| `5090eff` | `euromedia`, `euroflate`: decoders hardened (BUG-041), `flood_fill` |
| `11ddc27` | apps and shell: bounded file reads (`eurofs::read_file_bounded`), unwraps on user input |
| `bf87957` | `xhci.rs`, `nvme.rs`, `hda.rs`: BARs above the identity map (BUG-021, BUG-042) |
| `54e843f` | `xhci.rs`: left Alt from a USB keyboard (BUG-043) |
| `5d47884` | `textedit.rs`: gutter drawn past the window |
| (after)  | `scripts/gui-smoke.py`, docs, this proof |

Serial logs stay on the build servers (`*.log` is ignored); the `.markers` files hold
the verdict lines, the `.txt` files the harness reports and the excerpts.

## A. Panic sites

What each site does now. "Self-test" is a boot marker of the selftest image
(`[robust]`, `[robust-paging]`, `[robust-usb]`, `[robust-hda]`, `[robust-fs]`),
"host" a `cargo test`.

| Site (at `a36ef4b`) | Was | Now | Provoked by |
|---|---|---|---|
| `xhci.rs` 327-352: six `expect` on the controller's frames | panic when out of frames | the frames are counted from HCSPARAMS2 and checked before the controller reset; `[xhci] out of memory: the controller needs N frames, M free — USB skipped`, `init` returns `false` | `[robust-usb]`: the real `init` against an empty frame allocator; with a controller present it stops before the reset and the two HID devices keep working (`r630-kvm-highbar.markers`, matrix `usb` leg) |
| `xhci.rs`: 16 more `expect` on per-device frames | panic | `[xhci] out of memory (no frame for …) — device skipped`, that device is not configured | `[robust-usb]` (the helper); not provoked per device |
| `xhci.rs`: 10 `unwrap` (driver state, HID report map, audio/ECM endpoints) | panic on a controller that went away or a descriptor without X/Y | no controller: nothing to enumerate; descriptor without X or Y: fallback layout; missing endpoint: device skipped | read-through only |
| `hda.rs` 203-294: five `expect` | panic | 11 frames checked before the controller reset: `[hda] out of memory: 11 frames needed, M free — audio skipped`; later failures stop the CORB/RIRB DMA and return `false` | `[robust-hda]`, with a running controller in the matrix `hda` leg and on the R630 |
| `hda.rs` 282-285: audio buffer as eight single `allocate()` | worked only while the eight frames happened to be adjacent | `allocate_contiguous(8)` | matrix `hda` leg (stream runs) |
| `paging.rs` 131-136, 249-251, 362-364: process page tables | panic | `try_build_address_space{,_big,_rwx,_rwx_big,_remap}` return `Err(OutOfFrames)`; the frames are taken together, so a refusal keeps nothing | `[robust-paging]`: every builder at 0..n free frames, 19 refusals with 0 frames kept, 5 builds freed again; host tests of `allocate_many` |
| the old `build_address_space*` | panic | still panic (`try_().expect`): their ten callers are in `ring3.rs` | see `RING3-ENOMEM-TODO.md` |
| `paging.rs` 501-502, 535: guarded stacks | panic | `guarded_stack_alloc` returns 0 (its documented "no stack"), which `main.rs` and `smp.rs` already handle | read-through only |
| `paging.rs` 1298-1327: boot page tables | panic | unchanged, deliberately: without them there is no kernel | — |
| `nvme.rs` 208: `try_into().unwrap()`, 512 B assumed for unknown formats | wrong geometry for an odd device | `parse_identify_namespace`: empty page, block size outside 512..4096 or an overflowing size → `[nvme] Identify Namespace data unusable (…) — disk skipped` | host: zeros, ones, bad sizes, 20 000 random pages; `[robust]` nvme-identify |
| `shell.rs` 1138, 1188-1190: `net` command | panic if an own packet does not parse | a `… → FAILED` line of output | `[robust]` net-cmd |
| `shell.rs` 1351: `app` command | (guarded) `unwrap` | usage line | `[robust]` app-cmd: `app`, `apps x y`, `app nosuch net only`, … |
| `webview.rs` 347: typing in a form field | `last_mut().unwrap()` | the slot is created, then looked up | `[robust]` form-field: a focused field without a slot, typed into and left |
| `paint.rs` 240: bucket fill | `unwrap` on the clicked pixel, four stack entries per pixel | `euromedia::flood_fill`: any coordinate, scanline, fallible work list | host: clicks outside the canvas, 400 random images against the old algorithm; `[robust]` paint-bucket through the real `pointer()` |
| `paint.rs` 82, `textedit.rs` 41: `as_mut().unwrap()` | (guarded) | `get_or_insert_with` | — |
| `net.rs` 4597 | `expect("tls set after connect")` | `fail("TLS session missing after connect")` | not provoked |

Found on the way and fixed in the same files: BUG-041 (image decoders), the HDA
buffer above, and the EuroText gutter that was drawn 44 px past the window.

**Files larger than memory (BUG-025/BUG-038 class).** `eurofs::read_file_bounded`
reads the metadata first and refuses a file over the caller's limit;
`read_budget(free_heap, app_cap, copies)` gives a caller its share of the free heap.

| Caller | Limit | Over the limit |
|---|---|---|
| EuroText | 8 MiB, at most a quarter of the free heap | the open document stays; status line `…: file too large to open (300 MiB; at most 8 MiB fits in memory here)` |
| EuroPaint | file 32 MiB (a third of the free heap), decoded image an eighth of the free heap | canvas stays; same message, or `image too large to edit (WxH)` |
| EuroPaint undo | 24 steps and 48 MiB (a quarter of the free heap), copies made fallibly | oldest steps dropped; `not enough memory to keep an undo step` |
| Image viewer | 64 MiB (a third of the free heap) | `…: file too large to open (…)` in the viewer |
| `cat` | about a quarter of the free heap | `cat: FILE: file too large to open (…)` |
| `comm`, `join`, `split`, filters (`sort`, `grep`, …) | a third to a half | `CMD: FILE: file too large to open (…)` |
| digests, `wc` | seven eighths (they only read) | the same |
| `mime::resolve` (file type for the file manager) | 4 MiB | typed by name, not read |

`[robust-fs]` opens a 64 KiB file in the editor, the paint app, the viewer and
through the shell helper with the limit set to 4 KiB (the code path is the real
one, the limit is not), and checks that the open document, canvas and picture
are kept. A file that is really larger than the heap was not written: that test
belongs to `loadtest.py --scenario fsio --huge-mib`.

Not done: `notes.rs` (a note is rewritten from memory on save, so refusing to
load one would lose it), `cp` (the read is fallible since BUG-025 and nothing
copies it), and `main.rs` 7313-7351 (`populate_fs`, 15 `unwrap` on
`create_dir`/`write_file`: for the owner of `main.rs`).

## B. Results

### Host tests

`cargo test` (default members): **1284 passed, 0 failed** (`hosttests.txt`).
New: `euromm` 3, `eurofs` 5, `euronvme` 3, `euroflate` 1, `euromedia` 12. The
mutation tests of `euromedia` run every truncation and about 50 000
corruptions of 13 sample files; against the JPEG decoder of `a36ef4b` the
truncation test alone reports `2003 truncations panicked`.

### TCG, selftest image (.16)

`tcg-boot.sh` as in `../net-worker-2026-11/` (q35, qemu64, e1000e, AHCI boot
disk), two at a time: `user-smp4-iommu` and `user-smp1-noiommu` (512 MiB),
`storage-smp4-iommu-1g`, `combined-smp4-iommu-1g`. Against
`../net-worker-2026-11/tcg-*.markers` (`comparison-tcg.txt`):

| Boot | Desktop | OK lines (base) | FAILED lines (base) | Worse | Changed |
|---|---|---|---|---|---|
| `user-smp4-iommu` (512 MiB) | yes | 192 (187) | 5 (5) | 0 | 0 |
| `user-smp1-noiommu` (512 MiB) | yes | 189 (184) | 5 (5) | 0 | 0 |
| `storage-smp4-iommu-1g` | yes | 209 (204) | 12 (12) | 0 | 0 |
| `combined-smp4-iommu-1g` | yes | 218 (213) | 12 (12) | 0 | 0 |

The five extra OK lines per boot are the `[robust…]` markers. The FAILED lines
are the same tags with the same counts as in the base markers (`[churn-*]`,
`[fork]`, `[glibc]`, `[linux-abi]`, `[app]`, `[app-policy]`). `[lk] boot-total
order inversions=0 (self-test provoked=2)` in all four.

### q35 matrix legs (.16, TCG, 512 MiB)

`scripts/run-metal-matrix.py IMG --legs usb,hda,nvme` on its own copy of the
image (the legs boot it writable), with the release key (`EK_FRESH_DEV_KEY` not
set): `usb` PASS, `hda` PASS, `nvme` PASS; the `[robust…]` markers of the three legs, with a live xHCI controller in all of them and a running HDA stream in the `hda` leg, are in `matrix-robust-markers.txt` (`matrix.txt`).

### R630, KVM `-cpu host`: BARs at 56 TiB

`kvm-boot.sh`, xhci + usb-kbd + usb-tablet + intel-hda + nvme, no network.

| Image | Result |
|---|---|
| before `bf87957`, xhci + usb-kbd + usb-tablet only (BUG-042) | `[idt] PAGE FAULT addr=0x380000000000` right after `[euro] PS/2 mouse initialized`, 4 s into the boot (`r630-highbar-before.txt`) |
| before, nvme only (BUG-021) | the same fault, earlier in the boot (the NVMe driver comes first) |
| before, xhci + hda + nvme | the NVMe fault |
| before, hda only | boots (QEMU's HDA has a 32-bit BAR) |
| final, smp 4, 4 GiB | desktop after 37 s; NVMe self-tests OK, `[xhci] controller @ 0xffc0004000` (the window), 2 HID devices, HDA stream running, all five `[robust…]` markers OK, `[lk] … inversions=0` (`r630-kvm-highbar-smp4.markers`) |
| final, smp 1, 1 GiB | the same (`r630-kvm-highbar-smp1-1g.markers`) |

### GUI smoke (`scripts/gui-smoke.py`, public image)

| Run | Result | Time | Longest console round trip |
|---|---|---|---|
| TCG (.16), smp 2, 2 GiB, USB tablet + keyboard, no NIC | PASS: 54/55 steps, 1 known (BUG-044) | 471 s | 1055 ms |
| KVM (R630), smp 4, 4 GiB, USB tablet + keyboard, no NIC (smoke part of the soak run) | 54/55 steps, 1 known (BUG-044) | | |
| KVM (R630), smp 2, 2 GiB, virtio tablet + keyboard, SLIRP `restrict=on` (smoke part of the soak run) | 54/55 steps, 1 known (BUG-044) | | |

Reports: `gui-smoke-tcg.txt`, `gui-soak-usb.txt`, `gui-soak-virtio.txt`; the
QEMU command lines next to them; the per-app screenshots of the KVM/USB run in
`gui-soak-usb-shots/`.

The runs before these, on earlier images of the same day, are how the bugs
were found:

- first KVM run (USB input): page fault at boot, 4 s in → BUG-042 (and BUG-021,
  same cause), fixed in `bf87957`;
- first TCG run (USB input): 50 of 54 steps. Alt+2 left the windows where they
  were and typed a `2` into the terminal → BUG-043, fixed in `54e843f`; the
  launcher step after it failed as well and passes since; two drag steps were
  the harness (a slow guest latches the press a few pixels into the drag; the
  check now accepts a window that followed the pointer);
- KVM with virtio input, 15 min of monkey, seed 1: 1064 actions, no crash; one
  functional check failed because the monkey had left text on the terminal
  prompt (harness: the check now sends Enter first);
- the Alt-Tab step with EuroText in front, added after BUG-043: BUG-044, open.

### Monkey soak

R630, KVM: `--soak 35` after the smoke pass, on the final public image.

| Run | Verdict | Monkey actions | Functional checks passed | Longest console round trip | Whole run |
|---|---|---|---|---|---|
| USB input, smp 4, 4 GiB, no NIC, seed 20261110 | PASS | 2230 | 11/11 | 302 ms (none over 5 s) | 2288 s |
| virtio input, smp 2, 2 GiB, `restrict=on`, seed 20261111 | PASS | 2246 | 11/11 | 1562 ms (none over 5 s) | 2277 s |

Action mix, USB run: alt-tab 71, click 510, click-in-window 321, ctrl-alt-esc 11, dock 248, double-click 107, drag 82, key 180, launcher 54, move-window 147, right-click 95, status-panel 16, title-button 115, type 213, workspace 60.
Action mix, virtio run: alt-tab 77, click 482, click-in-window 335, ctrl-alt-esc 12, dock 273, double-click 101, drag 59, key 203, launcher 65, move-window 122, right-click 91, status-panel 18, title-button 110, type 238, workspace 60.

Serial consoles of both runs (`gui-soak-serial-summary.txt`):

```
usb: serial lines 2417, crash signatures 0, [isolation] lines 0, scon echoes 290, e2e commands 48
virtio: serial lines 2372, crash signatures 0, [isolation] lines 0, scon echoes 291, e2e commands 44
```

A functional check is: back to workspace 1, every window that can be found
closed, the terminal raised through the launcher (`term`, Enter), a command
typed with the injected keyboard and found on the serial console. The action sequences are in `gui-soak-*-actions.txt.gz`;
the desktops after the soak in `gui-soak-usb-shots/monkey-final.png` and
`gui-soak-virtio-monkey-final.png`.

TCG (.16), monkey only, 12 min, seed 7: PASS, 230 actions, functional check
1/1, longest console round trip 2356 ms (`gui-monkey-tcg.txt`).

The USB run in the table is the second one. The first, with the same seed and
image (`gui-soak-usb-run1.txt`): 2299 actions in 35 min without a crash or a
hang, longest console round trip 2417 ms, but only 5 of 11 functional
checks passed, none after action 1000. That check looked for the terminal's
title-bar buttons on a screendump after clicking its dock tile. It cannot see a
window whose buttons the monkey left cut off at a screen edge or covered by
another window, and a dock click on a terminal that is already in front closes
it. The check now raises the terminal through the launcher and does not need to
see it. Reading that run's screenshots is how BUG-045 was found
(`bug045-repro.py`, `bug045-window-over-dock.png`).

## To do for other teams

- **Owner of `ring3.rs`**: move the ten callers to the `try_` builders
  (`RING3-ENOMEM-TODO.md`). Until then a process start with fewer than five
  free frames still panics.
- **Owner of `main.rs`**: `populate_fs` (7313-7351) `unwrap`s every
  `create_dir`/`write_file`; a full or read-only root at first boot is a panic.
  The `[robust…]` self-tests hang off `textedit::selftest` and
  `shell::selftest` because this branch could not add a call in `main.rs`; a
  direct call there is the natural place.
- **Keyboard layer (`ps2.rs`)**: BUG-044.
- **Desktop loop / compositor**: BUG-045 (a window over the dock).
- **After the help/onboarding branch merges**: an onboarding overlay or a
  tooltip that appears on its own changes what the smoke pass sees right after
  boot; the first step ("close every window the desktop boots with") is where
  to dismiss it.

## Open

- Per-device out-of-frames paths in `xhci.rs` return `false` and leave what the
  device already took allocated (as every other early exit of the enumeration
  does). Nothing provokes them on a live controller.
- The `try_` builders exist and are tested; no process start uses them yet.
- BUG-044 (Alt+Tab while an editor has the keyboard) and BUG-045 (a window
  dragged to the left edge covers the dock, which still takes the clicks).
- The smoke pass checks that an app opens, takes input and survives it, plus
  one end-to-end result (a typed terminal command). It does not check what
  each app draws.

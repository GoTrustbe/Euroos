# Desktop pass: repaint on change, AltGr, mouse wheel, disks, window clip (2026-11-12)

Branch `feature/uidebug` on `feature/app-control` `7858b72`. Not merged, not
pushed. Bugs: `../../LOADTEST-TRACKER.md` (BUG-052, 054, 055, 057, 058 and the
new BUG-070 to BUG-077).

| Commit | What |
|---|---|
| `98bb550` | the kernel and crate changes: BUG-052, 054, 055, 057, 058, 070 to 076, the mouse wheel, the tour after the wizard, `display.mode`, help texts |
| `c50ddf9` | `gui-smoke.py` (ten more steps, wheel and AltGr in the monkey, incremental serial decoder: BUG-077), `wizard-install-test.py` (one more check) |
| `58109e5` | help panel: the footer names the wheel; no workspace pager over the open panel |
| `c78a8e8` | help: the AltGr row (German has `#` on a key of its own) |
| `286c508` | `gui-smoke.py`: the Alt-Tab steps wait for the switcher (harness timing under TCG) |
| (last) | this proof, the tracker, `GUI-TESTING.md`, `HELP-SYSTEM.md`, `SETTINGS.md` |

Which image was tested: release image and self-test image built at `c78a8e8`
(sha256 `7ce767e26763…` and `aa1de93b4262…`). Where a run is on the release
image of `58109e5` it says so; `c78a8e8` differs from it in one help string.
"Before" is the release image of `7858b72` (sha256 `c6a1ffef288e…`); for
1280x720 a build of `7858b72` plus only the `display.mode` boot option.

Serial logs stay on the build servers. The `.markers` files hold the verdict
lines of each log plus its `[lk]`, `[fork]` and `[boot-time]` lines.

## Per point

### 1. BUG-057: the notification list, and repainting on change

- **Cause.** One line of the desktop loop set `need_full` in every iteration
  while the notification list, a context menu, the launcher, the file dialog,
  the symbol picker or the Alt-Tab switcher was open, while a file was dragged,
  and for 3.2 s after every notification (the toast). The list had no way to
  close except a second click on the status panel.
- **Fix.** Repaint when what the overlays show changes (open flags, number of
  toasts on screen, a notification counter, the menu row under the pointer, the
  pointer during a file drag), once per 2 s tick while one is up, and instead
  of the in-place redraw of the Terminal or the calculator on a key. Esc closes
  the list and a context menu; a click beside the list closes it.
- **Also found.** The 2 s housekeeping (reaping, supervision, log flush, scrub,
  update check) ran only in an iteration that was a tick and not a full
  repaint: never while an overlay was open (BUG-073). It runs on every tick now.
- **Measured** (`frame-time.txt`, `frame_time.py`, R630 KVM, smp 4, seven
  windows, the list open; five pointer moves each):

  | Image | pointer move visible after | console echo, median / max |
  |---|---|---|
  | `7858b72` | 373, 291, 307, 371, 291 ms | 100 / 150 ms |
  | `58109e5` | 46, 44, 43, 35, 45 ms | 50 / 251 ms |
  | `c78a8e8` | 45, 46, 39, 44, 43 ms | 50 / 201 ms |

  Without the list (seven windows) all three images: 38 to 63 ms, with single
  values of 124 to 159 ms. The one slow echo with the list open is the full
  repaint of the 2 s tick. A first measurement of `c78a8e8` ran while a TCG
  guest of this team used the sibling hyperthreads and gave 42, 44, 292, 634,
  308 ms; it is in `frame-time.txt` as well.
- **Counted** (`repaints` shell command, `repaints.txt`): with the list open, 2 full
  repaints in 416 loop iterations over 4.3 s (418 at 1280x720); `gui-smoke.py` checks the same in every run (2 in 285,
  2 in 287). Before, every iteration was one.
- **Other states that forced a repaint per iteration.** Fixed with the same
  change: context menu, launcher, file dialog, symbol picker, switcher, toast.
  Already on change: help panel, tour, tooltip (one repaint when it appears and
  when it goes). Left, one repaint per event: a file drag (per pointer move), the
  menu highlight (per row change), a window drag or resize, a stroke in EuroPaint.

### 2. The tour over the install wizard

- **Cause.** The tour was decided in the first loop iteration, whatever was on
  screen. **Fix.** It is decided when no visible window is the wizard.
- **Shown.** `before/1080/wizard-0-first-desktop-tour-over-the-wizard.png`;
  `after/*/wizard-0-first-desktop.png` (wizard alone),
  `wizard-1-after-closing-the-wizard.png` (the tour card), at both sizes;
  `wizard-install-test.txt` check "the first-run tour waits while the install
  wizard is open".

### 3. BUG-052: AltGr

- **Cause and fix:** tracker. Besides the AltGr level the layouts themselves
  were wrong in places (BUG-072: German punctuation and umlauts, French `_`,
  the keys right of P, L and M on AZERTY, the ISO key `< >`).
- **Shown.** `hosttests.txt`: `eurokeymap` 15 tests (7 before). `[altgr]` in
  all ten matrix boots (`selftest-lines.txt`). `gui-smoke.py`: `@ # { } [ ] ~ €
  \ |` typed with AltGr on each of the three layouts and read back from the
  shell; AltGr+Tab and AltGr+2 switch nothing with the Terminal and with
  EuroText in front. USB keyboard and virtio keyboard, KVM and TCG.
- **Not covered:** a PS/2 keyboard (QEMU delivers no PS/2 interrupts under
  `-display none`; the PS/2 path is the one the boot self-test feeds). On the
  image without the fix the AltGr+Tab step passes with the USB keyboard,
  because that path did not forward right Alt at all; the character step fails.

### 4. BUG-055: Storage

- `before/1080/20-eurobeheer-storage.png`: "1 MiB of 8 MiB used (21%)", "(no
  virtio-blk devices …)". `after/*/20-eurobeheer-storage.png`: "1.7 MiB of
  8.0 MiB used (21%)", `sata0  256 MiB, SATA (AHCI), QEMU HARDDISK`.
- `disks.txt`, `disks-four-buses.png` (release image `58109e5`): a virtio disk,
  two SATA disks and an NVMe disk listed with size, bus and model, in the panel
  and in `lsblk`.
- Not looked at: the "(system disk)" mark on an installed system; a MegaRAID
  logical drive and a USB disk in the panel (the code path is the same list;
  the combined matrix boots have both controllers and `[disks]` is OK there).

### 5. BUG-058 and drawing outside the window

- **Fix:** a clip rectangle in the framebuffer set around every window body;
  saturating layout in the calculator; a minimum of 500 x 380 for the
  calculator; windows fitted into the screen at start (BUG-075).
- **Looked at, all of them** (`after/1080`, `after/720`: `app-NN-<name>.png`
  at the default size, `app-NN-<name>-smallest.png` after dragging the grip to
  200 x 150): the eleven dock apps. Nothing is drawn outside a window at either
  size. `before/1080/app-06-calculator-smallest.png`: the keys run down to the
  bottom of the screen. `before/720/`: EuroClock almost entirely below the
  screen, EuroNotes and EuroLog under the status panel, the Terminal and
  EuroWeb cut off by the bottom edge; `after/720/`: all inside.
- **Help panel and tour at 1280x720** (`after/720/30…37-help-*.png`,
  `tour-1…6.png`): the panel fits the screen,
  the page list, the text and the footer are complete; the six cards are inside
  the screen with all buttons. Found there and fixed: the workspace pager was
  drawn on the panel's bottom line (`before/720/30-help-overview.png`).
- **Seen, cosmetic, not changed:** at 320 px width EuroText's Open and Save
  buttons lie over the file path; EuroFiles shows two of its nine toolbar
  buttons; EuroNotes shows the start of the note only. Not looked at: EuroView,
  EuroPaint, the EuroSuite previews and an installed app's window at the
  smallest size (they have no dock tile; the clip applies to them as well).

### 6. BUG-054: the seam

`seam.txt` (`seam.py` on the screendumps of the empty desktop): the colour step
between the two pixels at the middle of the screen, mean over the rows, was
15.0 (1920x1080) and 15.0 (1280x720) and is 0.13 and 0.18; the largest is 2,
below the median of the other columns (3).

### 7. Mouse wheel

- PS/2: the IntelliMouse sequence at init, 4-byte packets when the device
  answers id 3. USB HID: the wheel byte of a boot mouse, the Wheel usage of a
  report descriptor, byte 5 of the usb-tablet report. virtio-input: `REL_WHEEL`.
- Connected: help panel, EuroBeheer, EuroFiles (the list had no scrolling at
  all: BUG-074), EuroText (the view moves, the cursor stays), the Terminal
  (400 lines kept; `after/*/24-terminal-scrolled-back.png`).
- **Shown.** `gui-smoke.py` steps "mouse wheel scrolls EuroBeheer down and back
  up" and "… the Terminal back, a key returns to the prompt", with the USB
  tablet and the virtio tablet; `after/*/31-help-overview-wheel.png`; host test
  `wheel_from_boot_mouse_tablet_and_descriptor` (`eurousb`).
- **Not covered by a run:** the PS/2 wheel (see point 3), a relative USB mouse
  with a wheel (the harness uses tablets), the wheel in EuroFiles and EuroText
  (no step; the monkey turns the wheel over them, 49 to 54 times per soak).

### 8. EuroFiles buttons, EuroNotes menu

`before/1080/app-00-files.png`: six buttons, slanted folder icons.
`after/*/app-00-files.png`, `22-eurofiles-etc.png`: nine buttons, folder icons.
`after/*/23-euronotes-context-menu.png`: the menu; Paste and Insert symbol go
to `notes::input` (read, not typed through in a run).

## Test table

| Test | Where | Result |
|---|---|---|
| `cargo test` (default members) | .16 | 186 suites, 1 313 passed, 0 failed, 1 ignored (`hosttests.txt`; 1 304 before: 8 more in `eurokeymap`, 1 in `eurousb`) |
| Self-test image, TCG matrix: `user-smp{4,1}-{iommu,noiommu}`, `kernel-smp{4,1}`, `storage-smp{4,1}-iommu-1g`, `combined-smp{4,1}-iommu-1g` | .16, `tcg-boot.sh`, `tcg-matrix.sh` | 10 of 10 to the desktop, no fault line; `markers-compare.py` against `../integ-2026-11/`: **0 worse** in all ten, new tags `[altgr]` and `[disks]` OK (`comparison-tcg.txt`); `[lk]` `boot-total order inversions=0 … task-lock-with-IF0=0` in all ten (`tcg-summary.txt`) |
| `gui-smoke.py`, release image, TCG (usb-kbd + usb-tablet, smp 2, 2 GiB) | .16 | with the final harness (`286c508`): **PASS 66/66** twice (`gui-smoke-tcg.txt`, `gui-smoke-tcg-b.txt`). With the harness of `c50ddf9`, three runs: 64/66 (both Alt-Tab steps: the single look 0.9 s after Tab came before the switcher was painted; the TCG matrix ran beside it), 65/66 (the calculator followed the title-bar drag for 37 % of the distance, the step wants half; the integration's run had 74 % for two windows), 66/66 (`gui-smoke-tcg-run1…3.txt`). The Alt-Tab steps now wait up to 12 s for the switcher; the drag step is unchanged and can fail again on a loaded host |
| `gui-smoke.py`, release image, KVM, USB input (smp 4, 4 GiB, no NIC) | R630 | PASS 66/66 (`gui-soak-usb.txt`, the smoke pass before the soak) |
| `gui-smoke.py`, release image, KVM, virtio input (smp 2, 2 GiB, `restrict=on`) | R630 | PASS 66/66 (`gui-soak-virtio.txt`) |
| The same harness on the image without the fixes (`7858b72`), KVM, USB | R630 | 59/66: the seven steps that test BUG-057, the AltGr characters and the wheel fail (`gui-smoke-kvm-baseline-image.txt`) |
| Monkey soak 35 min, USB input, seed 20261112 | R630 KVM | PASS: 2 227 actions (49 wheel, 18 AltGr), functional checks 11/11, no crash, console max 250 ms (`gui-soak-usb.txt`) |
| Monkey soak 35 min, virtio input, seed 20261113 | R630 KVM | PASS: 2 087 actions (50 wheel, 20 AltGr), functional checks 10/10, no crash, console max 2 054 ms, one echo (`gui-soak-virtio.txt`; the integration had 2 005 ms with virtio; 250 ms in the run on `58109e5`) |
| The same two soaks on the release image of `58109e5` | R630 KVM | PASS: 2 232 and 2 084 actions, 11/11 and 10/10, console max 200 and 250 ms (`gui-soak-*-58109e5.txt`) |
| `wizard-install-test.py` | .16 TCG | 15/15: the 14 of before and the new check (`wizard-install-test.txt`) |
| Boot time, release image, `boot-time.py`, SATA medium, smp 2, 2 GiB, `restrict=on`, 5 boots | R630 KVM | `c78a8e8`: median **2.70 s** (2.58 to 3.01); `7858b72` in the same session: 2.75 s (2.37 to 2.93); the integration: 2.71 s. `58109e5`, measured alone on the machine: 2.55 s (2.28 to 2.69), `7858b72` then 2.63 s (`kvm-5x.txt`) |

Time to the desktop of the self-test image under TCG in this matrix: 170 to
194 s (user and kernel configurations) where the integration had 144 to 159 s.
The build server ran builds and TCG guests of two teams at a load of 15 to 20;
every phase is longer by the same factor (`subsystems` 33.7 to 42.3 s,
`lockscreen` 88.8 to 110.3 s, `first-frame` 8.4 to 9.8 s in `user-smp4-iommu`),
none of them stands out. Not measured on a quiet machine.

## Open

- A PS/2 keyboard and a PS/2 wheel mouse are tested by the boot self-test and
  by reading, not by an injected device (points 3 and 7).
- EuroFiles and EuroText scroll with the wheel only: no scroll bar to drag, no
  key. The Terminal has no key for scrolling back either.
- One full repaint per pointer move during a file drag, per highlighted row of
  a context menu, during a window drag or resize.
- Dead keys do not combine, Shift+AltGr is not mapped, the US layout has no
  AltGr level (right Alt does nothing there).
- `graphics.rs` has one clip rectangle, not a stack; a window body that wants
  to clip a part of itself cannot nest one.
- `display.mode` only selects a mode the firmware offers.
- English only, and everything the earlier proofs list as open (BUG-053, 056).

## Files

| File | What |
|---|---|
| `probe.py`, `scenarios/*.txt` | the scripted screenshots (on top of the `Guest` class of `gui-smoke.py`); `after.txt` at both sizes, `before.txt`, `before720.txt`, `tour.txt` (`--keep-tour`), `wizard.txt` (blank virtio disk), `disks.txt` |
| `montage.py`, `seam.py`, `frame_time.py` | tiling crops of screenshots; the seam measurement; the frame-time measurement |
| `tcg-boot.sh`, `tcg-matrix.sh`, `r630-runs.sh.txt` | how the runs were started |
| `before/1080`, `before/720`, `after/1080`, `after/720` | the screenshots (QEMU `screendump`); all were looked at |
| `gui-soak-*-final.png`, `gui-soak-*-actions.txt.gz`, `gui-soak-serial-summary.txt` | the desktops after 35 min, every injected event, the crash scan of the serial logs |

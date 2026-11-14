# Small fixes: BUG-086, 087, 200, 201, 202, 114, 115, 112, and the BUG-161 decision (2026-11-14)

Branch `feature/small-2026-11e` from `feature/app-control` `3ecde9e` (worktree `/opt/euroos/wt-small` on .16). Not pushed.
R630: run directory `/srv/euroos/small-runs`, CPUs 24-39, KVM, `-nic none` (the DHCP boots: user-net `restrict=on`).
Rule kept: every bug has its test first (a commit that fails), then the fix. "Before" = the image of the test commits, "after" = the image with the fix.

| Commit | What |
|---|---|
| `f350026` | BUG-086/087 test first: `[randid]` marker; `euronet` reply carries `xid` and `chaddr` |
| `207c14e` | BUG-200/201/202 test first: `scripts/ro-install-test.py`, `[wizard-targets]`, `[nmi-bound]` |
| `47f4102` | BUG-114 test first: `scripts/hosted-end-test.py` |
| `81a322c` | BUG-087: SMB client challenge from the CSPRNG |
| `13b46b3` | BUG-086: DHCP xid per exchange from the CSPRNG, replies matched on xid and chaddr |
| `7b5c2e0` | BUG-200: the installer formats, provisions and verifies without the root block cache |
| `b2c1de1` | BUG-201: the wizard does not offer a read-only disk |
| `26e31a2` | BUG-202: the NMI probe stops at the top of the interrupted stack |
| `0c7e65a` | BUG-115: the tour looks at the keys again before the desktop's key readers (test: `scripts/tour-esc-test.py`) |
| `583cd76` | BUG-114: the desktop closes a hosted app that ended by itself |
| `9facaee` | BUG-112: the live GTK demo is started by the desktop loop |
| `fe9b648` | docs: BUG-161 is a decision (open codecs only) |
| (last) | this proof and the tracker (BUG-220 new) |

Images: self-test before `8ec5b1d4486b97ef`, public before (test commits + fixes of 086/087/200/201/202, not 114/115) built from `9b222bf`; final self-test `01f3bc31957bfd2a`, final public `91d64866662b0915`
(both of the last code commit `9facaee`, `scripts/build.sh release` / `image`, `-j 6`, `nice 10`).

## Before and after, measured

| Bug | Test | Before | After |
|---|---|---|---|
| 086 | `[randid] dhcp xid` (16 draws): all different, none an old constant | FAILED (`4e555021` twice) | OK |
| 087 | `[randid] smb client challenge` (16 draws): all different, no `EURO` tail | FAILED (same 8 bytes twice, tail `45 55 52 4f`) | OK |
| 086 | DHCP lease with the new xid (public image, user-net restrict=on) | n/a | virtio-net: `[net-up] lease 10.0.2.15 ... 50 ms`; e1000e (userspace driver): `DHCP ACK: lease 10.0.2.15` (`dhcp/`) |
| 200 | `MODE=inject ro-install-test.py`: disk 0 = null-co (writes accepted and lost) | `after remount hostname=true user=true euroid-store=true` although `GPT=false ESP=false` | all false, `creating the EuroFS root failed` |
| 201 | `[wizard-targets]` with a blank read-only virtio disk 1 | `selectable=true` FAILED | `selectable=false` OK; the install on the writable disk 0 verifies OK |
| 202 | `[nmi-bound]`: RSP 0x1e0 below the top 0x8000019000 | scan end `0x8000019620` (past the top) FAILED | `0x8000019000` OK; unknown stack near a page end `0x70001000`; far from the top the full 2 KiB |
| 114 | `hosted-end-test.py`: hello-x11, key, second `app run` | 2 PASS / 3 FAIL: `a hosted app is already running (close its window first)` | 5/5: `[hosted] the app ended by itself: frame closed, free RAM 1224 -> 1608 MiB`, second start launched |
| 115 | `tour-esc-test.py`: public image, virtio keyboard, Esc 0..1.5 s after the tour appeared | **27/30** (boots 2, 11, 24 failed: `[vinput] Key code 1` pressed and released, no `[tour] closed`, as in the report) | **80/80** (two lanes of 40, other seeds) |
| 112 | live-demos image, KVM, 4 GiB: does the demo load, are its keys read | 3 of 4 boots `ggtk: error while loading shared libraries` (`libc.so.6`, `libgmodule-2.0.so.0`, `libgdk-3.so.0`: `failed to map segment`), 0 of 4 with a `[keylat]` line | 0 of 2 load errors, 2 of 2 `[keylat]` (ring to client 4 us, queue to read 0.8 ms, total 9 ms) and the click delivered |

What the tests do not show. BUG-200: the read-only target of the report no longer reaches the check (BUG-191 fails first, BUG-201 hides the disk), so the null-co disk is the only way to show it.
BUG-202: no NMI was provoked on a loaded machine; the scan bound is shown by the function on the numbers of the report. BUG-112: the one-second timer (`GGTK: live tick`) was NOT
checked: the demo's own self-test closes its window 90 desktop loops after the first frame (`[gtk-test] closed GTK window`). `[mm-fail]` lines in those boots: glib's 64/128 MiB thread arenas
are refused in the 384 MiB arena (it copes). BUG-114: the second symptom (first start resized to its frame, later start keeps its 480x240) is not changed.
BUG-087: the `[io5]` SMB mount self-test needs a server and was not run. BUG-086: only virtio-net and e1000e were run, not r8169, bcm5720, usb-net.

## Regression (images of the last code commit)

| Check | Result |
|---|---|
| `cargo test` on .16 (`cargo-test.txt`) | 187 binaries, **1 364 passed, 0 failed**, 1 ignored (validate-2026-11d: 1 363; the one more is `reply_is_matched_on_xid_and_mac`) |
| `gui-smoke.py`, public image, KVM 4 vCPU 4 GiB (`final/gui-smoke-public-usb.txt`) | **PASS 67/67** (also on the image before the BUG-112 commit: `gui-smoke-public-usb-P1.txt`) |
| `gui-smoke.py --input virtio --soak 20 --seed 20261114` (`final/gui-smoke-virtio-soak20.txt`) | **PASS 73/73** (smoke pass incl. "first-run tour: shown on the first desktop, Esc closes it", plus the soak: 1 166 actions in 20 min, max console latency 250 ms); the same on the image before the BUG-112 commit: PASS 73/73, 1 150 actions (`gui-smoke-virtio-soak20-P1.txt`) |
| `wizard-install-test.py` (TCG, `final/wizard-install-test.txt`) | **15/15**, the tour closed with Esc |
| KVM self-test boots 1, 2, 4, 8 GiB, HD-Audio (`final/kvm-selftest-boots.txt`) | desktop in all four, 0 crash lines; FAILED lines 2/0/0/0 (the two at 1 GiB are `GBIG malloc(200 MiB)`, as in validate-2026-11d) |
| markers-compare against validate-2026-11d (`final/comparison-kvm.txt`) | **0 worse, 0 changed** at all four sizes; only new: `[randid]` OK x2, `[nmi-bound]` OK, `[keyfocus]` OK |

## Decision and new bug

BUG-161 is marked in the tracker and in `docs/CHROME-ON-EUROOS.md` as a decision: only open codecs (VP9, AV1, Opus, Vorbis), no H.264/AAC/HEVC because of patents.
New: BUG-220 (the self-test image panics at boot after an install on a read-only disk 0 failed: `EuroFS format: IoError`).

## Files

`before/`, `after/`: outputs of the test scripts and the marker lines; `after/hosted-*.png`: the screens of the BUG-114 test; `livedemos/`: BUG-112 lines per boot; `dhcp/`; `final/`.
Scripts added: `scripts/ro-install-test.py` (modes `plain`, `inject`), `scripts/hosted-end-test.py`, `scripts/tour-esc-test.py`.

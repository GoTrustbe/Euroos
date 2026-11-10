# Settings: storage, form toolkit, Settings panel — proof (2026-11-10)

Branch `feature/settings`, started on `feature/app-control` `a36ef4b`. What was
built and how it is meant to be used is in `../../SETTINGS.md`; this directory
holds what was run and what came out.

Serial logs (`*.log`) stay on the build server next to this file and are not
committed. The `.markers` files hold the verdict lines of the two selftest
boots, compared with `scripts/markers-compare.py`.

## 1. Host tests

`cargo test -p europrefs`: 18 passed, 0 failed (parser/serializer round trip,
typed getters and defaults, broken lines, escapes, limits, lists, 400 rounds of
generated garbage that must never panic and must serialize clean; the EU
summer-time rule on its boundaries for 2024 to 2027; host, domain, app and user
name validation). `eurokeymap` (7), `eurolocale` (31) and `euroclock` (7),
which the panel calls, pass unchanged. Output: `host-tests.txt`.

## 2. Selftest image, TCG (`scripts/build.sh release`)

Two boots with `tcg-boot.sh` of `../net-worker-2026-11/` (unchanged, e1000e,
AHCI boot disk): `user-smp4-iommu` (512M, 4 cores, intel-iommu) and
`user-smp1-noiommu` (512M, 1 core). Both reach the desktop.

| Boot | Tags | vs `net-worker-2026-11` | vs `virtio-modern-2026-11` | `[lk] boot-total order` |
|---|---|---|---|---|
| user-smp4-iommu | 185 | worse 0, changed 0, gone 0, new 8 | worse 0, changed 0, gone 0 | inversions=0, task-lock-with-IF0=0 |
| user-smp1-noiommu | 184 | worse 0, changed 0, gone 0, new 8 | worse 0, changed 0, gone 0 | inversions=0, task-lock-with-IF0=0 |

The five tags that say FAILED (`app`, `churn-proc`, `churn-thread`,
`churn-fork`, `app-policy`) say so in the base proof as well, with the same
text: they need glibc pack files this build server does not have.
`comparison-tcg.txt` has the full output.

The eight new tags, all OK in both boots (`markers-settings.txt`):

| Tag | What it proves |
|---|---|
| `[prefs-rt]` | typed values survive save and reopen; the save is atomic (`Ok(Atomic)`), no temp file is left, a second save replaces the first |
| `[prefs-bad]` | a damaged file keeps its 3 intact lines and skips the 4 broken ones; a 2 KiB binary file gives defaults; the next save is clean |
| `[prefs-user]` | `/home/<user>/.config/euroos/<app>.conf` per user, separate, owned by the user, names that would leave the directory refused |
| `[uikit]` | form logic: hit-test, Tab order, checkbox, dropdown (mouse and keys), slider, text field, scrolling, disabled items, word wrap |
| `[set-keymap]` | the panel's dropdown, clicked, switches the decoder to German (Z key gives `y`) and writes `/etc/keymap`; the arrow key steps to French (Q key gives `a`) |
| `[set-time]` | time zone and 12/24-hour setting change the clock string, are saved (system and per user) and read back |
| `[set-host]` | a name typed into the field is written to `/etc/hostname` through the immutability gate (flag kept); an invalid name is refused |
| `[set-guard]` | a blocked domain, an unblocked system-policy domain, an app block and an allow-list are saved, forgotten, loaded: all four back |

Each of these restores the settings and files it touched.

## 3. Public image, live session (`scripts/build.sh image`)

`t2-gui.py` boots the image under QEMU/TCG (q35, 2 cores, 1 GiB, virtio tablet
and keyboard, user network) in its own work directory with its own QMP socket,
and plays `live.txt`: absolute pointer clicks and key presses, a screendump
after each step. `screenshots/live-*.png` (the settings window cropped; full
frames where the desktop matters).

Every section as it opens: `live-01-system` … `live-09b-updates-page2`.

In use, each checked against the serial log (`live-public-image-excerpt.txt`):

| Screenshots | What was done | Result |
|---|---|---|
| `live-10`, `live-11` | typed a computer name, Enter | `[set] computer name set to euro-desk7: Ok(Atomic)`; the field and the footer show it |
| `live-20` … `live-22` | typed `qwertz yz` in "Try the layout", picked German QWERTZ from the dropdown, typed the same keys again | first `qwertz yz`, then `qwerty zy`: the same keys, the other layout |
| `live-30` … `live-32` | picked Central Europe, switched the 24-hour clock off | local time one hour ahead of the hardware clock; the status panel clock (full frame `live-32`) reads `h:mm AM` |
| `live-40` | typed a domain, Enter | listed under "Domains you blocked"; `saved=true` |
| `live-41` | switched "Block all network access" on for the first app | row marked "(network blocked)" |
| `live-50`, `live-51` | "Check now" in Storage | footer: `Check finished: 145 files, 140 verified, 0 error(s) …`; the log line of that pass shown as last result |
| `live-60`, `live-61` | "Check now" in Updates | the real update server answered: manifest and signature fetched, `up to date`; the button is disabled while the check runs |
| `live-70` | web server switch on | `On`, saved |
| `live-80` … `live-83` | background "Blue mist", text size 110 % by click, Tab to move the focus, Right arrow: 120 % | whole desktop redrawn (full frame `live-81`); focus ring; slider follows the key |

On the live image every one of these applies at once and the panel says, per
section, that the session runs from memory and the change lasts until shutdown.

## 4. Installed system: settings survive a restart

`scripts/wizard-install-test.py` installed the same public image onto a blank
4 GiB disk (14 of 14 checks). That disk was then booted twice on its own
(AHCI, 2 GiB RAM), `persist-a.txt` and `persist-b.txt`:

- **Boot 1**: signed in as the installed user, changed in the panel: computer
  name, time zone (Central Europe), 12-hour clock, a blocked domain, a network
  block for one app, background, text size 110 %, keyboard German QWERTZ. Then
  QEMU was quit without a shutdown: a power cut.
- **Boot 2**: nothing changed, only read.

What boot 2 printed before anyone touched it (`persistence-installed.txt`):

```
[ps2] keyboard layout from /etc/keymap: de-qwertz
[prefs] system settings applied: /etc/euroos/settings.conf (1 key(s), 0 broken line(s) skipped), time zone Europe/Brussels, region nl, EuroGuard saved rules: 1 domain change(s), 1 app rule(s)
[prefs] session settings applied: /home/tester/.config/euroos/desktop.conf (3 key(s)), clock 12h, wallpaper mist, text scale 110%
```

(boot 1 had printed `us-qwerty`, `time zone UTC`, `0 domain change(s)`,
`clock 24h, wallpaper euro, text scale 100%`). The panel after boot 2 shows the
same: `screenshots/installed-boot2-B1-system` (name `euro-desk7`),
`B2-language` (German QWERTZ), `B3-datetime` (Central Europe, 12-hour, local
time one hour ahead of the hardware clock), `B4-euroguard` (the domain),
`B5-apps` (the app blocked), `B6-display` (Blue mist, 110 %), and the desktop
itself `B0-desktop` (background and `10:16 AM` in the status panel).

## What is in this directory

| File | |
|---|---|
| `host-tests.txt` | host test output |
| `tcg-user-smp4-iommu.markers`, `tcg-user-smp1-noiommu.markers` | verdict lines of the two selftest boots |
| `comparison-tcg.txt` | `markers-compare.py` against both earlier proofs, summary, lock order line |
| `markers-settings.txt` | the lines of the new tags, both boots |
| `live-public-image-excerpt.txt` | serial lines of the live run that the table above cites |
| `persistence-installed.txt` | wizard install verdicts, the relevant serial lines of both boots, harness verdicts |
| `screenshots/` | 48 PNG: `live-*`, `installed-boot1-*`, `installed-boot2-*` |
| `t2-gui.py`, `live.txt`, `persist-a.txt`, `persist-b.txt`, `run-final.sh` | the harness and its scripts, as run |

## Not covered here

- KVM and real hardware: all runs are TCG on the build server.
- A mouse wheel: the pointer drivers deliver no wheel events; scrolling was
  tested with PageUp/PageDown and the scrollbar.
- The update check with a newer release on the server (it answered "up to
  date"), and a check on a machine with update slots.
- Regional formats other than English and Dutch on screen.

# Help, shortcut overview, first-run tour, in-app tooltips (2026-11-10)

Branch `feature/help`, on `feature/app-control` at `a36ef4b`. Not merged.
Description and maintenance rules: [`docs/HELP-SYSTEM.md`](../../HELP-SYSTEM.md).

Serial logs stay on the build servers; the `.markers` files hold the verdict
lines.

## What was built

- **Help panel** (`kernel/src/help.rs`, texts in `kernel/src/help_text.rs`):
  21 pages: "EuroOS at a glance", "Keyboard shortcuts", "Security badges and
  terms", and one page per app (EuroFiles, EuroNotes, EuroClock, EuroWeb,
  Terminal, EuroBeheer, EuroReken, EuroAgent, EuroText, EuroMonitor, EuroLog,
  EuroView, EuroPaint, EuroInstall, app store / packages / updates, Chromium,
  EuroSuite preview windows, installed apps). F1 (active window), `?` in the
  title bar, F2 (shortcuts).
- **Shortcut overview**: generated from the system key list, the mouse list
  and the `keys` of every page.
- **First-run tour** (`kernel/src/onboarding.rs`): six steps, off in the
  self-test image and with `ui.tour=off`, flag file
  `/home/<user>/.config/euroos/tour-done`.
- **Tooltips inside apps** (`tooltip::app_tip`): EuroPaint tools, size
  slider, colour field, hue strip, Undo/Redo/Clear/Save; EuroFiles toolbar and
  the `protected` badge; EuroNotes toolbar, font switch, delete mark; EuroView
  toolbar; EuroText Open/Save; EuroWeb new tab and address bar; EuroReken
  memory keys, sign, history rows; the `?` button and the `Protected` pill.
- **Shell**: `help` by topic (98 commands in 8 groups), `help <command>`,
  `help tour`.
- **`Welcome.txt`** rewritten (shorter, correct, points to F1).

## Every statement checked against the code

| Page | Read | Points that the page depends on |
|---|---|---|
| EuroFiles | `files.rs`, the Files branches of the click, context-menu and file-op code in `main.rs`, `trash.rs` | Places; nine toolbar actions, of which a 720 px window shows six; Delete is `remove_file`/`remove_dir` (no Trash), Trash only from the context menu (`/var/trash`); Copy reads one file; Protect = `euroattr::set_user` (own files); History = `versions`/`restore_version`; the only badge the listing sets is `protected` |
| EuroNotes | `notes.rs`, `editcore.rs` | `/home/euro/notes/note-N.md`, saved after each key burst; toolbar actions; search field keys; delete mark keeps the last note; context-menu Paste/Insert symbol call `textedit::input` (so they land in EuroText) |
| EuroClock | `clockapp.rs` | render only: local time, date, world clocks; no click or key handling exists |
| EuroWeb | `webview.rs`, Browser branches in `main.rs` | typing starts the address; no scheme = HTTP; `Hit` has Tab/NewTab/UrlBar/Field/Submit only (no link, no close, no scroll); scripts run once at load; fetch blocks the loop |
| Terminal | Terminal branch in `main.rs`, `shell.rs` | pipes and redirection; `/bin` programs with capabilities; buffer cut to the visible lines; char-only input |
| EuroBeheer | `settings_ui.rs`, Settings branches in `main.rs` | four sections; block-domain field; per-app network switch and revoke; HTTP server switch; System is text only |
| EuroReken | `calc_ui.rs`, calculator key mapping in `main.rs` | key set; Enter/= commit to history (8 lines); M+/M-/MR/MC; bracket auto-close |
| EuroAgent | `agent_ui.rs` | the panel itself states "scripted demo model"; Allow/Deny are drawn, no hit test |
| EuroText | `textedit.rs`, `filedialog.rs`, `editcore.rs` | Save path (open file or `/home/euro/euro.txt`); the Save mode of the file dialog is never opened; Ctrl keys return false in `editcore`; Tab = four spaces |
| EuroMonitor, EuroLog | `monitor.rs`, `logview.rs` | render only; EuroLog shows `audit::recent(rows that fit)` |
| EuroView | `imageview.rs` | formats (`handles`, `decode`), progressive JPEG refused, six toolbar actions, siblings of the same folder |
| EuroPaint | `paint.rs` | nine tools, slider 1..64, 24 undo steps, canvas 640x470, Save always `/home/euro/pictures/painting.png` (PNG); `paint::open` has no caller |
| EuroInstall | `installer.rs` | page order, key help line, blank NVMe/SATA only, password policy text, TPM/no-TPM text |
| App store, packages | `appstore.rs`, `pkg.rs`, `launcher.rs`, `update.rs` | signed index, developer key from the index, hash at launch; launcher rows marked `install`; `eupkg` uses `/pkg/index.json`; no slot partitions on the preview image |
| Chromium | `launch_chrome_app` in `main.rs` | needs `/pack/chrome`; message without it |
| EuroSuite | `suite_ui.rs` | sample documents; no input handling for these windows |
| At a glance | `compositor.rs`, `launcher.rs`, `switcher.rs`, `workspace.rs`, `notify.rs`, `lockscreen.rs`, `ctxmenu` code in `main.rs`, root-FS selection in `main.rs` | dock (11 tiles), launcher search, snap zones, Alt+Tab, four workspaces, context-menu items, notification centre, unattended sign-in on the live medium only, RAM root without a virtio disk |
| Security badges | `compositor::draw_window` and `draw_status_panel`, `ring3::cap_names`, `portal.rs`, `euroguard` commands, `files.rs` | the pill is drawn for `sec.sandboxed`, which every window sets; four capability names; three portal buttons; TPM / no-TPM status card |
| Shell table | every arm of `shell::exec`, `coreutils`, the inline commands of the Terminal in `main.rs`, `crates/eurocoreutils` argument parsing | names, aliases, usage lines |

## Found while reading (and what was done)

| Finding | Action |
|---|---|
| Esc was never decoded: `eurokeymap::control_key` had no entry for scancode 0x01, so every `'\u{1b}'` arm (launcher, file dialog, symbol picker, calculator, address bar, prompts, installer "back") was dead on a keyboard | fixed in `crates/eurokeymap` with a host test (7 tests pass) |
| A USB keyboard had no F keys (`xhci::hid_to_set1`) | F1 and F2 added to the table (one line) |
| The Terminal's own `help` in `main.rs` listed a dozen commands; `shell.rs` had a second, different list | both replaced by one table; the old inline branch in `main.rs` is now unreachable (left in place: `main.rs` belongs to T1) |
| Alt+Tab and Alt+1..4 are latched only in `ps2::poll_key`, not in `poll_key_ex`: they do nothing while EuroText, EuroNotes, EuroInstall or a EuroFiles prompt has the keyboard | documented on the shortcut page; not changed |
| Context-menu items show key names (Ctrl C, Del, F5, Ctrl Shift N, Ctrl Z, Ctrl V) that are not wired to the keyboard | documented; not changed |
| Context-menu Paste and Insert symbol in a EuroNotes window insert into EuroText | documented on the EuroNotes page; not changed |
| EuroFiles: Perms, Protect and History are not drawn in the default 720 px window | documented; tooltip only where the button is drawn |
| EuroPaint module doc says "Save to PNG/QOI/BMP"; the button writes one fixed PNG; `paint::open` is never called | documented as limits; `paint.rs` untouched |
| On the public image the hosted GTK window is topmost at start while the Terminal is drawn active: typed keys reach the Terminal only after a click on it | F1 uses the active window first; otherwise not changed |
| KVM on the R630 with `-device qemu-xhci`: page fault at `0x380000000000` (64-bit BAR above the identity map; fixed on `feature/virtio-modern`, not in this base) | runs here use `-fw_cfg name=opt/ovmf/X-PciMmio64Mb,string=0` |

## Tests

### Self-test image, TCG on .16

`tcg-boot.sh` is the script of `docs/proof/virtio-modern-2026-11/`. Base =
`feature/app-control` at `a36ef4b`, built and booted in the same hour on the
same host; the reference markers of `net-worker-2026-11` give the same result.

| Boot (`net.driver=user`, 512M, no IOMMU) | Base | `feature/help` | base → help | `net-worker-2026-11` → help |
|---|---|---|---|---|
| smp 4 | 177 tags, 185 OK lines | 178 tags, 186 OK lines | 0 worse, 0 changed, 1 new (`help` OK) | 0 worse, 0 changed, 1 new (`help` OK) |
| smp 1 | 176 tags, 184 OK lines | 177 tags, 185 OK lines | 0 worse, 0 changed, 1 new (`help` OK) | 0 worse, 0 changed, 1 new (`help` OK) |

- The five FAILED markers (`app`, `churn-proc`, `churn-thread`, `churn-fork`,
  `app-policy`) are the same in the base and in the reference: at 512M the
  glibc arena does not fit ("no arena for glibc").
- `[lk] after-stress order inversions=0` and `[lk] boot-total order
  inversions=0` in both boots.
- The new marker, identical in both boots and in the public image:

  ```
  [help] help pages: window kinds 21/21 · launcher apps 15/15 · 21 pages (18 apps) · shortcut rows 43 · panel open/page/close=true/true/true · shell help 98 cmds ok=true · in-app tooltips=true · tour steps ok=true → OK (every app has a help page; F1, F2, ?) ✓
  ```
- No `[tour]` line: the tour is off in the self-test image.
- Wall time of `tcg-boot.sh` (desktop marker polled every 5 s, then 120 s):
  smp 4 base 246 s, help 256 s; smp 1 base 251 s, help 251 s. The host ran
  other builds during the help boots (load 11 to 13), so this shows no
  slowdown beyond the noise, not a measurement of it. What the branch adds to
  the boot is the `[help]` self-test and, in the first desktop-loop
  iteration, one `exists()`.

Files: `tcg-base-user-smp{4,1}.markers`, `tcg-help-user-smp{4,1}.markers`,
`comparison-tcg.txt`, `tcg-boot.sh`.

### Public image (`./scripts/build.sh image`), KVM on the R630, `-nic none`

`gui-run.sh IMG WORKDIR SCRIPT` boots the image with its own QMP and monitor
sockets in its own work directory and drives it with `scripts/qmp-input.py`
(USB keyboard and tablet). Input scripts: `gui-tour-help-tooltips.txt`,
`gui-passthrough-shell.txt`.

Serial log of the two runs (`gui-serial-excerpt.txt`):

```
== run r6: gui-tour-help-tooltips.txt
[help] help pages: window kinds 21/21 · launcher apps 15/15 · 21 pages (18 apps) · shortcut rows 43 · panel open/page/close=true/true/true · shell help 98 cmds ok=true · in-app tooltips=true · tour steps ok=true → OK (every app has a help page; F1, F2, ?) ✓
[lock] unattended boot — no input within grace → auto-login as euro (demo/CI)
[desktop] interactive loop started — input + shell live
[tour] first-run tour shown (Esc, Skip, a click outside or typing closes it)
[tour] closed at step 6 of 6: skipped (Esc or Skip)
== run r7: gui-passthrough-shell.txt
[help] help pages: window kinds 21/21 · launcher apps 15/15 · 21 pages (18 apps) · shortcut rows 43 · panel open/page/close=true/true/true · shell help 98 cmds ok=true · in-app tooltips=true · tour steps ok=true → OK (every app has a help page; F1, F2, ?) ✓
[lock] unattended boot — no input within grace → auto-login as euro (demo/CI)
[desktop] interactive loop started — input + shell live
[tour] first-run tour shown (Esc, Skip, a click outside or typing closes it)
[tour] closed by a click at 1399,759 outside the card (the click goes to the desktop)
[e2e] $ help
[e2e] $ help tour
[e2e] The tour is shown on the desktop (Esc closes it).
[tour] closed by typing (scancodes [23] stay for the desktop)
[e2e] $ help ls
[e2e] ls [path]  -  list a directory
[e2e] example: ls /home/euro
[e2e] $ help fetch
[e2e] fetch <host> [save-path]  -  HTTP GET; with a path, save the answer to a file
[e2e] example: fetch example.com
[e2e] also: wget
[e2e] $ help nosuch
[e2e] help: no entry for 'nosuch'.  Type help for the list by topic.
[e2e] $ help tour
[e2e] The tour is shown on the desktop (Esc closes it).
[tour] closed at step 1 of 6: do not show again
[tour] do not show again: /home/euro/.config/euroos/tour-done written
[e2e] $ ls /home/euro/.config/euroos
[e2e]   -rw-r--r--        2 B  2026-11-10 09:15  tour-done
```

Screenshots (1920x1080 frames from the QEMU monitor's `screendump`):

| File | Shows |
|---|---|
| `tour-1.png` … `tour-6.png` | the six tour steps (Right arrow steps through them, Esc closes) |
| `tour-click-through.png` | after a click on the Terminal, outside the card: tour gone, Terminal in front |
| `tour-again.png` | `help tour` in the Terminal |
| `help-paint.png` | the panel opened with the `?` button of the EuroPaint window |
| `help-files.png`, `help-notes.png`, `help-clock.png`, `help-settings.png`, `help-calc.png`, `help-agent.png`, `help-text.png`, `help-monitor.png`, `help-log.png`, `help-terminal.png` | F1 with that app in front |
| `help-overview-1.png`, `help-overview-2.png` | "EuroOS at a glance" (two screens) |
| `help-security.png`, `help-security-2.png` | "Security badges and terms" |
| `shortcuts-1.png` … `shortcuts-3.png` | the shortcut overview (F2), three screens |
| `tooltip-paint-bucket.png`, `tooltip-paint-save.png`, `tooltip-files-delete.png`, `tooltip-help-button.png` | tooltips inside EuroPaint and EuroFiles and on the `?` button; `tooltips.png` is the four cut out and tiled |
| `terminal-help.png` | `help` by topic (cut-out of the Terminal window) |
| `terminal-help-cmd.png` | `help ls`, `help fetch` (alias `wget`), an unknown command, `help tour`, and the flag file after "Do not show again" |

What the second run shows about the tour staying out of the way: the first
click (on the Terminal, outside the card) closes the tour and still focuses
the Terminal; `help tour` brings it back and the `h` of the next command
closes it while the whole command arrives (`help ls` runs); "Do not show
again" writes the flag file.

### Host tests

`cargo test -p eurokeymap`: 7 passed (Esc on every layout; F1/F2 decode to no
character).

## Open

- The texts describe the apps at `a36ef4b`. T2 (settings, toolkit) and T4
  (paint, textedit, webview) are changing apps in parallel: the EuroBeheer,
  EuroPaint, EuroText and EuroWeb pages, and `tooltip::paint_tip` (a mirror
  of the EuroPaint toolbar layout), must be re-read when those branches land.
- "Do not show again" is a flag file; move it to the preferences store when
  T2 delivers it.
- English only. The texts are data, a second language is a second table.
- No tooltips in EuroBeheer (T2 is rewriting it) and none on page content in
  EuroWeb.
- The panel was looked at in 1920x1080. The geometry clamps to smaller
  screens and the `[help]` marker checks that the tour cards fit 1280x720,
  but there is no screenshot of a small screen.
- The old inline `help` branch in the Terminal dispatch of `main.rs` is dead
  code now; removing it is a change for the owner of `main.rs`.
- Not fixed, only documented: Alt+Tab under `poll_key_ex`, context-menu key
  labels, Paste into EuroNotes, typed keys before the first click on the
  public image.

# Help, shortcut overview, first-run tour and in-app tooltips

Branch `feature/help` (2026-11). Proof: [`docs/proof/help-2026-11/`](proof/help-2026-11/README.md).

## What the user gets

| Entry | Result |
|---|---|
| **F1** | Help panel on the page of the active window (F1 again closes it) |
| **?** in a title bar | The same, for that window |
| **F2** | Keyboard shortcut overview (system-wide keys, mouse, then per app) |
| `help` in the Terminal | Commands by topic |
| `help <command>` | One line of explanation and an example |
| `help tour` | The first-run tour again |
| First desktop of a session | A six-step tour (dock, launcher, Terminal, EuroBeheer, help) |
| Pointer rests on a control | Tooltip, now also inside EuroPaint, EuroFiles, EuroNotes, EuroView, EuroText, EuroWeb, EuroReken |
| `/home/euro/Welcome.txt` | Short text version, points to F1 |

The panel has a page list on the left (three general pages, then the apps) and
the text on the right. Keys: Esc closes, Up/Down and PgUp/PgDn scroll,
Left/Right or Tab change page, Home/End jump. The mouse wheel scrolls the page
(`help::wheel`). A click outside the panel closes it; a click on the text
scrolls. While the panel is open the workspace pager is not drawn (on a
720-line screen the panel reaches down to it).

## Files

| File | Role |
|---|---|
| `kernel/src/help_text.rs` | All texts as data: `PAGES` (title, summary, sections, keys per app), `SYSTEM_KEYS`, `POINTER_KEYS`, and `CMDS` for the shell. Nothing else has to change for a new language. |
| `kernel/src/help.rs` | The panel (layout, wrapping, input), the `?` button, the hooks for the desktop loop, `help` in the shell, the `[help]` self-test |
| `kernel/src/onboarding.rs` | The tour |
| `kernel/src/tooltip.rs` | `app_tip`: the tooltip of a control inside an app window |
| `files.rs`, `notes.rs`, `imageview.rs` | `tip_at` next to their own layout code |

Small additions elsewhere: `ps2.rs` (F1/F2 latches, `peek_scancodes`),
`mouse.rs` (`peek_press`), `bootopt.rs` (`ui.tour`), `xhci.rs` (F1/F2 in the
USB HID table), `compositor.rs` (one call that draws the `?`), `shell.rs` (the
`help` arm now calls `help::shell_help`), `crates/eurokeymap` (Esc, see below).

### Hooks in `main.rs`

All marked `// T3-hook`; nothing else in `main.rs` changed except the text of
`WELCOME_TXT`.

| Where | Line |
|---|---|
| module list | `mod help; mod help_text; mod onboarding;` |
| `tooltip_for` | `help::tooltip_hook(..)` first: no tooltips under an open overlay, the `?` button, in-app tooltips |
| boot self-tests | `help::selftest();` |
| desktop loop, before input handling | `help::pump(..)`: F1/F2, panel input, tour |
| window click, before the title buttons | `help::title_button_at(..)` → `help::open_for(..)` |
| Terminal command dispatch | `help::shell_line(..)` before the built-ins (the old inline `help` branch is now unreachable and can be removed by the owner of `main.rs`) |
| full repaint, after the switcher | `help::render(..)` |

## Rules for the texts

- Describe what the code does today. Every statement was checked against the
  app's source when written (table in the proof README). When an app changes,
  its page changes in the same commit.
- Name the limits (EuroPaint saves to one fixed file, EuroClock has no
  controls, EuroAgent runs a scripted model, ...). No promises, no "we".
- English, plain ASCII (the same strings are printed in the Terminal).

### Adding or changing a page

1. Edit or add a `Page` in `help_text.rs`. `id` is what `help::page_for`
   returns for the window kind; `keys` also feeds the shortcut overview.
2. A new window kind: add it to `help::page_for` and to `KINDS` in
   `help::selftest`. A new launcher entry: `help::page_for_icon`.
3. `[help]` in the boot log must stay OK: it checks that every window kind
   and every launcher entry has a page with content.

### Adding a tooltip

Give the app a `tip_at(win_x, win_y, .., mx, my) -> Option<&'static str>` next
to its hit-test code and call it from `tooltip::app_tip`. EuroPaint has no
public hit test; its toolbar layout is mirrored in `tooltip::paint_tip` and
must be changed together with `paint.rs`.

## The tour

- Shown when the desktop loop starts, unless: the image is the self-test
  image (`feature = "selftest"`), the boot option `ui.tour=off` is set
  (`EUROOS_BOOTOPTS="ui.tour=off"` at build time, or `euroos-boot.conf` on the
  ESP), or the user has `tour.done=true`.
- It waits for the install wizard: a live medium with a blank disk opens
  EuroInstall on the first desktop, and the tour is decided and shown when
  that window is closed (`help::pump` tells `onboarding::pump` whether a
  visible window is the wizard). Serial log: `[tour] waiting: the install
  wizard is open; the tour starts when it is closed`. `help tour` shows the
  tour at once, wizard or not.
- `tour.done` is a key in the user's desktop preferences
  (`~/.config/euroos/desktop.conf`, see `SETTINGS.md`), set by "Do not show
  again" and by finishing the tour, through `prefs::open_user`. Without a
  session nothing is saved and the log says so. On the preview image the file
  system is in memory, so the tour returns after a restart; on an installed
  system it stays away for that user.
- It never holds input that is not meant for it: Esc, the arrow keys and
  clicks on the card are consumed; any other key and any click outside the
  card close the tour and are left in place for the desktop. An unattended
  script that types or clicks loses nothing.
- Cost at boot: one read of `desktop.conf` in the first loop iteration (in the
  first iteration without the wizard, when that is open).
- Serial log: `[tour] first-run tour shown ...`, `[tour] closed ...`,
  `[tour] do not show again: tour.done=true saved in <file>`.
- Test harnesses: `gui-smoke.py`, `wizard-install-test.py` and `screenshot.py`
  press Esc when the log says the tour is up, also when that is after the
  wizard was closed (gui-smoke counts it as a step; `wizard-install-test.py`
  checks that the tour waits;
  `EK_TOUR=keep` keeps the card in a `screenshot.py` picture); `boot-time.py`
  writes `ui.tour=off` into its private image copy unless `--tour keep`.

## Keyboard notes

- F1/F2 are latched in `ps2::push_scancode`, the one place PS/2, USB HID and
  virtio-input pass, so they work whichever consumer drains the scancode ring.
- Esc was not decoded at all (`eurokeymap::control_key` had no entry for
  scancode 0x01), so every `'\u{1b}'` arm in the desktop (launcher, file
  dialog, prompts, calculator, address bar) was dead on a real keyboard. It is
  mapped now, with a host test.
- AltGr (right Alt, `E0 38`) selects the third level of the Belgian, French
  and German layouts (`eurokeymap::translate_ex`) in `poll_key` and
  `poll_key_ex`, and is never Alt (BUG-052). The `0xE0` prefix is remembered
  across polls. Dead keys do not combine; Shift+AltGr is not mapped.
- Esc also closes a context menu and the notification list: the desktop loop
  takes that Esc out of the key ring (`ps2::take_pending_esc`), so the window
  that has the keyboard does not act on it as well.
- Known gaps, also listed on the shortcut page: Alt+Tab and Alt+1..4 are not
  seen while a program that drains raw scancodes has the keyboard (a
  full-screen app, an X11 or Chromium window, an installed app in front;
  `poll_key` and, since BUG-044, `poll_key_ex` latch them); the key names
  at the right of context-menu items are labels only; there are no Ctrl
  shortcuts.

## Repaint

The panel and the tour are static. While one is up, the loop is asked for a
full repaint only after input and once after each 2 s status tick (the tick
redraws the status panel and live window bodies in place).

The other overlays follow the same rule since BUG-057 (`main.rs`, the block
before the repaint decision): a context menu, the launcher, the file dialog,
the symbol picker, the Alt-Tab switcher, the notification list and the toasts
are repainted when what they show changes (their open flags,
`notify::active_toasts`, `notify::generation`, the menu row under the pointer),
after the input they handle, and once per 2 s tick. A key in the Terminal or
the calculator, which normally redraws that one window in place, becomes a full
repaint while something is drawn above the windows. The 2 s housekeeping
(`reap_dead`, service and driver supervision, log flush, scrub, update check)
runs on every tick, whatever is painted. The `repaints` shell command prints
the number of full repaints and loop iterations since boot.

Still one full repaint per event: every pointer move while a file is dragged
out of EuroFiles (the ghost follows the pointer), every change of the
highlighted row of a context menu, every pointer move while a window is
dragged or resized or a stroke is painted in EuroPaint.

# Settings: storage, toolkit and the Settings panel

This document describes three things that belong together:

1. **`prefs`** — where EuroOS stores settings, and how an app stores its own.
2. **`euroui::form`** — the small toolkit a settings screen is built from.
3. **EuroBeheer (Settings)** — the panel itself: its sections, what each
   setting does, and where it is saved.

Code: `crates/europrefs/` (file format, host-tested), `kernel/src/prefs.rs`
(storage), `kernel/src/euroui.rs` + `uikit_form.rs` + `uikit_draw.rs`
(toolkit), `kernel/src/settings_ui.rs` (panel). Proof of the first delivery:
`docs/proof/settings-2026-11/`.

## 1. Settings storage (`prefs`)

### Files

| Scope | Path | Opened with |
|---|---|---|
| System-wide, per app | `/etc/euroos/<app>.conf` | `prefs::open_system(fs, "<app>")` |
| Per user, per app | `/home/<user>/.config/euroos/<app>.conf` | `prefs::open_user(fs, "<app>")` |

`<user>` is the user of the active session (`session.rs`). The per-user file
and the directories created for it are owned by that user. An app name is
1 to 32 characters of `a-z 0-9 _ -`, starting with a letter or digit; anything
else (dots, slashes, upper case) is refused, so a name cannot leave the
settings directory.

On the live image the root file system lives in memory: settings work for the
session and are gone at shutdown. On an installed system they survive a
restart. The Settings panel says which of the two applies.

### Format

Plain text, one `key=value` per line:

```text
# EuroOS system settings
time.zone=Europe/Brussels
net.httpd=false
```

- Keys: 1 to 64 characters of `a-z A-Z 0-9 . _ -`. Use `group.name`.
- Values: the rest of the line, trimmed (spaces, tabs, CR), at most 4096 bytes.
  Escapes: `\n`, `\r`, `\t`, `\\`, and `\s` for a space that must survive
  trimming.
- Lines starting with `#` or `;` are comments and are kept when the file is
  rewritten. A repeated key: the last one wins.
- A line that is none of these is skipped and counted (`rejected()`); it is
  never an error. A damaged or binary file yields defaults. Nothing in this
  path can panic. At most 512 entries and 64 KiB are read.

### Using it from an app

```rust
let mut p = prefs::open_user(ctx.fs, "notes");        // never fails
let wrap = p.get_bool("editor.wrap", true);           // default if unset or unreadable
let size = p.get_i64_in("editor.font_px", 14, 8, 48); // default if out of range
p.set_bool("editor.wrap", !wrap);
match p.save(ctx.fs) {
    Ok(_) => {}
    Err(prefs::PrefsError::NoSession) => { /* nobody signed in: keep it in memory */ }
    Err(e) => { /* show it; the setting still applies for this session */ }
}
```

Getters (`europrefs::Conf`, reached through the `Prefs` handle): `get`,
`get_str`, `get_bool` (`true/false`, `yes/no`, `on/off`, `1/0`), `get_i64`,
`get_i64_in`, `get_u64`, `get_choice` (index into a list of allowed strings),
`get_list` (comma separated). Every getter takes the default and returns it
when the key is missing or the value does not parse.

Setters: `set`, `set_bool`, `set_i64`, `set_u64`, `set_list`, `remove`. A
setter returns `false` when the key or value is not storable; nothing changes
then.

`save` creates missing directories, writes `<file>.tmp` and renames it over the
old file, so a crash leaves either the old or the new content
(`Saved::Atomic`). On a file system without rename it writes in place and
returns `Saved::Direct`. EuroFS has rename, on disk and on the RAM root.

Rules of thumb for app authors:

- Read with a default; never assume a key exists.
- Apply the change first, then save, and tell the user if saving failed.
- Keep one file per app. Do not write into another app's file.
- Things every user of the machine shares go to `open_system`; preferences go
  to `open_user`.

`prefs::write_system_file(fs, path, bytes)` is for the Settings panel only: it
writes a single-purpose `/etc` file atomically in system context and, if the
file carries the immutable flag (`/etc/hostname` does), clears and re-sets that
flag through `immutable::set_protected`, which audits both steps.

### What the system itself stores

| File | Key | Meaning | Applied |
|---|---|---|---|
| `/etc/euroos/settings.conf` | `time.zone` | Zone id from `europrefs::tz::ZONES` (`UTC`, `Europe/Brussels`, …) | boot (`prefs::boot_apply`) |
| | `net.httpd` | Built-in web server on (`true`) or off | session start |
| `/etc/euroos/euroguard.conf` | `domains.added` | Domains the user blocked on top of the system policy | boot |
| | `domains.removed` | System-policy domains the user unblocked | boot |
| | `app.N` | `<app>\|blocked` or `<app>\|allow:<a.b.c.d/len>;…` | boot |
| `~/.config/euroos/desktop.conf` | `clock.24h` | 24-hour clock (`true`) or 12-hour with AM/PM | session start (`prefs::session_apply`) |
| | `display.wallpaper` | `euro`, `sand` or `mist` | session start |
| | `ui.text_scale` | Text size of form-based panels, 90 to 130 (percent) | session start |
| | `tour.done` | `true` once the first-run tour was finished or "Do not show again" was chosen (`onboarding.rs`; not in the panel) | first desktop-loop iteration |

Single-purpose files that already existed keep their place and format, and the
panel writes them: `/etc/keymap` (layout tag, read at boot by `main.rs`),
`/etc/locale.conf` (`LANG=<tag>`), `/etc/hostname`, `/etc/euroupdate.conf`
(update policy, owned by `update.rs`). The EuroGuard system policy stays in
`/etc/euroguard/system.conf`; only the user's changes relative to it are in
`euroguard.conf`, so a newer system policy still takes effect.

EuroGuard rule changes are saved wherever they are made: the panel saves at
once, and the desktop loop saves within one iteration after a shell command
changed a rule. Changes made before the desktop runs (boot self-tests) are not
treated as the user's and are not saved.

## 2. The form toolkit (`euroui::form`)

A form is a scrollable column of items. An item has a label, an optional
explanation (`hint`: what the control does and what follows from it) and a
kind:

| Kind | Constructor | Event |
|---|---|---|
| Sub-heading | `Item::heading(text)` | |
| Paragraph (wrapped) | `Item::text(text)` | |
| Read-only value | `Item::info(label, value)` | |
| Block of lines | `Item::lines(vec)` | |
| Usage bar | `Item::meter(label, pct, value)` | |
| Switch | `Item::toggle(id, label, on)` | `Toggled(id, new)` |
| Checkbox | `Item::checkbox(id, label, on)` | `Toggled(id, new)` |
| Dropdown | `Item::choice(id, label, options, selected)` | `Chosen(id, index)` |
| Slider | `Item::slider(id, label, min, max, step, value, unit)` | `Slid(id, value)` |
| Text field | `Item::field(id, label, value, placeholder, max_len)` | `Submitted(id, text)` on Enter |
| Button | `Item::button(id, label, caption, primary)` | `Pressed(id)` |
| Selection list | `Item::list(id, label, rows, selected, visible)` | `Selected(id, index)` |
| Divider | `Item::divider()` | |

Add `.hint("…")` for the explanation and `.enabled(false)` for a control that
cannot be used right now (say why in the hint).

The pattern is: build the items from the real state, every time; keep one
`FormState`; apply events to the real state.

```rust
static FORM: Mutex<FormState> = Mutex::new(FormState::new());
const ID_WRAP: Id = 1;

fn items() -> Vec<Item> {
    vec![Item::toggle(ID_WRAP, "Wrap long lines", wrap_is_on())
        .hint("Long lines continue on the next line instead of scrolling sideways.")]
}

pub fn render(fb: &FrameBuffer, area: Rect) {
    form::draw(fb, area, &items(), &FORM.lock());
}
pub fn click(area: Rect, x: usize, y: usize) {
    if let Some(Event::Toggled(ID_WRAP, on)) = form::click(area, &items(), &mut FORM.lock(), x, y) {
        set_wrap(on); // the next render shows it
    }
}
pub fn key(area: Rect, k: ps2::Key) {
    let ev = form::key(area, &items(), &mut FORM.lock(), k);
    // same handling as in click
}
```

Behaviour:

- **Mouse:** a click focuses the control and acts on it. A switch or checkbox
  reacts to a click anywhere on its row. A click on the slider track sets the
  value. A click outside an open dropdown closes it.
- **Keyboard:** Tab / Down / Up move the focus (a 2 px accent ring shows it),
  Space or Enter activates, Left / Right change a slider or dropdown, Enter
  applies a text field and Esc drops the edit, PageUp / PageDown / Home / End
  scroll. In a field: Left, Right, Home, End, Backspace, Delete.
- **Scrolling:** the framebuffer has no clip rectangle, so an item that does
  not fit completely is not drawn and scrolling moves by whole items. The
  scrollbar pages on a click above or below the thumb. The mouse wheel calls
  `form::scroll(…, delta)`, one item per notch (`settings_ui::scroll`, from the
  desktop loop when the pointer is over the panel).
- **Text size:** `euroui::set_text_scale(90..=130)` scales text and controls of
  every form.
- `form::rect_of`, `form::hit` and `form::popup_option_center` give the
  position of an item, for tooltips and test harnesses.

Colours and radii come from `eds.rs`, text from `text::draw_px`. Switch state
is also written out ("On"/"Off"), so it does not rely on colour.

`euroui::Widget` + `draw_panel` (the static stack that existed before) are
still there for read-only panels and now draw with the same text renderer.

## 3. The Settings panel (EuroBeheer)

`main.rs` forwards input with three calls, marked `// T2-hook`:
`settings_ui::click(fs, window, x, y)`, `settings_ui::key(fs, window, key)` and
`settings_ui::tick(fs)` once per desktop-loop iteration. Two more hooks load
settings: `prefs::boot_apply(fs)` after the EuroGuard system policy, and
`prefs::session_apply(fs)` after `session::open`.

With nothing focused, Up / Down move between sections and Tab enters the form.

| Section | Setting | Effect | Saved in |
|---|---|---|---|
| System | Computer name | Name in the shell prompt; validated (letters, digits, hyphens, 1 to 63) | `/etc/hostname` |
| | About | Version, heap, tasks, uptime, session user, live or installed | read-only |
| Display | Desktop background | One of three light backgrounds, at once | `desktop.conf` (user) |
| | Text size in settings | 90 to 130 %, for form-based panels | `desktop.conf` (user) |
| Language & keyboard | Keyboard layout | US QWERTY, Belgian AZERTY, French AZERTY, German QWERTZ; at once | `/etc/keymap` |
| | Try the layout | Shows what the keys produce; stores nothing | |
| | Regional formats | Number, currency and date format (24 EU languages) for apps that use EuroLocale; preview shown | `/etc/locale.conf` |
| Date & time | Time zone | Clock in the status panel, lock screen and EuroClock; EU summer time automatic | `settings.conf` (system) |
| | 24-hour clock | `14:05` or `2:05 PM` in the same places | `desktop.conf` (user) |
| Network | Built-in web server | Port 80 status page on or off | `settings.conf` (system) |
| | Connection | Live interface state | read-only |
| EuroGuard | Block a domain / Unblock | Name lookups of that domain refused for all apps | `euroguard.conf` |
| | Status, policy, recent activity | Live | read-only |
| Apps | Block all network access | The selected app cannot connect | `euroguard.conf` |
| | Revoke all | Withdraws the app's portal grants | portal state |
| Storage | Usage, disks | Per mount (sizes with one decimal below 100 of a unit); every disk a driver has brought up (virtio-blk, SATA, NVMe, MegaRAID logical drive, USB) with name, size, bus and model, the system disk marked (`rootblk::all_disks`) | read-only |
| | Verify all files | Runs `scrub::run` now; result and log line shown | `/var/log/fsck.log` |
| Updates | When an update is available | `auto`, `ask` or `manual` policy | `/etc/euroupdate.conf` |
| | Check now | Starts the background check of `update.rs`; without update slots it only looks | |

Known limits, stated in the panel where they apply:

- The interface is English only. Regional formats do not translate menus.
- Accent colour, dark theme and display scaling are not offered: the design
  tokens are compile-time constants.
- The time-zone list is the EU zones plus UTC with the EU summer-time rule,
  not the IANA database. The hardware clock is read as UTC. Linux-compatible
  programs still get UTC from `/etc/localtime`.
- Self-test builds force the lab keyboard default (`be-azerty`) late in boot,
  after `/etc/keymap` was applied; release images do not.
- A disk behind a controller without a kernel driver is not in the Storage
  list, and neither is an NVMe disk handed to the user-space driver
  (`storage.nvme=user`).
- The userspace view of `/etc` (the copy `ring3` registers at boot) is not
  refreshed when the panel rewrites a file; a Linux-compatible program sees the
  new host name, keymap or locale after a restart.

## Boot self-test markers (selftest builds)

| Marker | Proves |
|---|---|
| `[prefs]` (two lines) | What was loaded at boot and at session start |
| `[prefs-rt]` | Typed round trip, atomic save, overwrite |
| `[prefs-bad]` | Damaged and binary files: intact lines kept, no panic, next save clean |
| `[prefs-user]` | Per-user files: separate, user-owned, path escape refused |
| `[uikit]` | Form logic: hit-test, Tab order, dropdown, slider, field, scroll |
| `[set-keymap]` | Layout picked by click and by key changes the decoder and `/etc/keymap` |
| `[set-time]` | Zone and clock format change the clock string, are saved and re-read |
| `[set-host]` | Host name typed into the field is written, protection flag kept |
| `[set-guard]` | Blocked domain and per-app rules survive save, reset, load |
| `[disks]` | The size formatting of the Storage page; every listed disk has a name, a size and a bus; the system disk is marked when the root is on a disk |

Host tests: `cargo test -p europrefs`.

# Sprint plan: guided installer (install EuroOS on a clean machine)

Status: ACTIVE, started 2026-10-03. Owner: Jeroen. Built without the NUC: every
exit criterion is measured in QEMU.

## Why

The public image is a live image: it boots the desktop from the stick and leaves
the disk alone. Installing EuroOS on a laptop today means the shell command
`euroinstall --to N`, with a fixed example user "anke" and no password. Real
adoption (a school pilot, an SME laptop) needs what every other OS has: boot the
stick, answer a few questions, create your own account, reboot into it.

## What exists (reused, not rebuilt)

- `crates/euroinstall`: the planner (partition layout, validation, ordered steps).
- `kernel/src/instexec.rs`: the executor (GPT, FAT32 ESP with loader + A/B kernel
  slots, EuroFS root, provisioning, TPM-sealed FDE enrol) on virtio, NVMe and AHCI.
- `kernel/src/installer.rs`: a one-button window that shows the plan.
- EuroID (`kernel/src/euroid.rs`, `crates/euroid`): Argon2id users, policy,
  persistence to `/etc/euroid/users.db`, lockscreen login.

## Gaps this sprint closes

1. No wizard: the window has one button and a hard-coded example config.
2. The installer writes a `/etc/passwd` line but no EuroID record, so the
   installed user has no password and cannot log in. The lockscreen still runs
   on the built-in demo store (`euro`/`euro`, `alice`, `bob`).
3. The demo store is also loaded on an installed system: `euroid::selftest()`
   replaces the identity store on every boot, in every build.
4. A blank disk is installed to silently at boot (`default_config`), and a blank
   disk 0 gets a GPT written as the live root. A live stick must never write to a
   disk before the user confirms.
5. The lockscreen auto-logs in after 0.6 s on an unattended boot. Right for the
   live image and CI, wrong for an installed system.
6. No way to reboot into the installation from the wizard.

## Work items

- W1 `euroinstall`: `Config.password` (never logged, never stored in clear);
  validation; host tests.
- W2 EuroID: `init_from_disk(fs)` at boot (users.db present = installed system,
  demo store otherwise); the self-test no longer overwrites a loaded store;
  `install_db(name, password)` builds the store for a fresh install (root locked,
  first user in `wheel` = admin); `default_login_user()`.
- W3 `instexec`: provisioning writes `/etc/euroid/users.db`; verification after
  remount checks it; a progress callback so the wizard can paint live progress.
- W4 Wizard in `installer.rs`: Welcome, Language, Keyboard, Disk, Account,
  Encryption, Summary, Installing, Done. Keyboard and mouse. Only blank disks
  can be selected; in-use disks are shown but disabled. Keyboard layout applies
  live when chosen. Password checked against the EuroID policy before Next.
- W5 Boot flow: silent install and blank-disk GPT only in `selftest` builds; the
  public image boots live and the wizard is the only path that writes a disk.
  The wizard window opens on a live boot when install media is present; it is
  also in the launcher as "Install EuroOS".
- W6 Lockscreen: default user = the installed user; no unattended auto-login
  when the store came from disk (installed system). Live image unchanged.
- W7 Harness `scripts/wizard-install-test.py`: drives the wizard over QMP keys
  on the public image profile, then boots the target alone and logs in with the
  typed password.

## Exit criteria (all in QEMU, public image profile)

1. Boot the live image with a blank 4 GiB virtio disk: desktop appears, the
   wizard is open, nothing is written to the disk until "Install".
2. Complete the wizard with keyboard only: language, keyboard, disk, hostname,
   username, password (policy enforced), encryption step, summary, install.
   Serial shows `[q1x3] ... OK` and the Done page offers Restart.
3. Boot the target disk alone: lockscreen shows the created user, no auto-login,
   the typed password signs in, serial shows `[lock] login successful: <user>`.
4. Wrong password on the installed system is rejected.
5. Host tests green (`euroinstall`, `euroid`); dev build boot self-tests still
   PASS (`[q1x]`, `[q1x3]`, `[k1]`, `[ae]`, `[ag-lock]`).

## Log

- 2026-10-03: W1-W7 built. First QEMU runs (public profile, 1536 MiB, TCG):
  the wizard completes keyboard-only, the executor reports `[q1x3] ... OK`
  with the EuroID store verified on the target, the target boots alone, the
  store loads from disk, the lockscreen offers the created user and does not
  auto-login. Found and fixed on the way: the identity self-test replaced a
  store loaded from disk (every build), the persistence self-test and the FDE
  self-test wrote the live root of an installed system, the lockscreen
  must-change check compared against a Dutch string, QEMU needs 1.5 GiB
  because the kernel's static image is 610 MB. Open at this point: the typed
  password is rejected on the installed system while a wrong one is also
  rejected. Root cause (found with a one-off character-class trace, removed
  again): the `[q1x]` installer self-test provisions a RAM disk with the default
  config and used to switch the LIVE keyboard to be-azerty as a side effect, so
  the installed system's lockscreen decoded the digits and `!` as AZERTY. Fix:
  provisioning only persists `/etc/keymap`; the kernel applies `/etc/keymap`
  at boot. Result: `scripts/wizard-install-test.py` 14/14 PASS on the public
  image profile (exit criteria 1 to 4).

- 2026-10-03, exit criterion 5: host tests green (euroinstall 8, euroid 29).
  The dev build's full boot self-test suite in QEMU (TCG, 1.5 and 3 GiB) stops
  with a kernel general-protection fault right after a glibc multi-thread test
  (a ring-3 thread executes an invalid opcode at a stack address, then the
  kernel faults). The same fault occurs on the untouched HEAD e7b9e4d built in
  a separate worktree, with and without a target disk, so it predates this
  sprint and is tracked separately. The markers this sprint touches pass
  before that point: `[install]`, `[q1x]`, `[q1x3]`, `[euroid]`, `[ps2]`.
- 2026-10-05: that fault is fixed (kernel: a faulting thread now ends its whole
  thread group as on Linux, and a timed-out program is killed before its stacks
  and address space are freed). The dev build's full self-test suite now runs
  to the desktop in QEMU: exit criterion 5 met. Side effect made visible: the
  gpango self-test dies with SIGSEGV (a worker thread writes to address 0x10);
  the old per-thread kill hid that, it is a pre-existing bug to fix separately.

## Out of scope (next)

Unattended install from a signed answer file (fleet), recovery passphrase for
FDE on machines without a TPM, dual-boot alongside another OS, resizing
partitions, Wi-Fi during install.

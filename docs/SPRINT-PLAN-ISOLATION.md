# Sprint plan: process isolation for the Linux-ABI layer (WP1)

Status: ACTIVE, started 2026-10-05. Verified in QEMU; the Chromium part also on
the NUC when it is back.

## Why

Every compat (glibc) process runs with the session's identity and the full set
of implemented Linux syscalls; Chromium runs with `--no-sandbox`. A compromised
renderer is therefore a compromised session. EuroGuard already decides what a
native app may do (capabilities, network policy); this sprint gives the kernel
the same say over Linux-ABI processes, without Linux namespaces.

## Design

- A **syscall profile per task** (`eurosandbox::syscalls`, host-tested):
  `Full` (default), `Renderer` (no new processes or program images, no inbound
  or non-AF_UNIX sockets, no identity change, no file-system modification, no
  System V IPC, no tracing/mounts/modules), `Strict` (Linux seccomp strict:
  read, write, exit, sigreturn).
- Inheritance: threads from their creator, fork children from the parent. An
  `execve` with `--type=renderer|gpu-process|utility` gets `Renderer`; the
  child cannot opt out. A process can lower itself with
  `prctl(PR_SET_SECCOMP, SECCOMP_MODE_STRICT)` or `seccomp(SET_MODE_STRICT)`.
- Enforcement at the top of the Linux-ABI dispatcher: a syscall outside the
  profile ends the whole process through the exit_group path, status
  128 + signal (SIGKILL for strict mode, SIGSYS for a profile violation), with a
  `[sbx]` audit line. W^X mappings under `Renderer` are logged, not yet refused.
- Filter mode (`SECCOMP_SET_MODE_FILTER`, classic BPF) is refused with EINVAL
  for now, so Chromium's own sandbox code falls back cleanly.

## Work items

- W1 ✅ Profiles + verdicts in `crates/eurosandbox/src/syscalls.rs` with tests.
- W2 ✅ Kernel wiring: per-task table, inheritance, dispatcher check, prctl/seccomp
  strict, renderer profile at execve, `[sbx]` self-test with `userland/glibc/gsandbox`.
- W3 ⬜ Measure the renderer profile on a real Chromium run (QEMU chrome-boot
  profile, then NUC): no `[sbx] ... killed` for a page load; count W^X mappings.
- W4 ⬜ W^X enforcement where the measurement allows; `mprotect` audit.
- W5 ⬜ Address randomisation for the demand pool (mmap/stack/brk bases).
- W6 ⬜ Real rlimits (NOFILE, NPROC, AS) and a per-process uid for compat
  processes, so file ownership and EuroGuard decisions apply per process.
- W7 ⬜ Classic-BPF interpreter for `SECCOMP_SET_MODE_FILTER`, so Chromium can
  run its own layer-2 sandbox (its layer-1 namespace sandbox stays replaced by
  the kernel profile).

## Log

- 2026-10-05: W1+W2 landed (commit d6c5c63). Host tests: eurosandbox 23 green.
  Dev build in QEMU: `[sbx] syscall profiles: ... OK`, the strict-mode open()
  kills the process with 137 (`[sbx] task 38 (strict): syscall 257(...) outside
  its profile -> process killed (signal 9, exit 137)`) and never returns into
  the program; LINUX COMPAT 30/34 (was 28/32); the suite reaches the desktop.

- 2026-10-05, W3 first measurement (QEMU TCG, `scripts/chrome-desktop.sh`,
  Chromium 152 from the signed chrome-pack2): the browser spawned 3 utility and
  2 renderer children, each got the renderer profile at execve. Findings:
  (1) both renderers call `mprotect(prot=RWX)` (V8's JIT), so W^X cannot be
  enforced for renderers without V8 flags (`--jitless` or PKU write-protect):
  stays log-only; (2) one renderer was killed on `unlink` of its own
  unlinked-but-open shared-memory file: unlink/unlinkat/rmdir are now allowed
  under the renderer profile (what may be deleted is the VFS uid check);
  (3) the kill log now names the path. Harness note: the dev/self-test boot
  used to be in Belgian AZERTY by accident (the `[q1x]` provisioning side
  effect); it is now set explicitly for self-test builds because every harness
  types AZERTY. The wizard window no longer auto-opens in self-test builds (a
  pack disk counts as install media and the window stole the terminal's focus).

- 2026-10-05, W3 second measurement with the relaxed profile: 2 utility + 2
  renderer children under the profile, 0 kills, 2 W^X logs, same 21-minute
  budget. Under TCG the browser window does not map within that budget, so
  "page renders with every child under the profile" (criterion 3) still needs
  the KVM/NUC run; the launch path is clean.

## Exit criteria

1. Host tests green for `eurosandbox`.
2. Dev build: `[sbx] ... OK` (strict-mode open kills the process with 137 and
   never returns), `LINUX COMPAT` count up by two, the suite still reaches the
   desktop.
3. Chromium renders euro-os.eu with every child under the renderer profile and
   no profile kill in the log (W3).
4. Public image still passes `scripts/wizard-install-test.py`.

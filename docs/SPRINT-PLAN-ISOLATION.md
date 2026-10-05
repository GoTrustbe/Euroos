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
- W4 🟡 W^X: measured on Chromium, renderers mprotect their JIT pages RWX, so
  enforcement stays log-only (`WX_SEEN`) until V8 is run with write-protected
  code; the strict profile never reaches mmap/mprotect at all.
- W5 ✅ Address randomisation: per image a CSPRNG slide of the brk base
  (64 KiB steps, 1 MiB), the mmap bump base (64 KiB steps, 2 MiB), the stack
  top (4 KiB steps, 1 MiB) and the demand pool (2 MiB steps, 2 GiB). The exe
  image itself still loads at a fixed base (next: slide the PIE base).
- W6 ✅ Resource limits (`eurosandbox::rlimits`, Linux semantics: soft never above
  hard, hard never raised): get/set/prlimit64 per process, `RLIMIT_NOFILE`
  enforced on every descriptor-returning syscall (EMFILE), `RLIMIT_NPROC` on
  clone/clone3/fork (EAGAIN). Per-process uid: `getuid`/`getresuid` report it,
  `setuid`/`setreuid`/`setresuid` may only drop to 65534 (nobody); fork
  children inherit, execve keeps. Because the compat VFS has no ownership
  model, the renderer's isolation is a **file-system scope** instead:
  open/stat/access/readlink/execve of a path outside `/pack /lib /usr /etc
  /bin /tmp /dev /proc /sys /run` (and never `/etc/euroid`, `/etc/shadow`,
  `/etc/fde`, `/etc/euroguard`, `/etc/vault`) fails with EACCES and is logged.
- W7 ✅ Classic BPF for `seccomp(SECCOMP_SET_MODE_FILTER)` and
  `prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER)` (`eurosandbox::bpf`): validated
  at load like Linux (forward jumps only, aligned 32-bit loads inside
  `seccomp_data`, 4096 instructions max), newest filter first, most restrictive
  action wins (KILL_PROCESS > KILL_THREAD > TRAP > ERRNO > ... > ALLOW).
  ERRNO returns the errno, LOG logs, TRAP/KILL end the process (SIGSYS, 159),
  TRACE/USER_NOTIF become ENOSYS (no tracer). Filters are per process,
  inherited on fork, kept across execve; `SECCOMP_GET_ACTION_AVAIL` answers.

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

- 2026-10-05, second half: `[sbx2]` self-test in QEMU: cBPF filter (getpid ->
  EPERM, socket -> killed 159), RLIMIT_NOFILE=8 -> 5 opens then EMFILE and a
  hard-limit raise -> EPERM, ASLR 3/3 addresses differ across two runs,
  renderer scope (/etc ok, /home EACCES) and uid drop to 65534. One iteration
  was needed: setuid under the renderer profile was in the deny list, but a
  drop to nobody must be allowed there. LINUX COMPAT 32/38.

## Exit criteria

1. Host tests green for `eurosandbox`.
2. Dev build: `[sbx] ... OK` (strict-mode open kills the process with 137 and
   never returns), `LINUX COMPAT` count up by two, the suite still reaches the
   desktop.
3. Chromium renders euro-os.eu with every child under the renderer profile and
   no profile kill in the log (W3).
4. Public image still passes `scripts/wizard-install-test.py`.

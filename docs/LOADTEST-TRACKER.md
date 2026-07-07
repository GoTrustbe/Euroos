# EuroOS — Load & Functional Test Tracker

*Companion to the load-test plan. This is the **living record** of what the harness
([`scripts/loadtest.py`](../scripts/loadtest.py)) runs, the **baselines** each run is
checked against (§7), the **bug-report template** (§8) for anything it surfaces, and the
**status board + bug log** that tie a run's CSV back to a tracked issue.*

**Harness in one line.** `loadtest.py` drives the EuroOS shell over COM1 via the in-kernel
serial console (`scon`): it streams command lines into a bidirectional QEMU serial socket
and parses the `[scon-ready]` / `[scon] $ <cmd>` / `[scon] <line>` framing. The loop lives
host-side. Drives are **persistent and disk-backed — never `-snapshot`** (so persistence
tests can't pass for the wrong reason), and the full serial stream is captured to a
timestamped log per run.

**Crash detection.** A run is `CRASH` if the serial stream matches
`KERNEL PANIC | [PANIC] | panicked at | DOUBLE FAULT | GENERAL PROTECTION FAULT`, or `HANG`
if the console goes silent past the per-command timeout. The regex is **deliberately tuned
not to trip on EuroOS's benign boot self-tests** (`breakpoint exception handled`,
`[isolation] … TERMINATED`, `[j3-fault]`, …) — see BUG-001 below for the false-positive
that drove that tuning.

---

## How to run

```bash
# build the bootable image first (scripts/build.sh), then:
python3 scripts/loadtest.py --scenario smoke
python3 scripts/loadtest.py --scenario fs
python3 scripts/loadtest.py --scenario users --users 50
python3 scripts/loadtest.py --scenario fs --disks "64M,2G"   # attach the §2 disk matrix
```

Each run writes `loadtest-results.csv` (one row per command: `id, command, status,
elapsed_ms, output_lines`) and `loadtest-serial-<ts>.log` (full capture). Exit code is
`0` = clean, `1` = crash/hang, `2` = could not attach the serial socket.

---

## §7 — Baselines (expected results per scenario)

Pass criterion per command is **status `ok` + output matches** the expectation below. These
expectations are anchored to a **boot-verified** run over the live serial console (the
`✓` rows were observed end-to-end). Anything diverging from a baseline is a finding → log
it with the §8 template.

### 7.1 `smoke` — liveness & core tools

| # | Command | Expected (baseline) | Verified |
|---|---------|---------------------|:--------:|
| 1 | `uname` | `EuroOS` | ✓ |
| 2 | `free` | one memory line (total/used/free), non-zero total | ✓ |
| 3 | `df` | root filesystem row with a sane used/avail | ✓ |
| 4 | `eurohealth` | health score `100/100` | ✓ |
| 5 | `ps` | at least the shell/desktop processes listed | ✓ |
| 6 | `ls /` | root entries present (no error) | ✓ |
| 7 | `echo serial-console-alive` | `serial-console-alive` | ✓ |

### 7.2 `fs` — filesystem writes, symlinks, copy, scrub

| # | Command | Expected (baseline) | Verified |
|---|---------|---------------------|:--------:|
| 1 | `mkdir /lt` | ok, no error | ✓ |
| 2 | `write /lt/a.txt hello-eurofs` | ok | ✓ |
| 3 | `cat /lt/a.txt` | `hello-eurofs` | ✓ |
| 4 | `sha256sum /lt/a.txt` | 64-hex digest = `bdb1526…` (host-cross-checked: `sha256sum` of `hello-eurofs\n`) | ✓ |
| 5 | `ln -s /lt/a.txt /lt/link` | ok (symlink created) | ✓ |
| 6 | `readlink /lt/link` | `/lt/a.txt` | ✓ |
| 7 | `cat /lt/link` | follows symlink → `hello-eurofs` | ✓ |
| 8 | `cp /lt/a.txt /lt/b.txt` | ok | ✓ |
| 9 | `ls /lt` | `a.txt  b.txt  link` | ✓ |
| 10 | `df` | usage reflects the new files | ✓ |
| 11 | `scrub` | scrub completes, 0 errors | ✓ |

> **Persistence check (with `--disks`):** re-run `fs` against the *same* persistent disk;
> `cat /lt/a.txt` must still return `hello-eurofs` on the second boot (writes survived,
> because the harness never uses `-snapshot`).

### 7.3 `users` — EuroID at volume + audit chain (the §B 50-user scenario)

| # | Command | Expected (baseline) | Verified |
|---|---------|---------------------|:--------:|
| 1 | `eurohealth` (before) | `100/100` | ✓ |
| 2 | `sudo eurousers add userNNN LoadtestPw!NNN users,net` × 50 | each prints `created (uid=…)` (Argon2id) | ✓ (uid 1005–1054) |
| 3 | `eurousers list` | lists all 50 created users (+ pre-existing) | ✓ (50 present) |
| 4 | `eurousers audit --verify-chain` | hash-chain **verifies** (tamper-evident log intact) | ✓ (20→70 events, intact) |
| 5 | `free` / `eurohealth` (after) | memory stable, health still `100/100` | ✓ |

> **Note the privilege + policy preconditions** (learned the hard way — see BUG-004/005/006):
> `eurousers add` needs `sudo` (the session is the unprivileged `euro` user), a **≥12-char**
> password, and a **real group** (`wheel/audit/net/vault/agent/users` — not `staff`). All
> three are now baked into the scenario, and each add is positively asserted on
> `created (uid=`.

### 7.4 Disk matrix (§2) — sizes attached via `--disks`

| Size | Attach | Expected |
|------|:------:|----------|
| 64 MiB | `--disks "64M"` | mounts, `fs` scenario passes, persistence survives reboot |
| 2 GiB | `--disks "2G"` | as above |
| 64 GiB | `--disks "64G"` | as above (sparse-allocated host file) |

> Matches the multi-disk range already proven in the interop sprint (8 MiB → 64 GiB). The
> harness only **attaches** the disks here; per-size functional assertions get `✓` as each
> is captured.

---

## §8 — Bug-report template

Copy this block per finding into the **Bug log** below, and add a machine-readable row to
[`scripts/loadtest-bugs.csv`](../scripts/loadtest-bugs.csv).

```
### BUG-NNN — <one-line summary>
- **Status:**   OPEN | IN-PROGRESS | FIXED | WONTFIX | DUPLICATE
- **Severity:** CRITICAL (data loss / panic / hang) | HIGH | MEDIUM | LOW | HARNESS | DOCS
- **Found:**    <date> · scenario `<smoke|fs|users>` · command `<cmd>` · disks `<spec>`
- **Expected:** <baseline from §7>
- **Actual:**   <what happened — quote the serial line / CSV status>
- **Repro:**    python3 scripts/loadtest.py --scenario <…> [flags]   (then: <step>)
- **Artifacts:** loadtest-serial-<ts>.log : line <n> · loadtest-results.csv : row <id>
- **Analysis:** <root cause once known — file:line>
- **Fix:**      <commit / file:line / "tightened regex" / N/A>
```

**Severity guide.** `CRITICAL` = anything that loses data, panics, or hangs the kernel —
stop the matrix and triage first. `HARNESS` = the test rig is wrong, not the OS (still
worth fixing so results stay trustworthy). `DOCS` = a claim/figure is stale or contradicts
the code.

---

## Status board

| Scenario | Last run | Result | CSV / serial log | Notes |
|----------|----------|--------|------------------|-------|
| `smoke`  | 2026-06-15 06:20 | ✅ clean (7/7 ok) | `loadtest-smoke.csv` · `loadtest-serial-20260615-062040.log` | all 7 match §7.1; health 100/100 |
| `fs`     | 2026-06-15 06:22 | ✅ clean (11/11 ok) | `loadtest-fs.csv` · `loadtest-serial-20260615-062229.log` | all 11 match §7.2; **sha256 digest cross-checked against host** (`hello-eurofs\n` → `bdb1526…`); scrub HEALTHY |
| `users` (50) | 2026-06-15 06:42 | ✅ clean (56/56 ok) | `loadtest-users.csv` · `loadtest-serial-20260615-064236.log` | **50/50 created (uid 1005–1054)**; list shows all 50; **audit chain 20→70, intact**; took 3 fixes (BUG-004/005/006) |
| disk matrix `64M,2G` | 2026-06-15 (fixed) | ✅ **0/27 after fix** | `loadtest-diskmatrix*.csv` · `/tmp/repro-*.log` | was ~20–40% intermittent hang (**BUG-007**); root-caused to a `reap_dead`/`BG.lock` deadlock and **fixed** — 27/27 clean post-fix |

> Fill `Last run` / `CSV` as runs land. A row stays ⬜ until a clean CSV exists — don't mark
> it ✅ from "it should work".

---

## Bug log

### BUG-001 — crash-detector false-positive on benign boot self-test
- **Status:**   FIXED
- **Severity:** HARNESS
- **Found:**    2026-06-14 · scenario `smoke` · during boot, before first command
- **Expected:** boot self-tests print diagnostic lines; harness should not flag them as crashes
- **Actual:**   the initial crash regex matched `breakpoint exception handled` (a deliberate
  `[isolation]` self-test line) and aborted the run as a CRASH
- **Repro:**    earlier broad regex (`exception|fault|panic`) over the boot stream
- **Analysis:** EuroOS prints intentional fault-handling self-tests at boot (`breakpoint
  exception handled`, `[isolation] … TERMINATED`, `[j3-fault]`); a loose regex can't tell
  them from a real panic
- **Fix:**      `scripts/loadtest.py` — `CRASH_RE` tightened to only
  `KERNEL PANIC | [PANIC] | panicked at | DOUBLE FAULT | GENERAL PROTECTION FAULT`, with a
  comment listing the benign lines to never match

### BUG-002 — stale/contradicting test-count figures across docs (§9)
- **Status:**   FIXED
- **Severity:** DOCS
- **Found:**    2026-06-15 · doc audit (plan noted "docs say 484, roadmap 755"; real = 793)
- **Expected:** every *current* test-count claim reads the same number (793 host tests)
- **Actual:**   living docs cited 484 / 690 / 188 as the *current* count; "755" was the
  interop-sprint end-state recorded in the roadmap/memory, not a doc string
- **Analysis:** living "version/build/document of record" headers and the STATUS §1.8
  breakdown had not been refreshed after Phase-3; dated milestone lines (181/255/484/717/
  730/744/750) are correct point-in-time history and were left intact
- **Fix:**      updated the *current*-claim lines to **793** in `STATUS.md`, `README.md`,
  `NEXT-SPRINTS.md`, `docs/TECHNICAL-OVERVIEW.md`, `docs/AGENT-BRIEFING.md`,
  `docs/EUROOS-DEEP-TECHNICAL-REFERENCE.md`, `IMPLEMENTATION-PLAN.md`, `SPRINTS.md`;
  reframed STATUS §1.8 so 188 reads as a subset of 793, not the total

### BUG-003 — harness marked denied commands `ok` (`status=ok` ≠ succeeded)
- **Status:**   FIXED
- **Severity:** HARNESS
- **Found:**    2026-06-15 · scenario `users` · command `eurousers add userNNN …`
- **Expected:** a command that returns to the prompt but did not achieve its intent reads
  as a failure, not `ok`
- **Actual:**   the first `users` run reported **56/56 `ok`** while all **50** `eurousers add`
  commands were **denied** (`EPERM — requires CAP_USER_ADMIN`). The harness only checked
  "did the shell come back," not "did the command succeed" — i.e. it passed for the wrong
  reason, the exact trap §7/§8 exist to prevent
- **Repro:**    `python3 scripts/loadtest.py --scenario users --users 50` (before fix)
- **Analysis:** `SerialConsole.run()` set `status=ok` whenever the next `[scon-ready]`
  arrived, ignoring the command's own output
- **Fix:**      added `ERR_RE` (errno tokens + `requires CAP_` + `Permission denied` +
  `not in the sudoers` + `command not found` …) scanned over each command's output in
  `run()`; a match now yields `status=err`. `main()` counts `err` rows and exits non-zero.
  **Validated:** 0 false positives over the green `smoke`/`fs` logs; catches exactly the 50
  EPERM lines. (Errno list is explicit, not a generic `E[A-Z]+`, to avoid re-introducing a
  BUG-001-style false positive on words like `EUPL`/`EUROPOL`.)

### BUG-004 — `users` scenario omitted `sudo`, so all 50 adds were denied
- **Status:**   FIXED
- **Severity:** MEDIUM (scenario bug, not an OS defect)
- **Found:**    2026-06-15 · scenario `users` · command `eurousers add userNNN …`
- **Expected:** the 50-user scenario actually creates 50 users
- **Actual:**   every add returned `eurousers: EPERM — requires CAP_USER_ADMIN (wheel group)`;
  `eurousers list` showed none of user001–050
- **Analysis:** **correct OS behaviour, not a bug** — the serial/desktop session runs as the
  unprivileged `euro` user (uid 1000, groups `users,net`), which is deliberately *not* in
  `wheel`; user administration requires `CAP_USER_ADMIN`. There is a `sudo` path and `euro`
  is in the sudoers. The scenario simply forgot to use it (Zero-Trust least-privilege caught
  a privileged op run without elevation — arguably a *good* sign)
- **Fix:**      `scenario_users()` now issues `sudo eurousers add …`; re-run to confirm the
  creates land and `eurousers audit --verify-chain` covers them

### BUG-005 — `users` passwords violated the 12-char policy
- **Status:**   FIXED
- **Severity:** MEDIUM (scenario bug)
- **Found:**    2026-06-15 · scenario `users` (after the BUG-004 `sudo` fix)
- **Actual:**   with `sudo` working (`[sudo] '…' as root:`), all 50 adds were still rejected:
  `eurousers: password too short (at least 12 characters)` — the `Pw!NNN` passwords are 6
  chars. **The denylist `ERR_RE` did not catch this** (no matching token) → harness still
  said 56/56 ok. Second leak.
- **Fix:**      scenario password → `LoadtestPw!NNN` (14 chars); `ERR_RE` broadened with
  `too short|too long|too weak|…`

### BUG-006 — `users` used a non-existent group `staff`; denylist leaked a *third* time
- **Status:**   FIXED
- **Severity:** MEDIUM (scenario bug) + the harness lesson
- **Found:**    2026-06-15 · scenario `users` (after the BUG-005 password fix)
- **Actual:**   adds rejected `eurousers: unknown group 'staff'` (built-in groups are
  `wheel/audit/net/vault/agent/users` — no `staff`). `eurousers list` still showed **zero**
  of user001–050; audit chain still 20 events. **`ERR_RE` missed it again** → 56/56 ok.
- **Analysis:** three different real failures (`EPERM`, `password too short`,
  `unknown group`) each passed an error-*denylist* — the denylist is the wrong tool for
  intent verification.
- **Fix:**      (1) scenario group → `users,net`; (2) **added positive baseline assertions**
  — scenario items may be `(cmd, expect_substring)` and the substring MUST appear or the row
  is `err`. Each add now asserts `created (uid=`, `eurousers list` asserts the last user is
  present, `audit --verify-chain` asserts `chain intact`. Smoke/fs got positive asserts too
  (`uname`→`EuroOS`, `eurohealth`→`100/100`, `cat`→`hello-eurofs`, `scrub`→`HEALTHY`, …).
  **Validated by offline replay:** smoke/fs still 0-flagged; all three failed users runs are
  now caught. This is the durable fix; ERR_RE stays as a backstop.

### BUG-007 — timer-vs-`SCHED` deadlock (scheduler lock not interrupt-safe) — FIX #2 applied
- **Status:**   ROOT CAUSE FOUND (deadlock, not a fault) · fix #2 applied · validating
- **Severity:** CRITICAL (kernel hang)
- **The "instrument & catch the fault" step paid off indirectly:** the fault handlers showed
  ring-3 faults print `[isolation] … TERMINATED` and ring-0 GP/PF/DF all print
  `GENERAL PROTECTION FAULT` / `DOUBLE FAULT` before halting. We saw **none of those — pure
  silence** ⇒ it's **not a fault at all**, it's a **spinlock deadlock**.
- **Root cause (definitive, from the code):** `sched::schedule_tick` (the APIC-timer
  interrupt path, `sched.rs:272`) took a **blocking** `SCHED.lock()`. Task-context code — the
  desktop loop via `sched::current` / `reap_dead` / `is_pid_alive` / `mark_*`, all `SCHED.lock()`
  — runs with **interrupts enabled**. When the timer fires inside that window, the handler
  spins on `SCHED` **with interrupts off**, while the only task that can release it is the one
  it just preempted ⇒ **hard deadlock, total silence, no fault**. Intermittent (timer must hit
  the narrow locked window) and load-sensitive (the disk matrix's heavier early loop + extra
  IRQ activity widens it) — exactly the observed behaviour. The cr3 theory (fix #1) was a red
  herring: that path completes before the silence.
- **Fix #2:** `schedule_tick` now uses `SCHED.try_lock()` and, if the lock is held, **skips
  that preemption tick** (returns the current rsp unchanged) instead of blocking — the holder
  frees it in microseconds and the next tick schedules normally. A latched
  `[sched-guard] … (BUG-007 deadlock averted)` line + `SCHED_SKIPS` counter prove the window
  is real when it's hit. File: `kernel/src/sched.rs`. (The AP-timer path has the same shape;
  not exercised here since only the BSP core is online — noted as a follow-up.)
- **Validation of fix #2:** baseline ~20%; fix #1 (cr3) 4/16; **fix #2 (SCHED try_lock): 1/16**
  — a real reduction, and the `[sched-guard]` line fired in a log, proving the SCHED window
  is hit & averted. But **1 hang remained** ⇒ a *second* deadlock of the same class.

### BUG-007b — second deadlock: input-ring lock (`SCANCODES`/`PACKET`) not IRQ-safe — FIX #3
- **Status:**   FIX #3 applied · validating (target 0/N)
- **Found:**    the lone fix-#2 hang had **`sched-guard` count = 0** (not the SCHED path) and
  wedged right after `[ioapic] keyboard IRQs received` — a different lock.
- **Root cause:** `ps2::push_scancode` did a **blocking** `SCANCODES.lock()`, and it runs in
  **two** contexts: the PS/2 keyboard IRQ *and* task context (the USB-HID harvest
  `xhci::poll → poll_inner`, `xhci.rs:799/801/806`, called from the desktop loop at
  `main.rs:2620`). `pop_scancode` is `without_interrupts`-safe, but the **task-side push in
  `poll_inner` is not** — so a PS/2 IRQ landing while the loop holds `SCANCODES` deadlocks
  (IRQ spins, interrupts off, holder suspended). Same hazard in `mouse::push_byte` (`PACKET`).
- **Fix #3:** both `push_scancode` and `push_byte` use `try_lock` and **drop on contention**
  (one lost input byte is harmless) — the IRQ side can never block. Files: `kernel/src/ps2.rs`,
  `kernel/src/mouse.rs`.
- **Systemic note:** BUG-007 was a *class* — blocking `spin::Mutex`es shared between IRQ
  handlers and interrupt-enabled task code. Three instances fixed: `SCHED` (timer),
  `SCANCODES` (keyboard/USB-HID), `PACKET` (mouse). A lock-discipline audit of all
  IRQ-reachable `*.lock()` sites is a worthwhile follow-up.
- **Found:**    2026-06-15 · scenario `smoke --disks "64M,2G"` · first command `uname`
- **Expected:** console processes commands after `[scon-ready]`
- **Actual:**   **1 of 2 runs** hung. The kernel booted fully, mounted/formatted the 2 GiB
  disk-1 EuroFS at `/mnt`, emitted `[desktop] interactive loop started` + `[scon-ready]` —
  then the harness sent `uname` and the loop went **silent for 60 s**. `uname` was never
  echoed (`[scon] $ uname` absent). Last log lines: the first `bg-musl` (pid 101) launch +
  `service ticker stopped -> restart (1/3)`. The re-run with the identical config passed
  clean (7/7), `uname` echoed right past that same point.
- **Repro:**    `python3 scripts/loadtest.py --scenario smoke --disks "64M,2G"` — ~1/2 so far
- **Repro rate:** baseline **1 hang / 8** runs on the pre-fix image (plus the original 1/2)
  ≈ ~15–20% on this config — confirmed real, not a one-off.
- **Root cause (confirmed from the code + log):** a **task-creation race**. `sched::spawn_user`
  set the new ring-3 task to `State::Ready` (schedulable) while its `cr3` was still **0**
  (the table default); the real `cr3` was installed *afterwards* by a separate
  `set_task_cr3` call at each of the 3 call sites (`ring3.rs` daemon / `spawn_bg_musl` /
  counter-demo). In the window between those two steps, a **timer-driven preemption** can pick
  the new task, and the scheduler's `switch` falls back to the **boot PML4** when `cr3==0`
  (`sched.rs`: `let next_cr3 = if …cr3 != 0 { … } else { boot }`). The task then resumes its
  ring-3 code on the boot PML4, where the user arena is **supervisor-only → fault → hang**.
  This explains every symptom: intermittent (needs the IRQ inside a ~dozen-instruction
  window), single-core (only preemption enters it), and **aggravated by the disk matrix**
  (the extra virtio-blk MSI-X completion IRQs perturb timing and widen the window). Not OOM
  (~805 MB free), not disk-data — the disks were only ever an aggravator.
- **Fix attempt #1 (cr3-before-Ready) — FAILED.** Changed `spawn_user` to take `cr3` and set
  it before `State::Ready` (mirrors `spawn_thread`); reordered the 3 call sites; removed
  `set_task_cr3`. **Post-fix rate: 4/16 (~25%) — no improvement over the ~20% baseline.** The
  change is a legitimate latent-bug hardening (a task should never be runnable with `cr3=0`)
  and is being **kept as defensive hardening**, but it is **NOT** the cause of BUG-007.
- **Why the cr3 theory was wrong:** in the captured hung logs, `spawn_bg_musl` had already
  **completed** (its final line printed, cr3 installed) *before* the silence — and 2 of 3
  post-fix hangs wedge **before any `bg-musl` spawn at all**, right after the first
  `[ioapic] keyboard IRQs received` line. So the hang is not the task-creation window.
- **Corrected picture (still open):** an intermittent wedge/fault in the **early desktop loop**
  (first 1–2 iterations), correlated with the **disk matrix** (extra virtio-blk MSI-X IRQs),
  hanging at **no single fixed point**. Open leads: (a) the boot log shows **both** virtio-blk
  disks programmed to MSI-X **vector 0x47** with `queue_msix_vector readback=0x0000` (vector
  write may not stick → misrouted completion IRQ); (b) full-screen present-blit runs
  113–224 ms/iter under TCG — a long window for a stray IRQ; (c) possible IRQ-vs-lock or
  spurious-vector fault. **Next:** instrument the IRQ path / check the second disk's MSI-X
  programming; consider an isolation run with `--disks 2G` alone vs `64M` alone to see if the
  rate tracks disk count or the 2 GiB `/mnt` mount specifically. Do NOT mark the disk matrix
  green.

### BUG-007c — THE root cause: `reap_dead` holds `BG.lock()` across preemptible freeing ✅ FIXED
- **Status:**   ✅ **FIXED & VALIDATED** — 0 hangs in 27 post-fix disk-matrix runs (15 instrumented
  + 12 clean), vs ~20–40% before. Clean build, heartbeat scaffolding removed.
- **How it was found (the "instrument & catch" step, done right):** after 3 wrong guesses,
  added a non-blocking (`write_raw`/`try_lock`) **heartbeat** at each desktop-loop step. Every
  hang stopped at the **same** marker: `[hb:pre-reap]` (then silence) ⇒ the wedge is inside
  `ring3::reap_dead`. Finer markers inside it showed two signatures, both with `BG` held.
- **Root cause (proven):** `reap_dead` (task 0 / desktop loop) takes `BG.lock()` and holds it
  across the **heavy, preemptible** frame-freeing (up to 512 `falloc.free` + page-table teardown
  per dead process). When the APIC timer preempts task 0 mid-free, the scheduler switches to a
  `bg-musl` process; its **`syscall_dispatch` (ring3.rs:2922) also takes `BG.lock()`** → it spins
  on the lock still held by suspended task 0, which can't run to release it → **silent core
  deadlock**. Intermittent (timer must hit the BG-held window *and* land on a syscalling task);
  disk-matrix-aggravated (heavier early loop + more process churn widen the window). The three
  earlier `try_lock` fixes didn't touch this — in fact the SCHED `try_lock` (fix #2) *enabled*
  the preemptive switch that exposes it.
- **Fix #4:** `reap_dead` now extracts the zombies under a **short, `without_interrupts`** `BG`
  critical section, then frees their frames/page-tables with **`BG` released**. The lock is held
  only for the O(n) list splice (no preemption possible), and the heavy freeing runs with `BG`
  free, so a preempted bg-process syscall can take `BG` normally. File: `kernel/src/ring3.rs`.
- **Validation:** **0 hangs / 27** post-fix disk-matrix runs (15 on the instrumented build, then
  12 on the clean build with the heartbeat removed) — vs ~20–40% before. `[hb:pre-supv]` printed
  in every instrumented run, i.e. `reap_dead` now completes past the old wedge point.
- **Final state:** heartbeat scaffolding removed from `main.rs`; the changes kept are
  `reap_dead` (the fix) + three IRQ-safety hardenings (`SCHED`/`SCANCODES`/`PACKET` `try_lock`) +
  the cr3-before-`Ready` latent-bug hardening + the `BG` irqsave hardening pass (below).
- **Systemic hardening — DONE.** Every **task-context** `BG.lock()` holder now holds it under
  `without_interrupts` (irqsave): `reap_dead`, `is_pid_alive`, `bg_lines`, `ps_lines`, `kill_pid`,
  `spawn_bg_musl` registration. So no holder is ever suspended while holding `BG`. The two
  unwrapped sites are already interrupts-off by construction: `syscall_dispatch` (the `syscall`
  insn clears IF via **FMASK=0x200**, ring3.rs:3601) and `note_isolation_kill` (called only from
  the GP/page-fault handlers). `BG` is now effectively an "irqsave spinlock" — correct on single
  core (no preemption while held) and still SMP-correct (the spinlock itself). Built clean.

### fsstress scenario (2026-06-15) — FS robustness battery on EuroFS `/mnt` (2 GiB)
New `fsstress` harness scenario (`scripts/loadtest.py`): directory **depth**, **many files**/dir,
**special-char names** (round-tripped), **large files**, and **edge paths**. Run:
`python3 scripts/loadtest.py --scenario fsstress --disks 2G --fsbase /mnt`. 261/270 ok.

**Input-layer limits discovered while building it (not OS bugs, but coverage gaps):**
- **scon is ASCII-only** — accepts `0x20–0x7e`, drops the rest, so **Unicode filenames can't be
  driven over serial**. A small scon UTF-8 upgrade (accumulate bytes, decode at line end) would
  unlock it.
- **shell has no quoting** (`split_whitespace`) — **spaces in names** aren't expressible.

**Passed (good robustness):** all printable-ASCII specials `! # $ % ^ & * ? ( ) [ ] { } ; : ' ~
, + = @ \` < >`, dots/`..dd`/`...` round-trip byte-for-byte; **case-sensitive** (`CaseX`≠`casex`);
edge paths (`ls`/`cat`/`mkdir` on bad targets) error gracefully; **depth 64** works (0 crashes/
hangs across 270 cmds — BUG-007 fix holds under heavy FS load).

**Bugs found:**
- **BUG-008 (HIGH) — FIXED ✅:** EuroFS **silently truncated filenames > 48 bytes**
  (`DIRENT_NAME_CAP`, disk.rs:663 `name.min(48)`); `write` reported success but `cat <full
  name>` → `NotFound`. **Fix:** a `check_name()` guard at all four create paths
  (`write_file_impl`/`create_dir`/`create_symlink`/`rename`) rejects over-long names with
  `InvalidPath` instead of truncating. Validated: long names now return `InvalidPath`; 89/89
  eurofs host tests pass.
- **BUG-009 — NOT A BUG (withdrawn):** the apparent "can't allocate > 2 MiB" was a **test-config
  error**, not an OS bug. `/mnt` is the **second** virtio-blk disk (device 1), so it only mounts
  with **two** disks; I ran with `--disks 2G` (device 0 only) → `/mnt` was never mounted and
  writes fell to the **8 MiB root**, which filled at ~4 MiB. **Caught by the new `fsdebug`
  diagnostic** (`blocks=2048` = 8 MiB, not ~524 000). Re-run with `--disks 64M,2G`: `/mnt` is the
  real 2 GiB volume (`blocks=524032`) and **1/4/16/64 MiB files all succeed**. EuroFS large files
  are fine. *Lesson (again): instrument before fixing — the dump prevented a wrong "fix".*
- **PERF-001 (MEDIUM, open):** path resolution is **O(depth)** (no dentry/path cache) → deep-tree
  ops are O(N²); `mkdir` time grew 2 s→17 s from depth 1→46 (TCG). A resolution cache flattens it.

**New tooling:** added an **`fsdebug <path>`** shell command + `FileSystem::alloc_debug` (EuroFS
impl) dumping total/free blocks, **largest contiguous free run**, free-run count, and the
used-block distribution — the diagnostic that resolved the BUG-009 confusion in one run. The
`fsstress` scenario now logs fs size up front and documents the **`--disks 64M,2G`** (2-disk)
requirement for `/mnt`.

<!-- Add BUG-NNN entries above this line. Keep newest at the bottom of the relevant group. -->

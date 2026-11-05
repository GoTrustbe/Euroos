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
# KVM and the load scenarios (LOADTEST-R630; full list in LOADTEST-R630-2026-11.md §7):
python3 scripts/loadtest.py --kvm --smp 4 --mem 4G --workdir /tmp/p --scenario procs
python3 scripts/loadtest.py --kvm --smp 4 --mem 4G --workdir /tmp/f --disks 8G --disk-bus ahci --scenario fsio
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
| `procs` (KVM, `-m 4G`) | 2026-11-03 | ✅ clean | `proof/loadtest-r630-2026-11/` | 3 × 64 workers + 300 burst + 32 pthread processes; after BUG-014/015/016/017/024 |
| `churn` scale 8 (KVM, `-m 16G`) | 2026-11-02 | ✅ clean | `proof/loadtest-r630-2026-11/` | 128 glibc procs, 8×60×4 threads, 4×6 forks, fs, net: counters back |
| `fsio` virtio / NVMe / AHCI (8 GiB) | 2026-11-02 | ✅ clean + reboot | `proof/loadtest-r630-2026-11/` | 3–10 MB/s; NVMe only with the BUG-021 workaround; BUG-022/023/025 found here |
| `net` e1000e kernel / user / user+IOMMU | 2026-11-02 | 🟡 | `proof/loadtest-r630-2026-11/` | outbound ~1 % failures and ~85 s blocks (BUG-026, open); inbound fixed (BUG-018), slow (BUG-019) |
| `net` + `inbound` on `feature/net-async` (KVM) | 2026-11-05 | ✅ | `proof/net-async-2026-11/` | outbound 400/400 (kernel), 300/300 (user, user+IOMMU), max 1.25 s; 4 httpd clients: 4 174 ok (user), 102 447 ok (kernel), console answers every second (max 633 ms); BUG-019/026 fixed |
| `soak` 1 h + 4 httpd clients on `feature/net-async` (KVM, `-m 16G`) | 2026-11-05 | ✅ 1 747/1 747 ok | `proof/net-async-2026-11/` | counters flat, heap +26 kB/min (BUG-027); httpd slow during long synchronous shell commands |
| `soak` 4 h 10 min (KVM, `-m 16G`) | 2026-11-03 | ✅ no crash, 1 failed fetch (BUG-026) | `proof/loadtest-r630-2026-11/` | first soak: kernel fault after 59 min (BUG-024, fixed); 2 h 20 min on the final image: 0 failures; heap +33 kB/min (BUG-027) |
| `soak` 1 h on the merged tree (KVM, `-m 16G`) | 2026-11-03 | ✅ 2 085/2 085 ok | `proof/integrate-loadtest-2026-11-03/` | after the merge into app-control (userspace NIC driver); tasks, files, frames flat; heap +31 kB/min (BUG-027) |
| lockbench KVM matrix | 2026-11-02 | ✅ 0 errors | `proof/loadtest-r630-2026-11/` | see `LOADTEST-R630-2026-11.md` §3 |
| `soak` 70 min, mem-hygiene (KVM, `-m 16G`) | 2026-11-05 | ✅ 2 478/2 478 ok | `proof/mem-hygiene-2026-11/` | heap +0.4 kB/min after the BUG-027 fix (+29.3 kB/min on the same machine before it); tasks, files, frames, process pool flat |
| `[sbx2]` and glibc at `-m 1G` (TCG) | 2026-11-05 | ✅ `[sbx2]` 15/15 | `proof/mem-hygiene-2026-11/` | BUG-028 fixed; GBIG, GSCM3's second fork and `[churn-fork]` still fail at 1 GiB (process pool too small, deterministic) |

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

### BUG-010 — flaky boot hang: heap + UART spinlocks not interrupt-safe (BUG-007 class, the two missed instances) ✅ FIXED
- **Status:**   ✅ FIXED (class-level) · validated by RIP-sampling + boot sweep
- **Severity:** CRITICAL (silent kernel hang, ~50%+ of TCG boots during the DOOM test campaign)
- **Found:**    2026-07-13 · the DOOM auto-retry harness turned "boots sometimes hang" into a
  measured rate; a new hang-catcher (`scratchpad/bootdbg.py`) samples RIP via QMP on stall.
- **Evidence (empirical, definitive):** hung boot mid-self-test-storm; **30/30 RIP samples at
  the exact same instruction** — `<LockedHeap as GlobalAlloc>::alloc +0x1e`, the heap-lock spin
  loop — with **RFLAGS.IF=0**. Symbolized offline via the new boot-time anchor line
  (`[euro] anchor dump_registers_and_backtrace @ …`, printed at boot so a hang needs no panic
  dump) + `scripts/symbolize.sh`.
- **Root cause:** the BUG-007c hardening pass covered `SCHED`/`SCANCODES`/`PACKET`/`BG` but
  missed the two most global locks in the kernel:
  1. **the heap** (`LockedHeap`, a plain spin::Mutex): IF=0 contexts allocate — the xHCI MSI-X
     harvest builds key-event `Vec`s (`eurousb kb.feed`), and **every bg-app syscall runs with
     IF=0** (FMASK clears IF) and allocates in the vfs paths. If such a context runs while a
     preempted task holds the heap lock → eternal spin, interrupts off, total silence.
  2. **the UART** (`serial::_print` blocking `UART.lock()`): the same IF=0 contexts also print
     (`[xhci-rpt]`, MSI-X-confirmed, `[doom-out]`, syscall traces) → identical deadlock on the
     console lock, hit-probability proportional to how much the boot task prints (a lot).
  Both windows are hit during the boot self-test print storm with bg-musl demos running —
  hence "varying self-tests, 100% CPU, no fault, load-sensitive".
- **Fix (rule, not patch):** every lock reachable from an IF=0 context is only ever held with
  interrupts disabled. `allocator.rs`: `IrqSafeHeap` wrapper — `alloc`/`dealloc` under
  `without_interrupts`. `serial.rs`: `_print` holds `UART` under `without_interrupts`.
- **Validation:** pre-fix baseline: 1 sampled hang + historical ~50%+ under load; post-fix
  boot sweep on the fixed image: see sweep tally in the session log (target 0/20). Host
  suite: 975 tests pass.

### BUG-011 — first keystrokes after boot intermittently lost (`push_scancode` try_lock drops) ✅ FIXED
- **Status:**   ✅ FIXED · validating with a first-keystroke harness
- **Severity:** HIGH (input loss, ~1-in-3 boots lost the first typed characters)
- **Root cause:** BUG-007b's fix #3 made `push_scancode`/`push_byte` use `try_lock` and **drop
  the byte on contention** — it traded the deadlock for silent data loss. Contention is real:
  the USB-HID harvest pushes scancodes from task context (desktop loop) *and* from the 100 Hz
  timer tick + xHCI MSI-X handler; a timer poll landing inside a task-side push window dropped
  a make/break code — a lost keystroke, worst right after boot when the loop runs hot.
- **Fix:** the definitive BUG-007-class rule instead of the lossy compromise: `SCANCODES` and
  `PACKET` are acquired **only under `without_interrupts` + a normal blocking lock** (irqsave).
  A holder can never be preempted on its CPU, so an IF=0 acquirer never spins on a suspended
  holder — deadlock-free **and lossless**. Files: `kernel/src/ps2.rs`, `kernel/src/mouse.rs`.
- **Note:** QMP-injected keys in the sandbox QEMU have a separate, host-side flakiness
  (send-key sometimes produces no HID report at all while a fullscreen app runs — diagnosed
  via QEMU tracing during the DOOM sprint); that is outside the kernel and not covered here.

### LOADTEST-R630 (2026-11-02) — KVM load test on a Dell R630, branch `feature/loadtest-r630`

Full report, numbers and limits: [`LOADTEST-R630-2026-11.md`](LOADTEST-R630-2026-11.md).
QEMU 10.2 with KVM (`-cpu host`), OVMF, selftest image (`scripts/build.sh release`).
The server carries a throw-away signing key, so the dev-key markers (`[upd3]`, `[3f1]`,
`[3e6]`, `[appstore]`, part of `[app]`) fail there by construction; they are not findings.
New harness pieces: `scripts/loadtest.py` (`--kvm`, `--workdir`, `--disk-bus`, `--nic`,
scenarios `procs`/`churn`/`fsio`/`net`/`soak`), `scripts/loadtest-lockbench.py`,
`scripts/loadtest-accept.py`, `scripts/loadtest-http-peer.py`, and the shell hooks
`ltstat`/`spawn`/`churn`/`httpd` (`kernel/src/ltstat.rs`).

### BUG-012 — more than 8 vCPUs: per-CPU slots alias, kwork silently single-core ✅ FIXED
- **Status:**   FIXED (`dcf87dd`)
- **Severity:** HIGH (wrong results, false self-test output; a real 2-socket machine has more than 8 threads)
- **Found:**    2026-11-02 · lockbench image · `-smp 16` under KVM
- **Expected:** at most 8 cores used, the others left offline and said so; `[smp]`/`[kwork]` correct
- **Actual:**   `[smp] parallel sum ... -> WRONG`; per-core lines for APIC ids 8..15 that never
  started (their counters were those of ids 0..7); `demo workers parked on 14/15`; `[kwork] no APs
  accepting work`, so every `kwork::parallel` job ran inline on the BSP and `[lk-bench]` printed
  "8 cores" figures that were single-core (scaling x0.99 for every workload).
- **Repro:**    `scripts/build.sh lockbench`, boot it with `-enable-kvm -cpu host -smp 16`
- **Analysis:** `smp::init` started only `MAX_AP`=7 APs but kept iterating all MADT cores; per-CPU
  state is indexed by `APIC id & 7` (`kernel/src/smp.rs`). Sparse APIC ids (multi-socket) alias the same way.
- **Fix:**      `euroacpi::select_cpus` (BSP first, at most 8, no two cores in one slot) + only
  online cores afterwards; `nproc` reports the cores in use. Host tests `select_cpus_*`.
  Proof: `-smp 16` → "using 8, 8 left offline", sum OK, `[kwork]` 7 APs OK.

### BUG-013 — `[lk-bench]` says "QEMU TCG" under KVM ✅ FIXED
- **Status:**   FIXED (`30a094c`) · **Severity:** DOCS (honesty label)
- **Actual:**   the start line was a constant string. **Fix:** label from CPUID (hypervisor bit,
  vendor leaf); a WARNING when APs are online but kwork does not accept jobs (BUG-012 shape).

### BUG-014 — reaped background processes never return their scheduler slot ✅ FIXED
- **Status:**   FIXED (`f215965`)
- **Severity:** CRITICAL (after ~230 process lifetimes since boot no new process can run)
- **Found:**    2026-11-02 · scenario `procs` (`spawn worker 64` + `spawn mpthread 16` per wave)
- **Expected:** `tasks` (scheduler slots in use) back to its high-water mark after every wave
- **Actual:**   `tasks` 25 → 106 → 188 → 256 over three waves; from then on spawned processes were
  registered but never scheduled (BUG-015).
- **Repro:**    `python3 scripts/loadtest.py --kvm --smp 4 --mem 4G --workdir W --scenario procs --waves 4`
- **Analysis:** `ring3::reap_dead` freed arena, kernel stack and address space but never called
  `sched::reclaim_task` for the process task or its threads.
- **Fix:**      reclaim the process and thread slots in `reap_dead` (dead threads' kernel-stack owner
  entries dropped first). After: `tasks` stays at its high-water mark (81) over 5 waves + a 300 burst.

### BUG-015 — full task table / OOM: ghost processes and a panic path ✅ FIXED
- **Status:**   FIXED (`ca1ac1b`) · **Severity:** HIGH
- **Actual:**   `spawn_bg_musl` registered a process with task `18446744073709551615` (usize::MAX)
  when the table was full: never ran, stayed "live" with its 2 MiB arena (93 such after BUG-014 hit).
  The bg clone path pushed a ghost thread (pthread_join waits forever), the bg fork path a ghost
  child (parent waits forever); `expect("bg-arena")`/`expect("bg-kstack")` panicked on OOM.
- **Fix:**      refuse cleanly: free what was taken, return false / `-EAGAIN`.
  Proof: `spawn worker 300` → "started 240 of 300 ... stopped early", all 240 reaped, frames back.

### BUG-016 — processes whose arena lies above 1 GiB die on their first instruction ✅ FIXED
- **Status:**   FIXED (`4cad76f`)
- **Severity:** CRITICAL (with ≥ ~2 GiB RAM almost every background process is killed)
- **Found:**    2026-11-02 · scenario `procs`, `-m 4G` under KVM
- **Expected:** a spawned worker computes its result and exits 0
- **Actual:**   `[isolation] ring-3 page fault ... INSTRUCTION_FETCH ... TERMINATED` for 398 of 400
  spawned processes; also the boot-time demo processes (tlscount, worker, forktest, forkpipe,
  mthread, ...) and the native bg-app tests (stack write fault). One page-table frame leaked per process.
- **Repro:**    any background musl process whose arena the frame allocator places above 1 GiB
  (the kernel heap fills low memory, so: any guest with more than ~1.5 GiB RAM)
- **Analysis:** `paging::build_address_space` refined only the PD of the lowest GiB (`arena/2MiB`
  ≥ 512 silently skipped); the arena stayed a supervisor-only 1 GiB identity page. Same assumption in
  `fill_remap_tables(_wx)`, `build_address_space_big/_rwx`, `arena_pt`, `table_frames`.
  Invisible under TCG with 512 MiB (every arena below 1 GiB).
- **Fix:**      the PD sits under the PDPT entry of the arena's GiB; helpers find it there.
  Proof: procs (5 × 64 workers + 300 burst): 0 isolation kills, frames exactly back (411100) each
  wave; selftest boot `-smp 4 -m 2G`: same OK/FAILED marker set as 813e627.

### BUG-017 — many musl pthread processes at once: pthread_join never returns ✅ FIXED
- **Status:**   FIXED (`4baf003`; diagnostic `bgps` in `c2ffef2`)
- **Severity:** HIGH (user processes hang forever; also the boot demos pid 13/14 on 813e627)
- **Found:**    2026-11-02 · scenario `procs` (`spawn mpthread 16`)
- **Expected:** each mpthread creates one thread, joins it, exits 0 within a second
- **Actual:**   12–16 of 16 never exit; 1 or 2 at a time usually finish. `bgps`:
  `pid 70000 task 35:Blocked thr 21:Dead ctids=0` — main Blocked, its thread gone, and the main
  task in no futex queue. The boot demos `/bin/mpthread` (pid 13) and `/bin/mmutex` (pid 14) were
  in the same state for the whole uptime.
- **Repro:**    `spawn mpthread 16`, wait 20 s, `bgps` / `ltstat` (bg_live above its boot value)
- **Analysis:** `futex_wait` on the musl bg path (no yield under `BG`) marks the task Blocked and
  should leave it queued until the next tick switches it out; the "resumed" code written for the
  yielding glibc path ran immediately, found the fresh entry, removed it as "spurious" and
  returned 0 — a Blocked task that no FUTEX_WAKE can reach once the joiner waited before the
  thread exited. (`kernel/src/ring3.rs`, `futex_wait`.)
- **Fix:**      return right after blocking on the bg path (musl re-checks the word); reap_dead drops
  the futex entries of reclaimed tasks (slots are reused, BUG-014). Proof: `procs --threads 32`
  clean, bg_live back to 2 (the two endless tlscount demos), pid 13/14 finish after boot.

### BUG-018 — inbound TCP never reached listen/accept or the background httpd ✅ FIXED
- **Status:**   FIXED (`3c27916`)
- **Severity:** HIGH (EuroOS could not accept any TCP connection since 2026-08-26)
- **Found:**    2026-11-02 · scenario `net` with `--hammer` (httpd `served 0`), then `tcpserve 8080`
- **Actual:**   accept() always timed out (0/3 with a client connecting in time).
- **Repro:**    `python3 scripts/loadtest-accept.py IMAGE WORKDIR 6`
- **Analysis:** the central RX demux (77309db) queues every TCP segment in PORTQ; the only
  passive-open code (`service()`: `try_accept_listener`, httpd) reads the legacy queue.
- **Fix:**      bare SYNs go to the legacy queue; `portq_pop` matches the peer port as well (several
  clients on one listening port); httpd reads its segments from PORTQ and purges them after.
  Proof: 6/6 accepted and answered (before 0/3).
- **After the merge into app-control (2026-11-03):** with `net.driver=user` (the default
  now) 6/6 accepted and answered, 0.26 s per connection under KVM; the NIC interrupt's
  drain (`rx_route_irq`) sends a bare SYN to the legacy queue too.
  Proof: `proof/integrate-loadtest-2026-11-03/`.

### BUG-019 — inbound TCP is handled synchronously; under client load the desktop loop starves ✅ FIXED
- **Status:**   FIXED (`feature/net-async`, `0d8a4c2`) · **Severity:** HIGH for any server use, LOW for the desktop
- **Actual:**   each accepted connection costs ~6.4–7.2 s (tcpserve, kernel driver) and ~37 s
  with `net.driver=user` under KVM: `accept_from`/httpd
  busy-poll the NIC (`SPINS`=4 M iterations per poll) in the desktop loop. With 4 host clients on
  the httpd: 71 ok / 62 errors, p99 5.9 s, `[3g2-wire] WATCHDOG TRIPPED - main loop hung`, and one
  run where the console stayed silent for over 60 s (harness HANG).
- **Repro:**    `scripts/loadtest.py --kvm --nic e1000e --restrict --hostfwd P --hammer 4 --scenario inbound --duration 300`
  (the `inbound` scenario: one console `echo` a second must answer within 2 s)
- **Analysis:** a SYN for the httpd or a LISTEN socket was served inside `service()` (the desktop
  loop): SYN-ACK, then polling for the ACK and the request, one connection at a time.
- **Fix:**      passive open without waiting (`kernel/src/net_passive.rs`): a SYN creates a half-open
  entry in a bounded table (`euronet::tcpopen::SynQueue`, host-tested: backlog, SYN-ACK
  retransmission 1/2/4 s, expiry, RFC 5961 resets) and sends the SYN-ACK; `passive::step` (from
  `service()` and `pump_all()`) completes handshakes, answers httpd requests, retires connections,
  never waits. Completed handshakes go to the listener's accept queue and wake accept(), which
  sleeps between looks. TCP receive queues are per connection (local port, peer, peer port),
  registered before the first segment leaves; a segment for no connection gets a rate-limited RST.
  Boot marker `[net-async]` (synthetic peers on TEST-NET-1: 16 concurrent httpd clients, a listen
  backlog). Proof (R630, 4 clients, 300 s): base 64 ok / 51 failed and the console silent for
  120 s; now 4 174 ok / 3 failed (`net.driver=user`, p99 751 ms, console max 633 ms) and 102 447 ok
  / 3 failed (`net.driver=kernel`, p99 21.6 ms). `proof/net-async-2026-11/`.
- **Left:**     inbound connections are still driven from the desktop loop, so a long synchronous
  shell or file-system command delays them (1 h soak with 4 clients: 4 384 ok, 641 failed, during
  `churn fs`/`truncate`/`sha256sum`).

### BUG-020 — `[fuzz]` fails under KVM: the fuzzer stops and run_glibc times it out ⬜ OPEN
- **Status:**   OPEN · **Severity:** MEDIUM (also on 813e627 → not a regression of this branch; the
  kernel keeps running)
- **Actual:**   `[glibc] gfuzz (syscall fuzzer): exit=0 (want 171)`. The fuzzer prints "5000 syscalls
  so far", then makes no further syscall:
  `[glibc] TIMEOUT waiting for the process to exit | main t27 last=106(a1=0x7fffffff)->...ffda`
  (setgid, ENOSYS), `timed-out program killed: main t27 (1 were still alive)`; the output shown is
  partly the previous program's (gsandbox), i.e. stale.
- **Repro:**    selftest boot, `-enable-kvm -cpu host -smp 4 -m 2G`, glibc signatures valid. The
  fuzzer's RNG seed is fixed, so the syscall sequence is the same on every run.
- **Analysis:** not root-caused. Under TCG (`-smp 4 -m 512M`) the same test passes on this branch
  and on 813e627. Next step: log the task state at the run_glibc timeout and every syscall of
  that run between round 5000 and 10000 (the stuck one leaves the task not Ready, or spinning in
  user space).

### BUG-021 — NVMe under KVM `-cpu host`: BAR above 512 GiB, kernel page fault at boot ⬜ OPEN
- **Status:**   OPEN (workaround for tests) · **Severity:** HIGH (boot halts)
- **Actual:**   `[idt] PAGE FAULT addr=0x380000000000 ... @ efi_main+0x54bc` right after
  `[epk] no pack files registered`, with an NVMe disk attached; the boot never continues.
- **Repro:**    `scripts/loadtest.py --kvm --disk-bus nvme --disks 8G --scenario fsio`
- **Analysis:** with host physical-address bits (46) QEMU/OVMF put the 64-bit PCI window at
  56 TiB; `nvme::init` dereferences BAR0 through the identity map, which covers only the lower
  512 GiB. Any 64-bit BAR placed that high has the same problem (the harness uses virtio
  `disable-modern=on`, legacy BARs). Real servers can place 64-bit BARs high too.
  Workaround: `--cpu-opts ,host-phys-bits=off,phys-bits=39` (window below 512 GiB): NVMe works.
  A fix needs an MMIO mapping facility for physical addresses above 512 GiB.
- **Harness:**  `CRASH_RE` now also matches `[idt] PAGE FAULT` (a ring-0 fault halts; healthy boots
  print only `[idt] BREAKPOINT`); before, the harness waited out the whole boot timeout.

### BUG-022 — every shell command read its last file argument into the heap ✅ FIXED
- **Status:**   FIXED (`b2e7260`) · **Severity:** HIGH (perf + heap exhaustion)
- **Found:**    2026-11-02 · scenario `fsio` · `stat` of a 256 MiB file: > 60 s (harness HANG)
- **Actual:**   `stat` of 16/64 MiB took 3.6/6.5 s; `echo /etc/hostname` printed nothing.
- **Analysis:** `shell::coreutils()` runs before the native commands and read the last argument
  that names a readable file for any command (`stat`, `cp`, `rm`, `truncate`, ...).
- **Fix:**      only the coreutils that take file input read one; the others return early.
  Regression: `fsio` asserts `stat` < 3 s and `echo /etc/hostname`.

### BUG-023 — the periodic scrub freezes the desktop and console for its whole duration 🟡 MITIGATED
- **Status:**   MITIGATED (`5e3d4e9`), root fix OPEN · **Severity:** HIGH (usability; grows with the data stored)
- **Found:**    2026-11-02 · scenario `fsio` (8 GiB disk, ~600 MiB stored) · `stat` waited 38 s; two
  harness HANGs (serial silent > 60 s) on NVMe and AHCI
- **Analysis:** `scrub::maybe_run` runs a full `fs.scrub()` (every data block, XXH3) synchronously in
  the desktop tick every ~60 s (`kernel/src/scrub.rs`). Measured pass: 60–134 s for ~600 MiB.
- **Fix (mitigation):** the next periodic pass waits max(60 s, 100 × the last pass), and the `[g5]`
  line reports the pass duration. One pass still freezes for its duration; an incremental scrub
  (bounded work per tick) is the real fix.

### BUG-024 — a new background process could run before it was registered → kernel page fault ✅ FIXED
- **Status:**   FIXED (`947b868`)
- **Severity:** CRITICAL (kernel halt)
- **Found:**    2026-11-02 · scenario `soak` (KVM, `-smp 4 -m 16G`) · after 59 min / 2109 commands
- **Actual:**   `[linux-abi] arch_prctl SET_FS = 0x44404138` then
  `[idt] PAGE FAULT addr=0x44404138 code=(PROTECTION_VIOLATION | CAUSED_BY_WRITE) @ 0x5906e0b4`, halt.
- **Repro:**    statistical: `spawn worker 32` repeatedly (the soak); one hit in ~6800 spawns
- **Analysis:** `spawn_bg_musl` / `spawn_bg_app` made the task Ready before pushing its `BgProc`;
  `syscall_dispatch` sends a task it does not find in `BG` to the foreground Linux path. A timer
  tick in between ran musl's first syscall there with the foreground's globals. The daemon had
  the same window before `DAEMON_TASK` was set.
- **Fix:**      interrupts off from `spawn_user` until the process is registered (ring-3 tasks run
  only on the BSP). Proof: the second soak (below) passed the point of the first crash.

### BUG-025 — reading a file larger than the free kernel heap panicked the kernel ✅ FIXED
- **Status:**   FIXED (`9b3730d`)
- **Severity:** CRITICAL (kernel panic from a shell `cp`)
- **Found:**    2026-11-02 · scenario `fsio` on AHCI (`-m 4G`, heap 512 MiB):
  `cp z256 c256` → `memory allocation of 268435456 bytes failed` → KERNEL PANIC
- **Analysis:** `EuroFs::read_data` used `Vec::with_capacity(size)`; an allocation failure aborts.
  The kernel heap is 512 MiB whatever the RAM (also with `-m 64G`). `truncate` also turned any
  read error into an empty file and wrote zeros back (data loss once reads can fail).
- **Fix:**      `buffer_for` (try_reserve → `NoSpace`), host test
  `buffer_for_refuses_what_cannot_be_allocated`; `truncate` only starts empty on `NotFound`.
  Proof: `truncate -s 1073741824` → "NoSpace (1073741824 bytes do not fit in memory; file left
  unchanged)", `cp` of 256 MiB on AHCI now completes (112 s), the system keeps answering.
- **Harness:**  the panic went unnoticed for 900 s: the panic path writes the UART byte by byte and
  `KERNEL PANIC` arrived split over reads. `CRASH_RE` now scans across chunk borders (`305125b`).

### BUG-026 — an outbound TCP connect sometimes blocks the desktop loop for ~85 s ✅ FIXED
- **Status:**   FIXED (`feature/net-async`, `0d8a4c2`, `77923ac`, `24b0fd5`) · **Severity:** HIGH (freeze + watchdog line; also on 813e627-based images)
- **Found:**    2026-11-02 · scenario `net` (sequential `fetch 10.0.2.100`, e1000e on SLIRP)
- **Actual:**   400 sequential fetches (kernel driver): 396 OK (median ~0.13 s), 2 failed fast
  ("no connection", 13 ms), 2 blocked 84–85 s and then failed; `[3g2-wire] WATCHDOG TRIPPED - main
  loop hung` in the log. The `net` scenario saw it after 163 (kernel), 106 (user + IOMMU) and 230
  (image without this branch's net change) connections; `net.driver=user` without IOMMU had 1
  failure in 250, no long block.
- **Repro:**    `scripts/loadtest.py --kvm --nic e1000e --restrict --host-http --scenario net --conns 400`
- **Analysis:** the peer closes first on every HTTP/1.0 fetch, and `TcpConn::close()` only sent our
  FIN while the connection was still open: each fetch left slirp's side in FIN-WAIT-2, which slirp
  keeps 10 min (`tcp_maxidle`). A later connect that drew the same random port met that block.
  libslirp drops such a SYN without an answer when its sequence lies left of `rcv_nxt` (no ACK
  flag: `goto drop`) and ACKs it otherwise (`dropafterack`): four unanswered SYN rounds of
  `SPINS`×3 spins (~21 s each under KVM) = the 85 s blocks, or four ACKed rounds in 13 ms = the
  fast failures, about equally often. Not a lost wake-up and not ARP.
- **Fix:**      our FIN goes out after the peer's FIN too (tick retransmits it); ephemeral ports avoid
  ports in use and the last 1024; SYN-SENT per RFC 793 (a stale ACK is answered with a RST at its
  ACK, then the SYN again; an acceptable RST refuses at once); the handshake is bounded in time
  (1 s + 2 s, then a fresh port and ISN, 1 s + 2 s); a spinning `RxWait` is bounded by HPET time as
  well as by spins. Host tests in `euronet::tcpopen`; `[net-async] connect` covers the stale-ACK
  and silent-drop peers. Proof (R630): kernel driver 400/400, max 558 ms (base: a fast failure at
  #119 and a 60 s hang at #178); `user` 300/300, max 1.0 s (base 3 failures); `user`+IOMMU 300/300,
  max 1.25 s (base 2 failures of 12.3 s). `proof/net-async-2026-11/`.
- **Left:**     with `net.driver=user` the median fetch is ~50 ms slower (576 vs 530 ms): the extra
  FIN frame through e1000d (A/B without it: 531 ms; the kernel driver shows no difference).

### BUG-027 — the kernel heap grows linearly under the soak mix ✅ FIXED
- **Status:**   FIXED (`888001b`, branch `feature/mem-hygiene`) · **Severity:** MEDIUM (slow; frames are not affected)
- **Found:**    2026-11-03 · scenario `soak` (`-m 16G`, 8 GiB disk root, e1000e)
- **Expected:** `heap_used` (ltstat) flat at quiescent samples, like frames, tasks and the file table
- **Actual:**   +8.2 MB over 4 h 10 min (147.4 → 155.6 MB, +32.8 kB/min, linear); the same rate in
  the other two soaks (+32.4 and +34.5 kB/min). Frames, tasks, kernel stacks, file table and
  process pool exactly flat.
- **Repro:**    `scripts/loadtest.py --kvm --smp 4 --mem 16G --disks 8G --nic e1000e --host-http
  --scenario soak --duration 3600`, then compare `heap_used` in the `-ltstat.csv`
- **Analysis:** with the new per-site heap accounting (`scripts/build.sh heaptrace`, `heapsnap` /
  `heapdiff` in the shell, `--heaptrace ROUNDS` in the soak, `scripts/heaptrace-sym.py`) every
  growing site was `EuroFs::write_object`'s insert into `extents_cache` (oid → data extents,
  PERF-002). `remove_file`, `remove_dir` and both rename-over-a-target paths removed the object
  from `objmap` but not from `extents_cache`; oids are never reused, so every removed file kept a
  BTreeMap entry and its extents Vec (`crates/eurofs/src/disk.rs`). The soak removes files every
  round (`rm`, `churn fs`). The ~40 bytes per spawned process measured on a RAM root did not show
  up as a site of their own on the disk root.
- **Fix:**      `EuroFs::forget_object` drops both entries; host test
  `removed_objects_leave_no_extents_behind`. Soak on the R630, release image, same options:
  +29.3 kB/min before, **+0.4 kB/min** after (least squares after the first 10 min, 70 min run,
  range 106 kB); heaptrace soak after the fix: sum over all sites −12 kB after 50 min.
  `proof/mem-hygiene-2026-11/`.
- **Harness:**  `ltstat` also reports `heap_cached` (per-CPU caches, counted in `heap_used`) and
  `big_mib` (big-block pool, not counted); `loadtest.py --restrict` (SLIRP restrict=on) and
  `--nmi-on-hang` (an NMI through the QEMU monitor before a hung VM is stopped).

### BUG-028 — `[sbx2]` and other glibc tests at `-m 1G` fail in some boots ✅ FIXED
- **Status:**   FIXED (`00fb152`) · **Severity:** MEDIUM (flaky self-tests; a leftover process in the next program's run)
- **Found:**    2026-11-02/03 · TCG boots at 1 GiB (`combined-*`, `storage-*`) · `[sbx2]` FAILED in 5 of 6
  `combined-smp4` boots and in every `-smp 1` boot, on the merged tree and on its base alike
- **Actual:**   `/bin/gsandbox: error while loading shared libraries: libc.so.6: failed to map
  segment from shared object`; which sub-test failed varied (rlimit, filter, ASLR)
- **Analysis:** GSCM3 forks child A; at 1 GiB fork B fails (one 96 MiB arena fits the process
  pool), the parent exits, and A waits 30 s for a descriptor B never sends. A outlived its run.
  While blocked in a syscall it owned the swapped per-process globals (`GLOBALS_OWNER`); the next
  run wrote its arena bounds into them, and at the new main task's first syscall
  `ensure_globals_for_current` swapped A's saved copy back in: the previous run's freed arena.
  ld.so's mmap of libc then got ENOMEM. A's exit also freed its demand pages into the next run's
  pool. Timing decided which test hit the window.
- **Fix:**      `run_glibc`, `run_glibc_disk` and `kill_persistent_glibc` end every fork child still
  registered before the run's demand pool goes back (`reap_orphan_fork_children`, sharing
  `fork_child_teardown` with kill(2)); closing chrome no longer leaves its children running.
  Proof: `[sbx2]` OK in 15 of 15 1 GiB boots; GEVFD's fork now succeeds too. GBIG, GSCM3's second
  fork and `[churn-fork]` still fail at 1 GiB: the process pool (160 MiB) is too small, every run.

### BUG-029 — e1000d could drop a received frame as zero-length (torn RX descriptor) ✅ FIXED
- **Status:**   FIXED (`f6a6adc`) · **Severity:** LOW (a lost frame; TCP retransmits)
- **Found:**    2026-11-05 · DMA read-order audit after euronvme's torn completion (`3f59298`)
- **Analysis:** the HAL reads DMA memory byte by byte, ascending; `E1000::receive` read the whole
  16-byte descriptor and then DD (byte 12), so a write-back during the read showed DD with the old,
  zeroed length (bytes 8-9) and the frame counted as `rx_dropped`.
- **Fix:**      status byte first, the descriptor once DD is set; host test
  `a_descriptor_written_back_during_the_read_is_not_torn` (fails on the old order).

### BUG-030 — megaraidd could read a torn reply-ring producer and skip completions ✅ FIXED
- **Status:**   FIXED (`f6a6adc`) · **Severity:** MEDIUM (commands time out after a rare torn read)
- **Found:**    2026-11-05 · the same audit
- **Analysis:** the 32-bit producer was read once per `reap`, byte by byte; an update from 0x00FF
  to 0x0100 between bytes reads 0x01FF, inside the 1008-entry ring: 256 slots the firmware never
  wrote were consumed as stray and the real completions behind them skipped.
- **Fix:**      `read_index` reads until two reads agree; host test
  `a_producer_index_updated_during_the_read_is_not_torn` (old: 1 008 strays, the next command
  lost). bcm5720d has the same torn read of its 16-bit producer, but `receive` re-reads it before
  every entry, so it takes only the entry the chip wrote; guarded by
  `a_torn_producer_read_takes_no_phantom_entry`, no change.

### BUG-031 — one boot without IOMMU went silent after `[net-late]` ⬜ OPEN
- **Status:**   OPEN, not reproduced · **Severity:** unknown (a hang, once in 15 boots)
- **Found:**    2026-11-05 · `scripts/megaraid-proof.sh` with `NO_IOMMU=1`, TCG, second boot of the
  first run, mem-hygiene image
- **Actual:**   console silent right after `[net-late] network waits in this suite ...`, before the
  `[udrv-linux]` glibc program loaded; no fault line; stopped by the 900 s timeout.
- **Analysis:** not attributed; 13 further boots without IOMMU and every other boot of the proof
  passed that point. `megaraid-proof.sh` now injects an NMI on a timeout, so a recurrence leaves
  the kernel's NMI probe in the log. `proof/mem-hygiene-2026-11/mr-noiommu-hang-boot2-tail.txt`.

### Notes (not bugs, measured)
- **Boot-time background musl processes and the native bg-app tests only run under KVM with
  ≥ 2 GiB after BUG-016**; on 813e627 most were killed at their first instruction and the boot
  self-tests did not notice (their markers do not check those processes' results). glibc
  processes were not affected (their arenas are mapped by `map_user_arena`).
- A QEMU guest without `-nic`/`-netdev` still gets the default e1000e on SLIRP: the background
  update check then reaches `euro-os.eu` (HTTP 404 on the manifest). All runs had a NIC.
- Swap self-test (`gswap`) on a disk-root boot: ~4 min of synchronous `virtio_blk::kick_and_wait`
  under KVM; a selftest boot with an installed 8 GiB disk takes 330–455 s to the desktop.

- **Base `bb2e9d2` under KVM, once (2026-11-03, R630, `net.driver=user`):** `KERNEL PANIC` at
  `kernel/src/sched.rs:755`, index out of bounds with a garbage `s.current`, during the boot
  (`proof/net-async-2026-11/r630-b2-out-user.txt`). Not seen on `feature/net-async`. Not filed as
  a number here (another team works on the heap); the lead decides.
- **Boots that stop in the self-tests under TCG** (`user-smp4-iommu`, 512 MiB): 1 of 6 on
  `feature/net-async`, 1 of 3 on the base, same picture (no output after a glibc self-test line).
  `proof/net-async-2026-11/README.md`.

<!-- Add BUG-NNN entries above this line. Keep newest at the bottom of the relevant group. -->

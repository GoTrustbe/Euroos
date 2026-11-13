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

**The desktop.** `loadtest.py` never touches a window. `scripts/gui-smoke.py` boots the
image and drives the desktop with a pointer and a keyboard over QMP: every dock app opened,
used, moved and closed, Alt-Tab, workspaces, launcher, context menu, and a seeded monkey soak
(`--soak MINUTES --seed N`). After every step the guest must be alive (no crash signature,
the desktop loop answers the serial console, the screen reacts). See
[`GUI-TESTING.md`](GUI-TESTING.md).

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
# the desktop (public image, ./scripts/build.sh image):
python3 scripts/gui-smoke.py --image eurokernel.img --workdir /tmp/g
python3 scripts/gui-smoke.py --image eurokernel.img --kvm --smp 4 --mem 4G --soak 30 --seed 1 --workdir /tmp/gs
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
| TCG matrix + `megaraid-proof.sh` on net-async + mem-hygiene integrated | 2026-11-05 | ✅ 0 markers worse | `proof/integrate-net-mem-2026-11/` | 14 boots (user/kernel/storage/combined, smp4/smp1, ± IOMMU) + 10 megaraid boots; `[net-async]` OK, `[sbx2]` 6/6 at 1 GiB, `[lk]` 0 inversions |
| `soak` 1 h + 4 httpd clients, integrated (KVM, `-m 16G`) | 2026-11-05 | ✅ 1 805/1 805 ok | `proof/integrate-net-mem-2026-11/` | heap +0.37 kB/min (heaptrace soak: no growing site); tasks, files, frames, process pool flat; httpd as on net-async (slow during long synchronous commands) |
| BUG-032 fix: TCG matrix + megaraid + R630 boots and soak (`feature/bug032`) | 2026-11-05 | ✅ 0 markers worse | `proof/bug032-2026-11/` | 30 TCG boots, 8 megaraid boots, 59 R630 KVM boots, 1 h soak with 4 httpd clients (1 720/1 720 commands, heap +0.43 kB/min); `[bug032]` DF self-test OK everywhere, `[lk]` 0 inversions |
| `inbound` 4 clients 300 s on `feature/net-worker` (KVM) | 2026-11-05 | ✅ | `proof/net-worker-2026-11/` | user 36 233 ok, p50 5.5 ms (base 4 356, 270 ms); kernel 207 498 ok, p50 5.9 ms, p99 17 ms (base 111 904, 9.9 / 21.7 ms); BUG-034/035, harness BUG-036 fixed |
| `inbound-busy` 600 s (new: httpd during long synchronous commands, KVM) | 2026-11-05 | ✅ | `proof/net-worker-2026-11/` | during the commands: user 63 212 ok / 0 failed, p99 125 ms (base 84 / 160, p99 8.8 s); kernel 206 822 / 0, p99 11.4 ms (base 76 / 162) |
| `soak` 1 h + 4 httpd clients on `feature/net-worker` (KVM, `-m 16G`) | 2026-11-05 | ✅ 1 663/1 663 ok | `proof/net-worker-2026-11/` | httpd 460 905 ok / 12 failed (first attempts), p99 123 ms, 302 014 / 0 during long commands (base 4 376 / 704, p99 9.5 s); heap +0.37 kB/min; tasks, files, frames, process pool flat; the base `52755fc` soak hit a ring-0 GPF after 47 min |
| TCG matrix + `megaraid-proof.sh` on `feature/net-worker` | 2026-11-05 | ✅ 0 markers worse | `proof/net-worker-2026-11/` | 10 TCG boots (user/kernel/storage/combined, smp4/smp1, ± IOMMU) + 4 megaraid boots against `proof/bug032-2026-11/`: 0 worse, 0 changed; `[net-worker]` OK everywhere, `[lk]` 0 inversions |
| `inbound` 4 clients 300 s + `net`, integrated (KVM) | 2026-11-05 | ✅ | `proof/integrate-net-mem-2026-11/` | user 4 165 ok, console max 693 ms; kernel 101 202 ok, max 380 ms; outbound 300/300 (user), 400/400 (kernel) |
| BUG-040/037/039/050 on `feature/bug-gpf`: TCG matrix, megaraid, modern virtio, CI q35 matrix; R630 KVM soaks and boots | 2026-11-10 | ✅ 0 markers worse | `proof/bug-gpf-2026-11/` | 24 TCG boots (matrix ×11, the two BUG-037 configurations ×12, modern virtio), megaraid 8 boots, CI q35 matrix 13/13 (`usbnet` green); R630: 168 boots ok, two soaks of 2 h 05 with `churn proc 8` every round: 5 268 commands ok, 5 430 glibc launches, 0 faults, heap +0.23 kB/min, httpd 1 779 547 answered / 22 failed; `[lk]` 0 inversions, `[net-worker]` OK |
| virtio 1.x (modern, non-transitional) on `feature/virtio-modern`: TCG device configs, matrix, megaraid; R630 KVM | 2026-11-06 | 🟡 0 markers worse; 2 of 10 matrix boots hung (BUG-037, not in the changed code) | `proof/virtio-modern-2026-11/` | modern virtio-blk (boot + data disk), virtio-net (DHCP, fetch, net worker), virtio-tablet/keyboard typed and pointed, BARs at 56 TiB through the high-MMIO window, no PIT (HPET / PM timer); transitional and legacy 0 worse; reruns of the hung configs 6/6 (branch) and 6/6 (base); megaraid 8 boots 0 worse; R630 `net` 8/8 clean, `inbound` over modern virtio-net within run-to-run spread of legacy |
| GUI smoke `scripts/gui-smoke.py` on `feature/robust` (public image): TCG, and KVM with USB and with virtio input | 2026-11-10 | ✅ 54/55 steps (TCG), 1 known (BUG-044) | `proof/robust-2026-11/` | all eleven dock apps opened, used, moved by the title bar and closed; Alt-Tab, workspaces, launcher search, context menu; a typed terminal command checked on serial; the first runs found BUG-042 (xHCI BAR at 56 TiB under KVM, with BUG-021) and BUG-043 (no Alt from a USB keyboard), both fixed; BUG-044 and BUG-045 found and left open (not this branch's files) |
| GUI monkey soak, 2 × 35 min (KVM): USB input smp 4 / 4 GiB, virtio input smp 2 / 2 GiB | 2026-11-10 | ✅ 2230 + 2246 actions, no crash, no hang | `proof/robust-2026-11/` | seeds 20261110 and 20261111; functional check (terminal runs a typed command) every 200 actions: 11/11 and 11/11; longest console round trip 302 ms and 1562 ms |
| Stability pass on `feature/robust`: TCG selftest boots, q35 legs `usb`/`hda`/`nvme`, R630 KVM with BARs at 56 TiB | 2026-11-10 | ✅ 0 markers worse | `proof/robust-2026-11/` | four TCG boots against `proof/net-worker-2026-11/`: 0 worse, 0 changed, the five `[robust…]` self-tests OK in every boot, `[lk]` 0 inversions; matrix legs PASS; NVMe and xHCI under `-cpu host` without the phys-bits workaround (BUG-021 fixed); BUG-041 (image decoders) fixed |
| Integration of bug-gpf, fast-boot, robust, settings, help (`feature/integ-2026-11`): TCG matrix, megaraid, modern virtio, CI q35 matrix, host tests | 2026-11-10 | ✅ 0 markers worse | `proof/integ-2026-11/` | 10 TCG boots, 27 comparisons against the five branch proofs; megaraid 4 boots (host checks 4/4 twice); modern virtio; q35 13/13; `[lk]` 0 inversions; 186 host suites, 1 304 tests; BUG-044, 045, 047, 051, 059 fixed; the self-test image reaches the desktop 45 s later under TCG since the settings merge (BUG-056, open) |
| Release image, integrated: wizard install, GUI smoke, settings across a power cut, open network, boot time (TCG and R630 KVM) | 2026-11-10 | ✅ | `proof/integ-2026-11/` | wizard 14/14; gui-smoke 56/56 (BUG-044 step counting, tour step new); installed system reads back seven settings and `tour.done` after QEMU was quit; ping, DNS, HTTP, HTTPS; R630 boot 2.71 s (SATA + legacy virtio-net) and 2.34 s (modern virtio), 5 boots each (fast-boot: 2.95 s, 2.70 s) |
| GUI monkey soak, integrated, 35 min each (R630 KVM): USB smp 4 / 4 GiB, virtio smp 2 / 2 GiB | 2026-11-10 | ✅ 3/3 with the fixed harness; 🟡 3/5 with the harness as merged | `proof/integ-2026-11/` | final harness: 2 217 + 1 958 (USB) and 2 190 (virtio) actions, functional checks 11/11, 9/9, 10/10, no crash, console max 250 ms / 2.0 s; as merged: one false "screen frozen" and one failed check, both with the notification list or a context menu open (BUG-057: full repaint in every iteration, 0.3 to 0.4 s per frame; same before the settings and help merges) |
| Desktop pass on `feature/uidebug` (BUG-052, 054, 055, 057, 058, 070 to 077; mouse wheel; tour after the wizard): host tests, TCG matrix, release image under TCG | 2026-11-12 | ✅ 0 markers worse | `proof/uidebug-2026-11/` | 186 host suites, 1 313 tests; 10 TCG boots, 0 worse against `proof/integ-2026-11/`, new `[altgr]` and `[disks]` OK, `[lk]` 0 inversions; wizard 15/15; gui-smoke under TCG 66/66 twice with the final harness (64, 65 and 66 of 66 before the Alt-Tab steps waited for the switcher: timing on a loaded host, one drag step can still fall short) |
| Desktop pass, release image on the R630 (KVM): GUI smoke and monkey soak 35 min with USB and with virtio input, frame time, boot time | 2026-11-12 | ✅ | `proof/uidebug-2026-11/` | gui-smoke 66/66 with USB and with virtio input (59/66 on the image without the fixes); monkey 2 227 + 2 087 actions, functional checks 11/11 and 10/10, no crash, console max 250 ms / 2.1 s; notification list open: pointer visible after 39 to 46 ms (was 291 to 373 ms); boot 2.70 s (2.75 s for `7858b72` in the same session) |
| `soak` 2 h 05 + `churn proc 8` every round + 4 httpd clients, integrated (KVM, `-m 16G`) | 2026-11-10 | ✅ 2 899/2 899 ok | `proof/integ-2026-11/` | on the head (`fb18477`): 371 `churn proc` commands, 2 987 glibc launches, every verdict OK; heap +0.25 kB/min; tasks, files, frames, process pool flat; httpd 1 006 005 answered / 11 failed, p99 122 ms; 0 panics, 0 fault lines. The same on `182b19c` before it (2 868 ok, +0.24 kB/min), whose boot showed BUG-059 |
| Kernel debug round on `feature/kdebug` (BUG-053, 056, 060 to 065): TCG matrix, CI q35 matrix, host tests | 2026-11-12 | ✅ 0 markers worse | `proof/kdebug-2026-11/` | 10 TCG boots against `proof/integ-2026-11/`: 0 worse, 0 changed, `[lk]` 0 inversions; new `[bug063]`, `[fdspace]`, `[bug053]` OK in all ten; megaraid 4 boots 0 worse; q35 13/13 (12/13 before BUG-064, a self-test race); 187 host suites, 1 312 tests; self-test image reaches the desktop in 67 to 79 s under TCG at 512 MiB (144 to 159 s before; BUG-056); two builds of one commit give one `eurokernel.efi` (BUG-062); BUG-031 closed (60 boots, none silent) |
| Same, R630 KVM: 100 short boots (user, user + IOMMU, smp 1), `soak` 2 h 05 + `churn proc 8` + 4 httpd clients, release image boot time and wizard install | 2026-11-12 | ✅ 100/100 boots, soak 2 671/2 671 ok; 🟡 BUG-065 in 8 of the 100 boots | `proof/kdebug-2026-11/` | final image: 100 boots (40 user, 30 user + IOMMU, 30 smp 1) to the desktop and clean, the three new self-tests OK in each; soak 2 h 05: 342 `churn proc` commands, 2 758 glibc launches, every verdict OK, heap +0.17 kB/min, 0 panics, httpd 883 286 answered / 10 failed; a first round on the commit before BUG-064: 78/78 boots, soak 2 724/2 724; release image 2.71 s (base 2.69 s beside it), wizard 14/14; lock screen phase 5.39 s → 0.87 s; `[3d8]` (BUG-065, open) FAILED in 8 of the 100 boots, base image 0 of 60 |
| Integration of `feature/kdebug` and `feature/uidebug` with BUG-065 (`feature/integ-2026-11b`): host tests, TCG matrix, megaraid, CI q35 matrix, reproducible build; public image: GUI smoke twice, wizard install, open network | 2026-11-12 | ✅ 0 markers worse, q35 13/13 | `proof/integ-2026-11b/` | 1 330 host tests; matrix 10/10, 0 worse in 30 comparisons against three proofs, `[lk]` 0 inversions; megaraid 0 worse; q35 13/13 (11/13 in the first round: BUG-083, BUG-084, from the desktop pass); REPRODUCIBLE; gui-smoke 67/67 without NIC and with e1000e; wizard 15/15; ping, DNS, fetch, https on both NICs |
| Same, R630 KVM: 204 + 20 entropy boots (and 20 under TCG), `soak` 2 h 05 + `churn proc 8` + 4 httpd clients, monkey soak 2 × 35 min, boot time, NVMe untouched, BUG-063 from the desktop | 2026-11-12 | ✅ 0 unseeded boots of 244; soak 2 744/2 744 ok | `proof/integ-2026-11b/` | before the fix 13 of 204 boots FAILED `[3d8]` and 11 were unseeded; after: 0 of 204, 0 of 20 jitter-only (KVM), 0 of 20 (TCG); soak heap +0.17 kB/min, 0 panics, httpd 917 858 answered / 0 failed; monkey 2 225 and 2 102 actions, no crash; public image 2.62 s; base image crashes 5 of 5 in the desktop BUG-063 scenario, final 0 of 5 |
| BUG-080, BUG-081 (`feature/linux-rand`): `getrandom`, `/dev/urandom`, `/dev/random`, `AT_RANDOM` and the kernel's own callers on the CSPRNG; one early seeding. Host tests, TCG matrix, CI q35 matrix, reproducible build, public image on the open network | 2026-11-12 | ✅ 0 markers worse, q35 13/13 | `proof/linux-rand-2026-11/` | 1 330 host tests; matrix 10/10, 0 worse and 0 changed against `integ-2026-11b`, `[lk]` 0 inversions, one `[entropy]` line in every leg (two in the `kernel` legs before); `[linux-rand]` OK in 10/10; q35 13/13; REPRODUCIBLE; ping, DNS, fetch, https on both NICs. Before the fix, the same probe: one `AT_RANDOM` for 8 of 8 processes of two boots, 32 of 32 `getrandom`/device results predicted from their first 8 bytes |
| Same, R630 KVM: 104 short boots, before/after boots (also without RDRAND and RDSEED), `soak` 1 h + `churn proc 8` + 4 httpd clients, process start before/after, public image GUI smoke and boot time | 2026-11-12 | ✅ 104/104 boots seeded, soak 1 364/1 364 ok, gui-smoke 67/67 twice; 🟡 one configuration asks before the early seeding | `proof/linux-rand-2026-11/` | 104 boots: `[3d8]`, `[3d7]`, `[3d3]` FAILED in 0, unseeded 0, `[linux-rand]` OK in 104; 416 `AT_RANDOM` values, guards, `getrandom` results and device reads each, none twice. Soak: 1 366 commands per hour (previous 1 314), 1 423 glibc launches, 0 panics, httpd 440 088 answered / 0 failed. `churn proc 8` before/after, alternating: 4 994 against 5 044 ms median, inside the spread. Boot time 2.62 s base and 2.55 s final (median of 5), kernel entry to desktop 811 and 812 ms. With a blank virtio disk the install self-test asks for a salt before the early seeding (served by the same seeding, one status line; see the proof's Open) |
| W23 on `feature/chrome-w23` (BUG-134/135/136, BUG-140 to 147): browser runs (R630 KVM), TCG matrix and repeated TCG boots, host tests, GUI smoke | 2026-11-13 | ✅ site matrix 3 rounds in one session, 0 aborts; 0 watchdog trips in 35 TCG boots; 0 markers worse under the same conditions; 🟡 key to screen 476 ms | `proof/chrome-w23/` | `[churn-forkthreads]` FAILED → OK; gui-smoke 67/67; cargo test 1 350/1 350; BUG-136 not reproduced (0 in 29 before) |

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
  `churn fs`/`truncate`/`sha256sum`). → BUG-034, fixed by the network worker.

### BUG-020 — `[fuzz]` fails under KVM: the fuzzer stops and run_glibc times it out ✅ FIXED
- **Status:**   FIXED (`75f254f`, 2026-11-13) · **Severity:** MEDIUM (also on 813e627 → not a regression of this branch; the
  kernel keeps running)
- **Cause:**    the fuzzer does not stop. With the syscall ring of its main thread dumped at the launcher's
  deadline it was seen making calls while the deadline message was printed: about 6 300 of its 20 000 calls were
  done when its 30 s were over. It spent them on one serial line per unknown syscall
  (`[linux-abi] ENOSYS Linux syscall N`, about 16 000 of its calls); under KVM such a line costs milliseconds of
  guest time (BUG-123), under TCG it does not.
- **Fix:**      an unknown syscall is logged at its first call per number and run, and counted (`LINUX_ENOSYS`).
  `[fuzz] … kernel-survived=true (exit 171) → OK` under KVM on every self-test boot since (R630, 4 GiB and 1 GiB;
  the 100 short boots of `proof/chrome-2026-11b/`).
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

### BUG-021 — NVMe under KVM `-cpu host`: BAR above 512 GiB, kernel page fault at boot ✅ FIXED
- **Status:**   FIXED (`bf87957`, 2026-11-10; the workaround is no longer needed) · **Severity:** HIGH (boot halts)
- **Actual:**   `[idt] PAGE FAULT addr=0x380000000000 ... @ efi_main+0x54bc` right after
  `[epk] no pack files registered`, with an NVMe disk attached; the boot never continues.
- **Repro:**    `scripts/loadtest.py --kvm --disk-bus nvme --disks 8G --scenario fsio`
- **Analysis:** with host physical-address bits (46) QEMU/OVMF put the 64-bit PCI window at
  56 TiB; `nvme::init` dereferences BAR0 through the identity map, which covers only the lower
  512 GiB. Any 64-bit BAR placed that high has the same problem (the harness uses virtio
  `disable-modern=on`, legacy BARs). Real servers can place 64-bit BARs high too.
  Workaround: `--cpu-opts ,host-phys-bits=off,phys-bits=39` (window below 512 GiB): NVMe works.
  A fix needs an MMIO mapping facility for physical addresses above 512 GiB.
- **Fix:**      that facility came with the virtio 1.x work (`paging::mmio_virt`, the high-MMIO
  window); `nvme::init` now reaches BAR0 through it, as do `xhci.rs` (BUG-042) and `hda.rs`.
  R630, `-cpu host` without the phys-bits workaround: `[nvme] self-test read/write @ LBA 1000: OK`,
  64 KiB PRP list OK, MSI-X delivery confirmed (`proof/robust-2026-11/r630-kvm-highbar-*.markers`).
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

### BUG-031 — one boot without IOMMU went silent after `[net-late]` ✅ CLOSED (not reproducible after BUG-037)
- **Status:**   CLOSED 2026-11-12: not reproducible after BUG-037 (60 boots of its configuration, none silent); no probe of the original boot exists · **Severity:** unknown (a hang, once in 15 boots)
- **Found:**    2026-11-05 · `scripts/megaraid-proof.sh` with `NO_IOMMU=1`, TCG, second boot of the
  first run, mem-hygiene image
- **Actual:**   console silent right after `[net-late] network waits in this suite ...`, before the
  `[udrv-linux]` glibc program loaded; no fault line; stopped by the 900 s timeout.
- **Analysis:** no probe of the hang exists, but BUG-032 explains this kind of silence: a context
  switch that runs with RFLAGS.DF set copies the scheduler's lock guard backwards, and with a
  stale stack word as its SCHED pointer the switch can run on a fake run queue, return a wrong
  stack or leave interrupts on inside the tick, without a fault line. Not seen again: megaraid
  proof 4 runs (8 boots) with and without IOMMU on the fix
  (`proof/bug032-2026-11/`). `megaraid-proof.sh` sends an NMI on a timeout, so a recurrence
  leaves the probe. `proof/mem-hygiene-2026-11/mr-noiommu-hang-boot2-tail.txt`.
  2026-11-10: the place also fits BUG-037 (the step after `[net-late]` is a glibc launch, which
  starts with `reset_fd_table`); no probe of this one exists, so it stays 🟡.
  2026-11-12: `megaraid-proof.sh` with `NO_IOMMU=1` under TCG on the head of `feature/app-control`
  (`7858b72`, with both fixes), three runs at a time beside other builds and boots (host load 6 to 21
  on 16 cores): 60 boots, 60 to the status line, no NMI probe, no fault line
  (`proof/kdebug-2026-11/bug031-megaraid-noiommu-tcg-60-boots.txt`). At 1 in 15 the chance of that
  is 1.6 %. Closed. BUG-063 (found the same day) is a third bug that fits the picture: a userspace
  driver's syscall returning through a stale stack pointer, no fault line. Which of the three it
  was cannot be told without a probe.

### BUG-032 — `KERNEL PANIC` at `sched.rs:755`: `s.current` read through a stale pointer ✅ FIXED
- **Status:**   FIXED (`b6a6fa8`, `feature/bug032`) · **Severity:** CRITICAL (kernel panic; silent
  corruption and hangs in the other cases)
- **Found:**    2026-11-03 · R630, KVM, base image `bb2e9d2`, `net.driver=user`, an outbound run of
  the net-async work (`proof/net-async-2026-11/r630-b2-out-user.txt`); numbered when
  `feature/net-async` and `feature/mem-hygiene` were integrated
- **Actual:**   during the boot, after the desktop apps came up: `panicked at kernel/src/sched.rs:755:5:
  index out of bounds: the len is 256 but the index is 5242243857361403874`, i.e.
  `s.tasks[cur]` in `schedule_core` with a `s.current` that is no task index.
- **Analysis:** nothing wrote over the scheduler; the scheduler read through a wrong pointer. An
  interrupt does not clear RFLAGS.DF, and the three raw context-switch stubs (`timer_switch`,
  `yield_switch`, `ap_timer_switch`) called Rust with the interrupted DF. The interrupted code can
  be inside a backward copy: `compiler_builtins` memmove does `std; rep movsq; cld` for an
  overlapping copy (the TCG recurrence interrupted `memmove+0x56`, under EuroFS
  `rebuild_allocator_trim`). Rust code assumes DF=0. `schedule_core` copies its 40-byte `SCHED`
  lock guard with `rep movsq`; with DF=1 the copy runs backwards, the guard's data pointer stays
  whatever word the stack held there, and `current` is read at that word + 0x6820. On the R630
  that word was a return address in `ab_glyph` (`quad_to`, the lockscreen text being drawn):
  return address + 0x6820 is `.text` of `core::num::dec2flt::lemire::compute_float<f32>`, exactly
  the eight bytes of the panic, and the panic dump shows `RFLAGS=0x406` (DF set). On TCG the word
  was 0 (`current` read at 0x6820) and, in the self-test, heap text. The same DF made every
  `memcpy` in the tick path write below its destination. SYSCALL had the same exposure: FMASK
  cleared only IF, so a ring-3 program could run the syscall path with DF=1.
  `proof/bug032-2026-11/r630-original-panic-analysis.txt`.
- **Fix:**      `cld` as the first instruction of the three stubs (iretq gives the interrupted code
  its DF back); FMASK clears DF, TF and NT as well as IF. `schedule_core` now checks `current`
  against `MAX_TASKS` and `count` before it indexes and dumps the guard address against the real
  `SCHED.current`, the interrupted RFLAGS and the 32 words around it (one compare). A
  debug-register write watch on `current`/`count` (`memwatch.rs`, `debug.schedwatch=on`) is left
  in as an opt-in tool.
- **Regression test:** boot self-test `[bug032] DF set across context switches` runs across timer
  ticks and a yield with DF=1 (pure assembly) and checks: handler never saw DF, DF back after
  iretq, FMASK clears DF. The same image without the three `cld` panics in that self-test with
  the BUG-032 signature (`proof/bug032-2026-11/tcg-nocld-selftest-panic.txt`).
- **Before / after:** before the fix 1 panic in 12 TCG `user-smp4-iommu`
  boots and 1 hang (silent after `gsdl`) in 31 R630 KVM boots; on the fix
  30 TCG boots and 59 R630 KVM boots without either, a 1 h soak with 4 httpd clients (1 720/1 720 commands, heap +0.43 kB/min).

### BUG-033 — TCG boots that stop in the boot self-tests (`user-smp4-iommu`) ✅ FIXED (it is BUG-037)
- **Status:**   FIXED as BUG-037 (`a410803`): it recurred after the BUG-032 fix, with an NMI probe,
  and that probe shows the `PIPE_FDS` wedge (`proof/bug-gpf-2026-11/tcg-bug033-nmi-probe.txt`). The
  BUG-032 mechanism was real but not this hang · **Severity:** MEDIUM (a boot that never reaches
  the desktop; a CI flake)
- **Found:**    2026-11-05 · TCG, `-smp 4 -m 512M` with intel-iommu, selftest image, the net-async
  work: 1 of 6 boots on `feature/net-async`, 1 of 3 on the base `bb2e9d2`
  (`proof/net-async-2026-11/README.md`); numbered at the integration
- **Actual:**   no output after a glibc self-test line (`[glibc] gscm` on the branch, a
  `[sched-guard]` line on the base, after `gsdl`); no fault line, no panic.
- **Analysis:** the BUG-032 mechanism (a context switch with DF=1) gives this picture when the
  stale guard pointer happens to read a small `current`: the switch then works on a fake run
  queue and the boot goes silent. Before the fix an R630 KVM boot went silent at the same place
  (after `[glibc] === SDL2 app (gsdl) ===`, `proof/bug032-2026-11/r630-before-hang-tail.txt`),
  and the TCG loop that reproduced BUG-032 ran on the same configuration. On the fix:
  21 `user-smp4-iommu` TCG boots, all to the desktop. `loadtest.py --nmi-on-hang`
  now also probes a boot that hangs before the console is up (`ffec65c`).

### BUG-034 — the httpd and listen/accept stand still while a long synchronous shell command runs ✅ FIXED
- **Status:**   FIXED (`feature/net-worker`, `69cd2f3` `77aeec0`) · **Severity:** HIGH for any server use
- **Found:**    2026-11-05 · the 1 h soaks with 4 httpd clients of `proof/net-async-2026-11/` and
  `proof/integrate-net-mem-2026-11/` (4 570 answered, 652 failed, p99 9.5 s); BUG-019's "Left"
- **Actual:**   the failures fall into the shell's long synchronous commands (`churn fs`,
  `truncate`/`sha256sum` of 8 MiB). `--scenario inbound-busy` (only such commands, 600 s): 84
  answered and 180 failed during them, p50 3.8 s, p99 9.8 s.
- **Analysis:** after BUG-019 nothing on the inbound path waits, but it still only moved when the
  desktop loop called `service()` / `pump_all()`, and that loop also runs the shell.
- **Fix:**      a kernel task on the BSP (`kernel/src/net_worker.rs`, `sched::spawn_kernel`, nice
  -5) takes the inbound frames from the legacy queue (SYNs for the httpd or a listener, segments of
  a late-registered connection, ARP requests; the rest goes back for `service()` and the blocking
  readers) and runs `passive::step` and, once a tick, `pump_all`. Woken by e1000d's frame
  deliveries (`net.driver=user`); with an in-kernel driver it polls every tick while the httpd or a
  listener is on (virtio-net's MSI-X wakes it earlier; an MSI-X interrupt for the 82574L was tried
  and measured worse: p99 21 → 61 ms idle, p50 59 ms busy); every tick while inbound work is in
  flight. A pass runs with interrupts off, so the net-global locks (CFG,
  PORTQ, NET_LEGACY, UDPQ: `lk()`), the firewall, the ICMP limiter and the e1000/virtio-net rings
  are held with IF=0 now (a syscall meeting one of them held by a preempted task 0 had the same
  exposure before). Also: `RX_WAITER` is cleared in `reap_dead` (`reset_task_slot`) and when a fork
  child is torn down (the loose end of the integration proof). Boot marker `[net-worker]`: 8
  synthetic clients served while the boot task spins 3 s without `service()`. `inbound-busy` on
  the R630, during the long commands: `net.driver=user` 84 answered / 160 failed (p50 6.5 s) →
  63 212 / 0 (p50 5.7 ms, p99 125 ms); `kernel` 76 / 162 → 206 822 / 0 (p50 10.0 ms, p99 11.4 ms).
  `proof/net-worker-2026-11/`.
- **Left:**     the desktop loop itself is still held by such a command (watchdog line, console);
  `churn fs` takes ~35 % longer while the BSP serves 120-390 requests a second.

### BUG-035 — e1000d slept a tick per frame: 10 ms on every frame of `net.driver=user` ✅ FIXED
- **Status:**   FIXED (`feature/net-worker`, `d04b533`) · **Severity:** MEDIUM (performance)
- **Found:**    2026-11-05 · the ~50 ms extra per sequential fetch with `net.driver=user`
  (`proof/net-async-2026-11/`: one frame more per connection through e1000d)
- **Analysis:** an empty `SYS_IPC_RECV` sleeps the driver one tick and nothing woke it early, so
  every frame the netstack queued for e1000d and every received frame its MSI-X interrupt announced
  waited up to 10 ms for the driver to look.
- **Fix:**      the driver's task is recorded while it sleeps in its receive (`RECV_SLEEPER`);
  `nic_send` wakes it, the pool interrupt stub try-wakes it, `revoke_pid` drops the mark. e1000d only.
  Sequential fetches (no inbound load): median 601 → 436 ms. 4 httpd clients, 300 s, together
  with BUG-034's worker: 4 356 → 36 233 answered, p50 270 → 5.5 ms, p99 351 → 111 ms.
  `proof/net-worker-2026-11/`.

### BUG-036 — harness: `sleepwait` left the console unread; the guest stalled in UART writes ✅ FIXED
- **Status:**   FIXED (`scripts/loadtest.py`, `2ef3934`) · **Severity:** MEDIUM (it skewed measurements)
- **Found:**    2026-11-05 · the net worker's long-pass log: a pass of 0.84-0.96 s with interrupts off
  every 1-2 s in the `inbound` scenario, base included (`[tick-late] +88 … +95 ticks` after each);
  a 45 s `sleepwait` stalled one pass for 44.6 s
- **Analysis:** a `sleepwait` was `time.sleep`: nobody read the console socket, QEMU's chardev
  buffer filled and the guest's next UART write (the `[tcp] FIN …` log, say) spun with interrupts
  off until the harness read again.
- **Fix:**      the pause pumps the socket into the log and scans for crashes (`SerialConsole.idle`);
  after a crash the harness reads 10 s more before it stops the VM (a log once ended at `[PA`).
  Longest worker pass after the fix: 16 ms. The `inbound` numbers before 2026-11-05 carry this stall
  in their `max_ms` and part of their p99.

### BUG-037 — `PIPE_FDS` taken with and without interrupts off: a process spins with IF=0 for ever ✅ FIXED
- **Status:**   FIXED (`a410803`, `feature/bug-gpf`) · **Severity:** HIGH (boot hang, rare)
- **Found:**    2026-11-05 · `feature/virtio-modern` TCG matrix, `user-smp4-iommu` and `user-smp1-noiommu`
  (2 of 10 boots, up to six QEMUs on the build server); NMI probe in `proof/virtio-modern-2026-11/tcg-hang-*`
- **Analysis:** task 10 (a musl background process) sits in `ring3::pipe_write_fd` (`+0xd1`) spinning on
  `ring3::PIPE_FDS` with RFLAGS `0x50002` (IF=0, AC=1), the boot task Ready. `pipe_write_fd` takes the
  lock under an `IfOffGuard` (`96f0aac`, 2026-08-31); most other users (`pipe_fd_exists`, dup/dup2,
  close, the fork fd copy) take it with interrupts on. A holder preempted by the tick and a spinner
  with IF=0 on the same core never meet again: the BUG-007 class. Not in the code the branch changes;
  3 more boots of each configuration on the branch and on the base all reached the desktop.
- **Fix:**      `PIPE_FDS` only through `ring3::pipe_fds()` (interrupts off first, lock second, as
  `pipes_lock()`; 23 call sites), and `reset_fd_table` (the launcher clears `PIPE_FDS`, `OPEN_FDS`,
  `OPEN_DIRS`, `PIPE_NONBLOCK`, `PIPE_WAITERS` at the start of every glibc run, with interrupts on
  until now) runs without interrupts. `PIPE_WAITERS` and `PIPE_NONBLOCK` were already only taken
  inside IF-off sections elsewhere. Seen once more before the fix (1 of 16 TCG boots of the BUG-040
  work, same probe: `proof/bug-gpf-2026-11/tcg-bug033-nmi-probe.txt`); on the fix 14 TCG
  boots of the two configurations and 168 R630 KVM boots, none silent. BUG-033 is this bug.

### BUG-038 — install-media preload panicked on a 512 MiB machine with a second disk ✅ FIXED
- **Status:**   FIXED · **Severity:** HIGH (boot panic)
- **Found:**    2026-11-06 · CI `q35 metal matrix` (`nvme`, `ahci`, `usb` legs, `-m 512M`), reproduced on the R630
- **Analysis:** with a second disk, `instexec::capture_media` reads `eurokernel-A/B.efi` and the loader
  (128 MiB together since the A/B kernels grew to ~52 MiB each) into the kernel heap before
  ExitBootServices. 512 MiB RAM gives a 128 MiB heap: `memory allocation of 67562496 bytes failed`.
- **Fix:** the preload sums the file sizes first and skips itself when they need more than three
  quarters of the free heap (`[inst] install media (…) does not fit the heap …`). The installer then
  reports that no install media is available, instead of the boot panicking. Matrix `nvme`, `ahci`,
  `usb` PASS at 512 MiB.

### BUG-039 — CDC-ECM USB ethernet gets no DHCP OFFER in the late bring-up ✅ FIXED
- **Status:**   FIXED (`9553f59`, `feature/bug-gpf`) · **Severity:** MEDIUM (USB tethering has no network)
- **Found:**    2026-11-06 · CI `q35 metal matrix`, `usbnet` leg (TCG, `-nic none`, `usb-net` on SLIRP)
- **Analysis:** the device enumerates (`[xhci] slot 3: USB ethernet (CDC-ECM) LIVE`) and the netstack
  binds it (`[net] NIC: usb-ethernet (CDC-ECM) … (late bring-up)`), but six DISCOVERs get no OFFER
  (`[net] late bring-up: no DHCP OFFER over usb-ethernet (CDC-ECM)`). Not new this week: the
  2026.11.05 image (`52755fc`, before the net worker) fails the same way. The matrix job never ran
  to the end before (it was cancelled), so the last good state is unknown.
- **Cause:**    `legacy_rx()` → `net::rx_route()` returned before it polled the NIC when the netstack
  had no configuration (`get()` is None), and the late bring-up is exactly that state: the DHCP and
  ARP readers never saw a frame. The DISCOVERs went out and the xHCI bulk-IN was armed; nobody
  fetched what arrived. Since the demux of BUG-019 (`0d8a4c2`), which is why `52755fc` fails too.
- **Fix:**      without an address `rx_route` routes with 0.0.0.0: every frame goes to the legacy
  queue. `run-metal-matrix.py --legs usbnet`: PASS (DHCP OFFER, ping 10.0.2.2); the full CI q35
  matrix 13/13 (`proof/bug-gpf-2026-11/q35-matrix.txt`).

### BUG-040 — ring-0 #GP at a stack address: a glibc program ran before its launcher published it ✅ FIXED
- **Status:**   FIXED (`22a1685`, `feature/bug-gpf`) · **Severity:** CRITICAL (kernel fault), rare
- **Found:**    2026-11-05 · R630 KVM, 1 h soak of `52755fc` (the published 2026.11.05 image), t = 2825 s:
  `[idt] GENERAL PROTECTION FAULT code=0x0 @ 0x7fe46c12 cs=0x8 ss=0x10 rsp=0x7fe46b80` in the third
  run of a `churn proc 4` (`proof/net-worker-2026-11/r630-base-soak-gpf-tail.txt`)
- **Analysis:** RIP and RSP are in the boot task's stack: the CPU executed stack bytes. The crashing
  launch is the only one of 252 `gtiny` launches in that log without its `[glibc] spawned scheduled
  task` line: the launcher was preempted between `sched::spawn_user` (task Ready) and its stores of
  `GLIBC_MAIN_TASK`, the caps, the kernel-stack owner and the profile, all done with interrupts
  on. The program ran in that window, `GLIBC_MAIN_TASK` still `usize::MAX`: its `exit_group` took
  the foreground (musl excursion) branch, `EXITED = 1`, and `syscall_entry` returned through a
  stale `SAVED_KERNEL_RSP` (`mov rsp, …; pop ×6; ret`) into the boot task's stack. The BUG-024
  class; daemon and bg-musl had their window closed then, the three glibc launch paths not.
- **Is this the cause of the #GP?** Demonstrated, not reproduced unprovoked. With the window held
  open (`[bug040]` self-test) and the old launch order, the same image dies at the same place with
  the same signature: R630 KVM: no spawn line, silence after `GLIBC-DYNAMIC-OK`, the NMI probe finds
  the glibc task after `exit_group(42)` with `RSP=0x7fe46b30` (0x50 below the original fault's RSP)
  at a wild RIP; TCG: reset. Unprovoked on `52755fc`: 23 runs of 50 × `churn proc 20` (24 150
  launches) and one more 1 h soak, no fault; the window is a few instructions against a 10 ms tick.
- **Fix:**      `sched::spawn_user_parked` / `start_parked`: the task is not runnable until
  `ring3::spawn_glibc_main` has stored everything (one helper for `run_glibc`, the disk and the
  persistent variant). A full task table returns an error instead of indexing with `usize::MAX`.
- **Regression test:** boot self-test `[bug040]` gives the CPU away for up to 1 s between spawn and
  start: the task must not run, `gtiny` must exit 42 through the scheduled exit. (It needs a glibc
  arena: skipped at 512 MiB, runs at 1 GiB and on the R630.)
- **Proof:**    `proof/bug-gpf-2026-11/`: the analysis, the provoked runs, 24 TCG boots + megaraid + the CI q35 matrix (0 markers worse), 168 R630 KVM boots, two 2 h soaks with 5 430 glibc launches (0 faults, heap +0.23 kB/min).

### BUG-041 — an image file can panic the kernel: header bombs and unchecked JPEG segments ✅ FIXED
- **Status:**   FIXED (`5090eff`) · **Severity:** CRITICAL (kernel panic from a file or a web page)
- **Found:**    2026-11-10 · host tests written while moving EuroPaint's flood fill into `crates/euromedia`
  (`robusttests.rs`); not seen in a guest first
- **Analysis:** the decoders trusted the file. QOI: 14 bytes with width = height = 65535 went to
  `Vec::with_capacity(4 294 836 225)` pixels, a 16 GiB allocation (the allocation-failure handler
  halts the kernel). PPM and BMP the same from their headers; PNG through an unbounded inflate of one
  block. JPEG indexed marker segments, table selectors (`qtables[tq]` with `tq` up to 15 of 4),
  sampling factors (0 divides, underflows) and Huffman size categories straight from the data:
  2003 truncations of the four test JPEGs panicked the old decoder. The image viewer, EuroPaint and
  the web view (`webview.rs`: `<img>` over HTTP(S) and `data:` URLs → `euromedia::decode` /
  `decode_ppm`) feed these decoders files and downloads.
- **Fix:**      `MAX_PIXELS` (16 Mpx) with `pixel_count()`, fallible `Image::try_new` / `try_clone`;
  QOI/PPM/BMP check the claimed size against the data length before a buffer exists; every JPEG
  segment through a bounds-checked `segment()`, selectors and factors validated, a scan that runs
  out of data returns `Truncated`; the inflate output cap (64 MiB) is checked inside a block.
  Host tests: header bombs per format, every truncation and about 50 000 corruptions of 13 sample
  files without a panic.
- **Left:**     `eurodocio` (zip/docx) uses the same inflate and is now capped at 64 MiB per entry,
  but its own parsing was not mutation-tested. `gzip_decompress` has no caller in the kernel.

### BUG-042 — xHCI under KVM `-cpu host`: BAR above the identity map, kernel page fault at boot ✅ FIXED
- **Status:**   FIXED (`bf87957`, with BUG-021) · **Severity:** HIGH (boot halts with any USB controller)
- **Found:**    2026-11-10 · the first KVM run of `scripts/gui-smoke.py` on the R630 (USB tablet and
  keyboard on `qemu-xhci`): `[idt] PAGE FAULT addr=0x380000000000` right after
  `[euro] PS/2 mouse initialized`
- **Analysis:** the BUG-021 mechanism in another driver. With the host's 46 physical address bits
  OVMF puts the 64-bit PCI window at 56 TiB; `xhci::init` read the capability registers through
  BAR0 as an identity-mapped address. The earlier KVM runs used virtio input (its transport maps
  high BARs since the virtio 1.x work) or no input device at all.
- **Fix:**      `xhci.rs`, `nvme.rs` and `hda.rs` translate BAR0 with `paging::mmio_virt` and skip
  the device with a log line when the window is full. R630, `-cpu host`, xhci + usb-kbd +
  usb-tablet + intel-hda + nvme: page fault 4 s into the boot before, desktop after, NVMe
  self-tests and MSI-X OK (`proof/robust-2026-11/r630-highbar-before.txt`, `r630-kvm-highbar-*.markers`).
- **Repro (before):** `python3 scripts/gui-smoke.py --image eurokernel.img --kvm` on a host with
  more than 40 physical address bits.

### BUG-043 — Alt+Tab and Alt+1..4 do nothing on a USB keyboard ✅ FIXED
- **Status:**   FIXED (`54e843f`) · **Severity:** MEDIUM (app switcher and workspaces unreachable from
  the keyboard on any machine without PS/2)
- **Found:**    2026-11-10 · `scripts/gui-smoke.py` under TCG with `usb-kbd`: after Alt+2 the windows
  were still there and a `2` had been typed into the terminal; the same steps passed with
  virtio-keyboard
- **Analysis:** `xhci::inject_keyboard` turns the modifier byte of a HID report into set-1
  scancodes for Shift and Ctrl only. Left Alt (bit 2) was dropped, so the keyboard layer's Alt
  latch never set and `ps2::poll_key` saw a plain Tab or digit.
- **Fix:**      left Alt transitions are forwarded as `0x38` / `0xB8` before the keys of the same
  report. Right Alt (AltGr) is not: it would turn AltGr combinations into shortcuts.
- **Proof:**    gui-smoke over `usb-kbd`: "Alt-Tab shows the switcher", "workspace switch" and the
  launcher step after them pass under TCG and KVM (`proof/robust-2026-11/gui-smoke-tcg.txt`, `gui-soak-usb.txt`).

### BUG-044 — Alt+Tab and Alt+1..4 are typed into the document while an editor has the keyboard ✅ FIXED
- **Status:**   FIXED (`3da84bf`, integration `feature/integ-2026-11`) · **Severity:** LOW (shortcut does nothing; a Tab or a digit lands in the text)
- **Found:**    2026-11-10 · `scripts/gui-smoke.py`, step "Alt-Tab with the text editor in front"
  (KVM and TCG, USB and virtio keyboard); screenshot `proof/robust-2026-11/bug044-alt-tab-editor.png`
- **Analysis:** the app switcher and the workspace switch are recognised in `ps2::poll_key`
  (`0x0F if ALT`, `0x02..=0x05 if ALT`). While EuroText, EuroNotes, the EuroFiles name prompt or the
  install wizard has the keyboard, the desktop loop drains the scancodes with `ps2::poll_key_ex`
  instead, which tracks the Alt latch but has no such arms: Tab comes back as `Key::Tab` and the
  digit as a character.
- **Repro:**    open the Terminal and EuroText from the dock (EuroText in front), hold Alt, press Tab:
  no "Switch window" panel, the caret moves a tab stop. With the Terminal in front the panel appears.
- **Fix:**      the same two arms in `poll_key_ex`, before the `0xE0` arm, each followed by `continue`: every
  consumer of `poll_key_ex` (EuroText, EuroNotes, the Settings panel, the help panel, the install wizard,
  the EuroFiles prompt) leaves the shortcut to the desktop. `KNOWN_ISSUES` in `gui-smoke.py` is empty; the step
  "Alt-Tab with the text editor in front" passes under TCG and KVM (`proof/integ-2026-11/`).
- **Left:**     programs that drain raw scancodes (a full-screen app, an X11 or Chromium window, an installed
  app in front) still keep the two shortcuts to themselves; the help text says so.

### BUG-045 — a window dragged to the left edge covers the dock, and the dock takes the clicks ✅ FIXED
- **Status:**   FIXED (`b65b395`, integration) · **Severity:** LOW (clicks land on something the user cannot see)
- **Found:**    2026-11-10 · while reading the screenshots of the monkey soak; repro
  `proof/robust-2026-11/bug045-repro.py`, screenshot `bug045-window-over-dock.png`
- **Analysis:** the drag code in the desktop loop clamps a window's `x` at 0 (`saturating_sub`), not at
  the dock's right edge (`SIDEBAR_W`), and windows are drawn after the dock. The click path tests the
  dock before the windows (`compositor::dock_icon_at` comes first), so a click on the strip of the
  window that lies over a dock tile opens or closes that tile's app.
- **Repro:**    drag the Terminal by its title bar until the pointer is at x ≈ 120: the window stops at
  x = 0 over the dock. Click inside the window 45 px from the left, at the height of the Settings
  tile: the Settings window opens.
- **Fix:**      `nx` is clamped to `SIDEBAR_W` in the drag and in the un-snap on pickup. The drag of the repro
  script now leaves the Terminal at the right edge of the dock (`proof/integ-2026-11/bug045-after.txt`; the
  last line of the script still says "reproduced" because it clicks at x = 45, which is the dock itself now,
  not a window).

### BUG-046 — an NVMe disk with foreign content was formatted at boot ✅ FIXED
- **Status:**   FIXED (`feature/fast-boot`) · **Severity:** HIGH (data loss on someone else's disk)
- **Found:**    2026-11-10 · reading `main.rs` for BUG-048, reproduced under KVM on the R630
- **Analysis:** an NVMe disk that is not the root is mounted at `/nvme`; when it did not mount as EuroFS it was
  formatted, in every image. A release medium booted on a machine with another system on its NVMe disk logs
  `NVMe disk is not EuroFS and not blank — … it stays intact` and then `[g2] EuroFS formatted on NVMe`.
  A 64 MiB disk of random data with a boot signature had another sha256 after one boot.
- **Fix:** only a disk whose first 4 KiB are zero is formatted (the rule the second virtio disk has); anything
  else is left alone (`[g2] NVMe disk carries unknown content: left untouched (no /nvme)`), and a failed
  format is a log line instead of an `expect`. `docs/proof/fast-boot-2026-11/kvm-nvme-foreign-disk.txt`.
- **Numbering:** this was BUG-042 on `feature/fast-boot` (and is in that branch's commit messages); renumbered at the integration because `feature/robust` used the same numbers.

### BUG-047 — ARP lookups before the configuration is saved cannot see their answers ✅ FIXED
- **Status:**   FIXED: ARP, ping and DNS by BUG-039 (`9553f59`, `feature/bug-gpf`), the TCP probes by `182b19c` (integration) · **Severity:** LOW/MEDIUM (self-test image and `late_bring_up` only)
- **Found:**    2026-11-10 · `[boot-time] phase net` on a virtio-net boot
- **Analysis:** `arp_resolve`, `icmp_ping`, `dns_query` read `legacy_rx()`, which starts with `rx_route()`, and
  `rx_route()` returns at once while `net::get()` is None. The inline bring-up in `main.rs` and
  `late_bring_up` (USB NIC) call them BEFORE `net::save`: nobody polls the NIC, both ARP lookups run into
  their time-out and every virtio-net boot logs `(no ARP reply from the gateway)`, also on an open network
  (so the ping, DNS, HTTP and HTTPS probes behind it never ran on that path). `late_bring_up`'s `poll_dhcp`
  reads `legacy_rx()` too, so it cannot see the OFFER either, which would explain BUG-039 (not verified on a USB NIC).
- **Release image:** not affected any more: `[net-up]` polls the NIC itself until it saves the configuration.
- **After the merge with BUG-039:** `rx_route` polls the NIC without a configuration, so the two ARP lookups, the
  ping and the DNS query of the inline path answer (`[net] ARP: 10.0.2.2 is-at …`, `PING … OK`, `DNS reply … ✓`),
  and `late_bring_up` (ARP and ping only before its `save`) is complete. What was left: `net::classify` sorts TCP
  segments by the SAVED address, so the HTTP and HTTPS probes of the inline path never saw a SYN-ACK:
  `(no HTTP response)`, `(TLS handshake failed)`, and `phase net` 22.2 s under TCG where it had been 11.8 s
  with the lookups failing early.
- **Fix:**      the inline path saves the address from DHCP before the first lookup (gateway and DNS MAC
  zero, as `udrv::boot` does) and the full configuration at the end. TCG, in-kernel e1000: `HTTP GET … 888 bytes ✓`,
  `HTTPS GET … (encrypted) ✓`, `phase net` 16.2 s; the ten-configuration matrix has 0 markers worse
  (`proof/integ-2026-11/`). The release image never ran this path (`[net-up]`).
- **Numbering:** this was BUG-043 on `feature/fast-boot` (and is in that branch's commit messages); renumbered at the integration because `feature/robust` used the same numbers.

### BUG-048 — the release image ran the self-tests, the churn runs and the boot network probes ✅ FIXED
- **Status:**   FIXED (`feature/fast-boot`) · **Severity:** MEDIUM (16 s from kernel entry to the desktop under KVM, 30 s under TCG; test residue in the release system)
- **Found:**    2026-11-10 · `[boot-time]` (new, per boot phase) on the release image, R630 KVM and build-server TCG
- **Analysis:** only the glibc/X11/chrome suite was behind the `selftest` feature. `./scripts/build.sh image`
  still ran about 150 self-tests, the eight churn runs, the inline network sequence (DHCP, ARP, ping, DNS, HTTP
  and HTTPS to example.com, IPv6) and the lockscreen self-test on every boot, wrote 164 kB to the serial port
  and left their residue behind: test files in `/`, six test apps and the example developer key, a
  `selftest-block.example` DNS block, deleted/locked demo accounts, a `[churn-thread] … FAILED` line. The
  bad-block, swap and crash-dump proofs wrote into the GPT gap of virtio disk 0 whatever it held.
- **Fix:** `SELFTEST` in `main.rs` is the one switch; the release image keeps only the self-tests that set up
  state the system uses (list in `docs/BOOT-TIME.md`). The network of an in-kernel NIC comes up in a kernel
  task (`[net-up]`, `net_bringup.rs`); no lockscreen flash on a live medium; no SMP demo. KVM: 19.5 s → 2.95 s
  from QEMU start to the desktop (kernel 16.0 s → 1.0 s); TCG: 36.8 s → 6.5 s (kernel 29.7 s → 2.1 s). The
  self-test image is marker-identical. Proof: `docs/proof/fast-boot-2026-11/`.
- **Numbering:** this was BUG-041 on `feature/fast-boot` (and is in that branch's commit messages); renumbered at the integration because `feature/robust` used the same numbers.

### BUG-050 — an exception through an IST gate on an AP ran on a garbage stack (APs loaded no TSS) ✅ FIXED
- **Status:**   FIXED (`dd4c3c8`, `feature/bug-gpf`) · **Severity:** HIGH when it hits (reset), latent
- **Found:**    2026-11-05 · code review during BUG-032 ("open" in `proof/bug032-2026-11/`)
- **Analysis:** the IDT is shared by all cores and its double-fault, page-fault and NMI gates use IST
  stacks; `gdt::init_ap` loaded the shared GDT and no TSS. TR after INIT has base 0, so an AP read
  the IST pointer from linear address 0x24… and pushed the frame there. Any #PF/#DF on an AP, and
  every monitor `nmi` (`--nmi-on-hang`, `tcg-boot.sh`) since it goes to all cores.
- **Fix:**      a GDT (the BSP's selector layout), a TSS and three IST stacks per AP.
- **Regression test:** `[ap-tss]`: an NMI IPI to every AP; each must run on its own NMI stack with TR
  loaded. With `load_tss` left out the same image resets in this self-test
  (`proof/bug-gpf-2026-11/tcg-ap-tss-selftest.txt`).

### BUG-051 — the periodic integrity check ran and was counted, but its log line was dropped ✅ FIXED
- **Status:**   FIXED (`7bf0a2d`, integration) · **Severity:** LOW (the check itself ran; its record did not exist)
- **Found:**    2026-11-10 · EuroBeheer, Storage, on the live image: "Checks since start: 2" next to "The log of the
  checks so far (/var/log/fsck.log) is empty or cannot be read"
- **Analysis:** both statements were true. `scrub::run` appended its line to `/var/log/fsck.log` with the uid of
  whoever called it and ignored the result. The release image no longer runs a pass before the session opens
  (BUG-048), so every pass comes from the desktop loop under the signed-in user, who may not write `/var/log`:
  two `[g5] scrub #…` lines on the serial log, then `cat /var/log/fsck.log: NotFound`
  (`proof/integ-2026-11/scrub-log-before-after.txt`). An interaction of `feature/fast-boot` and the periodic scrubber;
  the "Check now" button of the panel already ran in system context.
- **Fix:**      `scrub::run` appends the line in system context whoever calls it, and prints a `[g5]` line when the
  write is refused. After two passes the file holds two lines and the panel shows the last one.

### BUG-052 — right Alt (AltGr) counts as Alt in `poll_key`, not in `poll_key_ex` ✅ FIXED
- **Status:**   FIXED (`98bb550`, `feature/uidebug`) · **Severity:** LOW (AltGr+Tab or AltGr+1..4 switched window or workspace in the Terminal; the AZERTY and QWERTZ layouts had no way to type `@ # { } [ ] \ | ~` or the euro sign)
- **Found:**    2026-11-10 · reading `ps2.rs` for BUG-044; not tried on a keyboard
- **Analysis:** `poll_key_ex` consumed the `0xE0` prefix and ignored `E0 38`. `poll_key` had no `0xE0` arm: the prefix
  was dropped as a break code and the following `0x38` set the Alt latch. PS/2 and virtio keyboards only (the USB
  path forwarded left Alt alone, BUG-043, and did not forward right Alt at all). `eurokeymap::translate` had no AltGr level.
- **Fix:**      `eurokeymap::level3` and `translate_ex` (the third level of be-azerty, fr-azerty and de-qwertz as on the
  key caps; no dead-key composition, no Shift+AltGr). `ps2.rs`: the prefix is remembered across polls (`EXT`: its
  code can arrive in a later loop iteration), `E0 38` / `E0 B8` set and clear `ALTGR` in both decoders and never
  touch `ALT`, and with AltGr held a key without a third level types nothing. `xhci.rs` forwards the right-Alt
  modifier bit as `E0 38` / `E0 B8`. See BUG-072 for the keys that were wrong below the AltGr level.
- **Shown by:** 8 host tests in `crates/eurokeymap` (each of the ten characters on exactly one key per layout; AltGr never
  repeats the base character); boot self-test `[altgr]` (both decoders, the three layouts, AltGr+Tab and AltGr+1..4
  latch nothing, a prefix split over two polls, left Alt still latches); `gui-smoke.py` steps "AltGr: third-level
  characters …" and "AltGr+Tab and AltGr+2 … no switcher, no workspace change" (Terminal and EuroText), USB and
  virtio keyboard, TCG and KVM (`proof/uidebug-2026-11/`).

### BUG-053 — a glibc launcher that fails after it took its arena keeps the arena ✅ FIXED
- **Status:**   FIXED (`082afd1`, `feature/kdebug`) · **Severity:** LOW (needs a damaged executable or loader; 64 to 384 MiB per failed start)
- **Found:**    2026-11-10 · converting the callers of `build_address_space*` (`a19d507`); read, not provoked
- **Analysis:** `spawn_glibc_persistent` (`?` on `load_elf64` and `alloc_thread_kstack`), `glibc_disk_launch`
  (`(bad ld.so ELF)`, `(disk exe segment map failed)`, which also leave the demand flags switched on) and `run_glibc`
  (`(bad exe ELF)`, `(bad ld.so ELF)`) return after the arena was allocated without freeing it. The new
  out-of-frames exits in the same functions do free it. Also still `expect`s on allocation, boot only:
  `daemon-arena`, `daemon-kstack`, `utask-arena`, `utask kernel-stack`.
- **Provoked:** 2026-11-12, `[bug053]` on the unfixed tree, 1 GiB, TCG: `spawn_glibc_persistent` with an
  executable that is no ELF: `bad executable refused=true kept 49152 frames` (192 MiB), `free frames
  105708 → 56556 → FAILED` (`proof/kdebug-2026-11/bug053-before-fix-1g.txt`).
- **Fix:**      `glibc_launch_abort` / `glibc_disk_launch_abort` give back the demand pool, the address
  space (with the vDSO's table chain) and the arena; every exit after the arena allocation in
  `run_glibc`, `spawn_glibc_persistent` and `glibc_disk_launch` goes through one of them. The disk
  launch restores the two demand-paging flags on every exit (also when it found no arena, where it
  left them on too). `vdso_prepare` frees a half-allocated image. The four `expect`s are a log line
  and a return.
- **Regression test:** boot self-test `[bug053]`: the frame allocator drained down to the arena of the
  process, one frame handed back per attempt, for a background musl process (refused 8 times), a
  foreground program (4) and a glibc process (6); every refusal must leave the frames it found; the
  first attempt with enough frames runs with nothing else free; then the bad executable (0 frames
  kept) and an ordinary launch (exit 42). The glibc part needs a glibc arena (skipped at 512 MiB under
  TCG). Not provoked: the disk-served launch, `(no kernel stack)`, `(scheduler task table full)`.

### BUG-054 — the default desktop background has a vertical seam at half the screen width ✅ FIXED
- **Status:**   FIXED (`98bb550`) · **Severity:** COSMETIC
- **Found:**    2026-11-10 · reading the screenshots of the integration (it is in the earlier ones too,
  `proof/fast-boot-2026-11/kvm-after-desktop.png`)
- **Analysis:** `compositor::draw_wallpaper_compute` approximated the diagonal gradient with two row fills per line,
  left half and right half, each in one colour; the step between them was visible at x = width / 2.
- **Fix:**      the row is filled in 8 px segments whose colour follows the diagonal, in integer arithmetic (no
  soft-float call per segment; the picture is computed once and cached, as before).
- **Shown by:** `proof/uidebug-2026-11/seam.txt`: the largest colour step between two neighbouring pixels of a row,
  measured on the screendumps of the empty desktop, at x = width / 2 and elsewhere, before and after, 1920x1080
  and 1280x720; `before/1080/01-desktop-empty.png` against `after/1080/01-desktop-empty.png`.

### BUG-055 — EuroBeheer, Storage: "1 MiB of 8 MiB used (21%)", and "no virtio-blk devices" on a SATA boot ✅ FIXED
- **Status:**   FIXED (`98bb550`) · **Severity:** COSMETIC
- **Found:**    2026-11-10 · the Storage screenshots of the integration (`proof/integ-2026-11/screenshots/`)
- **Analysis:** `settings_ui::mib` rounded both figures down to whole MiB while the percentage came from the bytes
  (1.7 MiB of 8.0 MiB is 21 %), so a small file system read inconsistent. The "Disks" list was `fatmount::lsblk`,
  which knows virtio-blk only: a system booted from SATA, NVMe or USB was told there are no disks.
- **Fix:**      `rootblk::human_bytes` (one decimal below 100 of a unit, rounded to the nearest; the percentage is
  rounded the same way) and `rootblk::all_disks`: virtio-blk, SATA (AHCI, with the model string), NVMe (model),
  MegaRAID logical drives and a USB disk, each with name, size and bus, the disk that carries the root marked
  "(system disk)". The Storage page lists them; `lsblk` in the shell appends the non-virtio ones.
- **Shown by:** boot self-test `[disks]`; `proof/uidebug-2026-11/disks.txt` and `disks-four-buses.png`: the page and
  `lsblk` on a SATA boot medium with a virtio disk, an NVMe disk and a second SATA disk attached; screenshots
  `before/1080/20-eurobeheer-storage.png` and `after/*/20-eurobeheer-storage.png`. The "(system disk)" mark was not
  looked at on an installed system in this pass (the harnesses that install do not open the panel).
- **Not listed:** an NVMe disk handed to the user-space driver (`storage.nvme=user`), and disks behind a controller
  without a kernel driver.

### BUG-056 — the lock screen paints 25 to 45 s per frame under TCG (self-test image, 1920x1080) ✅ FIXED
- **Status:**   FIXED (`6022a8b`, `feature/kdebug`) · **Severity:** LOW (TCG boots of the self-test image; no marker changes; the release image under KVM is not slow)
- **Found:**    2026-11-10 · `[boot-time]` of the integrated matrix boots against `proof/fast-boot-2026-11/`:
  `lockscreen` 50 s -> 94 s in all ten configurations, 150 to 165 s to the desktop where fast-boot had 97 to 114 s
- **Analysis:** bisected over the merge commits with timestamped boots, then with throwaway TSC marks
  (`proof/integ-2026-11/lockscreen-bisect.txt`). The sign-ins take milliseconds. The time is
  `lockscreen::render`'s background, `fill_rounded_rect_grad(0, 0, w, h, 0, …)`: a float gradient and a
  `put_pixel` per pixel, written for icon tiles, here over two million pixels, twice per boot (self-test frame and
  the frame before the unattended sign-in). 60 000 to 64 000 Mcycles per frame up to the robust merge (`11c30d2`),
  99 000 to 105 000 after the settings merge (`05218c2`), about a tenth more after the help merge (timestamps
  only); neither that function nor `Color::lerp` changed in between, so the difference is in how the same code
  was compiled into the larger kernel (not established further). The kernel is built without hardware float:
  every `f32` operation is a call into the soft-float routines of `compiler_builtins` (`__mulsf3`, `__nesf2`, …;
  the NMI probe of BUG-057 stopped in one), which is why per-pixel float code costs this much, most of all under TCG.
- **Not affected:** the release image on a live medium skips the lock screen (`lockscreen` 9 ms). Installed system,
  release image, R630 KVM: first frame plus a refused sign-in 0.44 s after the mark before the lock screen.
  Self-test image under KVM: `lockscreen` 5.4 s.
- **Why it got slower without a change** (2026-11-12): the settings merge added a fourth call site of
  `Color::lerp` (`compositor.rs`), and at `opt-level = "s"` LLVM stopped inlining it: `fill_rounded_rect_grad`
  is 1481 bytes with no `lerp` symbol in the link maps of kernels built before that merge, 951 bytes
  plus a 454-byte `Color::lerp` since. Out of line `lerp` makes 26 soft-float calls per call; nine of
  them (six `u8` to `f32` conversions and three subtractions of the two end colours) do not depend on
  the pixel and are hoisted out of the loop only when it is inlined. Measured under TCG, same host
  load: `phase lockscreen` 88.1 s on the base tree, 52.4 s on the base tree with `#[inline(always)]`
  on `lerp` (fast-boot had 50 s).
- **Fix:**      `lockscreen::paint_background`: a row of this gradient is about a dozen runs of one
  colour and every channel is monotone along the row, so the end of a run is found by bisection with
  the same float expression and the run is filled with one row fill: about a hundred evaluations per
  row instead of 1920, no `put_pixel`. The picture is the same pixel for pixel: `[ag-lock]` compares
  15 360 pixels of the painted frame with the per-pixel expression in every boot (0 off), and full
  screenshots of base and fix are compared in `proof/kdebug-2026-11/lockscreen-pixel-compare.txt`.
- **Before / after** (self-test image, `phase lockscreen` = two frames + 0.6 s grace): TCG 88.7 s →
  5.4 s, one frame about 40 s → 2.6 s, kernel entry to desktop 160.6 s → 71.0 s
  (`user-smp4-noiommu`); R630 KVM 5.39 s → 0.87 s, one frame about 2.4 s → 0.18 s.
  `fill_rounded_rect_grad` and `Color::lerp` themselves are unchanged (the icon tiles use them).

### BUG-057 — the notification list stays open and the desktop repaints everything in every loop iteration ✅ FIXED
- **Status:**   FIXED (`98bb550`) · **Severity:** LOW/MEDIUM (a sluggish desktop for as long as the list is open)
- **Found:**    2026-11-10 · monkey soaks of the integration on the R630 (USB input): one run with a failed functional
  check after 2 200 actions, one stopped as "screen frozen" after 1 750 (`proof/integ-2026-11/gui-soak-usb-run1.txt`,
  `gui-soak-usb-run2.txt`); in both the notification list was open
- **Analysis:** the list opened and closed only by a click on the status panel (`notify::toggle_centre`; `close_centre`
  had no caller): no Esc, no click outside. While it was open `need_full` was set in every iteration. The same line
  of `main.rs` did that for an open context menu, the launcher, the file dialog, the symbol picker, the Alt-Tab
  switcher, a file being dragged, and for 3.2 s after every notification (the toast). With seven windows on the R630
  under KVM a pointer move showed after 0.3 to 0.4 s instead of 0.04 s.
- **Fix:**      the loop repaints for an overlay when what it shows changes: the tuple of open flags,
  `notify::active_toasts` and `notify::generation`; the context-menu row under the pointer; the pointer while a file
  is dragged. With an overlay up the 2 s tick and a key in the Terminal or the calculator (which redraw a window in
  place) become one full repaint. Esc closes the list and a context menu, and that Esc is taken out of the key ring
  (`ps2::take_pending_esc`); a left click beside the list closes it and is used up, a click on the list no longer
  reaches the window under it, a right click closes it and opens its menu. New shell command `repaints`.
- **Shown by:** `proof/uidebug-2026-11/frame-time.txt` (R630 KVM, seven windows, list open): pointer visible after
  291 to 373 ms before, 39 to 46 ms after; console echo median 100 ms before, median 50 ms after. `repaints` with the
  list open: 2 full repaints in 416 loop iterations (4.3 s). `gui-smoke.py`: four steps on the list, one on the menu; the same steps fail on the image
  without the fix (`gui-smoke-kvm-baseline-image.txt`). Monkey soaks 35 min USB and virtio.
- **Left as it is:** one full repaint per pointer move while a file is dragged out of EuroFiles, per change of the
  highlighted menu row, and while a window is dragged or resized or EuroPaint draws a stroke. The help panel and the
  tour already repainted only after input and once per tick.

### BUG-058 — EuroReken lower than 252 px draws its keypad down to the bottom of the screen ✅ FIXED
- **Status:**   FIXED (`98bb550`) · **Severity:** LOW (picture only; the desktop keeps working)
- **Found:**    2026-11-10 · looking for the cause of BUG-057; `proof/integ-2026-11/screenshots/bug058-calculator-200px.png`
- **Analysis:** the desktop let a window shrink to 320 x 200. `calc_ui::button_area` leaves 0 px for the keypad below
  252 px of window height, and `button_rect` then computed `(ah - gap * (ROWS - 1)) / ROWS` in `usize`: it wrapped,
  the 36 keys became six columns as tall as the screen, drawn outside the window. Nothing confined an app's drawing
  to its window: the framebuffer had no clip rectangle.
- **Fix:**      three layers. (1) `graphics.rs`: a clip rectangle (`set_clip` / `clear_clip`, honoured by `put_pixel`,
  `fill_rect`, `blit_xrgb`; `fill_rect` saturates), and `compositor::draw_window_body` draws every app inside its
  window rectangle. (2) `calc_ui`: saturating layout; a keypad or "=" bar that does not fit is neither drawn nor
  clickable. (3) the resize grip keeps the calculator at 500 x 380 or larger (`main.rs::min_window_size`); every other
  app still goes down to 320 x 200 and is cut off at the window edge there.
- **Shown by:** every dock app at its smallest size and at its default size, at 1920x1080 and 1280x720
  (`proof/uidebug-2026-11/after/*/app-*-smallest.png`, looked at: nothing outside a window);
  `before/1080/app-06-calculator-smallest.png` shows the keys down the screen.
- **Seen there, cosmetic, not changed:** at 320 px width EuroText's Open and Save buttons lie over the file path,
  and EuroFiles shows two toolbar buttons (the rest need a wider window, as its help page says).

### BUG-059 — `[robust-fs]` FAILED on a root the installer self-test had just provisioned (no `/tmp`) ✅ FIXED
- **Status:**   FIXED (`fb18477`, integration) · **Severity:** LOW (a self-test, not the code it tests)
- **Found:**    2026-11-10 · the boot of the R630 soak guest (`loadtest.py --disks 8G`): `[robust-fs] … editor
  refused+kept document=false ("opened: /tmp/robust-big.bin"), helper=false, … → FAILED ✗`; OK in every RAM-root
  boot of the matrix
- **Analysis:** with a blank virtio disk the self-test image installs onto it (`[q1x3]`) and runs on that root, which
  has no `/tmp` until the `[edit]` self-test saves its file (`textedit::save` creates the directory). `[robust-fs]`
  runs just before `[edit]`, on `feature/robust` as here, and wrote its two test files into `/tmp` without creating
  it; a missing file then "opens" as a new document. `feature/robust` never booted with a blank virtio disk.
- **Fix:**      the self-test creates `/tmp` first. Self-test code only (`cfg(feature = "selftest")`).

### BUG-060 — every program launch wiped the descriptors of the background processes ✅ FIXED
- **Status:**   FIXED (`cb65a7e`, `feature/kdebug`) · **Severity:** HIGH (a background process silently loses its pipes and open files; wrong data goes to the wrong descriptor)
- **Found:**    2026-11-10 by reading (`proof/bug-gpf-2026-11/`, "the fd tables are global"); provoked 2026-11-12 with the `[fdspace]` self-test
- **Analysis:** `OPEN_FDS`, `OPEN_DIRS`, `PIPE_FDS`, the pipes, `PIPE_NONBLOCK`, `PIPE_WAITERS` and the per-fd
  status flags were one global set, and every launch (`run_args`, `run_glibc`, the disk and persistent
  variants, `run_dynamic`, `run_interp`) begins with `reset_fd_table`, also when it then finds no arena.
  BUG-037 made that reset atomic; it stayed a wipe. `[fdspace]` on the unfixed tree: two background
  processes open a pipe and a file (the second gets fds 6, 7, 8: one table for both), a musl and a glibc
  program are launched in the foreground, and then `write=2 read=0` on the pipe (the write went to the
  console) and `lseek=-1` on the file, in both. Two smaller leaks sat in the same tables: a close in one
  process reset the `F_GETFL` flags of that fd number for every process, and a dead background process
  kept its fd numbers until the next launch.
- **Fix:**      fd spaces (`crates/eurofdspace`, `kernel/src/fdspace.rs`): a space owns descriptors. `ROOT`
  is the launcher, the kernel tasks and the one foreground or glibc process with its threads and fork
  children; `reset_fd_table` clears that space only. Every background process (`spawn_bg_musl`,
  `spawn_bg_app`) gets its own; its threads use it, a fork child shares it with its parent (reference
  counted), and it is released when the last process of the group is reaped. The locks are the same
  locks, taken where and how they were: a guard dereferences to the table of the calling task's space.
  A start that finds no heap for the tables (36 kB) is refused.
- **Regression test:** boot self-test `[fdspace]` (every build) and 8 host tests in `eurofdspace`.
- **Left:**     the glibc group is still one space with the per-child overlays (`FD_ALIAS`, deferred closes);
  epoll, eventfd and socket descriptors are numbered globally. Design for the next step:
  `proof/kdebug-2026-11/README.md`.

### BUG-061 — a glibc process without frames for its vDSO tables was started anyway ✅ FIXED
- **Status:**   FIXED (`082afd1`, `feature/kdebug`) · **Severity:** LOW (needs a launch with fewer than three free frames after its page tables)
- **Found:**    2026-11-12 · the `[bug053]` self-test on the unfixed tree, 1 GiB, TCG
- **Analysis:** `run_glibc` and `glibc_disk_launch` write the auxv (with `AT_SYSINFO_EHDR`) before they map
  the vDSO; when `vdso_map_into` found no frames for the three table levels they printed `[vdso] mapping
  FAILED — the auxv promised one; expect a crash` and started the process. With the arena plus three
  frames free: `glibc refused x3, ran with 3 spare (exit 139)`.
- **Fix:**      a refusal, `(out of memory: vDSO mapping)`, that frees the address space and the arena, as
  for the arena's own page tables. `[bug053]`: `glibc refused x6, ran with 6 spare (exit 42)`.

### BUG-062 — the same commit built twice gave two different `eurokernel.efi` ✅ FIXED
- **Status:**   FIXED (`dbf8460`, `feature/kdebug`) · **Severity:** LOW (builds; the release pipeline's reproducibility claim)
- **Found:**    2026-11-10 · integration, when the release kernel was relinked for its symbols
- **Analysis:** two clean builds of `7858b72` in one checkout differ in 4 bytes: the COFF `TimeDateStamp`
  of the PE header (file offset 0x80) and its copy in the debug directory (0x406ae44); lld-link writes
  the link time there. Nothing else differs (`proof/kdebug-2026-11/repro-binary-diff.txt`).
  `scripts/repro-build.sh` passed `/Brepro` against exactly that, but in `RUSTFLAGS`, which replaces the
  `rustflags` of `.cargo/config.toml`: the CI job "Reproducible build" compared two builds of a kernel
  without `panic=abort`, frame pointers, the link map and the forced soft crypto back ends. And kernel
  and loader are linked with the same `/MAP:target/kernel.map`, the loader last, so the map left behind
  was the loader's (hence the relink).
- **Fix:**      `/Brepro` in `.cargo/config.toml` for every build; `repro-build.sh` adds its remap flags
  through `--config`, refuses a `RUSTFLAGS` in the environment, pins the build version to the commit
  date; `build.sh` keeps the kernel's map as `target/eurokernel.map`. After: two clean `build.sh` builds of the self-test kernel and of the release kernel are byte-identical, and the CI procedure reports REPRODUCIBLE.
- **Left:**     two machines or two checkout paths were not compared; `EUROOS_BUILD_VERSION` is the date in
  `build.sh` by design.

### BUG-063 — after a foreground program, a driver's syscall returned through the foreground exit path ✅ FIXED
- **Status:**   FIXED (`0009b95`, `feature/kdebug`) · **Severity:** CRITICAL (kernel fault or silent corruption of the boot task's stack)
- **Found:**    2026-11-12 · the first version of the `[fdspace]` self-test under TCG with `net.driver=user`:
  3 boots of 3 died right after its foreground program returned
  (`proof/kdebug-2026-11/bug063-tcg-crash.txt`); not seen in a guest before
- **Actual:**   the launcher's locals were garbage (a self-test line printed with wild strings and counts),
  then `[idt] GENERAL PROTECTION FAULT code=0xa090 @ timer_switch+0x3e` (the `iretq`), `rsp` in the boot
  task's stack, the frame made of boot-stack words.
- **Analysis:** a foreground program leaves through `sys_exit`: `EXITED = 1`, and `syscall_entry` returns to
  the launcher through `SAVED_KERNEL_RSP`. The flag stayed 1 until the next launch cleared it, and
  `syscall_entry` tests it after every syscall of every task. `daemon_dispatch` and `bg_dispatch` clear
  it first thing for that reason; `udrv::syscall` (the userspace drivers) and the background routes that
  return before `bg_dispatch` (nanosleep, fork, wait4, socket calls, the two Euro sleeps) do not. The
  next syscall of `e1000d` after the foreground program had ended took the exit path in the driver's
  task: `mov rsp, [SAVED_KERNEL_RSP]`, six pops and a `ret` on the boot task's stack. The BUG-040
  class through another door. The boot's own foreground runs leave interrupts off until the next
  launch, which is why it had not been seen; the shell restores them after `run_args`, so a foreground
  program typed in the terminal while a userspace driver polls was exposed. The load tests start glibc
  and background processes, not foreground musl programs.
- **Fix:**      the exit epilogue and `force_kernel_return` clear the flag: it is set and consumed inside
  one syscall, with interrupts off.
- **From the desktop** (2026-11-12, `proof/integ-2026-11b/bug063-desktop.txt`): `hello` typed in the Terminal on a guest
  with an e1000e NIC (e1000d): the public image of `7858b72` dies with `[idt] PAGE FAULT` in 5 of 5 runs, the
  integrated image runs it in 5 of 5. `gui-smoke.py` has the step (`--net e1000e` for the driver).
- **Regression test:** boot self-test `[bug063]`: after a foreground program the flag is 0, then 30 ticks
  with interrupts on while the drivers and background processes make syscalls. `[fdspace]` does the
  same with two busy background processes.

### BUG-064 — the `[m43-wire]` self-test raced the interrupt for its USB audio completion events ✅ FIXED
- **Status:**   FIXED (`ff8d562`, `feature/kdebug`) · **Severity:** LOW (a self-test; CI leg `usbaudio`)
- **Found:**    2026-11-12 · CI q35 matrix of the kernel debug round, leg `usbaudio`: `[m43-wire] … 0 packet(s)
  consumed by the USB DAC → no consumption ✗`, with no change in the audio or xHCI code
- **Analysis:** `xhci::usbaudio_play` counts the transfer events it reads from the xHCI event ring, and
  `xhci::poll` reads the same ring from the timer interrupt; the self-test runs with interrupts on. Nine
  runs of the leg: 0, 0, 2, 2, 1 packets on the branch, 1, 2, 2, 1 on the base
  (`proof/kdebug-2026-11/usbaudio-runs.txt`): a race on both, lost twice.
- **Fix:**      the self-test calls `usbaudio_play` with interrupts off. Three runs of the leg after it: 2 of 2 packets each; the full matrix on the final image is 13 of 13.

### BUG-065 — the CSPRNG stays unseeded in some KVM boots: `[3d8]`, and `[3d7]`, `[3d3]` with it ✅ FIXED
- **Status:**   FIXED (`35a60c5`, `feature/integ-2026-11b`) · **Severity:** HIGH (keys, seeds and nonces drawn from an unseeded generator were all-zero buffers: every caller ignored the refusal)
- **Found:**    2026-11-12 · R630 KVM boot loops of the kernel debug round, by reading the FAILED lines of boots that
  were otherwise ok: `[3d8] … jitter-alone≈167bits→ready=false, seeded(jitter+TPM,from-tpm=false)-ready=false →
  FAILED` in 8 of 100 boots of the branch's final image and 5 of 78 of its first-round image. Older than that:
  `proof/robust-2026-11/r630-kvm-highbar-smp4.markers` has the line with 21 bits.
- **Before the fix, measured again** (`proof/integ-2026-11b/`, the merged tree `1c8eb91`, R630, KVM, `-cpu host`,
  204 boots): `[3d8]` FAILED in 13 (6.4 %), the generator unseeded for the whole boot in 11 (5.4 %), `[3d7]` FAILED
  in those 11.
- **Cause.**    Three things, each measured or read:
  1. *The noise source.* `gather_jitter` timed one multiplication between two RDTSCs, about 20 cycles. A sample
     took one of two to four values, and what changed from sample to sample was the beat between the core's
     clock and the TSC's: `12, 12, 12, 12, 38, …` with a 38 about every fifth sample in one P-state,
     `18, 18, 18, 19, …` in another, and in the boots that failed `24, 24, 24, …` with a `28` every few dozen
     samples (the raw samples of the 12
     failing boots of a second run of 204: `bug065-samples-failing-boots.txt`). Which of these a boot sees
     depends on the frequency the host's governor has the core at in that millisecond; it changes from batch
     to batch within a boot. `estimate_jitter_bits` credits a constant nothing, as it should.
  2. *`init` gave up.* 64 batches of 512 samples, about 0.6 ms, ready or not, and nothing seeded the pool later.
  3. *The callers.* `entropy::getrandom` returns `false` and leaves the buffer alone when the pool is not
     ready. None of its 18 call sites looked at the result: the EuroCA and attestation keys, the NTS nonces,
     the verity salt and key, the vault master key and nonce, the policy-bundle key and WASI `random_get`
     went on with the zeroed buffer. That is why `[3d7]` and `[3d3]` failed in the same boots: two nonces
     that are both zero make a replayed answer look fresh.
- **Not the cause:** where the loop lies. The kernel debug round suspected the cache-line placement of the two
  RDTSCs. A diagnostic kernel ran the same loop from two assembly copies in every boot, one inside a 64-byte
  line and one straddling a boundary: over 60 boots on an idle host the straddling copy was credited more in
  35 and less in 25, the medians are 3 829 and 3 892 bits per 64 batches (`bug065-diagnosis.txt`). The placement changes
  the cycle count of an iteration and with it which P-states give a constant; it is not what makes the
  source poor.
- **Fix.**
  - `crates/euroentropy`: `EntropyPool::seed_from` is the seeding procedure, host-tested (9 tests). Order: TPM
    (if any), RDSEED and RDRAND (if CPUID reports them), jitter. Everything is conditioned into one seed.
    The CPU's two sources are credited 32 and 16 bits per 64-bit word and together at most half the
    threshold, so the CPU's generator alone never opens the pool. At least four jitter batches are gathered
    whatever the other sources gave. Gathering goes on until the 256 bits are there, at most 4 096 batches;
    then the pool stays closed, the status line says so, and a later request continues from what is staged
    (64 more batches, at most once a second). `estimate_jitter_bits` and the threshold are unchanged.
  - `kernel/src/entropy.rs`: a jitter sample now times 32 read-modify-write accesses at pseudo-random places
    of a 512 KiB block (larger than the first two cache levels), the duration of each sample fed back into
    the walk. `getrandom` is `must_use`; `draw::<N>()` returns the bytes or `None`.
  - Callers: the six self-tests draw with `draw()` and print FAILED without bytes; `random_get` returns `EIO`;
    the TLS client random and key share (`net::gather_entropy`, also the RDP client's) take 32 bytes of the
    CSPRNG into their mix, and no handshake starts without them.
  - Every image prints one line: `[entropy] CSPRNG seeded: 537 of 256 bits · sources mixed: tpm=none,
    rdseed=8 words, rdrand=8 words (CPU credit 128 bits, at most half the threshold), jitter=4 batches of 512
    samples (409 bits) · 0.667 ms → OK`, or `CSPRNG NOT SEEDED: … → FAILED`.
- **After** (`proof/integ-2026-11b/`): 204 KVM boots on the R630 (`-cpu host`): `[3d8]`, `[3d7]`, `[3d3]` FAILED in 0, unseeded 0,
  537 bits credited in the median boot, four jitter batches in every boot; 20 KVM boots with
  `-cpu qemu64,-rdrand,-rdseed` and 20 under TCG (jitter only): 0 and 0. A first round on the commit before
  BUG-083/084: 174 + 20 boots, 0 and 0.
- **Cost:** `entropy::init` 0.58 to 1.51 ms under KVM (median 0.75), 4 to 9 ms under TCG; kernel entry to desktop of the
  public image 830 ms (median of 5) against 838 ms before the fix; `boot-time.py` 2.62 s against 2.77 s, inside
  the spread of a shared host.
- **Regression tests:** `[3d8]` (unchanged in what it demands: jitter alone reaches 256 bits in 16 batches; new:
  jitter is in the seed of the global pool); `[entropy]` in every boot; `seed_tests` in `crates/euroentropy`.
- **Left open:** BUG-082. BUG-080 and BUG-081 are fixed on `feature/linux-rand` (`proof/linux-rand-2026-11/`).

### BUG-070 — EuroFiles: the folder icon of a list row is a slanted hook ✅ FIXED
- **Status:**   FIXED (`98bb550`) · **Severity:** COSMETIC
- **Found:**    2026-11-12 · the first screenshots of this pass (it is in `proof/integ-2026-11/` as well)
- **Analysis:** the list rows used the glyph "folder", an SVG path with arc commands, which `icons.rs` does not
  rasterise. **Fix:** the rows use "files", the folder outline of straight segments the Places column already has.

### BUG-071 — EuroReken: the backspace key shows an empty box ✅ FIXED
- **Status:**   FIXED (`98bb550`) · **Severity:** COSMETIC
- **Found:**    2026-11-12 · screenshots. The UI font has no glyph for U+232B. **Fix:** `<x` when
  `text::can_render` says no.

### BUG-072 — keyboard layouts: French 8 key, German punctuation, the keys right of P/L/M on AZERTY, the ISO key ✅ FIXED
- **Status:**   FIXED (`98bb550`) · **Severity:** MEDIUM for a German keyboard (`- / ( ) = ? ; :` and the umlauts were not where the key caps say)
- **Found:**    2026-11-12 · writing the AltGr tables of BUG-052 against the key caps
- **Analysis:** `eurokeymap::base` mapped per layout only the letters and, for AZERTY, the digit row and four
  punctuation keys; everything else was the US table. German: the whole digit row shifted, ß, ü ö ä, `+ # - , .`
  shifted. French: the 8 key gave `!` (it is `_`). Belgian and French: the keys right of P, L and M gave `[ ] ' \`.
  The ISO key left of the bottom row (scancode 0x56, `< >`) was not mapped in any layout, and the USB table had no
  entry for it (HID usage 0x64) nor for the ISO key left of Enter (0x32).
- **Fix:**      `eurokeymap::national` (looked up before the shared tables), 0x56 in all four layouts, the two HID
  usages in `xhci.rs`. Host tests `german_punctuation`, `french_digit_row_and_right_hand_keys`,
  `belgian_right_hand_keys`, `iso_key_left_of_the_bottom_row`. Dead keys type their own character at once.

### BUG-073 — the 2 s housekeeping of the desktop loop did not run while every iteration was a full repaint ✅ FIXED
- **Status:**   FIXED (`98bb550`) · **Severity:** LOW/MEDIUM (ended processes not reaped, services and user-space
  drivers not supervised, the log not flushed, no scrub and no update check for as long as a list, a menu or the
  launcher was open; a tick that coincided with any other full repaint skipped them too)
- **Found:**    2026-11-12 · reading the repaint decision for BUG-057
- **Analysis:** `reap_dead`, `init::supervise`, the three `udrv*::supervise`, `flush_log`, `scrub::maybe_run` and
  `update::maybe_check` sat in the `else if tick` branch, which runs only when the tick is not a full repaint.
- **Fix:**      they run on every tick, before the repaint decision. Not provoked separately; the soaks and the TCG
  matrix run with it.

### BUG-074 — EuroFiles: entries below the bottom of the window cannot be reached ✅ FIXED (mouse wheel)
- **Status:**   FIXED (`98bb550`) · **Severity:** MEDIUM (`/bin` has more entries than fit a maximized window)
- **Found:**    2026-11-12 · connecting the mouse wheel: the list had no scroll position at all
- **Fix:**      `files::wheel` and a first-row index used by render, click, drag and tooltip; a mark at the right
  edge shows the position. Still no scroll bar to drag and no key for it (said on the help page).

### BUG-075 — 1280x720: windows open partly outside the screen or under the status panel ✅ FIXED
- **Status:**   FIXED (`98bb550`, `58109e5`) · **Severity:** MEDIUM on a small screen
- **Found:**    2026-11-12 · the first screenshots at 1280x720 (`proof/uidebug-2026-11/before/720/`): EuroClock almost
  entirely below the screen, EuroNotes and EuroLog under the status panel, the Terminal and EuroWeb cut off at the
  bottom; the workspace pager drawn on the bottom line of the help panel
- **Analysis:** the default window positions and sizes in `main.rs` are constants laid out for 1920x1080.
- **Fix:**      `main.rs::fit_window` at desktop start and when an installed app's window is placed: inside the
  screen, left of the status panel when that leaves 640 px, above the pager; nothing changes at 1920x1080. The
  pager is not drawn while the help panel is open. For testing: boot option `display.mode=<w>x<h>` (`bootopt.rs`).
- **Shown by:** `proof/uidebug-2026-11/after/720/` (every dock app, the help panel, the six tour cards, the wizard).
- **Not done:** a window is not moved when it is dragged out of the screen, and the layout does not follow a
  resolution below 1024x600.

### BUG-076 — EuroFiles: toolbar buttons that are not drawn still take a click ✅ FIXED
- **Status:**   FIXED (`98bb550`) · **Severity:** LOW
- **Found:**    2026-11-12 · making Perms, Protect and History fit: `toolbar_click` divided the x position by the
  92 px pitch without looking at the window width, so a click right of the last drawn button in the toolbar strip
  of a 720 px window acted as Perms, Protect or History.
- **Fix:**      one layout function (`tool_rects`) for render, click and tooltip; buttons as wide as their labels,
  so all nine fit the default window. Part of the `[help]` self-test (`files::tip_selftest`).

### BUG-077 — `gui-smoke.py`: a character of several bytes on the serial console becomes replacement characters ✅ FIXED
- **Status:**   FIXED (`c50ddf9`, harness) · **Severity:** LOW (a harness)
- **Found:**    2026-11-12 · the AltGr step: the euro sign of `[e2e] $ echo …` arrived as three U+FFFD
- **Analysis:** the reader decoded every `recv` on its own; the UART delivers the three bytes in separate reads.
  **Fix:** an incremental UTF-8 decoder.

### BUG-080 — `getrandom`, `/dev/urandom` and `/dev/random` of the Linux ABI are not the CSPRNG ✅ FIXED
- **Status:**   FIXED (`f83007a`, with `0b33035`; `feature/linux-rand`) · **Severity:** HIGH (every key a userspace
  program makes: Chromium's TLS, an SSH or GnuPG key; and the stack guard of every process)
- **Found:**    2026-11-12 · by reading, while looking for the callers of the CSPRNG (BUG-065). Measured in this
  round with `userland/randprobe.c` on the image before the fix (`proof/linux-rand-2026-11/`).
- **Before, measured** (self-test image of `91afd7d`, two boots under TCG and two under KVM, two musl and two
  glibc processes per boot):
  - `ring3::fill_random` served the `getrandom` syscall (318, both dispatchers) and reads of `/dev/urandom` and
    `/dev/random`: splitmix64 over a global counter plus `ticks * constant`. The first 8 bytes of a result
    are the generator's state: `scripts/rand-boots.py` predicted words 2 to 4 of 32 of 32 results from
    word 1, under TCG and under KVM.
  - `AT_RANDOM` was the constant `5a45640726c1e083a24d6c0f2ec9e88b` in all three stack builders: 8 of 8
    processes. glibc's stack guard was `0045640726c1e083` and its pointer guard `a24d6c0f2ec9e88b`, musl's
    guard `5a00640726c1e083`, in every process of every boot. A stack protector with a published canary
    protects nothing.
  - `pread` on a random device returned the bytes it was registered with: `00bb7733efab6622…` for
    `/dev/urandom` at offset 0 in every process, 32 zero bytes for `/dev/random`. `main.rs` registers
    `/dev/urandom` again for a chrome run, as an ordinary 4 KiB file; a name lookup takes the first entry.
  - Any flag value was accepted; nothing ever waited or refused.
  - Wrong in the tracker until now: "the pool is seeded before the first process starts". `entropy::init`
    ran at line 2304 of `main.rs`, after the scheduler, the network bring-up, `euroid::init_from_disk` and
    the boot programs.
- **Fix.**
  - `0b33035`: `tpm::init` and `entropy::init` run right after the HPET, before anything that draws. `init`
    works once; a request before it does the same seeding and says so. The pool is taken with interrupts
    off. `fill_wait`/`draw_wait` for kernel callers that cannot refuse.
  - `f83007a`: `fill_random` draws from `entropy::getrandom` in 4 KiB pieces. `getrandom(buf, len, flags)`:
    unknown flags and `GRND_RANDOM|GRND_INSECURE` → `EINVAL`; pool not seeded → the call waits (each round
    gathers 64 batches of noise), with `GRND_NONBLOCK` → `EAGAIN` and nothing written; `GRND_RANDOM` is the
    same generator; `GRND_INSECURE` is accepted and waits like no flag (no unseeded bytes exist here); never
    a short result; at most 32 MiB − 1 per call. Both devices: the same generator for `read` and `pread`,
    one entry per device in the file table, no stored bytes. `AT_RANDOM`: 16 CSPRNG bytes per process
    image. `aslr_slides`: 8 CSPRNG bytes.
  - There is no vDSO `getrandom` and no `/proc/sys/kernel/random`.
- **After** (`proof/linux-rand-2026-11/`): `[linux-rand]` OK in the 10 matrix boots, in 104 short KVM boots on
  the R630 and in 7 comparison boots (TCG, KVM, KVM without RDRAND and RDSEED); over the short boots
  416 `AT_RANDOM` values, stack guards, `getrandom` results and device reads each, no value twice;
  0 of them predicted; 1 MiB streams from `getrandom` and from `/dev/urandom` pass bit balance, chi-square
  and the repeated-block search (they did before as well: splitmix64 is statistically clean; the constant
  and the prediction are what distinguish the two).
- **Cost:**     `churn proc 8` (8 glibc launches) before and after, alternating, three rounds under KVM: median 4 994 against
  5 044 ms, inside the spread of the rounds; the 1 h soak ran 1 366 commands per hour (previous soak 1 314)
  and 1 423 glibc launches; public image boot 2.62 s base, 2.55 s final, kernel entry to desktop 811 and
  812 ms (median of 5). `entropy::init` 0.59 to 1.35 ms under KVM, as before; it only moved.
- **Regression tests:** `[linux-rand]` (every self-test image); `scripts/rand-boots.py` (between boots);
  `userland/randprobe.c` runs on Linux as well, as the reference.
- **Left open:** with a blank virtio data disk the install self-test `[q1x3]` asks for a salt before the early
  seeding and does the seeding itself (one status line and a note; a TPM would be missing from that seed):
  the two init lines belong before the storage phase. A Chromium start on this kernel was run once (40 minutes of a chrome boot under TCG beside the base: the same serial lines per tag, no fault, neither got to a paint); the Chromium team's own
  round is still to come. A read of a random device holds the fd-table lock for the whole request, as
  before, and the generator is slower than splitmix64: a cap per read (short reads are allowed above
  256 bytes) would bound that. `O_NONBLOCK` on the devices is not looked at. BUG-088 (the slides are
  narrow), BUG-089 (no reseed).

### BUG-081 — key material outside the CSPRNG with a weak fallback: `euroid::rand_bytes`, `net::rand_u64` ✅ FIXED
- **Status:**   FIXED (`fd0982b`, `feature/linux-rand`) · **Severity:** MEDIUM (only on a machine without a TPM,
  respectively without RDRAND; the QEMU default CPU model has neither)
- **Found:**    2026-11-12 · by reading (BUG-065). Not provoked.
- **Before:**   `euroid::rand_bytes` (password salts at 10 call sites, the session id of a login, the wallet's
  issuer and holder seeds) asked the TPM and otherwise ran a xorshift seeded with the tick counter.
  `net::rand_u64` (DNS transaction id and source port, TCP initial sequence numbers at four places,
  ephemeral ports, the ASLR slides) was RDRAND when the CPU has it and otherwise `rdtsc ^ hpet ^ counter`.
  Without a TPM, `main.rs` gave fixed bytes to the FDE self-test (`5a…`), the vault as its master key
  (`a5…`, kept for the running system), the VPN (`11…` to `14…`), ML-KEM (`21…`, `22…`), EuroCA (`3c…`),
  the quote (`2a…`, `9e…`) and EuroIDM (`1d…`). The vault's seal nonce was a counter without a TPM; the
  caller nonce of the two TPM policy sessions was `5a…` when `GetRandom` failed; `mktemp` named its file
  after `epoch ^ ticks * constant`.
- **Fix.**      All of them draw from the CSPRNG with `entropy::fill_wait`/`draw_wait`: they wait for the seed and
  have no other source. The pool is seeded before any of them runs (`0b33035`). The TPM arm of the
  `main.rs` seeds is unchanged (`from-tpm=true` still means bytes straight from the TPM).
- **After:**    the matrix (0 markers worse, the `kernel` legs run DHCP, DNS, HTTP and HTTPS through
  `rand_u64`), the public image on the open network (ping, DNS, fetch, https on both NICs), the soak with
  4 httpd clients.
- **Not covered:** no configuration of this proof has a TPM (no `swtpm` on either machine): the early
  `tpm::init` and a pool with TPM bytes in it were not run. BUG-086 (the DHCP id was never one of
  `rand_u64`'s users; it is a constant).

### BUG-082 — the jitter estimator credits a periodic series ⬜ OPEN
- **Status:**   OPEN · **Severity:** MEDIUM (an over-estimate; the source it over-estimated is no longer used)
- **Found:**    2026-11-12 · the raw samples of the BUG-065 diagnosis.
- **Actual:**   `estimate_jitter_bits` counts a sample when its difference from the previous one has low four bits
  other than 0 and 15, and credits a quarter bit per counted sample. The old two-instruction sample gave
  `12, 12, 12, 12, 38, 12, 12, 12, 14, 38, …`: two to four values, a 38 about every fifth sample. Every step
  to 38 and back is counted: 60 to 95 bits per batch of 512, 4 200 bits over 64 batches in the median boot,
  for a series that is mostly a clock beat. The estimator cannot tell a near-period from noise.
- **Now:**      the memory-walk sample is not such a series (88 ticks and up, 20 to 161 distinct values per batch,
  the most frequent value 5 to 19 % of a batch, no period up to 16 in the first 64 samples of any of 108
  boots: `proof/integ-2026-11b/bug065-diagnosis.txt`), and the quarter bit per sample is far below what its
  spread suggests. But the estimate is still a heuristic and not an SP 800-90B assessment.
- **Next:**     add the two SP 800-90B health tests on the raw samples (repetition count, adaptive proportion)
  and a check that refuses a batch drawn from a handful of values; measure the min-entropy of the walk
  off line with the 90B estimators on raw dumps from the R630, a NUC and TCG. This strengthens the gate;
  it was not done in the integration because every boot of every configuration has to be shown to still
  reach the threshold with it.
- **2026-11-12:** not done in the BUG-080/081 round either; what it needs, in five steps, is section 6 of
  `proof/linux-rand-2026-11/README.md`. With BUG-089 the same tests have to run on every reseed.

### BUG-083 — the `[altgr]` self-test left the AltGr latch set: typing on the self-test image gave third-level characters ✅ FIXED
- **Status:**   FIXED (`6fcf24c`) · **Severity:** LOW (self-test image only; the public image runs no self-test)
- **Found:**    2026-11-12 · CI q35 matrix of the integration, legs `usbhub` and `hwprobe`: a command typed on the
  desktop never ran. `feature/uidebug` alone fails the same two legs, `feature/kdebug` passes them
  (`proof/integ-2026-11b/q35-legs-by-branch.txt`); the desktop pass had not run this matrix.
- **Analysis:** `ps2::altgr_selftest` feeds `E0` alone, then `38 03 83 E0 B8`, reads one character and empties the
  ring. The release of AltGr (`E0 B8`) was thrown away undecoded, the latch stayed set for the rest of the
  boot, and every key was looked up in the third level of the layout: `uname` arrived as `€`
  (`typed-on-the-selftest-desktop.txt`).
- **Fix:**      the rest of the ring is decoded before it is emptied; the verdict now includes "latches released at
  the end".

### BUG-084 — staged 3-byte PS/2 mouse packets in 4-byte mode: no window had the keyboard, the Terminal was scrolled back ✅ FIXED
- **Status:**   FIXED (`6fcf24c`) · **Severity:** LOW (self-test image only)
- **Found:**    2026-11-12 · the same two q35 legs, after BUG-083 was fixed and they still failed.
- **Analysis:** the X11 self-tests (`gxwin`, `gxlive`) stage mouse input with `mouse::push_byte`, three bytes per
  packet, ending with the left button down. That press is still pending when the desktop loop starts and
  lands on the Terminal under the pointer, which is how the Terminal came to the front on the self-test
  image. Since the mouse wheel (desktop pass) a PS/2 mouse that answers as an IntelliMouse is in 4-byte
  mode: the staged bytes were framed as other packets, the press was lost and the wheel nibble of a
  misframed packet scrolled the Terminal 63 lines back. The window in front was the empty GTK demo
  window, which takes no keys.
- **Fix:**      `mouse::push_packet(flags, dx, dy)` sends a whole packet in the format the mouse is in; the two
  self-tests use it.
- **After:**    CI q35 matrix 13/13; `uname` typed 8 s after the desktop runs.

### BUG-085 — the desktop loop's deadman trips on a shell command longer than 15 s: one FAILED line in every soak ⬜ OPEN
- **Status:**   OPEN · **Severity:** LOW (a log line; nothing is reset)
- **Found:**    2026-11-12 · the soak of the integration, by reading the FAILED lines of the whole serial log
  instead of the boot part: `[3g2-wire] WATCHDOG TRIPPED - main loop hung; would reset (i6300esb) FAILED`.
- **Analysis:** `watchdog::arm(1500)`: the desktop loop pets the deadman once per iteration and the scheduler
  tick checks it. A shell command runs inside one iteration; the first `churn fs 1` of a soak takes 30 to
  57 s. The line is in the serial logs of the integration's soak (2026-11-10) and of both soaks of the
  kernel debug round as well, at the same command (`proof/integ-2026-11b/watchdog-line-in-soaks.txt`): the
  markers kept of a soak were cut at the desktop.
- **Next:**     pet from the long shell commands, or check the scheduler's progress instead of the desktop loop;
  keep the verdict lines of the whole soak log in the proof.

### BUG-086 — the DHCP transaction id is a constant and a reply is not matched on it ⬜ OPEN
- **Status:**   OPEN · **Severity:** LOW to MEDIUM (a host on the same link that answers faster than the DHCP server
  sets address, gateway and DNS server; the id is what makes that a race it has to win blind)
- **Found:**    2026-11-12 · by reading, the grep round of BUG-080/081 for randomness in a security context. Not provoked.
- **Actual:**   five senders use a fixed `xid`: `main.rs` (`0x4505_5201`), `net_bringup.rs` (`0x4E55_5021`), `net.rs`
  late bring-up (`0x4C41_5445`) and `udrv.rs` (`0x4555_0001`, `0x4555_0002`). `dhcp::parse` (`crates/euronet`) does
  not return the `xid` of a reply, and `dhcp_reply` filters on the message type only: any OFFER or ACK that
  arrives is taken. BUG-081's description counted the DHCP id among the users of `net::rand_u64`; it never was.
- **Next:**     `xid` from `net::rand_u64` (the CSPRNG since BUG-081) per exchange; `DhcpInfo` carries the reply's
  `xid` and the client MAC, and a reply with another one is dropped; host tests in `euronet`. Not done here: it
  touches all four bring-up paths, and each needs its DHCP run again on every NIC driver.

### BUG-087 — the SMB client challenge is the RTC second ⬜ OPEN
- **Status:**   OPEN · **Severity:** LOW to MEDIUM (the client challenge is the client's contribution to the
  NTLM response; a server that knows it in advance can prepare its side)
- **Found:**    2026-11-12 · by reading (the same grep round). Not provoked.
- **Actual:**   `smbfs::open_share` builds the 8-byte client challenge of the session setup from `rtc::epoch()`
  (four bytes) and the constant `45 55 52 4F`; the comment says "good enough; not secret". It has to be
  unpredictable per session.
- **Next:**     `crate::entropy::draw_wait::<8>()`. Two lines; not changed in this round because the images under
  test were already built and the SMB mount self-test (`[io5]`) only runs in a dev build with a server.

### BUG-088 — the address-space slides of a Linux-ABI process are 4 to 10 bits wide ⬜ OPEN
- **Status:**   OPEN · **Severity:** MEDIUM (hardening: the layout can be guessed in a few hundred tries)
- **Found:**    2026-11-12 · by reading `aslr_slides` while moving its source to the CSPRNG (BUG-080).
- **Actual:**   `brk` 4 bits (64 KiB steps), the mmap base 5 bits (64 KiB steps), the stack top 8 bits (4 KiB
  steps), the demand pool 10 bits (2 MiB steps). The bits are now unpredictable (they came from RDRAND or
  `rdtsc ^ hpet ^ counter`), but there are few of them: the slides stay inside a 2 MiB arena, respectively
  2 GiB of the demand pool. Where the executable and the interpreter are loaded was not looked at.
- **Next:**     measure what a glibc process can take (the arena is the limit, not the generator); 28 bits for
  mmap and 22 for the stack are what Linux gives a 64-bit process.

### BUG-089 — the CSPRNG is seeded once per boot and never again ⬜ OPEN
- **Status:**   OPEN · **Severity:** MEDIUM for a virtual machine that is snapshotted or cloned, LOW otherwise
- **Found:**    2026-11-12 · by reading `entropy.rs` and `crates/euroentropy` (BUG-080).
- **Actual:**   `seed_from` runs in `entropy::init` and, for a pool that is still closed, on a later request.
  Once the pool is open nothing is added to it: no periodic reseed, no reseed after resume, no
  `vmgenid`. Two guests resumed from one snapshot continue the same HMAC-DRBG state and hand out the same
  bytes (`AT_RANDOM`, TLS key shares, sequence numbers) until they diverge by the number of requests. A
  state that leaks once (a memory dump) predicts every later output of that boot.
- **Next:**     reseed from RDSEED/RDRAND, the TPM and a jitter batch every N minutes or N requests
  (`EntropyPool::add` already reseeds an open pool; SP 800-90A allows 2^48 requests, so this is about
  recovery, not about the limit), on resume, and when the ACPI VM generation id changes.

### BUG-090 — the X server left GetPointerMapping unanswered: the browser window stayed empty ✅ FIXED
- **Status:**   FIXED (`f72cfdd`) · **Severity:** HIGH for the hosted browser (it did not start at all)
- **Found:**    2026-11-12 · first `chrome` start on the 2026-11 kernel, R630, KVM.
- **Actual:**   the browser's main thread sat in `poll()` on its first X connection for good; the window was empty
  at 90 s. The last request on that connection was opcode 117, no reply.
- **Analysis:** the default arm of `handle_request` consumed a request that owes a reply in silence. Until `e0e35ac`
  `poll(-1)` returned 0 early, xcb took that for a dead connection and chrome went on without it: that was
  the "browser connection that selects input and never polls" and the X-input start-up race.
- **Fix:**      GetPointerMapping answers; every core request that owes a reply and has no handler gets
  `BadImplementation` and a line on the serial console.
- **Proof:**    `proof/chrome-2026-11/README.md`.

### BUG-091 — `rename` over an existing file moved every later file one slot down in the file table ✅ FIXED
- **Status:**   FIXED (`c57f9bd`) · **Severity:** HIGH (silent corruption of every file opened after the replaced one)
- **Found:**    2026-11-12 · the first navigation typed in chrome's omnibox ("Aw, Snap!"); by reading, the same
  day, by the linux-rand team.
- **Actual:**   `FATAL:base/metrics/persistent_sample_map.cc:148 corrupt=1` and
  `FATAL:.../disk_data_allocator.cc:213 Likely file corruption` in the renderers; `fstrace` showed SQLite WAL
  frames written through the descriptor of `History-wal` landing in `History-shm`.
- **Analysis:** an open descriptor holds an index into `FILES`; `vfs_rename` removed an existing destination
  with `Vec::retain`. Chrome saves Preferences and the like by renaming a new file over the old one.
- **Fix:**      the destination is tombstoned like `unlink` does. A tombstoned file that no descriptor in any fd
  space holds and that has no shared mapping gives its bytes back at once (`eurofdspace::Tab::any`).
- **Proof:**    three runs of the scenario without a FATAL; `proof/chrome-2026-11/README.md`.
- **Regression tests:** `[rename]` (every self-test image: `userland/renprobe.c` as a freestanding program and
  against glibc with demand paging; on `c6f033f` 20 and 24 of its checks fail, from the first rename on);
  `scripts/rename-persist.py` (an installed system, across a restart and a power cut). The fix freed the bytes of
  a file that was still mapped: BUG-110. `proof/validate-2026-11c/README.md`.

### BUG-092 — a hosted X client got no keys, no button release, no wheel, no AltGr ✅ FIXED
- **Status:**   FIXED (`637a5e9`) · **Severity:** HIGH for the hosted browser
- **Found:**    2026-11-12 · USB-HID input over QMP into chrome on the desktop.
- **Actual:**   Ctrl+L and typing never arrived (the desktop's character loop emptied the scancode ring first);
  a click was a press and release in one call (no drag, no selection); every click also went to the page over
  DevTools at another coordinate; no wheel; `E0`-prefixed keys were decoded as plain ones; the keymap handed to
  clients was US QWERTY whatever the layout.
- **Fix:**      see the commit. The DevTools copy of input is opt-in (`chrome.input=cdp`).

### BUG-093 — no atoms, no selection owners, no MappingNotify, a fixed 1280x800 screen in the X server ✅ FIXED
- **Status:**   FIXED (`6e9ad40`) · **Severity:** MEDIUM (no copy and paste, no layout switch, no maximise in the browser)
- **Fix:**      one atom table, `SetSelectionOwner`/`GetSelectionOwner`/`ConvertSelection`, MappingNotify from
  `ps2::set_layout`, the framebuffer as the X screen, `hosted_resize` from the desktop. The DevTools bridge's
  visit list no longer navigates a plain `chrome` or `chrome URL` by itself after two and five minutes.
- **Open:**     a paste from ANOTHER X client is empty (no SelectionRequest hand-over).

### BUG-094 — an input event was dropped while its connection was being processed on another core ✅ FIXED
- **Status:**   FIXED (`c75ce22`) · **Severity:** MEDIUM (two cores only; found by reading, the counter added
  with the fix stayed 0 in the measured runs)
- **Fix:**      `lock_home()`: the delivery paths wait until every connection is back in the table.

### BUG-095 — four unread X events triggered a 400-line dump with interrupts off: typed characters lost ✅ FIXED
- **Status:**   FIXED (`d9c6811`) · **Severity:** HIGH for typing into the hosted browser
- **Actual:**   three or four consecutive characters of a typed line missing, twice in two runs.
- **Analysis:** `send_input` dumped threads, futexes, the syscall histogram and the epoll sets when 128 bytes sat
  unread on a connection (the instrument for BUG-090's dead connection). The dump was the stall.
- **Fix:**      one line, and only from 512 queued events. Also: the `[compo]` trace budget wrapped at zero and
  printed every Compositor `epoll_wait` of a session; it stops at 40.

### BUG-096 — a second copy within five seconds pasted the first text ✅ FIXED
- **Status:**   FIXED (`016d738`) · **Severity:** MEDIUM
- **Analysis:** chrome's clipboard sequence number moves on XFixesSelectionNotify only, and a paste within 5 s is
  answered from a cache keyed by it. The server had no XFIXES.
- **Fix:**      for the hosted desktop client: XFIXES QueryVersion, SelectSelectionInput and the notify event;
  PropertyNotify for property changes (the server-time round trip).

### BUG-097 — a tooltip or menu replaced the browser window in its desktop frame ✅ FIXED
- **Status:**   FIXED (`bdf9384`) · **Severity:** HIGH for the hosted browser
- **Actual:**   the frame showed "the window that painted last": a tooltip took the browser's place, a maximise
  resized the tooltip, a click released on the status bubble.
- **Fix:**      the main window with the other mapped windows drawn on it at their X position; pointer events
  to the popup under the pointer, held by the window of the press; `QueryPointer` in the events' coordinates.
- **Open:**     a popup is clipped to the browser window.
- **Regression:** the frame then also showed the windows of X clients that were gone: BUG-113, fixed.

### BUG-098 — one fd space for the browser and all its children: "Too many open files" on the third site ✅ FIXED
- **Status:**   FIXED (`6484b85` the tests, `6de8770` the fix; 2026-11-13) · **Severity:** HIGH for a browsing session
  (the site matrix does not pass in one session)
- **Fix:**      one descriptor table per process (`kernel/src/fdtab.rs`, `crates/eurofdspace`: `ProcTab`, `Refs`):
  descriptor → object with close-on-exec per descriptor, a reference count per object. One gate in front of both
  dispatchers answers close, dup, dup2, dup3, `F_DUPFD(_CLOEXEC)`, `F_GETFD/F_SETFD`, `close_range`; fork copies the
  table (every object one reference more), exec closes the close-on-exec descriptors, the last close ends a pipe
  (EOF, EPIPE, POLLHUP), a socket, an eventfd, an epoll instance; `SCM_RIGHTS` moves an object with a reference of
  its own into the receiver's lowest free descriptor; `/proc/self/fd`; `RLIMIT_NOFILE` 4096/4096; lowest number
  first for every kind of object. The overlays `FD_ALIAS`, `FORK_INHERITED`, `DEFERRED_CLOSE`, `CHILD_OPENED`,
  `FORK_CHILD_CLOSED` are gone.
- **Proof:**    `[fdsem]` (glibc with fork 49 checks, musl 25, background path 16; on Linux 6.x the same programs
  pass 49 and 24): before the fix 12/47, exit 139, 3/16; after: PASS in every boot (KVM 4 GiB and 1 GiB, TCG 1 GiB
  and 512 MiB, every TCG matrix boot that reached the desktop and the short boots of `proof/chrome-2026-11b/`). Chromium: 186 of 4096
  file objects after euro-os.eu, 214 after Wikipedia, 220 in GitHub (before: 245 and 380 of 500, then EMFILE).
- **Found with it:** BUG-120, BUG-121, BUG-122.
- **Found:**    2026-11-12 · `chrome-smoke.py --only sites`, R630.
- **Actual:**   a census of the descriptor table (a temporary addition to `cdiag`, not in the tree;
  `proof/chrome-2026-11/fd-usage-b22.txt`) counts 245 open files in the browser's fd space after euro-os.eu, 380 after
  nl.wikipedia.org, 483 during github.com; then `Creating shared memory in /tmp/.org.chromium.Chromium.* failed:
  Too many open files (24)` in a renderer and `FATAL:base/memory/shared_memory_switch.cc:111 Check failed:
  shmem_token` in the browser. About 315 of the 483 are unlinked files: shared-memory regions, one number per
  process that holds one.
- **Analysis:** the glibc group (parent and fork children) shares one fd space (BUG-060), and file, pipe and
  directory descriptors live in the numbers below 500 because the socket classes start there
  (`SOCK_FD_BASE`). Ruled out: slots pinned by the deferred close of inherited descriptors (a patch that
  frees them when the child has closed them changed nothing: 245, 380, 483).
- **Next:**     an fd space per process (fork copies it, `SCM_RIGHTS` translates between spaces), or at least
  a file range beside the socket classes. Until then: one heavy site per browser session.

### BUG-099 — the hosted X window was recomposited on the 2-second status tick: a typed character showed 1.7 s later ✅ FIXED
- **Status:**   FIXED (`15f4f3d`) · **Severity:** HIGH for the hosted browser (it felt frozen while typing)
- **Found:**    2026-11-12 · `chrome-smoke.py --only perf`: key to screen 1742 ms median over 27 keys.
- **Analysis:** the desktop loop drew the hosted window's body only in its status tick, which the idle-CPU work
  had slowed to `t/200`, or when the mouse moved.
- **Fix:**      a paint of the hosted client is its own branch of the loop. Load of the reference site to the
  final picture: 2.67 s before, 1.35 s and 1.31 s after. The key-to-screen number itself is not re-measured
  (the harness step did not register on the new image).

### BUG-110 — a file that was still mapped lost its bytes when it was unlinked or replaced ✅ FIXED
- **Status:**   FIXED (`7be30cb`) · **Severity:** HIGH (silent zeros in a mapping: a library, a font or icon cache, any
  file of 1 MiB or more that a program maps and then closes) · a regression of BUG-091's fix, never in a merged tree
- **Found:**    2026-11-12 · the `[rename]` self-test (`de92f08`), step D, glibc run with demand paging; validation of
  `feature/chrome-workplace-2`.
- **Actual:**   a file of 1 MiB is mapped (private), its descriptor closed, one page read. Then a new file is renamed
  over it, and another such file is unlinked. Every page of the two mappings first read after that was zeros
  (byte 4097).
- **Analysis:** BUG-091's fix (`c57f9bd`) frees the bytes of a file that lost its name when "nobody" can reach it: a
  descriptor in any fd space, or a shared mapping of the process that is running. A mapping that is filled page by
  page (`DEMAND_FILE_MAPS`) reads the file at every first touch and was not counted; nor were the mapping lists of the
  other processes, which are parked in `CHILD_MEM`. Before `c57f9bd` an unlinked file kept its bytes for good, so the
  unlink half is new with that commit (the rename half read a neighbour's bytes before it: BUG-091).
- **Fix:**      `file_referenced` also looks at the lazily filled mappings and at the lists in `CHILD_MEM`.
- **Proof:**    `[rename]` FAILED (2 checks) on `de92f08`, OK on `7be30cb` and `f98da76` in every boot of the round
  (`proof/validate-2026-11c/README.md`, sections 2 and 3).
- **Open:**     such a file keeps its bytes until reboot (as a file unlinked while open does): nothing frees them when
  the last mapping goes.

### BUG-111 — a rename or an unlink by a Linux-ABI program does not reach the file system on disk ⬜ OPEN
- **Status:**   OPEN · **Severity:** HIGH for a Linux program that saves a document from the Terminal (the save is gone
  after a restart, and `cat` in the shell shows the old text in the same session) · not new: the same on `c6f033f`
- **Found:**    2026-11-12 · `scripts/rename-persist.py` on an installed system (R630, KVM), validation of
  `feature/chrome-workplace-2`.
- **Actual:**   `renprobe save DIR 1` creates `a.txt`, `b.txt`, `c.txt`; the Terminal prints `[fs] … -> EuroFS synced`
  for each. `renprobe save DIR 2` renames a new file over `a.txt`, replaces `b.txt` by unlink + rename and unlinks
  `c.txt`: the program reads back version 2 and exits 0, no `[fs]` line is printed, and the shell's `cat` still shows
  version 1 of all three, before and after a restart. After the restart a Linux program does not see the three files
  at all (`renprobe check` opens none of them), although `ls` and `cat` in the shell do.
- **Analysis:** the Linux ABI works on a file table in RAM (`ring3::FILES`). After a foreground program the Terminal
  writes back the paths that were opened for writing and still exist under that name (`ring3::take_dirty`). A file
  that got its name by `rename` was opened under another name; an `unlink` is not passed on at all. And a file
  written in an earlier boot is on disk but not in the table of the next boot.
- **Not affected:** `mv`, `rm`, `cp`, `write` in the shell and the desktop's own apps: they call EuroFS directly;
  those renames survive a restart and a power cut, and `scrub` is clean (same proof, same script).
- **Next:**     `vfs_rename` carries the dirty mark to the new name; the write-back also removes what the program
  unlinked; a program's `open` falls through to EuroFS for a path the table does not know.

### BUG-112 — the GTK demo (feature `live-demos`) stops after its first frame, and often does not load ⬜ OPEN
- **Status:**   OPEN · **Severity:** LOW (the demo is in no image that is built by default; there is no GTK test in the
  boot suite) · not new: the same on `c6f033f`
- **Found:**    2026-11-12 · `gtk-live.py` and `gtk-diag.sh`, self-test image built with `--features live-demos`, R630,
  KVM; validation of `feature/chrome-workplace-2`.
- **Actual:**   (1) when the app loads, its window shows the title, `tick 0`, the entry and the button, and stays
  like that: no `GGTK: live tick` line (its one-second timer never fires), and the keys and the click of the demo
  test are delivered to its X connection and never read (`160 B of input queued unread` on the base, 704 bytes
  queued on the branch). (2) In many starts the loader fails on one library or another
  (`liblz4.so.1: failed to map segment from shared object`, `libcap.so.2: cannot read file data: Operation not
  permitted`), or the window never appears.
- **Analysis:** not done. For (2): the demo is started "alongside the desktop" while the boot still runs the self-tests
  that come after the glibc suite, and those launch other processes; the failures differ from start to start.
- **Proof:**    `proof/validate-2026-11c/gtk-demo.txt`, `screenshots/gtk-demo-*.png`.

### BUG-113 — a hosted X app came up with the windows of X clients that were gone ✅ FIXED
- **Status:**   FIXED (`f98da76`) · **Severity:** HIGH for a hosted X app that is not the first X client of the boot
  (wrong picture, keys lost) · a regression of BUG-097's fix, never in a merged tree
- **Found:**    2026-11-12 · `app run hello-x11` on the desktop of the self-test image, validation of
  `feature/chrome-workplace-2`.
- **Actual:**   the frame "Hello (window)" showed the Pango demo and the Cairo scene of the boot self-tests over the
  app's own window; the key the app waits for went to window `0xa00001` of a self-test client. On `c6f033f` the
  frame shows the app's window alone.
- **Analysis:** a client that ends without closing its socket stays in the connection table with its windows mapped
  (all six X clients of the boot self-tests; an app closed with the close button of its frame). `compose_windowed`
  (BUG-097) draws every mapped window of every connection, and a key goes to the focus window or else to the window
  presented last.
- **Fix:**      `xserver::forget_clients()`: the desktop empties the connection table and what points into it right
  before it starts a hosted app (the browser, `app run` of an x11 app, the GTK demo). At most one hosted app runs.
- **Proof:**    `proof/validate-2026-11c/README.md`, section 3; `x11-app-*.txt`, `screenshots/x11-app-*.png`.
- **Open:**     the connections are still not closed when their process ends; they are dropped at the next start.

### BUG-114 — a hosted app that ended by itself leaves its frame, and the next start is refused ⬜ OPEN
- **Status:**   OPEN · **Severity:** LOW
- **Found:**    2026-11-12 · `hello-x11` on the desktop, validation of `feature/chrome-workplace-2` (`f98da76`).
- **Actual:**   the app gets its key, destroys its window and ends. The frame stays with the last picture, and
  `app run hello-x11` answers "a hosted app is already running (close its window first)" until the frame is closed
  with its close button. Also: the first start of a boot is resized to its frame (800x600), a later start keeps its
  own 480x240 and is centred.
- **Analysis:** not done. Whether the base does the same was not determined (there is no X trace on the base to tell
  whether the key arrived).

### BUG-115 — with virtio input the first-run tour sometimes ignores Esc ⬜ OPEN
- **Status:**   OPEN · **Severity:** LOW (the tour then closes on the next click or key) · not new: the same on `c6f033f`
- **Found:**    2026-11-13 · `gui-smoke.py --input virtio`, monkey soak of the public image (R630, KVM), validation of
  `feature/chrome-workplace-2`: "first-run tour: shown on the first desktop, Esc closes it: no [tour] closed line".
- **Actual:**   `[tour] first-run tour shown`, then `[vinput] Key { code: 1, pressed: true }` and its release: the Esc
  reaches the kernel, the tour stays; the next click closes it ("closed by a click … outside the card"). Smoke pass
  only, virtio input, five times each: the base failed 2 of 5, `f98da76` 0 of 5; `f98da76` failed it once in the
  virtio monkey soak (`proof/validate-2026-11c/tour-virtio.txt`). Not seen with USB input.
- **Next:**     where the desktop loop takes the Esc of a virtio keyboard while the tour is up.

### BUG-120 — a foreground program after a glibc program got that program's brk region ✅ FIXED
- **Status:**   FIXED (`75f254f`) · **Severity:** MEDIUM (a musl program started from the shell after the browser
  died in its first larger allocation)
- **Actual:**   `[isolation] foreground exec page fault at 0xe282000 -> clean exit (code 139)`, in opendir's calloc.
- **Cause:**    `run_args` did not reset `BRK_CUR`/`BRK_END`: brk(0) answered an address in an arena that was gone.
- **Fix:**      an empty brk region at every foreground start; musl then takes its memory from mmap.

### BUG-121 — a foreground musl program that waits in poll() on a pipe faults at its next instruction ⬜ OPEN
- **Status:**   OPEN · **Severity:** LOW (seen once, on the tree before BUG-098's fix)
- **Actual:**   the musl half of `[fdsem]` ended after its sixth check with `[isolation] foreground exec page fault
  at 0x208df0`; the program was in poll() with a 50 ms timeout on a pipe end that was not ready.
- **Note:**     with the descriptor tables the same poll answers at once (POLLHUP), so the path is no longer taken by
  the test. A foreground wait that times out has not been looked at.

### BUG-122 — every Chromium child aborted: "Crashing due to FD ownership violation" ✅ FIXED
- **Status:**   FIXED (`6de8770`) · **Severity:** HIGH (with per-process tables no child process ran)
- **Cause:**    a child told `--enable-crash-reporter` takes descriptor 3 as its crash-handler socket and wraps it in a
  ScopedFD whether the browser handed one over or not. Our browser runs without a handler and hands nothing over;
  with one table for all processes no child ever got a number that low. Seen in the child's own descriptor events
  (`[fdx]`): nothing at 3 after exec, then `new 3` from open, abort.
- **Fix:**      the child's exec gives it descriptor 3 (`/dev/null`) when the switch is there and 3 is free. No crash
  handler runs, as before.

### BUG-123 — the serial console cost two exits to the host per byte ✅ FIXED
- **Status:**   FIXED (`a7d6f62`) · **Severity:** HIGH under a hypervisor with a talkative log (BUG-020, BUG-126)
- **Fix:**      with a 16550A FIFO (found at init) the UART is written in loads of 16 bytes after one status read
  (`rep outsb`).

### BUG-124 — an NMI that landed in ring 3 livelocked the machine ✅ FIXED
- **Status:**   FIXED (`a7d6f62`) · **Severity:** HIGH for diagnosis (the probe made the hang it looked at)
- **Actual:**   runs g3, g4: `[demand] the same page faults again and again: … 262144 times in a row, write=false
  present=true`, every vruntime frozen.
- **Cause:**    the NMI probe read the interrupted code bytes and stack directly. In ring 3 that is user memory: with
  SMAP a page fault inside the NMI handler, which the demand-paging handler "handled" by mapping the same present
  page again, for ever, with interrupts off.
- **Fix:**      user code is read through the page walk, a user stack is not scanned; a present, non-write fault that
  repeats 64 times is no longer handled again (the line now gives the error code, rip and page walk).

### BUG-125 — `gui-smoke.py`: the serial reader copied the whole log for every chunk ✅ FIXED
- **Status:**   FIXED (`a7d6f62`) · **Severity:** HIGH for every long run (the harness made part of the stall it measured)
- **Cause:**    `self.text += chunk` on one growing string; late in a session megabytes per 16 bytes. The reader fell
  behind, the socket filled, the guest waited in its UART writes.
- **Fix:**      chunks in a list with absolute positions; `since(pos)` joins only what lies after `pos`.

### BUG-126 — the desktop loop stood for up to 18 s while GitHub loaded: per-event lines on the UART ✅ FIXED
- **Status:**   FIXED (`a7d6f62`) · **Severity:** HIGH (W22: the desktop stalled 12-17 s)
- **Actual:**   run g8: slowest console answer 18 117 ms in GitHub; every NMI probe found task 0 preempted in
  `serial::__print`; a GitHub load wrote ~1 MB of `[linux-abi]`, `[cst]`, `[cdp] <-`, `[scm]`, … lines.
- **Fix:**      from the start of the desktop loop these per-event tags go to the kernel log only (`dmesg`); boot
  option `serial.chatter=on` and the shell command `chatter on|off` bring them back.

### BUG-127 — the desktop loop halted with other tasks ready, then waited a round of all of them ✅ FIXED
- **Status:**   FIXED (`a7d6f62`) · **Severity:** HIGH (30 ready threads: 300 ms per pass)
- **Fix:**      the loop yields when others are ready; a timer tick that ends another task's turn gives the CPU to the
  loop first when it asked for more (not when it halted idle).

### BUG-128 — fork and the child's exec ran 90-340 ms with interrupts off ✅ FIXED
- **Status:**   FIXED (`a7d6f62`, smaller arena in `3c1892c`) · **Severity:** HIGH (no timer tick, no desktop)
- **Cause:**    the 256 MiB arena copy, the copy of every demand page, and the 256 MiB wipe at exec were each one piece
  (`[tick-late] +9..+12 ticks`).
- **Fix:**      2 MiB pieces (demand pages: per four page tables) with a window for the timer between pieces; `[fork]`
  and `[execve]` say how long each part took. Remaining late ticks during a site matrix: at most +5 (50 ms), in
  `do_child_execve`.

### BUG-129 — the desktop loop waited for the UART for its own lines ✅ FIXED
- **Status:**   FIXED (`a7d6f62`) · **Severity:** HIGH (one pass of 570 ms in the DevTools pump, run g12)
- **Fix:**      the loop's lines go into a 256 KiB queue; it sends up to 256 bytes at the end of a pass, a kernel task
  sends the rest with interrupts on between loads. Panics, faults, the NMI probe and `write_raw` send the queue first.
  (A variant that ran the sender right after every pass made things worse, g15/g16, and was not kept.)

### BUG-130 — TLS connections whose first two segments were never acknowledged ✅ FIXED
- **Status:**   FIXED (`2374531`) · **Severity:** HIGH (sites that do not load: `net::ERR_CONNECTION_CLOSED`)
- **Actual:**   `[conn] … closed …: rx 0 B, tx 1772 B, open=false, unacked=2`, 4 to 23 times per run (s3 onwards).
- **Cause:**    virtio-net sent every frame from one buffer through one descriptor and went on after a bounded wait
  whether the device had taken it or not. QEMU transmits from a bottom half: the next send wrote over the frame.
  A ClientHello in two segments reached the network as the second one twice; every retransmission round did the same.
- **Fix:**      32 transmit buffers, one per descriptor, reused only after the device gave them back.
- **Proof:**    0 of 83 closed connections with unacknowledged data (a4, a8, a16), all six sites at 4, 8, 16 GiB.

### BUG-131 — the demand region never gave addresses back; anonymous mappings under 16 MiB came from the arena ✅ FIXED
- **Status:**   FIXED (`3c1892c`) · **Severity:** HIGH (YouTube: `V8 process OOM (Zone)`)
- **Actual:**   `[mm-fail] … mmap(len 512 KiB, PROT_RW, anon) = ENOMEM | arena mmap window 468 KiB left of 253 MiB |
  demand addresses 403 of 480 GiB handed out`.
- **Fix:**      anonymous mappings from 64 KiB go to the demand region; munmap gives addresses back to a per-process list
  (`euromm::FreeRanges`, host-tested against a page-bitmap model), an anonymous mmap takes them from there first.
- **Proof:**    no `V8 OOM` in any run since; y4: 128 of 480 GiB handed out, 16 GiB given back, 11.6 GiB reused.

### BUG-132 — RAM files without a name were never freed while a mapping had ever existed ✅ FIXED
- **Status:**   FIXED (`3c1892c`) · **Severity:** HIGH (kernel heap panic in GitHub: "memory allocation of 1105728
  bytes failed")
- **Actual:**   `ramfiles`: 633 MiB in 542 files without a name (Chromium's shared memory, 2 MiB each).
- **Cause:**    a shared mapping counted as a reference for ever (the file's own frame list was taken for one); only
  the calling process's mappings were looked at; nothing ran at the last close or unmap.
- **Fix:**      a sweep (desktop loop, interrupts off) frees bytes and shared frames of every nameless file that no
  descriptor in any space and no mapping in any process names; due after a close of such a file, an munmap of a
  shared window, a process exit. A RAM file may not grow into the last 32 MiB of kernel memory (ENOSPC).
- **Proof:**    RAM files after GitHub 95-122 MiB (was 633-748); no heap panic since.
- **Open:**     the bytes of RAM files still live in the kernel heap (≤ 512 MiB at any guest size); a file that is
  only mapped is held twice (heap bytes and frames). Frames per file with a guest-RAM-dependent limit is the next step.

### BUG-133 — every Chromium process carried a 256 MiB arena ✅ FIXED
- **Status:**   FIXED (`3c1892c`) · **Severity:** HIGH on 4 GiB (`[fork] arena alloc FAILED (256 MiB, pool has 131 MiB)`)
- **Fix:**      128 MiB (the browser uses 80-86 MiB of it, `[fork]` line); fork copy 49-116 ms, exec wipe 50-101 ms.

### BUG-134 — after the PDF viewer a new renderer aborts at its start ✅ FIXED (BUG-140)
- **Status:**   FIXED (`7b0629c`, W23) · **Severity:** HIGH for criterion 1 (the second round of the site matrix failed)
- **Actual:**   `[abort] tN "CrRendererMain" tgkill(tgid=1, tid=N, sig=6)`, a 20-frame stack without symbols, child exit 134,
  "Aw, Snap! Error code 34304". Runs a8, d1, d2, the browser soak; reproduced in W23 with three sites only (b1:
  euro-os, Wikipedia, pdf, five rounds: four aborts from round 3 on).
- **Cause:**    BUG-140. The scheduler table was full: every run that aborted reached task numbers t250 to t255. The
  message of the abort went to the kernel log only (a `[linux-abi]` line); the same CHECK in an older log reads
  `pthread_create: Resource temporarily unavailable (11)` then `Check failed: io_thread_->StartWithOptions(...)`.
  The PDF viewer only made it come sooner (more processes, each with its threads).
- **Proof:**    `proof/chrome-w23/README.md`: no abort in any W23 run on the fixed images (a1, a2: five rounds of the three
  sites; m1, m3: the full matrix three times); the scheduler census after every site says "0 held by ended tasks".

### BUG-135 — typed URLs do not navigate any more (after BUG-134, once after GitHub) ✅ FIXED (BUG-140)
- **Status:**   FIXED (`7b0629c`, W23) for "does not navigate"; the lost first characters are BUG-145
- **Cause:**    BUG-140: with the table full the browser could not start the threads and processes a navigation
  needs. b1 (the tree before): from round 3 every typed URL stayed on its page (7 steps); a2 (fixed): 15 of 15
  navigations in five rounds; m1 and m3: 18 of 18 in three rounds of the full matrix.
- **Not the same bug:** a URL that arrives without its first characters (a8: `tps://www.openstreetmap.org/`, W23 m1:
  `s://www.openstreetmap.org/`) is BUG-145.

### BUG-136 — a TCG boot (smp 4, 512 MiB) wedged on the scheduler lock in a page fault ✅ FIXED (causes removed, not reproduced)
- **Status:**   FIXED (`7b0629c`, W23), with BUG-142, BUG-143 and BUG-144 · **Severity:** MEDIUM
- **Reproduction:** not reproduced: 0 wedges in 29 boots of the tree before (`fix0b`: 40f853a plus the tests;
  user-smp4-iommu, user-smp4-noiommu and kernel-smp4 at 512 MiB, 21 on the R630 and 8 on the build server), against 1 in 10
  in chrome-2026-11b.
- **What its trace shows, and what changed:**
  - It wedged right after `gsdl` started in a 64 MiB fallback arena. That arena had its mmap window past its end
    and its brk region over its stack (BUG-142): gsdl crashed in every 512 MiB boot (exit 139 in all 29
    boots above). Now gsdl runs there: `GSDL: SDL_Init ok`, `window created`, `exit=0`.
  - A page fault taken with the scheduler lock held waits for that lock in the fault path (`sched::current()`
    in the repeated-fault line, BUG-144); CR2 was 0x1000. The one read the scheduler made under its lock through
    an address it had not made itself (the byte before the interrupted rip, to tell idle from work) is gone
    (BUG-143).
  - The probe now gets to its stack scan and census (it stopped in its own `sched::current()`).
- **Proof:**    `proof/chrome-w23/README.md` (after: the TCG matrix and further 512 MiB smp 4 boots, no wedge).

### BUG-137 — the demand pool drains during a long browser session ✅ FIXED
- **Status:**   FIXED (`48bf2e7`, `b882f4d`, `15fe72b`; cause and proof: BUG-150, BUG-151, BUG-152, BUG-155 and
  `proof/chrome-mem/`) · **Severity:** HIGH for criterion 4 (the browser is killed after ~74 min)
- **Cause:**    not the page cache (212-235 MiB, flat). Forks that were refused because the task table was used up by
  slots of ended tasks (BUG-151), each keeping 128 MiB of arena and ~105 MiB of demand pages (BUG-150); the browser
  retried until the pool was empty, and the fault that then failed ended its main thread (BUG-155).
- **Actual:**   two-hour soak (`proof/chrome-2026-11b/`, 8 GiB): demand pool 2 834 MiB free at the start, 1 059 after
  round 1, 415 after round 2, 62 after round 3, 0 in round 4; then `[isolation] ring-3 page fault … (task 1)
  TERMINATED`: the browser's main thread faulted on a page the pool could not give. The guest kept running.
- **Candidates:** the disk page cache (`DISK_PAGE_CACHE`, never evicted); shared frames of named RAM files. Not
  attributed.
- **Next:**     a census of demand frames by owner (process pages, shared frames, page cache) after every round.

### BUG-140 — the scheduler slot of every task of an ended fork child stayed taken ✅ FIXED
- **Status:**   FIXED (`7b0629c`, test `bb2e7d8`) · **Severity:** HIGH (the cause of BUG-134 and BUG-135)
- **Found:**    2026-11-13 · W23, the task numbers in the logs of BUG-134 (t250 to t255) and of an older run (h1:
  `pthread_create: Resource temporarily unavailable (11)` after task 255).
- **Actual:**   only a worker thread that ended itself with exit(2) offered its slot again (RECLAIMABLE). The main task
  of a fork child and every thread still waiting when the child ended (exit_group by its main or by a worker,
  kill, abort, the end of the run) were marked Dead and kept their slot until the browser ended.
- **Test:**     `[churn-forkthreads]` (`userland/glibc/gslots.c`): 40 children with 6 threads each, ended the three ways a
  Chromium child ends. Before: `32 of 40 rounds ... pthread_create 11 ... task table 39->256 -> FAILED`. After:
  `40 of 40 rounds ... PASS (exit 88) · task table 34->34 -> OK` (KVM, 4 GiB).
- **Fix:**      `retire_task()`: kernel stack, futex entries, the registries that name a task by number (GLIBC_THREADS,
  GLIBC_CTIDS, THREAD_NAMES), then RECLAIMABLE; in the exit_group of the child itself and in `fork_child_teardown`
  (kill, end of run). The reuse hook clears capabilities, syscall profile and the RECLAIMABLE mark of the old
  occupant. `fds` prints the table: "scheduler: N of 256 task slots handed out, L live, R free for reuse, H held
  by ended tasks".
- **Open:**     BUG-147 (live tasks come close to 256 in a long session).
- **Integration:** `feature/chrome-integ` merged this with the same finding of the other team (BUG-140 = BUG-151, BUG-141 = BUG-150, BUG-147 = the second half of BUG-151): one `retire_task` (Dead, kernel stack, futex entries, the registries GLIBC_THREADS/GLIBC_CTIDS/THREAD_NAMES, RECLAIMABLE), one undo path for a refused fork (`undo_fork`, with the `[spawn-refused]` reason), 512 task slots and fd spaces, 480 thread kernel stacks (224 static, the rest from the heap on first use). `proof/chrome-integ/`.

### BUG-141 — a fork that failed late kept the arena of the child ✅ FIXED
- **Status:**   FIXED (`7b0629c`) · **Severity:** MEDIUM (each refused fork kept 128 MiB once the table was full)
- **Cause:**    `do_glibc_fork` returned EAGAIN or ENOMEM after the arena, page tables and demand pages of the child
  existed (no kernel stack, no scheduler slot, no memory for its demand pages) without giving them back; the
  page-table failure path freed one frame of the arena.
- **Fix:**      `fork_undo()`: what the exit of the child would have given back.
- **Integration:** `feature/chrome-integ` merged this with the same finding of the other team (BUG-140 = BUG-151, BUG-141 = BUG-150, BUG-147 = the second half of BUG-151): one `retire_task` (Dead, kernel stack, futex entries, the registries GLIBC_THREADS/GLIBC_CTIDS/THREAD_NAMES, RECLAIMABLE), one undo path for a refused fork (`undo_fork`, with the `[spawn-refused]` reason), 512 task slots and fd spaces, 480 thread kernel stacks (224 static, the rest from the heap on first use). `proof/chrome-integ/`.

### BUG-142 — a 64 MiB glibc arena had its mmap window past its end ✅ FIXED
- **Status:**   FIXED (`7b0629c`, test `bb2e7d8`) · **Severity:** HIGH on 512 MiB machines (gsdl crashed in every boot;
  see BUG-136)
- **Actual:**   `[glibc] arena: 256 MiB unavailable (fragmented) -> using 64 MiB`, then `mmap anon 8 KiB -> ENOMEM:
  arena mmap window full (break 0xa380000, end 0x9f76000)`, `gsdl (SDL2): exit=139`.
- **Cause:**    the four launchers place brk at +32 MiB and the mmap window at +64 MiB whatever the size of the arena.
- **Test:**     `cargo test -p euromm arena` (every size from 64 MiB: every region inside the arena): FAILED on the layout
  before, OK after.
- **Fix:**      `euromm::arena::glibc_layout`: from 128 MiB the old layout; smaller arenas the same order in the same
  proportions (64 MiB: brk from +16, mmap window from +24 MiB, about 35 MiB). Now `GSDL: SDL_Init ok`,
  `window created`, `gsdl (SDL2): exit=0` at 512 MiB.

### BUG-143 — the scheduler took the busy desktop loop for idle: one pass of 25-57 s under TCG ✅ FIXED
- **Status:**   FIXED (`7b0629c`) · **Severity:** HIGH under TCG (`[3g2-wire] WATCHDOG TRIPPED` in a third of the R630 TCG
  boots of the self-test image), and the desktop lost turns on any machine
- **Actual:**   `[desk] #12 the desktop loop was away 30715 ms ...` while it drew the status panel, the sidebar or a
  window.
- **Found how:** new in the stopwatch: the longest named step of a pass, and for a pass over a second where its ticks
  went: "ticks of the loop at rip ... (soft-float `__addsf3`, `drop_shadow`) x8; ticks of other tasks: t7..t11
  x517 each; the loop meanwhile: Ready x2649".
- **Cause:**    a tick was idle when the byte before the interrupted rip was 0xf4 (hlt). 0xf4 is also -12 as a
  displacement or an immediate, common in the soft-float code the loop draws with: the busy loop counted as idle,
  lost T0_WANTS (its turn at every tick, BUG-127), and five CPU-bound self-test processes had the CPU.
- **Fix:**      the loop says when it is idle (`desktop_idle_hlt`, T0_IDLE around its hlt).
- **Proof:**    before: 22 trips in 81 TCG boots of images with the old test on the R630 (kernel-smp1: 14 in 48), 3 in 8 on the build server; after: 0 trips in 35 TCG boots (12 kernel-smp1, the matrix of 10, 13 at 512 MiB smp 4)
  with a longest gap of 2.0-4.0 s (`proof/chrome-w23/`).

### BUG-144 — the NMI probe and the repeated-fault line waited for the scheduler lock ✅ FIXED
- **Status:**   FIXED (`7b0629c`) · **Severity:** MEDIUM for diagnosis (the probe of BUG-136 stopped in itself)
- **Fix:**      `current_lockfree()` and a try-lock for the thread names in the probe and in `handle_demand_fault`.

### BUG-145 — the first characters of a typed URL go to the page when its renderer is busy ⬜ OPEN (harness)
- **Status:**   OPEN · **Severity:** LOW for the kernel; the matrix judges such a step "ok" (the URL lands in a search)
- **Actual:**   after YouTube, Ctrl+L and `https://www.openstreetmap.org/` typed at once became a Google search for
  `s://www.openstreetmap.org/` (W23 m1, round 1; a8: `tps://...`).
- **Analysis:** the browser hands Ctrl+L to the page first and focuses the omnibox when the renderer answers "not
  handled"; characters typed before that answer go to the page. A busy renderer answers late.
- **Next (harness, `feature/chrome-harness`):** after Ctrl+L wait until the omnibox shows its selection (a screendump of
  the omnibox) before typing, and judge a site step on the URL the tab shows, not on the host appearing in it.

### BUG-146 — key to screen 300-500 ms ⬜ OPEN
- **Status:**   OPEN · **Severity:** MEDIUM (criterion 5)
- **Measured (kernel, `[keylat]`, 30 keys per run):** scancode ring to X client 0.1-2 ms, X queue to client read 4 ms,
  read to the first draw of the browser 74-156 ms, draw to screen 9-25 ms. The harness also counts the time to
  the draw that shows the character, the USB path and its own screendumps: 369-483 ms median.
- **Tried:** a wake hand-off (a waking system call yields): woken tasks then ran within 0.1 ms, the harness median went
  from 483 to 1102 ms; off by default (`wakeyield on|off`). Kept: the desktop loop goes next after the hosted
  window drew.
- **Next:** the browser runs on one CPU (ring 3 only on the BSP), with thirty ready threads and 10 ms slices.

### BUG-147 — live tasks come close to the 256 slots in one session ✅ FIXED
- **Status:**   FIXED (`95a3bf2`, test `e5990d5`) · **Severity:** MEDIUM
- **Measured:** after BUG-140 no slot is held by an ended task, but the browser keeps up to 16 processes (back/forward
  cache, the PDF viewer): 214 of 256 live tasks (a1); thread kernel stacks: 224.
- **Test:**     `[many-threads]` (`userland/glibc/gmanythreads.c`, 320 threads of one process alive at once): before `223 of 320
  threads alive at once, pthread_create: Resource temporarily unavailable -> FAILED`, after `320 of 320 -> PASS`.
- **Fix:**      512 task slots; thread kernel stacks 224 static, 224..480 from the kernel heap on first use (kept per slot).
  Static memory +4 MiB (the 16 KiB kernel-task stack per slot). A session of five rounds: 210 live of 512.
- **Integration:** `feature/chrome-integ` merged this with the same finding of the other team (BUG-140 = BUG-151, BUG-141 = BUG-150, BUG-147 = the second half of BUG-151): one `retire_task` (Dead, kernel stack, futex entries, the registries GLIBC_THREADS/GLIBC_CTIDS/THREAD_NAMES, RECLAIMABLE), one undo path for a refused fork (`undo_fork`, with the `[spawn-refused]` reason), 512 task slots and fd spaces, 480 thread kernel stacks (224 static, the rest from the heap on first use). `proof/chrome-integ/`.

### BUG-160 — Chromium's audio output fails: every clip with sound stops at t=0 (`AUDIO_RENDERER_ERROR`) ✅ FIXED (BUG-170, BUG-171)
- **Status:**   FIXED (`d4d06ed`, test `83d87de`) · **Severity:** HIGH for criterion 3
- **Cause:**    BUG-170 (a new socket inherited O_NONBLOCK from a closed one with the same object number: the renderer's blocking
  read of the audio sync socket answered EAGAIN, the renderer closed its end, the audio service's write was EPIPE), then
  BUG-171 (the blocking read, once it blocked, spun and stopped the machine).
- **Proof:**    `chrome-smoke.py --only localvideo` (KVM, `proof/chrome-w23b/`): VP9+Opus `currentTime` 0.0 -> 61.2 s, 12 of 12 five-second
  windows of the QEMU wav above the noise floor (RMS 1158), no media error; Opus alone the same; before: 0.0 s, RMS 0, error 3.
  H.264/AAC still fails: BUG-161 (the build has no such codecs).

### BUG-161 — this Chromium build has no H.264/AAC ⬜ OPEN (build, not kernel)
- **Actual:**   `canPlayType('video/mp4; codecs="avc1.42E01E, mp4a.40.2"')` is empty, an `.mp4` fails with `DEMUXER_ERROR_NO_SUPPORTED_STREAMS` (`error 4`). VP9/Opus/VP8 are supported.
  YouTube serves VP9/AV1 + Opus to such a browser, so it does not block criterion 3; sites that only offer H.264 will not play.
- **Next:**     decide whether `proprietary_codecs`/`ffmpeg_branding=Chrome` is wanted (licence); otherwise document it.

### BUG-162 — a click on a page link is sometimes not followed ✅ NOT A EUROOS BUG (harness timing)
- **Status:**   closed for the kernel · the harness change is described below
- **Repeated:** 10 fresh boots × 3 clicks on the Download link of euro-os.eu/en/ at the coordinate of `chrome-smoke --only interact`
  (`proof/chrome-w23b/link-clicks.txt`): 27 followed (0.7-1.1 s), 1 not, and the 2 after it in that boot clicked on the page the
  test had gone back to. Every click reached the X client (press and release, `[xin]`).
- **Cause:**    the click that was not followed came while euro-os.eu was still loading: the screendump before the click shows the
  previous page (`input-test.html`) under the new tab title, with the stop button; 15 s later the page is there and the
  pointer is over its Download button (screenshots in the proof). The harness waits for 1.5 s of unchanged picture, which a
  page that has not painted yet also gives.
- **Next (harness):** wait for the load to finish (`wait_loaded`, as the site matrix does) and for the target's pixels before
  clicking a link.

### BUG-170 — a new socket could be born non-blocking: Chromium's audio sync socket ✅ FIXED
- **Status:**   FIXED (`d4d06ed`, test `83d87de`) · **Severity:** HIGH (BUG-160)
- **Actual:**   `[audio-sys] ... 72(fd, F_GETFL) = 0x802` then `1(fd, 4 bytes) = -32` on the audio service's end of a fresh
  socketpair; in `[syncsock]`: "a new socketpair is blocking (after a closed non-blocking one)" FAILED (0x802), the child's
  blocking recvmsg EAGAIN.
- **Cause:**    O_NONBLOCK is kept per object number (SOCK_NONBLOCK, FD_NONBLOCK) and was never removed when the object closed;
  socketpair() set it only when asked. Object numbers are handed out again.
- **Fix:**      `nonblock_forget()` at the last close of an object; socketpair() sets the flag either way.
- **Test:**     `[syncsock]` (`userland/glibc/gsyncsock.c`): 9 of 17 FAILED -> 17 of 17 OK.

### BUG-171 — a blocking read on a unix socket spun with interrupts off ✅ FIXED
- **Status:**   FIXED (`d4d06ed`) · **Severity:** HIGH (the machine stopped answering a few seconds into the first clip once BUG-170 was
  fixed: KVM run v1, every NMI in the renderer's `AudioOutputDevice` thread inside `unix_recv_blocking` / `yield_switch`)
- **Cause:**    the wait was a loop of yields; a yield charges no vruntime, so the waiter was picked again at once.
- **Fix:**      two quick yields, then a tick of sleep per look, as poll does. Runs v2, v3, v4: 60 s of sound per clip, the desktop
  answering throughout.

### BUG-163 — YouTube answers "Sign in to confirm you're not a bot" to every public video (not a EuroOS bug, recorded) ⬜ OPEN
- **Actual:**   with the consent dialog answered and verified gone (`proof/chrome-harness/`), four public videos all show the bot page in the player; no login or workaround was tried. Criterion 3 (2 minutes of YouTube with sound) cannot be measured from this network; `--only localvideo` measures the same stack without YouTube (BUG-160).

### BUG-150 — a fork that was refused part-way kept everything it had taken ✅ FIXED
- **Status:**   FIXED (`48bf2e7`) · **Severity:** HIGH (this is where the demand pool of BUG-137 went)
- **Actual:**   `do_glibc_fork` returned from four places (no thread kernel stack, no task slot, no frame for the copy of
  the demand pages, no page table) with the child's arena (128 MiB for the browser), its page tables and a copy of every
  committed demand page (~105 MiB) still allocated and owned by nobody. Two of the four said nothing. The census
  (`[memstat] … unattributed`) showed it: 0 KiB, then 420 860 KiB after four refused forks, 659 300 KiB a minute later
  (run b8 of `proof/chrome-mem/`); a browser retries.
- **Fix:**      one undo path in `do_glibc_fork` gives it all back, as the exit of a child does, and says so
  (`[fork] refused (…): N MiB of demand pages and M MiB of arena and tables given back`); every refusal names its
  reason (`[spawn-refused]`).
- **Proof:**    `[memt] forkfail`: process pool 1 708 032 → 528 336 kB free over three refused forks and still so after
  the run (before); 0 kB lost (after). Browser: `unattributed=0 KiB` in every census of the proof runs.
- **Integration:** `feature/chrome-integ` merged this with the same finding of the other team (BUG-140 = BUG-151, BUG-141 = BUG-150, BUG-147 = the second half of BUG-151): one `retire_task` (Dead, kernel stack, futex entries, the registries GLIBC_THREADS/GLIBC_CTIDS/THREAD_NAMES, RECLAIMABLE), one undo path for a refused fork (`undo_fork`, with the `[spawn-refused]` reason), 512 task slots and fd spaces, 480 thread kernel stacks (224 static, the rest from the heap on first use). `proof/chrome-integ/`.

### BUG-151 — "the task table is full" with 135 live tasks; no room for the threads of 14 browser processes ✅ FIXED
- **Status:**   FIXED (`48bf2e7`) · **Severity:** HIGH (no thread, no process: the root of BUG-134, BUG-135 and BUG-137)
- **Actual:**   `[spawn-refused] fork: the task table is full (thread kernel stacks 131/224, tasks 135/256)` (run c8).
  The main task of every fork child, and every thread of a child that was killed or whose process exited under it,
  stayed Dead and never offered its slot; only a thread that ended by itself did. After some tens of ended processes
  the table of 256 was used up. With that fixed, a browser with 14 processes had 224 live tasks and all but 4 of the
  224 thread kernel stacks in use (run d8), and its next fork was refused for want of a stack.
- **Fix:**      `retire_task` (Dead and reclaimable) in `fork_child_teardown` and in the `exit_group` of a child; the
  task table is 512 (fd spaces with it) and there are 448 thread kernel stacks, the ones past the first 224 taken from
  the kernel heap the first time they are used.
- **Proof:**    `[memt] forkmany`: 270 children one after the other, every second one ended by SIGTERM, each reaped
  with its status. Browser: no `[spawn-refused]` line in the proof runs; peak tasks and stacks in the README.
- **Note:**     this is what BUG-134 (a renderer aborts at its start: its thread could not be made) and BUG-135 (typed
  URLs do not navigate: no renderer could be forked) looked like from outside in runs b8, c8 and c4. Whether every
  occurrence of those two has this cause is for their owners to say; the site matrix after the PDF now passes.
- **Integration:** `feature/chrome-integ` merged this with the same finding of the other team (BUG-140 = BUG-151, BUG-141 = BUG-150, BUG-147 = the second half of BUG-151): one `retire_task` (Dead, kernel stack, futex entries, the registries GLIBC_THREADS/GLIBC_CTIDS/THREAD_NAMES, RECLAIMABLE), one undo path for a refused fork (`undo_fork`, with the `[spawn-refused]` reason), 512 task slots and fd spaces, 480 thread kernel stacks (224 static, the rest from the heap on first use). `proof/chrome-integ/`.

### BUG-152 — madvise(MADV_DONTNEED) zeroed the pages and kept every frame ✅ FIXED
- **Status:**   FIXED (`b882f4d`) · **Severity:** HIGH for a long session (memory in use only grew)
- **Actual:**   PartitionAlloc and V8 give memory back with `madvise(MADV_DONTNEED/MADV_FREE)` and nothing else; the
  call zeroed the pages in place and kept them committed. The census counted 25-40% of the anonymous pages of the
  browser's processes as all zeros (`zero~`).
- **Fix:**      `paging::discard_demand_range`: unmap and free the pages the process owns alone; shared windows,
  page-cache mappings and private file pages stay; a swapped-out page loses its slot. The next touch faults in zeros.
- **Proof:**    `[memt] madvise`: 32 MiB touched and advised away came back in full (before: 0 kB of 32 768). Browser:
  `zero~` 2-3% of the anonymous pages.

### BUG-153 — the page cache of the packs only grew ✅ FIXED
- **Status:**   FIXED (`15fe72b`) · **Severity:** MEDIUM (212-235 MiB in a session; it is what can be given back first)
- **Fix:**      `euromm::PageCache` (recency, write state, eviction order; host-tested against a brute-force model) and
  `pcache_reclaim`: under 4 MiB free, clean pages go, unmapped first, then mapped ones no process referenced since the
  last pass, oldest first; mapped victims are unmapped from every process. See `design/MEMORY-RECLAIM.md` §4.
- **Proof:**    host tests; in a 4 GiB guest under pressure the `[pcache]` lines of the README.

### BUG-154 — the bytes of memory files lived in the kernel heap, twice when mapped; a memfd was never freed ✅ FIXED
- **Status:**   FIXED (`f9a8870`) · **Severity:** HIGH (the open half of BUG-132)
- **Actual:**   a memfd or an unlinked shared-memory file was a byte vector in the kernel heap (≤ 512 MiB at any guest
  size) plus one frame per touched page of its mappings; a mapping of more than 4 MiB never saw what `write()` wrote,
  nor `pread` what the mapping stored; and a memfd kept its made-up path, so the sweep of files without a name never
  took it: every memfd a program made stayed until the run ended.
- **Fix:**      a file without a name is a `euromm::ramfile::PageFile` (frames, 0 for a hole): one memory for read,
  write, ftruncate and every mapping. Budget from the pool: half of it, never its last sixteenth; ENOSPC at the limit.
  A memfd has no name from its creation. `design/MEMORY-RECLAIM.md` §3.
- **Proof:**    `[memt] memfd` 12/12: 16 MiB written to a memfd grew the kernel heap by 0 kB (before: 16 MiB) and
  mapping it took no further frame (before: 16 MiB more); a store through one mapping is read by `pread` and by a
  second mapping; ftruncate to 4 MiB gave 12 MiB back; ENOSPC at the limit of a 96 MiB pool and 4 MiB of anonymous
  memory still granted. Browser: `ramfile-bytes` in the kernel heap and `shm` frames in the README.

### BUG-155 — out of memory ended whoever faulted, the browser included; in a system call it stopped the kernel ✅ FIXED
- **Status:**   FIXED (`15fe72b`) · **Severity:** HIGH
- **Actual:**   a page fault that found the pool empty failed: `[isolation] … (task 1) TERMINATED` for the browser's
  main thread (the last line of BUG-137). The same fault from kernel mode (a system call copying into a page of its
  caller that was never touched) went to `[idt] PAGE FAULT` and `halt()`.
- **Fix:**      `memory_pressure` in the fault path: page cache, sweep of memory files nobody holds, swap when present;
  below 1 MiB free one process is ended, chosen by `euromm::oom::pick_victim` (the child that holds the most; the
  launched process only when it has no child). 64 frames are kept for faults from kernel mode; a system call that
  still finds none ends its process, not the kernel. `design/MEMORY-RECLAIM.md` §5.
- **Proof:**    `[memt] oom` 5/5: a child holds 60% of a 64 MiB pool, the main process touches another 60%: the child
  is ended (wait status SIGKILL) and the main process gets its memory; a child that touches twice the pool is ended
  and the parent lives. The path "inside a system call" has no self-test (see the README, not done).

### BUG-156 — idle utility processes of the browser never end; each keeps a 128 MiB arena ⬜ OPEN
- **Status:**   OPEN · **Severity:** HIGH for a long session (after 27 processes, 13 on 4 GiB, no renderer can be forked)
- **Actual:**   in the two-hour sessions of `proof/chrome-mem/` the process count only grows: 6, 16, 19, 21, 24, 26,
  27 (16 GiB, one census every ~12 min). The last census lists 17 `video_capture.CrUtility` (one per round of the
  video steps), 2 `unzip.CrUtility`, `data_decoder`, `on_device_model`, each with 1 MiB of anonymous memory and a
  128 MiB arena in the process pool. Then `[fork] arena alloc FAILED (128 MiB, pool has 137 MiB)` 64-116 times per
  session and the site steps fail. The demand pool is not involved (10 GiB free); nobody is ended.
- **Expected:** a utility process ends when the browser drops its connection (on Linux within seconds).
- **Not known:** whether the child never sees the end of its Mojo channel (an end of the socket still open in a
  process that was forked later would do that) or the browser never closes it.
- **Also:**     the split of the RAM in a process pool (up to half) and a demand pool is what makes this bite at
  10 GiB free; `design/MEMORY-RECLAIM.md` §6.

### BUG-157 — a 4 GiB browser guest hung after 5 minutes, the BSP in schedule_core with interrupts off ⬜ OPEN
- **Status:**   OPEN · **Severity:** HIGH (once in 12 browser sessions on the image of `f9a8870`)
- **Actual:**   run s4 of `proof/chrome-mem/` (4 GiB, site matrix, after the fourth site): "the desktop loop did not
  answer the serial console within 40 s". NMI probe: `sched::schedule_core+0x27c`, IF=0, CR2 0x11c030d9000; 153 live
  tasks, 572 MiB of the demand pool and 387 MiB of the process pool free; no `[oom]`, no refused fork before it.
- **Not known:** the cause. The reuse of the task slots of ended fork children (BUG-151) is new in what the scheduler
  sees and is not ruled out; BUG-136 is an older wedge on the scheduler lock.
- **Next:**     the probe with every task's state (it printed an empty "ready tasks" list); repeat s4.

### Notes (not bugs, measured)
- **Boot-time background musl processes and the native bg-app tests only run under KVM with
  ≥ 2 GiB after BUG-016**; on 813e627 most were killed at their first instruction and the boot
  self-tests did not notice (their markers do not check those processes' results). glibc
  processes were not affected (their arenas are mapped by `map_user_arena`).
- A QEMU guest without `-nic`/`-netdev` still gets the default e1000e on SLIRP: the background
  update check then reaches `euro-os.eu` (HTTP 404 on the manifest). All runs had a NIC.
- Swap self-test (`gswap`) on a disk-root boot: ~4 min of synchronous `virtio_blk::kick_and_wait`
  under KVM; a selftest boot with an installed 8 GiB disk takes 330–455 s to the desktop.

- The `sched.rs:755` panic and the TCG self-test stops that the net-async work noted here are
  BUG-032 and BUG-033 now.

<!-- Add BUG-NNN entries above this line. Keep newest at the bottom of the relevant group. -->

# Sprint plan: fine-grained locking so SMP actually scales (J1)

*Branch `feature/fine-locking`, started from `cfb47db` on 2026-11-02. Roadmap item
J1 ("Per-subsystem locking"). This document is the plan, the measured inventory
behind it, and the record of what was done and what is still open.*

Status labels as in the rest of the docs: ✅ done and boot-verified · 🟡 partial · ⬜ open.

---

## 1. The starting point, measured (not guessed)

### 1.1 What "SMP" meant before J1

Reading the code before changing it gave a sharper picture than "a global lock is
the ceiling":

- **The APs ran no kernel work at all.** After bring-up each AP ran two demo
  busy-loops (`ap_worker_a/b`) forever, plus a third after the load-balance demo.
  Every syscall, every filesystem, network and IPC operation, the desktop and all
  ring-3 processes ran on the BSP. With `-smp 4` three host cores spun at 100 %
  for nothing.
- **The root VFS was not a shared object.** It was a local `Vfs` value owned by
  the boot task (`main.rs`), passed around as `&mut dyn FileSystem`. No other
  core could reach a filesystem, so "per-mount locks" first needed a VFS that
  can be shared at all.
- **The APs could not run general kernel code safely.** The AP trampoline only
  enables PAE and long mode: CR4.OSFXSR was clear (Rust on this target uses SSE
  for memcpy and float formatting: `#UD`) and EFER.NXE was clear (page-table NX
  bits are then reserved bits: a page fault). Nothing noticed, because nothing
  but integer loops ever ran there.
- **An AP tick overwrote the BSP's current-task mirror.** `ap_schedule_tick`
  stored its own slot index (0..3) into `CURRENT_MIRROR`, the lock-free "which
  task runs on the BSP" value that capability checks and diagnostics read.
- **The lock-order checker existed but was not wired in.** `crates/eurolock`
  had a hierarchy and an `OrderTracker` with host tests; no kernel lock used it.

### 1.2 Inventory of global critical sections

Interrupts-off (`IF=0`) sections used as locking:

| Where | What | Notes |
|---|---|---|
| Syscall entry (`ring3.rs`, FMASK clears IF) | **every syscall** runs with IF=0 on the BSP | the biggest one: blocking socket calls spin with IF=0 for seconds (documented in `net.rs`) |
| Foreground exec (`enter_ring3`) | ring-3 code itself runs with IF=0 | synchronous boot-time programs |
| `ring3::syscall_dispatch` | the global `BG` process table is held across the whole `bg_dispatch` of a background process, with IF=0 | network syscalls therefore hold BG while they wait (the driver team's open point, see §6) |
| `sched::IfOffGuard` | 36 sites in `ring3.rs`, 3 in `net.rs` (`pump_all`, `rx_queued_bytes`), `virtio_net`, `sched` | spinlocks reachable from IF=0 contexts (the IRQSAVE rule) |
| `without_interrupts` | 58 sites: allocator 9, ring3 21, xserver 6, xhci 5, mouse 4, serial 3, ps2 3, euroapi 3, main 2 | heap and UART are the "sneaky" ones from BUG-007/010/011 |
| Explicit `cli`/`sti` | early boot (IF=0 until the APIC timer starts), timer/yield stubs, `sched.rs`, `ring3.rs` | |

Global locks: 177 `static` spinlocks in the kernel. The large ones:

| Lock | Protects | Reached from |
|---|---|---|
| `sched::SCHED` | the BSP run queue (all ring-3 tasks) | timer IRQ (try_lock), syscalls, task context |
| kernel heap (`LockedHeap`) + big-block pool | every allocation | everywhere, including IRQ handlers |
| `ring3::{FILES, OPEN_FDS, PIPES, BG, FUTEX_QUEUE, ...}` (65 statics) | per-process state kept in global tables | syscalls |
| `net::SOCKETS` | the whole TCP/UDP socket table | syscalls, desktop loop (`pump_all`) |
| `net::UNIX_SWITCH` | every AF_UNIX listener and connection | syscalls, X server |
| `euroipc::{PORTS, AUDIT}` | every port, queue and the audit lines | syscalls |
| root VFS | every mounted filesystem | boot task only (exclusive ownership) |
| `rootblk::CACHE` | root block cache | already an `RwLock` (earlier J1 work) |
| `klog` ring | kernel log | already lock-free (earlier J1 work) |

### 1.3 Measuring contention

`crates/eurolock` now carries contention statistics (§3.1). Every new lock and a
set of legacy globals (in a statistics-only mode, behaviour unchanged) report
acquisitions, contended acquisitions, spin count, longest wait and hold time.
The kernel prints them as `[lk]` lines and the `lockstat` shell command shows
them.

Result on the unchanged kernel structure: **the legacy global locks are not
contended in a normal boot**, because only the BSP takes them. That is the
measurement that matters: the bottleneck was not contention on an existing lock
but the absence of any parallel kernel work. Contention appears the moment real
work runs on several cores, and it was measured on the first multi-core run
(lockbench image, `-smp 4`, first version):

| Lock | Workload | Contended acquisitions |
|---|---|---|
| one global lock around the fs operations (the pre-J1 shape) | fs, 4 cores | 99 % |
| one global lock around the ipc operations | ipc, 4 cores | 88 % |
| **kernel heap lock**, with per-mount/per-channel locks in place | fs, 4 cores | **59 %** |
| kernel heap lock | ipc, 4 cores | 24 % |
| per-mount lock, all cores on ONE mount | fs-1mount | 97 % (expected: one filesystem is one lock) |

After the subsystem locks were split, the heap was the top cost, and after the
heap the next measured limit was shared cache lines: shared statistics counters,
the reader count of each table's `RwLock`, the IPC audit ring head. Each step in
§2 was chosen from the previous measurement.

## 2. Chosen order (and why)

1. **Instrument + make the APs usable** (measure first; without kernel work on
   the APs nothing can be measured or gained).
2. **Per-mount VFS** (the root VFS becomes shareable at all).
3. **Per-connection network locks** (AF_UNIX per connection, TCP/UDP per socket).
4. **Per-channel EuroIPC** (as a host-tested core; see §6 for the merge).
5. **Heap**: per-CPU small-object caches (measured top bottleneck after 2-4).
6. **Shared cache lines**: per-CPU sharded counters, per-CPU replicated
   read-mostly tables, a cheap per-CPU id.
7. **Scheduler per CPU**: the AP side (queues, stealing, idle in `hlt`) is done;
   moving ring-3 tasks onto APs is the next sprint (§7).

## 3. What was built

### 3.1 EuroLock as the lock library (`crates/eurolock`) ✅

- **Hierarchy** extended to the fine-grained classes, one class per kind of
  instance lock: `Sched < RunQueue < Process < FsMountTable < Fs < BlockCache <
  Vault < IpcTable < IpcChannel < Net < NetSocket < UnixTable < UnixConn < Agent
  < Firewall < Audit < Serial < Heap`. Two instances of one class may never nest
  (no path holds two mounts, two channels or two connections).
- **`sync::Mutex` / `sync::RwLock`**: wrappers around `spin` with a **mode** that
  encodes the IRQSAVE rule: `Irq` (interrupts off while held or waited for:
  mandatory for anything reachable from a syscall or IRQ), `Task` (interrupts
  untouched, for long holds such as a filesystem operation; an acquisition with
  IF=0 is counted as a violation) and `Plain` (statistics only, for measuring
  legacy locks without changing them).
- **Order checking** through kernel-installed hooks: before waiting for a lock
  the class is checked against everything this context holds. Contexts are
  per task on the BSP (a task can be preempted while it holds a `Task` lock)
  and per core on the APs.
- **Statistics** per class and per named site, sharded per CPU (one cache line
  per core), with hold times from the TSC.
- **`sync::ReadMostly`**: a per-CPU replicated reader-writer lock. A reader
  locks only its own core's copy, a writer updates every copy under all copy
  locks (in order). Used for the mount table, the IPC channel table and the
  AF_UNIX connection table, where a plain `RwLock` read is an atomic on a
  shared word (measured: it held IPC at x1.7 on 4 cores).
- All of it `#![forbid(unsafe_code)]`, host-tested with real threads (exclusion,
  contention counting, IRQ-mode nesting, replica consistency under concurrent
  writes). The `unsafe` parts (cli/sti, TSC, per-CPU state) live in
  `kernel/src/klock.rs`.

### 3.2 APs run kernel work (`kernel/src/kwork.rs`, `smp.rs`, `sched.rs`) ✅

- AP CPU state: OSFXSR/OSXMMEXCPT/OSXSAVE, CR0.EM clear/MP/NE/WP and EFER.NXE
  copied from the BSP before an AP runs general code.
- Demo workers are parked once the `[smp]` self-test has read their counters
  (the `[smp]` markers are unchanged); idle APs sleep in `sti; hlt` instead of
  spinning.
- Per-CPU work queues (class `RunQueue`, `Irq` mode) with a lock-free pending
  hint, **work stealing with `try_lock` only** (an idle core never waits on
  another core's queue), and a ping IPI to wake a sleeping core. This is the
  shape of Redox's per-CPU run queues and stealing (recent `context/switch.rs`
  work), re-implemented; no Redox code was copied.
- `CURRENT_MIRROR` is BSP-only again (the AP tick no longer overwrites it).
- `kernel/src/percpu.rs`: a per-CPU id from the stack pointer. The Local APIC id
  is an MMIO read, and under TCG it **halved** single-core lock throughput once
  every lock asked for it (measured); `qemu64` has no RDPID/RDTSCP and GS base is
  user-settable on the BSP. The `[kwork]` self-test checks the stack-derived id
  against the APIC id on every core.

### 3.3 Per-mount VFS (`crates/eurofs/src/shared.rs`, `kernel/src/kvfs.rs`) ✅

`SharedVfs`: mount table in a `ReadMostly` (class `FsMountTable`, held only to
route a path and clone an `Arc`), **one lock per mount** (class `Fs`, `Task`
mode: a filesystem operation can do milliseconds of disk I/O and must not run
with interrupts off). Umount during an in-flight operation is safe (the
operation holds the `Arc`). The kernel's root VFS is now a `SharedVfs`; the boot
task keeps using it as `&mut dyn FileSystem`, and a clone is published for other
cores (`kvfs::root()`).

### 3.4 Per-connection / per-socket network locks ✅ (AF_UNIX measured, TCP structural)

- `euronet::unix::SharedSwitchboard`: listeners behind one lock, the connection
  table in a `ReadMostly`, **one lock per connection**; `Irq` mode (syscall
  path). Same semantics as the old `Switchboard` (ids never reused, same EOF and
  EPIPE rules); host-tested with parallel streams and connect/close churn.
  `net.rs` uses it instead of the global `Mutex<Switchboard>`.
- `net::SOCKETS` is now **one lock per socket** instead of one mutex around the
  table. A blocking `recv` that spins on one connection no longer holds every
  other socket; `pump_all` locks one connection at a time and visits only
  slots hinted as connections; slot allocation uses `try_lock` so `socket()`
  never waits for another socket's blocking call. Honest limit: TCP traffic
  itself still goes through one NIC driven from the BSP, so parallel TCP is not
  benchmarked; the per-socket locks are verified by the boot net self-tests and
  the order checker.

### 3.5 Per-channel EuroIPC (`crates/euroipc`) ✅ core, ✅ kernel wiring (after the merge)

`ChannelTable`: channel table in a `ReadMostly`, **one lock per port**, per-CPU
sharded counters and audit rings (merged by timestamp on read), all `Irq` mode.
It already implements the driver branch's semantics (bounded queues with
`MAX_QUEUE`, restricted ports, `set_only_from`, `unregister`, `take` by port,
`send_with` without audit). The kernel's `euroipc.rs` was deliberately left
unchanged on this branch, because the driver branch rewrote it; see §6. After
the merge the kernel bus runs on `ChannelTable`: see §9.

### 3.6 Heap: per-CPU small-object caches (`kernel/src/allocator.rs`) ✅

Blocks up to 2048 bytes (align ≤ 16) come from a per-CPU free list per size
class (14 classes), taken and returned with interrupts off and no lock; the
shared heap lock is taken once per batch of 16 to refill or to give back. The
class depends only on the `Layout`, so `dealloc` finds the same class as
`alloc`. `big_owns` (called on every free) no longer takes the big-pool lock.

## 4. Proof

### 4.1 Host tests

`cargo test -p eurolock -p eurofs -p euronet -p euroipc`: green. The new tests
use real OS threads: per-mount isolation and serialisation, mount/umount churn
during I/O, a busy mount not blocking another mount, AF_UNIX streams exact and
in order under parallel load and control-plane churn, IPC pairs on separate
channels, many senders on one port with registrations racing, audit records
never torn, replicated tables consistent under concurrent writers.

### 4.2 Boot self-tests

| Marker | Proves |
|---|---|
| `[lk] checker self-test` | Irq mode really disables interrupts and restores them, correct order silent, a deliberate inversion detected (and kept out of the real-inversion count), Task mode leaves IF alone, try_lock, RwLock, tracker balanced |
| `[kwork]` | every AP runs a job that allocates, formats and uses SSE, with a per-CPU id equal to its APIC id; work stealing happens |
| `[lk-stress]` | 3 rounds, all cores at once, mixed fs/ipc/unix plus a read of the real root VFS from every core: 0 correctness errors, 0 order inversions, 0 task locks under IF=0, every round finishes (no deadlock) |
| `[lk-bench]` (lockbench image) | throughput per workload, J1 locks vs one global lock, 1 core vs all cores, 3 rounds each, every operation verified |

### 4.3 Throughput (QEMU TCG, multi-threaded, no KVM)

**These are TCG numbers.** The cores run in parallel on host threads, but
emulation changes the cost of everything (atomics, MMIO, cache-line transfers);
hardware numbers will differ and have not been measured. `-smp 4` TCG also
generates slower code than `-smp 1` (it must emulate atomics for real
parallelism), which is why the one-core figures inside a `-smp 4` run are lower
than in a `-smp 1` run. Medians of 3 runs of 700 ms; host shared with other
services, so single runs vary by ±15 %.

| Workload (ops/s) | `-smp 1` | `-smp 4`, one global lock | `-smp 4`, J1 |
|---|---|---|---|
| fs, one mount per core (EuroFS on RAM disks, write 512 B + verify) | 7 154 | 4 852 | **19 547** |
| ipc, one channel per core (send + recv + verify) | 206 728 | 87 074 | **427 895** |
| unix, one connection per core (64 B through + verify) | 142 971 | 76 077 | **377 444** |
| fs, all cores on ONE mount | 7 157 | n/a | 3 337 (one filesystem stays one lock) |

Within one `-smp 4` boot, 4 cores vs 1 core: fs x3.27, ipc x3.04, unix x3.40 with
J1; x0.80 to x0.85 with one global lock. How the J1 figure got there (same
workloads, `-smp 4`, 4 cores vs 1):

| Step | fs | ipc | unix |
|---|---|---|---|
| per-mount / per-channel / per-connection locks only | x1.25 | x1.43 | x1.50 |
| + per-CPU heap caches | x3.12 | x1.72 | x2.07 |
| + sharded counters, replicated tables, cheap per-CPU id | x3.27 | x3.04 | x3.40 |

The "before" of the real pre-J1 kernel is not in this table because it could not
run this work on more than one core at all.

### 4.4 Full boot, before and after (selftest image, same QEMU command line)

| Run | `cfb47db` (before) | `feature/fine-locking` |
|---|---|---|
| `-smp 4`, 512 MiB | **halts** in the Euro API fuzzer: kernel-mode page fault writing into a user code page (`[idt] PAGE FAULT ... PROTECTION_VIOLATION \| CAUSED_BY_WRITE`); `[euroapp]` native app FAILED (`frame-presented=false`) | full self-test suite to the interactive desktop in about 3 minutes wall time; `[euroapp]` OK, `[eurofuzz]` OK; `[lk]`, `[kwork]`, `[lk-stress]` OK |
| `-smp 4`, 2 GiB | same halt at the same place | to the desktop; the glibc tests that need memory pass |
| `-smp 1`, 512 MiB | not at the desktop after 20 minutes (no progress after the `gxlive` X11 test) | to the desktop |

Failures that remain on this branch, and why they are not regressions: with
512 MiB the glibc-based tests (`[app]` SDK run, `[churn-proc]`, `[churn-thread]`,
`[churn-fork]`, `[app-policy]`) fail with `(no arena for glibc)`, which the
baseline log shows too (the same 46 lines); with 2 GiB they pass except
`[churn-fork]` (`arena alloc FAILED (384 MiB, pool has 288 MiB)`, a memory
sizing limit of that test).

**Why the `-smp 4` baseline halted, found by this sprint:** the AP timer tick
wrote its own slot index into the BSP's lock-free current-task mirror
(`CURRENT_MIRROR`). Two security-relevant paths read that mirror on every
syscall: `effective_caps()` (the capability set of the running task) and
`copy_to_user()` (whether the running task is a native app whose read-only
pages must be checked before a kernel write). With more than one core they
could see task 0..3 instead of the real task: the wrong capability set, and a
skipped writability check, which is exactly the kernel write into a read-only
user page above. The fix (the AP tick no longer touches the mirror) is part of
§3.2. **With `-smp 1` this never happened**, which is why it went unnoticed.

### 4.5 What the statistics say about the syscall path (for §6)

From a full `-smp 4` boot to the desktop (2 GiB), `[lk] boot-total` lines:

| Lock | Acquisitions | Longest hold | Average hold | Context |
|---|---|---|---|---|
| `ring3.BG` (process table) | 184 916 | 24.9 ms | 14 µs | held across every background syscall, IF=0 |
| `ring3.FILES` | 6 374 | 8.7 ms | 53 µs | syscall file table |
| `net.CFG` | 12.2 M | 50 ms | < 1 µs | one long holder (still to identify) |
| `sched.SCHED` | 697 344 | 1.8 ms | 1 µs | |
| `vfs.mount` (per mount) | 16 798 | 115 ms | 461 µs | Task mode, interrupts on: disk I/O on the root disk |

None of the legacy locks is contended (only the BSP takes them), but the hold
times are interrupts-off windows: 25 ms under `BG` is 2.5 timer ticks with IF=0.

## 5. Lock order (the rule, recorded in `crates/eurolock`)

```
Sched < RunQueue < Process < FsMountTable < Fs < BlockCache < Vault
      < IpcTable < IpcChannel < Net < NetSocket < UnixTable < UnixConn
      < Agent < Firewall < Audit < Serial < Heap
```

Table locks (`FsMountTable`, `IpcTable`, `UnixTable`) are released before the
instance lock is taken, so in practice they never nest with it; the order still
allows it. The heap is the innermost leaf. Measured: 0 inversions in every
boot of this sprint.

## 6. Notes for the merge with `feature/userspace-drivers`

Files both branches touch and what this branch changed there:

- `kernel/src/euroipc.rs`: **unchanged here**. The driver branch rewrote it
  (bounded queues, restricted ports, IRQ notification slots, `without_interrupts`
  around every lock). The per-channel core `crates/euroipc` already has the same
  API surface. After the merge, the follow-up is mechanical: keep their
  notification slots (`bind_notify`/`notify_from_irq`, lock-free already) in the
  kernel file and replace `PORTS`/`AUDIT` with one
  `static TABLE: euroipc::ChannelTable = ChannelTable::with_clock(rdtsc)`:
  `claim/register/register_restricted -> TABLE.register*`, `send/send_with ->
  TABLE.send_with` (`Err` values are the same errnos), `recv(pid, buf, max) ->
  TABLE.recv(pid)` then copy out after the lock is gone, `take(port) ->` their
  notification check, then `TABLE.take(port)`, `unregister`, `set_only_from`,
  `audit_lines -> TABLE.audit_lines(6)`, `DROPPED_FULL`/`REFUSED_SENDER ->
  TABLE.dropped_full`/`refused_sender`. Their `without_interrupts` wrappers become
  unnecessary (all `ChannelTable` locks are `Irq` mode).
- `kernel/src/ring3.rs`: only declarations changed: `FILES`, `OPEN_FDS`, `BG`,
  `FUTEX_QUEUE`, `CHILD_EXITS` became `eurolock::sync::Mutex` in `Plain` mode
  (statistics only, same behaviour). `syscall_dispatch` is untouched, so their
  driver-profile routing before the BG lock merges cleanly.
- `kernel/src/interrupts.rs`, `kernel/src/iommu.rs`, `kernel/src/e1000.rs`: not
  touched here.
- `kernel/src/main.rs`, `kernel/Cargo.toml`, `scripts/build.sh`: additive
  (new modules, dependencies, a `lockbench` feature and build profile).

**The BG lock and IF=0 in the syscall path (their open point): not split in this
sprint.** What the measurement says, for planning that step: every syscall of a
background process holds the global `BG` table lock for the whole
`bg_dispatch`, with IF=0, including socket calls that wait on the network. The
statistics now show it (`ring3.BG` in `lockstat`: hold-max). Splitting it means
`BG: Vec<BgProc>` becoming a table of per-process locks (`Arc<Mutex<BgProc>>`
in a `ReadMostly`), dropping the table lock before `bg_dispatch`, and making the
blocking socket calls give up the CPU instead of spinning with IF=0 (they already
hold only their own socket lock now, see §3.4). That is the first item of the
next sprint (§7); it touches `syscall_dispatch`, so it should start after the
two branches are merged.

## 7. Open (honest)

- ⬜ **Ring-3 on the APs.** Needs per-CPU TSS/IST, per-CPU syscall state
  (`SAVED_REGS`, `USER_RIP`, the syscall stack globals), per-CPU CR3 handling
  and FPU switching on the AP scheduler. Until then all user processes run on the
  BSP; the APs run kernel work (`kwork`) only. Also: the APs share the BSP's IST
  configuration without a TSS of their own, so an exception that uses an IST
  stack on an AP is fatal today (it never happened in these runs).
- ⬜ **BG per-process lock + blocking syscalls without IF=0** (see §6, and §9
  for what the merge measured: the musl background path has no socket calls at
  all, the Linux-ABI path spins without yielding).
- ✅ **Kernel `euroipc.rs` on the per-channel core** (after the merge, §9).
- 🟡 **Per-socket TCP**: structural (per-socket locks, verified by the boot net
  tests), not benchmarked in parallel: one NIC, driven from the BSP.
- ⬜ Remaining legacy globals in `ring3.rs` (65 statics of per-process state in
  global tables) move into per-process structures with the BG split.
- ⬜ The heap's large-object path (> 2 KiB) still takes the shared list-heap lock;
  it showed up again at a few percent in the fs workload.
- ⬜ Numbers on real hardware. (KVM numbers on a Dell R630, which are virtualised, not
  bare metal: [`LOADTEST-R630-2026-11.md`](LOADTEST-R630-2026-11.md) §3.)

## 8. Reference: how Redox approaches this (studied, not copied)

Recent Redox kernel commits (October 2026) were read for this sprint:

- **Compile-time lock levels** (`src/sync/ordered.rs`, adapted there from the
  MIT `ordered-locks` crate): a lock of level `Ln` can only be taken with a token
  proving that only lower levels are held. EuroOS keeps a runtime checker with a
  documented hierarchy (`eurolock`), because most kernel locks here are statics
  taken from many unrelated call paths; the order is the same idea.
- **Futex lock ordering** (`378b856`, `57ccc8f`): block the context first, then
  insert into the global futex table, then re-validate the futex word and roll
  back; never hold the table while taking the context lock. The EuroOS
  equivalent rule is in §5 (table locks are released before instance locks) and
  applies to `FUTEX_QUEUE` vs `SCHED` when the BG split happens.
- **Per-CPU run queues with work stealing** (`04db129`, `f52b427`): a per-CPU
  queue behind a spin mutex, a lock-free "stealable" hint, stealing only with
  `try_lock`. `kwork` follows the same shape.

No Redox source was copied; nothing in this branch needs the MIT notice.

## 9. Merge with `feature/userspace-drivers` (2026-11-02)

Both branches were rewritten onto `b117647` and merged on
`feature/integrate-j1-udrv`.

- **Conflicts** were only in `Cargo.toml` (both new crate sets), the module list
  of `kernel/src/main.rs` and the EuroIPC section of the deep reference.
  `ring3.rs` merged cleanly, as §6 predicted: the EuroLock declarations and the
  driver-profile routing before the BG lock are separate hunks.
- **Kernel EuroIPC on `ChannelTable`** ✅: `kernel/src/euroipc.rs` keeps the
  driver branch's API and its IRQ notification slots, and stores ports, queues
  and the audit trail in `crates/euroipc`. One semantic difference surfaced
  while wiring it: the core used `only_from == u64::MAX` as "unrestricted",
  but the driver supervisor claims the netstack port with `u64::MAX` to mean
  "nobody until a driver holds the grant". The core now has a separate
  restricted flag (host test `restricted_to_nobody_until_set`).
- **Boot proof after the merge** (selftest image, QEMU TCG, 512 MiB): `-smp 4`
  and `-smp 1` reach the interactive desktop with the same per-marker results as
  this branch's own runs (the same glibc "no arena" failures at 512 MiB, nothing
  else); `[lk]`, `[kwork]`, `[lk-stress]` OK, 0 order inversions, 0 task locks
  under IF=0. With `net.driver=user` and `-device intel-iommu,intremap=off` all
  `[udrv-*]` markers pass, including the IOMMU faults and the crash/restart,
  with the per-channel EuroIPC underneath. Logs:
  `docs/proof/integration-j1-udrv-2026-11-02/`.
- **The BG lock and the userspace NIC (§6 open point), measured:** an
  experimental boot ran `/bin/msock` as a scheduled musl background process
  over `e1000d` (patch in the proof folder, not merged). It "finished" in 8
  ticks with 0 frames sent: `bg_dispatch` has no socket syscalls (unknown
  numbers return 0), so on that path the BG lock is not what stops the
  network; the path does not exist. Socket syscalls live only in
  `linux_dispatch` (foreground programs, glibc processes), which does not hold
  BG but spins in `net.rs` with IF=0 and never yields. Splitting BG is still
  worth doing (the hold times in §4.5 stand), but it is not by itself what
  makes the userspace driver usable for processes; see the driver plan.

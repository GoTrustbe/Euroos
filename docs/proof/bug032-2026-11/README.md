# BUG-032: the scheduler ran with RFLAGS.DF set (2026-11-05)

Branch `feature/bug032` on `feature/app-control` (`e9f9a31`; the nightly sync
had added nothing). Not merged.

| Commit | What |
|---|---|
| `44885f4` | detection: `schedule_core` validates `current` and dumps what is around it; debug-register write watch (`memwatch.rs`); `loadtest.py` keeps the panic dump |
| `b6a6fa8` | the fix: `cld` in the three context-switch stubs, FMASK clears DF/TF/NT; `[bug032]` DF self-test |
| `ffec65c` | `loadtest.py --nmi-on-hang` also probes a boot that hangs before the console is up |
| `e5abd63` | the isolation fault dump read its second code qword at address 0 (operator precedence), found on the way |
| (last) | this proof, the tracker |

Serial logs stay on the build servers (`*.log` is not committed); the
`.markers` files hold the verdict lines, the `.txt` files the excerpts that
carry the argument.

## Cause

An interrupt does not clear RFLAGS.DF. The three raw context-switch stubs
(`timer_switch`, `yield_switch`, `ap_timer_switch`, `sched.rs`) push the
registers and call Rust with whatever DF the interrupted code had. The
interrupted code can be in the middle of a backward copy: `compiler_builtins`
memmove does `std; rep movsq; cld` for an overlapping copy (Vec/VecDeque
shifts, EuroFS), and ring 3 may do the same. Rust code assumes DF=0.

`schedule_core` gets its `SCHED` guard from `try_lock` into one stack slot and
copies the 40 bytes to another with `rep movsq`. With DF=1 that copy runs
backwards: the first qword lands, the others are written below the target,
and the guard's data pointer (`0x18(%rbp)`) is whatever the stack held there
before. `current` is then read at that word + 0x6820.

- **R630, the original panic** (`r630-original-panic-analysis.txt`, rebuild
  of `bb2e9d2`): the bad value `0x48C031037FFFFFE2` is the eight `.text` bytes
  at `0x140211b3c` (`core::num::dec2flt::lemire::compute_float<f32>`, inside
  `and edx, 0x37fffff`). `0x140211b3c - 0x6820 = 0x14020b31c` is a return
  address: the instruction after a call in `ab_glyph`'s
  `OutlineCurveBuilder::quad_to`, i.e. the lockscreen text that `[ag-lock]`
  had just drawn at the same stack depth. The panic dump shows
  `RFLAGS=0x406`: DF still set in the panic handler.
- **TCG, a recurrence on the detection image** (`tcg-before-panic.txt`, 1 of
  12 `user-smp4-iommu` boots): the guard pointer was 0, `current` was read at
  `0x6820`; the interrupted RIP is `compiler_builtins::mem::memmove+0x56`
  (under EuroFS `rebuild_allocator_trim`), `RFLAGS=0x402`.
- **Provoked** (`tcg-nocld-selftest-panic.txt`): the fix commit with the three
  `cld` removed panics in the `[bug032]` self-test (DF=1 across a tick): guard
  at `0x14349c10` instead of `SCHED.current` at `0x1439cbb8`, `current` = heap
  text (`"n change"`).

Nothing wrote over the scheduler state; the write watch saw no foreign write
in any boot. With DF=1 every `memcpy` in the tick path also wrote below its
destination, and when the stale word pointed at memory with a small value the
switch worked on a fake run queue (and the guard's stale `saved` field could
re-enable interrupts inside the tick): silence without a fault line. That is
the picture of BUG-031 and BUG-033; an R630 boot of the detection image went
silent exactly where the BUG-033 base boot did, after
`[glibc] === SDL2 app (gsdl) ===` (`r630-before-hang-tail.txt`, 1 of 31).

SYSCALL had the same exposure: FMASK cleared only IF, so a ring-3 program
could enter the syscall path with DF=1 (the ABI keeps DF clear at a call, a hostile program need not).

## Fix

- `cld` as the first instruction of the three stubs. iretq restores the
  interrupted RFLAGS, so the interrupted copy continues with its own DF. The
  `x86-interrupt` handlers already start with `cld` (LLVM emits it).
- FMASK = `0x4700`: IF, DF, TF and NT are cleared on SYSCALL. The entry stub
  sets AC itself afterwards (SMAP window), unchanged.
- Kept, always on: `schedule_core` checks `current < MAX_TASKS && current <
  count` before it indexes (the bounds check it replaces was there anyway)
  and reports the guard address against the real `SCHED.current`, the
  interrupted RIP/RFLAGS, the core, CR3 and the 32 words around it.
- Kept, opt-in: `memwatch.rs`, a DR0/DR1 write watch on `SCHED.current` and
  `SCHED.count` on every core (`debug.schedwatch=on` in `euroos-boot.conf`).
  Every legitimate write is one #DB (a VM exit under KVM), so it is off by
  default. It was on in all before-fix runs below.

## Regression test

`[bug032] DF set across context switches` (boot self-test, every build): with
DF=1 in pure assembly it spins across timer ticks and executes one yield
(`int 0x4A`), then checks that the switches saw DF=1 in the interrupted frame,
that the Rust handler never saw it, that iretq gave DF back and that FMASK
clears DF:

```
[bug032] DF set across context switches: 2 switch(es) interrupted code with DF=1, handler saw DF=1 in 0, DF back after iretq=true, SYSCALL clears DF (FMASK 0x4700)=true → OK ✓
```

Without the `cld` the same image panics in this test (above). OK in every
boot below, TCG (`qemu64`, FXSAVE) and KVM (`-cpu host`, XSAVE).

## Before / after

| | before (`44885f4`, detection only) | after (`b6a6fa8` … `e5abd63`) |
|---|---|---|
| TCG `user-smp4-iommu` (512 MiB) | 12 boots: **1 panic** (BUG-032) | 21 boots: all to the desktop |
| R630 KVM `net`, `net.driver=user`, 4 GiB | 31 boots: **1 hang** after `gsdl` (BUG-033 picture) | 48 boots: all ok |
| R630 KVM `net`, `net.driver=kernel` | | 6 boots: all ok |
| R630 KVM `net`, `net.driver=user` + intel-iommu | | 5 boots: all ok |

## Build server (QEMU 10.2, TCG)

As in `../integrate-net-mem-2026-11/` (`tcg-boot.sh`): `-machine q35 -cpu
qemu64,+smep,+smap`, selftest image, `-nic user,model=e1000e`, each boot until
120 s after `[desktop] interactive loop started`, three QEMUs at a time
(`nice`), each stopped by its own PID. musl ELFs from the main checkout, the
dev key as a symlink.

| Runs | Result |
|---|---|
| `user-smp{4,1}-{iommu,noiommu}`, `kernel-smp{4,1}`, `storage-smp{4,1}-iommu-1g`, `combined-smp{4,1}-iommu-1g`, `user-smp4-iommu` ×6 more (`tcg-*.markers`) | 16 of 16 to the desktop; 0 markers worse than `../integrate-net-mem-2026-11/` (`comparison-tcg.txt`); the only new marker is `[bug032]` OK |
| `user-smp4-iommu` ×12 more on the head image, + 1 | all to the desktop, 0 worse, `[bug032]` OK |
| `megaraid-proof.sh` with IOMMU ×2, without ×2 (8 boots, `mr-*.markers`) | 0 markers worse (`comparison-mr.txt`); host checks 4/4 in every run |

`[lk] boot-total order inversions=0 … task-lock-with-IF0=0` in every boot. No
panic, no watchdog line, no boot without desktop.

## Dell R630 (KVM)

The branch carried as a git bundle into `/srv/euroos/wt-bug032`, built there
with a throwaway dev key (glibc test binaries re-signed locally, not
committed). `scripts/loadtest.py --kvm --smp 4 --nic e1000e --restrict
--nmi-on-hang`, each VM pinned to its own 4 host CPUs, no VFIO device,
release image. `--scenario net --host-http --conns 50 --downloads 10` per
boot (`r630-summary-*.txt`).

### Soak, 1 h, 4 httpd clients the whole hour (`r630-soak-fix*`)

`--mem 16G --disks 8G --host-http --hammer 4 --procs 32 --scenario soak
--duration 3600 --boot-timeout 900`, `net.driver=user`, head image
(`e5abd63`), as `../integrate-net-mem-2026-11/` (`r630-soak-user`).

| | this branch | integration (`r630-soak-user`) |
|---|---|---|
| commands | 1 720, all ok, `completed clean` | 1 805, all ok |
| `heap_used` first / at 10 min / last | 147 936 784 / 147 968 384 / 147 990 144 | 147 927 568 / 147 962 240 / 147 980 416 |
| slope after 10 min (least squares) | +0.43 kB/min (range 99 kB) | +0.37 kB/min (range 96 kB) |
| tasks / files / kernel stacks / process pool | 55 / 186 / 0 / 887 296, flat | the same, flat |
| free frames | 2 964 983 → 2 981 623 (the first wave's 32 zombies reaped) | 2 964 988 → 2 981 628 |
| httpd during the soak | 4 326 answered, 604 failed, p50 990 ms, p99 9.6 s | 4 570 / 652, p99 9.5 s |
| serial | 0 panics, 0 watchdog lines, `[bug032]` OK | 0 panics, 1 watchdog line |

The httpd failures and the p99 are the known slowness during long synchronous
shell commands (`churn fs`, 8 MiB `sha256sum`), unchanged; another team works
on that. An earlier soak on `b6a6fa8` ran 40 min (1 watchdog line in
`churn fs`, no failure) before the repro loop's own `timeout 2400` stopped it;
it is not counted.

## Open

- The AP cores load no TSS (`gdt::init_ap`), but the IDT is shared and the
  page-fault, double-fault and NMI gates use IST stacks: an exception or an
  NMI on an AP would take its stack pointer from a TSS at linear address 0.
  Not hit here (APs run kernel jobs only); `--nmi-on-hang` NMIs reach the APs.
- `procpool`/`FrameAllocator::free` accept a frame the pool never owned (the
  bitmap starts all-used, so freeing a foreign frame adds it to the pool);
  the exit paths free a task's kernel-stack slot and a fork child's page
  tables while still running on them and rely on "single core, nothing
  allocates in between". Not implicated in BUG-032; worth a hardening pass.
- BUG-031 and BUG-033 are not reproduced on the fix; they stay 🟡 until a
  longer TCG series without IOMMU and with megaraid has run.

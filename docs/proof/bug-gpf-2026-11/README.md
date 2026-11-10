# The ring-0 #GP in the 1 h soak: BUG-040, with BUG-037, BUG-039 and BUG-050 (2026-11-05 … 11-10)

Branch `feature/bug-gpf` on `feature/app-control` (`a36ef4b`). Not merged.

| Commit | What |
|---|---|
| `83930f3` | `euromm`: a frame the allocator never owned cannot be freed into it (hardening, host tests) |
| `22a1685` | **BUG-040**: a glibc main task is parked until the launcher has published it; `[bug040]` self-test |
| `a410803` | **BUG-037** (and the BUG-033 hangs): `PIPE_FDS` and the fd-table reset with interrupts off |
| `9553f59` | **BUG-039**: `rx_route` polls the NIC without a configuration (USB ethernet late bring-up) |
| `dd4c3c8` | **BUG-050**: every AP has its own GDT, TSS and IST stacks; `[ap-tss]` self-test |
| `eaa95dd` | a dead task yields until the switch really happens (hardening of the exit paths) |
| `67097f1` | `loadtest.py --soak-churn-proc N` |
| (last) | this proof, the tracker |

The images of the runs below were built as `b085395`: the same tree as
`67097f1`, before a commit message was renumbered (BUG-050). Serial logs stay on the build servers; the `.markers` files hold the verdict
lines, the `.txt` files the excerpts that carry the argument.

## The #GP (`r630-gpf-analysis.txt`)

`[idt] GENERAL PROTECTION FAULT code=0x0 @ 0x7fe46c12 cs=0x8 ss=0x10
rsp=0x7fe46b80`, R630 KVM, 1 h soak of `52755fc`, t = 2825 s, in the third run
of a `churn proc 4`.

- RIP and RSP both lie in the boot task's stack (the stack UEFI handed over;
  the BUG-032 panic of the same image family had `RSP=0x7fe45150` there). RIP
  is 0x92 above RSP: the CPU was executing stack bytes. The stack words are a
  `.rdata` constant, heap and stack addresses (map of exactly that build).
- The crashing launch is the only `gtiny` launch in the log without its
  `[glibc] spawned scheduled task N` line (252 launches, 251 with the line):
  the launcher was preempted after `sched::spawn_user` and before that print. The
  program's own output (`GLIBC-DYNAMIC-OK`) is the last line before the fault.
- `run_glibc` made the task Ready and only then, with interrupts on, stored
  `GLIBC_MAIN_TASK`, the task's caps, its kernel-stack owner and its profile.
  A program that ran in that window found `GLIBC_MAIN_TASK == usize::MAX`: its
  `exit_group` took the foreground (musl excursion) branch, `EXITED = 1`, and
  `syscall_entry` then leaves through `mov rsp, [SAVED_KERNEL_RSP]; pop …; ret`,
  the frame of the last foreground exec on the boot task's stack, long since
  reused. A `ret` into whatever lies there.

**Status of the original crash: cause demonstrated, the spontaneous crash not
reproduced.** What supports BUG-040 as the cause:

1. the log signature above (the one launch without a spawn line, death at the
   program's exit, execution on the boot task's stack);
2. the same image with the window held open dies at the same place with the
   same signature. `[bug040]` gives the CPU away for up to 1 s between spawn
   and publication; with `spawn_user` in place of `spawn_user_parked`:
   - R630 KVM (`r630-oldorder-selftest-kvm.txt`): no spawn line, silence after
     `GLIBC-DYNAMIC-OK`; the NMI probe finds the glibc task (last syscall
     `231(a1=0x2a)`, exit_group(42)) with `RSP=0x7fe46b30`, in the boot task's
     stack, 0x50 below the original fault's RSP, at a wild RIP;
   - TCG (`tcg-oldorder-selftest.txt`): the VM resets right after
     `GLIBC-DYNAMIC-OK`.
   Where the stale `ret` lands differs per boot (#GP, reset, wild loop).
3. on the fix the same self-test passes everywhere (task stayed parked, exit 42).

What is missing: the unprovoked crash on the unfixed image. 23 runs of 50 × `churn proc 20` (24 150 launches) and one more 1 h soak
on `52755fc`: no fault. The window is a few instructions wide against a 10 ms
tick, so one hit in a few thousand launches is what it should take.

## Fixes

- **BUG-040** `sched::spawn_user_parked` + `start_parked`: the task exists but
  is not runnable (a reserved wait channel that `unblock`, `unblock_any` and
  `try_unblock_any` skip) until `ring3::spawn_glibc_main` has stored everything;
  one helper for the three glibc launch paths (`run_glibc`, the disk and the
  persistent variant). A full task table is an error now, not an index with
  `usize::MAX`. Same class as BUG-024 (daemon and bg-musl got their window
  closed then; the glibc paths did not).
- **BUG-037** found again here, with a probe (`tcg-bug033-nmi-probe.txt`): a
  bg-musl task spinning with IF=0 in `pipe_write_fd` on `PIPE_FDS`, the boot
  task Ready. The launcher's `reset_fd_table` (every glibc run) and several
  syscall-side helpers took `PIPE_FDS` with interrupts on. Now only through
  `pipe_fds()` (interrupts off, then the lock, as `pipes_lock()`), and
  `reset_fd_table` runs without interrupts (it also clears `OPEN_FDS`,
  `OPEN_DIRS`, `PIPE_NONBLOCK`, `PIPE_WAITERS`). This is the BUG-033 picture
  (silence after the boot self-tests, `user-smp4-iommu`), now with a cause.
- **BUG-039** `net::rx_route` returned before polling the NIC when the stack
  had no address; the DHCP/ARP readers of `late_bring_up` never saw a frame.
  The xHCI side needed nothing.
- **BUG-050** the IDT is shared and its #DF/#PF/NMI gates use IST stacks, but
  the APs loaded no TSS; after INIT TR has base 0, so an AP read its IST
  pointer from linear address 0x24… A monitor `nmi` (what `--nmi-on-hang`
  sends) goes to every core. `gdt::init_ap(cpu)` builds a GDT (same selector
  layout), a TSS and three IST stacks per AP. `[ap-tss]`: an NMI IPI to each
  AP, the handler records its stack and TR (`tcg-ap-tss-selftest.txt`: OK on
  the fix; without `load_tss` the VM resets in the self-test).
- Hardening, nothing observed: `FrameAllocator::free` refuses a frame outside
  the usable regions it was built from (the bitmap starts all-used, so such a
  free used to add a foreign frame to the pool); refused frees are counted
  and `procpool` names the first callers. `[bug040]` prints the counters:
  0 in every boot. And `switch_away_dead()`: the exit paths that free their
  kernel-stack slot (a fork child also its arena and page tables) and yield
  now yield until the switch happens; one yield returns without switching
  when SCHED is held on another core, and the dead task then went back to
  ring 3. The frees before the switch were reviewed and are sound as long as
  the task cannot run again: syscalls run with IF=0 on the only core that
  runs ring-3 tasks, and the scheduler's fallback is task 0, never a Dead task.

## Regression tests (boot self-tests, every build)

```
[bug040] glibc main task with the CPU given away between spawn and start: probe-ran=true ran-before-start=false exit=42 (want 42) · foreign frees refused so far: main=0 pool=0 demand=0 → OK ✓
[ap-tss] 3 AP(s), an NMI to each through the shared IDT's IST gate: cpu1: tr=0x28 … own-stack=true … → OK ✓ (own TSS, own IST stack)
```

`[bug040]` needs a glibc arena: at 512 MiB it prints a skip (⬜), as
`[churn-proc]` fails there for the same reason; it runs at 1 GiB and on the
R630. Host tests: `euromm` 17 (2 new). `run-metal-matrix.py --legs usbnet` is
the BUG-039 test.

## Build server (QEMU 10.2, TCG), head `67097f1`

`tcg-boot.sh` as in the earlier proofs (three QEMUs at a time, each stopped
by its own PID, NMI on a silent boot). Compared per marker with
`../virtio-modern-2026-11/` where it has the configuration, else
`../net-worker-2026-11/` (`comparison-tcg.txt`).

| Runs | Result |
|---|---|
| `user-smp{4,1}-{iommu,noiommu}`, `kernel-smp{4,1}`, `storage-smp{4,1}-iommu-1g`, `combined-smp{4,1}-iommu-1g`, `user-smp4-iommu-1g` (`tcg-*.markers`) | 11 of 11 to the desktop; 0 markers worse; new markers only `[bug040]`, `[ap-tss]` |
| `user-smp4-iommu` ×6 more, `user-smp1-noiommu` ×6 more (the two BUG-037 configurations) | 12 of 12 to the desktop, 0 worse (before the PIPE_FDS fix: 1 of 16 boots silent, 2 of 10 on `feature/virtio-modern`) |
| modern (non-transitional) virtio: virtio-blk boot + data disk, virtio-net, tablet, keyboard, `disable-legacy=on`, 1 GiB, smp 2 | desktop; 0 worse than `../virtio-modern-2026-11/tcg-dev-v2-modern.markers` (`comparison-tcg-virtio.txt`) |
| `megaraid-proof.sh` with IOMMU ×2, without ×2 (8 boots) | 0 markers worse (`comparison-mr.txt`); host checks 4/4 in every run |
| CI q35 metal matrix, `EK_FRESH_DEV_KEY=1 EK_BOOT_DEADLINE=900 … --legs base,nvme,ahci,e1000e,hda,usb,usbhub,usbaudio,usbnet,scan,printer,power,hwprobe` | 13/13 PASS (`q35-matrix.txt`); `usbnet` failed on the base (BUG-039) |

`[lk] boot-total order inversions=0` and `[net-worker]` OK in all 24 boots;
`[bug032]` DF self-test OK; no panic, no fault line, no NMI probe. `[bug040]`
OK in the six 1 GiB boots (skipped at 512 MiB), `[ap-tss]` OK in every
smp > 1 boot. Host tests (`cargo test`): 184 suites, 1 262 tests, 0 failures.

## Dell R630 (KVM), head `67097f1`

Own worktree `/srv/euroos/wt-gpf`, throwaway dev key, `loadtest.py --kvm
--smp 4 --nic e1000e --restrict --nmi-on-hang`, own CPUs, no VFIO.

### Two soaks of 2 h 05 min, `churn proc` in every round (`r630-soak-{a,b}*`)

`--mem 16G --disks 8G --host-http --hammer 4 --procs 32 --scenario soak
--soak-churn-proc 8 --duration 7500`, `net.driver=user`: every round a
`churn proc 8` (9 glibc launches) on top of the usual mix, 4 httpd clients
the whole time.

| | soak a | soak b |
|---|---|---|
| commands | 2 597, all ok, `completed clean` | 2 671, all ok, `completed clean` |
| `churn proc` commands / glibc launches (`GLIBC-DYNAMIC-OK`) | 332 / 2 676, every `[churn-proc]` verdict OK | 342 / 2 754, every verdict OK |
| `heap_used` first / at 10 min / last | 148 019 216 / 148 045 696 / 148 103 552 | 148 019 216 / 148 046 208 / 148 070 784 |
| slope after 10 min (least squares) | +0.23 kB/min (range 106 kB) | +0.23 kB/min (range 106 kB) |
| tasks / files / kernel stacks / process pool | 56 / 185 / 0 / 887 296, flat | 56 / 186 / 0 / 887 296, flat |
| free frames | 2 964 845 → 2 981 485 (first wave's zombies reaped) | 2 964 843 → 2 981 483 |
| httpd during the soak | 896 756 answered, 12 failed, p50 5.1 ms, p99 123 ms | 882 791 / 10, p50 5.2 ms, p99 124 ms |
| serial | 0 panics, 0 fault lines, 0 watchdog lines, 0 refused frame frees | the same |

5 430 glibc launches in the two soaks; the 1 h soak that hit the #GP had 252.

### Short boots (BUG-037 is a boot hang; `r630-boots-summary.txt`)

`--scenario smoke`, 4 GiB, each to the desktop and through the smoke
commands: user driver 96, user + intel-iommu 24, kernel driver 24, smp 1 24:
**168 of 168 ok**, none silent, `[bug040]` and `[ap-tss]` OK in each.

### The unfixed image, unprovoked

`52755fc` (the exact image of the crash), 50 × `churn proc 20` per run: 23
runs (24 150 launches) and one more 1 h soak: no fault. Provoked: see above.

## Open

- BUG-031 (megaraid without IOMMU, silent after `[net-late]`): the place fits
  BUG-037 (the next step is a glibc launch, i.e. `reset_fd_table`), but there
  is no probe of it; left 🟡.
- The fd tables are global and a glibc launch clears them while background
  processes live; BUG-037 only makes that atomic. Per-process tables are the
  real answer.
- The original #GP was not reproduced unprovoked (see above).

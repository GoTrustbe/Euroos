# Memory hygiene: BUG-027, [sbx2] at 1 GiB, DMA read order (2026-11-05)

Branch `feature/mem-hygiene` on `feature/app-control` (`1b022ea`: `bb2e9d2`
plus two `.gitignore` lines from the nightly sync). Commits, oldest first:

| Commit | What |
|---|---|
| `00fb152` | ring3: a glibc fork child ends with the run that forked it (the `[sbx2]` flake) |
| `93ad16e` | heaptrace: heap accounting per allocation site (debug feature, BUG-027 instrumentation) |
| `f6a6adc` | drivers: DMA read-order audit of e1000d, bcm5720d and megaraidd |
| `888001b` | eurofs: a removed object drops its extents hint too (BUG-027) |
| `c9d9b31` | loadtest: `--nmi-on-hang` |
| (last) | megaraid-proof.sh: NMI probe on a timeout; this proof, the tracker |

Full serial logs stay on the servers (`*.log` is not committed); the
`.markers` files hold the verdict lines of those logs. Tracker entries: BUG-027
(fixed), BUG-028 (`[sbx2]`, fixed), BUG-029/030 (DMA order, fixed), BUG-031 (one
hang, open) in `../../LOADTEST-TRACKER.md`. Comparisons with
`scripts/markers-compare.py` against `../integrate-loadtest-2026-11-03/`.

## BUG-027: the kernel heap grew ~30 kB/min under the soak mix

**Instrument.** `scripts/build.sh heaptrace` builds the selftest image with
per-call-site heap accounting (`kernel/src/heaptrace.rs`): a 16-byte header
per block names its site, `heapsnap` / `heapdiff` / `heaptop` in the shell
report live bytes per site, `scripts/heaptrace-sym.py` names the frames. The
UEFI target keeps the Windows x64 frame layout and no unwind tables, so a
frame-pointer walk found nothing; the site is a call chain scanned from the
stack (direct calls checked against the previous return address). Normal
images do not contain any of this.

**Found.** A heaptrace soak (`loadtest.py --scenario soak --heaptrace 15`,
R630, KVM, `-smp 4 -m 16G`, 8 GiB disk root, e1000e on SLIRP with
`restrict=on`), heap snapshot after 15 rounds, diff 50 and 100 rounds later
(`r630-heaptrace-before.txt`; sum over all sites +375 kB after 50 rounds,
+791 kB after 100, +1.19 MB after 150, +1.26 MB at the end). The 100-round
diff, grouped by the first two frames outside the allocator (the names come
from a heuristic demangler; the frames are in the file):

```
     +438016 B    +1148 blocks  (allocs ever 1190, 4 site(s))  eurofs::disk::EuroFs::cache::object <- ...SharedVfs::fs::FileSystem::set_clock::eurokernel
      +89360 B     +235 blocks  (allocs ever 298, 4 site(s))  eurofs::disk::EuroFs::cache::object <- eurofs::disk::EuroFs::cache::object
      +64496 B     +169 blocks  (allocs ever 169, 2 site(s))  eurofs::disk::EuroFs::cache::object <- ...
```

Nearly every block these sites ever allocated is still live (1 148 of
1 190). They are the `extents_cache.insert` in
`EuroFs::write_object` (oid -> data extents, PERF-002). `remove_file`,
`remove_dir` and the two rename-over-a-target paths dropped the object's
`objmap` entry but not its `extents_cache` entry, and oids are never reused:
every file the soak removes (`rm big*`, `churn fs`, the rename in a write)
left a BTreeMap entry plus a Vec behind. In the first diff the block cache
(256 slots of 4 KiB, filled on first use) also grew; it is full after about
an hour and was flat in the 100-round diff. The ~40 bytes per spawned
process of the tracker entry did not show up as a site of their own on the
disk root (`Mutex<Box<..>>` from the IPC path: +648 B for ~1 600 processes,
within noise).

**Fixed.** `EuroFs::forget_object` removes both entries (`888001b`), host
test `removed_objects_leave_no_extents_behind` (404 cache entries left
without the fix, 3 with it).

**After.** The same heaptrace soak on the fixed tree (`r630-heaptrace-after.txt`,
snapshot after 40 rounds): sum over all sites +10.5 kB after 50 rounds,
-15.7 kB after 100, -12.0 kB at the end. Then release images (no
heaptrace), the load-test soak (`--procs 32`), same machine and options, in
parallel on separate cores:

| | before (`00fb152` + `93ad16e`: no eurofs fix) | after (source of `c9d9b31`) |
|---|---|---|
| duration | 60 min, 2 000 commands, all ok | 70 min, 2 478 commands, all ok |
| `heap_used` first sample | 147 895 312 | 147 887 376 |
| at 10 min | 148 273 936 | 147 999 504 |
| last sample | 149 692 800 | 147 943 808 |
| slope after 10 min (least squares) | **+29.3 kB/min** | **+0.4 kB/min** |
| range after 10 min | 1.53 MB | 106 kB |
| `heap_cached` (per-CPU caches) | 319 → 396 kB | 331 → 409 kB |
| tasks / files / procpool / kstacks | 55 / 186 / 887 296 / 0, flat | the same, flat |
| free frames | 2 964 998 → 2 981 638 (zombies reaped) | 2 964 994 → 2 981 634 |
| serial | 0 panics, 1 watchdog line | 0 panics, 1 watchdog line |

`r630-soak-{before,after}{,-ltstat}.csv` hold the samples. `ltstat` now
also reports `heap_cached` (blocks parked in the per-CPU caches, counted in
`heap_used`) and `big_mib` (the big-block pool, not counted).

One heaptrace soak before these hung once (console silent 300 s on a
`truncate`, no fault line; the harness had no way to ask where). It did not
come back in the two later heaptrace soaks (50 min each) nor in the two
release soaks. `--nmi-on-hang` now injects an NMI through the QEMU monitor
before the VM is stopped, so a next hang leaves the kernel's NMI probe in
the log.

## [sbx2] and glibc at 1 GiB

**Cause.** At 1 GiB `GSCM3` forks child A, fork B fails (the process pool
has room for one 96 MiB arena), the parent exits with "fork B FAILED", and A
keeps waiting 30 s for a descriptor B never sends. A outlived its run and
ran into the next glibc runs. While A was blocked in a syscall it owned the
swapped per-process globals (`GLOBALS_OWNER`); the next run wrote its arena
bounds into those globals, and at the new main task's first syscall
`ensure_globals_for_current` swapped A's saved copy back in: the bounds of
the previous run's freed arena. ld.so's first mmap of `libc.so.6` got
ENOMEM ("failed to map segment from shared object"). Whether `[sbx2]` (or
another glibc test) hit that window depended on when A's 30 s ran out,
which is why it varied between runs and trees. A's exit also freed its
demand pages into the next run's demand pool.

**Fixed** (`00fb152`): `run_glibc`, `run_glibc_disk` and
`kill_persistent_glibc` end every fork child still registered, before the
run's demand pool is handed back (`reap_orphan_fork_children`, sharing
`fork_child_teardown` with kill(2)). Closing a persistent app (chrome) no
longer leaves its children running either. Every 1 GiB boot now prints

```
[fork] child pid 1004 (task 16) outlived its run: ended, 96 MiB back to the pool
```

**Result** (`sbx2-1g.txt`): `[sbx2]` OK in 15 of 15 boots at 1 GiB: six
`combined-smp4-iommu-1g` boots of `00fb152` alone (2026-11-03) and nine
boots of the final image (six `combined-smp4`, `combined-smp1`,
`storage-smp4`, `storage-smp1`), where the base failed it in 5 of 6, in
`combined-smp1` and in `storage-smp1`. GEVFD's fork now also succeeds
(one `[linux-abi]`, `[glibc]` and `[fork]` FAILED line fewer in every 1 GiB
boot). Still failing at 1 GiB, deterministically and as on the base: GBIG
(`malloc(200 MiB)`), GSCM3's second fork and `[churn-fork]` (a 192 MiB
arena does not fit the 160 MiB process pool). Those are capacity, not
flakes.

## DMA read order (e1000d, bcm5720d, megaraidd)

The userspace drivers read DMA memory one byte at a time in ascending
order. Per core, with a host test whose HAL reads byte by byte and lets the
device write after the n-th byte:

| Driver | Finding | Change | Test (fails on the old code) |
|---|---|---|---|
| e1000d (`euroe1000`) | `receive` read the whole RX descriptor, then DD (byte 12): a write-back during the read showed DD with the old, zeroed length (bytes 8-9), and the frame was dropped as zero-length | status byte first, the descriptor after DD | `a_descriptor_written_back_during_the_read_is_not_torn` |
| megaraidd (`euromegaraid`) | the 32-bit reply-ring producer, read once per reap, could be torn (0x00FF -> 0x0100 read as 0x01FF, inside the 1008-entry ring): 256 slots the firmware never wrote consumed as stray, the real completions behind them skipped (their commands time out) | `read_index`: read until two reads agree | `a_producer_index_updated_during_the_read_is_not_torn` (old: 1 008 strays, next command lost) |
| bcm5720d (`eurobcm5720`) | the 16-bit return-ring producer can be torn the same way, but `receive` reads it again before every entry, so a torn value only takes the one entry the chip did write; descriptor after index, status tag before the drain | none | `a_torn_producer_read_takes_no_phantom_entry` (guards the property; fails if the producer is read once per drain) |
| eurodrv | no DMA access helpers | none | |

bcm5720d on the R630's own BCM5720 (VFIO `0000:02:00.0`, IOMMU group 23,
both functions back to `tg3` afterwards; eno1/eno2 and the PERC untouched),
`-nic none`, the final release image: every `[udrv-bcm-*]` marker as in
`../integrate-drivers-2026-11-02/r630-bcm-only.markers` (no marker worse),
`r630-bcm-vfio.markers`; the host's VT-d refused the malicious driver's
write and read (`r630-bcm-vfio-host-dmar.txt`).

## Build server (QEMU 10.2, TCG)

The runs of `../integrate-loadtest-2026-11-03/`, same options (`-machine
q35 -cpu qemu64,+smep,+smap`, selftest image, `-nic user,model=e1000e`,
until 120 s after the desktop loop started), two QEMUs at a time with
`nice`, each stopped by its own PID. `comparison.txt`:

| Runs | Result per marker |
|---|---|
| `user-smp{4,1}-{iommu,noiommu}` (512 MiB) | identical |
| `storage-smp{4,1}-iommu-1g`, `combined-smp1-iommu-1g`, `combined-smp4-iommu-1g` + `-r2` … `-r6` | no marker worse; `[sbx2]` FAIL -> OK where the base failed; `[fork]`, `[glibc]`, `[linux-abi]` one FAILED line fewer (GEVFD) |
| `megaraid-proof.sh` with IOMMU (1 run, 2 boots) | identical, host check identical |
| `megaraid-proof.sh` without IOMMU (7 runs) | 6 runs (12 boots) identical, host checks identical; see below |

`[lk] boot-total order inversions=0 ... task-lock-with-IF0=0` in every boot.
Host tests (`cargo test`, default members): 182 suites, 0 failures.

**Open: one hang without IOMMU.** The second boot of the first
`megaraid-proof.sh` run without IOMMU went silent right after `[net-late]`,
before the `[udrv-linux]` glibc program loaded (no fault line, 14 min until
the script's timeout; `mr-noiommu-hang-boot2-tail.txt`). It did not happen
in the 13 other boots without IOMMU (one more run was aborted by an edit of
the script it ran from and is not counted) nor in any other boot of this
proof. Nothing of this branch runs at that point in the boot besides the
e1000d receive change; not attributed. `megaraid-proof.sh` now sends an NMI
on a timeout, so a recurrence prints the kernel's NMI probe.

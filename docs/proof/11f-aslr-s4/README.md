# ASLR step S4 (docs/design/ASLR.md): the launched process at a fixed base outside the identity map

Branch `feature/11f-aslr-s4` from `feature/11f-aslr-s01` `c71cce8` (S0..S3 done). Worktree `/opt/euroos/wt-11f-aslr4` on .16. Not
pushed. R630 run directory `/srv/euroos/aslr4-runs`, CPUs 0-15 and siblings 20-35. Images built without rebuilding userland
(binaries and signatures from the main checkout). ON = `aslr.arena=fixed`, OFF = `aslr.arena=identity` (the default while the
tests ran), base = `c71cce8`. Drivers of the runs: `drivers/`.

| commit | what |
|---|---|
| `0eeb97e` | boot option `aslr.arena=identity|fixed`; the three launchers with two numbers; `paging::try_build_address_space_arena`; `LAUNCH_ARENA_PHYS` (reaper fix); `[pf-diag]` range; `[robust-paging]` covers the new builder |
| `c695d58` | `fixed` is the default (after every check below was green) |
| (docs) | design section 9, BOOT-OPTIONS, tracker BUG-088, this directory |

What changed and what the design got wrong: `docs/design/ASLR.md` section 9.

## Tests first (`tests-first.txt`)

`[aslr-phys]` (S0) looks the addresses of a glibc process up in its own page tables. Same image, KVM 4 GiB: identity -> FAILED
(exe ldso stack brk mmap at their own physical address); fixed -> OK. `[robust-paging]` gained the case `arena_zone` (23 refusals,
6 builds -> OK). Chromium with fixed: ld.so `0x80000800000`, brk and stack below 8 TiB + 128 MiB.

## Audit (design A1..A13, risks 1 and 2)

Method: grep of `ARENA_BASE`, `in_user_arena`, the copy helpers, `from_raw_parts`, `as *const/*mut` on syscall arguments and
user addresses in all kernel files, `translate_in`, `GLIBC_PML4`, `>> 39`, and the driver entry points called from syscall code;
then the widest workloads with the fixed base (every access to the arena of the launched process from another address space is a
ring-0 page fault there: 8 TiB is not mapped in the boot address space or in a native process).

| place | verdict |
|---|---|
| A1 launchers `run_glibc`, `spawn_glibc_persistent`, `glibc_disk_launch` (+ `run_glibc_disk`, `spawn_glibc_disk_persistent`) | converted: zero and load through the RAM, virtual numbers to the process |
| A2/A3 `load_elf64`, `setup_user_stack_glibc` | launchers pass (virtual, kernel side); `do_child_execve` passes equal numbers (own CR3, lazy arena): correct |
| A4 `glibc_arena_layout` | takes the virtual base; kernel side = RAM + offset |
| A5 builders | `try_build_address_space_arena`: PML4[0] = 1 GiB supervisor pages, arena under its zone slot; identity builders stay for `identity` and native programs |
| A6 `arena_split`, frees | virtual base; `free_address_space` frees the zone chain (S2); `table_frames` and the native reaper stay on PML4[0] (A12) |
| A7/A8 fault dispatch, copy-on-write | by zone (S2); counts per frame; split tables marked SPLIT_PT; children use the same virtual base |
| reaper `reap_orphan_fork_children` | **miss, fixed**: cleared the copy-on-write counts of `ARENA_BASE..+span` (virtual); now the RAM. Silent (range clamped to the table), so no failing test without injecting a leaked count |
| A9 copy helpers | always on the caller's CR3 (syscalls, ALSA ioctls, exit and clone tid writes): unchanged |
| A10 thread exit, `clear_child_tid` | in the thread's own `exit` call (`write_user` + wake): own CR3 |
| A10 signal frames | none: no signal delivery exists (`rt_sigaction` is a stub) |
| A10 futex | private keys per address space (BUG-307); shared ones keyed by the bare address: fork children share the virtual arena of the parent before and after S4; a wake never dereferences |
| A10 kill, `kill_glibc_group`, `retire_task`, tombstones | no user memory (tombstones are native background programs) |
| A10 diagnostics from the launcher: `[futex]`/`[abort]` dumps, `phys_probe_note` | through `read_glibc_u32` / `translate_in` (by frame) |
| A10 ptrace-like | none (`process_vm_*`, `ptrace` not implemented); `gdbstub` reads the address space that is loaded when it stops (debug only) |
| A10 vDSO, page cache, shared regions (`sync_shared_region`, `pcache_reclaim`), census, swap | by frame or by table walk |
| A10 X server, Wayland, clipboard, desktop loop | no raw user pointer; data comes through socket syscalls |
| A11 block | `pack_read`, `disk_read_bytes`: kernel buffer; `virtio_blk::read_io_dev`/`write_io_dev`, AHCI, NVMe: own DMA buffers; mmap of a disk file: `pack_read` into a kernel buffer, then copied |
| A11 audio | ALSA `writei`: `copy_from_user` -> `hda::pcm_write` into the DMA ring; ALSA mmap maps the ring frames into the process |
| A11 network | socket calls copy; `virtio_net` RX buffers belong to the driver |
| A11 USB | no path from a syscall to a transfer buffer (`xhci::poll` only) |
| A11 userspace drivers | native, DMA pages identity on purpose (A12) |
| A12 native (musl) programs | out of scope, unchanged |
| A13 `demand_addr_origin`, `mm_fail`, census arena size, fork log | virtual offsets or sizes: correct; `[pf-diag]` now also counts arena-zone code pointers |

IOMMU (risk 2): the TCG `*-iommu` legs with fixed show only the fault records that the malicious-driver self-tests provoke
(`[udrv-dma]`, `[mr-dma]`, `[udrv-blk-dma]`), as with identity (`tcg-matrix.txt`).

Ring-0 faults with the fixed base in all workloads below: **0**.

## Results (`0eeb97e`, ON and OFF)

| check | ON (`fixed`) | OFF (`identity`) | file |
|---|---|---|---|
| cargo test (default-members) | 188 suites, 1387 passed, 0 failed | same | `cargo-test-summary.txt` |
| KVM self-test 1/2/4/8 GiB, 2 vCPU, HD-Audio | 4/4 desktop, 0 crash; markers-compare vs `c71cce8`: **0 worse**, only change `[aslr-phys]` FAILED -> OK | 4/4, **0 worse, 0 changed** | `kvm-markers-compare.txt` |
| layout with `aslr=off`, 2 GiB | | 168 address lines (`[aslr]` first addresses, spawn lines) identical to `c71cce8` | `kvm-markers-compare.txt` |
| TCG matrix (10), restrict=on, smep+smap | **10/10 desktop**, 0 watchdog, 0 NMI, 0 crash; vs OFF 0 worse | 10/10, 0/0/0 | `tcg-matrix.txt` |
| IOMMU on (6 TCG `*-iommu` legs + 2 extra boots) | only the deliberate fault records of the self-tests | same | `tcg-matrix.txt` |
| q35 metal matrix, KVM | 15/15 legs PASS (base nvme ahci e1000e hda usb usbhub usbaudio usbnet hwprobe scan nvmeroot ahciroot power printer); tpm/tpmcrb: no swtpm | 15/15 (scan after a rerun) | `q35-matrix.txt` |
| gui-smoke, public image, 4 vCPU 4 GiB | **67/67** | 67/67 | `gui-smoke-installs.txt` |
| 60 short TCG boots to `[fuzz]`, both siblings busy | **60/60**, 0 FAILED, 0 crash | 60/60 | `short-boots.txt` |
| installs: wizard; ro-install plain/inject/ro0 | 15/15; 6/6 3/3 5/5 | same | `gui-smoke-installs.txt` |
| Chromium 20 starts, smp 2 | **20/20**, 0 FROZEN, 0 aborts | 20/20 | `chrome.txt` |
| sites, 3 rounds (YouTube banner rejected by a click) | **36/36** | 35/36, rerun 36/36 (below) | `chrome.txt` |
| interaction | **18/18** | 18/18 | `chrome.txt` |
| local VP9+Opus with sound | 6 of 6 rounds: picture 28/28, sound 12/12 windows | same | `chrome.txt` |
| chrome-mem-soak 1 h, 8 GiB | 6 rounds, 99/99 steps, **0 aborts, 0 ring-0 faults**, 0 FROZEN, 0 `[oom]`, 0 refused forks | same | `chrome.txt` |
| loadtest soak 1 h, 16 GiB, 4 vCPU | 5 583 commands, 0 errors, **0 watchdog, 0 panic**, `[churn-proc]` 0 FAILED of 4 117 | 5 489, 0, 0, 0 of 4 048 | `loadtest-soak.txt` |
| default after `c695d58` (no option) | `aslr.arena=fixed`, `[aslr-phys]` OK, 0 worse | `aslr.arena=identity`: as before | `kvm-markers-compare.txt` |

`[oom]` 3 in each loadtest soak = the `[memt]` self-test. 512 MiB TCG legs: "no arena for glibc" in both modes (no contiguous
64 MiB), as before.

## Deviations

- OFF sites, first run: one step failed (`github`, "slowest console answer 1023 ms") while 120 TCG boots loaded the host; the
  identity path is the code of `c71cce8`. Rerun on a quiet host: 36/36.
- q35: both matrices first ran at the same time; the scan leg of OFF failed on the shared mock-server port. scan and printer
  were run again one after the other: PASS in both.
- `chrome-smoke --only localvideo` refused a clip directory without `h264.mp4`; the VP9+Opus clip with sound is measured in the
  two 1 h soaks (one clip per round).
- The reaper miss has no failing-first test (silent safety-net path, see the audit table).
- `ChildMem` does not carry an `ARENA_PHYS` (design 3.2): see design section 9.
- The first build ran `userland/build.sh`, which rebuilt three driver binaries and re-signed their git-ignored signatures;
  they were restored from the main checkout and every tested image was built without the userland step.

## Default

`fixed` is the default (`c695d58`): every check is green with it, the OFF path keeps the old layout, and
`aslr.arena=identity` is the way back.

## What S5 needs

Design section 9, last paragraph: draw the arena base in `arena_virt_for` and in `do_child_execve` (order of 2.4), page slides
for exe and ld.so; then `[aslr]` can turn OK.

## New bugs

None (BUG-360..369 not used).

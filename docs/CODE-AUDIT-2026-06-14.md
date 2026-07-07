# EuroOS Code Audit — 2026-06-14 (Sprint 1+2 storage/finish-cores)

**Scope.** The code added in Phase-3 Sprint 1+2: exFAT write (`euroexfat`), ext2 write
(`euroext`), CoW TRIM + deferred reclaim + discard plumbing (`eurofs`, `eurofde`), EuroFS
symlinks, the block-cache live-root wiring, swap auto-evict (`swapmgr`), USB auto-mount
(`fatmount`/`xhci`), virtio `DISCARD` (`virtio_blk`), and the new shell/coreutils commands.

**Method.** Five independent adversarial reviewers, one per area, each required to
*hand-verify* every finding against the real code path (the prior audit had 3 scanner
false-positives; this run reports none). The TRIM-vs-rollback and swap-frame-lifetime
concerns were traced with concrete commit/eviction sequences. Findings ranked
CRITICAL/HIGH/MEDIUM/LOW; fixes applied and regression-tested where marked **FIXED**.

## Findings

| # | Sev | Area | Issue | Status |
|---|-----|------|-------|--------|
| 1 | **CRITICAL** | euroext | `write_file`/`create_dir` left an **orphaned inode + leaked blocks** when `dir_insert` fails on a full extent-mapped parent dir (the normal `mkfs.ext4` case once a dir block fills). Empirically reproduced; `e2fsck` reported unattached inodes. | **FIXED** — roll back the inode + data blocks on `dir_insert` failure; regression test `dir_insert_failure_rolls_back_no_orphan` |
| 2 | HIGH | euroexfat | Overwriting a file **leaked the old cluster chain** (old clusters stayed allocated, unreferenced) → monotonic space leak. | **FIXED** — overwrite now `free_chain`s the old first cluster; regression `overwrite_frees_old_chain_no_leak` |
| 3 | HIGH | eurofs | `write_file` on a path whose final component is a **symlink** wrote a `TYPE_FILE` inode to the symlink's OID but left the dir entry kind `Symlink` → entry/inode type desync → path became permanently unresolvable. | **FIXED** — `write_file` now follows the symlink (POSIX), depth-bounded vs loops; regression `write_through_symlink_writes_target_not_corrupt` |
| 4 | HIGH | eurocoreutils | `shuf -i LO-` (open-ended range → `usize::MAX`) tried to materialise ~1.8e19 strings → OOM/hang, reachable from shell/pipeline input. | **FIXED** — range capped at 1,000,000 items; empty when LO>HI |
| 5 | MEDIUM | fatmount | `UsbDev` truncated the absolute LBA `u64→u32` → wrong-sector R/W (silent corruption) on >2 TiB USB disks. | **FIXED** — bounds-reject LBA > `u32::MAX` in read/write + skip mounting >2 TiB at detect |
| 6 | MEDIUM | fatmount | `usb_auto_mount` **wrote `/EUROOS.TXT` to the user's USB stick on every boot** — unconsented mutation of removable media (sovereign-OS policy violation). | **FIXED** — USB volume now auto-mounts **read-only at boot** (no writes); FAT write still available via host tests + explicit shell ops |
| 7 | MEDIUM | fatmount/xhci | The entire FAT mount+write+read ran inside one `without_interrupts` → unbounded interrupts-off window (timer drift / missed IRQs on real HW). | **FIXED** — masking moved to per-BOT-transfer inside `usb_read_block`/`usb_write_block` (bounded), removed the whole-mount mask |
| 8 | MEDIUM | shell | `comm`/`join` shell arm split tokens on `-` prefix, so value-options (`-1 N`, `-t C`) were misfiled as files and silently dropped → wrong output. | **FIXED** — inputs are the last two readable-file tokens; everything else passes through as args |
| 9 | MEDIUM | eurocoreutils | `split -b NNNg` overflowed `usize` multiply → panic (debug) / silent wrap (release). | **FIXED** — `checked_mul` saturating to `usize::MAX` |
| 10 | MEDIUM | swapmgr | Swap reserve accounting is not invariant-bound: a direct `swap_out` parks a frame in `pool` forever, and pool exhaustion → `try_swap_in` returns false → PF handler halts (lost page). Steady-state safe; no hard invariant. | **DEFERRED** — documented; needs a `try_swap_in` fallback to the global allocator. Not reachable in current wiring (BSP-only, reserve held) |
| 11 | LOW | euroexfat | `dir_find_slot` grows a directory by only one cluster; an entry-set larger than one cluster (512 B clusters + 241+ char name) fails with `Corruption` after side effects (leak). Narrow trigger. | **DEFERRED** — documented; loop the growth |
| 12 | LOW | euroexfat | `cluster_first_sector` does unchecked u32 arithmetic on a cluster number read from disk → wraps on corrupt media (release has no overflow checks). | **DEFERRED** — add `cluster_valid()` guard |
| 13 | LOW | virtio_blk | `discard_dev` lacked the `sector+count ≤ capacity` bounds check that read/write have. | **FIXED** |
| 14 | LOW | virtio_blk / swapmgr | Lockless `&'static mut` device access + lockless PTE walks — safe **only** because APs are parked (no concurrent storage/MM mutators). | **DEFERRED (documented)** — must add a Mutex + TLB-shootdown before SMP user scheduling |
| 15 | LOW | shell | `mktemp` 32-bit name space → same-tick collision can overwrite; `realpath` relative-target join rooted at `/`; `split` suffix past `zz` diverges from GNU. | **DEFERRED (cosmetic)** |

## Verified SAFE (explicitly traced, no change needed)

- **CoW TRIM vs A/B rollback (the highest-stakes item).** The one-generation TRIM deferral
  (`pending_trim`) was traced against a `format → write A → write B → snapshot → remove →
  crash` sequence: blocks discarded at commit N are those freed at commit **N-1** (already
  not reachable from the N-1 rollback fallback), snapshot-pinned blocks are excluded
  (`mark_state_blocks` runs before the trim emit), and a `!used[b]` re-check guards CoW
  reuse. No committed, rollback-fallback, or snapshot block can ever be discarded/zeroed.
  All 89 eurofs tests incl. the fault-injection crash sweep pass.
- **Swap frame double-free / use-after-free.** The frame `auto_evict` returns to the global
  allocator is the just-evicted, now-non-present frame (LIFO top of `pool`); `FrameAllocator`
  also detects double-frees. No UAF.
- **virtio DISCARD descriptor chain + capacity-read ordering.** Chain hdr(read)→data(read)→
  status(write) is well-formed; capacity is read before MSI-X enable; adding `F_DISCARD`
  doesn't shift legacy device-config.
- **Block-cache root type.** No code assumes the concrete `RootBlk` type for the cached root;
  it's erased to `Box<dyn FileSystem>`. The separate uncached `/mnt` (`fs2`) is correct.
- **Hash implementations** (SHA-1/MD5/BLAKE2b-512): padding/length/endianness correct vs RFC
  vectors; no reachable overflow.
- **Symlink loop guard** effective for absolute, relative, and self-referential-dir-intermediate
  shapes (monotonic `hops` ≤ 40 → `InvalidPath`).

## Net

15 findings: **1 CRITICAL + 3 HIGH + 5 MEDIUM fixed** (with regression tests), 1 LOW fixed,
and 5 LOW/architectural deferred-with-documentation. The two genuinely high-stakes
correctness questions (CoW TRIM safety, swap frame lifetime) were verified safe by tracing.
Host tests after fixes: eurofs 89, euroext 11, euroexfat 10, eurocoreutils 45 — all green.

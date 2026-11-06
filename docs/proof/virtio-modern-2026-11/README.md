# virtio 1.x (modern, non-transitional) for Kyvernon VMM (2026-11-05/06)

Branch `feature/virtio-modern`, rebased on `feature/app-control` at `ce04f9e`
(net worker). Not merged.

| Commit | What |
|---|---|
| `07285b5` | `crates/eurovirtio`: capability walk, feature negotiation, ring layout, queue size, notify address, net header length, virtio-input decoder; 15 host tests |
| `8442aec` | `virtio_pci.rs` shared transport (modern + legacy fallback); virtio-blk, virtio-net, virtio-gpu, virtio-snd on it; new virtio-input; high-MMIO window (`paging::mmio_virt`); MSI-X table through it, memory decoding on |
| `6087e93` | LAPIC timer calibration without a PIT: HPET, then ACPI PM timer |
| `caca08f` | `[hpet] HPET active @ ...` (the literal Kyvernon's test looks for) |
| (last) | docs, this proof |

Serial logs stay on the build servers; the `.markers` files hold the verdict
lines, the `.txt` files the excerpts that carry the argument.

## What was there before

- virtio-blk and virtio-net: legacy only (I/O BAR 0, `QUEUE_PFN`, 32-bit
  features). The PCI ids 1041/1042 were matched, but a modern-only device has
  no I/O BAR: `[blk] 0 virtio-blk disk(s)`, `[net] virtio-net NIC not found`
  (`tcg-config-excerpts.txt`, `base2-modern`). virtio-net only looked at bus 0,
  function 0.
- virtio-gpu and virtio-snd: each its own copy of a modern handshake
  (VERSION_1 only, 64-bit stores to the queue address registers).
- `pci::virtio_cap` (capability lookup) existed, no virtio-input driver.
- MSI-X tables and virtio BARs were used through the identity map, which ends
  at 1 TiB. OVMF with `-cpu host` (R630: 46 physical bits) puts the 64-bit
  aperture at `0x380000000000` (56 TiB).
- The LAPIC timer was calibrated against PIT channel 2 only. Without a PIT:
  `calibrated: 6450 ticks/period` instead of ~630 000 (`base2-pitoff`), i.e.
  a tick about 100 times too fast.
- PCI config: ECAM only after a verified MCFG, otherwise `0xCF8/0xCFC`; that
  already worked without MCFG.

## Design

`kernel/src/virtio_pci.rs` (decisions in `crates/eurovirtio`, written from
the OASIS virtio 1.x spec, no code from other systems):

- `Transport::probe`: vendor capabilities common/notify/ISR/device (first of
  each, BAR ≤ 5, common ≥ 0x38 bytes) → modern; else legacy I/O BAR 0.
  Transitional devices take the modern path; `virtio.transport=legacy` in
  `euroos-boot.conf` keeps the old one.
- Handshake: reset and wait for 0, ACKNOWLEDGE, DRIVER, 64-bit features via
  `feature_select`, accept `offered & wanted` (VERSION_1 mandatory; EVENT_IDX,
  INDIRECT_DESC, packed never), FEATURES_OK read back (FAILED otherwise),
  queues, DRIVER_OK.
- Queues: one contiguous split ring (legacy layout, the modern transport
  accepts it), size = min(device, 256) as a power of two, desc/driver/device as
  two 32-bit writes, notify = `notify_base + queue_notify_off × multiplier`.
- Device config generation-checked; 64-bit fields under one generation.
- MSI-X: entry 0 for the queue that needs it, config vector NO_VECTOR,
  read-back logged. Without MSI-X: polling (all drivers confirm completions on
  the used ring anyway). INTx is not routed (needs `_PRT`).
- BARs above 1 TiB: `paging::mmio_virt` maps them into the last GiB of the
  shared high PDPT (uncached 2 MiB pages, valid under every CR3). Below 1 TiB
  nothing changes.

Per device:

| Device | Status | Features accepted |
|---|---|---|
| virtio-blk 1042 / 1001 | ✅ modern + legacy, boot disk (FAT image) and data disk (EuroFS `/mnt`), FLUSH | VERSION_1, FLUSH, DISCARD, RO (writes refused) |
| virtio-net 1041 / 1000 | ✅ modern (12-byte `virtio_net_hdr_v1`) + legacy (10 bytes); DHCP, DNS, HTTP; net worker woken by its MSI-X | VERSION_1, MAC, STATUS. No CSUM/TSO/UFO/ECN, no MRG_RXBUF, no MQ/CTRL_VQ |
| virtio-input 1052 | ✅ new: tablet (ABS_X/ABS_Y 0..32767 from ABS_INFO → cursor), keyboard (evdev → set-1 scancodes), polled per tick | VERSION_1 |
| virtio-gpu 1050 | transport switched, function unchanged (2D scanout) | VERSION_1 |
| virtio-snd 1059 | transport switched, function unchanged (control queue only) | VERSION_1 |
| virtio-console, rng, balloon | not driven | — |

`net.driver=user` (the default) only concerns an e1000: `nic::init` tries
virtio-net first, so a machine with virtio-net and no e1000 runs the kernel
virtio-net driver (`[net] NIC: virtio-net`), as before.

## Build server .16 (QEMU 10.2, TCG)

`tcg-virtio-boot.sh`: `-machine q35 -cpu qemu64,+smep,+smap -m 1G -smp 2`,
OVMF, boot image on virtio-blk (`bootindex=0`), a 64 MiB data disk, virtio-net
on user-net, virtio-tablet + virtio-keyboard, `-vga std`; `MODE=modern`
(`disable-legacy=on` everywhere), `trans` (QEMU default), `legacy`
(`disable-modern=on`, no input devices). Selftest images: branch `6087e93`,
base `ce04f9e`.

| Run | Result |
|---|---|
| modern | desktop; `524288 sectors`, FLUSH, MSI-X read-back 0, `[j2-blk]` completions, `/mnt` EuroFS written and read back (`[g2]`); virtio-net 12-byte header, DHCP ACK; `[net-worker]` OK; vs base transitional: 0 worse, only new `[vinput]` |
| input + network, typed | QMP `input-send-event` (abs): cursor at exactly 99,99 / 1799,999 / 959,539; `help` and `fetch example.com` typed on the virtio-keyboard into the terminal: `HTTP/1.1 200 OK` (`tcg-*-fetch*.png`, `tcg-rebased-modern-desktop.png`) |
| transitional | modern transport used; 0 worse vs base |
| transitional + `virtio.transport=legacy` | legacy transport, 10-byte header; 0 worse |
| legacy (`disable-modern=on`) vs base legacy | 0 worse, 0 changed |
| modern, `vectors=0` (no MSI-X) | polled; desktop, DHCP; only `[j2-blk]` (MSI-X count) absent |
| modern, `-machine q35,pit=off` | `calibrated: 634514 ticks/period against HPET (no PIT)`; 0 worse |
| modern, `pit=off,hpet=off` | calibrated against the ACPI PM timer (637 078); `[net-async] connect ... bound 6 s FAILED`: that self-test reads time from the HPET (0 us everywhere without one) |
| public image (`build.sh image`), modern + `pit=off` | desktop, keys and tablet (`tcg-public-image-pitoff-desktop.png`) |

`comparison-tcg-virtio.txt`, `tcg-config-excerpts.txt`,
`tcg-rebased-modern-excerpt.txt`.

### Selftest matrix (`tcg-boot.sh`, e1000e, AHCI boot disk)

10 boots (user/kernel/storage/combined, smp4/smp1, ± IOMMU) against
`../net-worker-2026-11/`: **8 of 10 to the desktop with 0 markers worse, 0
changed; 2 hung** (`user-smp4-iommu`, `user-smp1-noiommu`; their "only in
base" markers are the ones after the hang). `[lk] boot-total order
inversions=0` in every boot that finished.

Both hangs are the same place (`tcg-hang-*-nmi.txt`): the NMI probe finds a
musl process (task 10) in `ring3::pipe_write_fd` spinning on `PIPE_FDS` with
IF=0 (RFLAGS `0x50002`), while the boot task is Ready. `pipe_write_fd` takes
`PIPE_FDS` under an `IfOffGuard` (since 2026-08-31), other paths
(`pipe_fd_exists`, dup, close, …) take it with interrupts on: a holder
preempted by the tick, then a spinner with IF=0 on the same core, is a
deadlock of the BUG-007 class (BUG-037 in the tracker). It is not in the code this branch changes, and
not specific to it: 3 more boots of each of the two configurations on the
branch and on the base, four at a time, all reached the desktop with 0 worse
(`tcg-rerun-*`). The two hangs came while the build server ran up to six
QEMUs at once. Reported as open below.

`megaraid-proof.sh` with IOMMU ×2, without ×2 (8 boots): 0 worse
(`comparison-mr.txt`), host checks 4/4.

## Dell R630 (KVM, `-cpu host`)

The branch as a git bundle in `/srv/euroos/wt-virtio`, built with the host's
throwaway dev key (signatures re-signed locally, not committed). VMs pinned to
their own 4 CPUs, SLIRP `restrict=on` with a `guestfwd` HTTP peer at
10.0.2.100, no VFIO. Base = the net-worker tree's image (`77aeec0`, code equal
to `ce04f9e`).

| Run | Result |
|---|---|
| modern | `[paging] high MMIO 0x380000000000+0x1000 (above the 1 TiB identity map) → window 0xffc0000000`; blk/net/input on the window; desktop after 154-159 s; tablet exact, `fetch 10.0.2.100` typed on the virtio-keyboard: `HTTP/1.0 200 OK` (`r630-modern-fetch-typed.png`, host hit log); `[net-worker]` OK |
| transitional, legacy, modern + `pit=off` (`against HPET (no PIT)`), transitional + `virtio.transport=legacy` | 0 worse vs base; `[appstore]`/`[upd3]` better only because the base image's pre-signed artifacts do not match this host's key (`comparison-r630-virtio.txt`) |
| public image, modern + `pit=off` | desktop after 22 s, `524288 sectors`, `[hpet] HPET active`, tablet and keyboard events |
| `--scenario net` e1000e, `user` ×6, `kernel` ×2 | 8/8 completed clean, 67/67 commands each (`r630-net-e1000e-summary.txt`) |
| `--scenario inbound`, 4 httpd clients 300 s, virtio-net | base legacy 178 506 / 173 780 ok; branch legacy 157 448 / 170 454; branch modern 150 479 / 174 231; p50 0.9 ms, p99 60.6-60.9 ms in all six; console answered throughout (`r630-inbound-virtio.txt`). The spread is between runs, not between transports; max latency 2.1 s in 3 of 4 branch runs against 1.1 s in both base runs |

## Answers for the Kyvernon team

- **Features**: no indirect descriptors, no packed rings, no EVENT_IDX, no MQ
  (nor CTRL_VQ, MRG_RXBUF or any checksum/segmentation offload). Accepted:
  VERSION_1 everywhere; blk FLUSH (+ DISCARD, RO when offered); net MAC and
  STATUS. Split rings, one descriptor chain at a time, MSI-X per queue
  (config NO_VECTOR), polling without MSI-X.
- **virtio-gpu**: not needed. EuroOS draws into the GOP framebuffer the
  firmware hands over (Bochs/VBE 1234:1111 under OVMF) and never programs the
  Bochs registers; virtio-gpu is used only when present.
- **Guest agent**: no. There is no virtio-console driver and no agent; leave
  `org.qemu.guest_agent.0` unused for now.

## Open

- BUG-037: `PIPE_FDS` taken with and without interrupts off (the two TCG hangs above).
- INTx is not routed (no `_PRT` evaluation); without MSI-X the drivers poll,
  virtio-net receive then relies on the net worker's per-tick poll.
- `[net-async]` measures time with the HPET only; without an HPET its bound
  check fails.
- Not exercised: a Kyvernon guest itself, Secure Boot, the AHCI cd-rom path,
  virtio-console/rng/balloon, a notify multiplier of 0 on real hardware (host
  test only), MSI-X tables in BAR 0 beside the virtio regions (QEMU puts them
  in BAR 1).
- Mouse wheel events of a virtio mouse/tablet are decoded but not delivered
  (the mouse layer has no wheel input).

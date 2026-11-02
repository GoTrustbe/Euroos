# The four userspace drivers merged: boot proof (2026-11-02)

Branch `feature/integrate-drivers`: `feature/app-control` at `a590c7f`
(e1000d by default, nvmed opt-in) with `feature/udrv-bcm5720` (bcm5720d) and
`feature/udrv-megaraid` (megaraidd) merged, then one vector pool and one
driver-syscall dispatch for all four (`kernel/src/udrv.rs`), and a fix in
`crates/euronvme` found while testing (below). What changed and why:
`docs/EUROOS-DEEP-TECHNICAL-REFERENCE.md`, "One layer for every userspace
driver".

Full serial logs are kept next to this file on the build server (`*.log` is
not committed); the `.markers` files are the driver lines of those logs.
`scripts/markers-compare.py` makes the per-marker comparisons.

## Build server (QEMU 10.2, TCG)

`-machine q35 -cpu qemu64,+smep,+smap`, selftest image (`scripts/build.sh
release`), `-nic user,model=e1000e`, one QEMU at a time, each boot until 120 s
after `[desktop] interactive loop started`.

| Log | Run | Result |
|---|---|---|
| `user-smp4-iommu.log` | defaults, 512 MiB, `-smp 4`, `-device intel-iommu,intremap=off` | desktop; every `[udrv-*]` OK, `[udrv-linux]` ⬜ (no glibc arena at 512 MiB) |
| `user-smp1-iommu.log` | the same, `-smp 1` | the same |
| `user-smp4-noiommu.log`, `user-smp1-noiommu.log` | defaults, 512 MiB, no IOMMU | the same; `[udrv-iommu]`/`[udrv-dma]` "not confined" ⬜ |
| `storage-smp4-iommu-1g.log`, `storage-smp1-iommu-1g.log` | `storage.nvme=user`, 1 GiB, IOMMU, a blank 64 MiB NVMe data disk | desktop; every `[udrv-blk-*]` OK |
| `combined-smp4-iommu-1g.log`, `combined-smp1-iommu-1g.log` | as storage, plus `-device megasas-gen2` with two logical drives (`-fw_cfg name=opt/ovmf/X-PciMmio64Mb,string=0`) | desktop; e1000d, nvmed and megaraidd in one boot, every `[udrv-*]`, `[udrv-blk-*]` and `[mr-*]` OK |
| `mr-iommu-boot{1,2}`, `mr-noiommu-boot{1,2}` (`.markers`, `-host-check.txt`) | `scripts/megaraid-proof.sh` with and without `intel-iommu` (two boots on the same disk images, then a host-side check of the images) | every `[mr-*]` OK (⬜ for `[mr-iommu]`/`[mr-dma]` without an IOMMU); verdicts identical to `../megaraid-2026-11-02/` (which ran under KVM) |

Compared per marker with `../udrv-default-2026-11-02/` (the proof of
`a590c7f`), in `comparison.txt`: no marker got worse in any run. New: `[udrv-vec]`
everywhere, the `[mr-*]` markers in the combined runs. `[sbx2]` failed in the
base's 1 GiB storage runs and passed in two of the four 1 GiB runs here. The
only `FAILED` lines are those of the base logs: at 512 MiB the glibc tests
without an arena (`[app]`, `[churn-proc]`, `[churn-thread]`, `[churn-fork]`,
`[app-policy]`); at 1 GiB `[linux-abi]`, `[glibc]`, `[fork]`, `[churn-fork]`
(and `[sbx2]` in the `-smp 1` runs), the same set as the base's 1 GiB storage logs. `[lk]`
0 order inversions and 0 task locks with IF=0 in every run; `[kwork]` OK in
the `-smp 4` runs. Every run reached the desktop; the chrome_crashpad stop of
the base proof did not come back.

Host tests of the whole workspace: green (`cargo test`), including the new
`eurodrv::vectors` tests.

```
[udrv-vec] MSI-X vectors from the pool 0x4c..0x4f: e1000d 0x4c, nvmed 0x4e, megaraidd 0x4d; distinct=true, stubs bound to their owner=true, second claims on a held vector refused 9/9; unowned interrupts 0 → OK ✓
```

## Dell R630 (KVM, VFIO)

The merged tree carried there as a git bundle and built on the host (its
throwaway dev key makes the tests of pre-signed artifacts fail there:
`[3e6]`, `[3f1]`, `[app]`, `[app-policy]`, `[appstore]`, `[churn-*]`,
`[upd3]`, and `[udrv-linux]`, whose `/bin/gnet` is refused by verify; the
same set as in the earlier R630 comparisons). Only `0000:02:00.0`/`02:00.1`
(eno3/eno4, IOMMU group 23) were moved to `vfio-pci`, and back to `tg3`
afterwards.

```
qemu-system-x86_64 -machine q35,accel=kvm -cpu host,host-phys-bits-limit=39 -m 1024 -smp 2 \
  -bios /usr/share/ovmf/OVMF.fd -drive format=raw,file=eurokernel.img \
  -device intel-iommu,intremap=off,caching-mode=on -device vfio-pci,host=0000:02:00.0 ...
```

| Log | Run | Result |
|---|---|---|
| `r630-bcm-only.log` | the branch proof's line (`-nic none`) | every `[udrv-bcm-*]` marker as in `../udrv-bcm5720-2026-11-02/` (`[udrv-bcm-net]` ⬜: no cable); `[udrv-vec]`: bcm5720d alone gets `0x4c`; host VT-d: `r630-bcm-only-host-dmar.txt` |
| `r630-all-four-1.log`, `r630-all-four-2.log` | plus `-nic user,model=e1000e`, a blank NVMe data disk with `storage.nvme=user`, `-device megasas-gen2` with two logical drives, 1.5 GiB | all four drivers in one boot, every `[udrv-*]`, `[udrv-bcm-*]`, `[udrv-blk-*]` and `[mr-*]` marker OK (bcm5720d's link ⬜), to the desktop |

```
[udrv-irq] MSI-X vector 0x4c: 16 interrupt(s) raised, 16 delivered to e1000d as EuroIPC Irq messages → OK ✓
[udrv-bcm-irq] MSI-X vector 0x4d: 1 interrupt(s) raised by the chip, 1 delivered to bcm5720d as EuroIPC Irq messages → OK ✓
[mr-irq] MSI-X vector 0x4e: 58 interrupt(s) raised by reply-queue completions, 58 delivered to megaraidd as EuroIPC Irq messages → OK ✓
[udrv-blk-irq] MSI-X vector 0x4f: 36 completion interrupt(s) raised, 36 delivered to nvmed as EuroIPC Irq messages → OK ✓
[udrv-vec] MSI-X vectors from the pool 0x4c..0x4f: e1000d 0x4c, nvmed 0x4f, bcm5720d 0x4d, megaraidd 0x4e; distinct=true, stubs bound to their owner=true, second claims on a held vector refused 12/12; unowned interrupts 0 → OK ✓
```

Host side, the same boot (`r630-all-four-1-host-dmar.txt`): the R630's own
VT-d refused the malicious bcm5720d's write to the canary and read of the
secret page.

```
DMAR: [DMA Write NO_PASID] Request device [02:00.0] fault addr 0x1a2000 [fault reason 0x05] PTE Write access is not set
DMAR: [DMA Read NO_PASID] Request device [02:00.0] fault addr 0x1a3000 [fault reason 0x06] PTE Read access is not set
```

## Found and fixed: nvmed's torn completion read

Before the `euronvme` fix (tree `d8fb405`) nvmed instances stopped with
`controller init failed: Unexpected`: in `incident-nvmed-init-storage-smp4-iommu-1g.log`
(TCG) the first instance of the boot and the first one after the crash test,
in `incident-nvmed-init-r630-all-four.log` (KVM) the first two after the
crash test. The supervisor restarted them and the markers still passed.
nvmed reads DMA memory a byte at a time in ascending order; a completion
posted during that read could show the new phase bit with the old command id.
`euronvme` now reads the phase half of the entry first. Every run in the
tables above is on the fixed tree; none of them has an nvmed init failure.

## Open

- e1000d, bcm5720d and megaraidd read DMA memory byte by byte too. Whether
  each core reads an entry's ready bit (descriptor done, status tag, reply
  context) before the rest of the entry was not audited in this merge.
- nvmed and megaraidd keep two block interfaces (see
  `docs/SPRINT-PLAN-USERSPACE-STORAGE.md`, "Two block interfaces").
- bcm5720d runs its destructive self-tests (the DMA escape and the crash)
  whenever it starts; e1000d does so only in the selftest image.
- The vector a driver gets depends on the boot order; nothing outside the
  log depends on the number.

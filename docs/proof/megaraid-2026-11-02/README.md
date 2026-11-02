# Userspace MegaRAID: boot proof (2026-11-02)

Serial-log excerpts (the `[mr*]`, `[megaraidd ...]`, `[mrblk]` lines and the
relevant `[iommu]` lines) from QEMU 10.2 under KVM on the Dell R630 test host,
`-machine q35 -m 1024M -smp 4 -enable-kvm -cpu host`, `-device megasas-gen2` with
two `scsi-hd` logical drives (64 MiB and 256 MiB raw images),
`-fw_cfg name=opt/ovmf/X-PciMmio64Mb,string=0`, `-nic user,model=e1000e`.
Produced with `scripts/megaraid-proof.sh` (two boots on the same disk images,
then a check of the image files from the host).

- `iommu-boot1.markers`, `iommu-boot2.markers`, `iommu-host-check.txt`: with
  `-device intel-iommu,intremap=off`, image built from a fresh clone of the branch. Boot 1 formats EuroFS on LD1 and writes
  generation 1; boot 2 reads generation 1 back and writes generation 2.
- `no-iommu-boot1.markers`, `no-iommu-boot2.markers`, `no-iommu-host-check.txt`:
  the same without an IOMMU; `[mr-iommu]` and `[mr-dma]` report "not confined" ⬜.
- `with-e1000d-user-mode.markers`: one boot with `net.driver=user`, `e1000d` and
  `megaraidd` side by side (fresh disk images).
- `suite-comparison.txt`: the FAILED markers of a full dev self-test boot of the
  base commit `813e627` and of this branch (built from a fresh clone), same
  host, same QEMU line, same controller attached. Identical sets; they are the tests that depend on
  pre-signed artifacts (a throwaway dev key is used here) and the glibc
  churn/app-policy tests, unrelated to this work.

See `docs/SPRINT-PLAN-MEGARAID.md`.

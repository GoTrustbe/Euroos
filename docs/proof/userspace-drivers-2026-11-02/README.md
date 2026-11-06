# Userspace e1000: boot proof (2026-11-02)

Serial-log excerpts (lines matching the `[udrv*]`, `[e1000d]`, `[bootopt]` and
relevant `[iommu]` markers) from QEMU 10.2 under TCG, `-machine q35 -m 512M
-cpu qemu64,+smep,+smap`, `-nic user,model=e1000e`:

- `user-mode-intel-iommu.log`: `net.driver=user`, `-device intel-iommu,intremap=off`.
- `user-mode-no-iommu.log`: `net.driver=user`, no IOMMU (DMA not confined, reported as such).
- `default-kernel-driver.log`: no option file: the in-kernel e1000 binds as before.

See `docs/SPRINT-PLAN-USERSPACE-DRIVERS.md`.

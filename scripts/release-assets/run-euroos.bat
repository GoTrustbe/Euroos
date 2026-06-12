@echo off
REM ==========================================================================
REM  EuroOS - preview launcher (Windows)
REM  Boots the EuroOS preview in QEMU with networking + a persistent disk.
REM  Requires QEMU for Windows: https://www.qemu.org/download/#windows
REM  (add the QEMU folder to PATH, or run this from that folder).
REM ==========================================================================
cd /d "%~dp0"

where qemu-system-x86_64 >nul 2>nul
if errorlevel 1 (
  echo QEMU not found. Install QEMU for Windows from https://www.qemu.org/download/
  echo and make sure qemu-system-x86_64.exe is on your PATH.
  pause
  exit /b 1
)

if not exist euroos-disk.qcow2 qemu-img create -f qcow2 euroos-disk.qcow2 512M

echo Starting EuroOS...  (close the QEMU window to stop; delete euroos-disk.qcow2 to reset)
REM -accel whpx uses Windows Hypervisor Platform (enable it in Windows Features
REM "Windows Hypervisor Platform"). Remove "-accel whpx -cpu max" to use software emulation.
qemu-system-x86_64 ^
  -machine q35 -m 512M -accel whpx -cpu max ^
  -bios OVMF.fd ^
  -drive format=raw,file=euroos.img ^
  -drive format=qcow2,file=euroos-disk.qcow2,if=none,id=hd0 ^
  -device virtio-blk-pci,drive=hd0,disable-modern=on ^
  -netdev user,id=n0,ipv4=on -device virtio-net-pci,netdev=n0,disable-modern=on ^
  -name "EuroOS preview"
pause

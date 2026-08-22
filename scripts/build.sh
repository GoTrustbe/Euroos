#!/usr/bin/env bash
# Build the EuroKernel UEFI binary and pack it into a bootable FAT32 image.
set -euo pipefail
cd "$(dirname "$0")/.."

PROFILE="${1:-release}"
EFI="target/x86_64-unknown-uefi/${PROFILE}/eurokernel.efi"
IMG="eurokernel.img"

echo "==> rustc: $(rustc --version)"
echo "==> EuroToolchain: compiling userspace programs (Track 6)"
./userland/build.sh >/dev/null
if [ "$PROFILE" = "image" ]; then
  # Public download/VNC image: no self-test suite -> fast boot to an idle desktop.
  PROFILE="release"
  EFI="target/x86_64-unknown-uefi/release/eurokernel.efi"
  cargo kbuild-image
  cargo lbuild-release           # G4: two-stage loader
elif [ "$PROFILE" = "chrome" ]; then
  # Iteration image: chrome runs in the boot phase (see the chrome-boot feature).
  PROFILE="release"
  EFI="target/x86_64-unknown-uefi/release/eurokernel.efi"
  cargo kbuild-chrome
  cargo lbuild-release
elif [ "$PROFILE" = "release" ]; then
  cargo kbuild-release
  cargo lbuild-release           # G4: two-stage loader
else
  cargo kbuild
  cargo build -p loader --target x86_64-unknown-uefi -Z build-std=core,compiler_builtins,alloc -Z build-std-features=compiler-builtins-mem
fi
[ -f "$EFI" ] || { echo "ERROR: $EFI not found"; exit 1; }
LOADER="target/x86_64-unknown-uefi/${PROFILE}/loader.efi"
[ -f "$LOADER" ] || { echo "ERROR: $LOADER not found"; exit 1; }
echo "==> kernel: $(du -h "$EFI" | cut -f1) · loader: $(du -h "$LOADER" | cut -f1)"

echo "==> building FAT32 image ($IMG) via mtools (no root needed)"
dd if=/dev/zero of="$IMG" bs=1M count=256 status=none
mkfs.fat -F 32 -n EUROKERNEL "$IMG" >/dev/null
mmd -i "$IMG" ::/EFI ::/EFI/BOOT
# G4 TWO-STAGE: BOOTX64.EFI = loader; the kernel sits as slot image A and B.
mcopy -i "$IMG" "$LOADER" ::/EFI/BOOT/BOOTX64.EFI
mcopy -i "$IMG" "$EFI" ::/EFI/BOOT/eurokernel-A.efi
mcopy -i "$IMG" "$EFI" ::/EFI/BOOT/eurokernel-B.efi
echo "==> done: $IMG (two-stage: loader → eurokernel-A/B.efi)"
echo "    Test:        ./scripts/run-qemu.sh"
echo "    Screenshot:  python3 scripts/screenshot.py $IMG boot.png"
echo "    To USB:      sudo dd if=$IMG of=/dev/sdX bs=4M status=progress  # lsblk first!"

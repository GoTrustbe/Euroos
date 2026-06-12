#!/usr/bin/env bash
# Bouw de EuroKernel UEFI-binary en verpak hem in een bootable FAT32-image.
set -euo pipefail
cd "$(dirname "$0")/.."

PROFILE="${1:-release}"
EFI="target/x86_64-unknown-uefi/${PROFILE}/eurokernel.efi"
IMG="eurokernel.img"

echo "==> rustc: $(rustc --version)"
echo "==> EuroToolchain: userspace-programma's compileren (Track 6)"
./userland/build.sh >/dev/null
if [ "$PROFILE" = "release" ]; then
  cargo kbuild-release
  cargo lbuild-release           # G4: twee-traps-loader
else
  cargo kbuild
  cargo build -p loader --target x86_64-unknown-uefi -Z build-std=core,compiler_builtins,alloc -Z build-std-features=compiler-builtins-mem
fi
[ -f "$EFI" ] || { echo "FOUT: $EFI niet gevonden"; exit 1; }
LOADER="target/x86_64-unknown-uefi/${PROFILE}/loader.efi"
[ -f "$LOADER" ] || { echo "FOUT: $LOADER niet gevonden"; exit 1; }
echo "==> kernel: $(du -h "$EFI" | cut -f1) · loader: $(du -h "$LOADER" | cut -f1)"

echo "==> FAT32-image ($IMG) bouwen via mtools (geen root nodig)"
dd if=/dev/zero of="$IMG" bs=1M count=64 status=none
mkfs.fat -F 32 -n EUROKERNEL "$IMG" >/dev/null
mmd -i "$IMG" ::/EFI ::/EFI/BOOT
# G4 TWEE-TRAPS: BOOTX64.EFI = loader; de kernel staat als slot-image A én B.
mcopy -i "$IMG" "$LOADER" ::/EFI/BOOT/BOOTX64.EFI
mcopy -i "$IMG" "$EFI" ::/EFI/BOOT/eurokernel-A.efi
mcopy -i "$IMG" "$EFI" ::/EFI/BOOT/eurokernel-B.efi
echo "==> klaar: $IMG (twee-traps: loader → eurokernel-A/B.efi)"
echo "    Test:        ./scripts/run-qemu.sh"
echo "    Screenshot:  python3 scripts/screenshot.py $IMG boot.png"
echo "    Naar USB:    sudo dd if=$IMG of=/dev/sdX bs=4M status=progress  # lsblk eerst!"

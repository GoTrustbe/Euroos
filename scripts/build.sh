#!/usr/bin/env bash
# Build the EuroKernel UEFI binary and pack it into a bootable FAT32 image.
set -euo pipefail
cd "$(dirname "$0")/.."

# One build at a time. Two builds share cargo's output path, so a second one
# (a manual `cargo kbuild-*`, another harness) silently replaces the .efi this one
# is about to pack — and the image then boots a kernel nobody asked for. That
# cost two full test runs before it was noticed, each time looking like a code bug.
exec 9>"${TMPDIR:-/tmp}/eurokernel-build.lock"
flock 9

PROFILE="${1:-release}"
# EuroUpdate compares this against the signed channel manifest (YYYYMMDD).
export EUROOS_BUILD_VERSION="${EUROOS_BUILD_VERSION:-$(date -u +%Y%m%d)}"
echo "==> EUROOS_BUILD_VERSION=$EUROOS_BUILD_VERSION"
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

# Stage the binaries the moment they exist: the image assembly below takes a minute,
# and until it is done these paths must not change underneath it.
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
cp "$EFI" "$STAGE/kernel.efi"
cp "$LOADER" "$STAGE/loader.efi"
EFI="$STAGE/kernel.efi"
LOADER="$STAGE/loader.efi"

# Sign the kernel with the daily key: the loader refuses an ESP kernel file
# without a valid signature, and the installer copies the signature into the
# slot header so a fresh install boots through the verified slot path.
KEY="toolchain/eupkg/keys/dev.key"
[ -f "$KEY" ] || { echo "ERROR: $KEY missing — run: python3 toolchain/eupkg/gen-dev-key.py"; exit 1; }
SIG="$STAGE/kernel.efi.sig"
python3 -c '
import sys
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
sk = Ed25519PrivateKey.from_private_bytes(open(sys.argv[1], "rb").read())
open(sys.argv[3], "wb").write(sk.sign(open(sys.argv[2], "rb").read()))
' "$KEY" "$EFI" "$SIG"

echo "==> building FAT32 image ($IMG) via mtools (no root needed)"
dd if=/dev/zero of="$IMG" bs=1M count=256 status=none
mkfs.fat -F 32 -n EUROKERNEL "$IMG" >/dev/null
mmd -i "$IMG" ::/EFI ::/EFI/BOOT
# G4 TWO-STAGE: BOOTX64.EFI = loader; the kernel sits as slot image A and B.
mcopy -i "$IMG" "$LOADER" ::/EFI/BOOT/BOOTX64.EFI
mcopy -i "$IMG" "$EFI" ::/EFI/BOOT/eurokernel-A.efi
mcopy -i "$IMG" "$EFI" ::/EFI/BOOT/eurokernel-B.efi
mcopy -i "$IMG" "$SIG" ::/EFI/BOOT/eurokernel-A.efi.sig
mcopy -i "$IMG" "$SIG" ::/EFI/BOOT/eurokernel-B.efi.sig
echo "==> done: $IMG (two-stage: loader → eurokernel-A/B.efi)"
echo "    Test:        ./scripts/run-qemu.sh"
echo "    Screenshot:  python3 scripts/screenshot.py $IMG boot.png"
echo "    To USB:      sudo dd if=$IMG of=/dev/sdX bs=4M status=progress  # lsblk first!"

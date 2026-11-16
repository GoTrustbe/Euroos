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
# The kernel's link map. Kernel and loader are linked with the same
# /MAP:target/kernel.map (one flag for the target) and the loader is linked
# last, so after a build that file was the LOADER's map and the kernel had to
# be relinked for its symbols. Called right after the kernel build: when the
# file is the kernel's (it was just linked) it is kept as
# target/eurokernel.map; when the kernel was up to date, the copy of the
# build that linked it is still there.
keep_kernel_map() {
  if grep -q 'eurokernel-[0-9a-f]*\.eurokernel\.' target/kernel.map 2>/dev/null; then
    cp -f target/kernel.map target/eurokernel.map
  fi
}
# EuroUpdate compares this against the signed channel manifest (YYYYMMDD).
# With SOURCE_DATE_EPOCH in the environment (a reproducible build) the version is the date of
# that moment, not of today: the kernel binary then does not depend on the day it is built.
export EUROOS_BUILD_VERSION="${EUROOS_BUILD_VERSION:-$(date -u ${SOURCE_DATE_EPOCH:+-d "@$SOURCE_DATE_EPOCH"} +%Y%m%d)}"
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
  keep_kernel_map
  cargo lbuild-release           # G4: two-stage loader
elif [ "$PROFILE" = "lockbench" ]; then
  # J1 lock benchmark image: boots, runs [lk-stress]/[lk-bench] on every core
  # right after SMP bring-up, prints the contention report and powers off.
  PROFILE="release"
  EFI="target/x86_64-unknown-uefi/release/eurokernel.efi"
  cargo kbuild-lockbench
  keep_kernel_map
  cargo lbuild-release
elif [ "$PROFILE" = "heaptrace" ]; then
  # BUG-027 debug image: the selftest image plus heap accounting per allocation
  # site (heapsnap / heapdiff / heaptop in the shell, kernel/src/heaptrace.rs).
  PROFILE="release"
  EFI="target/x86_64-unknown-uefi/release/eurokernel.efi"
  cargo kbuild-heaptrace
  keep_kernel_map
  cargo lbuild-release
elif [ "$PROFILE" = "chrome" ]; then
  # Iteration image: chrome runs in the boot phase (see the chrome-boot feature).
  PROFILE="release"
  EFI="target/x86_64-unknown-uefi/release/eurokernel.efi"
  cargo kbuild-chrome
  keep_kernel_map
  cargo lbuild-release
elif [ "$PROFILE" = "release" ]; then
  cargo kbuild-release
  keep_kernel_map
  cargo lbuild-release           # G4: two-stage loader
else
  cargo kbuild
  keep_kernel_map
  cargo build -p loader --target x86_64-unknown-uefi -Z build-std=core,compiler_builtins,alloc -Z build-std-features=compiler-builtins-mem
fi
[ -f "$EFI" ] || { echo "ERROR: $EFI not found"; exit 1; }
LOADER="target/x86_64-unknown-uefi/${PROFILE}/loader.efi"
[ -f "$LOADER" ] || { echo "ERROR: $LOADER not found"; exit 1; }
echo "==> kernel: $(du -h "$EFI" | cut -f1) · loader: $(du -h "$LOADER" | cut -f1) · kernel link map: target/eurokernel.map"

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
# Reproducible image: building the same commit twice gives the same bytes. The kernel,
# loader and signatures already were; what differed was FAT metadata: the volume id
# (mkfs.fat takes the clock), the label entry's time stamp and the creation/modification
# times mtools writes for each directory and file. All of these now come from one fixed
# time: SOURCE_DATE_EPOCH if set, else the commit time of HEAD (else 1980-01-01).
# A FAT time stamp cannot be older than 1980-01-01.
if [ -z "${SOURCE_DATE_EPOCH:-}" ]; then
  SOURCE_DATE_EPOCH="$(git log -1 --format=%ct 2>/dev/null || true)"
fi
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-315532800}"
[ "$SOURCE_DATE_EPOCH" -ge 315532800 ] || SOURCE_DATE_EPOCH=315532800
export SOURCE_DATE_EPOCH
# The ESP is laid out in a staging tree first: mcopy -m takes each file's and directory's
# modification time from the source, and the tree is the one place those are set.
ESP="$STAGE/esp"
mkdir -p "$ESP/EFI/BOOT"
# G4 TWO-STAGE: BOOTX64.EFI = loader; the kernel sits as slot image A and B.
cp "$LOADER" "$ESP/EFI/BOOT/BOOTX64.EFI"
cp "$EFI" "$ESP/EFI/BOOT/eurokernel-A.efi"
cp "$EFI" "$ESP/EFI/BOOT/eurokernel-B.efi"
cp "$SIG" "$ESP/EFI/BOOT/eurokernel-A.efi.sig"
cp "$SIG" "$ESP/EFI/BOOT/eurokernel-B.efi.sig"
# Boot options (kernel/src/bootopt.rs), e.g. EUROOS_BOOTOPTS="net.driver=user"
# (several separated by ';'). Without it the ESP holds no option file: defaults.
if [ -n "${EUROOS_BOOTOPTS:-}" ]; then
  printf '%s
' "$EUROOS_BOOTOPTS" | tr ';' '
' > "$ESP/EFI/BOOT/euroos-boot.conf"
  echo "==> boot options on the ESP: $EUROOS_BOOTOPTS"
fi
find "$ESP" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
dd if=/dev/zero of="$IMG" bs=1M count=256 status=none
# Volume id fixed (the first four bytes of the label, as hex); mkfs.fat 4.2 takes its
# label-entry time from SOURCE_DATE_EPOCH.
mkfs.fat -F 32 -n EUROKERNEL -i 45524f4b "$IMG" >/dev/null
mcopy -i "$IMG" -s -m "$ESP/EFI" ::/
echo "==> done: $IMG (two-stage: loader → eurokernel-A/B.efi)"
echo "    Test:        ./scripts/run-qemu.sh"
echo "    Screenshot:  python3 scripts/screenshot.py $IMG boot.png"
echo "    To USB:      sudo dd if=$IMG of=/dev/sdX bs=4M status=progress  # lsblk first!"

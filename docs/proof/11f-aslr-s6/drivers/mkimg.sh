#!/usr/bin/env bash
# mkimg.sh SRC KIND OUT.img [EPOCH]  (KIND: st = selftest/release kernel, pub = public image kernel)
# scripts/build.sh without the userland step (no userland binary or signature is rebuilt or re-signed),
# one target dir per source tree; the kernel is signed for the image only (no .sig file in any tree).
set -euo pipefail
export PATH=/root/.cargo/bin:$PATH
SRC=$1 KIND=$2 OUT=$3
cd "$SRC"; mkdir -p target
export CARGO_TARGET_DIR=/opt/euroos/aslr6-target/$(basename $SRC)
export CARGO_BUILD_JOBS=6
export SOURCE_DATE_EPOCH="${4:-$(git -C /opt/euroos/wt-11f-aslr6 log -1 --format=%ct)}"
export EUROOS_BUILD_VERSION="${EUROOS_BUILD_VERSION:-$(date -u -d "@$SOURCE_DATE_EPOCH" +%Y%m%d)}"
if [ "$KIND" = pub ]; then nice -n 10 cargo kbuild-image -j 6; else nice -n 10 cargo kbuild-release -j 6; fi
nice -n 10 cargo lbuild-release -j 6
T=$CARGO_TARGET_DIR/x86_64-unknown-uefi/release
STAGE=$(mktemp -d); trap "rm -rf $STAGE" EXIT
cp $T/eurokernel.efi $STAGE/kernel.efi; cp $T/loader.efi $STAGE/loader.efi
python3 -c "
import sys
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
sk = Ed25519PrivateKey.from_private_bytes(open(sys.argv[1], \"rb\").read())
open(sys.argv[3], \"wb\").write(sk.sign(open(sys.argv[2], \"rb\").read()))
" /opt/euroos/wt-11f-aslr6/toolchain/eupkg/keys/dev.key $STAGE/kernel.efi $STAGE/kernel.efi.sig
ESP=$STAGE/esp; mkdir -p $ESP/EFI/BOOT
cp $STAGE/loader.efi $ESP/EFI/BOOT/BOOTX64.EFI
for s in A B; do cp $STAGE/kernel.efi $ESP/EFI/BOOT/eurokernel-$s.efi; cp $STAGE/kernel.efi.sig $ESP/EFI/BOOT/eurokernel-$s.efi.sig; done
if [ -n "${EUROOS_BOOTOPTS:-}" ]; then printf "%s\n" "$EUROOS_BOOTOPTS" | tr ";" "\n" > $ESP/EFI/BOOT/euroos-boot.conf; fi
find $ESP -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
dd if=/dev/zero of="$OUT" bs=1M count=256 status=none
mkfs.fat -F 32 -n EUROKERNEL -i 45524f4b "$OUT" >/dev/null
mcopy -i "$OUT" -s -m $ESP/EFI ::/
echo "built $OUT from $(git -C $SRC log -1 --format=%h 2>/dev/null || echo archive) kind=$KIND"

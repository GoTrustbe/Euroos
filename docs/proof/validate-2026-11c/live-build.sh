#!/bin/bash
# The self-test image with the live GTK demo window (feature live-demos): the kernel
# is built with that feature and put into a copy of an existing self-test image,
# signed as scripts/build.sh signs it.
# usage: live-build.sh <worktree> <base image> <tag>
export PATH=$HOME/.cargo/bin:$PATH
R=/opt/euroos/validate-runs
W=$1; BASEIMG=$2; T=$3
cd $W || exit 1
(
  flock 9
  export EUROOS_BUILD_VERSION=$(date -u +%Y%m%d)
  CARGO_BUILD_JOBS=4 nice -n 15 cargo build --release -p kernel --features live-demos --target x86_64-unknown-uefi \
    -Z build-std=core,compiler_builtins,alloc -Z build-std-features=compiler-builtins-mem > $R/build/$T.buildlog 2>&1
  rc=$?
  if [ $rc = 0 ]; then
    S=$(mktemp -d)
    cp target/x86_64-unknown-uefi/release/eurokernel.efi $S/kernel.efi
    python3 -c '
import sys
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
sk = Ed25519PrivateKey.from_private_bytes(open(sys.argv[1], "rb").read())
open(sys.argv[3], "wb").write(sk.sign(open(sys.argv[2], "rb").read()))
' toolchain/eupkg/keys/dev.key $S/kernel.efi $S/kernel.efi.sig
    cp --sparse=always $BASEIMG $R/build/$T.img
    for s in A B; do
      mcopy -o -i $R/build/$T.img $S/kernel.efi ::/EFI/BOOT/eurokernel-$s.efi
      mcopy -o -i $R/build/$T.img $S/kernel.efi.sig ::/EFI/BOOT/eurokernel-$s.efi.sig
    done
    rm -rf $S
    sha256sum $R/build/$T.img > $R/build/$T.sha
  fi
  echo "rc=$rc $(date +%T) $(git rev-parse --short HEAD) dirty=$(git status -s | grep -vc '^??')" > $R/build/$T.done
) 9>/tmp/eurokernel-build.lock

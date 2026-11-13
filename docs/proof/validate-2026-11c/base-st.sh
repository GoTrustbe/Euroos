#!/bin/bash
# The self-test kernel of a given commit (default features), put into a copy of an existing self-test image: the
# base for a before-and-after of the desktop.
# A throwaway detached worktree that shares the target directory of wt-chrome. Cargo
# names the artefacts of a workspace crate the same in both worktrees and judges
# freshness by file times, so a build in the other worktree leaves artefacts that
# wt-chrome would take for its own: everything here runs under the build lock, and
# the sources that differ are touched in wt-chrome before the lock is released.
# usage: base-live.sh <commit> <base image> <tag>
export PATH=$HOME/.cargo/bin:$PATH
R=/opt/euroos/validate-runs; X=/opt/euroos/wt-validate-base; M=/opt/euroos/eurokernel; C=/opt/euroos/wt-chrome
COMMIT=$1; BASEIMG=$2; T=$3
(
  flock 9
  cd $C || exit 1
  git worktree add -q --detach $X $COMMIT || exit 1
  cd $X || exit 1
  ln -s $C/target target
  ln -s $M/toolchain/eupkg/keys/dev.key toolchain/eupkg/keys/dev.key
  cp -n $M/userland/*.elf $M/userland/*.elf.sig userland/ 2>/dev/null
  export EUROOS_BUILD_VERSION=$(date -u +%Y%m%d)
  CARGO_BUILD_JOBS=4 nice -n 15 cargo build --release -p kernel --target x86_64-unknown-uefi \
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
  rm -f toolchain/eupkg/keys/dev.key target
  cd $C && git worktree remove --force $X
  # wt-chrome must rebuild what the other worktree built from other sources
  git diff --name-only $COMMIT HEAD -- crates loader kernel | xargs -r touch
  touch kernel/src/main.rs
) 9>/tmp/eurokernel-build.lock

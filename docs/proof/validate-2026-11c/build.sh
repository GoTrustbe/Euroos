#!/bin/bash
# usage: build.sh <profile> <tag> [bootopts]  -> /opt/euroos/validate-runs/build/<tag>.{img,efi,map,sha,done}
export PATH=$HOME/.cargo/bin:$PATH
W=${W:-/opt/euroos/wt-chrome}
cd $W || exit 1
P=$1; T=$2
D=/opt/euroos/validate-runs/build
mkdir -p $D
rm -f $D/$T.done
( EUROOS_BOOTOPTS="${3:-}" CARGO_BUILD_JOBS=4 nice -n 15 ./scripts/build.sh $P ) > $D/$T.buildlog 2>&1
rc=$?
if [ $rc = 0 ]; then
  mv eurokernel.img $D/$T.img
  cp target/x86_64-unknown-uefi/release/eurokernel.efi $D/$T.efi
  cp target/eurokernel.map $D/$T.map 2>/dev/null
  sha256sum $D/$T.img $D/$T.efi > $D/$T.sha
fi
echo "rc=$rc $(date +%T) $(git rev-parse --short HEAD) dirty=$(git status -s | grep -vc '^??')" > $D/$T.done

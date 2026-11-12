#!/bin/bash
# .16 (TCG): host tests, the public image on the open network (ping, DNS, fetch,
# https, both NICs), then repro-build.sh twice. Builds and tests take the
# build lock, one at a time.
export PATH=$HOME/.cargo/bin:$PATH
R=/opt/euroos/rand-runs; W=/opt/euroos/wt-rand
IMG=$R/img/after-pub.img
mkdir -p $R/proof $R/bt
cd $W
( flock 9
  CARGO_BUILD_JOBS=4 nice -n 15 cargo test > $R/hosttests.log 2>&1
  echo "rc=$? $(git rev-parse --short HEAD) $(date +%T)" > $R/hosttests.done
) 9>/tmp/eurokernel-build.lock
cd $R
printf 'net\nping 10.0.2.2\nnslookup example.com\nfetch example.com\nhttps example.com\nnetstat\n' > $R/cmds-net.txt
cp --sparse=always $IMG $R/image-net.img
python3 $W/scripts/boot-time.py net-open image-net.img $R/bt --accel tcg --net open --type-wait 5 --hold 3 --timeout 600 --cmds cmds-net.txt > $R/bt-net.out 2>&1
echo "rc=$? $(date +%T)" > $R/bt-net.done
python3 $W/scripts/boot-time.py net-open-e1000e image-net.img $R/bt --accel tcg --net open --nic e1000e --type-wait 5 --hold 3 --timeout 600 --cmds cmds-net.txt > $R/bt-net-e1000e.out 2>&1
echo "rc=$? $(date +%T)" > $R/bt-net-e1000e.done
rm -f $R/image-net.img $R/bt/*.d/boot.img
echo "done $(date +%T)" > $R/chainB.done
cd $W
(
flock 9
echo "# $(git rev-parse --short HEAD), $(date -u +%F)"
CARGO_BUILD_JOBS=4 nice -n 15 ./scripts/repro-build.sh 2>&1 | grep "^==>"
H1=$(sha256sum target/x86_64-unknown-uefi/release/eurokernel.efi | cut -d' ' -f1)
L1=$(sha256sum target/x86_64-unknown-uefi/release/loader.efi | cut -d' ' -f1)
rm -f target/x86_64-unknown-uefi/release/eurokernel.efi
touch kernel/src/main.rs
CARGO_BUILD_JOBS=4 nice -n 15 ./scripts/repro-build.sh 2>&1 | grep "^==>"
H2=$(sha256sum target/x86_64-unknown-uefi/release/eurokernel.efi | cut -d' ' -f1)
L2=$(sha256sum target/x86_64-unknown-uefi/release/loader.efi | cut -d' ' -f1)
echo "build #1: $H1 (loader $L1)"
echo "build #2: $H2 (loader $L2)"
echo "paths of the build machine in the kernel: $(strings target/x86_64-unknown-uefi/release/eurokernel.efi | grep -c '/root/')"
test "$H1" = "$H2" && test "$L1" = "$L2" && echo "REPRODUCIBLE" || echo "NON-REPRODUCIBLE"
) 9>/tmp/eurokernel-build.lock > $R/proof/repro-build.txt 2>&1
echo "done $(date +%T)" > $R/chainC.done

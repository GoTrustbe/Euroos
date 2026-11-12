#!/bin/bash
# .16 (TCG): host tests, then the public image: gui-smoke twice, wizard install, open network.
export PATH=$HOME/.cargo/bin:$PATH
R=/opt/euroos/integ2-runs; W=/opt/euroos/wt-integ2
IMG=$R/build/pub5.img
mkdir -p $R/proof $R/bt
cd $W
( flock 9
  CARGO_BUILD_JOBS=4 nice -n 15 cargo test > $R/hosttests.log 2>&1
  echo "rc=$? $(git rev-parse --short HEAD) $(date +%T)" > $R/hosttests.done
) 9>/tmp/eurokernel-build.lock
rm -rf $R/gs-tcg-a $R/gs-tcg-b $R/wizard
nice -n 10 python3 scripts/gui-smoke.py --image $IMG --workdir $R/gs-tcg-a > $R/gs-tcg-a.out 2>&1
echo "rc=$? $(date +%T)" > $R/gs-tcg-a.done
nice -n 10 python3 scripts/gui-smoke.py --image $IMG --workdir $R/gs-tcg-b --net e1000e > $R/gs-tcg-b.out 2>&1
echo "rc=$? $(date +%T)" > $R/gs-tcg-b.done
cp --sparse=always $IMG $R/image-wiz.img
nice -n 10 python3 scripts/wizard-install-test.py $R/image-wiz.img $R/wizard > $R/wizard.out 2>&1
echo "rc=$? $(date +%T)" > $R/wizard.done
rm -f $R/image-wiz.img
cd $R
printf 'net\nping 10.0.2.2\nnslookup example.com\nfetch example.com\nhttps example.com\nnetstat\n' > $R/cmds-net.txt
cp --sparse=always $IMG $R/image-net.img
python3 $W/scripts/boot-time.py net-open image-net.img $R/bt --accel tcg --net open --type-wait 5 --hold 3 --timeout 600 --cmds cmds-net.txt --shot $R/bt/desktop-tcg-net.png > $R/bt-net.out 2>&1
echo "rc=$? $(date +%T)" > $R/bt-net.done
python3 $W/scripts/boot-time.py net-open-e1000e image-net.img $R/bt --accel tcg --net open --nic e1000e --type-wait 5 --hold 3 --timeout 600 --cmds cmds-net.txt > $R/bt-net-e1000e.out 2>&1
echo "rc=$? $(date +%T)" > $R/bt-net-e1000e.done
rm -f $R/image-net.img
echo "done $(date +%T)" > $R/chainB.done
# The installed system at 1280x720: the lock screen (display.mode on the installed ESP).
mkdir -p $R/wizard720 && cp --sparse=always $R/wizard/target.img $R/wizard720/target.img
printf "display.mode=1280x720\n" > $R/wizard720/euroos-boot.conf
OFF=$(sfdisk -d $R/wizard720/target.img 2>/dev/null | grep "type=C12A7328" | sed -E "s/.*start= *([0-9]+).*/\1/")
mcopy -o -i $R/wizard720/target.img@@$((OFF*512)) $R/wizard720/euroos-boot.conf ::/EFI/BOOT/euroos-boot.conf
cd $W && nice -n 10 python3 scripts/wizard-install-test.py $IMG $R/wizard720 --standalone > $R/wizard720.out 2>&1
echo "rc=$? $(date +%T)" > $R/wizard720.done
echo "done $(date +%T)" > $R/chainB2.done

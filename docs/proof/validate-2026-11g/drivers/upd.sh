#!/bin/bash
# Update path, local, no network: (1) the PUBLIC 2026.11.17 image installs itself on a blank disk (wizard keys); (2) copy A: unchanged (baseline,
# rename-persist on 11.17); copy B: the candidate kernel (kernel + build signature of the candidate image, not re-signed) is written into the slot partition
# the way write_image_to_slot does (slotwrite.py), then rename-persist on it: the loader must verify and boot it, and the root/user created by
# the 11.17 installer must sign in and persist. The update CLIENT (euroupdate fetch/check) is root-only and the 11.17 client only talks to euro-os.eu:443; not run.
B=/srv/euroos/val-runs/g; cd $B; S=$B/scripts; I=$B/img; W=$B/upd; PIN=${PIN:-30-33}
rm -rf $W; mkdir -p $W
python3 inst-keep.py $I/b0-pub.img $W/r1 $PIN > $W/r1.out 2>&1 < /dev/null; cat $W/r1.out
cp $W/r1/target.img $W/target-base.img; cp $W/r1/target.img $W/target-upd.img
python3 slotwrite.py $W/target-upd.img $B/upd2/cand.efi $B/upd2/cand.efi.sig | tee $W/slotwrite.out
for v in base upd; do
  python3 $S/rename-persist.py --image $I/m-pub.img --workdir $W/rp-$v --wizard $S/wizard-install-test.py --kvm --pin $PIN --target $W/target-$v.img > $W/rp-$v.out 2>&1 < /dev/null
  echo "== $v"; tail -n 2 $W/rp-$v.out; grep -a "loader\] kernel image\|euroupdate\] \(boot from\|slot\)" $W/rp-$v/boot1.serial.log | cut -c1-200 | tee $W/rp-$v.slot.txt
done
touch $W/done

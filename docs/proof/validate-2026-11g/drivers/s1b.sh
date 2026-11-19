#!/bin/bash
# S1b: reference KVM boots, q35 matrix, installs, TCG matrix of the reference
B=/srv/euroos/val-runs/g; cd $B; S=$B/scripts; I=$B/img
mkdir -p kvm/b0 inst tcg q35m
kl() { SMP=$1 PIN=$2; for MEM in 1G 2G 4G 8G; do ./kboot-hda.sh $I/b0-st.img $B/kvm/b0 k-${MEM}-s$SMP $MEM $SMP $PIN "interactive loop started" 900 > kvm/b0/k-${MEM}-s$SMP.res 2>&1; done; }
kl 1 0 & kl 2 2-3 & kl 4 4-7 &
# q35 matrix, candidate (tpm legs not run: no swtpm)
( cp $I/m-st.img q35m/st.raw && cd q35m && EK_RESTRICT=1 EK_ACCEL=kvm EK_BOOT_DEADLINE=900 taskset -c 12-13 python3 $S/run-metal-matrix.py $B/q35m/st.raw --legs base,nvme,ahci,e1000e,hda,usb,usbhub,usbaudio,usbnet,hwprobe,scan,power,printer,nvmeroot,ahciroot > matrix.out 2>&1; rm -f st.raw; touch ../q35.done ) &
# installs: 10 side by side (candidate public image), wizard, ro-install variants, rename-persist
( for i in $(seq 1 10); do (python3 inst.py $I/m-pub.img inst/m-$i 8-11,28-31 > inst/m-$i.out 2>&1 < /dev/null &); done
  until [ $(cat inst/m-*.out 2>/dev/null | grep -c "^\[inst\]") -ge 10 ]; do sleep 10; done
  taskset -c 8-11,28-31 python3 $S/wizard-install-test.py $I/m-pub.img $B/wizard > wizard.out 2>&1 < /dev/null
  for m in plain inject ro0; do mkdir -p roi-$m; MODE=$m python3 $S/ro-install-test.py $I/m-st.img roi-$m 8-11 > roi-$m.out 2>&1 < /dev/null; done
  mkdir -p rp; python3 $S/rename-persist.py --image $I/m-pub.img --workdir $B/rp --wizard $S/wizard-install-test.py --kvm --pin 8-11 > rp.out 2>&1 < /dev/null
  mkdir -p wizard-ref; taskset -c 8-11,28-31 python3 $S/wizard-install-test.py $I/b0-pub.img $B/wizard-ref > wizard-ref.out 2>&1 < /dev/null
  touch inst.done ) &
# TCG matrix of the reference
./tcgq.sh b0 $I/b0-st.img 24-27 user-smp4-iommu user-smp4-noiommu kernel-smp4 storage-smp4-iommu-1g combined-smp4-iommu-1g &
./tcgq.sh b0 $I/b0-st.img 32-33 user-smp1-iommu user-smp1-noiommu kernel-smp1 storage-smp1-iommu-1g combined-smp1-iommu-1g &
wait; touch s1b.done

#!/bin/bash
# phase3a.sh: disk-heavy lanes, after the soak: TCG matrix, installs, wizard, ro-install, rename-persist (lane N, CPUs 6-11 / 26-29), q35 matrix (lane Q, CPUs 18-19)
B=/srv/euroos/v11f-runs; cd $B
laneN() {
  mkdir -p tcg/m; ./tcgmatrix.sh m $B/img/m-st.img 6-9 10-11
  mkdir -p inst; for i in $(seq 1 10); do (python3 inst.py img/m-pub.img inst/m-$i 6-9,26-29 > inst/m-$i.out 2>&1 < /dev/null &); done
  until [ $(cat inst/m-*.out 2>/dev/null | grep -c "^\[inst\]") -ge 10 ]; do sleep 10; done
  taskset -c 6-9,26-29 python3 scripts/wizard-install-test.py img/m-pub.img $B/wizard > wizard.out 2>&1 < /dev/null
  for m in plain inject ro0; do mkdir -p roi-$m; MODE=$m python3 scripts/ro-install-test.py img/m-st.img roi-$m 6-9 > roi-$m.out 2>&1 < /dev/null; done
  mkdir -p rp; python3 scripts/rename-persist.py --image img/m-pub.img --workdir $B/rp --wizard scripts/wizard-install-test.py --kvm --pin 6-9 > rp.out 2>&1 < /dev/null
  touch laneN.done; }
laneQ() {
  mkdir -p q35m && cp img/m-st.img q35m/st.raw && (cd q35m && EK_RESTRICT=1 EK_ACCEL=kvm EK_BOOT_DEADLINE=900 taskset -c 12-13 python3 $B/scripts/run-metal-matrix.py $B/q35m/st.raw --legs base,nvme,ahci,e1000e,hda,usb,usbhub,usbaudio,usbnet,hwprobe,scan,power,printer,nvmeroot,ahciroot > matrix.out 2>&1)
  touch laneQ.done; }
laneN & laneQ & wait
echo phase3a-done > phase3a.done

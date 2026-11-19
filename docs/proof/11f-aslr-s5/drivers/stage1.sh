#!/bin/bash
# stage1.sh: final regression of ASLR S5 (feature/11f-aslr-s5 e9b488d) on the R630.
# on = default (aslr=on, aslr.arena=random), off = aslr=off, leg = aslr=legacy; base = 417f6b9 (S4 + bugs-35x merge), same option.
B=/srv/euroos/aslr5-runs; cd $B; S=$B/scripts; I=$B/img; P=/srv/euroos/chrome-runs
shm() { rm -rf runs/$1 /dev/shm/a5/$1; mkdir -p /dev/shm/a5/$1 runs; ln -s /dev/shm/a5/$1 runs/$1; }
cs() { python3 $S/chrome-smoke.py --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --smp 2 "$@"; }
chromeon() { PIN=0-1
  N=soak-on; shm $N
  python3 $S/chrome-mem-soak.py --image $I/s5-br.img --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --pin $PIN --smp 2 --mem 8192M --workdir /dev/shm/a5/$N --no-video --free-video-seconds 0 --hours 1 --local-video-dir $B/vid --local-seconds 60 --local-port 18793 > runs/$N.out 2>&1 < /dev/null
  N=starts20-on; shm $N; cs --image $I/s5-br.img --pin $PIN --workdir /dev/shm/a5/$N --starts 20 > runs/$N.out 2>&1 < /dev/null
  N=sites-on; shm $N; cs --image $I/s5-br.img --pin $PIN --workdir /dev/shm/a5/$N --only sites --rounds 3 > runs/$N.out 2>&1 < /dev/null
  N=interact-on; shm $N; cs --image $I/s5-br.img --pin $PIN --workdir /dev/shm/a5/$N --only interact > runs/$N.out 2>&1 < /dev/null
  touch chrome-on.done; }
chromestarts() { M=$1 IMG=$2 PIN=$3
  N=starts20-$M; shm $N; cs --image $IMG --pin $PIN --workdir /dev/shm/a5/$N --starts 20 > runs/$N.out 2>&1 < /dev/null
  touch chrome-$M.done; }
soaklane() { PIN=4-7
  mkdir -p soak-on; (cd soak-on && python3 -u $S/loadtest.py --image $I/s5-st.img --workdir $B/soak-on/w --kvm --smp 4 --pin $PIN --nic e1000e --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hostfwd 18611 --hammer 4 --procs 32 --scenario soak --soak-churn-proc 8 --duration 3900 --boot-timeout 900 > out.txt 2>&1 < /dev/null)
  rm -rf soak-on/w/*.img soak-on/w/*.raw 2>/dev/null
  taskset -c $PIN python3 $S/wizard-install-test.py $I/s5-pub.img $B/wizard-on > wizard-on.out 2>&1 < /dev/null
  for m in plain inject ro0; do mkdir -p roi-on-$m; MODE=$m python3 $S/ro-install-test.py $I/s5-st.img roi-on-$m $PIN > roi-on-$m.out 2>&1 < /dev/null; done
  rm -f roi-on-*/*.img wizard-on/*.img 2>/dev/null
  touch soak-on.done; }
kvmlane() { A=$1 Bp=$2; shift 2
  for job in "$@"; do set -- $(echo $job | tr : " "); T=$1 M=$2 MEM=$3; mkdir -p kvm/$T
    ./kboot-hda.sh $I/$T.img $B/kvm/$T k-$MEM $MEM 2 $A "interactive loop started" 900 > kvm/$T/k-$MEM.res 2>&1
  done; }
kvmall() {
  kvmlane 8-9 x s5-st:on:1G s5-st:on:2G s5-st:on:4G s5-st:on:8G s5-st-off:off:2G s5-st-leg:leg:2G &
  kvmlane 10-11 x m-st:on:1G m-st:on:2G m-st:on:4G m-st:on:8G m-st-off:off:2G m-st-leg:leg:2G &
  kvmlane 12-13 x s5-st-off:off:1G s5-st-off:off:4G s5-st-off:off:8G s5-st-leg:leg:1G s5-st-leg:leg:4G s5-st-leg:leg:8G &
  kvmlane 14-15 x m-st-off:off:1G m-st-off:off:4G m-st-off:off:8G m-st-leg:leg:1G m-st-leg:leg:4G m-st-leg:leg:8G &
  wait
  for M in on off leg; do mkdir -p runs; IMG=$I/s5-pub.img; [ $M != on ] && IMG=$I/s5-pub-$M.img
    rm -rf runs/gs-$M; mkdir -p runs/gs-$M; python3 $S/gui-smoke.py --image $IMG --workdir $B/runs/gs-$M --kvm --smp 4 --mem 4G --pin 8-11 > runs/gs-$M.out 2>&1 < /dev/null; done
  mkdir -p q35-on && cp $I/s5-st.img q35-on/st.raw && (cd q35-on && EK_RESTRICT=1 EK_ACCEL=kvm taskset -c 12-13 python3 $S/run-metal-matrix.py $B/q35-on/st.raw --legs base,nvme,ahci,e1000e,hda,usb,usbhub,usbaudio,usbnet,hwprobe,scan,nvmeroot,ahciroot,power,printer > matrix.out 2>&1 < /dev/null); rm -f q35-on/st.raw
  touch kvm.done; }
tcglane() {
  ./tcgq.sh on $I/s5-st.img 24-27 user-smp4-iommu user-smp4-noiommu kernel-smp4 storage-smp4-iommu-1g combined-smp4-iommu-1g &
  ./tcgq.sh on $I/s5-st.img 28-29 user-smp1-iommu user-smp1-noiommu kernel-smp1 storage-smp1-iommu-1g combined-smp1-iommu-1g &
  wait; touch tcg.done; }
mkdir -p /dev/shm/a5
chromeon &
chromestarts off $I/s5-br-off.img 2-3 &
chromestarts leg $I/s5-br-leg.img 20-21 &
chromestarts base $I/m-br.img 22-23 &
soaklane &
kvmall &
tcglane &
wait
touch stage1.done

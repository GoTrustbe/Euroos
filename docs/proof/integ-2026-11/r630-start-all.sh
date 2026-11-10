#!/usr/bin/env bash
# Integration runs on the R630 (KVM). Own CPUs, SLIRP restrict=on or no NIC, no VFIO.
B=/srv/euroos/integ-runs
S=$B/scripts
cd $B
# 1. soak, 2 h 05, churn proc 8 every round, 4 httpd clients (selftest image)
mkdir -p $B/soak/w
setsid nohup python3 $S/loadtest.py --image $B/selftest.img --workdir $B/soak/w --kvm --smp 4 --pin 8-11 --nic e1000e \
  --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hostfwd 18480 --hammer 4 --procs 32 \
  --scenario soak --soak-churn-proc 8 --duration 7500 --boot-timeout 900 > $B/soak/out.txt 2>&1 < /dev/null &
# 2. boot time, public image: 5 x default, 5 x modern virtio, then monkey soaks on the same cores
( mkdir -p $B/bt
  for i in 1 2 3 4 5; do python3 $S/boot-time.py default-$i $B/image.img $B/bt --pin 20-21 --hold 20; done > $B/bt/kvm-5x.txt 2>&1
  for i in 1 2 3 4 5; do python3 $S/boot-time.py modern-$i $B/image.img $B/bt --mode modern --pin 20-21 --hold 20; done >> $B/bt/kvm-5x.txt 2>&1
  python3 $S/boot-time.py tour-keep $B/image.img $B/bt --pin 20-21 --hold 8 --tour keep --shot $B/bt/tour-step1-kvm.png >> $B/bt/kvm-extra.txt 2>&1
  python3 $S/boot-time.py desktop $B/image.img $B/bt --pin 20-21 --hold 8 --shot $B/bt/desktop-kvm.png >> $B/bt/kvm-extra.txt 2>&1
  echo done >> $B/bt/kvm-5x.done ) < /dev/null > /dev/null 2>&1 &
# 3. monkey soaks, 35 min each after the smoke pass
setsid nohup python3 $S/gui-smoke.py --image $B/image.img --workdir $B/gui-usb --kvm --smp 4 --mem 4G --pin 24-27 \
  --input usb --net none --soak 35 --seed 20261110 > $B/gui-usb.out 2>&1 < /dev/null &
setsid nohup python3 $S/gui-smoke.py --image $B/image.img --workdir $B/gui-virtio --kvm --smp 2 --mem 2G --pin 28-29 \
  --input virtio --net restrict --soak 35 --seed 20261111 > $B/gui-virtio.out 2>&1 < /dev/null &
disown -a
sleep 3
echo started

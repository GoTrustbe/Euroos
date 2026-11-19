#!/bin/bash
# lanes4.sh: final regression of ASLR S4 (feature/11f-aslr-s4) on the R630, CPUs 0-15 + 20-35.
# ON = aslr.arena=fixed (boot option), OFF = default (identity); base = c71cce8.
B=/srv/euroos/aslr4-runs; cd $B; S=$B/scripts; I=$B/img; P=/srv/euroos/chrome-runs
waitdone() { until grep -aq "smoke\] \(PASS\|FAIL\|CRASH\)\|starts: [0-9]" runs/$1.out 2>/dev/null; do sleep 15; done; }
shm() { rm -rf runs/$1 /dev/shm/a4/$1; mkdir -p /dev/shm/a4/$1; ln -s /dev/shm/a4/$1 runs/$1; }
cp $I/s4a-pub.img $I/s4a-pub-fix.img; printf "aslr.arena=fixed\n" > /tmp/a4p.conf; mcopy -o -i $I/s4a-pub-fix.img /tmp/a4p.conf ::/EFI/BOOT/euroos-boot.conf; rm /tmp/a4p.conf
# browser lanes: L1 = ON on CPUs 0-1, L2 = OFF on 2-3 (siblings 20-23 idle)
chromelane() { M=$1 IMG=$2 PIN=$3 PORT=$4
  N=soak-$M; shm $N
  python3 $S/chrome-mem-soak.py --image $IMG --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --pin $PIN --smp 2 --mem 8192M --workdir /dev/shm/a4/$N --no-video --free-video-seconds 0 --hours 1 --local-video-dir $B/vid --local-seconds 60 --local-port $PORT > runs/$N.out 2>&1 < /dev/null
  N=starts20-$M; shm $N
  python3 $S/chrome-smoke.py --image $IMG --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --pin $PIN --workdir /dev/shm/a4/$N --starts 20 --smp 2 > runs/$N.out 2>&1 < /dev/null
  for t in sites interact localvideo; do N=$t-$M; shm $N; X=""
    [ $t = sites ] && X="--rounds 3"; [ $t = localvideo ] && X="--media-dir $B/vid --local-port $((PORT+10))"
    python3 $S/chrome-smoke.py --image $IMG --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --pin $PIN --workdir /dev/shm/a4/$N --only $t --smp 2 $X > runs/$N.out 2>&1 < /dev/null
  done
  touch chrome-$M.done; }
# loadtest soak lanes: ON 4-7, OFF 8-11; then installs
soaklane() { M=$1 IMG=$2 PIN=$3 PUB=$4 PORT=$5
  mkdir -p soak-$M; (cd soak-$M && python3 -u $S/loadtest.py --image $IMG --workdir $B/soak-$M/w --kvm --smp 4 --pin $PIN --nic e1000e --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hostfwd $PORT --hammer 4 --procs 32 --scenario soak --soak-churn-proc 8 --duration 3900 --boot-timeout 900 > out.txt 2>&1 < /dev/null)
  rm -rf soak-$M/w/*.img soak-$M/w/*.raw 2>/dev/null
  taskset -c $PIN python3 $S/wizard-install-test.py $PUB $B/wizard-$M > wizard-$M.out 2>&1 < /dev/null
  for m in plain inject ro0; do mkdir -p roi-$M-$m; MODE=$m python3 $S/ro-install-test.py $IMG roi-$M-$m $PIN > roi-$M-$m.out 2>&1 < /dev/null; done
  touch soak-$M.done; }
# KVM boots, layout, gui-smoke, q35 on 12-15
kvmlane() {
  mkdir -p kvm/base kvm/off kvm/on
  for m in 1 2 4 8; do
    ./kboot-hda.sh $I/s4a-st-fix.img $B/kvm/on k-${m}g ${m}G 2 12-13 "interactive loop started" 900 > kvm/on/k-${m}g.res 2>&1 &
    ./kboot-hda.sh $I/s4a-st.img $B/kvm/off k-${m}g ${m}G 2 14-15 "interactive loop started" 900 > kvm/off/k-${m}g.res 2>&1; wait
    ./kboot-hda.sh $I/base-st.img $B/kvm/base k-${m}g ${m}G 2 12-13 "interactive loop started" 900 > kvm/base/k-${m}g.res 2>&1
  done
  # identical layout with aslr=off: base and S4 identity
  mkdir -p kvm/layout; for t in base s4a; do cp $I/$t-st.img kvm/layout/$t-off.img; printf "aslr=off\n" > /tmp/a4l.conf; mcopy -o -i kvm/layout/$t-off.img /tmp/a4l.conf ::/EFI/BOOT/euroos-boot.conf; done; rm -f /tmp/a4l.conf
  ./kboot-hda.sh $B/kvm/layout/base-off.img $B/kvm/layout base-2g 2G 2 12-13 "interactive loop started" 900 > kvm/layout/base.res 2>&1 &
  ./kboot-hda.sh $B/kvm/layout/s4a-off.img $B/kvm/layout s4a-2g 2G 2 14-15 "interactive loop started" 900 > kvm/layout/s4a.res 2>&1; wait
  rm -f kvm/layout/*.img
  for M in on off; do IMG=$I/s4a-pub.img; [ $M = on ] && IMG=$I/s4a-pub-fix.img
    mkdir -p runs/gs-$M; python3 $S/gui-smoke.py --image $IMG --workdir $B/runs/gs-$M --kvm --smp 4 --mem 4G --pin 12-15 > runs/gs-$M.out 2>&1 < /dev/null; done
  for M in on off; do IMG=$I/s4a-st.img; PIN=14-15; [ $M = on ] && { IMG=$I/s4a-st-fix.img; PIN=12-13; }
    mkdir -p q35-$M && cp $IMG q35-$M/st.raw && (cd q35-$M && EK_RESTRICT=1 EK_ACCEL=kvm taskset -c $PIN python3 $S/run-metal-matrix.py $B/q35-$M/st.raw > matrix.out 2>&1 < /dev/null) &
  done; wait
  touch kvm.done; }
tcglane() {
  ACONF=aslr.arena=fixed ./tcgmatrix.sh on $I/s4a-st.img 24-27 28-29 &
  ./tcgmatrix.sh off $I/s4a-st.img 30-33 34-35 &
  wait; touch tcg.done; }
chromelane on $I/s4a-br-fix.img 0-1 18791 &
chromelane off $I/s4a-br-id.img 2-3 18792 &
soaklane on $I/s4a-st-fix.img 4-7 $I/s4a-pub-fix.img 18601 &
soaklane off $I/s4a-st.img 8-11 $I/s4a-pub.img 18602 &
kvmlane &
tcglane &
wait
echo stage1-done > stage1.done

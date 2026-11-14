cd /srv/euroos/final-runs; S=$PWD/scripts; I=$PWD/img
(setsid nohup bash -c "
mkdir -p $PWD/bt2
for i in 1 2 3 4 5 6 7 8; do
  python3 $S/boot-time.py final-\$i $I/plain.img $PWD/bt2 --pin 38-39 --hold 5
  python3 $S/boot-time.py rel-\$i $I/pubrel.img $PWD/bt2 --pin 38-39 --hold 5
done > $PWD/bt2/kvm-ab.txt 2>&1; echo done > $PWD/bt2.done
" > /dev/null 2>&1 < /dev/null &)
run() { cp --sparse=always img/$2.img q35/$1.raw
  TMPDIR=$PWD/tmp EK_RESTRICT=1 EK_ACCEL=kvm EK_BOOT_DEADLINE=900 taskset -c $3 python3 scripts/run-metal-matrix.py $PWD/q35/$1.raw --legs hwprobe > q35/$1.out 2>&1; rm -f q35/$1.raw; }
for spec in "q1:st:22-25:final" "q2:base40f:32-35:base" "q3:bis-integ:26-29:integ"; do
  IFS=: read t im p nm <<< $spec
  (setsid nohup bash -c "$(declare -f run); cd /srv/euroos/final-runs; run hwq-$nm-1 $im $p; run hwq-$nm-2 $im $p; run hwq-$nm-3 $im $p" > /dev/null 2>&1 < /dev/null &)
done

cd /srv/euroos/final-runs
run() { cp --sparse=always img/$2.img q35/$1.raw
  TMPDIR=$PWD/tmp EK_RESTRICT=1 EK_ACCEL=kvm EK_BOOT_DEADLINE=900 taskset -c $3 python3 scripts/run-metal-matrix.py $PWD/q35/$1.raw --legs hwprobe > q35/$1.out 2>&1; rm -f q35/$1.raw; }
# usage: hwn.sh name img pins1 pins2 pins3 : three parallel runs, 2 sequential each
nm=$1; im=$2; shift 2
for p in "$@"; do
  (setsid nohup bash -c "$(declare -f run); cd /srv/euroos/final-runs; run hwn-$nm-$p-1 $im $p; run hwn-$nm-$p-2 $im $p" > /dev/null 2>&1 < /dev/null &)
done

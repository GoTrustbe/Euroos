cd /srv/euroos/final-runs; mkdir -p q35
cp --sparse=always /srv/euroos/harness-runs/img/rel.img img/base40f.img
run() { # tag img pins accel
  cp --sparse=always img/$2.img q35/$1.raw
  if [ "$4" = tcg ]; then A=""; else A=kvm; fi
  TMPDIR=$PWD/tmp EK_RESTRICT=1 EK_ACCEL=$A EK_BOOT_DEADLINE=900 taskset -c $3 python3 scripts/run-metal-matrix.py $PWD/q35/$1.raw --legs hwprobe > q35/$1.out 2>&1
  rm -f q35/$1.raw
}
(setsid nohup bash -c "$(declare -f run); cd /srv/euroos/final-runs; run hw-base base40f 32-35 kvm" > /dev/null 2>&1 < /dev/null &)
(setsid nohup bash -c "$(declare -f run); cd /srv/euroos/final-runs; run hw-sttcg st 22-25 tcg" > /dev/null 2>&1 < /dev/null &)

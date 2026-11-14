cd /srv/euroos/final-runs; mkdir -p q35
run() { # tag img pins
  cp --sparse=always img/$2.img q35/$1.raw
  TMPDIR=$PWD/tmp EK_RESTRICT=1 EK_ACCEL=kvm EK_BOOT_DEADLINE=900 taskset -c $3 python3 scripts/run-metal-matrix.py $PWD/q35/$1.raw --legs hwprobe > q35/$1.out 2>&1
  rm -f q35/$1.raw
}
(setsid nohup bash -c "$(declare -f run); cd /srv/euroos/final-runs; run hw-st1 st 32-35; run hw-st2 st 32-35; run hw-st3 st 32-35" > /dev/null 2>&1 < /dev/null &)
(setsid nohup bash -c "$(declare -f run); cd /srv/euroos/final-runs; run hw-plain1 plain 36-37; run hw-rel1 pubrel 36-37; run hw-plain2 plain 36-37" > /dev/null 2>&1 < /dev/null &)

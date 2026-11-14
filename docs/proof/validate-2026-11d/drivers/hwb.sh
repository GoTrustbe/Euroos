# hwb.sh tag img pins
cd /srv/euroos/final-runs
cp --sparse=always img/$2.img q35/$1.raw
TMPDIR=$PWD/tmp EK_RESTRICT=1 EK_ACCEL=kvm EK_BOOT_DEADLINE=900 setsid nohup taskset -c $3 python3 scripts/run-metal-matrix.py $PWD/q35/$1.raw --legs hwprobe > q35/$1.out 2>&1 < /dev/null &

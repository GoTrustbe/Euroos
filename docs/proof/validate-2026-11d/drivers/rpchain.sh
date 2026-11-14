cd /srv/euroos/final-runs; S=$PWD/scripts; I=$PWD/img
chain() { # img pins names...
  im=$1; p=$2; shift 2
  for d in "$@"; do rm -rf $d; mkdir -p $d; python3 $S/rename-persist.py --image $I/$im.img --workdir $PWD/$d --wizard $S/wizard-install-test.py --kvm --pin $p > $d.out 2>&1; rm -f $d/target.img; done
}
(setsid nohup bash -c "$(declare -f chain); cd /srv/euroos/final-runs; S=\$PWD/scripts; I=\$PWD/img; chain plain 4-7 rp3 rp4 rp5" > /dev/null 2>&1 < /dev/null &)
(setsid nohup bash -c "$(declare -f chain); cd /srv/euroos/final-runs; S=\$PWD/scripts; I=\$PWD/img; chain pubrel 26-27 rprel2 rprel3" > /dev/null 2>&1 < /dev/null &)

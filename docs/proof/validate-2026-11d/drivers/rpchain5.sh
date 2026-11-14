cd /srv/euroos/final-runs; S=$PWD/scripts; I=$PWD/img
chain() { im=$1; p=$2; shift 2
  for d in "$@"; do rm -rf $d; mkdir -p $d; python3 $S/rename-persist.py --image $I/$im.img --workdir $PWD/$d --wizard $S/wizard-install-test.py --kvm --pin $p > $d.out 2>&1; rm -f $d/target.img; done
}
for spec in "$@"; do
  IFS=: read im p a b c <<< $spec
  (setsid nohup bash -c "$(declare -f chain); cd /srv/euroos/final-runs; S=\$PWD/scripts; I=\$PWD/img; chain $im $p $a $b $c" > /dev/null 2>&1 < /dev/null &)
done

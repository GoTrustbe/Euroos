cd /srv/euroos/final-runs; S=$PWD/scripts; I=$PWD/img
chain() { # img pins names...
  im=$1; p=$2; shift 2
  for d in "$@"; do rm -rf $d; mkdir -p $d; python3 $S/rename-persist.py --image $I/$im.img --workdir $PWD/$d --wizard $S/wizard-install-test.py --kvm --pin $p > $d.out 2>&1; rm -f $d/target.img; done
}
for spec in plain:4-7:rq1:rq2:rq3 plain:28-29:rq4:rq5:rq6 plain:32-35:rq7:rq8:rq9 pubrel:22-25:rq10:rq11:rq12; do
  IFS=: read im p a b c <<< $spec
  (setsid nohup bash -c "$(declare -f chain); cd /srv/euroos/final-runs; S=\$PWD/scripts; I=\$PWD/img; chain $im $p $a $b $c" > /dev/null 2>&1 < /dev/null &)
done

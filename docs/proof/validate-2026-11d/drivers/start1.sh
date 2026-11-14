cd /srv/euroos/final-runs
B=$PWD
mkdir -p kvm tcg/f soak/w
# KVM self-test boots with HDA, 1/2/4/8 GiB, 2 vCPU each
for m in 1 2 4 8; do
  case $m in 1) P=0-1;; 2) P=2-3;; 4) P=4-5;; 8) P=6-7;; esac
  (setsid nohup ./kboot-hda.sh $B/img/st.img $B/kvm k-${m}g ${m}G 2 $P "interactive loop started" 900 > kvm/k-${m}g.res 2>&1 < /dev/null &)
done
# TCG matrix, two queues
(setsid nohup ./tcgmatrix.sh f $B/img/st.img 8-11 12-13 > tcg/f/matrix.out 2>&1 < /dev/null &)
# soak 2 h
(setsid nohup python3 scripts/loadtest.py --image $B/img/st.img --workdir $B/soak/w --kvm --smp 4 --pin 16-19 --nic e1000e \
    --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hostfwd 18691 --hammer 4 --procs 32 \
    --scenario soak --soak-churn-proc 8 --duration 7500 --boot-timeout 900 > soak/out.txt 2>&1 < /dev/null &)
sleep 3; echo started

cd /srv/euroos/final-runs
B=$PWD; I=$B/img; S=$B/scripts
# chain A: wizard (TCG) then rename-persist (KVM) on 4-7
(setsid nohup bash -c "
cp --sparse=always $I/plain.img $B/image-wiz.img
rm -rf $B/wizard; taskset -c 4-7 nice -n 5 python3 $S/wizard-install-test.py $B/image-wiz.img $B/wizard > $B/wizard.out 2>&1; echo rc=\$? > $B/wizard.done
rm -f $B/image-wiz.img
rm -rf $B/rp; mkdir -p $B/rp
python3 $S/rename-persist.py --image $I/plain.img --workdir $B/rp --wizard $S/wizard-install-test.py --kvm --pin 4-7 > $B/rp.out 2>&1; echo rc=\$? > $B/rp.done
" > /dev/null 2>&1 < /dev/null &)
# chain G: monkey soak 35 min, then --net e1000e smoke, on 0-3
(setsid nohup bash -c "
mkdir -p $B/runs/gs-soak $B/runs/gs-e1000e
python3 $S/gui-smoke.py --image $I/plain.img --workdir $B/runs/gs-soak --kvm --smp 4 --mem 4G --pin 0-3 --soak 35 --seed 20261114 > $B/runs/gs-soak.out 2>&1; echo rc=\$? > $B/runs/gs-soak.done
python3 $S/gui-smoke.py --image $I/plain.img --workdir $B/runs/gs-e1000e --kvm --smp 2 --mem 4G --pin 0-3 --net e1000e > $B/runs/gs-e1000e.out 2>&1; echo rc=\$? > $B/runs/gs-e1000e.done
" > /dev/null 2>&1 < /dev/null &)
# short boots: 100 of the self-test image, 2 lanes
(setsid nohup bash -c "
bash $S/short-boots.sh $I/st.img $B short-a 50 14-15 kvm host '[rename] rename over' 600 2 &
bash $S/short-boots.sh $I/st.img $B short-b 50 36-37 kvm host '[rename] rename over' 600 2 &
wait; echo done > $B/boots.done
" > /dev/null 2>&1 < /dev/null &)
# boot time, alternating plain (final) and pubrel (2026.11.13), 8 each
(setsid nohup bash -c "
mkdir -p $B/bt
for i in 1 2 3 4 5 6 7 8; do
  python3 $S/boot-time.py final-\$i $I/plain.img $B/bt --pin 38-39 --hold 5
  python3 $S/boot-time.py rel-\$i $I/pubrel.img $B/bt --pin 38-39 --hold 5
done > $B/bt/kvm-ab.txt 2>&1; echo done > $B/bt.done
" > /dev/null 2>&1 < /dev/null &)
sleep 2; echo ok

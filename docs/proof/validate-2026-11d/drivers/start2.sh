cd /srv/euroos/final-runs
B=$PWD; I=$B/img
./cm.sh starts20 $I/pub.img 22-23 chrome-smoke.py --starts 20 --smp 2
./cm.sh sites3 $I/pub.img 24-25 chrome-smoke.py --only sites --rounds 3 --smp 2
./cm.sh interact $I/pub.img 26-27 chrome-smoke.py --only interact --smp 2
./cm.sh localvideo $I/pub.img 28-29 chrome-smoke.py --only localvideo --media-dir $B/vid --local-port 18782 --smp 2
./cm.sh wm480 $I/pub.img 30-31 wmvideo.py --smp 2 --mem 6144M --seconds 90 --times 2 --url https://upload.wikimedia.org/wikipedia/commons/transcoded/c/c0/Big_Buck_Bunny_4K.webm/Big_Buck_Bunny_4K.webm.480p.vp9.webm
mkdir -p runs/gs-pub
(setsid nohup python3 scripts/gui-smoke.py --image $I/plain.img --workdir $B/runs/gs-pub --kvm --smp 4 --mem 4G --pin 0-3 > runs/gs-pub.out 2>&1 < /dev/null &)
sleep 2; echo ok

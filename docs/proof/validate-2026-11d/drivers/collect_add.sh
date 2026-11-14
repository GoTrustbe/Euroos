cd /srv/euroos/final-runs; S=$PWD/scripts; O=$PWD/out
for r in soak2h:session-A-loaded-host soak2hb:session-B-quiet-host; do n=${r%%:*}; t=${r##*:}
  { echo "# chrome-mem-soak.py --no-video --free-video-seconds 0 --hours 2 --local-video-dir (vp9.webm: VP9+Opus) --local-seconds 60, public image (ui.tour=off;update.check=off), 8 GiB, 2 vCPU, KVM, $t"
    grep -a "chrome-mem-soak\] \|chrome-smoke\] \(PASS\|FAIL\|running\)" runs/$n.out | cut -c1-300
    echo "-- steps that failed:"; grep -a "\] FAIL" runs/$n.out | cut -c1-700
    echo "-- serial: FROZEN $(grep -ac 'timer-probe\] FROZEN' runs/$n/serial.log), aborts $(grep -ac '^\[abort\]' runs/$n/serial.log), spawn-refused $(grep -ac spawn-refused runs/$n/serial.log), oom $(grep -ac '^\[oom\]' runs/$n/serial.log), isolation $(grep -ac '^\[isolation\]' runs/$n/serial.log), NMI probes $(grep -ac 'NMI PROBE' runs/$n/serial.log), panic/fault lines $(grep -aEc 'KERNEL PANIC|DOUBLE FAULT|GENERAL PROTECTION FAULT|\[idt\] PAGE FAULT' runs/$n/serial.log)"
    echo "-- local clip (VP9+Opus) steps:"; grep -a "local video vp9+opus" runs/$n.out | grep -ac "\] ok"; grep -a "local video vp9+opus: sound" runs/$n.out | head -n 2 | cut -c1-260
    echo "-- mem-curve.py:"; python3 $S/mem-curve.py runs/$n/serial.log runs/$n.out --csv out/$n-curve.csv 2>&1 | sed -n '/after the warm-up/,/last census/p' | cut -c1-200
  } > $O/chrome-$n.txt
done
cp runs/soak2h/report.txt $O/chrome-soak2h-report.txt; cp runs/soak2hb/report.txt $O/chrome-soak2hb-report.txt
echo done2

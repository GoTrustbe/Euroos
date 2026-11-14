cd /srv/euroos/final-runs; S=$PWD/scripts; O=$PWD/out; mkdir -p $O
w=soak/w; L=$(ls $w/loadtest-serial-*.log | head -1)
cp $w/loadtest-results.csv $O/r630-soak-final.csv
cp $w/loadtest-results-ltstat.csv $O/r630-soak-final-ltstat.csv 2>/dev/null
awk '/interactive loop started/{exit} {print}' "$L" | grep -a -E "^\[[a-z0-9][a-z0-9_./-]*\]" | grep -a -E "✓|FAILED|⬜|→ OK|-> OK|^\[lk\] boot-total|^\[fork\]|^\[boot-time\]" > $O/r630-soak-final-boot.markers
{
  echo "# Soak, R630 KVM, self-test image of the merged tree: loadtest.py --kvm --smp 4 --nic e1000e --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hostfwd ... --hammer 4 --procs 32 --scenario soak --soak-churn-proc 8 --duration 7500"
  python3 $S/analyze_soak.py $w /dev/null 2>&1
  echo "serial: $(basename $L) $(stat -c %s $L) bytes"
  for k in "GLIBC-DYNAMIC-OK" "KERNEL PANIC" "GENERAL PROTECTION" "[idt] PAGE FAULT" "DOUBLE FAULT" "NMI PROBE" "[watchdog]" "refused to free" "out of memory: page tables" "no frames for page tables" "timer-probe] FROZEN" "[spawn-refused]" "[oom]"; do echo "  '$k': $(grep -acF "$k" $L)"; done
  echo "  [churn-proc] verdict lines: $(grep -ac '^\[churn-proc\]' $L), with FAILED: $(grep -a '^\[churn-proc\]' $L | grep -c FAILED)"
  echo "  FAILED lines after the desktop (count, line):"
  awk 'f{print} /interactive loop started/{f=1}' "$L" | grep -a -E "^\[[a-z0-9][a-z0-9_./-]*\]" | grep -a FAILED | tr -d '\r' | cut -c1-200 | sort | uniq -c | head -8
  grep -a "^\[lk\] boot-total order" $L | tr -d '\r' | tail -n 1
  echo "--- end of the harness output:"; tail -n 6 soak/out.txt | cut -c1-420
} > $O/r630-soak-final-summary.txt
python3 $S/soak_compare.py f98da76=base/validate-2026-11c/r630-soak-f98da76.csv final=$O/r630-soak-final.csv > $O/r630-soak-compare.txt 2>&1
cat $O/r630-soak-final-summary.txt | cut -c1-260; cat $O/r630-soak-compare.txt | cut -c1-250
echo; echo "== previous soak summary (11c f98da76) =="; head -n 12 base/validate-2026-11c/r630-soak-f98da76-summary.txt

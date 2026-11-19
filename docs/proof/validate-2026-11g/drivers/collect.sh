#!/bin/bash
# collect.sh: builds /srv/euroos/val-runs/g/proof/ (text evidence) from the run directories
B=/srv/euroos/val-runs/g; cd $B; P=$B/proof; rm -rf $P; mkdir -p $P/drivers
CRASH='KERNEL PANIC|GENERAL PROTECTION|\[idt\] PAGE FAULT|DOUBLE FAULT|panicked at'
{ for f in img/m-pub.img img/m-st.img img/b0-pub.img img/b0-st.img; do sha256sum $f; done; echo "public download euroos-x86_64-uefi.img.gz sha256 56c5f3becde470231ecc7e200ef37b5f7506e6eda9c9e3ab5942f1b06f2045d4 (checked)"; } > $P/images.txt
{ echo "# KVM self-test boots, HD-Audio, no NIC; cand = m (368919c), ref = b0 (b30adb6 = 2026.11.17 code). end/after/FAILED lines/crash lines; then the FAILED marker lines"
for c in 1G-s1 1G-s2 1G-s4 2G-s1 2G-s2 2G-s4 4G-s1 4G-s2 4G-s4 8G-s1 8G-s2 8G-s4; do for t in m b0; do l=kvm/$t/k-$c.log
 echo "== $t $c: $(cat kvm/$t/k-$c.res | cut -c1-120)"; grep -a "^\[[a-z0-9_./-]*\]" $l | grep FAILED | tr -d "\r" | cut -c1-200 | sed "s/^/    /"; done
 echo "-- markers-compare ref -> cand:"; python3 scripts/markers-compare.py compare kvm/b0/k-$c.log kvm/m/k-$c.log | head -n 6; done; } > $P/kvm-boots.txt 2>&1
for T in m b0 m2g b02g; do { echo "# TCG $T: per leg"; for l in tcg/$T/*.log; do n=$(basename $l .log); echo "$n desktop=$(grep -ac "interactive loop started" $l) wd=$(grep -ac "WATCHDOG TRIPPED" $l) crash=$(grep -aEc "$CRASH" $l) frozen=$(grep -ac FROZEN $l) vdso-mono-failed=$(grep -a "^\[vdso-mono\]" $l | grep -c FAILED) clock-drift-failed=$(grep -a "^\[clock-drift\]" $l | grep -c FAILED) failed-lines=$(grep -a "^\[[a-z0-9_./-]*\]" $l | grep -c FAILED)"; grep -a "^\[[a-z0-9_./-]*\]" $l | grep FAILED | tr -d "\r" | cut -c1-70 | sed "s/^/    /"; done; cat tcg/$T/queue.txt; cat tcg/$T/problems.txt 2>/dev/null; } > $P/tcg-$T.txt 2>&1; done
tail -n 22 q35m/matrix.out > $P/q35-matrix.txt
for k in on off leg fix id nosplit; do echo "gs-$k: $(grep -ah "gui-smoke\] \(PASS\|FAIL\)" runs/gs-$k.out)"; done > $P/gui-smoke.txt
{ echo "== 10 installs side by side (candidate public image, 8 CPUs)"; cat inst/m-*.out | cut -c1-110; echo "== wizard-install-test candidate"; tail -n 2 wizard.out; echo "== wizard-install-test reference (2026.11.17 public)"; tail -n 2 wizard-ref.out; for m in plain inject ro0; do echo "== ro-install-test $m (self-test image)"; grep -a "PASS\|FAIL" roi-$m.out | cut -c1-150; done; echo "== rename-persist candidate"; tail -n 3 rp.out; } > $P/installs.txt
cp upd.out $P/update-path.txt
{ for k in on off leg fix id nosplit; do echo "==== $k"; if [ $k = on ]; then L=kvm/m/k-2G-s2.log; else L=opt/o-$k.log; fi; grep -a "^\[bootopt\] aslr" $L | tr -d "\r"; grep -a "^\[aslr\] 64\|^\[aslr-ind\] 64\|^\[aslr-exec\]\|^\[arena-segs\]" $L | tr -d "\r" | cut -c1-300 | head -n 4; done
echo "==== Chromium launch/exec entropy per option (5 starts)"; for k in on off leg fix id nosplit; do d=/dev/shm/g11/opt-starts-$k; echo "-- $k"; python3 entropy6.py chrome $d/start-*/serial.log | head -n 2; done; } > $P/bootopts.txt 2>&1
{ python3 entropy6.py aslr ent2/*/aslr-*.txt; echo; cat ent2/*/summary.txt | grep -v "^done" | awk -F"|" '{print $1 "|" $6 "|" $7}' | cut -c1-200 | head -n 25; } > $P/entropy.txt 2>&1
{ echo "# Chromium: 20 starts (default), launch + exec"; python3 entropy6.py chrome /dev/shm/g11/starts20-m/start-*/serial.log; python3 execkeep-g.py | tail -n 1; echo "mmap window full lines: $(cat /dev/shm/g11/starts20-m/start-*/serial.log | grep -ac "mmap window full")"; } > $P/chrome-entropy.txt 2>&1
{ for n in starts20-m starts20-b0 sites3-m sites3-b0 interact-m wm480 opt-starts-on opt-starts-off opt-starts-leg opt-starts-fix opt-starts-id opt-starts-nosplit opt-sites-on opt-sites-off opt-sites-leg opt-sites-fix opt-sites-id opt-sites-nosplit; do echo "== $n: $(grep -ah "chrome-smoke\] \(PASS\|FAIL\)\|^\[wmvideo\] \(PASS\|FAIL\)" runs/$n.out | sed "s#report and screenshots in.*##" | sort | uniq -c | sort -rn | head -n 3 | cut -c1-110 | tr "\n" "|")"; done
echo; echo "== localvideo-m (chrome-smoke --only localvideo): $(tail -n 1 runs/localvideo-m.out)"; } > $P/chrome.txt
{ for n in msoak1 msoak2 msoak3 msoak-b0; do echo "== $n"; grep -ah "chrome-mem-soak\] \(round\|PASS\|FAIL\)\|chrome-smoke\] \(PASS\|FAIL\)" runs/$n.out | cut -c1-200; grep -ah "FAIL site" runs/$n.out | cut -c1-120; L=/dev/shm/g11/$n/serial.log; echo "serial: irqlock=$(grep -ac "\[irqlock\]" $L) frozen=$(grep -aci FROZEN $L) watchdog=$(grep -ac WATCHDOG $L) crash=$(grep -aEc "$CRASH" $L) oom=$(grep -ac "\[oom\]" $L) mmap-window-full=$(grep -ac "mmap window full" $L)"; grep -ah "local video.*sound reaches\|local video.*time increases" runs/$n.out | cut -c1-140 | head -n 4; done; } > $P/chrome-soaks.txt 2>&1
{ for T in m b0; do echo "== soak 1 (beside other lanes) $T"; tail -n 3 soak/$T/out.txt | cut -c1-400; done; python3 scripts/soak_compare.py cand=soak/m/w/loadtest-results.csv ref=soak/b0/w/loadtest-results.csv; echo; echo "#### side by side (soak2), same time, same load"; for T in m b0; do echo "== soak2 $T"; tail -n 3 soak2/$T/out.txt | cut -c1-400; done; python3 scripts/soak_compare.py cand=soak2/m/w/loadtest-results.csv ref=soak2/b0/w/loadtest-results.csv; for T in m b0; do echo "== serial $T soak2: crash=$(cat soak2/$T/w/loadtest-serial-*.log 2>/dev/null | grep -aEc "$CRASH") watchdog=$(cat soak2/$T/w/loadtest-serial-*.log 2>/dev/null | grep -ac "WATCHDOG TRIPPED") irqlock=$(cat soak2/$T/w/loadtest-serial-*.log 2>/dev/null | grep -ac "\[irqlock\]")"; done; } > $P/loadtest-soaks.txt 2>&1
cat > /tmp/g_perf.py <<'PY'
import re,glob,statistics as st
print("# boot time, quiet host, 12 pairs alternating (seconds from QEMU start)")
d={"m":[],"b0":[]};k={"m":[],"b0":[]}
for l in open("bt-quiet.txt"):
    print(l.rstrip())
    m=re.match(r"(m|b0)-\d+: kernel-entry ([\d.]+)s desktop ([\d.]+)s",l)
    if m: d[m.group(1)].append(float(m.group(3))); k[m.group(1)].append(float(m.group(2)))
for t in d: print(t,len(d[t]),"desktop median %.2f (%.2f-%.2f) kernel-entry median %.2f"%(st.median(d[t]),min(d[t]),max(d[t]),st.median(k[t])))
print()
print("# key latency (chrome-smoke --only perf, 30 keys, fbread = verdict method), 6 rounds, 4 guests at once, CPU sets swapped every round")
res={}
for f in sorted(glob.glob("runs/kl*-*.out")):
    r,t,s=re.match(r"runs/kl(\d+)-(m|b0)-(\d)\.out",f).groups(); t_=open(f,errors="replace").read()
    fb=re.search(r"keys, fbread: median (\d+) ms",t_); sd=re.search(r"\(until 11e\): median (\d+)",t_)
    res.setdefault((t,s),[]).append((int(r),int(fb.group(1)) if fb else None,int(sd.group(1)) if sd else None))
for k_,v in sorted(res.items()):
    fbs=[x[1] for x in v if x[1]]; sds=[x[2] for x in v if x[2]]
    print("%s smp%s fbread %s median %s | screendump %s median %s"%("cand" if k_[0]=="m" else "ref",k_[1],fbs,st.median(fbs),sds,st.median(sds) if sds else None))
print()
print("# [fair] hogtest 5 + churn 8, 4 vCPU")
for t in ("m","b0"):
    print("==",t)
    for n in range(1,11):
        try: print("%s-%d: %s"%(t,n,open("fair/%s-%d.txt"%(t,n),errors="replace").read().replace("\r","").strip().splitlines()[-1][:200]))
        except Exception as e: print("%s-%d: -"%(t,n))
PY
python3 /tmp/g_perf.py > $P/perf.txt 2>&1
cat > /tmp/g_timing.py <<'PY'
import re,glob,statistics as st
print("# Chromium start (first page painted and settled, seconds after Enter; the harness caps settle at 40 s)")
for t in ("starts20-m","starts20-b0"):
    print("==",t)
    for f in sorted(glob.glob("/dev/shm/g11/%s/start-*/report.txt"%t)):
        m=re.search(r"settled after [0-9.]+ s \(seconds after Enter: [^)]*",open(f).read()); print(m.group(0)[:170] if m else "-")
print()
print("# sites, 3 rounds (seconds until load finished), per site")
def sites(t):
    d={}
    for m in re.finditer(r"site (\S+): .*? after ([\d.]+) s, load finished ([\d.]+) s later",open("/dev/shm/g11/"+t+"/report.txt").read()): d.setdefault(m.group(1),[]).append(round(float(m.group(2))+float(m.group(3)),1))
    return d
a=sites("sites3-m"); b=sites("sites3-b0")
for k_ in a: print(k_,"cand %s median %.1f | ref %s median %.1f"%(a[k_],st.median(a[k_]),b.get(k_),st.median(b.get(k_,[0]))))
PY
python3 /tmp/g_timing.py > $P/chrome-timing.txt 2>&1
{ echo "# drift against the host, 30 min, KVM (vdrift.py; first sample dropped, least squares). cand: vDSO clock; ref: tick count x 10 ms (MONOTONIC column) and HPET (REALTIME column) because 2026.11.17 prints no vDSO clock"; for n in m-rest m-load b0-rest b0-load; do python3 drift-ana.py drift/$n/samples.txt 2>&1 | cut -c1-330; done
echo; echo "# [clock-drift] / [vdso-mono] of the 16 repeat boots at 8 GiB 1 vCPU (8 cand, 8 ref)"; for l in kvm/rep/*.log; do echo "$(basename $l .log): clock-drift $(grep -a "^\[clock-drift\] the clock" $l | tr -d "\r" | grep -o "\-> [A-Z]*") crash=$(grep -aEc "$CRASH" $l)"; done
echo; echo "# [clock-drift] and [vdso-mono] in all cand KVM boots"; for l in kvm/m/k-*.log; do echo "$(basename $l .log): $(grep -a "^\[clock-drift\] the clock" $l | tr -d "\r" | grep -o "\-> [A-Z]*")"; done; } > $P/clock.txt 2>&1
cp s1a.sh s1b.sh s2.sh s3a.sh s3b.sh s4.sh upd.sh upd-peer.py upd-drive.py slotwrite.py inst-keep.py vdrift-ref.py execkeep-g.py tcgq.sh kboot-hda.sh collect.sh $P/drivers/ 2>/dev/null
ls $P

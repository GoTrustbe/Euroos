#!/bin/bash
# .16 (TCG): one chrome-boot run of the base (b9a5755) and one of the final
# commit, side by side, with the chrome pack opened snapshot=on. After
# scripts/chrome-iter.sh, with its own work directory and no shared sockets;
# each QEMU is stopped by its own PID.
export PATH=$HOME/.cargo/bin:$PATH
R=/opt/euroos/rand-runs; W=/opt/euroos/wt-rand
PACK=/opt/euroos/packs/chrome-pack2.img
mkdir -p $R/chrome
cd $W
HEAD_REV=$(git rev-parse --short HEAD)
build() { # name
  CARGO_BUILD_JOBS=4 nice -n 15 ./scripts/build.sh chrome > $R/logs/build-chrome-$1.log 2>&1
  grep -q "^==> done: eurokernel.img" $R/logs/build-chrome-$1.log || { echo "build $1 FAILED" >> $R/chrome/status.txt; return 1; }
  cp --sparse=always eurokernel.img $R/img/chrome-$1.img
}
build after || exit 1
git checkout -q b9a5755 || { echo "checkout base failed" >> $R/chrome/status.txt; exit 1; }
build base
rm -f userland/randprobe.elf.sig
git checkout -q feature/linux-rand || echo "CHECKOUT BACK FAILED" >> $R/chrome/status.txt
echo "tree back at $(git rev-parse --short HEAD) (was $HEAD_REV)" >> $R/chrome/status.txt
run() { # name pins
  local n=$1 pins=$2 LOG=$R/chrome/$1.log
  : > $LOG
  taskset -c $pins nice -n 10 qemu-system-x86_64 -machine q35 -m 3584M -cpu qemu64,+smep,+smap \
    -bios /usr/share/ovmf/OVMF.fd \
    -drive format=raw,file=$R/img/chrome-$n.img,snapshot=on \
    -drive format=raw,file=$PACK,if=virtio,snapshot=on \
    -display none -serial file:$LOG -no-reboot > $R/chrome/$n.qemu.out 2>&1 < /dev/null &
  local Q=$! START=$SECONDS why=timeout
  while [ $((SECONDS - START)) -lt 2400 ]; do
    grep -aq "chrome-run\] DONE" $LOG && { why=done; break; }
    kill -0 $Q 2>/dev/null || { why=exit; break; }
    sleep 10
  done
  if [ -n "$Q" ] && [ "$Q" -gt 1 ]; then kill "$Q" 2>/dev/null; wait "$Q" 2>/dev/null; fi
  echo "$n: $why after $((SECONDS - START)) s" >> $R/chrome/status.txt
}
run base 8-9 &
run after 10-11 &
wait
echo "done $(date +%T)" > $R/chrome.done

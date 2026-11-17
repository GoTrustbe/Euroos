#!/bin/bash
# mkvar.sh: image variants by boot option (euroos-boot.conf copied onto a copy of the image; nothing rebuilt).
cd /srv/euroos/aslr6-runs/img
var() { SRC=$1 DST=$2 CONF=$3; cp $SRC.img $DST.img; printf "%s\n" "$CONF" | tr ";" "\n" > /tmp/aslr6.conf; mcopy -o -i $DST.img /tmp/aslr6.conf ::/EFI/BOOT/euroos-boot.conf; rm -f /tmp/aslr6.conf; }
for T in s6 base; do
  var $T-st $T-st-off "aslr=off"; var $T-st $T-st-leg "aslr=legacy"
  var $T-st $T-st-fix "aslr.arena=fixed"; var $T-st $T-st-id "aslr.arena=identity"
  var $T-pub $T-br "ui.tour=off;update.check=off"; var $T-pub $T-br-off "ui.tour=off;update.check=off;aslr=off"; var $T-pub $T-br-leg "ui.tour=off;update.check=off;aslr=legacy"
  var $T-pub $T-pub-off "aslr=off"; var $T-pub $T-pub-leg "aslr=legacy"
done
var s6-st s6-st-nosplit "aslr.split=off"; var s6-pub s6-br-nosplit "ui.tour=off;update.check=off;aslr.split=off"
ls

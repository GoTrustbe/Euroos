#!/bin/bash
W=/opt/euroos/integ2m-work; cd $W
for n in m b410 b400; do ./mkimg.sh $W/src/$n st $W/img/$n-st.img > build-$n-st.log 2>&1; echo "$n-st rc=$?" >> build-status.txt; done
./mkimg.sh $W/src/m pub $W/img/m-pub.img > build-m-pub.log 2>&1; echo "m-pub rc=$?" >> build-status.txt
EUROOS_BOOTOPTS="ui.tour=off;update.check=off" ./mkimg.sh $W/src/m pub $W/img/m-br.img > build-m-br.log 2>&1; echo "m-br rc=$?" >> build-status.txt
touch p1.done

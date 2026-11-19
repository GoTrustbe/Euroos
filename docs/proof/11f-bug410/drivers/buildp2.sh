#!/bin/bash
W=/opt/euroos/integ2b410-work; cd $W
while [ ! -f p1.done ]; do sleep 20; done
./mkimg.sh $W/src/fix st $W/img/fix-st.img > build-fix-st.log 2>&1; echo "fix-st rc=$?" >> build-status.txt
./mkimg.sh $W/src/fix pub $W/img/fix-pub.img > build-fix-pub.log 2>&1; echo "fix-pub rc=$?" >> build-status.txt
EUROOS_BOOTOPTS="ui.tour=off;update.check=off" ./mkimg.sh $W/src/fix pub $W/img/fix-br.img > build-fix-br.log 2>&1; echo "fix-br rc=$?" >> build-status.txt
touch p2.done

#!/bin/bash
W=/opt/euroos/integ2b410-work; cd $W
for c in d18fa36 4df79a3 5ec0c28; do ./mkimg.sh $W/src/$c st $W/img/$c-st.img > build-$c-st.log 2>&1; echo "$c-st rc=$?" >> build-status.txt; done
./mkimg.sh $W/src/d18fa36 pub $W/img/d18fa36-pub.img > build-d18fa36-pub.log 2>&1; echo "d18fa36-pub rc=$?" >> build-status.txt
EUROOS_BOOTOPTS="ui.tour=off;update.check=off" ./mkimg.sh $W/src/d18fa36 pub $W/img/d18fa36-br.img > build-d18fa36-br.log 2>&1; echo "d18fa36-br rc=$?" >> build-status.txt
touch p1.done

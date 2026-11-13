#!/bin/bash
# The public image of the base: c6f033f plus the [rename] test commit (the probe in /bin), nothing else.
# A throwaway detached worktree that shares the target directory of wt-chrome (one target directory).
export PATH=$HOME/.cargo/bin:$PATH
R=/opt/euroos/validate-runs; X=/opt/euroos/wt-validate-base; M=/opt/euroos/eurokernel
cd /opt/euroos/wt-chrome || exit 1
git worktree add -q --detach $X c6f033f || exit 1
cd $X || exit 1
git cherry-pick -n de92f08 > $R/build/pubbase.pick 2>&1; echo "pick rc=$?" >> $R/build/pubbase.pick
ln -s /opt/euroos/wt-chrome/target target
ln -s $M/toolchain/eupkg/keys/dev.key toolchain/eupkg/keys/dev.key
cp -n $M/userland/*.elf userland/ 2>/dev/null
W=$X $R/build.sh image pubbase
rm -f toolchain/eupkg/keys/dev.key target
cd /opt/euroos/wt-chrome && git worktree remove --force $X
echo "done $(date +%T)" > $R/build/pubbase.chain

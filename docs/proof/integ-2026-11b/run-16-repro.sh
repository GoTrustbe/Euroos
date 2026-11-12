#!/bin/bash
# The CI job "Reproducible build": repro-build.sh twice, the second after removing the .efi and touching main.rs.
export PATH=$HOME/.cargo/bin:$PATH
cd /opt/euroos/wt-integ2
(
flock 9
echo "# $(git rev-parse --short HEAD), $(date -u +%F)"
CARGO_BUILD_JOBS=4 nice -n 15 ./scripts/repro-build.sh 2>&1 | grep "^==>"
H1=$(sha256sum target/x86_64-unknown-uefi/release/eurokernel.efi | cut -d' ' -f1)
L1=$(sha256sum target/x86_64-unknown-uefi/release/loader.efi | cut -d' ' -f1)
rm -f target/x86_64-unknown-uefi/release/eurokernel.efi
touch kernel/src/main.rs
CARGO_BUILD_JOBS=4 nice -n 15 ./scripts/repro-build.sh 2>&1 | grep "^==>"
H2=$(sha256sum target/x86_64-unknown-uefi/release/eurokernel.efi | cut -d' ' -f1)
L2=$(sha256sum target/x86_64-unknown-uefi/release/loader.efi | cut -d' ' -f1)
echo "build #1: $H1 (loader $L1)"
echo "build #2: $H2 (loader $L2)"
echo "paths of the build machine in the kernel: $(strings target/x86_64-unknown-uefi/release/eurokernel.efi | grep -c '/root/')"
test "$H1" = "$H2" && test "$L1" = "$L2" && echo "REPRODUCIBLE" || echo "NON-REPRODUCIBLE"
) 9>/tmp/eurokernel-build.lock

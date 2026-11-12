#!/usr/bin/env bash
# 3E-4: a REPRODUCIBLE kernel+loader build.
#
# Every build is deterministic for one checkout on one machine since 2026-11:
# .cargo/config.toml passes /Brepro, so lld-link writes a content hash, not the
# wall clock, into the PE COFF TimeDateStamp and the debug directory (the only
# bytes in which two builds of one commit differed). What this script adds is
# independence of WHERE the build runs:
#   * --remap-path-prefix   no absolute path of the checkout, of ~/.cargo or of
#                           ~/.rustup in the binary (panic locations of the
#                           dependencies and of core/alloc carry them)
#   * EUROOS_BUILD_VERSION  pinned to the commit date instead of today
#
# The remap flags are ADDED to the flags of .cargo/config.toml (cargo joins the
# two arrays). They used to be passed in RUSTFLAGS, which replaces the config
# flags: this script then built a kernel without panic=abort, frame pointers,
# the link map and the three forced soft crypto back ends, i.e. not the kernel
# scripts/build.sh builds. A RUSTFLAGS in the environment still does that, so
# it is refused here.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ -n "${RUSTFLAGS:-}" ] || [ -n "${CARGO_ENCODED_RUSTFLAGS:-}" ]; then
  echo "ERROR: RUSTFLAGS is set; it would replace the flags in .cargo/config.toml. Unset it." >&2
  exit 1
fi
CFG="target.x86_64-unknown-uefi.rustflags=[\"--remap-path-prefix=$PWD=/build\", \"--remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo\", \"--remap-path-prefix=${RUSTUP_HOME:-$HOME/.rustup}=/rustup\"]"
# SOURCE_DATE_EPOCH pins any remaining timestamp-derived content to the commit.
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || echo 0)}"
export EUROOS_BUILD_VERSION="${EUROOS_BUILD_VERSION:-$(date -u -d "@$SOURCE_DATE_EPOCH" +%Y%m%d)}"

echo "==> reproducible build (SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH, EUROOS_BUILD_VERSION=$EUROOS_BUILD_VERSION)"
cargo build --config "$CFG" --release -p kernel --target x86_64-unknown-uefi \
  -Z build-std=core,compiler_builtins,alloc \
  -Z build-std-features=compiler-builtins-mem
# The loader link writes target/kernel.map too (one flag for the target): keep the
# kernel's as target/eurokernel.map (see scripts/build.sh).
if grep -q 'eurokernel-[0-9a-f]*\.eurokernel\.' target/kernel.map 2>/dev/null; then
  cp -f target/kernel.map target/eurokernel.map
fi
cargo build --config "$CFG" --release -p loader --target x86_64-unknown-uefi \
  -Z build-std=core,compiler_builtins,alloc \
  -Z build-std-features=compiler-builtins-mem

K=target/x86_64-unknown-uefi/release/eurokernel.efi
echo "==> eurokernel.efi sha256: $(sha256sum "$K" | cut -d' ' -f1)"
echo "==> loader.efi     sha256: $(sha256sum target/x86_64-unknown-uefi/release/loader.efi | cut -d' ' -f1)"

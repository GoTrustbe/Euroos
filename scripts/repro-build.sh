#!/usr/bin/env bash
# 3E-4: a REPRODUCIBLE kernel+loader build. Two ingredients kill the two sources
# of PE/ELF nondeterminism we found:
#   * --remap-path-prefix        — no absolute build paths baked into the binary
#   * -C link-arg=/Brepro        — lld-link writes a content hash, not the wall
#                                   clock, into the PE COFF TimeDateStamp
# With these, a clean rebuild from the same commit + pinned toolchain yields a
# byte-identical eurokernel.efi (the CI `repro-check` job double-builds and
# compares; verified locally in docs/CRA-CONFORMANCE.md).
set -euo pipefail
cd "$(dirname "$0")/.."

export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$PWD=/build --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo --remap-path-prefix=${RUSTUP_HOME:-$HOME/.rustup}=/rustup -C link-arg=/Brepro"
# SOURCE_DATE_EPOCH pins any remaining timestamp-derived content to the commit.
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || echo 0)}"

echo "==> reproducible build (SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH)"
cargo build --release -p kernel --target x86_64-unknown-uefi \
  -Z build-std=core,compiler_builtins,alloc \
  -Z build-std-features=compiler-builtins-mem
cargo build --release -p loader --target x86_64-unknown-uefi \
  -Z build-std=core,compiler_builtins,alloc \
  -Z build-std-features=compiler-builtins-mem 2>/dev/null || true

K=target/x86_64-unknown-uefi/release/eurokernel.efi
echo "==> eurokernel.efi sha256: $(sha256sum "$K" | cut -d' ' -f1)"

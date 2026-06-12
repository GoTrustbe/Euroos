#!/usr/bin/env bash
# Tier 1: logica-tests op de host (geen VM). Tier 2: kernel compileert + boot.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> [tier 1] cargo test (host, std) — alle library-crates"
cargo test

echo "==> [tier 1] clippy (strict)"
cargo clippy -p eurofs -p euronet -- -D warnings

echo "==> [tier 2] kernel UEFI-build"
cargo kbuild-release

echo "==> [tier 2] image + QEMU-boot + screenshot"
./scripts/build.sh release
python3 scripts/screenshot.py eurokernel.img boot.png 28

echo "==> alles groen."

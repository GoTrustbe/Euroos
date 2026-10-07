#!/usr/bin/env bash
# Publish the EuroOS app store (sprint N7): build the SIGNED index for the
# example packages and copy index + packages to https://euro-os.eu/apps/.
# Run as root on the build server (needs toolchain/eupkg/keys/dev.key).
set -euo pipefail
cd "$(dirname "$0")/.."
OUT=${OUT:-/var/www/euro-os.eu/apps}
PKGS=(toolchain/eupkg/hello-euro-1.0.0.eupkg toolchain/eupkg/hello-euro-wasm-1.0.0.eupkg
      toolchain/eupkg/hello-cli-1.0.0.eupkg toolchain/eupkg/hello-x11-1.0.0.eupkg)
[ -f sdk/c/hello-euro-c-1.0.0.eupkg ] && PKGS+=(sdk/c/hello-euro-c-1.0.0.eupkg)
mkdir -p "$OUT"
python3 toolchain/eupkg/make-app-index.py --out "$OUT" --base /apps --pub sdk/examples/keys/example-dev.pub "${PKGS[@]}"
chown -R www-data:www-data "$OUT"
chmod 755 "$OUT"; chmod 644 "$OUT"/*
echo "published: https://euro-os.eu/apps/index.json ($(ls "$OUT"/*.eupkg | wc -l) packages)"

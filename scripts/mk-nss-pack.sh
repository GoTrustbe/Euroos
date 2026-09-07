#!/usr/bin/env bash
# Build a small EuroPack disk with the NSS runtime chrome needs for HTTPS.
#
# Chrome verifies server certificates through NSS, and NSS loads its software
# token and trust roots as SEPARATE shared objects at runtime - so they are not
# in the library closure a linker reports, and they were missing from the chrome
# pack. Without them NSS refuses to initialise, certificate verification never
# finishes, and every TLS handshake stalls after the server's first flight: the
# page loads to ready=complete with an empty document.
#
# Attach the result as an extra virtio disk; the kernel scans every disk for a
# EuroPack volume, so it needs no other wiring.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${1:-nss-pack.img}"
L=/usr/lib/x86_64-linux-gnu
SQLITE=$(find "$L" -maxdepth 1 -name 'libsqlite3.so.0*' | head -1)
[ -n "$SQLITE" ] || { echo "libsqlite3 not found"; exit 1; }
python3 scripts/mkeuropack.py "$OUT" \
  "$L/libsoftokn3.so:/lib/x86_64-linux-gnu/libsoftokn3.so" \
  "$L/libfreebl3.so:/lib/x86_64-linux-gnu/libfreebl3.so" \
  "$L/libfreeblpriv3.so:/lib/x86_64-linux-gnu/libfreeblpriv3.so" \
  "$L/libnssckbi.so:/lib/x86_64-linux-gnu/libnssckbi.so" \
  "$L/libsmime3.so:/lib/x86_64-linux-gnu/libsmime3.so" \
  "$L/libssl3.so:/lib/x86_64-linux-gnu/libssl3.so" \
  "$L/libnssdbm3.so:/lib/x86_64-linux-gnu/libnssdbm3.so" \
  "$SQLITE:/lib/x86_64-linux-gnu/libsqlite3.so.0" \
  "$L/libnss3.so:/lib/x86_64-linux-gnu/libnss3.so" \
  "$L/libnssutil3.so:/lib/x86_64-linux-gnu/libnssutil3.so" \
  "$L/libnspr4.so:/lib/x86_64-linux-gnu/libnspr4.so" \
  "$L/libplc4.so:/lib/x86_64-linux-gnu/libplc4.so" \
  "$L/libplds4.so:/lib/x86_64-linux-gnu/libplds4.so"

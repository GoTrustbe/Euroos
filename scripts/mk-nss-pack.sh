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
#
# VERSION RULE. The chrome pack already ships libnss3, libnssutil3, libnspr4,
# libplc4, libplds4 and libsmime3, and for a path that both packs serve the
# chrome pack wins. So the software token and its friends in THIS pack must
# come from the same NSS generation as the chrome pack's libnssutil3, or the
# certificate verifier aborts the moment a TLS handshake reaches the server's
# certificate: "libnssutil3.so: version `NSSUTIL_3.108' not found (required by
# libsoftokn3.so)", nss_error -5925 (a pack rebuilt from this build server's
# NSS 3.120 against a chrome pack from Ubuntu noble's NSS 3.98). Build from the
# matching Ubuntu package, not from /usr/lib:
#
#   sudo NSS_DEB=/path/to/libnss3_3.98-1build1_amd64.deb scripts/mk-nss-pack.sh out.img
#
# (sudo: the pack is Ed25519-signed with toolchain/eupkg/keys/dev.key, mode 0600.)
#
# The .deb is extracted to a temp dir and its libraries are packed. Without
# NSS_DEB the script falls back to /usr/lib and says so; that is only right on a
# machine whose NSS matches the chrome pack.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${1:-nss-pack.img}"
if [ -n "${NSS_DEB:-}" ]; then
  T=$(mktemp -d); trap 'rm -rf "$T"' EXIT
  dpkg-deb -x "$NSS_DEB" "$T"
  L="$T/usr/lib/x86_64-linux-gnu"
  echo "==> NSS libraries from $(basename "$NSS_DEB")"
else
  L=/usr/lib/x86_64-linux-gnu
  echo "==> WARNING: packing this machine's NSS ($L); it must match the chrome pack's NSS generation"
fi
SQLITE=$(find /usr/lib/x86_64-linux-gnu -maxdepth 1 -name 'libsqlite3.so.0*' | head -1)
[ -n "$SQLITE" ] || { echo "libsqlite3 not found"; exit 1; }
echo "==> softokn requires: $(objdump -T "$L/libsoftokn3.so" | grep -oE 'NSSUTIL_[0-9.]+' | sort -V | uniq | tail -1)"
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
  "$L/libnssutil3.so:/lib/x86_64-linux-gnu/libnssutil3.so"

#!/usr/bin/env bash
# ============================================================================
#  publish-release.sh — build today's EuroOS, sign it, and publish it as both
#  the download images (euro-os.eu/download/) and the over-the-air update
#  channel (euro-os.eu/update/). Runs on the build server as root.
#
#    scripts/server/publish-release.sh            # version = today (YYYYMMDD)
#    EUROOS_BUILD_VERSION=20260908 scripts/server/publish-release.sh
#    EUROOS_SIGN_KEY=/dev/shm/rotation.key scripts/server/publish-release.sh
#                                                 # (used by rotate-signing-key.sh)
#    scripts/server/publish-release.sh --no-try   # leave the live-try VM image alone
#
#  Every installed EuroOS that checks the channel sees the new version within
#  6 hours (policy `ask`: notification; `auto`: staged + reboot; `manual`: notice).
# ============================================================================
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WEB="${EUROOS_WEBROOT:-/var/www/euro-os.eu}"
OUT="${EUROOS_RELEASE_DIR:-/var/lib/euroos/release}"   # root-owned, not /tmp
TRY_IMG="${EUROOS_TRY_IMG:-/opt/eurovnc/base.img}"
UPDATE_TRY=1
[ "${1:-}" = "--no-try" ] && UPDATE_TRY=0
[ "$(id -u)" = 0 ] || { echo "run as root (the daily key is root-only)"; exit 1; }

export EUROOS_BUILD_VERSION="${EUROOS_BUILD_VERSION:-$(date -u +%Y%m%d)}"
[[ "$EUROOS_BUILD_VERSION" =~ ^20[0-9]{6}$ ]] && date -u -d "$EUROOS_BUILD_VERSION" >/dev/null 2>&1 \
  || { echo "EUROOS_BUILD_VERSION must be a YYYYMMDD date (clients use it as a monotonic watermark): $EUROOS_BUILD_VERSION"; exit 1; }
install -d -m 700 "$(dirname "$OUT")"
export VERSION="${VERSION:-$(echo "$EUROOS_BUILD_VERSION" | sed -E 's/^([0-9]{4})([0-9]{2})([0-9]{2})$/\1.\2.\3/')}"
echo "==> publish-release: version $VERSION (channel $EUROOS_BUILD_VERSION), signing key ${EUROOS_SIGN_KEY:-daily dev.key}"

cd "$ROOT"
./scripts/build.sh image
./scripts/release-web.sh "$OUT"

# A rotation release is kept as the RESCUE channel too: channel/rescue.json points
# at images/rescue.efi, both signed with the rotation key, and both survive later
# daily publishes (rsync protects them). A kernel whose stable manifest no longer
# verifies (it only trusts a retired daily key + rotation) falls back to rescue.json.
if [ "${EUROOS_RESCUE:-0}" = 1 ]; then
  [ -n "${EUROOS_SIGN_KEY:-}" ] || { echo "EUROOS_RESCUE=1 needs EUROOS_SIGN_KEY (the rotation key)"; exit 1; }
  python3 "$ROOT/toolchain/update-server/make-channel.py" --kernel "$ROOT/target/x86_64-unknown-uefi/release/eurokernel.efi" \
    --version "$EUROOS_BUILD_VERSION" --channel rescue --image-name rescue.efi --out "$OUT/update" --key "$EUROOS_SIGN_KEY" --expires-days 3650
fi

# Refuse to publish a channel the running build cannot have produced.
python3 - "$OUT/update/channel/stable.json" "$EUROOS_BUILD_VERSION" <<'PY'
import json, sys
m = json.load(open(sys.argv[1]))
assert m["version"] == int(sys.argv[2]), f"channel version {m['version']} != build {sys.argv[2]}"
PY

echo "==> publishing to $WEB"
install -d -o www-data -g www-data "$WEB/download" "$WEB/update"
rsync -a --delete --filter="P channel/rescue.json*" --filter="P images/rescue.efi*" "$OUT/update/" "$WEB/update/"
rsync -a "$OUT/"{euroos-x86_64-uefi.img.gz,euroos-preview.qcow2.gz,euroos-preview.vmdk.gz,euroos-preview-x86_64.tar.gz,SHA256SUMS,VERSION} "$WEB/download/"
chown -R www-data:www-data "$WEB/download" "$WEB/update"

if [ "$UPDATE_TRY" = 1 ] && [ -d "$(dirname "$TRY_IMG")" ]; then
  echo "==> refreshing live-try image $TRY_IMG"
  cp "$ROOT/eurokernel.img" "$TRY_IMG.new" && mv -f "$TRY_IMG.new" "$TRY_IMG"
  systemctl try-restart eurovnc-orchestrator.service 2>/dev/null || true
fi
echo "==> published: $(cat "$OUT/update/channel/stable.json")"

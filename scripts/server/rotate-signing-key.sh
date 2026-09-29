#!/usr/bin/env bash
# ============================================================================
#  rotate-signing-key.sh — replace a leaked (or simply old) daily signing key.
#
#  Every installed EuroOS trusts two keys: the daily key (dev.pub) that signs
#  ordinary releases, and the rotation key (rotation.pub) that is kept encrypted
#  and signs nothing day to day. Rotation = one release, signed with the
#  rotation key, whose kernel embeds a NEW daily public key. Installed systems
#  accept it (rotation.pub is trusted), boot it, and from then on only accept
#  the new daily key. The old daily key is dead the moment they update.
#
#  Systems that are offline during the rotation window are not stranded: the
#  rotation release stays published as channel/rescue.json (+ its image) and a
#  kernel whose stable manifest no longer verifies falls back to it.
#
#  Needs: the passphrase of /root/euroos-keys/rotation.key.age (password manager).
#  Runs on the build server as root, interactive (age asks for the passphrase).
#
#    scripts/server/rotate-signing-key.sh
# ============================================================================
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
KEYDIR="${EUROOS_KEYDIR:-/root/euroos-keys}"
KEYS="$ROOT/toolchain/eupkg/keys"
[ "$(id -u)" = 0 ] || { echo "run as root"; exit 1; }
[ -f "$KEYDIR/rotation.key.age" ] || { echo "no $KEYDIR/rotation.key.age (run toolchain/update-server/gen-rotation-key.py first)"; exit 1; }
cmp -s "$KEYDIR/rotation.pub" "$KEYS/rotation.pub" || { echo "repo rotation.pub differs from $KEYDIR/rotation.pub: installed systems would not trust this release"; exit 1; }
if [ -e /root/euroos-rotation-passphrase.txt ]; then
  echo "refusing: /root/euroos-rotation-passphrase.txt still exists. The rotation key only protects"
  echo "against a compromised server while its passphrase is NOT on the server. Move it to your"
  echo "password manager and: shred -u /root/euroos-rotation-passphrase.txt"
  exit 1
fi

# 1. Decrypt the rotation key into RAM-backed storage only, wiped on exit.
TMP="$(mktemp -d -p /dev/shm euroos-rotate.XXXXXX)"; chmod 700 "$TMP"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
RETIRED="$KEYDIR/retired"; install -d -m 700 "$RETIRED"
DONE=0
cleanup() {
  # Whatever happened, never leave the repo with a daily key nobody trusts:
  # until the rotation release is published, the OLD pair is the valid one.
  if [ "$DONE" != 1 ] && [ -f "$TMP/old.key" ]; then
    cp -f "$TMP/old.key" "$KEYS/dev.key"; cp -f "$TMP/old.pub" "$KEYS/dev.pub"; chmod 600 "$KEYS/dev.key"
    echo "!! rotation did not complete: the previous daily key has been restored (nothing was published)"
  fi
  shred -u "$TMP"/* 2>/dev/null || true; rm -rf "$TMP"
}
trap cleanup EXIT
echo "==> passphrase of the rotation key:"
age -d -o "$TMP/rotation.key" "$KEYDIR/rotation.key.age"
chmod 600 "$TMP/rotation.key"
python3 - "$TMP/rotation.key" "$KEYS/rotation.pub" <<'PY'
import sys
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives import serialization as s
pub = Ed25519PrivateKey.from_private_bytes(open(sys.argv[1],"rb").read()).public_key().public_bytes(s.Encoding.Raw, s.PublicFormat.Raw)
assert pub == open(sys.argv[2],"rb").read(), "decrypted rotation key does not match rotation.pub"
print("rotation key OK, fingerprint", pub[:8].hex())
PY

# 2. Keep the old daily pair aside (restored by the trap if anything fails),
#    generate the new one in place (dev.key git-ignored, dev.pub embedded by the build).
cp "$KEYS/dev.key" "$TMP/old.key"; cp "$KEYS/dev.pub" "$TMP/old.pub"
python3 "$ROOT/toolchain/eupkg/gen-dev-key.py"
echo "==> new daily key fingerprint: $(python3 -c "import sys;print(open(sys.argv[1],'rb').read()[:8].hex())" "$KEYS/dev.pub")"

# 3. One release signed with the rotation key. The kernel inside embeds the new
#    dev.pub, so after this every installed system trusts new-daily + rotation.
#    EUROOS_RESCUE=1 makes publish-release.sh also keep it as channel/rescue.json.
EUROOS_SIGN_KEY="$TMP/rotation.key" EUROOS_RESCUE=1 "$ROOT/scripts/server/publish-release.sh" "$@"

# 4. Only now is the old key retired for good.
cp "$TMP/old.key" "$RETIRED/dev.key.$STAMP"; cp "$TMP/old.pub" "$RETIRED/dev.pub.$STAMP"; chmod 600 "$RETIRED/dev.key.$STAMP"
DONE=1
echo "==> rotation published. Commit toolchain/eupkg/keys/dev.pub. The retired key is in $RETIRED/ (delete it once every system has updated)."

#!/usr/bin/env bash
# ============================================================================
#  release-web.sh — build the public download artifacts from the latest image.
#
#  Takes the fresh bootable image (eurokernel.img) and produces the four
#  download variants for euro-os.eu/try/, plus SHA256SUMS and a VERSION
#  file. Reproducible: same image → same artifacts (gzip -n, no
#  timestamps in the tar). Deploying to the webroot is done separately by the caller.
#
#  Usage:  ./scripts/release-web.sh [OUT_DIR]
#            (default OUT_DIR = /tmp/euroos-release)
# ============================================================================
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC_IMG="${SRC_IMG:-$ROOT/eurokernel.img}"
ASSETS="$ROOT/scripts/release-assets"
OVMF="${OVMF:-/usr/share/ovmf/OVMF.fd}"
OUT="${1:-/tmp/euroos-release}"
VERSION="${VERSION:-$(date -u +%Y.%m.%d)}"

[ -f "$SRC_IMG" ] || { echo "image not found: $SRC_IMG (build first with ./scripts/build.sh release)"; exit 1; }
[ -f "$OVMF" ]    || { echo "OVMF not found: $OVMF (install 'ovmf')"; exit 1; }

echo "==> EuroOS web release $VERSION from $(basename "$SRC_IMG") ($(du -h "$SRC_IMG" | cut -f1))"
rm -rf "$OUT"; mkdir -p "$OUT"
WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
cp "$SRC_IMG" "$WORK/euroos.img"

# 1. Raw UEFI image (gzip, deterministic with -n).
echo "  -> euroos-x86_64-uefi.img.gz"
gzip -9 -n -c "$WORK/euroos.img" > "$OUT/euroos-x86_64-uefi.img.gz"

# 2. qcow2 cloud image.
echo "  -> euroos-preview.qcow2.gz"
qemu-img convert -f raw -O qcow2 "$WORK/euroos.img" "$WORK/euroos-preview.qcow2"
gzip -9 -n -c "$WORK/euroos-preview.qcow2" > "$OUT/euroos-preview.qcow2.gz"

# 3. vmdk (VirtualBox).
echo "  -> euroos-preview.vmdk.gz"
qemu-img convert -f raw -O vmdk "$WORK/euroos.img" "$WORK/euroos-preview.vmdk"
gzip -9 -n -c "$WORK/euroos-preview.vmdk" > "$OUT/euroos-preview.vmdk.gz"

# 4. QEMU bundle (image + firmware + launchers + README).
echo "  -> euroos-preview-x86_64.tar.gz"
BUNDLE="$WORK/euroos-preview"
mkdir -p "$BUNDLE"
cp "$WORK/euroos.img"            "$BUNDLE/euroos.img"
cp "$OVMF"                       "$BUNDLE/OVMF.fd"
cp "$ASSETS/run-euroos.sh"       "$BUNDLE/run-euroos.sh"
cp "$ASSETS/run-euroos.bat"      "$BUNDLE/run-euroos.bat"
cp "$ASSETS/README.txt"          "$BUNDLE/README.txt"
chmod +x "$BUNDLE/run-euroos.sh"
tar --sort=name --mtime='2026-01-01 00:00:00' --owner=0 --group=0 --numeric-owner \
    -C "$WORK" -czf "$OUT/euroos-preview-x86_64.tar.gz" euroos-preview

# 5. SHA256SUMS + VERSION.
echo "  -> SHA256SUMS"
( cd "$OUT" && sha256sum \
    euroos-x86_64-uefi.img.gz \
    euroos-preview.qcow2.gz \
    euroos-preview.vmdk.gz \
    euroos-preview-x86_64.tar.gz > SHA256SUMS )

RAW_SIZE="$(du -h "$WORK/euroos.img" | cut -f1)"
cat > "$OUT/VERSION" <<EOF
EuroOS preview build
version: $VERSION
channel: alpha
arch:    x86-64 UEFI
raw image size (decompressed): $RAW_SIZE
built:   $(date -u +%Y-%m-%dT%H:%M:%SZ)
EOF

echo "==> done in $OUT:"
ls -lh "$OUT"

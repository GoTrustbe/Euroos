#!/usr/bin/env bash
# Boot proof for the userspace MegaRAID driver (docs/SPRINT-PLAN-MEGARAID.md).
#
# Two boots of the same image against the same two logical drives on QEMU's
# megasas-gen2 (LSI SAS2108, the classic MFI frame interface), then a check
# from the host side of what the driver wrote to the disk image files:
#   boot 1: controller info, LD list, raw write/read on LD0, EuroFS formatted
#           on LD1 and written, the driver killed mid-write (with and without
#           replay), the IOMMU refusing DMA outside the driver's window;
#   boot 2: the EuroFS files of boot 1 are read back from LD1;
#   host:   the 1 MiB raw pattern and the self-test mark on ld0.raw, the text
#           file's bytes on ld1.raw.
#
# Usage: scripts/megaraid-proof.sh [image] [outdir]
#   NO_IOMMU=1   boot without -device intel-iommu (the "not confined" path)
#   TIMEOUT=600  seconds per boot before giving up
#
# OVMF places 64-bit BARs in a high aperture (56 TiB with -cpu host), above the
# 512 GiB EuroOS identity-maps; X-PciMmio64Mb=0 keeps them in the 32-bit hole,
# where a real PERC's BARs sit as well (see the sprint plan).
set -euo pipefail
cd "$(dirname "$0")/.."

IMG=$(readlink -f "${1:-eurokernel.img}")
OUT=${2:-/tmp/megaraid-proof}
mkdir -p "$OUT"
OUT=$(readlink -f "$OUT")
LD0="$OUT/ld0.raw"
LD1="$OUT/ld1.raw"
rm -f "$LD0" "$LD1"
truncate -s 64M "$LD0"
truncate -s 256M "$LD1"

OVMF=/usr/share/ovmf/OVMF.fd
[ -f "$OVMF" ] || OVMF=/usr/share/OVMF/OVMF.fd
ACCEL=(-cpu qemu64,+smep,+smap)
[ -e /dev/kvm ] && ACCEL=(-enable-kvm -cpu host)
IOMMU=(-device intel-iommu,intremap=off)
[ "${NO_IOMMU:-0}" = 1 ] && IOMMU=()

boot() {
    local log=$1
    cp "$IMG" "$OUT/boot.img"
    : > "$log"
    qemu-system-x86_64 -machine q35 -m 1024M -smp 4 "${ACCEL[@]}" \
        -bios "$OVMF" -drive format=raw,file="$OUT/boot.img" \
        "${IOMMU[@]}" \
        -fw_cfg name=opt/ovmf/X-PciMmio64Mb,string=0 \
        -device megasas-gen2,id=mr0 \
        -drive if=none,id=ld0,file="$LD0",format=raw -device scsi-hd,bus=mr0.0,drive=ld0,scsi-id=0 \
        -drive if=none,id=ld1,file="$LD1",format=raw -device scsi-hd,bus=mr0.0,drive=ld1,scsi-id=1 \
        -nic user,model=e1000e -display none -serial file:"$log" -no-reboot \
        -monitor unix:"$OUT/monitor.sock",server=on,wait=off &
    local pid=$!
    local end=$((SECONDS + ${TIMEOUT:-600}))
    # The megaraid self-tests end with the "[mr] userspace megaraidd:" status line.
    until grep -aq '^\[mr\] userspace megaraidd:' "$log" || ! kill -0 "$pid" 2>/dev/null || [ $SECONDS -ge $end ]; do
        sleep 2
    done
    if ! grep -aq '^\[mr\] userspace megaraidd:' "$log" && kill -0 "$pid" 2>/dev/null; then
        # Timed out with the VM still up: an NMI makes the kernel print where
        # its cores are (NMI PROBE) before the VM is stopped.
        echo "==> no status line after ${TIMEOUT:-600} s: NMI probe" >&2
        python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); s.sendall(b"nmi\n")' "$OUT/monitor.sock" || true
        sleep 8
    fi
    sleep 2
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    grep -aE '^\[(mr|mr-[a-z]+|mrblk)\]|^\[megaraidd |DMA FAULT' "$log" > "${log%.log}.markers" || true
}

echo "==> boot 1"
boot "$OUT/boot1.log"
echo "==> boot 2 (same disks)"
boot "$OUT/boot2.log"

echo "==> host-side check of the disk images"
python3 - "$LD0" "$LD1" <<'EOF' | tee "$OUT/host-check.txt"
import sys
ld0, ld1 = sys.argv[1], sys.argv[2]
MARK = b"EUROOS-MEGARAID-SELFTEST-LD-V1\0"
with open(ld0, "rb") as f:
    s0 = f.read(512)
    f.seek(4096 * 512)
    region = f.read(1 << 20)
want = bytes(((((i * 2654435761) & 0xFFFFFFFF) >> 13) & 0xFF) ^ ((i >> 16) & 0xFF) for i in range(1 << 20))
print(f"ld0.raw: self-test mark in sector 0: {s0.startswith(MARK)}")
print(f"ld0.raw: 1 MiB at LBA 4096 equals the pattern the driver wrote: {region == want}")
data = open(ld1, "rb").read()
for g in (2,):
    needle = f"written by EuroOS through megaraidd, generation {g},".encode()
    print(f"ld1.raw: /mr/hello.txt text of generation {g} (the last boot) present on the disk: {needle in data}")
print(f"ld1.raw: EuroFS superblock magic present: {b'EUROFS01' in data[:1 << 20]}")
EOF
echo "==> markers: $OUT/boot1.markers $OUT/boot2.markers"
grep -aE '→|⬜' "$OUT/boot1.markers" "$OUT/boot2.markers" | sed 's/^.*markers://' || true

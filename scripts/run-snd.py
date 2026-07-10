#!/usr/bin/env python3
"""3B-7: boot with an emulated virtio-sound device + capture serial → prove the
virtio-snd driver ([3b7]). Headless, no KVM needed."""
import subprocess, sys, time, os
IMG = sys.argv[1] if len(sys.argv) > 1 else "eurokernel.img"
WAIT = int(sys.argv[2]) if len(sys.argv) > 2 else 175
OVMF = "/usr/share/ovmf/OVMF.fd"
q = subprocess.Popen([
    "qemu-system-x86_64", "-machine", "q35", "-cpu", "qemu64,+smep,+smap",
    "-m", "256M", "-bios", OVMF, "-drive", f"format=raw,file={IMG}",
    "-device", "virtio-sound-pci,audiodev=snd0", "-audiodev", "none,id=snd0",
    "-display", "none", "-serial", "file:serial-snd.log", "-no-reboot",
], stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
time.sleep(WAIT)
q.kill()
print("=== [3b7] marker ===")
os.system("grep -E '\\[3b7\\]' serial-snd.log || echo '(no [3b7] marker)'")

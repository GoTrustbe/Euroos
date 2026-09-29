#!/usr/bin/env python3
"""Negative tests for the verifying loader, on a copy of the freshly installed disk:
 1. tamper: flip one byte inside the slot A image -> loader must report the sha256 mismatch,
    refuse the slot, and boot the signed ESP file instead ("ESP file (Ed25519 verified)").
 2. unsigned: additionally zero slot A's header and corrupt the ESP signature files ->
    loader must refuse everything ("FATAL: no verified kernel image"), never boot.
"""
import os, re, subprocess, sys, time
FRESH="/tmp/euroos-ota-target-fresh.img"; T="/tmp/euroos-tamper.img"; OUT="/tmp/euroos-ota"; OVMF="/usr/share/ovmf/OVMF.fd"
def gpt_parts(path):
    f=open(path,"rb"); f.seek(512); h=f.read(512); ent=int.from_bytes(h[72:80],"little"); num=int.from_bytes(h[80:84],"little"); esz=int.from_bytes(h[84:88],"little")
    f.seek(ent*512); arr=f.read(num*esz); parts={}
    for i in range(num):
        e=arr[i*esz:i*esz+128]
        if e[:16]==b"\0"*16: continue
        name=e[56:128].decode("utf-16-le").rstrip("\0"); parts[name]=(int.from_bytes(e[32:40],"little"),int.from_bytes(e[40:48],"little"))
    return parts
def run(tag, wait_for, timeout=400):
    serial=f"{OUT}/{tag}.serial"; open(serial,"w").close()
    q=subprocess.Popen(["qemu-system-x86_64","-machine","q35","-cpu","qemu64,+smep,+smap","-m","2048M","-bios",OVMF,"-display","none","-serial",f"file:{serial}","-no-reboot",
                        "-drive",f"id=tgt,format=raw,file={T},if=none","-device","virtio-blk-pci,drive=tgt,disable-modern=on"],stdout=subprocess.DEVNULL,stderr=subprocess.STDOUT)
    t0=time.time(); hit=None
    while time.time()-t0<timeout:
        time.sleep(5); txt=open(serial,errors="replace").read()
        for pat in wait_for:
            if re.search(pat,txt): hit=pat; break
        if hit or q.poll() is not None: break
    q.terminate()
    try: q.wait(timeout=10)
    except subprocess.TimeoutExpired: q.kill()
    txt=open(serial,errors="replace").read()
    print(f"[{tag}] {'HIT: '+hit if hit else 'TIMEOUT'} after {int(time.time()-t0)}s")
    for l in txt.splitlines():
        if "[loader]" in l or "boot from slot" in l: print("   "+l[:200])
    return txt
subprocess.run(["cp",FRESH,T],check=True)
parts=gpt_parts(T); a_first,_=parts["EuroSlot-A"]; esp_first,_=parts[[n for n in parts if n.startswith("E") and "Slot" not in n and "FS" not in n][0]] if False else (None,None)
# 1. tamper one byte of the slot A image (sector first+1, byte 100)
with open(T,"r+b") as f:
    f.seek(a_first*512); hdr=f.read(512); assert hdr[:8]==b"EUROSLT2", "fresh install has no slot A header?"
    f.seek((a_first+1)*512+100); b=f.read(1); f.seek((a_first+1)*512+100); f.write(bytes([b[0]^0xFF]))
t=run("tamper",[r"\[euroupdate\] slot . confirmed GOOD", r"FATAL"])
ok1=("sha256 MISMATCH" in t) and ("ESP file (Ed25519 verified)" in t) and ("confirmed GOOD" in t)
print("TAMPER-TEST", "PASS" if ok1 else "FAIL")
# 2. no valid candidate at all: zero the slot A header + corrupt both ESP signature files
with open(T,"r+b") as f:
    f.seek(a_first*512); f.write(b"\0"*512)
for name in ("eurokernel-A.efi.sig","eurokernel-B.efi.sig"):
    # rewrite the .sig on the ESP in place via mtools (partition offset from GPT)
    pass
# find the ESP: the partition that is neither a slot nor EuroFS
esp=[v for n,v in parts.items() if "Slot" not in n and "FS" not in n and "EuroOS" not in n][0]
off=esp[0]*512
subprocess.run(["mcopy","-o","-i",f"{T}@@{off}","/dev/stdin","::/EFI/BOOT/eurokernel-A.efi.sig"],input=b"\x11"*64,check=True)
subprocess.run(["mcopy","-o","-i",f"{T}@@{off}","/dev/stdin","::/EFI/BOOT/eurokernel-B.efi.sig"],input=b"\x11"*64,check=True)
t=run("unsigned",[r"\[euroupdate\] slot . confirmed GOOD", r"FATAL"],timeout=300)
ok2=("FATAL: no verified kernel image" in t) and ("confirmed GOOD" not in t)
print("UNSIGNED-TEST", "PASS" if ok2 else "FAIL")
sys.exit(0 if ok1 and ok2 else 1)

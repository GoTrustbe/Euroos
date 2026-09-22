#!/usr/bin/env python3
# Generate the EuroOS ROTATION keypair on the build server, without a second
# machine and without ever writing the private seed to disk in plaintext:
#
#   seed (RAM only) --age -p--> /root/euroos-keys/rotation.key.age     (0400, root)
#   passphrase (random, 8 words) -> /root/euroos-rotation-passphrase.txt (0400, root)
#   public key -> /root/euroos-keys/rotation.pub + toolchain/eupkg/keys/rotation.pub
#
# Move the passphrase into your password manager and delete the file. From then
# on the encrypted key is useless without the passphrase, so a compromised
# server leaks only the daily key (dev.key), which the rotation key can replace:
# scripts/server/rotate-signing-key.sh. Rebuild after this: the kernel and the
# loader embed rotation.pub at build time.
import os, pty, secrets, subprocess, sys, select, time
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives import serialization as s
KEYDIR="/root/euroos-keys"; REPO_PUB="/opt/euroos/eurokernel/toolchain/eupkg/keys/rotation.pub"
os.makedirs(KEYDIR, mode=0o700, exist_ok=True); os.chmod(KEYDIR, 0o700)
if os.path.exists(f"{KEYDIR}/rotation.key.age"): sys.exit("rotation.key.age exists already; refusing to overwrite")
words=open("/usr/share/dict/words").read().split() if os.path.exists("/usr/share/dict/words") else None
if words:
    words=[w for w in words if w.isalpha() and w.islower() and 4<=len(w)<=8]
    passphrase="-".join(secrets.choice(words) for _ in range(8))
else:
    passphrase=secrets.token_urlsafe(32)
sk=Ed25519PrivateKey.generate()
seed=sk.private_bytes(s.Encoding.Raw,s.PrivateFormat.Raw,s.NoEncryption())
pub=sk.public_key().public_bytes(s.Encoding.Raw,s.PublicFormat.Raw)
def age_pty(args, data, pw, answers):
    import fcntl, termios
    m,sl=pty.openpty()
    def child():
        os.setsid(); fcntl.ioctl(sl, termios.TIOCSCTTY, 0)
    inp,outp=os.pipe(); ino,outo=os.pipe()
    p=subprocess.Popen(["age"]+args, stdin=inp, stdout=outo, stderr=subprocess.PIPE, preexec_fn=child, close_fds=True)
    os.close(inp); os.close(outo); os.close(sl)
    os.write(outp,data); os.close(outp)
    out=b""; sent=0; t0=time.time()
    while p.poll() is None and time.time()-t0<30:
        r,_,_=select.select([m,ino],[],[],0.2)
        if m in r:
            try: chunk=os.read(m,4096)
            except OSError: chunk=b""
            if chunk and sent<len(answers) and (b"passphrase" in chunk.lower() or b"confirm" in chunk.lower()):
                os.write(m,answers[sent].encode()+b"\n"); sent+=1
        if ino in r:
            c=os.read(ino,65536)
            if not c: break
            out+=c
    if p.poll() is None:
        p.kill(); p.wait()
        raise SystemExit("age did not finish within 30 s (prompt not recognised?)")
    while True:
        try: c=os.read(ino,65536)
        except OSError: break
        if not c: break
        out+=c
    p.wait(); os.close(ino); os.close(m)
    if p.returncode!=0: raise SystemExit(f"age failed: {p.stderr.read().decode()}")
    return out
enc=age_pty(["-p","-a"], seed, passphrase, [passphrase, passphrase])
dec=age_pty(["-d"], enc, passphrase, [passphrase])
assert dec==seed, "age round trip failed"
assert Ed25519PrivateKey.from_private_bytes(dec).public_key().public_bytes(s.Encoding.Raw,s.PublicFormat.Raw)==pub
del seed, dec
for path,data,mode in [(f"{KEYDIR}/rotation.key.age",enc,0o400),(f"{KEYDIR}/rotation.pub",pub,0o444),(REPO_PUB,pub,0o644),
                       ("/root/euroos-rotation-passphrase.txt",(passphrase+"\n").encode(),0o400)]:
    fd=os.open(path,os.O_WRONLY|os.O_CREAT|os.O_TRUNC,mode); os.write(fd,data); os.close(fd); os.chmod(path,mode)
print("rotation.pub fingerprint:", pub[:8].hex())
print("encrypted key:", f"{KEYDIR}/rotation.key.age"); print("passphrase file (move to your password manager, then delete): /root/euroos-rotation-passphrase.txt")

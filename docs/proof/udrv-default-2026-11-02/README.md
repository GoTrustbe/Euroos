# `net.driver=user` as the default, and the first storage driver out of the kernel: boot proof (2026-11-02)

Branch `feature/udrv-default` (from `813e627`). QEMU 10.2 under TCG (no KVM),
`-machine q35 -cpu qemu64,+smep,+smap`, selftest image (`scripts/build.sh
release`), `-nic user,model=e1000e`. One QEMU at a time. The three option
variants are the same kernel with a different `\EFI\BOOT\euroos-boot.conf`
(none = the defaults, `net.driver=kernel`, `storage.nvme=user`).

Full serial logs (kept next to this file on the build server; `*.log` is not
committed):

| Log | Run | Result |
|---|---|---|
| `user-smp4-iommu.log` | defaults (`net.driver=user`), 512 MiB, `-smp 4`, `-device intel-iommu,intremap=off` | to the desktop; every `[udrv-*]` OK; `[net-late]`, msock/mdns/mtrack, `[g3]`, `[bgnet-cap]` OK; `[udrv-linux]` ⬜ (no glibc arena at 512 MiB) |
| `user-smp1-iommu.log` | defaults, 512 MiB, `-smp 1`, IOMMU | the same |
| `user-smp1-noiommu.log` | defaults, 512 MiB, `-smp 1`, no IOMMU | the same; `[udrv-iommu]`/`[udrv-dma]` "not confined" ⬜ as before |
| `user-smp4-iommu-1g.log` | defaults, 1 GiB, `-smp 4`, IOMMU | the same plus `[udrv-linux]` OK (a glibc process with blocking sockets over e1000d) |
| `kernel-smp4.log`, `kernel-smp1.log` | `net.driver=kernel`, 512 MiB, no IOMMU | to the desktop; per-marker verdicts identical to `../integration-j1-udrv-2026-11-02/kernel-smp{4,1}.log` |
| `storage-smp4-iommu-1g.log`, `storage-smp1-iommu-1g.log` | `storage.nvme=user`, 1 GiB, IOMMU, a blank 64 MiB NVMe data disk | every `[udrv-blk-*]` OK, to the desktop |
| `baseline-813e627-user-smp4-iommu-1g.log` | `813e627` with `net.driver=user`, 1 GiB | for the 1 GiB comparison (the glibc suite's results vary between 1 GiB runs on both trees) |
| `incident-*.log` | see "Open" | two runs that stopped at the chrome_crashpad step |

Compared with `../integration-j1-udrv-2026-11-02/`: in `user` mode the
per-marker verdicts are those of the earlier user-mode runs plus `[net]`
(the network probes, now run), `[bgnet-cap]`, `[udrv-linux]` and the
programs' mirrored output; mtrack's DENIED line moved from `[linux-abi]` to
`[bg-out]` (it runs as a scheduled process now). The only `FAILED` lines in
the 512 MiB runs are the glibc tests without an arena (`[app]`,
`[churn-proc]`, `[churn-thread]`, `[churn-fork]`, `[app-policy]`), as in the
earlier logs. 0 order inversions and 0 task locks under IF=0 in every run.
Host tests of the whole workspace: green.

Excerpts, defaults, `-smp 4`, IOMMU:

```
[bootopt] no euroos-boot.conf (defaults): net.driver=user, storage.nvme=kernel
[net] NIC: e1000 left to the userspace driver e1000d (net.driver=user); network bring-up and the network tests run after the scheduler starts ([udrv-*], [net-late])
[net-late] network ready over e1000d (waited 0 ticks): lease 10.0.2.15, gateway 10.0.2.2, dns 10.0.2.3; running the network part of the boot now
[net] PING 10.0.2.2: echo-reply OK ✓
[net] DNS reply: example.com = 104.20.23.154 ✓
[net] HTTP GET http://example.com/ -> 888 bytes ✓
[net] HTTPS GET https://example.com/ -> 888 bytes (encrypted) ✓
[bg-out] pid 196608 /bin/msock:   status:   HTTP/1.1 200 OK
[exec] /bin/msock (abi=linux, ed25519=ok, scheduled over e1000d, 13 ticks) exit=0 (220 bytes)
[bg-out] pid 196609 /bin/mdns:     -> 104.20.23.154
[bg-out] pid 196609 /bin/mdns:     no A record in the answer
[bg-out] pid 196610 /bin/mtrack:   connect() DENIED — EuroGuard blocked this ✓
[lk] net-late ring3.BG (background processes): ... hold-max=18825us
[bgnet-cap] /bin/msock as a background process WITHOUT CAP_NET: socket() refused 1 time(s), done (exit 1), ... → OK (no capability, no socket) ✓
[net-late] network waits in this suite gave the CPU away 11 time(s); 7 of them were woken early by e1000d (EuroIPC frame delivery)
[bg-enosys] boot total: 0 background-path syscall(s) answered -ENOSYS ...
[lk] boot-total order inversions=0 (self-test provoked=2) task-lock-with-IF0=0
```

1 GiB (`user-smp4-iommu-1g.log`):

```
[udrv-linux] glibc process /bin/gnet (Linux ABI, scheduled, blocking sockets) over e1000d in 59 ticks: "GNET: glibc blocking sockets: connect+send+recv to example.com (104.20.23.154): 888 bytes, status \"HTTP/1.1 200 OK\"" exit=78; its waits gave the CPU away 5 time(s), 3 woken early by e1000d → OK ✓
```

BG lock, boot-total hold-max: 18 to 22 ms in every run, before
(`../integration-j1-udrv-2026-11-02/`: 22.2, 19.8, 20.2, 18.3 ms) and after
(18.8, 21.6, 18.5 ms user; 18.5, 22.5 ms kernel). The background network
waits run outside it.

Storage, `storage.nvme=user`, `-smp 4`, IOMMU, 1 GiB:

```
[udrv-blk-grant] nvmed pid 135168: BAR0 0xc000000000..0xc000002000 (8 of the BAR's 16 KiB: registers + doorbells) mapped at 0x7f00000000=true, next page (MSI-X table side)=false, ... → OK (exactly the granted ranges) ✓
[udrv-blk-iommu] NVMe 00:03.0 has its own VT-d domain 'nvmed' with 8 page(s) = the DMA window, nothing else → OK ✓
[udrv-blk] nvmed (pid 135168, ring 3, profile driver) brought the NVMe 1b36:0010 up: 131072 sectors x 512 B = 64 MiB, reported over EuroIPC → OK ✓
[udrv-blk-rw] raw sectors via EuroIPC through nvmed: 8 KiB at sector 131056 written + read back identical=true, original restored=true ... → OK ✓
[udrv-blk-irq] MSI-X vector 0x4d: 36 completion interrupt(s) raised, 36 delivered to nvmed as EuroIPC Irq messages → OK ✓
[udrv-blk-dma] malicious nvmed: READ of LBA 0 aimed at kernel canary 0x1d1000 · ... IOMMU fault records 1 · canary changed=false · disk still works=true → OK ✓
[udrv-blk-fs] EuroFS through nvmed: 20000-byte file written=true, read back identical=true; scrub: 2 object(s), 0 error(s), superblock ok=true → OK ✓
[nvmed pid 135168] crash requested after this write (lba 254): faulting now, before reporting it
[udrv-blk] nvmed pid 135168 died (memory isolation: access 0x1000 denied) -> controller quiesced: bus-master off, IOMMU domain revoked, 8 DMA pages freed; restart at tick ...
[udrv-blk-crash] nvmed pid 135168 killed by a real fault mid-write ...; the 256 KiB write failed=true (1 request error(s)); restarted as pid 135169; EuroFS remounted from the disk: scrub 2 object(s), 0 error(s), superblock ok=true, bitmap ok=true; committed file intact=true; file being written: absent (the commit never happened) → OK ✓
[udrv-blk-mount] EuroFS on the userspace-driven NVMe mounted at /data; VFS write+read through it → OK ✓
```

## Open

- **Two runs stopped at the chrome_crashpad step** of the glibc suite, out of
  all the boots of this branch: one `-smp 1` 512 MiB run without IOMMU stopped
  logging after `gfmmap` (killed after 14 minutes; `incident-hang-...`), one
  `-smp 4` 1 GiB run ended with a machine reset right after crashpad's
  "failed to map segment" message (`incident-reset-...`). Neither reproduced:
  the same configurations passed again, and three `813e627` and three branch
  runs side by side at 1 GiB all reached the desktop. That step does no
  network I/O; it is not attributed, and it is not explained either.
- An earlier storage run showed the first nvmed instance timing out in
  controller init, because bus mastering was switched on after the driver
  task could already run; fixed (`58f3bfb`), the runs above are after the fix.

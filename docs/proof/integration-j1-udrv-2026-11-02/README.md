# Merge of J1 fine-grained locking and the userspace e1000 driver: boot proof (2026-11-02)

Branch `feature/integrate-j1-udrv` (merge of `feature/userspace-drivers` and
`feature/fine-locking`, plus the kernel EuroIPC bus on `crates/euroipc`).
QEMU 10.2 under TCG (no KVM), `-machine q35 -m 512M -cpu qemu64,+smep,+smap`,
selftest image (`scripts/build.sh release`). One QEMU at a time.

Full serial logs (kept next to this file on the build server; `*.log` is not
committed):

| Log | Run | Result |
|---|---|---|
| `kernel-smp4.log` | default (`net.driver=kernel`), `-smp 4` | to the interactive desktop; per-marker results equal to the J1 branch run |
| `kernel-smp1.log` | default, `-smp 1` | to the interactive desktop; per-marker results equal to the J1 branch run |
| `user-iommu-smp4.log` | `net.driver=user`, `-device intel-iommu,intremap=off -nic user,model=e1000e`, `-smp 4` | every `[udrv-*]` marker OK, to the desktop |
| `user-noiommu-smp1.log` | `net.driver=user`, `-nic user,model=e1000e`, `-smp 1` | every `[udrv-*]` marker OK, IOMMU steps "not confined" (as before the merge) |
| `diag-msock-user-smp4.log` | experiment, `udrv-msock-diagnostic.patch` (not merged) | msock as a musl background process over e1000d: no socket syscalls on that path |

The only `FAILED` lines in the default runs are the glibc tests that need more
than 512 MiB (`no arena for glibc`: `[app]`, `[churn-proc]`, `[churn-thread]`,
`[churn-fork]`, `[app-policy]`), the same as in the J1 branch logs.

Excerpts, `-smp 4`, default:

```
[bootopt] no euroos-boot.conf (defaults): net.driver=kernel
[exec] /bin/msock (abi=linux, ed25519=ok) exit=0 (221 bytes)
[kwork] per-CPU work queues: 3 AP(s) online, 4 worker(s) ran alloc+format+SSE jobs with a correct per-CPU id (4 ok), work-stealing=yes -> OK ✓
[lk] checker self-test: irqsave=true correct-order-silent=true deliberate-inversion-detected=true task-mode-keeps-IF=true try_lock=true rwlock=true tracker-balanced=true -> OK ✓
[lk-stress] 3 round(s) x 4 core(s), mixed fs/ipc/unix + root VFS from every core: 125698 ops, correctness errors=0, order inversions=0, task-lock-with-IF0=0, all rounds completed in 0.9 s (no deadlock) -> OK ✓
[desktop] interactive loop started — input + shell live
[lk] boot-total order inversions=0 (self-test provoked=2) task-lock-with-IF0=0
```

Excerpts, `net.driver=user` with the IOMMU, `-smp 4`:

```
[bootopt] \EFI\BOOT\euroos-boot.conf read: net.driver=user
[udrv-grant] e1000d pid 131072: BAR0 0x810a0000..0x810c0000 mapped for ring 3 (first=true, last=true), page before=false, page after=false; ... LAPIC=false, kernel code=false → OK (exactly the granted ranges) ✓
[udrv-iommu] device 00:02.0 has its own VT-d domain 'e1000d' with 32 page(s) = the DMA window, nothing else → OK ✓
[udrv-e1000] e1000d (pid 131072, ring 3, profile driver) brought the 82574L (e1000e) up: MAC 52:54:00:12:34:56 reported over EuroIPC; ... → OK ✓
[udrv-net]   HTTP GET http://example.com/ -> 888 bytes, HTTP/1.1 200 OK
[udrv-irq] MSI-X vector 0x4c: 16 interrupt(s) raised, 16 delivered to e1000d as EuroIPC Irq messages → OK ✓
[iommu] DMA FAULT blocked: device 00:02.0 tried to read physical 0x1fa000 (reason 0x6) — outside its domain
[iommu] DMA FAULT blocked: device 00:02.0 tried to write physical 0x1f9000 (reason 0x5) — outside its domain
[udrv-dma] malicious e1000d: ... IOMMU fault records: TX read 1, RX write 1 (total 2) · canary changed=false · driver still works=true → OK (device DMA confined to the driver's own window) ✓
[udrv] e1000d pid 131072 died (memory isolation: access 0x1000 denied) -> device quiesced: bus-master off, IOMMU domain revoked, 32 DMA pages freed; restart at tick 524 (death #1)
[udrv-crash] e1000d pid 131072 killed by a real fault (...); kernel kept running; sends refused while dead=true; restarted as pid 131073, MAC 52:54:00:12:34:56, DHCP + HTTP again → OK (driver crash = process exit, network back) ✓
[exec] /bin/msock (abi=linux, ed25519=ok) exit=1 (126 bytes)
[desktop] interactive loop started — input + shell live
```

(msock runs before the scheduler, so in `user` mode it has no network; see
"Making `user` the default" in `docs/SPRINT-PLAN-USERSPACE-DRIVERS.md`.)

Experiment (not merged):

```
[udrv-msock] (diagnostic) /bin/msock as a scheduled process over e1000d: finished=true after 8 ticks; during the run: netstack tx 0 rx 0 frames, driver IRQ messages 0
[udrv-msock]   |   GET sent (56 bytes)
[udrv-msock]   |   response: 0 bytes
```

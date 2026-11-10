# Fast boot of the release image: proof (2026-11-10)

Branch `feature/fast-boot`, started on `feature/app-control` `a36ef4b`. Not
merged. What changed and why: `../../BOOT-TIME.md`, `../../LOADTEST-TRACKER.md`
(BUG-041 to BUG-043) and the commit messages.

| Commit | What |
|---|---|
| `a03adcf` | `[boot-time]`: one line per boot phase, one summary line at the desktop, every image |
| `cd81ece` | `[net-up]`: DHCP, ARP and router discovery of an in-kernel NIC in a kernel task |
| `b9c33cd` | release image: no SMP demo, no lockscreen flash on a live medium, crash-dump report without a test dump |
| `5112706` | release image: self-tests, churn and the boot network probes behind the `selftest` feature |
| `2c8c01d` | `scripts/boot-time.py`, `screenshot.py` paths by environment, wizard test without `[ag-lock]` |
| `3ab5d2c` | never format an NVMe disk that carries something (every image) |
| (last) | docs, this proof |

Images: "before" = `a36ef4b`, "before, instrumented" = `a03adcf` (the same
boot plus `[boot-time]`), "after" = `3ab5d2c`. Built on the build server with
`scripts/build.sh image` (release image) and `scripts/build.sh release`
(self-test image). Serial logs stay on the servers; the files here are the
verdict lines, the excerpts that carry the argument and one whole release boot.

## A. Self-test image: unchanged

TCG matrix on the build server (`tcg-matrix.sh`, `tcg-boot.sh`: the scripts of
`../virtio-modern-2026-11/`, ten boots: user-driver smp4/smp1 with and without
IOMMU, kernel driver, storage, combined), three times: the proof of
`../virtio-modern-2026-11/` as the reference, a control run of `a36ef4b`
built and run the same day, and `3ab5d2c`.

| Image | To the desktop | Markers |
|---|---|---|
| control `a36ef4b` | 10 of 10 | against the virtio-modern proof: 0 worse, 0 changed, 0 only in base in all ten (the two boots that hung in that proof show as "only in new") |
| `3ab5d2c` | 9 of 10, and 3 of 3 repeats of the tenth | against the control: **0 worse, 0 changed, 0 only in base, 0 only in new** in each of the nine and of the three repeats |

`[lk] boot-total order inversions=0 (self-test provoked=2)
task-lock-with-IF0=0` in every boot that reached the desktop
(`tcg-selftest-lk.txt`). Run times: `tcg-selftest-matrix-runs.txt`.

**The tenth boot** (`storage-smp4-iommu-1g`) stopped in the glibc suite and
was silent for 900 s. It is BUG-037, open since 2026-11-05 and not in the
code this branch changes: the NMI probe finds task 10 in
`ring3::pipe_write_fd +0xd1` with RFLAGS `0x50002` (IF=0), the boot task
Ready, the same offset and the same code bytes as
`../virtio-modern-2026-11/tcg-hang-*`
(`tcg-hang-storage-smp4-iommu-1g-nmi.txt`, symbolized). Its markers up to the
stop are in `tcg-storage-smp4-iommu-1g.markers` (in the comparison: 150 "only
in base", the ones after the stop, and two FAILED counts that are lower for
the same reason). Three more boots of that configuration, one after the
other: all to the desktop, identical to the control
(`tcg-storage-smp4-iommu-1g-r1.markers` to `-r3`). The build server was shared
with the matrices of other branches that morning.

Before the final image, a smoke run of an earlier state of the branch (eight
of the ten configurations) was identical to the virtio-modern proof as well;
it is not kept here.

`tcg-*.markers` are the verdict lines of the `3ab5d2c` boots (plus their
`[boot-time]` and `[lk]` lines), `control-tcg-*.markers` those of `a36ef4b`.
`scripts/markers-compare.py compare` gives `comparison-tcg-vs-control.txt`,
`comparison-tcg-vs-virtio-modern-proof.txt` and
`comparison-control-vs-virtio-modern-proof.txt`.

The new lines of the self-test image are the `[boot-time]` ones
(`tcg-selftest-boot-time.txt`): no verdict, not counted by the comparison.

## B. Release image: boot time

`scripts/boot-time.py` (here as it ran: `../../../scripts/boot-time.py`):
host time from QEMU start to `[desktop] interactive loop started`.

R630, KVM (`-machine q35 -enable-kvm -cpu host -smp 2 -m 2G`, OVMF, user-net
`restrict=on`, pinned to two cores), five boots each, `kvm-5x.txt`:

| Configuration | Before, median (range) | After, median (range) |
|---|---|---|
| SATA boot medium + legacy virtio-net (as `scripts/screenshot.py`) | 19.51 s (19.06 to 19.71) | 2.95 s (2.88 to 3.06) |
| modern virtio only: virtio-blk, virtio-net, tablet, keyboard, `disable-legacy=on` | 19.16 s (18.97 to 20.11) | 3.31 s (2.75 to 4.23); repeat 2.70 s (2.53 to 2.77) |
| e1000e, userspace driver e1000d (one boot) | 22.55 s | 3.36 s |

The first modern-virtio series ran while other guests were busy on the R630
(load 11); `kvm-5x-modern-rerun.txt` is the repeat a few minutes later.

Build server, TCG (`-cpu qemu64,+smep,+smap -smp 2 -m 2G`, open user-net, the
machine shared with other builds and matrices, load about 12), alternating
before/after, `tcg-release-boots.txt`:

| Configuration | Before | After |
|---|---|---|
| SATA + legacy virtio-net, 3 boots, median (range) | 36.75 s (36.46 to 39.32) | 6.46 s (6.40 to 6.69) |
| 1 vCPU, 1 GiB, one boot | 32.12 s | 7.11 s |
| modern virtio only, one boot | | 6.35 s |

Kernel entry to desktop (`[boot-time]`, `kvm-boot-time.txt`,
`tcg-boot-time.txt`): KVM 16003 ms → 977 ms (medians of 3 and 5), TCG
29748 ms → 2090 ms (medians of 3). The per-phase table is in
`../../BOOT-TIME.md`. The rest of the host time is firmware (1.2 s KVM) and
the loader (0.6 s KVM, 1.7 s TCG).

Serial log up to the desktop: 1590 lines, 164 kB → 278 lines, 18 kB
(`kvm-summary.txt`); `kvm-after-default-serial.txt` is that whole boot.

## C. Release image: nothing lost

- **No FAILED, no PANIC** in any "after" log, KVM and TCG (`kvm-summary.txt`).
  Before: `[churn-thread] … FAILED`, `[conn] TCP handshake FAILED`.
- **Desktop**: `kvm-before-desktop.png`, `kvm-after-desktop.png` (the terminal
  shows the same demo; the test notification is gone, "Network connected" is
  new), `kvm-after-modern-virtio-desktop.png`,
  `tcg-after-modern-virtio-desktop.png`, `tcg-after-smp1-1g-desktop.png`.
- **Shell, file system, identity, security state**: the same 50 commands
  (`shell-battery-commands.txt`) over the serial console on both images.
  `kvm-shell-battery-before-vs-after.diff` is the whole difference: what the
  self-tests left behind is gone (test files and directories, test apps,
  `/home/alice`, the example key, the test DNS block, the test programs in
  the per-app network report, 31 audit events, the scratch quota), the demo accounts are
  as built (`alice` active, `bob` active, no `carla`), one closed "auto"
  session instead of three. Vault, policy, VPN, CA, attestation, IDM, agent,
  firewall, services, devices, `wasm /agents/demo.wasm`, writing and
  versioning a file in the home directory: no difference.
- **Network**: `[net-up] lease …` 10 ms (KVM) to 30 ms (TCG) after the task
  starts, before the desktop is up. `tcg-after-network.txt` (open network):
  ping, DNS, `fetch` and `https` of example.com, the network worker woken by
  the NIC's interrupt. `kvm-after-modern-virtio-excerpt.txt`: the same on
  modern virtio (ping the gateway; the user-net there is restricted).
- **No DHCP server** (a virtio-net NIC on an empty hub),
  `kvm-no-dhcp-server.txt`: after, the desktop is there 2.54 s after QEMU
  starts and the task keeps asking (`[net-up] no DHCP offer in the first
  second …; asking again in the background`); before, 90.85 s to the desktop.
- **e1000e / update check**: `kvm-after-e1000e-and-update-check.txt`. The
  first update check comes 90 s after boot as before (a preview image has no
  slot partitions and says so).
- **Modern virtio**: boot from virtio-blk 1.x, virtio-net 1.x, virtio-tablet
  and virtio-keyboard, KVM and TCG (excerpts and screenshots above).
- **Installer**: `scripts/wizard-install-test.py` on the "after" image, 14 of
  14 checks (`kvm-host-wizard-install-test.txt`): live boot, guided install
  on a blank disk, standalone boot of the installation, the lockscreen waits
  (`wizard-installed-lockscreen.png`), a wrong password is refused, the typed
  one signs in (`wizard-installed-desktop.png`). The script used to wait for
  the lockscreen self-test line; it now waits for the `[boot-time]` mark
  before the lockscreen.

## D. NVMe disk with foreign content (BUG-042)

`kvm-nvme-foreign-disk.txt`: a 64 MiB disk of random bytes with a boot
signature, attached as NVMe. After one boot of the "before" image its sha256
is another one (`[g2] EuroFS formatted on NVMe`); after one boot of the
"after" image it is the same (`[g2] NVMe disk carries unknown content: left
untouched`). A blank NVMe disk is formatted and mounted at `/nvme` as before.

## Not shown here

- Real hardware. All numbers are QEMU.
- An installed system under KVM: the wizard test boots one under TCG
  (lockscreen reached 2.4 s after kernel entry there).
- A DHCP server that answers late: the retry of `[net-up]` ran against a
  network with none (above), not against one that comes up later.

# Boot time

How long the boot takes, where the time goes, and what the release image
(`./scripts/build.sh image`) does and does not run on the way to the desktop.
Measurements and logs: `docs/proof/fast-boot-2026-11/`.

## Measuring: `[boot-time]`

Every image prints one line per boot phase and one summary line when the
desktop loop starts (`kernel/src/boottime.rs`):

```
[boot-time] phase storage-rootfs: 52.335 ms (at 267 ms)
...
[boot-time] desktop after 1066 ms since kernel entry (HPET, release build): firmware-exit=203 cpu-mm=11 ... first-frame=126 (ms per phase)
```

- The zero is the first instruction of the kernel's `main`. Firmware and the
  loader (reading and verifying the kernel image) come before it and are not
  in these numbers; `scripts/boot-time.py` stamps every serial line with the
  host time since QEMU started, which shows them.
- The clock is the HPET main counter. It is reachable only on the kernel's own
  page tables, so the stretch before that is stamped with the TSC and converted
  once the TSC rate has been measured against the HPET; those first lines are
  printed together a moment later. Without an HPET the tick clock (10 ms) is
  used and the summary says so.
- A phase is named at its END: `phase storage-rootfs` is everything since the
  previous mark. The cost is one `rdtsc`, one HPET read and one line per phase.

| Phase | What it covers |
|---|---|
| `firmware-exit` | heap, GOP mode, install-media capture, boot options, ExitBootServices |
| `cpu-mm` | GDT, IDT, page tables, guarded stack, process frame pool |
| `storage-rootfs` | virtio-blk, EuroPack scan, NVMe, AHCI, root file system (mount, or format and populate) |
| `storage-checks` | A/B slot decision; self-test image: update, FAT, bad-block, swap, crash-dump, container proofs |
| `identity-config` | HPET report, EuroID store, key map, `/etc` registration, hosts, program registry, EuroGuard |
| `net` | NIC init; self-test image: DHCP, ARP, ping, DNS, HTTP, HTTPS, IPv6 inline |
| `userland-demos` | `/bin/hello`, library registration, the boot script; self-test image: linker, TLS, WASM, Wayland proofs |
| `sched-init` | guarded task stacks, FPU, scheduler, EuroInit, background test processes (self-test image) |
| `acpi-pci-tpm` | MADT, IOMMU detection, ECAM, PCI, device tree, AML, TPM, entropy |
| `smp-timer` | LAPIC timer calibration, EuroLock hooks, AP start, IO-APIC, mouse |
| `devices` | xHCI, virtio-input, HD-Audio, IOMMU programming, interrupts on |
| `drivers-late` | userspace drivers (e1000d, bcm5720d, megaraidd), late NIC bring-up, network worker |
| `glibc-suite` | self-test image only: glibc, X11, Cairo, Pango, GTK, SDL, chrome |
| `vfs-mounts` | VFS, `/var`, swap, `/mnt`, `/nvme`, USB auto-mount |
| `subsystems` | immutability, integrity sweep, audit log, policy, vault, firewall, VPN, agent, CA, attestation, IDM; self-test image: every other subsystem self-test and the churn runs |
| `boot-good-scrub` | slot marked good, journal restore; self-test image: scrub pass |
| `desktop-build` | terminal contents, windows, apps, virtio-gpu scanout |
| `lockscreen` | sign-in |
| `first-frame` | first composite and present |

## Before and after (2026-11)

Release image (`./scripts/build.sh image`), before = `a36ef4b`, after =
`3ab5d2c` (`feature/fast-boot`). Host time from QEMU start to
`[desktop] interactive loop started`, median of the boots:

| Machine, configuration | Before | After |
|---|---|---|
| R630, KVM, 2 vCPU, 2 GiB, SATA boot medium + legacy virtio-net (5 boots) | 19.51 s | 2.95 s |
| R630, KVM, 2 vCPU, 2 GiB, modern virtio only: blk, net, tablet, keyboard (5 boots) | 19.16 s | 2.70 s |
| R630, KVM, e1000e with the userspace driver (1 boot) | 22.55 s | 3.36 s |
| Build server, TCG, 2 vCPU, 2 GiB, SATA + legacy virtio-net, open user-net (3 boots) | 36.75 s | 6.46 s |
| Build server, TCG, 1 vCPU, 1 GiB (1 boot) | 32.12 s | 7.11 s |

After the integration of the five 2026-11 branches (`182b19c`, settings,
help and the first-run tour included; `docs/proof/integ-2026-11/`), the same
two R630 configurations, five boots each: **2.71 s** (2.42 to 2.92) and
**2.34 s** (2.05 to 2.43); kernel entry to desktop 812 ms. The tour costs one
read of the user's `desktop.conf`, after the desktop mark.

Of the 2.95 s under KVM, 1.2 s is firmware and 0.6 s the loader; the kernel
takes 1.0 s. Under TCG firmware and loader take 4.4 s, the kernel 2.1 s.
(The first modern-virtio series on the R630 ran next to other guests: median
3.31 s, 2.75 to 4.23; the table has the repeat.)

Kernel entry to desktop per phase, in ms (`[boot-time]`; "before" is `a36ef4b`
with only the measurement added, `a03adcf`; the runs with the median total):

| Phase | KVM before | KVM after | TCG before | TCG after |
|---|---:|---:|---:|---:|
| firmware-exit | 185 | 71 | 136 | 139 |
| cpu-mm | 11 | 11 | 7 | 8 |
| storage-rootfs | 50 | 54 | 80 | 84 |
| storage-checks | 65 | 9 | 85 | 3 |
| identity-config | 50 | 46 | 221 | 228 |
| net | 2094 | 24 | 8996 | 6 |
| userland-demos | 5890 | 227 | 294 | 132 |
| sched-init | 13 | 16 | 6 | 4 |
| acpi-pci-tpm | 144 | 66 | 363 | 20 |
| smp-timer | 311 | 62 | 290 | 38 |
| devices | 31 | 29 | 14 | 15 |
| drivers-late | 33 | 33 | 7 | 3 |
| glibc-suite | 1 | 2 | 0 | 0 |
| vfs-mounts | 85 | 4 | 19 | 1 |
| subsystems | 5830 | 108 | 8235 | 166 |
| boot-good-scrub | 15 | 10 | 17 | 4 |
| desktop-build | 40 | 48 | 56 | 78 |
| lockscreen | 985 | 6 | 9739 | 15 |
| first-frame | 164 | 139 | 1175 | 1137 |
| **total** | **16003** | **977** | **29748** | **2090** |

Where the 16 s went under KVM: 5.9 s in `userland-demos` (3.0 s waiting for a
DNS answer in `mdns`, 2.8 s in two `nslookup` calls), 2.1 s in `net` (ARP
lookups that could not see their answers before the configuration was saved),
5.8 s in `subsystems` (the native-app and fuzzer self-tests, the churn runs,
and about 10 ms of serial output per self-test line), 1.0 s at the lockscreen
(two self-test logins, the grace period, the login). Under TCG the same boot
spent 9.0 s in `net` (the two ARP time-outs), 9.7 s at the lockscreen (two
full-screen renders and three Argon2id logins) and 8.2 s in `subsystems`.

The serial log up to the desktop went from 1590 lines (164 kB) to 278 lines
(18 kB). Under KVM every byte is a VM exit, about 30 µs.

The R630 runs use QEMU's restricted user-net, where nothing answers DNS: the
5.8 s of DNS waiting in the "before" column are the time-outs of that set-up.
On an open network the old boot did a DNS query, an HTTP and an HTTPS request
to example.com there instead (the TCG column).

## What the release image no longer runs

The `selftest` feature (on by default) was meant to separate the test image
from the release image, but only the glibc/X11/chrome suite was behind it. The
release image still ran about 150 self-tests, the churn runs and the network
demonstration on every boot, and printed 164 kB to the serial port doing it
(under KVM every byte of that is a VM exit; on a 115200 baud UART it would be
14 s by itself). `SELFTEST` in `main.rs` is now the one switch:

- **Self-tests and proofs**: update pipeline, FAT, bad-block remap, swap cycle
  and fault path, block cache, containers, display protocol, AF_UNIX,
  EuroGuard, user pointers, dynamic linker, TLS, WASM/WASI, Wayland handshake,
  device tree, NTS, verity, NVMe/xHCI/virtio-blk MSI-X, lock checker, kwork,
  symlinks, coreutils, immutability, integrity tamper test, versioning, audit
  chain, snapshots, FDE, user attributes, GDPR, metrics, TPM seal, FDE unseal,
  health, firewall, hybrid VPN, LLM transport, locale, installer planner,
  attestation report, EuroID, wallet, package manager, native and WASM apps,
  API fuzzer (20 000 calls), app store, GDB stub, reproducible builds,
  accessibility, suite, key maps, portal, shell, settings, clipboard, menus,
  window snapping, launcher, file dialog, notifications, screenshots,
  tooltips, symbol picker, spelling, switcher, workspaces, net bridge, audio,
  web engine, calculator, notes, archive, safe, window manager, the small
  apps, files, media, Wi-Fi, GPU, virtio-gpu/snd, print, scan, find, pipes,
  update channel, agent tools, app policy, local LLM, sessions, quota, MIME,
  MCP daemon, WASM agent, install executor, lockscreen, scheduler write watch.
- **Churn**: process, thread, fork, file system, socket, surface, package and
  I/O-storm runs (the `[churn-thread] ... FAILED` line of the release image
  came from here).
- **Network at boot**: the inline DHCP/ARP/ping/DNS/HTTP/HTTPS/IPv6 sequence,
  the socket programs of the boot script (`msock`, `mdns`, `mtrack`), the two
  `nslookup` calls and `netstat`, the SMB/NFS/IPP/eSCL/update-server/Ollama
  probes. A release boot no longer contacts example.com.
- **Raw-sector writes**: the bad-block, swap and crash-dump proofs wrote into
  the GPT gap of virtio disk 0 (LBA 48 to 207) whatever that disk held.
- **Demonstrations**: the SMP scheduler/IPI/load-balance demo (0.25 s of
  waiting), the table of 106 suspect addresses, the boot scrub (the periodic
  scrub makes its first pass a minute in), the lockscreen flash before the
  automatic sign-in of a live medium.

Side effects of those self-tests are gone with them: the test files and
directories in `/` (`find-test`, `mimetest`, `pipe-*.txt`, `containers`,
`pkg`, `.versions`), `/home/alice`, the packages and the example developer key
in `/tmp`, the six test apps in `/bin`, the test notification, the
`selftest-block.example` entry in the DNS block list, the deleted/locked demo
accounts, the fabricated metrics.

### What stays in both images

Self-tests that also set up state the running system uses stay where they
were, until their set-up is separated from their proof:

| Call | State it leaves |
|---|---|
| `tpm::selftest` | extends PCR 16, the PCR keys are sealed to |
| `europol::selftest`, `bundle_selftest` | the active policy |
| `vault::selftest` | the vault and its master key |
| `vpn::selftest` | the local public key |
| `agent::selftest` | the effective agent capabilities the dispatch panel uses |
| `ca::selftest`, `attest::selftest`, `idm::selftest` | root fingerprint, attestation key, IDM key |
| `audit::selftest` | the boot records and the append-only log file |
| `wasm::selftest_file` | `/agents/demo.wasm`, the example the `wasm` command names |
| `journal::persist_selftest` | restores the journal of the previous boot |
| `audio::usb_wire_selftest` | routes the mixer to a USB DAC |
| `acpi_power::report`, `integrity::check_and_report` | not self-tests |

Also in both: `/bin/hello` with its signature and tamper check, the eight
boot-script programs that need no network, the shell demo in the terminal.
`entropy::selftest` is replaced by `entropy::init` (the seeding without the
proof), `crashdump::selftest` by `crashdump::recover` (the report of a dump
from the previous boot, without writing a test dump).

The demo commands in the terminal run as the desktop user, as before: the
session self-test used to leave that session open, the release image now
opens it itself.

## Network in the background: `[net-up]`

`kernel/src/net_bringup.rs`. The release image initializes an in-kernel NIC
(virtio-net, e1000 with `net.driver=kernel`) where it always did and starts a
kernel task once the scheduler runs. The task does DHCP, ARP for the gateway
and the DNS server and one IPv6 router solicitation, saves the configuration,
starts the network worker and parks. It sleeps between looks at the NIC.
While it works the netstack is unconfigured, so nothing else reads the NIC.
The desktop shows "Network connected" when it is done; before that, network
commands answer as on a machine without a lease.

- No DHCP answer: the task asks again after 1, 2, 4, 8 and then every 10 s,
  for as long as the machine runs. The desktop does not wait: 2.5 s from QEMU
  start under KVM with a NIC on an empty network, where the inline sequence
  took 91 s.
- Gateway and DNS server come from the lease (the inline path of the
  self-test image assumes QEMU's 10.0.2.2 and 10.0.2.3).
- The update check is unchanged: first check 90 s after boot, and it waits
  for the network if there is none yet.
- The userspace driver (`net.driver=user`, an e1000) is configured by
  `udrv::boot` as before, a USB NIC by `net::late_bring_up`.

## NVMe disks are not formatted at boot

Found on the way, in every image: an NVMe disk that was not the root and did
not mount as EuroFS was formatted and mounted at `/nvme`. A release medium
booted next to someone else's system on an NVMe disk logged "it stays intact"
and wrote an EuroFS over it. Now only a disk whose first 4 KiB are zero is
formatted (the rule the second virtio disk already had); anything else is left
alone: `[g2] NVMe disk carries unknown content: left untouched (no /nvme)`.
`docs/proof/fast-boot-2026-11/kvm-nvme-foreign-disk.txt` has the disk hashes
before and after a boot of both images.

## Lockscreen

A live medium (demo identity store) signed the demo user in 0.6 s after
drawing the lockscreen, typing or not. The release image signs in at once and
does not draw it. An installed system waits for its password as before. The
self-test image keeps the grace period.

## Tools

- `scripts/boot-time.py NAME IMG OUTDIR [--mode modern] [--accel tcg] ...`:
  one timed boot, host time per serial line, optional screenshot, optional
  shell commands over the serial console. Own socket and files per run.
- `scripts/screenshot.py`: `EK_QMP`, `EK_SERIAL`, `EK_PCAP` set the socket and
  log paths (defaults unchanged).

## Open

- The kept self-tests (table above) should become `init` functions with the
  proof split off; then the release log has no self-test lines left.
- The loader reads and verifies the 51 MiB kernel image before the kernel
  starts (0.6 s under KVM, 1.7 s under TCG); firmware takes 1.2 s under KVM.
  Neither is in `[boot-time]`.
- The first frame is 1 s under TCG (full-screen software composite).
- `udrv::boot` waits for e1000d's link and its DHCP inline (50 ms under KVM;
  up to 10 s without a link).
- The lock screen paints its background with `fill_rounded_rect_grad` over
  the whole screen (per-pixel float): 25 to 45 s per frame for the self-test
  image under TCG at 1920x1080, two frames per boot (BUG-056 in
  `docs/LOADTEST-TRACKER.md`). The release image on a live medium skips the
  lock screen; on an installed system under KVM the first frame and a sign-in
  attempt together take 0.44 s.

Closed since: the inline bring-up of the self-test image looked for answers
before the configuration was saved (BUG-047: fixed by BUG-039 and `182b19c`;
`phase net` of a kernel-driver boot under TCG is 15.9 s, with the HTTP and
HTTPS probes answered).

# EuroKernel

Europees soeverein besturingssysteem, **from scratch** in Rust. Microkernel ·
capability-based · UEFI · nul telemetrie · EUPL 1.2. Zie `STAPPENPLAN.md` voor de
volledige bouwroadmap en de tracks.

## Wat hier al werkt (geverifieerd)

| Onderdeel | Track | Status |
|---|---|---|
| Bootable UEFI-kernel + GOP-framebuffer | 1 | ✅ boot in QEMU, `boot.png` |
| EuroFS — ramdisk + on-disk **CoW**-FS (inodes, extents, checkpoints, crash-consistent) | 2 | ✅ 36 host-tests; in kernel gemount, `boot-eurofs.png` |
| Interactieve shell (UEFI-toetsenbord) — ls/cat/write/mkdir/df/net | 1 | ✅ live op EuroFS, `shell.png` |
| EuroNet — Ethernet/ARP/IPv4/ICMP/UDP parse+build+checksums | 4 | ✅ 13 host-tests; `net`-selftest in kernel, `net.png` |
| EuroMM — fysieke frame-allocator + UEFI memory-map | 3.1 | ✅ 6 host-tests; `mem`-commando, `mem.png` |
| **Kernelmodus** — ExitBootServices, eigen GDT/IDT + exceptions, PS/2-driver, panic-handler, COM1 | 3.2 | ✅ volledige shell zónder UEFI, `kernelmode-typed.png` |
| **Preemptief multitasking** — PIT-timer-IRQ + round-robin scheduler met context-switch | 3.3 | ✅ 3 achtergrondtaken + shell parallel, `sched-typed.png` |
| **IRQ-toetsenbord** — scancode-ringbuffer in de IRQ1-handler | 3.4 | ✅ geen tekenverlies, `irqkbd.png` |
| **Eigen paging** — 4-niveau page tables, eigen CR3 | 3.3 | ✅ identity-map, `paging.png` |
| **Ring-3 userspace + SYSCALL** — privilege-scheiding | 3.4 | ✅ userspace trapt naar kernel, `ring3.png` |
| **Echt userspace-programma** — `/bin/hello` uit EuroFS, sys_write/sys_exit, SYSCALL→SYSRET | 3.4 | ✅ programma print via syscall, `userspace.png` |
| **Userspace-multitasking** — ring-3 proces preemptief geschedulet naast kernel-threads + shell | 3.4 | ✅ 5-weg round-robin, `usersched-typed.png` |
| **EuroDesktop compositor** — vensters (z-order, titelbalken), sidebar, PS/2-muis, slepen | 5 | ✅ desktop in prototype-stijl, `desktop.png`/`mouse-drag.png` |
| **EuroToolchain** — C-broncode → gcc → flat PIC-binary → draait in ring 3 op de kernel | 6 | ✅ `/bin/hello` (100 B) uit EuroFS, `toolchain.png` |
| **eupkg** — package-manager: `.eupkg` met SHA256 + Ed25519, tamper-detectie | 6 | ✅ build/verify/info, `toolchain/eupkg/` |

Totaal **55 host-tests** groen, clippy schoon, kernel boot + screenshot in CI.
De kernel draait in twee fases: UEFI-bring-up → `ExitBootServices` → eigen kernelmodus
(eigen heap, GDT, IDT, framebuffer, toetsenbord). Debug via COM1 (`-serial file:serial.log`).

## Structuur

```
eurokernel/
├── kernel/            # no_std UEFI-binary (Track 1): main, graphics, font, desktop
├── crates/eurofs/     # host-testbare FS-logica (Track 2): block, path, fs, ramdisk, superblock, checksum
├── design/            # UI-prototype (Track 5 huisstijl-referentie)
├── scripts/           # build.sh, run-qemu.sh, screenshot.py, test.sh
└── .github/workflows/ # CI: tier-1 tests + tier-2 QEMU-boot
```

De gedeelde `eurofs`-crate is `no_std` + `alloc` voor de kernel, maar test onder
`std` op de host — dezelfde code, getest zonder VM.

## Vereisten

```bash
# Rust (rust-toolchain.toml pint nightly + x86_64-unknown-uefi automatisch)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
# Systeemtools (voor image + QEMU-boot)
sudo apt install qemu-system-x86 ovmf dosfstools mtools   # Debian/Ubuntu
```

## Bouwen & testen

```bash
./scripts/test.sh          # alles: host-tests, clippy, kernel-build, QEMU-boot + screenshot
# of los:
cargo test -p eurofs       # tier 1 — logica (geen VM)
cargo kbuild-release       # tier 2 — UEFI-binary (alias, zie .cargo/config.toml)
./scripts/build.sh         # bootable eurokernel.img
./scripts/run-qemu.sh      # boot met GUI (of headless zonder $DISPLAY)
python3 scripts/screenshot.py eurokernel.img boot.png   # headless boot + PNG
```

## Testniveaus

1. **Logica** — `cargo test` op de host. Snel, geen VM. Het meeste werk hier.
2. **Boot/integratie** — QEMU + OVMF, headless screenshot in CI.
3. **Hardware** — `dd` naar USB, boot op referentielaptop (Secure Boot uit).

## Security

Security vulnerabilities: please email **jeroen@gotrust.be** privately — do not open a public issue. See [`SECURITY.md`](SECURITY.md).

## License

Copyright (c) 2026 EuroOS Contributors / GoTrust.

EuroOS is licensed under the **European Union Public Licence (EUPL) v1.2** — see
[`LICENSE`](LICENSE). Third-party component licences are listed in [`NOTICE`](NOTICE).
Contributions are accepted under the EUPL v1.2 via the Developer Certificate of
Origin (`git commit -s`); see [`CONTRIBUTING.md`](CONTRIBUTING.md) and our
[`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md).

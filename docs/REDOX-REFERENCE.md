# Redox OS as a reference: learn from it, don't copy it

*Owner decision, 2026-11-02.* Redox OS (<https://gitlab.redox-os.org/redox-os>) is a Rust microkernel OS
that has been in development since 2015, under the MIT licence. It has solved many problems EuroOS still
faces. We use its work to **speed EuroOS up and as inspiration**, and we **do not take it over one to
one**. EuroOS keeps its own architecture and identity (see `AGENT-BRIEFING.md` §1: capabilities,
EuroIPC, `Euro*`, the Linux ABI only as a bridge).

## Allowed, encouraged

- **Study** how Redox solves a problem: hardware bring-up, drivers, scheduling, filesystem, POSIX
  semantics. Read the code, the commits, the Redox Book and the issues before designing something
  hard yourself.
- **Use it as an oracle.** When EuroOS behaves differently from Redox on the same hardware or the
  same syscall, find out why. Boot Redox in QEMU next to EuroOS as a comparison.
- **Write it again in the EuroOS way:** a pure, host-tested `crates/euro*` core plus thin kernel
  glue with a `[xx]` boot self-test, capability checks at the boundary, `#![forbid(unsafe_code)]`
  in library crates.
- **Take over hardware facts** (register offsets, init sequences, quirks of specific chips). Those
  are facts, not creative work. Still name the source (spec, datasheet or Redox file) in a comment.

## Not allowed

- **Copy files, modules or larger blocks unchanged** and present them as EuroOS code.
- **Take over Redox's architecture as EuroOS's identity**, such as schemes (`/scheme/...`) instead
  of EuroIPC/capabilities, or a Unix permission model instead of EuroGuard.
- **Use "Redox" in marketing** other than descriptively. "Redox OS" is a trademark
  (`TRADEMARK.md` in their repo). Never claim that EuroOS is "the first" or "the only" OS in Rust
  or on a microkernel.

## When code is actually derived from Redox

A function, a table or an algorithm structure adapted from Redox code (more than a hardware fact):

1. **Source comment** above it:
   `// Adapted from Redox OS <repo>/<path> @ <commit>, MIT, Copyright (c) <year> <holder>.`
2. **NOTICE**: add the component under *THIRD-PARTY COMPONENTS* with the MIT licence and the
   copyright line from that repo's `LICENSE`. They differ per repo, for example kernel "(c) 2017
   Jeremy Soller", RedoxFS "(c) 2016 Jeremy Soller", relibc "(c) 2018 Redox OS". Check the
   `LICENSE` of the exact repo. Include the MIT licence text (MIT requires that).
3. **Commit message**: name the Redox source and commit.

MIT code may be included in EUPL code, provided that notice is preserved. Without it, it is a
licence violation, so never leave it out.

## Where Redox is most useful to EuroOS

Check against the current Redox tree. These are starting points, not guarantees.

| EuroOS topic | Where to look in Redox |
|---|---|
| APIC / I/O APIC / x2APIC, TSC-deadline timer, SMP, NUMA | `kernel` (recent work in October 2026 on IRQ routing and the LAPIC tick) |
| Futex, scheduler, affinity | `kernel` |
| ACPI / AML (power, shutdown, devices) | `base/drivers/acpid`, `amlserde` |
| PCI enumeration, drivers in userspace | `base/drivers/pcid`, `pcid-spawner` |
| NVMe, AHCI, USB (xHCI), NICs, audio, VirtIO | `base/drivers/storage`, `usb`, `net`, `audio`, `virtio-core` |
| POSIX/Linux semantics for the compat bridge | `relibc` (what programs expect from signals, threads, sockets) |
| CoW filesystem, encryption | `redoxfs` (compare with EuroFS) |
| Display server, window manager | `orbital` |
| Porting existing software | the "recipes" in the `redox` build repository |
| ARM64 / RISC-V (Horizon later) | `kernel` arch modules |

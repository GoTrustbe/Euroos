# Contributing to EuroOS

Thanks for your interest in EuroOS — a sovereign, security-first operating system
built from scratch in Rust (`no_std`, x86-64 UEFI), with no Linux or BSD
underneath. This document explains how to build it, the standards we hold every
change to, and how contributions are licensed and governed.

## License of contributions (DCO)

EuroOS is licensed under the **European Union Public Licence (EUPL) v1.2**
(see [`LICENSE`](LICENSE)). By contributing, you agree your contribution is
provided under the EUPL v1.2.

We use the **Developer Certificate of Origin (DCO)** — lightweight, no CLA. Sign
off every commit with:

```
git commit -s -m "your message"
```

The `Signed-off-by:` line certifies you wrote the patch (or have the right to
submit it) under the project's licence, per <https://developercertificate.org/>.

## Building & running

You need a recent Rust **nightly** (the toolchain is pinned in
`rust-toolchain.toml`), plus `qemu-system-x86`, `mtools`, and `python3`.

```bash
./scripts/build.sh release      # builds the kernel + loader, makes a bootable FAT32 image
./scripts/run-qemu.sh           # boots it in QEMU (use KVM/HVF/WHPX for ~1.5–2 s boot)
./scripts/test.sh               # the canonical gate: host tests + kernel build + boot
```

### The signing key (important)

EuroOS verifies every userland binary with an **Ed25519 signature** before it
runs. The matching **public** key (`toolchain/eupkg/keys/dev.pub`) is committed and
embedded in the kernel. The **private** seed (`dev.key`) is **NOT** in the
repository — publishing it would let anyone forge "verified" binaries.

To build the signed userland locally, generate your own dev keypair:

```bash
python3 toolchain/eupkg/gen-dev-key.py    # writes a fresh dev.key + dev.pub
./scripts/build.sh release
```

This overwrites your local `dev.pub` (and the kernel embeds yours); that is fine
for development. Official release builds are signed with a key held offline.

## Standards — how we work

EuroOS has two non-negotiable habits; please follow them:

1. **Verify by running.** A change is not "done" until `cargo test` is green for
   the affected crate **and**, where it touches the kernel, a boot self-test
   marker (`[xx]`) proves it live. Quote the actual output in your PR. If tests
   fail, say so.

2. **Never present mock as real.** Distinguish "the engine works" from "the
   feature works end-to-end." If a path can't reach a real peer/backend, it must
   say `[mock]` — never pretend. Under-claim rather than over-claim.

The pattern you'll see everywhere: the security-critical logic lives in a pure,
host-tested `crates/euro*` library (compiles under `std`, `cargo test`); the
kernel module is thin glue wiring it onto hardware and printing a `[xx]` boot
self-test. Replicate this for anything you add — that's why there are 700+ host
tests *and* a boot self-test for almost everything.

See [`docs/AGENT-BRIEFING.md`](docs/AGENT-BRIEFING.md) for a fuller orientation
and the current backlog, and [`docs/EUROOS-DEEP-TECHNICAL-REFERENCE.md`](docs/EUROOS-DEEP-TECHNICAL-REFERENCE.md)
for the per-subsystem reference.

## Pull requests

- Keep PRs focused; one logical change per PR.
- Run `./scripts/test.sh` and paste the result.
- Match the surrounding code's style, comment density, and naming.
- Security-sensitive changes (crypto, the syscall boundary, capability checks,
  signature verification) get extra scrutiny — explain the threat model.

## Reporting security issues

Please **do not** open a public issue for security vulnerabilities. Report them
privately to the maintainers (see the repository's Security policy / contact in
the README). We'll coordinate a fix and disclosure.

## Reporting hardware compatibility

EuroOS keeps an honest **Hardware Compatibility List**
([`docs/HARDWARE-COMPAT.md`](docs/HARDWARE-COMPAT.md)): what is verified working,
what has a protocol core but needs real silicon, and what is unsupported. To add
a result, open an issue titled `HCL: <vendor> <device>` with the device IDs,
the EuroOS commit, and the relevant serial-log `[...]` markers or a screenshot.
A device only enters the *verified* table once a maintainer can reproduce or
accept the report from that evidence — never on an unverified claim.

Support commitments and the release/security-update cadence are stated in
[`SUPPORT-POLICY.md`](SUPPORT-POLICY.md).

## Governance (current)

EuroOS is at an early stage. Today the GoTrust maintainers own merge rights and
the roadmap. As the contributor base grows we intend to publish a fuller
governance model (maintainer roles, decision process, trademark policy). Until
then: open an issue to discuss anything substantial before a large PR.

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). By
participating you agree to uphold it.

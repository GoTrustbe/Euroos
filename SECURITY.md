# Security Policy

EuroOS is a security-first, sovereign operating system. We take vulnerability
reports seriously and appreciate responsible disclosure.

## Reporting a vulnerability

**Please do not open a public issue for security vulnerabilities.**

Report them privately by email to:

> **info@euro-os.eu**

Please include, where possible:

- a description of the issue and its impact,
- the affected component (kernel, a `crates/euro*` library, the toolchain, …),
- steps to reproduce or a proof of concept,
- the build/commit you tested against.

We will acknowledge your report, work with you on a fix, and coordinate
disclosure. Please give us reasonable time to release a fix before any public
disclosure.

## Scope

In scope: the EuroOS kernel, its `crates/euro*` libraries, the build/signing
toolchain, and the boot/verification chain. Out of scope: third-party
dependencies (report those upstream) and the preview's intentionally weak demo
credentials (e.g. the seeded `euro` demo account).

## Note on the signing key

EuroOS verifies userland binaries with Ed25519 signatures. The project's
release-signing private key is held offline and is **not** in this repository —
only the public verification key (`toolchain/eupkg/keys/dev.pub`) is distributed.

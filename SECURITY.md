# Security Policy

EuroOS is a security-first, sovereign operating system. We take vulnerability
reports seriously and appreciate responsible disclosure.

This policy also documents EuroOS's **coordinated vulnerability disclosure (CVD)**
and **vulnerability-handling process**. EuroOS is a "product with digital
elements", so the EU **Cyber Resilience Act (CRA)** applies to it; this document
is part of how we meet the CRA's vulnerability-handling obligations (Regulation
(EU) 2024/2847, Article 13 and Annex I Part II). See
[`docs/CRA-CONFORMANCE.md`](docs/CRA-CONFORMANCE.md) for the full mapping and the
compliance timeline.

## Reporting a vulnerability

**Please do not open a public issue for security vulnerabilities.**

Report privately, whichever you prefer:

- **Email:** **info@euro-os.eu** (use "SECURITY" in the subject).
- **GitHub:** open a private advisory via *Security → Report a vulnerability* on
  [github.com/GoTrustbe/Euroos](https://github.com/GoTrustbe/Euroos/security/advisories/new).

Please include, where possible:

- a description of the issue and its impact,
- the affected component (kernel, a `crates/euro*` library, the toolchain, …) —
  the release **SBOM** (`sbom.cdx.json`, attached to each release) can help you
  pin the exact component and version,
- steps to reproduce or a proof of concept,
- the build/commit you tested against.

## What to expect (our commitments)

| Stage | Target |
|-------|--------|
| **Acknowledgement** of your report | within **3 business days** |
| **Triage** (severity + affected versions, using CVSS) | within **10 business days** |
| **Fix or mitigation** for high/critical issues | as fast as practical; we keep you updated |
| **Coordinated public disclosure** | by default **90 days** after the report, or when a fix ships — whichever is sooner, agreed with you |

We will request a **CVE** where appropriate and publish a **GitHub Security
Advisory** for confirmed vulnerabilities.

## How we handle vulnerabilities (CRA-aligned process)

1. **Intake & acknowledgement** — every report is logged privately and
   acknowledged.
2. **Triage** — reproduce, assess impact (CVSS), and identify affected
   components and versions. The machine-readable **SBOM** lets us determine
   whether an upstream dependency advisory affects EuroOS and which release.
3. **Remediation** — develop and test a fix (host tests green **and** a boot
   marker, per our engineering discipline), with a mitigation/workaround where a
   full fix takes time.
4. **Release** — ship a security update through the signed A/B update channel
   (Ed25519 verify-before-activate), and record it in a security advisory + the
   changelog.
5. **Disclosure** — coordinate public disclosure with the reporter on the
   agreed timeline, crediting reporters who wish it.
6. **Upstream** — for issues in third-party dependencies, we report upstream and
   pull in the fixed version (the SBOM tracks exactly which versions we ship).

## Security updates & support period

EuroOS is currently an **alpha preview**; while it is pre-1.0 we provide security
fixes on a best-effort basis against the `main` branch. On the first stable
release we will publish a defined **support period** during which security
updates are provided, as required by the CRA — see
[`docs/CRA-CONFORMANCE.md`](docs/CRA-CONFORMANCE.md).

## Safe harbour

We will not pursue or support legal action against anyone who, in good faith:

- makes a reasonable effort to report privately and promptly,
- avoids privacy violations, data destruction, and service degradation,
- only interacts with systems/accounts they own or have explicit permission to
  test.

Research within these bounds is authorised and welcomed.

## Scope

In scope: the EuroOS kernel, its `crates/euro*` libraries, the build/signing
toolchain, and the boot/verification chain. Out of scope: third-party
dependencies (report those upstream — we will pull in the fix), and the
preview's intentionally weak demo credentials (e.g. the seeded `euro` demo
account).

## Note on the signing key

EuroOS verifies userland binaries with Ed25519 signatures. The project's
release-signing private key is held offline and is **not** in this repository —
only the public verification key (`toolchain/eupkg/keys/dev.pub`) is distributed.

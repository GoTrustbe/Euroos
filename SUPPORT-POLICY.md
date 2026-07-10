# EuroOS Support & Release Policy

This policy states, in advance, how long a EuroOS release is supported and how
security fixes reach users. It exists both as OSS governance (3E-7) and as
**Cyber Resilience Act** supporting evidence: the CRA requires a manufacturer to
define a *support period* and to deliver security updates throughout it. See
[`docs/CRA-CONFORMANCE.md`](docs/CRA-CONFORMANCE.md) for the full mapping.

> **Alpha notice.** EuroOS is pre-1.0 alpha software. The commitments below
> describe the policy that takes effect **at the 1.0 release**; before 1.0 there
> are no stability or support guarantees, and interfaces may change without
> notice.

## Release channels

| Channel | Contents | Cadence |
|---|---|---|
| `stable` | tagged releases (`vMAJOR.MINOR.PATCH`) | on maturity milestones |
| `beta`   | release candidates | before each stable |
| `nightly`/`main` | development head | continuous (no guarantees) |

Every stable release ships:

- the bootable image (`eurokernel.img`),
- a **signed release manifest** (`release-manifest.json` + `.sig`) binding the
  source commit to the exact binary hashes (Ed25519, the project dev key — the
  same trust root that gates A/B updates), produced by the `release` CI job,
- a **CycloneDX SBOM** (`sbom.cdx.json`, CRA Annex II).

Releases are built reproducibly (`scripts/repro-build.sh`); the CI
`repro-check` job double-builds and compares the kernel hash so a third party
can independently reproduce a release from its commit.

## Support period

From the **1.0 release**, each stable **minor** release (`vX.Y`) is supported
with security updates for **18 months** from its release date. Two minor
releases are supported at any time (the current and the previous), so upgrading
across one minor boundary is never forced by an urgent security fix.

The CRA's minimum support-period expectation (at least 5 years for many product
classes, or the product's expected lifetime) will be re-assessed and this figure
raised before EuroOS is offered as a CRA-in-scope commercial product; the 18-month
figure is the initial community-release commitment, not a ceiling.

## Security updates

- Vulnerabilities are handled under [`SECURITY.md`](SECURITY.md) (coordinated
  disclosure, triage SLAs, 90-day default disclosure).
- Fixes are delivered as **signed** A/B updates: the loader verifies the Ed25519
  signature before activating a slot and rolls back automatically if the new
  slot fails to boot (`euroupdate`). A hostile mirror can withhold an update but
  cannot forge one.
- Security releases are announced via the repository's advisories and the
  `stable` update channel.

## End of life

When a minor release reaches end of support, it is marked EOL in the release
notes and the HCL; users are directed to the current supported minor. EOL does
not remove already-published artifacts or their manifests.

## Hardware support

The tested hardware envelope is documented in
[`docs/HARDWARE-COMPAT.md`](docs/HARDWARE-COMPAT.md). Support commitments apply
only within that envelope.

---

*Last revised 2026-07-10 (Phase 3E). This policy will be restated in the Annex II
technical documentation at the 1.0 release.*

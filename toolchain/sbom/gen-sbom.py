#!/usr/bin/env python3
"""EuroSBOM — a from-scratch, deterministic CycloneDX 1.5 SBOM generator.

The EU Cyber Resilience Act (CRA) requires a machine-readable Software Bill of
Materials for a "product with digital elements". EuroOS is one, so the CRA
applies to us. This tool reads the pinned Cargo dependency graph (`cargo
metadata`, backed by the committed `Cargo.lock`) and emits a valid CycloneDX 1.5
JSON SBOM listing every component with its version, license and package URL, and
flags which components are EuroOS first-party (sovereign) code versus upstream
dependencies.

Deterministic by design (sorted components, caller-supplied timestamp, content-
hashed serial number) so the SBOM is reproducible — it hashes identically for an
identical `Cargo.lock`, which is exactly what a verifiable release pipeline and a
CRA auditor want.

Usage:
    gen-sbom.py [--timestamp ISO8601] [--version VERSION] [-o out.json]
    gen-sbom.py --check out.json        # structural self-validation
"""

import argparse
import hashlib
import json
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SPEC_VERSION = "1.5"
# A fixed namespace so the content-hashed serial number is stable across runs.
NS = "6f5c0d3e-euroos-sbom"


def cargo_metadata():
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--all-features"],
        cwd=REPO, check=True, capture_output=True, text=True,
    )
    return json.loads(out.stdout)


def license_obj(expr):
    """Map a Cargo SPDX license string to a CycloneDX license entry."""
    if not expr:
        return []
    # An SPDX *expression* (OR/AND/WITH) goes in `expression`; a single id in `id`.
    if any(op in expr for op in (" OR ", " AND ", " WITH ", "(", ")")):
        return [{"expression": expr}]
    return [{"license": {"id": expr}}]


def build_bom(md, timestamp, product_version):
    members = set(md.get("workspace_members", []))
    components = []
    for pkg in md["packages"]:
        pkg_id = pkg["id"]
        name = pkg["name"]
        version = pkg["version"]
        purl = f"pkg:cargo/{name}@{version}"
        first_party = pkg_id in members
        comp = {
            "type": "application" if name in ("kernel", "loader") else "library",
            "bom-ref": purl,
            "name": name,
            "version": version,
            "purl": purl,
            "licenses": license_obj(pkg.get("license")),
            "properties": [
                {"name": "euroos:first-party", "value": "true" if first_party else "false"},
            ],
        }
        # Record the upstream source for third-party components (provenance).
        src = pkg.get("source")
        if src:
            comp["properties"].append({"name": "cargo:source", "value": src})
        components.append(comp)

    # Deterministic order: by purl.
    components.sort(key=lambda c: c["purl"])

    # Content-hashed serial number (stable for identical inputs).
    digest = hashlib.sha256(
        json.dumps([c["purl"] for c in components], sort_keys=True).encode()
    ).hexdigest()
    serial = f"urn:uuid:{digest[0:8]}-{digest[8:12]}-{digest[12:16]}-{digest[16:20]}-{digest[20:32]}"

    bom = {
        "bomFormat": "CycloneDX",
        "specVersion": SPEC_VERSION,
        "serialNumber": serial,
        "version": 1,
        "metadata": {
            "timestamp": timestamp,
            "tools": [{"vendor": "EuroOS", "name": "eurosbom", "version": "0.1.0"}],
            "component": {
                "type": "operating-system",
                "bom-ref": "euroos",
                "name": "EuroOS",
                "version": product_version,
                "licenses": [{"license": {"id": "EUPL-1.2"}}],
                "supplier": {"name": "GoTrust BV", "url": ["https://euro-os.eu"]},
            },
        },
        "components": components,
    }
    return bom


def check(path):
    """Structural self-validation against the CycloneDX 1.5 essentials."""
    bom = json.loads(Path(path).read_text())
    errors = []
    if bom.get("bomFormat") != "CycloneDX":
        errors.append("bomFormat != CycloneDX")
    if bom.get("specVersion") != SPEC_VERSION:
        errors.append(f"specVersion != {SPEC_VERSION}")
    comps = bom.get("components", [])
    if not comps:
        errors.append("no components")
    seen = set()
    for c in comps:
        for req in ("type", "name", "version", "purl"):
            if not c.get(req):
                errors.append(f"component missing {req}: {c.get('name')}")
        if c["purl"] in seen:
            errors.append(f"duplicate purl {c['purl']}")
        seen.add(c["purl"])
    fp = sum(1 for c in comps if any(p.get("name") == "euroos:first-party" and p.get("value") == "true" for p in c.get("properties", [])))
    if errors:
        for e in errors:
            print(f"  INVALID: {e}", file=sys.stderr)
        return False
    print(f"  OK: valid CycloneDX {SPEC_VERSION}, {len(comps)} components ({fp} EuroOS first-party, {len(comps) - fp} upstream)")
    return True


def main():
    ap = argparse.ArgumentParser(description="Generate a CycloneDX SBOM for EuroOS")
    ap.add_argument("--timestamp", default="1970-01-01T00:00:00Z", help="ISO-8601 build timestamp (fixed for reproducibility)")
    ap.add_argument("--version", default="alpha", help="EuroOS product version/tag")
    ap.add_argument("-o", "--output", default=str(REPO / "sbom.cdx.json"))
    ap.add_argument("--check", metavar="FILE", help="validate an existing SBOM instead of generating")
    args = ap.parse_args()

    if args.check:
        sys.exit(0 if check(args.check) else 1)

    bom = build_bom(cargo_metadata(), args.timestamp, args.version)
    text = json.dumps(bom, indent=2, sort_keys=False) + "\n"
    Path(args.output).write_text(text)
    fp = sum(1 for c in bom["components"] if any(p["name"] == "euroos:first-party" and p["value"] == "true" for p in c["properties"]))
    print(f"wrote {args.output}: CycloneDX {SPEC_VERSION}, {len(bom['components'])} components ({fp} first-party, {len(bom['components']) - fp} upstream)")


if __name__ == "__main__":
    main()

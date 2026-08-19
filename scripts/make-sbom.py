#!/usr/bin/env python3
"""Generate the CycloneDX SBOM and the Rust table the About dialog renders.

Runs `cargo cyclonedx`, then post-processes the result so it is fit to commit
and to ship inside the binary:

* **Reproducible.** `cargo cyclonedx` stamps a random `serialNumber` and the
  wall-clock time, so two builds of identical source produce different SBOMs.
  Both are made deterministic here (UUIDv5 over name+version; timestamp from
  `SOURCE_DATE_EPOCH` or the HEAD commit date). Without this, "is the committed
  SBOM current?" is unanswerable in CI.
* **Free of build-machine paths.** Raw `bom-ref` values embed the absolute
  source directory (`path+file:///C:/Users/...`), which leaks the developer's
  filesystem layout into a published artefact and differs per machine.
* **CRA-complete.** Adds the supplier identification and support-period
  properties that Annex II expects and `cargo cyclonedx` does not know about.

The document carries the product version, so it must be regenerated whenever
the version is bumped -- `scripts/version.py` does not do it for you, and CI
fails if the committed copy is stale.

Outputs:
    docs/compliance/sbom.cdx.json   committed, shipped, linked from About
    src/sbom_data.rs                generated Rust table, no runtime JSON parser

Usage:
    python scripts/make-sbom.py           # regenerate
    python scripts/make-sbom.py --check   # fail if the committed copy is stale
"""

import json
import os
import re
import subprocess
import sys
import uuid
from datetime import datetime, timezone

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT_JSON = os.path.join(ROOT, "docs", "compliance", "sbom.cdx.json")
OUT_RS = os.path.join(ROOT, "src", "sbom_data.rs")
RAW = os.path.join(ROOT, "supertile.cdx.json")

SUPPLIER = {
    "name": "Andreas Wiren",
    "url": ["https://github.com/andreaswiren/supertile"],
    "contact": [{"name": "Andreas Wiren", "email": "andreas.wiren@gmail.com"}],
}
# CRA Article 13(8): the manufacturer states a support period.
SUPPORT_YEARS = 5
# Stable namespace so the serial number is a pure function of the product.
NS = uuid.UUID("6f9619ff-8b86-d011-b42d-00c04fc964ff")


def run(cmd, **kw):
    return subprocess.run(cmd, cwd=ROOT, check=True, capture_output=True, text=True, **kw)


def deterministic_timestamp():
    """A timestamp that does not change unless the source does."""
    epoch = os.environ.get("SOURCE_DATE_EPOCH")
    if epoch:
        return datetime.fromtimestamp(int(epoch), timezone.utc)
    try:
        out = run(["git", "log", "-1", "--format=%cI"]).stdout.strip()
        if out:
            return datetime.fromisoformat(out).astimezone(timezone.utc)
    except Exception:
        pass
    return datetime.now(timezone.utc)


def strip_local_paths(obj):
    """Replace absolute-path bom-refs with stable, machine-independent ones."""
    if isinstance(obj, dict):
        return {k: strip_local_paths(v) for k, v in obj.items()}
    if isinstance(obj, list):
        return [strip_local_paths(v) for v in obj]
    if isinstance(obj, str):
        # path+file:///C:/Users/x/workspace/supertile#0.1.0 -> path+source#0.1.0
        s = re.sub(r"path\+file://[^#\s\"]*", "path+source", obj)
        s = s.replace("?download_url=file://.", "")
        return s
    return obj


def generate():
    run(["cargo", "cyclonedx", "--format", "json", "--spec-version", "1.5", "--all-features"])
    with open(RAW, encoding="utf-8") as f:
        bom = json.load(f)
    os.remove(RAW)

    bom = strip_local_paths(bom)

    name = bom["metadata"]["component"]["name"]
    version = bom["metadata"]["component"]["version"]

    bom["serialNumber"] = f"urn:uuid:{uuid.uuid5(NS, f'{name}@{version}')}"
    ts = deterministic_timestamp()
    bom["metadata"]["timestamp"] = ts.strftime("%Y-%m-%dT%H:%M:%SZ")
    bom["metadata"]["supplier"] = SUPPLIER
    bom["metadata"].setdefault("component", {})["supplier"] = SUPPLIER

    bom["metadata"]["properties"] = [
        {"name": "cra:supportPeriodYears", "value": str(SUPPORT_YEARS)},
        {"name": "cra:supportPeriodEnd",
         "value": ts.replace(year=ts.year + SUPPORT_YEARS).strftime("%Y-%m-%d")},
        {"name": "cra:vulnerabilityDisclosurePolicy",
         "value": "https://github.com/andreaswiren/supertile/blob/main/SECURITY.md"},
        {"name": "cra:productCategory",
         "value": "default (not important or critical per Annex III/IV)"},
        {"name": "supertile:networkAccess", "value": "none"},
        {"name": "supertile:requiresElevation", "value": "false"},
    ]

    # Sort components so diffs are readable and order-independent.
    bom.get("components", []).sort(key=lambda c: (c.get("name", ""), c.get("version", "")))
    for dep in bom.get("dependencies", []):
        dep.get("dependsOn", []).sort()
    bom.get("dependencies", []).sort(key=lambda d: d.get("ref", ""))

    os.makedirs(os.path.dirname(OUT_JSON), exist_ok=True)
    text = json.dumps(bom, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
    return bom, text


def rust_table(bom):
    """Emit the component table the About dialog renders."""
    rows = []
    for c in bom.get("components", []):
        if c.get("type") not in ("library", "framework", "application"):
            continue
        cname = c.get("name", "")
        cver = c.get("version", "")
        lic = ""
        for entry in c.get("licenses", []):
            if "expression" in entry:
                lic = entry["expression"]
                break
            if "license" in entry:
                lic = entry["license"].get("id") or entry["license"].get("name", "")
                break
        sha = ""
        for h in c.get("hashes", []):
            if h.get("alg") == "SHA-256":
                sha = h.get("content", "")
                break
        purl = c.get("purl", "")
        if not cname:
            continue
        rows.append((cname, cver, lic, purl, sha))

    # Drop the product's own sub-targets; they are not third-party components.
    product = bom["metadata"]["component"]["name"]
    rows = [r for r in rows if r[0] != product]
    rows.sort()

    def esc(s):
        return s.replace("\\", "\\\\").replace('"', '\\"')

    lines = [
        "//! Generated by scripts/make-sbom.py -- DO NOT EDIT.",
        "//!",
        "//! Mirrors docs/compliance/sbom.cdx.json so the About dialog can render",
        "//! the component list without linking a JSON parser into the binary.",
        "",
        "/// One third-party component: name, version, licence, purl, SHA-256.",
        "pub struct Component {",
        "    pub name: &'static str,",
        "    pub version: &'static str,",
        "    pub license: &'static str,",
        "    pub purl: &'static str,",
        "    pub sha256: &'static str,",
        "}",
        "",
        f"/// CycloneDX serial number of the SBOM these rows came from.",
        f'pub const SBOM_SERIAL: &str = "{esc(bom["serialNumber"])}";',
        f'pub const SBOM_TIMESTAMP: &str = "{esc(bom["metadata"]["timestamp"])}";',
        f'pub const SBOM_SPEC_VERSION: &str = "{esc(bom["specVersion"])}";',
        "",
        f"/// {len(rows)} third-party components.",
        "pub const COMPONENTS: &[Component] = &[",
    ]
    for (n, v, l, p, s) in rows:
        lines.append(
            f'    Component {{ name: "{esc(n)}", version: "{esc(v)}", '
            f'license: "{esc(l)}", purl: "{esc(p)}", sha256: "{esc(s)}" }},'
        )
    lines += ["];", ""]
    return rustfmt(chr(10).join(lines))


def rustfmt(text):
    """Format generated Rust exactly as `cargo fmt` would.

    Without this the generator and the formatter disagree: the generator
    emits one line per component, rustfmt wraps them, and CI fails
    `cargo fmt --check` on a file no human wrote. Formatting here rather
    than after writing also keeps `--check` honest, since it compares the
    generated text against the file and both sides must have been through
    the same formatter.
    """
    try:
        r = subprocess.run(
            ["rustfmt", "--edition", "2021", "--emit", "stdout", "--quiet"],
            input=text, capture_output=True, text=True, check=True,
        )
    except (OSError, subprocess.CalledProcessError) as e:
        # Returning unformatted Rust would turn CI red later, a long way
        # from the cause.
        raise SystemExit(f"rustfmt failed; cannot write src/sbom_data.rs: {e}")
    return r.stdout


def licence_of(c):
    """The licence expression or id, however CycloneDX chose to express it."""
    for entry in c.get("licenses", []) or []:
        if "expression" in entry:
            return entry["expression"]
        if "license" in entry:
            return entry["license"].get("id") or entry["license"].get("name", "")
    return ""


def hash_of(c):
    """The SHA-256 of the component, if the generator recorded one."""
    for h in c.get("hashes", []) or []:
        if h.get("alg") == "SHA-256":
            return h.get("content", "")
    return ""


def strip_stamps(text):
    """Drop the lines recording when and under what serial this was generated."""
    if not text:
        return ""
    return chr(10).join(
        line
        for line in text.splitlines()
        if not line.startswith(("pub const SBOM_SERIAL", "pub const SBOM_TIMESTAMP"))
    )


def read_json(path):
    """The committed document, or an empty one if it is missing."""
    if not os.path.exists(path):
        return {}
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return {}


def components_of(bom):
    """The part of an SBOM that is a factual claim about this build.

    Name, version, licence, package URL and hash for every component, sorted.
    Everything else in the document -- serial number, timestamp, the generator's
    own version -- describes the act of generating it, not the software it
    describes.
    """
    out = []
    for c in bom.get("components", []) or []:
        out.append(
            (
                c.get("name", ""),
                c.get("version", ""),
                licence_of(c),
                c.get("purl", ""),
                hash_of(c),
            )
        )
    return sorted(out)


def report_component_drift(have, want):
    """Say which dependencies changed, so the failure is actionable."""
    have_set, want_set = set(have), set(want)
    for name, version, *_ in sorted(want_set - have_set):
        print(f"  + {name} {version}", file=sys.stderr)
    for name, version, *_ in sorted(have_set - want_set):
        print(f"  - {name} {version}", file=sys.stderr)


def main():
    check = "--check" in sys.argv
    bom, json_text = generate()
    rs_text = rust_table(bom)

    if check:
        # Compare the claim, not the file.
        #
        # The claim an SBOM makes is "these are the dependencies this build has,
        # at these versions, under these licences". Byte-for-byte comparison
        # asserts far more than that: the CycloneDX generator writes its own
        # name and version into the document, so a newer cargo-cyclonedx on the
        # CI runner than on the machine that committed the file fails the check
        # while every component still agrees. That is a red build carrying no
        # information, and a check that cries wolf gets switched off.
        stale = []
        want_parts = components_of(bom)
        have_parts = components_of(read_json(OUT_JSON))
        if want_parts != have_parts:
            stale.append(os.path.relpath(OUT_JSON, ROOT))
            report_component_drift(have_parts, want_parts)
        # Same rule for the generated Rust: compare what it asserts about the
        # build, not the serial number and timestamp of the document it came
        # from. Those change on every regeneration, so including them would
        # fail this check on any machine that had not just run the generator --
        # which is every CI runner, every time.
        have_rs = open(OUT_RS, encoding="utf-8").read() if os.path.exists(OUT_RS) else None
        if strip_stamps(have_rs) != strip_stamps(rs_text):
            stale.append(os.path.relpath(OUT_RS, ROOT))
        if stale:
            print("SBOM is out of date: " + ", ".join(stale), file=sys.stderr)
            print("Run: python scripts/make-sbom.py", file=sys.stderr)
            return 1
        print("SBOM is up to date")
        return 0

    with open(OUT_JSON, "w", encoding="utf-8", newline="\n") as f:
        f.write(json_text)
    with open(OUT_RS, "w", encoding="utf-8", newline="\n") as f:
        f.write(rs_text)
    n = len([c for c in bom.get("components", [])])
    print(f"wrote {os.path.relpath(OUT_JSON, ROOT)} ({n} components)")
    print(f"wrote {os.path.relpath(OUT_RS, ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

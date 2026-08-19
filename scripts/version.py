#!/usr/bin/env python3
"""SemVer bookkeeping: bump the version and check everything agrees.

Four things have to say the same number — the Cargo manifest, the top heading
of the changelog, the version compiled into the binary, and the git tag. Any one
of them drifting makes "which build am I running?" unanswerable, which is also
a problem for the CRA documentation, since it claims each release is identified
and changelogged.

So the agreement is checked rather than trusted.

    python scripts/version.py --check          verify everything agrees
    python scripts/version.py --show           print the current version
    python scripts/version.py patch "Summary"  bump and open a changelog section
    python scripts/version.py minor "Summary"
    python scripts/version.py major "Summary"

`--check` is what CI runs. The bump commands edit Cargo.toml and CHANGELOG.md
and stop there; committing and tagging is left to RELEASING.md so nothing is
pushed by accident.
"""

import datetime
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CARGO = os.path.join(ROOT, "Cargo.toml")
CHANGELOG = os.path.join(ROOT, "CHANGELOG.md")

SEMVER = re.compile(r"^(\d+)\.(\d+)\.(\d+)$")
# Only the first `version = ` in [package]; dependency versions must not match.
CARGO_VERSION = re.compile(r'^version\s*=\s*"([^"]+)"', re.M)
# "## [1.2.3] — 2026-01-01", tolerating an em dash or a hyphen.
HEADING = re.compile(r"^##\s*\[(\d+\.\d+\.\d+)\]\s*[—-]\s*(\d{4}-\d{2}-\d{2})\s*$", re.M)


def fail(msg):
    print(f"error: {msg}", file=sys.stderr)
    raise SystemExit(1)


def cargo_version():
    text = open(CARGO, encoding="utf-8").read()
    # Restrict the search to the [package] table so a dependency pinned to
    # "1.2.3" can never be mistaken for the product version.
    pkg = text.split("[dependencies]")[0]
    m = CARGO_VERSION.search(pkg)
    if not m:
        fail("no version found in the [package] table of Cargo.toml")
    v = m.group(1)
    if not SEMVER.match(v):
        fail(f"Cargo.toml version {v!r} is not SemVer (MAJOR.MINOR.PATCH)")
    return v


def changelog_versions():
    """Every released version heading, newest first."""
    text = open(CHANGELOG, encoding="utf-8").read()
    return HEADING.findall(text)


def git(*args):
    try:
        out = subprocess.run(
            ["git", *args], cwd=ROOT, capture_output=True, text=True, check=True
        )
        return out.stdout.strip()
    except Exception:
        return ""


def check():
    problems = []
    version = cargo_version()
    entries = changelog_versions()

    if not entries:
        fail("CHANGELOG.md has no released version headings")

    top, top_date = entries[0]
    if top != version:
        problems.append(
            f"Cargo.toml says {version} but the newest changelog entry is {top}"
        )

    # Versions must descend, and none may repeat.
    seen = set()
    parsed = []
    for v, _ in entries:
        if v in seen:
            problems.append(f"{v} appears more than once in the changelog")
        seen.add(v)
        parsed.append(tuple(int(p) for p in v.split(".")))
    for a, b in zip(parsed, parsed[1:]):
        if a <= b:
            problems.append(
                f"changelog is out of order: {'.'.join(map(str, a))} "
                f"is listed above {'.'.join(map(str, b))}"
            )

    try:
        datetime.date.fromisoformat(top_date)
    except ValueError:
        problems.append(f"{top} has a malformed date {top_date!r}")

    # A tag for this version, if present, must point at a commit that exists.
    tags = set(git("tag", "-l").split())
    if f"v{version}" in tags:
        print(f"note: v{version} is already tagged")

    # Every released version should eventually be tagged; warn, do not fail,
    # because the tag is created after the release commit.
    untagged = [v for v, _ in entries if f"v{v}" not in tags]
    if untagged:
        print(f"note: not yet tagged: {', '.join(untagged)}")

    if problems:
        for p in problems:
            print(f"error: {p}", file=sys.stderr)
        print(
            "\nRun: python scripts/version.py <patch|minor|major> \"Summary\"",
            file=sys.stderr,
        )
        return 1

    print(f"version {version} — Cargo.toml, CHANGELOG.md and history agree")
    return 0


def bump(kind, summary):
    major, minor, patch = (int(p) for p in cargo_version().split("."))
    if kind == "major":
        major, minor, patch = major + 1, 0, 0
    elif kind == "minor":
        minor, patch = minor + 1, 0
    elif kind == "patch":
        patch += 1
    else:
        fail(f"unknown bump {kind!r}; use major, minor or patch")
    new = f"{major}.{minor}.{patch}"

    if any(v == new for v, _ in changelog_versions()):
        fail(f"{new} already has a changelog entry")

    # --- Cargo.toml: only the [package] version -----------------------------
    text = open(CARGO, encoding="utf-8").read()
    head, sep, tail = text.partition("[dependencies]")
    head, n = CARGO_VERSION.subn(f'version = "{new}"', head, count=1)
    if n != 1:
        fail("could not rewrite the version in Cargo.toml")
    open(CARGO, "w", encoding="utf-8", newline="\n").write(head + sep + tail)

    # --- CHANGELOG.md: open a section under [Unreleased] --------------------
    text = open(CHANGELOG, encoding="utf-8").read()
    today = datetime.date.today().isoformat()
    section = (
        f"## [{new}] — {today}\n\n"
        f"### Changed\n- {summary}\n\n"
    )
    marker = "## [Unreleased]\n"
    if marker not in text:
        fail("CHANGELOG.md has no '## [Unreleased]' section")
    idx = text.index(marker) + len(marker)
    # Skip whatever currently sits under Unreleased, up to the next heading.
    nxt = text.find("\n## ", idx)
    if nxt == -1:
        fail("CHANGELOG.md has no released section after [Unreleased]")
    text = text[:idx] + "\nNothing yet.\n\n" + section + text[nxt + 1 :]

    # Refresh the comparison links at the foot.
    prev = changelog_versions()[0][0] if changelog_versions() else None
    repo = "https://github.com/andreaswiren/supertile"
    text = re.sub(
        r"^\[Unreleased\]:.*$",
        f"[Unreleased]: {repo}/compare/v{new}...HEAD",
        text,
        count=1,
        flags=re.M,
    )
    if prev:
        link = f"[{new}]: {repo}/compare/v{prev}...v{new}"
        text = text.replace(
            f"[Unreleased]: {repo}/compare/v{new}...HEAD",
            f"[Unreleased]: {repo}/compare/v{new}...HEAD\n{link}",
            1,
        )
    open(CHANGELOG, "w", encoding="utf-8", newline="\n").write(text)

    print(f"bumped to {new}")
    print("  Cargo.toml and CHANGELOG.md updated — edit the entry, then:")
    print(f'  git commit -am "Release {new}: {summary}" && git tag -a v{new} -m "v{new}"')
    return 0


def main():
    args = sys.argv[1:]
    if not args or args[0] in ("-h", "--help"):
        print(__doc__)
        return 0
    if args[0] == "--check":
        return check()
    if args[0] == "--show":
        print(cargo_version())
        return 0
    if args[0] in ("major", "minor", "patch"):
        if len(args) < 2:
            fail("a one-line summary is required: version.py patch \"Summary\"")
        return bump(args[0], " ".join(args[1:]))
    fail(f"unknown argument {args[0]!r}")


if __name__ == "__main__":
    sys.exit(main())

# Releasing SuperTile

SuperTile follows [Semantic Versioning](https://semver.org/). **Every change
gets a version bump and a changelog entry** — not just the notable ones.

## Why the process is enforced rather than documented

Four things have to agree: the version in `Cargo.toml`, the top heading in
`CHANGELOG.md`, the version compiled into the binary, and the git tag. If any
one drifts, "which build am I running, and what is in it?" stops having an
answer — and the [CRA documentation](docs/compliance/EU-CRA.md) claims each
release *is* identified and changelogged, so drift is a compliance inaccuracy,
not merely untidy.

`scripts/version.py --check` verifies the agreement and runs in CI. It is not a
convention anyone has to remember.

## Choosing the number

Pre-1.0, SemVer allows the minor number to carry breaking changes, and that is
what this project does:

| Bump | When | Example |
|---|---|---|
| **patch** `0.8.1 → 0.8.2` | Bug fix, performance work, docs, refactor with no behaviour change | The drag-resize fix |
| **minor** `0.8.2 → 0.9.0` | New feature, or a breaking change while pre-1.0 | Focus dimming; the TOML → JSON move |
| **major** `0.9.0 → 1.0.0` | Breaking change once 1.0 has shipped | — |

A change to the **configuration file format** is always at least a minor bump,
because a user's file has to survive it.

## Making a release

```bash
# 1. Bump the version and open a changelog section.
python scripts/version.py patch "Fix the taskbar dimming at the wrong level"

# 2. Write the real entry in CHANGELOG.md.
#    Group under Added / Changed / Fixed / Removed / Performance / Security.
#    Say what changed and why it mattered, not just which file moved.

# 3. Verify exactly what CI verifies.
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
python scripts/check-unsafe.py
python scripts/version.py --check

# 4. Regenerate the SBOM if dependencies changed. It embeds the version, so
#    this is required for any release that touches Cargo.lock.
python scripts/make-sbom.py

# 5. Commit, tag, push.
git commit -am "Release 0.8.3: fix taskbar dimming level"
git tag -a v0.8.3 -m "v0.8.3"
git push && git push --tags

# 6. Build and publish.
cargo build --release
Get-FileHash target/release/supertile.exe -Algorithm SHA256
```

Attach to the GitHub release:

- `supertile.exe`
- `SHA256SUMS.txt`
- `sbom.cdx.json` (the copy from `docs/compliance/`, which is the one embedded
  in the binary)

## Build identity

Every binary carries more than its version:

```
Build 0.8.2 (a1b2c3d4e5f6, release)
```

Shown in **Tray → About & SBOM**, and written to the first line of the log when
diagnostics are on. A build from an uncommitted tree is marked `-dirty`, so a
bug report against a local build is never mistaken for one against the tagged
release. Ask for this string in bug reports; the version alone does not identify
a commit.

## Security releases

A release fixing a vulnerability additionally:

1. Carries a **[Security]** section in the changelog, with the CVE if assigned.
2. Ships as a **patch** bump wherever possible, so applying it never forces
   unrelated change on the user.
3. Is accompanied by a GitHub Security Advisory.
4. Follows the timelines in [SECURITY.md](SECURITY.md) — 30 days for critical.

For an actively exploited vulnerability, the CRA Article 14 reporting clock
(24 h / 72 h / 14 days) starts when the manufacturer becomes aware, not at
release. See [docs/compliance/EU-CRA.md](docs/compliance/EU-CRA.md) §4.

## Version history

`v0.1.0` through `v0.8.1` were tagged retroactively onto the commits they
correspond to, when this process was introduced. They were development
milestones on `main` rather than published releases; no artefacts were ever
distributed under those numbers. Everything from `v0.8.2` onwards follows the
process above.

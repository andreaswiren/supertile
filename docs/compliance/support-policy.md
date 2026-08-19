# Support policy

Required by **CRA Article 13(8)**, which obliges a manufacturer to state, and
then honour, how long a product will receive security updates.

## The commitment

**Five years from the date of each release.**

Five years is chosen because it is credible to honour rather than impressive to
publish. SuperTile is a single self-contained executable with three runtime
dependencies and no server side; there is no infrastructure to keep alive and
no protocol to keep compatible. A longer promise on a hobby-scale project would
be the kind of commitment that quietly lapses.

The period is machine-readable in the SBOM as `cra:supportPeriodYears` and
`cra:supportPeriodEnd`.

## What a security update means

A patch release that fixes a vulnerability in SuperTile itself or in a
dependency it links. Security updates are:

- announced in [CHANGELOG.md](../../CHANGELOG.md) under **[Security]**,
- published as a GitHub Release with SHA-256 checksums,
- accompanied by a GitHub Security Advisory carrying a CVE where one is
  assigned.

Security updates are shipped separately from feature work, so applying one
never forces the user to take unrelated change.

## Supported versions

| Version | Status | Security updates until |
|---|---|---|
| 0.28.x | current pre-release | superseded by 0.29.x, or 2031-08-18 |
| < 0.28 | superseded | upgrade to the current release |

Only the latest patch release of a supported minor line receives fixes.
Backports to older patch releases are not provided; upgrading within a minor
line is by design a low-risk operation.

## Response targets

See [SECURITY.md](../../SECURITY.md). In summary: acknowledgement within 72
hours, triage within 7 days, critical fixes within 30 days.

## End of support

When a version leaves support it is marked clearly in this file and in the
changelog. Users are given at least 90 days' notice, and the release page for
the unsupported version says so plainly rather than leaving it to be inferred.

## If the project is abandoned

Honesty is worth more than an unenforceable promise. If SuperTile stops being
maintained, this file and the repository README will say so explicitly, and the
final release will be marked as end-of-life. The MIT licence permits anyone to
fork and continue maintenance, and the SBOM plus the documented threat model
are there to make that practical for whoever does.

# Contributing to SuperTile

## Acknowledgements

**Johan Bogg** — with thanks for contributing tremendously to this project.

---

## Before you start

SuperTile is a Windows 11 tiling window manager written in Rust against the
Win32 API directly. There is no UI framework: windows are registered classes
with their own message loops, and everything is drawn with GDI. If that is
unfamiliar, [`src/ui/about.rs`](src/ui/about.rs) is the smallest complete
example of the pattern the other windows follow.

Read [`docs/architecture.md`](docs/architecture.md) first. It explains why the
program is shaped the way it is, which is usually the question a change runs
into.

## Building

```powershell
cargo build
cargo test
```

Everything below must pass before a change is ready. CI runs the same set, so
a red build here is a red build there.

```powershell
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
python scripts/check-unsafe.py
python scripts/version.py --check
python scripts/make-sbom.py --check
```

## House rules

These are not style preferences; each one exists because its absence caused a
specific problem.

**Every `unsafe` block carries a `// SAFETY:` comment** stating the invariant it
relies on. `scripts/check-unsafe.py` fails the build otherwise. This codebase is
almost entirely FFI, so `unsafe` is unavoidable — what is avoidable is `unsafe`
nobody has thought about.

**Comments explain why, not what.** The code already says what it does. A
comment earns its place by recording the reasoning that is not recoverable from
reading it: the API that lies, the ordering that matters, the obvious approach
that was tried and did not work.

**Pure logic goes in a pure function with tests.** Geometry, ranking, parsing,
version comparison — none of it needs a desktop, and all of it is where the
bugs are cheapest to catch. Win32 calls should be a thin layer over tested
arithmetic, not mixed into it.

**Test the property, not the example.** Assert that no two zones overlap for
every window count, not that four windows produce four particular rectangles.
Several real faults here were found by a test written that way and missed by
the specific one beside it.

**Every change gets a changelog entry and a version bump.** See
[`RELEASING.md`](RELEASING.md). `scripts/version.py --check` enforces that the
manifest, the changelog and the tag agree.

## What is hard about this codebase

Worth knowing before you spend an evening on it:

- **Windows lie.** `WM_GETMINMAXINFO` reports a minimum that many applications
  do not honour. Chromium and GTK windows do not move while their own border is
  dragged. `PROCESS_QUERY_LIMITED_INFORMATION` succeeds across a privilege
  boundary that blocks everything else. Where the API and observed behaviour
  disagree, trust the observation and write down which it was.

- **The debug log is the fastest tool here.** Tray → Settings → Debug logging →
  extensive. On any window-behaviour problem it has settled in one reading what
  hours of reasoning got wrong. Read the *sequence* of values, not a single
  line: one observation cannot tell a misplaced window from a moving one.

- **Reentrancy is real.** `TrackPopupMenuEx` and `ChooseColorW` run their own
  message loops, so a `RefCell` borrow held across one will panic. Copy what you
  need out first.

## Reporting a problem

Tray → Settings → **Create issue report** assembles the machine's shape as
Markdown: OS build, displays and scaling, the windows on screen and their
minimum sizes. Window titles, directory paths and your user name are left out.
Read it before posting — anonymisation is best-effort, not a guarantee.

For anything security-relevant, follow [`SECURITY.md`](SECURITY.md) instead of
opening a public issue.

## Licence

Contributions are accepted under the project's licence — see
[`LICENSE`](LICENSE).

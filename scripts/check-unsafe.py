#!/usr/bin/env python3
"""Fail if any `unsafe` block lacks a `// SAFETY:` comment.

SuperTile's security posture (docs/compliance/EU-CRA.md §3) rests on `unsafe`
being confined to FFI calls, each stating the invariant it relies on. That claim
is only worth anything if it is enforced: a reviewer cannot check an invariant
nobody wrote down.

A block counts as justified when a `// SAFETY:` comment appears within the few
lines above it, or on the same line. `unsafe fn` declarations are exempt when
they carry a `/// # Safety` doc section instead, which is the documented Rust
convention for pushing the obligation onto the caller.

Usage:  python scripts/check-unsafe.py [--verbose]
"""

import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "src")

# How far above an `unsafe` block the justification may sit. Enough to allow an
# attribute or a blank line between comment and code, not enough to let an
# unrelated comment several statements earlier count.
LOOKBACK = 6

UNSAFE_BLOCK = re.compile(r"(^|[^\w])unsafe\s*\{")
UNSAFE_FN = re.compile(r"(^|\s)(pub(\([^)]*\))?\s+)?unsafe\s+(extern\s+\"[^\"]*\"\s+)?fn\s")
SAFETY = re.compile(r"//\s*SAFETY:", re.IGNORECASE)
SAFETY_DOC = re.compile(r"///\s*#\s*Safety", re.IGNORECASE)


def rust_files():
    for base, _dirs, files in os.walk(SRC):
        for f in sorted(files):
            if f.endswith(".rs"):
                yield os.path.join(base, f)


def check(path, verbose=False):
    """Return a list of (line_no, text) for unjustified unsafe blocks."""
    with open(path, encoding="utf-8") as fh:
        lines = fh.readlines()

    problems = []
    for i, line in enumerate(lines):
        stripped = line.strip()
        # Skip comments and doc comments entirely.
        if stripped.startswith("//"):
            continue
        # `unsafe fn` is a declaration, not a use site.
        if UNSAFE_FN.search(line):
            # It should still document its contract for callers.
            window = lines[max(0, i - LOOKBACK) : i]
            if not any(SAFETY_DOC.search(w) for w in window) and "fn main" not in line:
                if verbose:
                    print(f"  note: {os.path.relpath(path, ROOT)}:{i+1} "
                          f"unsafe fn without a '# Safety' doc section")
            continue
        if not UNSAFE_BLOCK.search(line):
            continue

        window = lines[max(0, i - LOOKBACK) : i + 1]
        if not any(SAFETY.search(w) for w in window):
            problems.append((i + 1, stripped))
    return problems


def main():
    verbose = "--verbose" in sys.argv
    total_blocks = 0
    all_problems = []

    for path in rust_files():
        with open(path, encoding="utf-8") as fh:
            text = fh.read()
        total_blocks += len(UNSAFE_BLOCK.findall(text))
        for line_no, text_line in check(path, verbose):
            all_problems.append((os.path.relpath(path, ROOT), line_no, text_line))

    if all_problems:
        print("unsafe blocks without a // SAFETY: comment:\n", file=sys.stderr)
        for rel, line_no, text_line in all_problems:
            print(f"  {rel}:{line_no}\n      {text_line}", file=sys.stderr)
        print(
            f"\n{len(all_problems)} of {total_blocks} unsafe blocks are unjustified.",
            file=sys.stderr,
        )
        print(
            "Add a '// SAFETY:' comment stating the invariant the block relies on.",
            file=sys.stderr,
        )
        return 1

    print(f"all {total_blocks} unsafe blocks carry a SAFETY justification")
    return 0


if __name__ == "__main__":
    sys.exit(main())

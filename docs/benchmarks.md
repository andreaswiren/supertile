# Measured performance

Cited by the framework evaluation in the [README](../README.md). Every figure
for SuperTile here is **measured on this build**, not estimated. Where a number
is an estimate or comes from elsewhere, it says so.

## Test machine

| | |
|---|---|
| CPU | 32 logical cores |
| Display | 5120 × 2160 virtual desktop |
| OS | Windows 11 Pro, build 26200 |
| Build | `cargo build --release` — fat LTO, one codegen unit, panic=abort, symbols stripped |
| Toolchain | rustc 1.97.1, `x86_64-pc-windows-msvc` |
| Desktop state | ~20 tiled windows, 223 Start Menu entries |

**Caveat worth stating up front:** this machine was not idle during measurement.
An active development session generates window events continuously, and
SuperTile is event-driven, so its idle cost tracks how busy the desktop is. The
numbers below are therefore an upper bound for a quiet machine, not a
best case.

## Footprint

| Metric | Measured |
|---|---|
| Binary on disk | **732 KB** (749,568 bytes) |
| Private working set | **1.93 MB** |
| Total working set | 13.6 MB *(includes shared user32/gdi32 pages every Win32 app maps)* |
| Threads | 2 at rest (4 briefly, during the startup Start Menu scan) |
| Handles | 141 |

Private working set is the honest "what does this program cost" number; total
working set counts DLL pages shared with every other Win32 process.

## Startup

| Metric | Measured |
|---|---|
| Cold start to message loop running | **31–74 ms**, median **46 ms** |
| CPU consumed in the first 5 s | **94 ms** |

The first five seconds include scanning 223 Start Menu shortcuts on a worker
thread and performing the initial tile. The start-to-ready figure includes
`Start-Process` overhead from the measuring harness, so the true figure is
lower; it is quoted as measured rather than adjusted downwards.

## Idle cost

Measured over a 30-second window, 5 seconds after launch, tiling active with
auto-tile on.

| Build stage | CPU per 30 s | % of one core |
|---|---|---|
| Before optimisation | 469 ms | 1.56% |
| **After optimisation** | **172 ms** | **0.57%** |

With tiling paused, idle CPU is **0 ms** — the WinEvent hooks themselves cost
nothing measurable.

Memory drift over the same window is **negative** (−57 KB): allocations from
retiling are returned rather than accumulating.

### What the optimisation was

Profiling an early build showed 2.19% of a core at idle, which is not what a
resident tray utility should cost. Three causes, all addressed:

1. **One over-broad WinEvent hook.** A single range from
   `EVENT_SYSTEM_FOREGROUND` to `EVENT_OBJECT_HIDE` also delivers menu, scroll,
   alert, capture and drag-drop events from every process on the desktop —
   filtered out and discarded, but only after waking this process. Replaced by
   five tight ranges.
2. **`OpenProcess` per window per enumeration.** Reading the owning
   executable's path is the most expensive thing a retile does, and a window's
   executable cannot change. Now cached per `HWND`.
3. **One full desktop enumeration per monitor.** `retile_all` walked every
   top-level window once for each display. Now once in total, with each
   window's monitor resolved during that single pass.

Windows already in the right place are also skipped rather than issued a
redundant `SetWindowPos`.

## Layout engine

Pure computation, no Win32 involved. From the test suite, which exercises the
engine across six layouts, 1–24 windows and six work-area sizes including
negative origins:

| Operation | Cost |
|---|---|
| Compute zones for 24 windows | microseconds; not separately measurable against test overhead |
| Full test suite (323 tests) | **0.53 s** |

The engine is exact by construction: zone edges are shared boundaries, so a
tiled monitor has no seams or overlaps at any resolution. This is asserted, not
assumed — see `tiling_is_exact_and_gapless`.

## Comparison figures

The other stacks in the README's framework table were **not** benchmarked here.
Building four equivalent tray applications was out of scope. Those figures are
representative values for minimal tray applications on Windows, drawn from:

- **.NET NativeAOT** — Microsoft's published NativeAOT size and startup
  guidance for console and desktop apps.
- **.NET JIT** — a shared-runtime WinForms/WPF tray app.
- **Go** — a `syscall`-based tray application; binary size and GC behaviour
  from the Go runtime's documented characteristics.
- **C++ / Win32** — a minimal Win32 tray application built with MSVC.
- **Python + PyWin32** — a PyInstaller one-folder bundle.

They are used to justify a decision that has already been made, and are
labelled as estimates in the README. Only the Rust column is measured. Anyone
wanting a rigorous comparison should build the equivalents and measure them;
the conclusion this project rests on — that a resident, mostly-idle process is
dominated by runtime overhead rather than throughput — does not depend on the
precise values.

## Reproducing

```powershell
# Footprint and startup
$sw = [Diagnostics.Stopwatch]::StartNew()
$p  = Start-Process .\target\release\supertile.exe -PassThru
Start-Sleep -Seconds 5
$p.Refresh()
"private: {0:N2} MB  threads: {1}" -f ($p.PrivateMemorySize64/1MB), $p.Threads.Count

# Idle CPU over 30 s
$c0 = $p.TotalProcessorTime.TotalMilliseconds
Start-Sleep -Seconds 30
$p.Refresh()
"idle: {0:N0} ms = {1:N3}% of one core" -f
  ($p.TotalProcessorTime.TotalMilliseconds - $c0),
  (($p.TotalProcessorTime.TotalMilliseconds - $c0) / 30000 * 100)
```

Last measured: 2026-08-18, commit on `main` at the time of writing.

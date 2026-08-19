# Architecture

A map of the codebase and the handful of decisions that shape it. Referenced
from the [README](../README.md).

## Shape

One process, one thread, one message loop. Everything below hangs off that.

```
main.rs            single-instance mutex, PerMonitorV2, then app::run()
  app.rs           the host window, message dispatch, tiling orchestration
    config.rs      config.json: untrusted input, clamped and never overwritten
    hotkeys.rs     i3-derived binding table, parsing, fallback chains
    layout.rs      pure geometry: six layouts, gaps, dragged splits
    drag.rs        pure geometry: which edge moved, which boundary it owns
    window.rs      discovery, tileability classification, placement
    monitor.rs     displays, work areas, per-monitor DPI, arrangement identity
    memory.rs      per-application geometry, bounded and atomically written
    dimmer.rs      focus dimming: content and shell overlays
    autostart.rs   one HKCU Run value
    applist.rs     Start Menu scan on a worker thread
    tray.rs        icon, context menu, runtime-rendered glyphs
    sbom.rs        the embedded CycloneDX document
    ui/
      theme.rs     colours, fonts, DPI, GDI double buffer
      palette.rs   the command palette
      keys.rs      shortcut list and editor
      about.rs     About & SBOM
      highlight.rs click-through outline / drag preview
      hover.rs     tray hover state, in thread-local storage
      tip.rs       tray menu tooltip
```

## Decisions that shape everything else

### One thread

Win32 window state belongs to the thread that created it, and a tiling manager
touches window state constantly. A single UI thread removes an entire category
of bug rather than defending against it. The only background thread is the
Start Menu scan, which owns no window handles and communicates by posting one
message when it finishes.

### Pure logic separated from Win32

`layout.rs`, `drag.rs`, `fuzzy.rs` and most of `config.rs` contain no Win32
types. That is why the tiling arithmetic, the drag-to-boundary mapping and the
tileability rules have real test coverage: they can be exercised without a
desktop. `window.rs` deliberately splits `classify` — a pure function over a
`Candidate` snapshot — from the Win32 calls that produce the snapshot.

### Events, not polling

Window changes arrive through `SetWinEventHook`, in five narrow ranges, and are
coalesced through a debounce timer. `EVENT_OBJECT_LOCATIONCHANGE` is excluded
everywhere: it fires continuously during any drag or animation. Live drag
feedback is sampled on a timer instead, and only while a drag is in progress.

### Reentrancy is the hard part

`TrackPopupMenuEx` runs its own modal message loop, and `ShowWindow` and
`SetWindowPos` dispatch messages synchronously. A window procedure reached from
inside one of those, while `App::show_tray_menu` still holds `&mut self`, would
alias that borrow — undefined behaviour, and the classic way a Rust Win32
application acquires a crash nobody can reproduce.

Two mechanisms handle it:

- **`App` is reached once, from the top.** `WM_MENUSELECT` is handled *before*
  the window procedure touches `App` at all.
- **State needed during a modal loop lives elsewhere.** `ui::hover` owns the
  highlight overlay and the tooltip in thread-local storage that `App` does not
  reference, so nothing is borrowed twice.

The satellite windows (palette, keys, about, highlight, tip) each own their
state behind a `RefCell` and use `try_borrow_mut`, dropping a reentrant message
rather than aliasing. Dropping a repaint is harmless; aliasing is not.

### GDI, not Direct2D

A D3D device costs roughly 30 MB resident and ~80 ms to create. For windows
that are visible a few seconds a day, on a process whose whole claim is that it
costs nothing at rest, that is the wrong trade. GDI double-buffering plus the
DWM system backdrop gets the Windows 11 look — rounded corners, acrylic — with
neither cost.

### Exact tiling by construction

Zone edges are computed as shared boundaries (`start + i * span / count`), never
by accumulating per-zone widths. Adjacent zones therefore agree on the pixel
where they meet, so a tiled monitor has no 1px seams and no 1px overlaps at any
resolution. Dragged splits preserve the property because they only move a shared
boundary.

## Data

Two files under `%LOCALAPPDATA%\SuperTile\`, both written atomically
(temp file, then `MoveFileEx` with replace) so a crash mid-write cannot truncate
them:

| File | Contents | On corruption |
|---|---|---|
| `config.json` | Settings and keybindings | Defaults used, file left untouched |
| `geometry.json` | Per-app zone memory, bounded and LRU-evicted | Discarded; positions are a cache |

Optional `supertile.log`, off by default, truncated at each start.

## Testing

323 tests, all in-tree.

- **Pure logic** — layout geometry, drag mapping, fuzzy matching, config
  parsing, hotkey parsing, geometry memory. Property-style where it matters:
  the tiling is asserted to be exact across six layouts, 1–24 windows and six
  work areas.
- **Live desktop** — monitor enumeration, window classification, overlay
  creation, shell surface discovery. These assert invariants that hold on any
  Windows machine, so they run in CI. Running the classifier against a real
  desktop is how the WinForms parking-window bug was found.

CI additionally enforces `clippy -D warnings`, that every `unsafe` block
carries a `// SAFETY:` comment, that the committed SBOM matches `Cargo.lock`,
and that the `.ico` still matches its SVG source.

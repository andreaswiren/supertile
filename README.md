# SuperTile

**Fullscreen autotiling window manager, focus dimmer and command palette for
Windows 11.**

SuperTile keeps every window on a monitor packed into a gap-free layout,
remembers where each application likes to live, and gets out of the way. It
sits in the system tray, starts in ~46 ms, and costs 1.9 MB and 0.57% of one
core while running.

> **Status: pre-release (0.26.x).** Everything described below is implemented
> and tested — 539 tests. See [CHANGELOG.md](CHANGELOG.md) for what landed and
> [TODO.md](TODO.md) for what is still open.

---

## What it does

| | |
|---|---|
| **Fullscreen autotiling** | Every tileable window on a monitor is packed into a zone automatically, the moment it appears. |
| **Seven layouts** | Split (the default: drop on an edge to divide that cell), Grid, Master + Stack, Columns, Rows, Dwindle, Monocle. |
| **Drag to resize** | Pull a tile edge and the boundary moves; the neighbour gives up exactly that space, live. |
| **Drag to rearrange** | Drag a window over another tile — the destination lights up, release to swap. Drop on an edge to place it beside; hold Shift to detach it from the grid entirely. |
| **i3 keybindings** | The i3 binding set, adapted for Windows 11, with automatic fallback when a shortcut is already taken. |
| **Focus dimming** | Darken everything except the window you are in, with a *separate* level for the taskbar so it stays usable. |
| **Window list** | Every open window in the tray: focus it, exclude it from tiling, or pin it always-on-top. Hovering outlines it on screen. |
| **Geometry memory** | Remembers the zone each executable was last given, per display arrangement, and restores it next launch. |
| **Command palette** | Fuzzy launcher over installed apps, open windows and every SuperTile command. Tab switches to asking Claude a question, opt-in. |
| **Multi-monitor, per-monitor DPI** | Work areas, gaps and zones computed per monitor at that monitor's DPI. |
| **Virtual desktops** | Each desktop keeps its own layout on each monitor; arranging one does not disturb another. |
| **Overlay themes** | Twelve of them, plus an editor with a live preview, for the outlines drawn while you drag. |
| **Issue reports** | One click assembles your machine's shape as Markdown, with window titles, paths and your user name left out. |

### What it is for

**Screen space, not keyboard navigation.** A large monitor spends most of its
day partly empty — windows overlapping, a strip of wallpaper down one side, one
application maximised over four others that are still open. SuperTile exists to
stop that happening: every window that is open gets a share of the glass, kept
gapless and kept current as windows come and go.

The other half of it is the daily tax of arranging them by hand. Dragging a
window to a half, nudging an edge, re-doing it when something opens, re-doing it
all again after a reboot or a dock. SuperTile takes that over, remembers where
each application likes to live, and restores the arrangement next time.

It is **not** a keyboard-first window manager, and it is not trying to be. The
i3 bindings are there because they are a good set and cost nothing to support,
but nothing here assumes you want to leave the mouse alone — dragging boundaries
and dropping windows onto each other are first-class, and most people will
arrange things that way. If you want a keyboard-driven tiling WM in the dwm or
i3 tradition, that is a different tool and you should use one.

### Why this rather than FancyZones

PowerToys FancyZones is a *snapping* tool: you define zones and drag windows
into them. SuperTile is a *tiling window manager* — it owns the work area and
maintains the packing continuously, the way dwm, i3 or yabai do. Opening a
window re-tiles the monitor; you never drag anything into a zone to make tiling
happen.

---

## Framework evaluation

The brief called for the best-performing stack for *this specific workload*: a
process resident 24/7 that does nothing at all most of the time and must
respond to a hotkey inside a frame. That profile punishes runtime overhead and
idle cost far more than it rewards throughput.

| Stack | Binary | Idle RSS | Cold start | Runtime | GC pauses | Win32 access |
|---|---|---|---|---|---|---|
| **Rust + windows-rs** ✅ | **732 KB** † | **1.9 MB** † | **46 ms** † | none | none | complete, typed, zero-cost |
| C# / .NET 10 NativeAOT | ~11 MB | ~15 MB | ~15 ms | none (AOT) | yes | CsWin32 source generators |
| C# / .NET 10 JIT | ~1 MB + runtime | ~30 MB | ~60 ms | shared runtime | yes | P/Invoke |
| Go | ~5 MB | ~10 MB | ~8 ms | none | yes (sub-ms STW) | `syscall`, hand-rolled |
| C++ / Win32 | ~1 MB | ~3 MB | ~2 ms | none | none | native |
| Python + PyWin32 | ~15 MB bundle | ~35 MB | ~250 ms | interpreter | refcount + GC | pywin32 |

**Chosen: Rust + [windows-rs](https://github.com/microsoft/windows-rs).** In
order of weight:

1. **Idle cost dominates.** This process is asleep essentially all the time.
   Rust has no runtime, no JIT, no GC thread and no background finalizer, so an
   idle SuperTile is genuinely idle.
2. **No GC pauses during window motion.** A retile is a batched
   `DeferWindowPos` transaction that must land inside one frame; a collection
   arriving mid-transaction is visible tearing. Rust removes the failure mode
   rather than tuning it.
3. **Memory safety is a regulatory asset.** SuperTile parses untrusted input —
   configuration, shortcut paths, window titles from other processes — and
   passes buffers across the Win32 boundary, which is exactly where
   memory-safety defects concentrate. C++ matches Rust on performance and loses
   here. See [docs/compliance/EU-CRA.md](docs/compliance/EU-CRA.md).
4. **`windows-rs` is first-party**, generated from the same metadata as the
   Windows SDK, so `SetWinEventHook`, `DeferWindowPos`, `Shell_NotifyIconW` and
   per-monitor DPI are all available and correctly typed. Go's story here is
   hand-written `syscall` wrappers, which is where window-manager bugs live.
5. **A small dependency tree is a small SBOM** — three runtime crates.

Python was excluded on cold start alone: a 250 ms palette is not a palette.
C++ was the closest runner-up, rejected on point 3.

> † Measured on this build; see [docs/benchmarks.md](docs/benchmarks.md) for
> method and caveats. The other rows are **not** benchmarked here — they are
> representative values for equivalent minimal tray applications and are
> labelled as estimates. Only the Rust row is measured.

---

## Install

There is no installer. Download `supertile.exe` from
[Releases](https://github.com/andreaswiren/supertile/releases) and put it
somewhere it will not be deleted by accident — the suggested home is a folder of
its own under Program Files:

```
C:\Program Files\SuperTile\supertile.exe
```

Creating that folder needs an administrator prompt, because Program Files is
protected. That is the only elevation involved anywhere: **SuperTile itself runs
unelevated**, and deliberately so — see
[Windows that run as administrator](#windows-that-run-as-administrator). If you
would rather avoid the prompt entirely, anywhere writable works just as well;
`%LOCALAPPDATA%\Programs\SuperTile\` is the usual choice.

Run the executable and it appears in the system tray. Nothing else is required.

> Releases are **not yet Authenticode-signed**, so SmartScreen will warn on
> first run. Verify the SHA-256 against the release page. Tracked in
> [TODO.md](TODO.md) as a 1.0 blocker.

### What it writes

Two things, both under your own user account:

| What | Where | When |
|---|---|---|
| Autostart | one `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value | only if you turn on **Start with Windows** |
| Settings, geometry memory, saved splits, log | `%LOCALAPPDATA%\SuperTile\` | on first run |

SuperTile never writes to `HKLM`, never touches the Program Files folder it runs
from, and never installs a driver, service or scheduled task. Uninstalling is
deleting the executable, the `%LOCALAPPDATA%\SuperTile` folder, and the Run
value if you created one.

### Build from source

```bash
git clone https://github.com/andreaswiren/supertile.git
cd supertile
cargo build --release
```

Needs Rust 1.85+ with `x86_64-pc-windows-msvc` and the Visual Studio Build
Tools. Verify the way CI does:

```bash
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
```

---

## Keyboard shortcuts

Bindings follow **i3**, with `$mod` mapped to <kbd>Win</kbd>+<kbd>Alt</kbd>.

Windows 11 reserves most `Win`+*key* combinations for the shell — `Win+D`,
`Win+E`, `Win+L`, `Win+R`, `Win+S`, `Win+X`, `Win+1`…`9` and `Win`+arrows are
claimed before any application sees them — so i3's usual Super-key `$mod` is
not available. Everything *after* `$mod` matches i3.

| Action | SuperTile | i3 |
|---|---|---|
| Command palette | <kbd>Win</kbd>+<kbd>Alt</kbd>+<kbd>D</kbd> | `$mod+d` |
| Terminal | <kbd>Win</kbd>+<kbd>Alt</kbd>+<kbd>Enter</kbd> | `$mod+Return` |
| Close window | <kbd>Win</kbd>+<kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>Q</kbd> | `$mod+Shift+q` |
| Focus left / down / up / right | <kbd>Win</kbd>+<kbd>Alt</kbd>+<kbd>H</kbd>/<kbd>J</kbd>/<kbd>K</kbd>/<kbd>L</kbd> | `$mod+h/j/k/l` |
| Move window | add <kbd>Shift</kbd> | `$mod+Shift+…` |
| Columns / Rows | <kbd>Win</kbd>+<kbd>Alt</kbd>+<kbd>B</kbd> / <kbd>V</kbd> | `$mod+b` / `$mod+v` |
| Monocle | <kbd>Win</kbd>+<kbd>Alt</kbd>+<kbd>S</kbd> or <kbd>W</kbd> | `$mod+s` / `$mod+w` |
| Cycle layout | <kbd>Win</kbd>+<kbd>Alt</kbd>+<kbd>E</kbd> | `$mod+e` |
| Fullscreen | <kbd>Win</kbd>+<kbd>Alt</kbd>+<kbd>F</kbd> | `$mod+f` |
| Float | <kbd>Win</kbd>+<kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>Space</kbd> | `$mod+Shift+space` |
| Grow / shrink master | <kbd>Win</kbd>+<kbd>Alt</kbd>+<kbd>=</kbd> / <kbd>-</kbd> | `$mod+r` then `l`/`h` |
| Increase / decrease gaps | <kbd>Win</kbd>+<kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>=</kbd> / <kbd>-</kbd> | — |
| Retile | <kbd>Win</kbd>+<kbd>Alt</kbd>+<kbd>T</kbd> | — |
| Pause tiling | <kbd>Win</kbd>+<kbd>Alt</kbd>+<kbd>P</kbd> | — |
| Toggle dimming | <kbd>Win</kbd>+<kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>D</kbd> | — |
| Reload config | <kbd>Win</kbd>+<kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd> | `$mod+Shift+c` |
| Exit | <kbd>Win</kbd>+<kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>E</kbd> | `$mod+Shift+e` |

### When a shortcut is already taken

Windows offers no way to ask *who* owns a hotkey — the only test is to try to
claim it. Each action therefore carries a fallback chain, and SuperTile walks it
until one is accepted, then writes the working key back to the config.

On a machine running PowerToys, 7 of the 24 first choices are typically already
taken and resolve silently to their <kbd>Ctrl</kbd>+<kbd>Alt</kbd> fallback.
The tray menu says how many moved and which; hovering any menu item shows the
key actually in use.

Change any of them in **Tray → Settings → Keyboard shortcuts**: click a row,
press the new combination.

---

## Tray menu

- **Open command palette**
- **Retile now**
- **Layout ▸** — the six layouts, each showing its shortcut
- **Resize ▸** — gaps and master-pane fraction
- **Dimming ▸** — enable, auto-track, select window, and separate levels for
  windows and for the taskbar
- **Windows (*n*) ▸** — every open window: bring to front, exclude from tiling,
  always on top. Hovering outlines the real window on screen.
- **Settings ▸** — Start with Windows, Auto-tile, Pause, Keyboard shortcuts,
  Edit config.json
- **About & SBOM…** — version, licence, the CycloneDX component list and the
  EU CRA documentation
- **Exit**

Every item shows its shortcut in the accelerator column, and a tooltip on hover
giving the key in use, whether it is a fallback, and the i3 binding it came
from.

---

## The command palette

`Win+Alt+D` (or `Ctrl+Alt+D` where the shell has claimed that). Type to search
applications, open windows and SuperTile's own commands.

Press **Tab** to switch to asking Claude: everything you type is then a
question, and Enter opens `claude.ai/new` in your browser with it already typed
into the composer. **It is not sent** — you read it and press Enter yourself.
**Esc** steps back to searching.

Off by default, because the question travels in a URL and therefore leaves the
machine. Turn it on with `palette.claude_desktop` in the config. Nothing needs
to be installed; the desktop client cannot pre-fill a chat, so the web app is
used instead.

## Dragging

| Gesture | What it does |
|---|---|
| Drag a window onto another | Swap them |
| Drag onto the edge of another | Place it left, right, above or below |
| **Ctrl** + drop on an edge | Split that cell in two (adopts the split layout) |
| **Shift** + drag | Detach the window from the grid; it stays where you drop it |
| **Shift** + maximise | True fullscreen — the whole monitor, over the taskbar |
| Drag a boundary | Resize the cells that share it |

Shift matches FancyZones deliberately. Dragging a detached window again without
Shift returns it to the grid — the same gesture in both directions — and the
tray window list offers the same thing under **Windows**, which is the route to
reach for when a fullscreen window is awkward to grab.

## Focus dimming

Darkens every window except the one in focus — for gaming in a window, or
reading one document on a busy desktop.

The taskbar and Start menu get their **own** dim level, defaulting to 40%
against 85% for windows, because a taskbar dimmed to 85% is a taskbar you
cannot use. Both overlays are click-through, so tray icons stay clickable
straight through the dim.

**Auto-track** follows focus; **Select window** pins one window bright so a
windowed game stays lit while you click elsewhere.

---

## Configuration

`%LOCALAPPDATA%\SuperTile\config.json`, created on first run. Reload with
**Win+Alt+Shift+C** — no restart.

JSON rather than TOML because a keybinding is structured data: the key, the i3
binding it derives from, a note explaining any deviation, and its fallback
chain. JSON has no comments, so the explanations are fields — `_readme` at the
top and `i3`/`note` per binding — regenerated on every save so they cannot go
stale.

```jsonc
{
  "general":  { "start_with_windows": false, "paused": false, "auto_tile": true },
  "layout":   { "kind": "grid", "outer_gap": 8, "inner_gap": 8,
                "master_fraction": 0.55, "master_count": 1 },
  "dimming":  { "enabled": false, "window_level": 85,
                "taskbar_level": 40, "auto_track": true },
  "appearance": { "theme": "auto", "palette_max_rows": 9 },
  "memory":   { "enabled": true, "max_entries": 500 },
  "diagnostics": { "logging": false },
  "keybindings": {
    "bindings": [
      { "action": "focus_left", "keys": "Win+Alt+H", "i3": "$mod+h",
        "fallbacks": ["Ctrl+Alt+H", "Win+Alt+Left"] }
    ]
  },
  "rules": [
    { "exe": "mstsc.exe", "action": "float" },
    { "title_contains": "Picture-in-Picture", "action": "ignore" }
  ]
}
```

A malformed file never prevents startup: SuperTile falls back to defaults,
reports a warning, and **leaves your file untouched** rather than overwriting
it. Numeric fields are clamped with a warning per adjustment.

---

## Windows that run as administrator

Windows enforces User Interface Privilege Isolation: a program running at normal
privilege cannot reposition a window owned by an elevated one. This is a
security boundary, not an oversight, and it is why an elevated Task Manager or
an admin console will not tile.

**SuperTile stays unelevated by design.** Running a window manager with full
rights over your session, permanently, so that the occasional admin window can
be tiled is not a trade worth making — it would give a program that talks to
every window on the desktop far more authority than the job needs.

So SuperTile leaves those windows out of the layout altogether. Reserving a cell
for a window that can never be filled would make the rest tile around a hole
while the elevated window floats over them anyway; excluding it gives that space
to windows that can use it, and the admin window stays wherever its own
application put it — on top, which is usually where it wants to be.

They are named in the log and marked in an issue report, so "why is this one not
tiling" always has an answer. A couple of admin windows floating free is the
honest cost of not holding privileges the program does not need.

## The configuration file

`%LOCALAPPDATA%\SuperTile\config.json`. Edited by hand or from the tray; the
tray picks up hand edits with **Reload configuration**.

Every save rotates the previous ten versions into `config.json.1` … `.10`, so a
bad edit can be walked back. The file records the SuperTile version that wrote
it in `_version`. Unknown keys are ignored and missing ones take their defaults,
so a file from an older version simply works — the stamp exists to make a
mismatch visible when diagnosing something, not to refuse the file.

If the file cannot be parsed at all, SuperTile falls back to defaults and leaves
it untouched, so nothing is lost to a syntax error.

## Privacy

SuperTile is entirely local.

- **No network access.** The binary opens no sockets. No telemetry, no update
  check, no crash reporting. `cargo-deny` bans networking crates from the
  dependency tree so this cannot regress silently.
- **Not a keylogger.** It uses `RegisterHotKey`, which delivers only the
  combinations it registered. There is no `SetWindowsHookEx` call anywhere in
  the source.
- **What is stored:** `config.json` and `geometry.json` under
  `%LOCALAPPDATA%\SuperTile\`. Geometry holds executable paths, window class
  names and rectangles — never window contents.
- **Logging is opt-in** and truncated at each start. It records window titles
  and executable paths, which is why it defaults to off.
- **Uninstalling** is deleting the `.exe`; remove `%LOCALAPPDATA%\SuperTile\`
  and the `HKCU\...\Run` value to erase all state.

---

## Compliance

Built to EU Cyber Resilience Act expectations from the first commit, with a
CycloneDX SBOM viewable in-app under **Tray → About & SBOM**.

- [EU CRA conformance notes](docs/compliance/EU-CRA.md) — Annex I mapped clause
  by clause, with stated limitations
- [Threat model](docs/compliance/threat-model.md)
- [Support policy](docs/compliance/support-policy.md) — five years per release
- [CycloneDX SBOM](docs/compliance/sbom.cdx.json)
- [Security policy](SECURITY.md)

SuperTile is free software distributed outside any commercial activity, so per
CRA Article 2(18) it falls outside the Regulation's obligations. The
documentation is a self-assessment, not a Declaration of Conformity.

---

## Documentation

- [CHANGELOG.md](CHANGELOG.md) · [TODO.md](TODO.md)
- [docs/architecture.md](docs/architecture.md) — module map, threading, the
  reentrancy problem and how it is handled
- [docs/benchmarks.md](docs/benchmarks.md) — measured figures and method

## Licence

[MIT](LICENSE) © Andreas Wiren

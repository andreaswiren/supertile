# SuperTile — Plan & Open Work

Working document. Items move to [CHANGELOG.md](CHANGELOG.md) when released.

**Legend:** `[ ]` open · `[~]` in progress · `[x]` done · **P0** blocks release ·
**P1** should ship · **P2** nice to have

---

## Milestone 1 — Foundation ✅

- [x] Evaluate Go / Rust / .NET / Python / C++ for a resident tray workload;
      record the decision and its reasoning in the README
- [x] Cargo project targeting `x86_64-pc-windows-msvc`, pinned toolchain,
      LTO + single-codegen-unit release profile
- [x] Safe `.gitignore` (build output, local state, credential-shaped files)
- [x] Layout engine with six layouts, gaps, neighbour lookup, zone hit-testing
- [x] Fuzzy matcher with word-boundary / camelCase / acronym scoring
- [x] Win32 helper layer: UTF-16 lifetimes, known folders, opt-in logging
- [x] README, CHANGELOG, SECURITY, LICENSE, this file

## Milestone 2 — Core window management ✅

- [x] **Monitor enumeration** — `EnumDisplayMonitors`, per-monitor work area,
      per-monitor DPI (`GetDpiForMonitor`), stable monitor identity across
      hot-plug so geometry memory survives a dock/undock cycle
- [x] **Window discovery** — `EnumWindows` plus the tileability filter:
      visible, non-minimised, owner-less, not `WS_EX_TOOLWINDOW`, **not DWM
      cloaked** (this is what makes UWP ghost windows disappear), non-empty
      title, not one of ours
- [x] **Placement** — batched `BeginDeferWindowPos` / `DeferWindowPos` /
      `EndDeferWindowPos` so a whole monitor retiles in one transaction
- [x] **Frame compensation** — `DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)`
      differs from `GetWindowRect` by the invisible resize border; without
      compensating, tiled windows show gaps that do not match the configured gap
- [x] **Event-driven retile** — `SetWinEventHook` for create / destroy /
      minimise / restore / move-end, coalesced through a short debounce timer
      rather than polling
- [x] **Pause / resume** with the tray icon reflecting state
- [x] **Per-window rules** — float / ignore by executable or title substring

## Milestone 3 — Tray & interaction ✅

- [x] SVG icon set, rasterised to a multi-resolution `.ico` at build time
      (16/20/24/32/40/48/64/256 px for DPI-correct tray rendering)
- [x] `Shell_NotifyIconW` tray icon with `NOTIFYICON_VERSION_4` semantics
- [x] Context menu: Retile · Layout ▸ · **Resize ▸** · Pause · Settings ·
      About & SBOM · Exit
- [x] Taskbar-recreation handling — re-add the icon on the
      `TaskbarCreated` broadcast message, otherwise the icon vanishes when
      Explorer restarts
- [x] Global hotkeys via `RegisterHotKey`, with a clear error path when a
      binding is already owned by another process
- [x] Hotkey string parser and round-trip formatter (`"Win+Alt+Shift+L"`)

## Milestone 4 — Geometry memory ✅

- [x] Key on normalised executable path + window class, scoped to the current
      monitor-set fingerprint
- [x] Store the zone index *and* a fractional fallback rect, so a remembered
      position still makes sense when the layout or resolution changes
- [x] Bounded store (`max_entries`, LRU eviction) — must not grow without limit
- [x] Atomic write (temp file + `ReplaceFileW`) so a crash mid-save cannot
      corrupt the store
- [x] Schema version field with forward-compatible loading

## Milestone 5 — Command palette ✅

- [x] Popup window: `WS_EX_TOOLWINDOW | WS_EX_TOPMOST`, no taskbar button,
      dismiss on focus loss and on <kbd>Esc</kbd>
- [x] GDI double-buffered rendering; DWM rounded corners and system backdrop
      via `DwmSetWindowAttribute` rather than a D3D device
- [x] Text input, caret, selection, history
- [x] Result sources: installed applications + all SuperTile commands
- [x] Start Menu `.lnk` enumeration on a background thread, resolved through
      `IShellLink`, cached with an mtime check
- [x] Match highlighting from `fuzzy::Match::positions`
- [x] Full keyboard control; light/dark following the system theme

## Milestone 6 — Settings & About (mostly done)

- [x] Tray Settings submenu: start with Windows, auto-tile, pause
- [x] Keyboard shortcut list and editor (click a row, press the new keys)
- [ ] **P1** Full settings window for rules, appearance and diagnostics
- [x] Live apply — no restart required
- [ ] **P2** Config hot-reload via `ReadDirectoryChangesW` (reload is a hotkey today)
- [x] **About & SBOM** dialog: version, licence, scrollable CycloneDX component
      list rendered from the embedded SBOM, links to the CRA documentation
- [x] "Start with Windows" toggle writing a single `HKCU\...\Run` value

## Milestone 7 — Compliance ✅

- [x] Generate the CycloneDX 1.6 SBOM with `cargo-cyclonedx`; commit it and
      embed it in the binary so the About dialog and the release always agree
- [x] `docs/compliance/EU-CRA.md` — Annex I Part I (security properties) and
      Part II (vulnerability handling) mapped clause by clause
- [x] `docs/compliance/threat-model.md` — assets, adversaries, mitigations
- [x] `docs/compliance/support-policy.md` — five-year support window
- [x] `.well-known/security.txt` (RFC 9116)
- [x] `cargo-deny` in CI: advisories, licences, banned crates, sources
- [x] `cargo-audit` on a schedule, not just on push, so new advisories against
      unchanged code are still caught

## Milestone 8 — Quality gates (mostly done)

- [x] CI: `fmt --check`, `clippy -D warnings`, `test`, `deny`, release build
- [x] Reject any `unsafe` block lacking a `// SAFETY:` comment (230 blocks, all justified)
- [x] Tests for config round-trip, memory store, hotkey parsing (323 total)
- [x] `docs/benchmarks.md` with **measured** binary size, idle RSS, cold start
      and idle CPU
- [ ] Manual multi-monitor / mixed-DPI / dock-undock test matrix

## Milestone 9 — BSP tree layout (P0, next major piece)

Two requests and one bug all resolve to the same change: replace the
parametric layouts with a real **binary space partition tree**.

**The bug they share.** In Grid today, `Splits.main` holds *column* boundaries
that every row shares. Dragging the boundary between two cells on the bottom
row therefore moves the same boundary on every row above it — reported, and
correct behaviour for a parametric grid, but not what anyone wants. Columns
have to belong to their row, which means a tree, not two flat vectors.

- [ ] **Tree model.** Each node is either a leaf (one window) or a split
      (orientation, ratio, two children). Zones fall out of a recursive walk.
      Replaces `Splits`, and makes every boundary local to its own split by
      construction.
- [ ] **Split on drop.** Dropping a window onto a band of another window
      creates a split there:
      - the middle **60%** of an edge (20% margin top and bottom, or left and
        right) targets that edge
      - dropping on the right band splits vertically, new column to the right
      - dropping on the bottom band splits horizontally, new row below
      - windows above and below the new split keep their full span, so a new
        column in one row does not slice the rows around it
- [ ] **A drop-action indicator, not just a target zone.** Dropping on a
      window should show *what will happen* and let the user choose between
      the options — split vertically (new column), split horizontally (new
      row), or swap. The current overlay highlights a destination, which
      cannot express a choice. Requested explicitly: hovering an edge band
      should preview the split it would create, and the centre should preview
      a swap, before the button is released.
- [ ] **Destroy a split.** Closing or moving out the last leaf collapses the
      parent and its sibling takes the space. Plus an explicit "flatten this
      split" action and a hotkey.
- [ ] **Per-node resize** — the drag machinery already maps an edge to a
      boundary; it needs to map to a tree node instead.
- [ ] Persist the tree per monitor and per display arrangement.
- [ ] Keep the six parametric layouts as presets that *seed* a tree, so
      Grid/Columns/Rows/Master+Stack remain one keystroke away.

## Milestone 10 — Palette sources (P1)

- [ ] **Claude Desktop integration.** Detected via
      `HKCU\SOFTWARE\Classes\claude` and
      `%LOCALAPPDATA%\AnthropicClaude\claude.exe` (both confirmed present on
      the dev machine). A palette entry that opens a new chat, launched with
      `ShellExecuteW` on the `claude://` protocol with the executable as
      fallback. Shown **only when Claude Desktop is installed**, and toggleable
      in Settings.
      - Note: `Start-Process` cannot launch custom URI schemes, but the handler
        is registered and `ShellExecuteW` can. No documented deep link exists
        for pre-filling a prompt, so the entry opens a new chat and nothing
        more, which is what was asked for.
- [ ] Widen the app source toward PowerToys Run parity: UWP/Store apps,
      Control Panel items, `%PATH%` executables, calculator, unit conversion.

## Review loop

Each build iteration runs four independent reviewers, per the brief:

| Reviewer | Focus |
|---|---|
| Critique A | Design, visual language, information hierarchy, tray/palette UX |
| Critique B | Function, correctness, edge cases, whether it actually works |
| Security A | Memory safety, FFI boundaries, `unsafe`, input validation |
| Security B | Supply chain, privilege, persistence, data at rest, privacy |

Findings are triaged here before the next build.

### Open findings

_None yet — first review round runs after Milestone 3._

---

## Deferred / out of scope for 1.0

- [ ] **P2** Virtual desktop awareness (`IVirtualDesktopManager` is unstable
      across Windows builds; needs a version-gated shim)
- [ ] **P2** Drag-to-swap zones with a live overlay
- [ ] **P2** Per-monitor layout persistence
- [ ] **P2** Palette plugins (calculator, unit conversion, file search)
- [ ] **P2** MSIX package and winget manifest
- [ ] **P2** Authenticode signing in the release pipeline — needs a certificate
- [ ] **P2** ARM64 target

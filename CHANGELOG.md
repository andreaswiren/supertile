# Changelog

All notable changes to SuperTile are documented here — and, per project policy,
the ones that are merely routine as well. **Every change gets an entry.**

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
See [RELEASING.md](RELEASING.md) for how versions are assigned and enforced.

Pre-1.0, the minor number carries breaking changes and the patch number carries
fixes, as SemVer permits for `0.y.z`.

Security-relevant entries are marked **[Security]** and carry a CVE where one
has been assigned, per EU CRA Annex I Part II(4). See [SECURITY.md](SECURITY.md)
for the disclosure policy.

## [Unreleased]

Nothing yet.

## [0.28.2] - 2026-08-18

### Fixed

- **The Claude palette toggle still described the desktop client.** Its tooltip
  said the entry "opens Claude Desktop", which stopped being true in 0.28.0 when
  the question started going to claude.ai in a browser. It now says what
  happens: Tab in the palette, and the question opens in your browser already
  typed in, ready for you to send.

- **The same toggle was hidden unless Claude Desktop was installed.** That gate
  was right when the question went to the desktop client and wrong the moment it
  did not — the feature needs nothing installed, so hiding it meant anyone
  without the desktop app could not find a setting that would have worked
  perfectly. It is always offered now, and the user's own choice is the only
  thing that gates it.

- Remaining desktop-era wording in the tray and palette source, left behind by
  the same change.

## [0.28.1] - 2026-08-18

### Added

- A **Prune releases** workflow, run by hand from the Actions tab, that deletes
  every GitHub Release older than a tag you name. Deleting a release takes its
  binary with it and is irreversible from the UI, so the workflow is built to
  make an accident hard rather than to be quick: it reports and stops unless
  `dry_run` is turned off, refuses to proceed unless `confirm` is typed exactly
  as `DELETE`, and orders versions with `sort -V` so 0.9.0 is correctly older
  than 0.10.0 — a lexical sort has that backwards, which is precisely the
  mistake that would delete the wrong release.

- [`CONTRIBUTING.md`](CONTRIBUTING.md), thanking **Johan Bogg** for
  contributing tremendously to this project, and covering what a newcomer needs:
  the gates that must pass, why every `unsafe` block carries a justification,
  and the three things that make this codebase awkward — that Windows APIs
  routinely report what applications do not honour, that the debug log settles
  window-behaviour questions faster than reasoning does, and that modal Win32
  loops will panic on a held `RefCell` borrow.

### Fixed

- SECURITY.md listed 0.1.x as the supported version, twenty-seven releases after
  that stopped being true. It now names the current series and says plainly that
  only the current minor is supported pre-1.0, with the reason: there is no
  installed base on an older series that could not upgrade.

- The same staleness across the compliance set — EU-CRA.md, the support policy
  and the threat model all claimed to cover 0.1.x. All three are reassessed
  against 0.28.x, and the CRA document carries a revision entry recording that
  its network-access claim was overtaken by the update check in 0.25.0 rather
  than silently rewritten.

### Notes

- Git history was collapsed to a single commit at this version, and the tags
  before it removed. The changelog is therefore the only remaining record of how
  the project got here, which is why it has been kept in full rather than
  collapsed with it.

## [0.28.0] - 2026-08-18

### Changed

- **Ask Claude goes to the browser, and the question arrives already typed.**
  `https://claude.ai/new?q=…` fills the composer at claude.ai; the desktop
  client has no equivalent and no deep link that makes one. Nothing needs to be
  installed any more.

  **It is deliberately not sent.** The URL fills the box and stops there, and
  the user presses Enter themselves. Composing is a reasonable thing for a
  launcher to do on somebody's behalf; sending a message is not.

  A question too long for a URL — over 1500 characters — goes to the clipboard
  instead, with a blank chat opened and a notification saying so. The clipboard
  is the fallback rather than the default: overwriting it is a real cost, not
  worth paying when the URL would have carried the text.

- **Ask Claude is now off by default.** The question travels in a URL and
  therefore leaves the machine. That is a decision to be made deliberately
  rather than discovered afterwards. `palette.claude_desktop` turns it on.

### Documentation

- SECURITY.md and the CRA document distinguish the two cases: the update check
  is a connection SuperTile makes, while Ask Claude is a URL handed to the
  browser, whose request belongs to the browser and the user's own session —
  and which is visible in the address bar before anything is sent.

## [0.27.0] - 2026-08-18

### Fixed

- **Asking Claude sent an empty question.** The palette clears its text when it
  hides, and hiding happens *before* the accepted action is posted — so by the
  time anything read the query it was gone. The text typed at the moment Enter
  was pressed is now kept separately.

- **The claim that a new chat opens was not true, and now neither is made.** A
  direct test settled it: launching `claude://new` while Claude Desktop is
  already running brings the existing window forward — same process, same
  window, no new conversation. Only when Claude is not running does it start the
  application, on whatever it was last showing. There is no working deep link
  for a pre-filled chat.

  So the question goes on the clipboard, Claude is brought to the front, and a
  notification says exactly that: paste it with Ctrl+V. Promising a chat that
  never appears is worse than asking for one paste.

### Changed

- **The default overlay theme is thinner**: a one-pixel line and a two-pixel
  corner radius, rather than three and eight. Three pixels and Windows 11's own
  eight-pixel rounding were heavier than the job needs — an overlay appears for
  a second over somebody's work, and one pixel is enough to read a boundary by.
  The five quiet themes follow suit.

### Documentation

- **SECURITY.md and the compliance documents claimed SuperTile makes no network
  connections whatsoever.** That stopped being true in 0.25.0 when the update
  check was added, and a conformance document that overstates its own guarantees
  is worse than one admitting a narrower scope.

  All three now describe what actually exists: one client (WinHTTP, from the
  OS), one host (`api.github.com` over validated TLS), one trigger (off by
  default), one direction (read only, nothing uploaded, nothing downloaded or
  executed). What the request discloses to GitHub is spelled out — source IP,
  timing, and a `User-Agent` naming the program — along with what it does not.
  The verification commands cover both states rather than asserting one.

  The threat model records that its own claim moved rather than editing the
  history away, because a document people rely on should show where it has
  changed.

- Corrected the SBOM schema version: the documents said CycloneDX 1.6, the file
  says 1.5.

## [0.26.1] - 2026-08-18

### Documentation

- README says what SuperTile is *for*: reclaiming screen space and removing the
  daily tax of arranging windows by hand — and, just as plainly, that it is not
  a keyboard-first window manager and is not trying to be. The i3 bindings are
  there because they are a good set and cost nothing, but dragging boundaries
  and dropping windows are first-class and most people will work that way.
  Anyone wanting a keyboard-driven tiler in the dwm tradition is better served
  elsewhere, and should be told so rather than discovering it.

- Install instructions suggest `C:\Program Files\SuperTile\`, and say that
  creating that folder needs an administrator prompt while the program itself
  never does — with `%LOCALAPPDATA%\Programs\SuperTile\` offered for anyone
  who would rather avoid the prompt. A table lists exactly what is written and
  when: one `HKCU` Run value, only if autostart is enabled, and the
  `%LOCALAPPDATA%\SuperTile` folder on first run. Uninstalling is spelled out.

- Corrected several stale claims in the feature table: six layouts had become
  seven, 323 tests had become 539, and the version had been reading 0.1.x for
  twenty-five releases.

## [0.26.0] - 2026-08-18

### Added

- **Tab in the command palette switches to asking Claude.** Everything typed
  after that is a question, and Enter sends it; Escape steps back to searching
  without closing the palette.

  Two modes rather than an entry in the list, because they want opposite things
  from the same keystrokes. Searching wants every character to narrow a list;
  asking wants every character kept verbatim, including the spaces and
  punctuation that make a fuzzy matcher discard everything. Typing a question
  into the old entry meant watching the list empty out and hoping the one thing
  left was still reachable.

  The glyph and the placeholder both change, because hidden state that alters
  what Enter does is how a keystroke ends up somewhere nobody intended. Tab
  previously moved the selection — the arrow keys and Page Up/Down already do
  that, so the switch costs nothing, and where Claude Desktop is not installed
  Tab keeps its old meaning rather than offering a mode that leads nowhere.

## [0.25.0] - 2026-08-18

### Fixed

- **Elevated windows were still being given a cell.** Two faults. The detection
  used `PROCESS_QUERY_LIMITED_INFORMATION`, chosen as the least privilege that
  answered the question — but it answers a different one. That right exists
  precisely so a lower-integrity process *can* ask basic things about a higher
  one, so it succeeds across the boundary and every elevated window was reported
  as ordinary. It now asks for the full query right, which the mandatory
  integrity policy refuses: the same boundary that stops `SetWindowPos` working.

  Separately, the exclusion added in 0.23.2 was no longer present in the tree at
  all — the call site had gone while the function it called remained. It has
  been restored, and an elevated PowerShell should stop reserving a cell it
  immediately jumps out of.

### Changed

- **Split is now the default layout.** The parametric grids reflow every window
  when one arrives or leaves, which is disorienting once there are more than a
  few; a tree divides only the cell you dropped onto and leaves the rest where
  they were. Existing configurations are untouched — this changes what a fresh
  install starts with.

### Added

- **An optional update check.** Off by default, and it should stay a decision
  rather than a default: a check contacts a third party, revealing an IP address
  and the fact that this software is installed. Nobody should discover after the
  fact that their window manager has been phoning home.

  When on, it asks GitHub once a day whether a newer release exists and says so
  once — being told daily about a release you have decided not to install is
  nagging. **Check for updates now** in the tray answers either way, because
  silence after asking is indistinguishable from a broken button. The About
  window carries the same check, and turns into a direct link once a newer
  release is known. Nothing is ever downloaded or installed automatically.

- **The configuration records its schema as well as its author.** `_version` is
  the SuperTile build that wrote the file; `_config_version` is the shape of the
  file itself. They answer different questions and move at different rates —
  most releases change no settings at all — so conflating them would mean
  pretending every release needs a migration. A mismatch in either direction
  warns rather than refuses, and the warning for a *newer* file says plainly
  that unrecognised settings will be dropped on the next save and where the
  backup is.

- **A short list of applications that float by default**, so they are not given
  a cell they cannot occupy. Task Manager is the reported case: it runs
  elevated, so Windows forbids moving it, and the reserved cell showed as a
  black gap with the window floating over it.

  The list is deliberately four entries long — Task Manager, the UAC prompt, the
  on-screen keyboard and Magnifier. A program that merely *might* dislike being
  resized is left off: a window that will not tile is obvious and easily
  excluded from the tray, whereas a window that should have tiled and silently
  did not is a mystery. A `rules` entry still overrides it.

  The check sits after the "is this a real window" tests, not before them. These
  programs own untitled scaffolding like any other, and floating that would put
  invisible entries into the layout.

- **A toggle for whether elevated windows keep a cell**, under Settings. Off by
  default, since a cell reserved for a window Windows forbids us to move can
  never be filled. On, for anyone who would rather the layout hold still while
  an admin console comes and goes.

- **Ten rolling backups of the config**, rotated on every save as
  `config.json.1` through `.10`. The file is edited by hand as well as by the
  tray, and a bad edit was otherwise unrecoverable. A backup that cannot be
  written is ignored rather than failing the save: losing a backup is a
  nuisance, refusing to save your settings is a fault.

- **The config records the version that wrote it**, as `_version`. Not a schema
  number: those have to be remembered and incremented by hand, and the one time
  that matters is the time somebody forgot. The application version is already
  maintained, already meaningful to a reader, and answers the question actually
  being asked. Unknown keys are ignored and missing ones take defaults, so an
  older file simply works — this exists to make a mismatch visible in the log
  and in an issue report, not to gate anything.

## [0.24.2] - 2026-08-18

### Fixed

- **A window detached with Shift could not be put back.** 0.24.0 said a plain
  drag would return it, and that code could never run: `begin_drag` refuses any
  window that is not in the layout, and a detached window is excluded from it by
  definition. No session, no `end_drag`, no way back. Shift was a one-way door.

  A detached window can now be dragged. The drag changes nothing while it is in
  progress — there is no boundary to move and no cell to preview — and the
  decision is made at the drop: plain puts it back in the grid, Shift leaves it
  out. The same gesture in both directions.

  Returning it also un-maximises first, since a window detached by
  Shift+maximise is still maximised and the tiler cannot size one of those. The
  tray window list does the same when re-including, so both routes back work.

## [0.24.1] - 2026-08-18

### Fixed

- **Applications that draw their own title bar were never tiled.** Bambu Studio
  was the report; the classifier floated any window lacking `WS_CAPTION`, and a
  program painting its own frame does not have it. Probing the live window said
  so plainly: wxWidgets, resizable, maximisable, unowned, titled, visible — an
  application by every measure except the one being tested.

  The caption rule was there to catch modal popups, but those are already caught
  by the checks above it: a popup either has an owner, or is not resizable and
  maximisable. Requiring a caption on top of that excluded a growing class of
  ordinary applications to catch windows that were being caught anyway.

  The test that asserted the old behaviour has been inverted rather than
  deleted, so the reasoning is recorded where the next person will look.

## [0.24.0] - 2026-08-18

### Added

- **Shift while dragging detaches a window from the grid**, as it does in
  FancyZones. The window stays exactly where you drop it and the rest tile
  around its absence. Dragging it again without Shift puts it back under the
  tiler, so the gesture undoes itself; the tray window list still shows it as
  excluded, which is the durable way back.
- **Shift while maximising gives genuine fullscreen** — the whole monitor, over
  the taskbar — rather than Windows' maximise, which fits the work area and
  keeps the frame. The window leaves the grid while it is like that, since a
  fullscreen window the tiler still owned would be dragged back into its cell on
  the next pass.
- **A theme editor**, under Settings → Overlay theme → Theme editor. All twelve
  themes listed with live swatches, the system colour picker for the custom
  theme's three colours, plus and minus for opacity, line thickness, corner
  radius and readout size, and inline renaming.

  The preview is the point of it: three mock cells drawn by the same region
  arithmetic the real grid overlay uses, over dark, light and mid backgrounds at
  once, including the size-readout chip and the warning colour. A theme can be
  judged without dragging a real window. Every edit applies and saves
  immediately — a preview the rest of the program does not yet agree with is a
  preview of nothing.

### Changed

- **An edge drop now splits with Ctrl held, not Shift.** Shift is spoken for:
  FancyZones users expect it to detach a window, and matching that costs nothing
  while contradicting it costs muscle memory. Splitting is SuperTile's own
  invention and can take the less-worn key.
- The theme editor's opacity controls stop where the renderer stops — outline at
  60, fill at 200 — rather than offering travel that would be clamped away.
  Pressing minus and seeing nothing change reads as a broken button, not as a
  limit.

## [0.23.2] - 2026-08-18

### Changed

- **Elevated windows are left out of the layout entirely** rather than holding a
  cell they can never occupy. Keeping one in the order made the other windows
  tile around a hole while the elevated window floated over them regardless.
  Excluding it hands that space to windows that can use it, and the admin window
  stays wherever its own application put it — on top, which is generally where
  it wants to be.

- Elevation is now checked when a window first appears rather than after a
  placement has already failed, so it never gets a cell in the first place. The
  probe is one `OpenProcess` per window, remembered for the life of that window:
  the answer cannot change, and repeating it would be a syscall per window per
  retile.

  The failure-path detection this replaces is gone, along with the log line that
  guessed "(elevated?)" whenever `SetWindowPos` was refused. It no longer needs
  to guess.

## [0.23.1] - 2026-08-18

### Removed

- **Restart as administrator**, added an hour ago in 0.23.0. SuperTile stays
  unelevated by design: running a window manager with full rights over the
  session, permanently, so that the occasional admin window can be tiled is not
  a trade worth making. A program that talks to every window on the desktop
  should hold the least authority that does the job.

  Detection stays, because it is the useful half. Elevated windows are still
  named in the log, still marked in an issue report as
  `elevated (cannot be moved)`, and still left alone rather than retried
  forever — the question "why will this window not tile" now has an answer even
  though there is nothing to be done about it. The README says so plainly.

## [0.23.0] - 2026-08-18

### Added

- **Windows running as administrator are now identified rather than retried.**
  Windows enforces User Interface Privilege Isolation: a program at normal
  privilege cannot reposition a window owned by an elevated one. That is a
  security boundary, not a bug, and it is why an elevated console never tiled.

  Such a window used to be indistinguishable from one that simply refuses a
  size, so it was counted as stubborn and written off. It is now detected
  directly, logged by name, and shown in an issue report as
  `elevated (cannot be moved)`. "May not" and "will not" are different problems
  and only one of them is worth retrying.

- **Restart as administrator**, offered in the tray once an elevated window has
  actually been seen and only when SuperTile is not already elevated. It is the
  only way a program can move those windows: `uiAccess` would be narrower but
  needs a binary signed by a trusted authority and installed under Program
  Files, which an unsigned build cannot satisfy.

  Presented as the trade it is — SuperTile gains full rights over the session —
  and Windows still prompts before anything happens. README documents the choice
  and says plainly that declining it is reasonable.

## [0.22.0] - 2026-08-18

### Added

- **Create issue report**, under Settings in the tray. Assembles the machine's
  shape into Markdown that pastes straight into a GitHub issue or an email: OS
  edition and build, architecture, uptime and last restart, last Windows update,
  Defender status, CPU, RAM, fixed disks, every monitor's resolution, work area
  and scaling, the windows on screen, other running processes, and the tail of
  the debug log if it is switched on.

  What it deliberately withholds: **window titles** (a title is the most
  revealing thing a window manager sees — document names, customers, subject
  lines, URLs), **directory paths** (only the file name survives, as
  `*\*\*
ame.exe`), and **your user and machine names**, scrubbed from every
  free-text field including where they are embedded in something else. Titles
  that reached the log are stripped from the log tail by convention: SuperTile
  logs titles inside single quotes and nothing else inside single quotes, which
  is what makes the tail publishable at all.

  The report is put on the clipboard and opened for review before it can go
  anywhere. In an editor rather than a window of our own, deliberately:
  anonymisation is best-effort, so whoever is about to paste this into a public
  tracker needs to read, search and edit all of it. Nothing is uploaded and
  nothing is sent.

### Fixed

- `src/report.rs` was never declared in `src/lib.rs`, so the module — including
  its anonymisation tests — had never been compiled. It had been described as
  written and tested; it was neither, and one clippy failure had been hiding
  behind the omission.

## [0.21.0] - 2026-08-18

### Added

- **Split layouts survive a restart.** A monitor divided into a particular shape
  now comes back that way, rather than being rebuilt from scratch on every
  launch — which is what made the split layout feel arbitrary, and made a whole
  evening's debugging irreproducible.

  The obstacle was that a tree's leaves are window handles, and Windows does not
  preserve those: a saved handle names whatever window inherits the number next
  time, which is worse than no memory at all. So a saved leaf records *what kind
  of window* was there — the executable and class, the same identity the
  geometry memory uses — and each leaf claims one live window of that kind on
  startup.

  Leaves that cannot be filled collapse into their siblings, exactly as closing
  a window does, so six windows last session and four this one restores the
  shape minus those cells rather than leaving holes. Windows matching nothing
  saved are inserted afterwards like any newly-opened window. Saved trees are
  keyed by display fingerprint as well as monitor, because a shape made for a
  5120px ultrawide is nonsense on a laptop panel and docking changes one without
  changing the other.

  Restoring is attempted once per monitor per run: a tree that could not be
  filled at startup will not become fillable a moment later, and retrying would
  fight the user's own subsequent edits.

## [0.20.0] - 2026-08-18

### Fixed

- **Blocks appearing between windows while resizing.** Each cell's outline was
  grown *outwards*, pushing it into the gutter where it met the outlines of the
  cells opposite. Along an edge that merely doubled the line; at a junction
  where four cells meet, four outlines overlapped and filled the corner with a
  solid square. Outlines are now drawn inside their own cell, where they cannot
  touch another one whatever the gap or line thickness.

### Added

- **Five quiet themes** — Dark gray, Gray, Dark blue, Dark green and Dark
  purple. The original six all announce themselves; an overlay that appears for
  half a second over your own windows does not need to be the brightest thing on
  screen. These use low-saturation colours, one- or two-pixel lines and lower
  opacity, and a test asserts they really are quieter than the default rather
  than merely different colours.
- **`appearance.overlay_line_dip`** overrides the theme's line thickness without
  giving up its colours. Zero keeps whatever the theme specifies. One pixel is
  enough to read a boundary by on a dense display.

## [0.19.1] - 2026-08-18

### Fixed

- **Place left/right/above/below did nothing.** 0.19.0's Shift gate was drawn
  around the wrong thing. What needed consent was converting a monitor to the
  tree layout — a mode change that outlives the drag and survives restarts — not
  the split itself.

  On a monitor already using the tree there is nothing to consent to: splitting
  is how that layout works. Worse, the fallback path reordered the window list,
  and in the tree layout that list is *derived from the tree* and overwritten by
  the very next retile — so the drop genuinely did nothing at all.

  An edge drop on a tree layout now splits, as it did before. Shift is still
  required to convert a grid layout into a tree, which is the accident that was
  worth preventing.

## [0.19.0] - 2026-08-18

### Fixed

- **Dragging one edge in the split layout resized every window on the monitor.**
  Each boundary in the tree is stored as a *fraction* of its parent's area, so
  moving any boundary changes the area every boundary inside it is a fraction
  of, and they all slide. That is what a ratio-based tree does by construction,
  and it is not what anyone means by moving a boundary.

  Every boundary's pixel position is now recorded before the change and restored
  afterwards, except the one being dragged. A drag is local: only the cells that
  actually touch that boundary move.

  The test asserts the property that genuinely holds — a window not touching the
  boundary does not move — for three to nine windows. My first attempt asserted
  "at most two cells move" and failed correctly: a boundary can be shared by
  more than two cells when one side is a stack.

### Changed

- **An edge drop only splits when Shift is held.** Dropping on an edge converted
  the whole monitor to the tree layout and wrote it to the config, so a single
  stray drag changed how tiling worked from then on — including across
  restarts. That happened four times today without being intended, and most of
  the resize debugging took place in a layout that was never chosen. A plain
  edge drop now reorders; Shift makes the split deliberate.

### Added

- An end-to-end test of the resize pipeline — pointer to grabbed edges to
  boundary edits to splits to zones — for the grid layout. Every resize fault
  reported today lived in the wiring between those steps rather than inside any
  one of them, and every step had its own passing unit tests.

## [0.18.6] - 2026-08-18

### Fixed

- **WhatsApp could not resize its cell, and the log showed why in one number:
  zero drag polls.** No drag session was ever created, so the gesture did
  nothing and the next retile put the window back.

  Drag adoption, added in 0.16.6, keys on *the window having moved* since the
  previous pass. That misses exactly the applications it was written for: WinUI,
  Chromium and GTK windows do not move while their own border is dragged, so
  nothing about them changes and no drag is ever detected. It only ever caught
  windows that were already halfway to working.

  A drag is now also adopted when the **pointer** moves while resting on one of
  a managed window's resize borders, with the button held. The pointer is the
  one thing that reliably moves, which is the same realisation behind 0.18.0 —
  applied to detecting the drag as well as to measuring it.

## [0.18.5] - 2026-08-18

### Fixed

- **Windows overlapping after a resize, and the desktop freezing for seconds
  before catching up all at once.** Both are the same fault: a drag session that
  was never closed suppresses every event-driven retile, so the layout keeps
  changing while no window is moved into its new cell. Windows overlap because
  they are still where they were, and the desktop appears stuck until the
  session finally closes and everything snaps at once.

  Restores the guard from 0.17.5, which was undone in the 0.17.6 bulk revert.
  It was a sound fix caught up in an indiscriminate one, and these two reports
  are what it addresses. `poll_drag` also ends a session on button-up, but only
  while its timer is running, so it cannot be the only guard; the suppression
  now checks the physical button itself.

### Added

- A test that a grid with unevenly dragged row and column boundaries never
  produces overlapping zones, for two to nine windows. It passes — which is what
  ruled the layout out as the cause above and pointed at stale placement
  instead. Worth keeping so that stays true.

## [0.18.4] - 2026-08-18

### Fixed

- **Dragging a boundary could move a whole group's outer edge rather than the
  edge under the cursor**, so the far side of the neighbouring window moved too.

  0.18.3 made the tree pick a split with the window on the correct side, which
  was necessary but not sufficient: that split may be an *ancestor* separating
  two groups, and moving it drags the outer edge of everything inside them. The
  chosen boundary must also be one the window's own cell actually borders, so
  the walk now compares the window's edge against the boundary position and
  keeps going outwards when they differ.

  A two-pixel tolerance, because halving an area with integer division leaves a
  pixel of slack at some depths and an exact comparison would reject boundaries
  the window really does border.

## [0.18.3] - 2026-08-18

### Fixed

- **In the split layout, every window could only be resized from one of its two
  inner edges.** Discord's left edge worked, the Chrome beside it did not,
  WhatsApp's right edge did not — a pattern that looked like windows blocking
  each other and was nothing of the sort.

  The boundary a dragged edge belongs to depends on which side of the split the
  window sits on: dragging a left or top edge moves the boundary shared with
  whatever precedes it, which is the split where this window is the **second**
  child; a right or bottom edge moves the one where it is the **first**. The
  tree took the innermost split of the matching orientation without checking
  the side, so each window had exactly one working edge, decided by where it
  happened to land in the tree — and which edge that was differed per window,
  which is why it looked arbitrary.

  Dragging the outer edge of the work area still does nothing, because there is
  no boundary there to move.

## [0.18.2] - 2026-08-18

### Fixed

- **A resize worked in one direction but not the other.** 0.18.0 anchored the
  pointer-derived rectangle on the window's own frame, then handed it to code
  that compares against the *zone* — two different coordinate spaces. Which
  boundary an edge belongs to, and the fraction it becomes, are both expressed
  relative to the zone, so the edges that were not being dragged arrived
  displaced by however far the window sat from its cell. One side read as moved,
  the other did not.

  The rectangle is now anchored on the zone, which is what the rest of the
  resize path measures against. 0.18.0's substance is unchanged: the dragged
  edge still comes from the cursor, because a Chromium or GTK window does not
  move during its own resize.

## [0.18.1] - 2026-08-18

### Fixed

- The resize log line now names the window it is measuring. It recorded the
  numbers but not the subject, so one window's measurements were read as
  another's and a diagnosis was built on the mistake: Discord was reported as
  overflowing its cell by 215px when direct measurement shows it sitting exactly
  on it (1276x1052 visible frame, 1276x1052 zone). The 1491px rectangle belonged
  to whichever window the drag session was holding.

  A measurement without its subject is worse than no measurement — it invites a
  confident wrong answer.

### Notes

- The pointer-driven resize in 0.18.0 stands on its own evidence and is
  unaffected: eight consecutive polls with an identical window rectangle during
  an active drag show the window is not moving, whichever window it was.

## [0.18.0] - 2026-08-18

### Fixed

- **A resize now follows the pointer instead of the window.** This is the
  calculation that was wrong underneath most of today's resize faults, and the
  log stated it plainly once it was asked: eight consecutive polls during an
  active drag, all reading `now 1491x1162`, byte-identical.

  Chromium, Electron and GTK windows do not resize themselves while a border is
  being dragged. The rectangle is constant for the entire gesture, so a boundary
  derived from it never moves — which is why Discord, Chrome and GIMP could not
  be resized while ordinary Win32 windows could. Worse, a window sitting off its
  cell (Discord occupied 1491px of a 1276px zone) made that fixed offset look
  like an enormous drag on every poll, which was then rejected for squeezing a
  neighbour, and the tree path reported none of it.

  The dragged edges are now taken from `WM_NCHITTEST` at the start of the
  gesture and moved to follow the cursor, anchored on the rectangle the window
  had when it was grabbed. The cursor is the one thing that is certainly moving.
  Windows that do resize themselves are unaffected — their edge is under the
  cursor anyway, so the two agree.

  Everything I shipped between 0.17.2 and 0.17.5 was chasing consequences of
  this: the vetoed drags, the missing clamping, the refusals. The reverts were
  right; the diagnosis had simply not gone deep enough.

## [0.17.7] - 2026-08-18

### Added

- The verbose log now records the whole minimum-size comparison on every resize
  poll — the live rectangle, the minimum, the zone and the verdict — rather than
  only the conclusion. The readout turning amber with nothing in the log to
  explain it means the boundary stopped for a reason the resize path never
  recorded, and a wrong minimum needs to be as visible as a real limit.

  Behaviour is unchanged. This is instrumentation for the Discord case, where
  the reported minimum (816x508) is far below the window's actual size
  (1501x1156), so the amber warning should not be firing at all.

### Notes

- A measurement worth recording, because it contradicts something said earlier
  in this session: the desktop is **not** over-packed. The work area is
  5120x2112 and the seven windows' reported minimum widths sum to 3980 — they
  would fit side by side in a single row. Overlapping is therefore not the
  arithmetic being impossible, and looking for a policy that "cannot satisfy
  everyone" was the wrong place to look.

## [0.17.6] - 2026-08-18

### Changed

- **Reverted the tiling code to its 0.17.1 state.** Tiling had degraded to the
  point of windows sitting on top of one another, and the four releases before
  this one were each fixing a fault introduced by the one before it. That is not
  convergence, and continuing would have been stubbornness rather than
  engineering.

  Undone: the learned-minimum inference and its veto (0.17.2), the clamping
  added to the split layout (0.17.3), the decision never to refuse a resize
  (0.17.4), and the orphaned-session guard (0.17.5). Some of those were sound in
  isolation — the split layout genuinely has no resize clamping, and that is
  still true and still worth fixing — but each was written and shipped without
  being watched under real use, and the combination was worse than any of them.

  Kept: everything up to and including 0.17.1, which covers the drag adoption
  that fixed GIMP, the Claude palette carrying its question, binary releases and
  the CI fixes.

- The layout is reset to Grid. The split tree does not survive a restart, so a
  monitor left in `bsp` rebuilds an arbitrary partition each launch — a poor
  baseline to judge anything else from.

## [0.17.5] - 2026-08-18

### Fixed

- **Re-tiling stopped entirely, leaving windows overlapping.** A drag session
  that was never closed suppresses every event-driven retile, which 0.15.5
  introduced deliberately so the layout would hold still mid-gesture. The
  failure mode is total and silent: no tiling at all, and nothing in the log to
  say why.

  `poll_drag` already ended a session when the mouse button came up, but only
  while its timer was running — so it could not be the only guard, and any
  session orphaned another way froze the program indefinitely. The suppression
  itself now checks the button: if it is up, the session is discarded and the
  retile proceeds.

  The freeze can no longer outlive the button however the session was orphaned.
  A state this damaging needed a check that does not depend on the mechanism
  that created it.

## [0.17.4] - 2026-08-18

### Changed

- **A resize is never refused outright any more.** With the split layout finally
  reporting (0.17.3), the log showed the answer: 55 refusals against 4 clamps.
  The squeeze guard was vetoing nearly every drag.

  The guard was added because a window squeezed past its minimum clamps and
  overlaps its neighbour, which is untidy. But a window manager that will not
  resize is not untidy, it is broken — that was the wrong trade. SuperTile still
  prefers a movement that keeps every window above its minimum, and still walks
  the drag back looking for one; if none exists it now does what was asked
  anyway and turns the readout amber to say the result will overlap.

  The user's drag wins. A guard may inform a decision that is the user's to
  make; it should not quietly overrule it.

## [0.17.3] - 2026-08-18

### Fixed

- **Nothing could be resized at all.** Revert of 0.17.2, which recorded a
  minimum size from what a window demonstrated. Feeding those figures into the
  squeeze guard made almost every drag look like it worsened the total
  shortfall, so every drag was rejected. The inference itself
  (`layout::learned_minimum`) and its seven tests are kept — the fault is in
  letting it veto the user's own drags, not in the reading.

- **The split layout never had the resize clamping.** 0.16.1 taught a blocked
  resize to stop at the limit instead of refusing outright, but only on the
  parametric layouts. The tree path — which is what a monitor uses after a
  drop-to-split — still rejected outright, silently and without a log line. On
  such a monitor a boundary blocked by a neighbour's minimum did not slow down,
  it stopped dead, which reads as resizing being broken entirely.

  The tree now walks the movement back the same way, and says so in the log.
  The absence of those log lines is what led me to blame the wrong layer twice:
  I read "no clamps, no refusals" as "the guard is not involved", when it meant
  "this path never reports".

## [0.17.2] - 2026-08-18

### Fixed

- **Chrome windows crowding out their neighbours.** `WM_GETMINMAXINFO` says
  Chrome needs 657px of width. In practice it refuses to go below about 1320:
  asked for 1157px at x=2555 it came back 1321px at x=2391 — the same right
  edge, so it honoured the move and declined the size. The tiler went on asking
  for widths Chrome would never accept, Chrome overflowed leftward, and the
  windows to its left could not take the space.

  Every minimum-size defence built since 0.13 trusted that one API, so none of
  them applied to the windows that most needed them.

  A minimum a window demonstrates is now recorded alongside the one it reports,
  and only when **one of its edges landed where it was asked**. That test is
  what 0.16.4 lacked: without it, a window that ignored a placement outright
  looks identical to one that clamped, and reading a minimum out of the former
  froze GIMP at whatever width it happened to have. A window that honours a move
  and refuses a size keeps its anchor edge and pushes the other one out; a
  window that ignored the move matches on neither.

  Changing layout or asking for a retile clears what was learned, so a window
  that becomes more accommodating is not held to an old figure.

## [0.17.1] - 2026-08-18

### Fixed

- **Resizing stopped working on windows next to the one being dragged.** A
  regression from 0.16.6, which adopts a drag when a managed window has moved
  since the previous pass while the mouse button is held. A resize moves *every*
  window on the monitor, so that test matched the neighbours as readily as the
  window under the pointer — and whichever came first in iteration order won.
  Adopting a neighbour opens a drag session for the wrong window, and since
  0.15.5 an open session suppresses event-driven re-tiling, so the layout froze.

  The pointer must now be on the window before its movement is read as a drag,
  with a twelve-pixel grab band because an edge drag puts the cursor just
  outside the frame.

  0.16.6 fixed a real fault and this keeps that fix; it only narrows which
  window the evidence is allowed to point at.

## [0.17.0] - 2026-08-18

### Changed

- **The Claude palette entry carries your question.** It was built as "open an
  empty chat", which was a misreading of the request. Type a question into the
  palette, pick **Ask Claude**, and the question travels with it — the palette
  is a text field you have already filled in, and retyping it in Claude is the
  sort of small indignity that stops a feature being used.

  Claude Desktop publishes no documented deep link for pre-filling a prompt, so
  this does the honest thing rather than the hopeful one: the question goes on
  the clipboard first, then the chat opens. The `?q=` parameter is still tried
  first, so if a future build accepts it the question arrives typed rather than
  pasted — but either way nothing is silently lost, and it is one paste away.

  Percent-encoding is hand-rolled rather than pulling in a URL crate: one
  escaped field is not worth a dependency in the SBOM, an entry in the licence
  audit and another supply-chain surface. It is tested to keep `:`, `/`, `?`,
  `#`, `&` and `=` out of the query, so a question cannot break out of it.

## [0.16.7] - 2026-08-18

### Added

- Instrumentation for the Chrome tear-off placement fault. The log now records
  a drag whose pointer has left the monitor it began on, and every poll where no
  drop resolves — with the cursor position, the number of zones tested and the
  monitor. 0.16.2's theory that the drop was being discarded for matching its
  source slot is now ruled out: that never once happened. Whatever is wrong
  happens earlier, and this says which of the three possible places it is.

## [0.16.6] - 2026-08-18

### Fixed

- **GIMP could not be resized, because SuperTile never saw the drag.** The log
  settles what several attempts had guessed at. GIMP was asked for 1648px of
  width three times and ended at 2187, then 2336, then 1623 — it accepted 1623,
  so the 1855 that looked like a minimum size never was one. Those were
  mid-drag widths. And no clamp or refusal was recorded at all, meaning no drag
  session existed: the user dragged the edge, GIMP obeyed, and the next retile
  put it straight back.

  GTK raises no `EVENT_SYSTEM_MOVESIZESTART`, and neither does Chromium's custom
  frame, so a drag on those windows is invisible to the usual mechanism. A drag
  is now adopted when a managed window has **moved since the previous pass**
  while the primary mouse button is held.

  That test is the fix for the flaw in 0.15.3, which asked whether the window
  differed from what was last requested. A window whose minimum exceeds its cell
  differs from its request permanently and is not moving, so that condition
  collapsed to "the button is held" and suppressed re-tiling on every click,
  including on a close box. One observation cannot tell a misplaced window from
  a moving one; two can.

## [0.16.5] - 2026-08-18

### Fixed

- **Revert 0.16.4**, which stopped GIMP resizing in either direction — worse
  than the leftward-only fault it was meant to fix.

  0.16.4 read "the window ended up larger than it was asked for" as "the window
  has told us its minimum size". That inference is only valid if the window
  actually clamped. GIMP did something different: it ignored the move entirely,
  keeping both its width *and* its left edge. Recording its current width as a
  floor then froze it at that width, because every subsequent layout grew its
  cell to match and the squeeze guard blocked anything smaller.

  A window that has not yet been asked to shrink is not a window at its minimum,
  and the two are indistinguishable from a single observation. Telling them apart
  needs the window watched across several different requests — refusing to go
  below some size repeatedly is evidence; refusing one particular move is not.

## [0.16.4] - 2026-08-18

### Fixed

- **Windows that do not answer `WM_GETMINMAXINFO` are no longer treated as
  having no minimum size.** GIMP could not be grown to the left: the log showed
  it asked for 1290px of width, kept 1855, and held its own left edge — so it
  overlapped its neighbour rather than moving. Asking politely is not the only
  way to find out. GTK applications keep their size constraints inside their own
  toolkit and report nothing through the Win32 message, so the tiler believed
  GIMP had no floor at all and kept requesting sizes it would never accept.

  A window that ends up larger than it was asked for has just stated its real
  minimum more reliably than the API did. That figure is now recorded and fed
  back into the layout, so the next pass asks for something the window will
  actually take. The minimum-size handling added in 0.13 applies to these
  windows for the first time — until now it only ever saw the ones that answer.

## [0.16.3] - 2026-08-18

### Fixed

- **CI failed the SBOM check on every run.** The check compared the generated
  documents byte for byte, which asserts far more than an SBOM claims. The
  claim is "these are the dependencies of this build, at these versions, under
  these licences"; the files also carry a serial number, a generation timestamp
  and the generator's own version, none of which describe the software. A fresh
  clone regenerates different values for all three, so the check could not pass
  anywhere except a machine that had just run the generator. It now compares the
  component set and reports which dependencies were added or removed, so a
  failure is both true and actionable.

- **The autostart test failed on the CI runner.** It asserted that writing to
  `HKCU\Run` succeeds, which is not true on a runner with no loaded user hive.
  An absent facility is not a failing one; the test now skips when the key
  cannot be written and still runs in full on every machine SuperTile ships to.

- **Node 20 deprecation warnings.** `actions/checkout` and
  `actions/upload-artifact` moved to v5.

## [0.16.2] - 2026-08-18

### Changed

- The release workflow can be re-run by hand for a tag that already exists
  (**Actions → Release → Run workflow**, giving the tag), so a first attempt
  that failed can be published without inventing a new version number to carry
  the retry.
- Regenerating the SBOM during a release is advisory rather than a gate. The
  SBOM committed to the repository ships either way; a difference in the build
  toolchain's own metadata must not be able to block a release that is
  otherwise sound.

### Added

- The verbose log now records a drop that was resolved and then discarded for
  matching the slot the drag started from. A window torn out of another one
  inherits a slot index from a layout it was never part of, which is a way a
  perfectly good drop can be thrown away silently — this is instrumentation for
  the Chrome tear-off placement fault, not a fix for it.

## [0.16.1] - 2026-08-18

### Fixed

- **A blocked resize now stops at the limit instead of refusing to move.**
  0.13.4 correctly refused to squeeze a neighbour below its minimum size, but
  refusing the *whole* movement turned "you cannot go past here" into "this does
  not work" — with a large minimum nearby, such as Steam's 1010px width, a
  boundary would not budge at all and nothing said why.

  The movement is now walked back towards where it started until it fits, so the
  boundary follows the pointer up to the neighbour's floor and stops there, which
  is what a limit should feel like. Five probes put it within a tenth of the
  limit; finer would cost a layout computation per step inside a 16ms drag tick
  and would not be visible. The readout turns amber whenever the boundary is
  short of the pointer, so a limit reads as a limit rather than as a stuck
  window, and the reason is recorded in the verbose log.

  Each probe restarts from the layout as it was, so a rejected larger step
  leaves nothing behind for the next one to build on.

## [0.16.0] - 2026-08-18

### Added

- **Overlay themes are chosen from the tray**, under *Settings → Overlay
  theme*, with the six built-ins listed and check-marked. Previously they
  existed only as a config key, which is not a setting anyone will find.
- **A custom theme with the system colour chooser.** Grid colour, blocked
  colour and readout text colour each open the standard Windows picker —
  spectrum, saturation ramp, hex field and custom swatches. It is the dialog
  every other Windows application uses for this, and a hand-drawn one would be
  worse and would need maintaining. Picking a colour also switches to the custom
  theme, since editing a theme you are not using and seeing nothing change is a
  puzzle rather than a feature.
- **Releases publish a binary.** Pushing a `vX.Y.Z` tag now builds
  `supertile.exe`, runs the tests and the version check against the tag,
  verifies the SBOM matches the source, and attaches the executable, a SHA-256
  checksum and the CycloneDX SBOM to a GitHub release. It rebuilds from the tag
  rather than reusing a CI artefact, so what ships is built from exactly the
  source the tag names. The release notes say plainly that the builds are not
  code-signed and show how to verify the checksum.

### Notes

- Renaming the custom theme opens `config.json` at `appearance.custom_theme.name`
  rather than prompting. Win32 has no stock text-input dialog and a hand-built
  one is more window than a single string is worth; the tray picks the name up
  on save. A proper editor window with live preview is still outstanding.

## [0.15.6] - 2026-08-18

### Fixed

- **A torn-off tab dropped with "place left" or "place right" landed in the top
  right instead.** Tearing a tab out of Chrome *replaces* the window mid-drag:
  the handle the gesture began with belongs to the window the tab left, and the
  thing under the pointer is a new window that did not exist when the drag
  started. SuperTile was splitting the chosen cell and putting the **old**
  window into it, leaving the new one to be placed by the next retile — which
  drops a newcomer into the largest free space, the top right. The window
  holding focus at the drop is now taken as the one being carried.

- **A drag session can no longer outlive the mouse button.** Chrome's tab drag
  does not reliably send `MOVESIZEEND`, and since 0.15.5 an open session
  suppresses every event-driven retile — so a session left behind stopped
  tiling altogether and let windows overlap, which is what made moved tabs look
  like they were not being re-tiled. The physical button state is now the
  authority: button up means the gesture is over, whatever Windows did or did
  not report.

  0.15.5 made a stale session far more damaging than it used to be, and shipping
  that without this guard was a mistake.

## [0.15.5] - 2026-08-18

### Fixed

- **The layout no longer re-flows underneath a drag.** Event-driven retiles — a
  focus change, a tab closing, a window appearing — were still running while a
  window was being dragged. Each one moved the other cells, which changed the
  zones the drop was being resolved against, so the drop caption flipped between
  "place left" and "place right" and the dragged window jerked from one cell to
  another with the button still held. Retiles the drag itself requests are
  unaffected; everything arriving from elsewhere now waits until the drop.

### Added

- **The grid is outlined while moving a window, not only while resizing it.**
  "Place left" names a direction with nothing to refer it to unless you can see
  which cell is about to be divided, which is why the drop action read as
  erratic even when it was correct.

## [0.15.4] - 2026-08-18

### Fixed

- **Closing an application stopped re-tiling the grid.** A straight revert of
  0.15.3's drag adoption, which caused it.

  That change treated "primary mouse button held **and** a managed window is not
  where it was put" as a drag Windows had failed to announce, and skipped the
  retile for that pass. The flaw is that the second half of the condition is not
  transient. A window whose minimum size exceeds its cell never matches where it
  was put — that is the whole reason the minimum-size handling exists — so the
  condition reduced to "the mouse button is held". Clicking a window's close box
  is holding the mouse button, so the retile that should have re-flowed the grid
  was suppressed, and a phantom drag session was opened instead.

  The frozen-Chrome-window problem 0.15.3 addressed is therefore back, and is
  the lesser of the two. The condition needs to be "this window moved *since the
  last pass*", which requires tracking each window's previous rectangle rather
  than comparing against the last rectangle requested — a chronic mismatch and a
  fresh movement are different things, and 0.15.3 conflated them.

## [0.15.3] - 2026-08-18

### Fixed

- **A Chrome window that could not be resized at all.** The log showed why:
  zero drag sessions were recorded for it. Chromium's custom frame resizes some
  windows without ever raising `EVENT_SYSTEM_MOVESIZESTART`, so SuperTile never
  entered drag mode and the next retile simply put the window back — every
  time, which reads as the window being frozen.

  SuperTile now adopts the drag from evidence rather than from the
  announcement: if the primary mouse button is held and a managed window has
  moved away from where it was last placed, that is a drag whatever Windows
  chose to announce. The retile stands down for that pass, because the window
  is under the pointer and moving it is precisely the fight this exists to end.

  Costs nothing at idle — the check runs inside the retile pass that was
  already comparing each window against its requested rectangle, so there is no
  extra hook and no extra polling.

## [0.15.2] - 2026-08-18

### Fixed

- **"Open log folder" showed a recycle icon.** It was given the auto-tile glyph
  as a placeholder that never got replaced. It has a folder now.
- Switches no longer take a glyph argument at all. Since 0.15.1 the check-mark
  column is left to Windows, so any glyph passed to a switch was silently
  discarded — and an argument that does nothing is exactly how the wrong icon
  reached these items in the first place. Removing it makes the mistake
  unrepresentable rather than merely corrected, and dropped four now-unused
  glyph constants with it.

## [0.15.1] - 2026-08-18

### Fixed

- **Windows were being written off for being in motion.** This is the fault
  behind Signal, Claude, Steam and a torn-off Chrome tab all losing their place,
  and the logging added in 0.15.0 found it: the log showed a new Chrome window
  at miss 54 of 3, asked to move to `1,1351` while it sat at `3391,1356`,
  ignoring every request.

  Three *consecutive passes* was never evidence of anything. A resize re-tiles
  every 16ms and an event storm can fire a dozen retiles in a quarter of a
  second, so a window could exhaust its three chances in 75 milliseconds — while
  its own application was dragging it, animating it, or starting it up, which is
  precisely when it cannot comply and precisely when that means nothing. Tearing
  a tab out of Chrome does exactly this: Chrome runs its own drag loop and the
  new window ignores every `SetWindowPos` until the drop.

  Three fixes, all pointing the same way:
  - Misses must now be **750ms apart** to count. Passes are not evidence; time
    is.
  - No misses are counted **while a drag is in progress** at all.
  - A written-off window is **retried after 20 seconds** instead of being
    condemned for the session. The reasons a window refuses a size are mostly
    temporary, and a two-second problem should not last until SuperTile is
    restarted.

- **Tray switches now show a real check mark.** Setting `hbmpItem` puts a bitmap
  in the very column Windows reserves for the check, which does not merely fail
  to show the state — it suppresses the check entirely, so every switch looked
  identical whatever it was set to. Swapping the bitmap for a tick glyph was a
  first attempt and still wrong: a Segoe icon in the gutter is not what a
  Windows user reads as "on", because no other menu on the system does that.
  The column is left alone now and `CheckMenuItem` draws the native mark.

  Affected every switch added since 0.12: auto-tile, both resize overlays, and
  both logging toggles.

## [0.15.0] - 2026-08-18

### Added

- **Overlay themes**, with six built in: Windows, Graphite, Ember, Forest,
  Violet and High contrast. A theme sets the grid colour, the warning colour,
  how transparent the outlines and filled bands are, the line thickness, the
  corner radius and the size-readout font. Chosen with
  `appearance.overlay_theme`. These are deliberately separate from the
  light/dark setting that dresses the palette and About window: those are
  ordinary windows and should follow the system, while the overlays are drawn
  *over* your own windows, where what reads well depends on the wallpaper and
  the applications rather than on a system preference.
- **The grid outline is rounded to match Windows 11's window corners** (8dip by
  default, per theme). A square overlay drawn over rounded windows reads as a
  separate object sitting on top rather than as the outline of the space those
  windows occupy. The High contrast theme stays square on purpose — rounding
  costs contrast exactly at the corners where boundaries meet, which is where
  someone relying on that theme most needs to see what is happening.
- **Debug logging is switchable from the tray**, with a second toggle for
  extensive detail: every retile, every placement, and every window entering or
  leaving the managed set, each line timestamped. Turning it on says plainly
  that titles and program paths are recorded and that nothing leaves the
  machine. **Open log folder** reveals the file. Extensive logging implies
  ordinary logging, since the alternative records nothing and reads as a broken
  toggle.

### Fixed

- **CI was failing `cargo fmt --check` on generated code.** `make-sbom.py` emits
  `src/sbom_data.rs` one line per component and rustfmt wraps them, so the
  formatter and the generator disagreed about a file no human wrote. The
  generator now pipes its output through rustfmt, which also keeps
  `make-sbom.py --check` honest: it compares generated text against the file,
  so both sides have to have been through the same formatter.

### Notes

- Windows still occasionally losing their place is not fixed here. The
  instrumentation above exists to catch it: it logs the moment a window leaves
  the layout together with why — excluded, written off as stubborn, or absent
  from the order.

## [0.14.0] - 2026-08-18

### Added

- **The whole grid is outlined while a boundary is dragged.** Dragging moves a
  shared edge, not one window's edge, and an overlay that highlighted only the
  window under the pointer told the opposite story. Every cell is now outlined
  and every cell visibly answers to the drag, so it reads as pulling the
  partition rather than resizing an application. One shaped window rather than
  one per cell: a dozen layered windows appearing and vanishing at 60 Hz would
  cost a dozen creations, z-order insertions and repaints, where a region is
  rebuilt in microseconds. Toggle under Settings ("Show grid while resizing")
  or `appearance.show_grid_on_resize`.

### Fixed

- **Drop actions no longer appear in the middle of a resize.** Move and resize
  were told apart by comparing the window's rectangle against where it started,
  re-decided on every poll. Dragging a left or top edge changes the origin as
  well as the size, so a frame in which the size barely moved read as a move and
  the drop overlay appeared. SuperTile now asks the window what the user grabbed
  — `WM_NCHITTEST`, once, before the drag has moved anything — and holds that
  answer for the whole gesture. Grabbing the title bar moves; grabbing an edge
  resizes; the geometric guess remains only as a fallback for windows that do
  not answer.

  Asking the window also handles the Chromium and Electron title bars that
  dominate a modern desktop, where the "title bar" is a custom-drawn strip no
  amount of geometry would identify.

## [0.13.4] - 2026-08-18

### Fixed

- **Resizing a window no longer squeezes its neighbour below the neighbour's
  minimum size.** The clamp added in 0.13.0 held the *dragged* rectangle to its
  own floor, which protected only the window under the pointer. Every boundary
  has two sides: growing one cell shrinks another, and the shrunken one clamps
  itself, overflows its cell and overlaps — the same mess the clamp was added
  to prevent, arrived at from the other direction.

  The finished layout is now judged instead of the dragged edge, which makes
  the rule layout-agnostic: grid columns, master fractions and tree ratios are
  all covered by one check, including drags that move two boundaries at once.
  A drag that would make the total shortfall worse is rejected and the boundary
  stays put, with the size readout turning amber to say why.

  The test is "does this make it worse", not "does this violate a minimum".
  Fifteen windows on one monitor will never all clear their minimums, and a
  rule phrased as "never violate" would freeze every boundary on the screen —
  including the drags that were relieving the pressure.

## [0.13.3] - 2026-08-18

### Changed

- **A split no longer has to be even to be allowed.** "Too small to split"
  was being reported for plenty of splits that were perfectly possible a little
  off centre: a 1000px cell holding a 700px minimum next to a 250px one has an
  obvious answer, and it is not "no". The boundary is now placed as close to
  the middle as both occupants' minimums allow, and moves only as far as it
  must. The refusal is kept for the one case where it is true — when the cell
  cannot hold both windows however it is divided.
- The drop overlay states the resulting share when the boundary is visibly off
  centre ("Place left (30%)"), so an uneven split is expected rather than
  surprising. An even split says nothing, since every split reading "50%" is
  noise.
- A cell is never divided below 48px along the split axis, even for a window
  that reports no minimum at all. Such a window will still accept whatever it
  is given, and giving it eight pixels is not a split.

### Fixed

- The tray notification about the layout switch was ragged, with gaps in the
  middle of sentences. A backslash line-continuation inside a Rust string
  literal does not survive this file's CRLF line endings; the text is one line
  now.

## [0.13.2] - 2026-08-18

### Fixed

- **Seeding the split layout no longer produces a staircase.** Building the
  tree by repeatedly halving the largest cell is the right rule for adding
  *one* window; applied to a whole list it gave the first window half the
  screen and the last a slice of a slice — with eight windows the smallest cell
  was 1/128th of the area, far below any real application's minimum size. Cells
  are now within a factor of two of each other, and the shape is a pure
  function of the window count, so a restart rebuilds something recognisable
  instead of something new. This is what made the desktop look unsynced after
  each restart: Remote Desktop Manager, WhatsApp, Signal and Chrome were being
  handed cells no application would accept.

- **A drop can no longer change the layout mode silently and permanently.**
  Dropping a window on an edge switches the monitor to the split layout,
  because no parametric grid can divide one cell and leave its neighbours
  alone. That switch was written to the config with nothing said about it, so a
  single drag changed how tiling worked from then on — including across
  restarts — and looked like a fault rather than a feature. It is now announced
  with a tray notification, and **Reset sizes** restores the layout that was in
  use before the drop.

### Notes

- The split tree still does not survive a restart: its leaves are window
  handles, which Windows does not preserve. The balanced seeding above makes a
  cold start predictable rather than random, but reattaching saved splits to
  the applications that owned them is still outstanding.

## [0.13.1] - 2026-08-18

### Fixed

- **Windows with a minimum size are no longer ejected from the grid.** FreeCAD,
  WhatsApp and Steam stopped being tiled at all. Two defects introduced in
  0.12.1, both mine:

  The diagnosis was wrong. A window that clamps to its minimum size was counted
  as refusing to be placed, so the check written to accommodate minimum-size
  windows condemned precisely the windows it was meant to accommodate — three
  retiles was all it took.

  The response was worse than the disease. Being written off removed the window
  from the *order*, not merely from the list of windows to nudge, so every other
  window re-flowed into the vacated slot and the abandoned one floated loose on
  top. "Leave it alone" was meant to mean "stop nagging it"; it meant "eject it
  from the layout".

  SuperTile now grows a cell to the occupant's own floor before asking, so the
  request is one the window can accept and no miss is recorded. Where the floor
  exceeds the cell the window overlaps its neighbour — honest and predictable,
  and vastly better than dropping out of the grid. A window that genuinely will
  not accept a size now keeps its slot and is simply not nudged again.

- A window written off in one layout is no longer condemned in every layout
  after it: asking for a different rectangle resets the tally, because it is a
  different question.

### Changed

- Minimum track sizes are cached per window and pruned with the window list.
  `WM_GETMINMAXINFO` is a synchronous cross-process send; doing it for every
  window on every retile would make the tiler as slow as its slowest
  application.

## [0.13.0] - 2026-08-18

### Added

- **A window can no longer be dragged below its own minimum size.** Detecting
  the minimum was only half the job: 0.12.1 refused splits that would not fit,
  but a plain edge drag could still ask a window to be 40 px wide. It clamped
  itself, ended up wider than its cell and overlapped its neighbour. The
  boundary now stops at the floor, so the window stops following the pointer
  instead of springing back. The edge that gives way is the one being dragged —
  clamping the other end would move a boundary the user is not touching.
- **Windows at their minimum are shown in amber rather than in the accent
  colour.** "Too small to split" in the same blue as "Place right" made the
  caption the only signal, and mid-drag the caption is the thing least likely
  to be read. Applies both to a refused split and to a resize that has hit the
  floor.
- **A live pixel readout during resizes** — `1352 x 651` over the middle of the
  window as it is dragged, in amber once it is at its minimum. Off-by-default
  would bury it, so it ships on, with a tray toggle under Settings
  ("Show size while resizing") and an `appearance.show_size_readout` key in the
  config for anyone who finds it noisy.

### Changed

- The minimum-size probe now runs once per drag rather than once per poll.
  `WM_GETMINMAXINFO` is a synchronous cross-process send; sixty of those a
  second would stall on any application that is briefly busy.

## [0.12.1] — 2026-08-18

### Fixed
- **Windows that refuse a size were fought forever.** A window with a hard
  minimum — Electron apps such as Signal, consoles that size in character
  cells, some Qt dialogs — clamps whatever it is given, so its rectangle never
  matches its zone and every single retile re-issued a `SetWindowPos` for it.
  That reads as erratic resizing, and it can sustain a loop when the
  application reacts to being resized by creating or destroying a window.

  The tiler now compares each window against the rectangle it was last asked
  to take. Three consecutive misses and it concludes the window will not
  comply, logs what was asked for versus what happened, and leaves that window
  alone for the session. Changing layout or asking for an explicit retile
  gives every window another chance, since a window that cannot fit one shape
  may fit another.

- **A split is now refused when it cannot hold.** The cause behind the erratic
  behaviour, rather than only its symptom: halving a cell can leave it smaller
  than the window's minimum, and such a window does not shrink — it clamps,
  overflows its cell and overlaps its neighbours. SuperTile now asks the window
  what its minimum is (`WM_GETMINMAXINFO`, via `SendMessageTimeoutW` with
  `SMTO_ABORTIFHUNG`, so a hung application cannot stall the tiler) and refuses
  a split that would put either window below it. The drop overlay says **"Too
  small to split"** while you are still dragging, instead of promising a split
  and then doing nothing.

- The placement-failure log no longer attributes every failure to elevation.
  `SetWindowPos` being rejected outright and a window clamping the size it was
  given are different problems and now say so.

## [0.12.0] — 2026-08-18

### Added
- **A real split.** Dropping a window on another window's edge now divides
  that window's cell in half — 50% stays, 50% goes to the window you dropped —
  and the windows around it keep their span. Previously an edge drop only
  re-ordered the existing cells, because a layout derived from a window
  *count* has nowhere to put a boundary belonging to one cell.

  This is a new layout, **Split (drag to divide)**, backed by a binary
  partition tree: each node is a leaf holding one window, or a split with an
  orientation and a ratio. Dropping on an edge replaces a leaf with a split
  containing the old occupant and the new one, so nothing outside that subtree
  moves.

- **Splits are destroyed automatically.** Moving a window out of a cell, or
  closing it, collapses the boundary and its sibling absorbs the space — no
  hole is left behind. Moving a window between rows therefore removes the cell
  it vacated, as it should.

- **Dragging a boundary in the split layout** moves the split it belongs to,
  found by walking to the innermost split of the right orientation that
  encloses the window. No index arithmetic, so none of the off-by-ones the
  parametric path needed fixing for.

- Dropping on an edge from any other layout **switches that monitor to the
  split layout**, seeding the tree from the windows already on screen so
  nothing scatters. A grid cannot hold the boundary the drop asks for, and
  silently doing nothing — which is what happened before — is worse than
  changing model and saying so in the log.

### Changed
- `LayoutKind` gains `Bsp`, so the layout cycle and the tray Layout submenu now
  offer seven layouts.
- An edge drop no longer re-orders windows; it splits. The `move_relative`
  path it used has been removed rather than left as tested dead code.

## [0.11.1] — 2026-08-18

### Fixed
- **An edge drop placed the window on the wrong side.** The order was
  rearranged with `remove(from); insert(target)`, but removing the dragged
  window shifts every later element down by one, so a window moving *forwards*
  landed one slot late — dropping on "left" put it on the right. Placement is
  now expressed relative to the target *window* rather than a raw index, which
  removes the ambiguity instead of correcting for it.

### Changed
- **Drop captions now say what actually happens.** They read "New column
  right", which was untrue: the dragged window is already part of the layout,
  so moving it in the order re-orders the existing columns rather than adding
  one. They now read **Place left / right / above / below**, and **Swap**.
  Creating a genuine new column — splitting one window's cell while its
  neighbours keep their span — needs the BSP tree in Milestone 9, and a caption
  must not promise it before it exists.

## [0.11.0] — 2026-08-18

### Added
- **Drop actions.** Dragging a window over another now shows *what will
  happen*, not just where it will land. Each window is divided into bands: the
  middle 60% of an edge inserts there, the centre swaps.
  - the right or left band → **New column right / left**
  - the top or bottom band → **New row above / below**
  - the centre → **Swap**

  The overlay highlights the band it would act on and captions it, so the
  action is legible before the button is released. Corners deliberately fall
  through to Swap: they belong to two bands at once, and without that the
  action would flicker as the pointer moved a pixel.

  Insert works by renumbering the window order, and position in the order *is*
  position in the layout — so in Columns and Rows a drop on an edge genuinely
  produces a new column or row. What it does **not** yet do is split one
  window while its neighbours keep their span; that needs the BSP tree in
  Milestone 9.

### Fixed
- **A corner drag only resized one axis.** The drag resolver returned the
  single edge that had moved furthest, so pulling a corner moved one boundary
  and silently discarded the other. It now returns one edit per axis — within
  an axis the furthest-moved edge still wins, since an edge cannot be pulled
  two ways at once.

## [0.10.0] — 2026-08-18

### Added
- **Claude Desktop in the command palette.** A "New chat in Claude" entry that
  opens Claude Desktop on an empty conversation, searchable by *claude*, *ask*,
  *chat* or *ai*.
  - Shown **only when Claude Desktop is installed**, detected from the
    `claude:` protocol handler and `%LOCALAPPDATA%\AnthropicClaude\claude.exe`.
    Both are checked because either can be absent on a working install.
  - Toggleable in Tray ▸ Settings ▸ Claude in command palette, which itself
    only appears when the application is present — a switch for something
    absent is noise.
  - There is no documented deep link for pre-filling a prompt, so the entry
    opens a new chat and nothing more. Launching uses `ShellExecuteW`, which
    handles custom URI schemes; `Start-Process` does not, which makes the
    handler look broken when it is not.

### Fixed
- **An open palette never picked up the application scan.** The scan runs on a
  worker thread at startup and posted a message that only wrote a log line, so
  opening the palette within the first second showed commands and windows but
  no applications, with nothing to explain why.

## [0.9.1] — 2026-08-18

### Fixed
- **A short final grid row could not be resized — it snapped straight back.**
  Column boundaries were applied only to rows holding a full set of cells, on
  the theory that a short row's cells do not line up with the others. But a
  short final row is the common case, not the exception: five windows is three
  plus two. Every row now carries its own boundaries, sized to its own cell
  count. A row whose width later changes reverts to equal splits rather than
  misapplying a stale vector.

## [0.9.0] — 2026-08-18

Four independent reviewers audited the codebase — two adversarial
design/function critics and two security reviewers. This release is their
confirmed findings.

### Fixed
- **Grid rows could not be resized independently.** Column boundaries were
  shared by the whole grid, so dragging a divider on the bottom row moved the
  same divider on every row above and the lower rows could not be adjusted at
  all. Boundaries now belong to their row.
- **Live resize fought the window being dragged.** `retile_monitor_except`
  documented a `skip` parameter that only the un-maximise loop honoured; the
  placement loop handed the dragged window to `DeferWindowPos` on every 16 ms
  poll.
- **A dropped edge landed short of the pointer.** Drag fractions were measured
  against the raw work area while the layout engine reads them inside the outer
  gap and before the per-zone inner-gap deflation, so an edge missed by
  `outer_gap/2` at the left wall, growing with distance.
- **Pause was not honoured** on the path that actually moves windows, so Move
  window and Retile still re-tiled while the tray said paused.
- **A drag in the first minute after logon was silently killed.** A global
  search-and-replace had put `KillTimer(TIMER_DRAG)` into the tray-icon retry
  path, which fires every two seconds until Explorer is ready.
- **A cross-monitor move swapped two innocent windows** on the source monitor,
  and left the destination untiled.
- **A stale drag session was committed against the next drag.**
  `WM_DRAG_END` ignored the window handle it carries.
- **The exclusion set grew forever.** Windows recycles `HWND` values, so a
  closed excluded window could silently stop an unrelated future window from
  tiling.
- **Dragged boundaries survived a dock/undock**, because device names are
  adapter-output slots rather than monitor identities.
- **The mouse wheel did nothing** in the About and Keyboard shortcuts windows:
  a `RefCell` borrow was held across the call that needed it mutably, so the
  scroll silently failed every time.
- **The palette advertised the wrong shortcut** for six of its eight commands —
  it told the user Exit SuperTile was the palette's own key.
- **Tray toggles gave no indication of their state.** `hbmpItem` occupies the
  same column Windows draws the check mark in, so an item with a glyph could
  never show one: Start with Windows, Auto-tile, Pause and Dimming looked
  identical on and off. The glyph is swapped for a tick when the item is on.
- **"Dimming is on" was a status sentence pretending to be a command**, with no
  clue that clicking it turns dimming off. It is now a stable "Enable dimming"
  label with a tick.

### Changed
- **A hotkey fallback no longer overwrites the chosen key on disk.** The
  application that owned the first choice may have been running only that once;
  the conflict is transient, the user's intent is not. The resolved key is shown
  in the tray and the shortcut editor instead.
- **Working fallbacks are no longer called "unavailable."** The tray counted
  them as conflicts, so a machine running PowerToys met a warning about seven
  shortcuts that all worked.
- The conflict dialog points at Settings ▸ Keyboard shortcuts rather than
  telling the user to edit JSON.

### Security
- **Fixed undefined behaviour and a wrong-window action.** `TrackPopupMenuEx`
  runs a modal message loop that dispatches to the owner window, so a timer,
  hotkey or second tray click while the menu was open produced a second live
  `&mut App` aliasing the first. Observably: a nested menu rewrote the window
  list, so "Always on top" from the outer menu landed on a different window
  than the one clicked. Messages arriving during a modal loop are now not
  handled at all, and the save timer is suspended for the menu's lifetime.
- **Fixed a GDI handle exhaustion reachable in a day of normal use.** Menu
  glyph bitmaps were allocated on every menu open and freed only when the menu
  metric changed — once, on a fixed-DPI machine. About 175 opens exhausted the
  10,000-handle quota, after which the palette and About windows stopped
  painting. Glyphs are now cached by character.
- **Fixed a denial of service driven by another process's window title.**
  Window titles are unbounded and reach the fuzzy matcher's `O(pattern × text)`
  dynamic program on every keystroke; a 2 MB title allocated hundreds of
  megabytes per keypress. The haystack is capped at 512 characters.
- **Fixed a search-path hijack in the terminal shortcut.** `Win+Alt+Enter`
  launched `"wt.exe"` by bare name, which `ShellExecuteW` resolves through
  `HKCU\...\App Paths` and the current working directory — both writable by an
  unprivileged process in the same session, and the CWD is the folder the user
  launched SuperTile from. Absolute paths are resolved from
  `GetSystemDirectoryW` and the user profile instead.
- **Shift alone is no longer accepted as a shortcut modifier.** `Shift+A` bound
  globally swallows every capital A typed anywhere — which is exactly what the
  rejection message claimed to prevent while permitting it.
- `glyph_bitmap` bounds its own `size` rather than relying on its one caller's
  clamp, so the pixel-slice length cannot overflow.

## [0.8.2] — 2026-08-18

### Added
- SemVer enforced end to end: `scripts/version.py` bumps the manifest and opens
  the changelog section together, and CI fails if the manifest version, the top
  changelog heading and the git tag disagree.
- [RELEASING.md](RELEASING.md) documenting the release process.
- Version, git commit and build profile compiled into the binary and shown in
  **About & SBOM**, so a user can report exactly which build they are running.
- Retroactive `v0.1.0`–`v0.8.1` tags on the commits they correspond to.

### Changed
- The changelog is now organised by released version rather than accumulating
  under `[Unreleased]`.

## [0.8.1] — 2026-08-18

### Added
- EU CRA conformance notes mapping Annex I Part I and Part II clause by clause,
  with product classification, support period and six stated residual risks.
- Threat model: assets, trust boundaries, ten threats with mitigations and
  residuals.
- Five-year support policy and `.well-known/security.txt` (RFC 9116).
- `docs/benchmarks.md` with measured figures, explicit about which numbers are
  measured and which are estimates.
- `docs/architecture.md` covering the module map, the single-threaded model and
  the Win32 reentrancy problem.
- CI: fmt, clippy at `-D warnings`, tests, `cargo-deny`, SBOM freshness against
  `Cargo.lock`, and icon/SVG agreement.
- `scripts/check-unsafe.py`, which fails the build on any `unsafe` block without
  a `// SAFETY:` comment. It found 24 unjustified blocks out of 230 on first
  run; all are now documented.

### Fixed
- README linked to `docs/benchmarks.md` and `docs/architecture.md`, neither of
  which existed, and quoted estimated figures for Rust that had since been
  measured.
- README still documented TOML configuration and the pre-i3 hotkeys.

### Security
- `cargo-deny` bans networking crates outright, so the "no network access"
  claim in the CRA notes cannot regress silently.

## [0.8.0] — 2026-08-18

### Added
- Tray **Settings** submenu: Start with Windows, Auto-tile new windows, Pause
  tiling — the switches that previously required editing a file.
- **Start with Windows implemented**, not merely exposed: the setting existed in
  config but nothing ever wrote the registry.
- **Keyboard shortcut editor**: every action, the key actually registered,
  whether it fell back, and the i3 binding it derives from. Click a row and
  press the new combination.
- Real bindings for Increase / Decrease gaps. i3 has none and i3-gaps hides them
  behind a resize mode; SuperTile binds them directly.
- Accelerators and hover tooltips on **every** menu item. Previously only a
  handful of top-level items had them, so Increase gaps, Grow master pane and
  the layout entries showed nothing.

### Security
- The `HKCU\...\Run` value is written **quoted**; an unquoted path containing a
  space is the classic unquoted-path weakness.
- The Start with Windows menu item reports the *registry's* state rather than
  the config's, so a value removed through Task Manager's Startup tab shows as
  off instead of the menu claiming something untrue.

## [0.7.1] — 2026-08-18

### Fixed
- **Drag-resize never started.** `EVENT_SYSTEM_MOVESIZESTART` was missing from
  the WinEvent allow-list, so it was discarded before the drag code ran;
  `MOVESIZEEND` then arrived with no open session and the ordinary retile
  snapped the window back to its zone.
- **The taskbar went fully dark when focusing another window.** `SetWindowPos`
  promotes a non-topmost window to topmost when inserted after a topmost one, so
  focusing any topmost window — or the taskbar itself — dragged the content
  overlay into the topmost band, covering the taskbar at the window level
  instead of its own. The content layer is now confined to the work area, only
  inserts after non-topmost windows, and shell windows are no longer treated as
  the focused application.
- Dim overlays did not follow a retile: the shell layer's cut-out for the
  focused window went stale on resize.

### Performance
- **Idle CPU reduced from 1.56% to 0.57% of one core** (0% when paused), with
  memory drift now negative. Three causes: one WinEvent hook spanning
  `EVENT_SYSTEM_FOREGROUND`..`EVENT_OBJECT_HIDE` delivered a large volume of
  shell traffic that was filtered and thrown away, replaced by five tight
  ranges; `OpenProcess` + `QueryFullProcessImageNameW` ran for every window on
  every enumeration, now cached per `HWND`; and `retile_all` enumerated the
  whole desktop once per monitor, now once in total.
- Windows already in the correct position are left alone rather than issued a
  redundant `SetWindowPos`.

## [0.7.0] — 2026-08-18

### Added
- **Drag to resize**: pulling a tile edge moves the boundary and the neighbour
  gives up exactly that space, live. Columns, Rows, Grid and Master + Stack.
  Monocle and Dwindle have no stored splits and are deliberately excluded.
- **Drag to rearrange**: dragging a window over another tile highlights the
  destination in translucent accent and swaps the two on release.
- `layout::Splits`, storing dragged boundaries as fractions of the work area and
  as shared edges, so the tiling stays exact and survives a resolution change.
- `drag.rs`: pure geometry mapping an edge to the boundary it owns, per layout.

### Performance
- A drag poll whose boundary has not moved by a visible amount does no work, so
  holding the pointer still during a resize costs nothing.

## [0.6.1] — 2026-08-18

### Added
- Tray menu items show their shortcut in the accelerator column, plus a hover
  tooltip giving the key in use, whether it is a fallback, and the i3 origin.
  The accelerator column has room for a key and nothing else, which is not
  enough when a binding silently moved.
- Items without a shortcut get a description of what they do.

## [0.6.0] — 2026-08-18

### Added
- **Focus dimming**: darkens every window except the focused one, with a
  **separate level for the taskbar and Start menu**. Two overlays per monitor,
  because Windows draws in two z-order bands and a single overlay cannot reach
  both — and they want different levels anyway, since a taskbar dimmed to 85% is
  a taskbar you cannot use.
- Auto-track follows focus; **Select window** pins one window bright so a
  windowed game stays lit while you click elsewhere.
- Both overlays are `WS_EX_TRANSPARENT`, so tray icons stay clickable through
  the dim.

## [0.5.0] — 2026-08-18

### Added
- **i3 keybindings**, adapted for Windows 11. `$mod` maps to `Win+Alt`; the
  shell reserves `Win+D`, `Win+E`, `Win+L`, `Win+R`, `Win+S`, `Win+X`,
  `Win+1`–`9` and `Win`+arrows before any application sees them. Everything
  after `$mod` matches i3.
- New actions to match i3: launch terminal, close window, fullscreen toggle,
  reload config, quit.
- **Automatic conflict avoidance.** Each action carries an ordered fallback
  chain and registration walks it until one is accepted, writing the working key
  back to config. On this machine 7 of 24 first choices were already taken and
  all 7 resolved, with nothing left dead.

### Changed
- **Configuration moved from TOML to JSON** (breaking). A binding is structured
  data: the key, the i3 binding it derives from, a note explaining any
  deviation, and its fallback chain. JSON has no comments, so the explanations
  are fields, regenerated on every save so they cannot go stale.
- Geometry memory moved to JSON, dropping the `toml` runtime dependency and
  shrinking the release binary from 806 KB to 702 KB.

### Fixed
- `"Win+Alt+Return"` was not the canonical spelling the formatter emits, so the
  first save would have rewritten every binding. An invariant test now requires
  every default and fallback to round-trip unchanged.

## [0.4.0] — 2026-08-18

### Added
- Tray **Windows** submenu listing every visible, non-minimised window, each
  with bring-to-front, exclude-from-tiling and always-on-top.
- Hovering an entry outlines the real window on screen with a click-through
  layered overlay — titles alone are not enough when three windows belong to the
  same application.
- Hotkey conflicts are counted in the tray menu and explained in a dialog naming
  each binding and the config path. `RegisterHotKey` failing is otherwise
  completely silent, and the product just looks broken.
- The tray icon retries for a minute if the shell is not ready, since at logon
  Explorer can take that long and a missing icon is indistinguishable from a
  crash.

### Fixed
- `own_windows` was conflated with the tiling-exclusion set, so a window
  excluded from tiling also vanished from the command palette. Excluding a
  window from *tiling* should not make it unreachable by name.

## [0.3.0] — 2026-08-18

### Added
- Tray icon with `NOTIFYICON_VERSION_4`, context menu with Layout and Resize
  submenus, and re-registration on the `TaskbarCreated` broadcast so the icon
  survives an Explorer restart.
- Menu glyphs rasterised at runtime from Segoe Fluent Icons into premultiplied
  ARGB DIBs — crisp at any DPI, no bitmap assets in the binary.
- Command palette: fuzzy launcher over applications, open windows and every
  SuperTile command, with match highlighting.
- **About & SBOM** window with a scrollable CycloneDX component table, CRA
  facts, and SBOM export.
- Bounded Start Menu scan on a worker thread. Shortcuts are launched *as*
  shortcuts via `ShellExecuteW` rather than resolved through `IShellLink`;
  Explorer already handles arguments, working directories and Store activation.
- WinEvent-hook-driven retiling with a debounce timer, stable per-monitor window
  ordering, and geometry memory applied to newly-appeared windows.
- Single-instance mutex; PerMonitorV2 declared in the manifest before any window
  exists.
- Reproducible CycloneDX SBOM generation, embedded in the binary.
- Multi-resolution `.ico` generated from the SVG geometry, with a drift check.

### Changed
- GDI and the DWM system backdrop instead of Direct2D. A D3D device costs
  roughly 30 MB resident and ~80 ms first-show for a window visible seconds a
  day.

## [0.2.0] — 2026-08-18

### Added
- Hotkey binding parser and formatter with canonical ordering.
- Configuration treated as untrusted input: a malformed file falls back to
  defaults and is left on disk untouched; one bad binding costs that binding
  only; every numeric field is clamped with a warning per adjustment.
- Monitor enumeration, work areas, per-monitor DPI, and a stable FNV-1a
  arrangement fingerprint. FNV rather than `DefaultHasher` because the value is
  persisted and must not move with the toolchain.
- Window classification isolated as a pure function over a snapshot, including
  DWM cloak detection (the UWP ghost-window case) and extended-frame-bounds
  compensation for the invisible resize border.
- Geometry memory keyed by fingerprint + executable + class, storing both an
  exact zone and a resolution-independent fractional rectangle. Bounded with LRU
  eviction, atomic writes and schema versioning.

### Fixed
- Ownership was tested before the empty-title check, so untitled WinForms
  parking windows were classified as floating windows instead of discarded.
  Found by running the classifier against a live desktop.

### Security
- Modifier-less global hotkeys are refused: a bare-key binding would capture
  that key across the entire desktop.
- `MOD_NOREPEAT` is always set, so a held hotkey cannot flood the message queue.
- Diagnostic logging is opt-in and truncated at each start.

## [0.1.0] — 2026-08-18

### Added
- Project scaffold targeting `x86_64-pc-windows-msvc`, pinned toolchain, release
  profile with fat LTO and a single codegen unit.
- Framework evaluation recorded in the README: Rust + windows-rs chosen over
  .NET NativeAOT, Go, C++ and Python.
- Tiling layout engine with six layouts, gap handling, directional neighbour
  lookup and zone hit-testing. Zone edges are shared boundaries, so a tiled
  monitor has no seams or overlaps at any resolution.
- Fuzzy matcher: fzf-style dynamic programming with word-boundary, camelCase,
  consecutive-run and word-start bonuses.
- Win32 helper layer: lifetime-safe UTF-16 conversion, known-folder resolution,
  opt-in file logging.
- Safe `.gitignore` denying build output, local state and credential-shaped
  files by default.

### Fixed
- Dwindle produced inverted rectangles past ~14 windows; the split axis now
  falls back to whichever side has room and the split point is bounded so the
  remainder keeps a pixel per queued window.
- `Rect::deflate` collapsed small zones to zero size when the inner gap exceeded
  half the zone, producing invisible windows.
- The fuzzy scorer ranked incidental dense substrings above word-initial
  acronyms, so `vsc` preferred "Advanced vsconfig backup" to "Visual Studio
  Code".

### Security
- Layout parameters read from user-editable configuration are clamped before
  use, so a negative gap, an out-of-range fraction or a `NaN` cannot reach
  `SetWindowPos` as an inverted or non-finite rectangle.

[Unreleased]: https://github.com/andreaswiren/supertile/compare/v0.12.1...HEAD
[0.12.1]: https://github.com/andreaswiren/supertile/compare/v0.12.0...v0.12.1
[0.12.0]: https://github.com/andreaswiren/supertile/compare/v0.11.1...v0.12.0
[0.11.1]: https://github.com/andreaswiren/supertile/compare/v0.11.0...v0.11.1
[0.11.0]: https://github.com/andreaswiren/supertile/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/andreaswiren/supertile/compare/v0.9.1...v0.10.0
[0.9.1]: https://github.com/andreaswiren/supertile/compare/v0.9.0...v0.9.1
[0.9.0]: https://github.com/andreaswiren/supertile/compare/v0.8.2...v0.9.0
[0.8.2]: https://github.com/andreaswiren/supertile/compare/v0.8.1...v0.8.2
[0.8.1]: https://github.com/andreaswiren/supertile/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/andreaswiren/supertile/compare/v0.7.1...v0.8.0
[0.7.1]: https://github.com/andreaswiren/supertile/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/andreaswiren/supertile/compare/v0.6.1...v0.7.0
[0.6.1]: https://github.com/andreaswiren/supertile/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/andreaswiren/supertile/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/andreaswiren/supertile/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/andreaswiren/supertile/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/andreaswiren/supertile/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/andreaswiren/supertile/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/andreaswiren/supertile/releases/tag/v0.1.0

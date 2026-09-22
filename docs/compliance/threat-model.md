# SuperTile — threat model

Referenced by [SECURITY.md](../../SECURITY.md) and
[EU-CRA.md](EU-CRA.md) §2. Last reviewed 2026-08-18 against 0.28.x.

---

## 1. What SuperTile is, in security terms

A single unprivileged executable, resident in one user's desktop session, that:

- reads metadata about every top-level window on that desktop,
- moves and resizes those windows,
- registers global hotkeys,
- launches programs the user picks from a list of their own Start Menu
  shortcuts,
- reads and writes two files under `%LOCALAPPDATA%`.

It has **no elevation, no service, no driver, no IPC
endpoint, and no plugin mechanism**. That absence is most of the threat model:
the majority of the attack surface a desktop utility normally has simply is not
present.

The one exception is the update check, added in 0.25.0. It is off by default; it
speaks to exactly one host over TLS; it reads a version and a URL and writes
nothing; and the URL is rejected unless it is under the project's own GitHub
path, because it later reaches `ShellExecuteW`. With the check disabled the
program makes no network connection at all.

From 0.34.0 the same feature can also **download and install** a release, on an
explicit click and no other way. That widens this considerably: bytes fetched
over the network are written over the running executable and then run. What
holds it in: both URLs must be under the project's own GitHub path or nothing is
fetched; the binary must match the `.sha256` published beside it or nothing is
written; and the replacement is put in place by rename, with the previous
executable kept until the next start.

The digest bounds corruption, not authorship. It shows the bytes are the ones
GitHub served for that release; it says nothing about who built them, since
whoever can publish a release can publish its digest. **T11** below covers the
residual risk.

Installing does not elevate SuperTile. Where the install directory is not
user-writable, one `cmd` command is submitted to UAC and the user approves it;
the program itself stays unelevated.

An earlier version of this document said SuperTile had *no network access*. That
was true when written and is no longer, and it is recorded here rather than
edited away: a threat model people rely on has to show where its own claims have
moved.

---

## 2. Assets

| Asset | Why it matters | Where it lives |
|---|---|---|
| Window titles | Can reveal document names, URLs, chat contents | Process memory; on disk only if logging is enabled |
| Executable paths of running programs | Reveals what the user runs | `geometry.json`, and the log if enabled |
| The user's configuration | Controls hotkeys and behaviour | `config.json` |
| The `HKCU\...\Run` value | A persistence location | Registry, only if autostart is enabled |
| SuperTile's own execution | It can move any of the user's windows | The running process |

Notably **absent**: no credentials, tokens, keys, payment data or personal
documents are handled at any point.

---

## 3. Trust boundaries

```
┌─────────────────────────────────────────────────────────────┐
│ Other user sessions, other machines, the network            │
│   → No reachable surface. No sockets, no shared objects.    │
├─────────────────────────────────────────────────────────────┤
│ Elevated processes on the same machine                      │
│   → Already outrank SuperTile. Out of scope (see §6).       │
├─────────────────────────────────────────────────────────────┤
│ Other processes in the SAME user session          ── B1 ──  │
│   → Can supply window titles, class names, .lnk files.      │
│     Treated as untrusted input.                             │
├─────────────────────────────────────────────────────────────┤
│ The user's own files                              ── B2 ──  │
│   → config.json / geometry.json are hand-editable and       │
│     therefore untrusted input.                              │
├─────────────────────────────────────────────────────────────┤
│ SuperTile                                                   │
└─────────────────────────────────────────────────────────────┘
```

**B1** and **B2** are the only boundaries that matter. Everything below is
about them.

---

## 4. Adversaries

| # | Adversary | Capability | In scope |
|---|---|---|---|
| A1 | Malicious/buggy program in the same session | Creates windows with hostile titles, class names, geometry; drops `.lnk` files in the Start Menu | **Yes** |
| A2 | Someone who can edit the user's config | Writes arbitrary JSON to `config.json` | **Yes** |
| A3 | A hostile dependency | Ships malicious code in a crate SuperTile links | **Yes** |
| A4 | Someone tampering with the download | Substitutes a modified `supertile.exe` | **Partly** — see §6.2 |
| A5 | Remote attacker | Network access to the machine | **No surface** |
| A6 | Elevated local malware | Full control of the session | Out of scope |
| A7 | Physical access to an unlocked session | Everything the user can do | Out of scope |

A6 and A7 are excluded because an adversary at that level does not need
SuperTile; nothing SuperTile could do would meaningfully change their position.

---

## 5. Threats and mitigations

### T1 — Hostile window metadata (A1)

A program creates a window whose title is 100,000 characters, contains format
specifiers, or is invalid UTF-16.

**Mitigations.** Titles are read into a length-checked buffer via
`GetWindowTextLengthW` then `GetWindowTextW`, decoded with
`String::from_utf16_lossy` semantics (never assumed valid), and only ever used
as data — formatted with `{}`, never as a format string. Menu labels are elided
on a `char` boundary, and the tooltip caps its own width so a long title cannot
produce a full-screen window. Rust's bounds checking makes the buffer classic
overflow impossible.

**Residual.** A window with a deliberately confusing title can impersonate
another in the tray window list. The list shows the owning executable next to
each title, and hovering outlines the real window on screen, which is the
practical defence.

### T2 — Degenerate window geometry (A1)

A window reports a rectangle designed to break the layout arithmetic —
zero-size, inverted, or near `i32::MAX`.

**Mitigations.** The tileability filter discards windows narrower or shorter
than 2px. Frame insets outside `0..64` are treated as zero rather than trusted.
DPI scaling saturates instead of wrapping, so a large value cannot become a
negative one. Layout arithmetic uses shared boundaries and is property-tested
to tile exactly and never invert, including at negative monitor origins.

### T3 — Malicious configuration (A2)

`config.json` is edited to contain out-of-range numbers, `NaN`, a
modifier-less hotkey, or thousands of rules.

**Mitigations.** Every numeric field is clamped on load with a warning per
adjustment. Non-finite floats are rejected outright. A binding with no modifier
is refused, because a bare-key global hotkey would capture that key across the
whole desktop — a denial-of-service against the user's own machine. Unparseable
files fall back to defaults **without overwriting the user's file**. Unknown
action names are reported and ignored.

**Residual.** A2 can already run programs as the user; config manipulation adds
nothing they could not do directly.

### T4 — Hostile Start Menu shortcut (A1)

A program drops a `.lnk` in the Start Menu so it appears in the palette.

**Mitigations.** The walk is bounded in depth (6) and count (5000), and
`file_type()` does not follow links, so a directory symlink loop cannot trap
it. Only `.lnk` and `.url` are considered. Launch goes through `ShellExecuteW`
with the `open` verb — exactly what Explorer does for the same file.

**Residual.** A shortcut placed in the Start Menu is *already* launchable by
the user from the Start Menu; the palette does not lower the bar. Being able to
write to the Start Menu is itself the compromise.

### T5 — Supply-chain compromise (A3)

A dependency ships malicious code.

**Mitigations.** 34 SBOM components, most build-time only; the runtime tree is
`windows`, `serde` and `serde_json`. `Cargo.lock` is committed and every
registry component is pinned by SHA-256. `cargo-deny` and `cargo-audit` run on
every push **and daily**, so an advisory against unchanged code is still
caught. The SBOM is reproducible and CI fails if the committed copy drifts from
`Cargo.lock`.

**Residual.** A compromised release of a legitimate crate that predates any
advisory would not be caught. This is unsolved industry-wide; the small
dependency count is the mitigation that actually helps.

### T6 — Tampered binary (A4)

A modified `supertile.exe` is substituted for the real one.

**Mitigations.** SHA-256 checksums are published with every release.

**Residual — the significant one.** Releases are **not Authenticode-signed**,
so Windows cannot verify the publisher and users must check the hash manually.
Tracked in [TODO.md](../../TODO.md) as a release blocker for 1.0. Until then
this is a genuine, stated weakness rather than a solved problem.

### T7 — Privilege escalation via SuperTile

Could SuperTile be used to gain privileges it does not have?

**Mitigations.** It requests `asInvoker` and never elevates. It writes only to
`%LOCALAPPDATA%` and one `HKCU` value. It hosts no IPC endpoint, so there is
nothing for a lower-privileged process to talk to. It cannot move elevated
windows — and does not try to acquire the ability.

### T8 — Persistence abuse

The autostart entry is a persistence mechanism.

**Mitigations.** Off by default, written only on explicit user action, a single
`HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value pointing at the
executable's own path, visible in Task Manager's Startup tab and removable
there. Nothing is written to `HKLM`.

**A failure worth recording.** Until 0.34.0 the unit test covering this wrote to
the real `Run` key and restored only *whether* a value had been there, not what
it said — putting back `current_exe()`, which under `cargo test` is the test
harness. Running the suite on a machine with autostart enabled therefore
repointed the user's autostart at a build artefact, and every subsequent logon
launched that instead of SuperTile. It was self-perpetuating, since the harness
runs the test. The test now saves and restores the exact value.

### T11 — Malicious or altered update payload

The in-app updater writes bytes fetched over the network over the running
executable and then runs them. This is the most consequential thing the program
does, so it is worth stating plainly rather than burying in a feature list.

**Mitigations.** The feature never acts on its own: a download begins only when
the user clicks Install. Both URLs are checked against
`https://github.com/andreaswiren/supertile/` when the release is parsed, before
anything is fetched, so a release whose asset points elsewhere simply offers no
Install. The binary is rejected unless it matches the `.sha256` published beside
it, and it is size-capped so a redirect to something enormous is abandoned
rather than buffered. The replacement goes in by rename and the previous
executable is kept until the next start, so a failed write leaves a working
program. Nothing elevates: where the directory needs permission, one visible
`cmd` command goes to UAC.

**Residual risk, not mitigated.** The digest is a checksum, not a signature.
Anyone who can publish a release on this repository can publish a matching
digest, so the feature's trust root is *control of the GitHub repository* — the
same root as downloading the file by hand, but exercised without a person
looking at it. There is no code-signing certificate and no update key. A user
who does not accept that root should leave update checks off and install
manually; the feature is off by default.

### T9 — Screen content exposure through dimming

The dim overlays cover the screen.

**Mitigations.** The overlays are painted black and read nothing. They capture
no screen content and take no screenshots. They are `WS_EX_TRANSPARENT`, so
they cannot intercept input either.

### T10 — Keystroke capture

A window manager with global hotkeys is structurally close to a keylogger.

**Mitigations.** SuperTile uses `RegisterHotKey`, which delivers **only** the
specific combinations it registered. It installs **no** `WH_KEYBOARD_LL` or
`WH_KEYBOARD` hook, so it never sees general keyboard traffic. The command
palette receives characters only while it holds focus, like any text box. This
is verifiable in the source: there is no call to `SetWindowsHookEx` anywhere.

---

## 6. Accepted risks

1. **Elevated windows are unmanageable.** Accepted deliberately: the
   alternative is running a permanently-resident process as administrator,
   which is a much worse security posture than an incomplete feature.
2. **Unsigned releases.** Accepted for 0.x pre-release only; a blocker for
   1.0.
3. **No independent audit.** The internal review is thorough — four
   independent reviewers per build, two of them security-focused — but it is
   not third-party assurance, and is not presented as such.
4. **Window titles held in memory.** Inherent to being a window manager.
   Mitigated by never persisting them unless logging is explicitly enabled.

---

## 7. What would change this model

Any of the following would require a full re-assessment before shipping:

- Adding **any** network access, including an update check
- Adding a plugin, scripting or extension mechanism
- Adding an IPC endpoint of any kind
- Requesting elevation, or installing a service or driver
- Persisting window titles by default
- Handling credentials or any secret material

Each of these removes a load-bearing assumption above.

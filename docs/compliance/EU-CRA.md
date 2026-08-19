# EU Cyber Resilience Act — conformance notes

**Product:** SuperTile — fullscreen autotiling window manager for Windows 11
**Manufacturer:** Andreas Wiren
**Contact:** andreas.wiren@gmail.com
**Repository:** https://github.com/andreaswiren/supertile
**Version covered:** 0.28.x
**Last reviewed:** 2026-08-18

---

## 1. Scope and honest status

This document maps SuperTile against **Regulation (EU) 2024/2847** (the Cyber
Resilience Act). It is written because the project chose to build to CRA
expectations from the first commit, not because SuperTile is currently placed
on the EU market.

> **Status: self-assessment, not a Declaration of Conformity.**
> SuperTile is free, open-source software distributed without monetisation. Per
> **Article 2(18)** and **Recital 18**, free and open-source software supplied
> outside the course of a commercial activity falls outside the Regulation's
> obligations. Should SuperTile ever be commercialised, this document is the
> starting point for a real conformity assessment, and a signed EU Declaration
> of Conformity would be added alongside it.
>
> Nothing here should be read as a claim that a notified body has assessed this
> product. It has not.

### Product classification

SuperTile is a **default-category** product with digital elements. It is not
listed in **Annex III** (important) or **Annex IV** (critical): it is not an
identity manager, password manager, browser, VPN, network management system,
boot manager, hypervisor, or any other listed category. Under
**Article 32(1)** a default-category product uses the internal control
procedure (**Annex VIII, Module A**) — no notified body involvement.

### The main dates

| Date | Obligation |
|---|---|
| 2024-12-10 | Regulation entered into force |
| **2026-09-11** | **Article 14 reporting obligations apply** |
| 2027-12-11 | Full application of remaining obligations |

The Article 14 reporting process in §4 is in place ahead of the September 2026
date.

---

## 2. Annex I, Part I — essential cybersecurity requirements

### (1) Delivered without known exploitable vulnerabilities

`cargo-deny` and `cargo-audit` run in CI against the RustSec advisory database
on every push **and on a daily schedule**. The schedule matters: an advisory
published against an unchanged dependency would never be noticed by push-only
scanning. A release is not tagged with an open advisory of High or Critical
severity outstanding.

### (2)(a) Secure by default configuration

| Default | Value | Reasoning |
|---|---|---|
| Network access | **opt-in, one host** | HTTPS to `api.github.com` for update checks, off by default — see §3 |
| Telemetry | none | Nothing is collected, so nothing can leak |
| Diagnostic logging | **off** | Logs contain window titles and executable paths |
| Focus dimming | off | Opt-in; changes what the screen shows |
| Geometry memory | on, capped at 500 entries | Bounded; local only |
| Elevation | never requested | `asInvoker` in the manifest |
| Autostart | off | One `HKCU\...\Run` value only when the user asks |

Deleting `%LOCALAPPDATA%\SuperTile\` restores the delivered state exactly.

### (2)(b) Protection from unauthorised access

SuperTile runs wholly within one user's session and exposes **no IPC surface**:
no named pipe, no socket, no RPC endpoint, no COM server, no shared memory. The
only cross-process interaction is outbound — the documented Win32 calls in §3.
Its single-instance guard is a `Local\` mutex, so two different users on the
same machine each get their own instance and cannot interfere.

Windows enforces the boundary that matters: a non-elevated SuperTile cannot
manipulate windows owned by an elevated process. That failure is expected,
handled, and logged rather than worked around — SuperTile deliberately does not
request elevation to gain the capability, because a permanently-resident
process running as administrator is a far worse trade than not tiling
Task Manager.

### (2)(c) Confidentiality of stored and transmitted data

Nothing is transmitted. Stored data is confined to
`%LOCALAPPDATA%\SuperTile\`, protected by the user's own NTFS ACL:

| File | Contents |
|---|---|
| `config.json` | Settings and keybindings — no personal data |
| `geometry.json` | Executable paths, window class names, rectangles |
| `supertile.log` | Window titles and executable paths — **only when enabled** |

No credentials, tokens or secrets are handled at any point, so there is nothing
to encrypt at rest that is not already covered by the user's profile
protections.

### (2)(d) Integrity of data, commands and configuration

- **Configuration is untrusted input.** A malformed `config.json` never
  prevents startup: SuperTile falls back to defaults, reports a warning, and
  leaves the user's file untouched rather than overwriting it. Every numeric
  field is clamped on load, so a hand-edited negative gap or `NaN` fraction
  cannot reach `SetWindowPos` as an inverted rectangle.
- **Writes are atomic.** Config and geometry are written to a temporary file and
  renamed over the target (`MoveFileEx` with `MOVEFILE_REPLACE_EXISTING`), so a
  crash or power loss mid-write cannot truncate them.
- **The store is versioned.** A schema mismatch discards the cache rather than
  misinterpreting it.
- **Dependencies are pinned by hash.** `Cargo.lock` is committed and every
  registry component carries its SHA-256 in the SBOM.

### (2)(e) Data minimisation

SuperTile stores only what the features require. Window titles are read to
classify and list windows but are **never persisted** unless diagnostic logging
is explicitly enabled. The geometry store is capped and LRU-evicted, so it
cannot grow into a long-term record of everything the user has ever run.

### (2)(f) Availability of essential functions

A failure in one subsystem does not take the product down:

- A corrupt geometry store degrades to "no remembered positions", not a crash.
- A hotkey another application already owns falls back to an alternative
  automatically, and is reported when no alternative works.
- If the shell is not ready at logon, the tray icon is retried for a minute
  rather than silently never appearing.
- Windows that cannot be placed (elevated ones) are counted and logged; the
  rest are still tiled.

### (2)(g) Minimising impact on other services

The process is event-driven, not polling: window changes arrive through
`SetWinEventHook` and are coalesced through a debounce timer. Idle CPU is
effectively zero and measured resident memory is **1.9 MB private**. The one
place that could be expensive — live re-tiling during a drag — does no work at
all on a poll where nothing moved.

### (2)(h) Limiting attack surfaces

- **~800 KB binary, 34 SBOM components**, most of them build-time only. The
  runtime dependency tree is `windows`, `serde` and `serde_json`.
- **No JSON parser in the About path**: the SBOM is pre-flattened into a Rust
  table at build time, so rendering it parses nothing.
- **No script engine, no plugin loader, no elevation.** One network client, 
  used for a single opt-in GET against a single host.

### (2)(i) Reducing the impact of an incident

Blast radius is one user session. SuperTile cannot modify system state: it
never writes to `HKLM` or `%PROGRAMFILES%`, installs no service, driver or
scheduled task, and holds no privileged handle.

### (2)(j) Security information — recording and monitoring

Opt-in logging records startup, configuration warnings, hotkey resolution,
display changes and placement failures to
`%LOCALAPPDATA%\SuperTile\supertile.log`, truncated at each start so it cannot
grow without bound. It is off by default because it records
user-identifying data, which is the correct trade for a product that has no
security-relevant events to monitor in normal operation.

### (2)(k) Secure removal of data

Deleting `%LOCALAPPDATA%\SuperTile\` removes all stored data. There is no
uninstaller to trust, because there is no installer: the product is a single
executable.

---

## 3. Design decisions bearing on security

### Memory-safe language

SuperTile is written in **Rust**. It parses untrusted-ish input (configuration,
shortcut paths, window titles from other processes) and passes buffers across
the Win32 boundary — precisely where memory-safety defects concentrate in this
class of software. C++ was the closest performance rival and was rejected on
this basis; see the framework evaluation in the README.

`unsafe` is confined to FFI calls. Every block carries a `// SAFETY:` comment
stating the invariant it relies on, and CI rejects any that does not.

### Network access: one host, opt-in, off by default

Until 0.25.0 this section claimed the binary linked no networking API at all.
That stopped being true when the update check was added, and a conformance
document that overstates its own guarantees is worse than one that admits a
narrower scope — so the claim is restated rather than quietly left standing.

What exists now:

- **One client.** WinHTTP, from the operating system. No HTTP crate was added;
  the dependency graph is unchanged.
- **One host.** `api.github.com`, over TLS validated against the OS trust store.
  The release URL taken from the response is rejected unless it is under
  `https://github.com/andreaswiren/supertile/`, because it is later passed to
  `ShellExecuteW`.
- **One trigger.** `updates.check_automatically`, which defaults to `false`. With
  it off, the program makes no connection of any kind. With it on, one request
  per day plus any the user asks for by hand.
- **One direction.** The response is read; nothing is uploaded, and nothing is
  downloaded or executed. A newer version produces a notification and a link.

The request discloses what any HTTPS request discloses: source IP, timing, and a
`User-Agent` of `supertile/<version>`. No identifier is attached and no
configuration, window or machine data is sent.

Separately, **Ask Claude** hands `https://claude.ai/new?q=…` to the default
browser. That is a shell invocation, not a connection SuperTile makes: the
request belongs to the browser and the user's own session, and the question is
visible in the address bar before anything is sent. The URL fills the composer;
it does not submit. That feature is also off by default.

Verifiable either way:

```powershell
# With update checks off, this should list nothing at all
Get-NetTCPConnection -OwningProcess (Get-Process supertile).Id

# With them on, the only remote host should be GitHub
Get-NetTCPConnection -OwningProcess (Get-Process supertile).Id |
  Select-Object RemoteAddress, RemotePort, State
```

Any observed outbound traffic from `supertile.exe` should be
[reported as a vulnerability](../../SECURITY.md).

### Win32 capabilities used, and why

| Capability | Purpose | Risk note |
|---|---|---|
| `SetWinEventHook` (out-of-context) | Detect window create/destroy/focus/drag | Out-of-context: no code is injected into other processes |
| `SetWindowPos` / `DeferWindowPos` | Place windows | Fails safely on elevated windows |
| `RegisterHotKey` | Global shortcuts | No keyboard hook — SuperTile is **not** a keylogger and installs no `WH_KEYBOARD_LL` hook |
| `OpenProcess(QUERY_LIMITED_INFORMATION)` | Read the executable path for rules | Least privilege that answers the question |
| `ShellExecuteW` | Launch from the palette | Only paths from the Start Menu or compile-time constants |
| `Shell_NotifyIconW` | Tray icon | — |

The absence of a low-level keyboard hook is deliberate and worth stating
plainly: `RegisterHotKey` receives only the specific combinations SuperTile
registers, whereas a `WH_KEYBOARD_LL` hook would see every keystroke on the
desktop. The command palette's text box receives input only while it has focus,
like any other window.

---

## 4. Annex I, Part II — vulnerability handling

| Requirement | How it is met |
|---|---|
| (1) Identify and document components | CycloneDX 1.5-schema SBOM at [`sbom.cdx.json`](sbom.cdx.json), regenerated reproducibly from `Cargo.lock`, embedded in the binary and viewable in-app under **Tray → About & SBOM** |
| (2) Address vulnerabilities without delay | Targets in [SECURITY.md](../../SECURITY.md): 30 days critical, 90 days high/medium |
| (3) Apply effective regular tests | `cargo test` (315 tests), `clippy -D warnings`, `cargo-deny`, `cargo-audit`, and a four-reviewer pass each build — two adversarial design/function critics and two security reviewers |
| (4) Publicly disclose fixed vulnerabilities | GitHub Security Advisories; **[Security]** entries in [CHANGELOG.md](../../CHANGELOG.md) |
| (5) Coordinated vulnerability disclosure policy | [SECURITY.md](../../SECURITY.md) |
| (6) Facilitate sharing of information | Public advisories with CVE identifiers where assigned |
| (7) Secure update distribution | GitHub Releases with published SHA-256 checksums |
| (8) Disseminate security updates without delay | Patch releases announced in the changelog and via advisories |

### Article 14 — reporting actively exploited vulnerabilities

| Stage | Deadline | Recipient |
|---|---|---|
| Early warning | 24 hours | CSIRT + ENISA single reporting platform |
| Vulnerability notification | 72 hours | CSIRT + ENISA |
| Final report | 14 days | CSIRT + ENISA |

The same deadlines apply to a severe incident affecting the security of the
product.

### Article 13(8) — support period

**Five years** from each release. The rationale: SuperTile is a small,
self-contained utility with a shallow dependency tree, so a five-year window is
credible to honour rather than aspirational. The period is recorded in the SBOM
as `cra:supportPeriodYears`.

---

## 5. Annex II — information to users

| Required | Where |
|---|---|
| Manufacturer identity and contact | This document, §head; the About dialog |
| Point of contact for vulnerabilities | [SECURITY.md](../../SECURITY.md); `.well-known/security.txt` |
| Product identification | Version resource in the binary; About dialog |
| Intended use and essential requirements | [README](../../README.md) |
| Known circumstances leading to significant risks | §6 below |
| Where the SBOM can be found | In-app **About & SBOM**, and [`sbom.cdx.json`](sbom.cdx.json) |
| Technical security support and support period end | [SECURITY.md](../../SECURITY.md) |
| Secure installation, operation and removal | [README](../../README.md) |
| Effect of changes on data security | [CHANGELOG.md](../../CHANGELOG.md) |
| Secure decommissioning, including data removal | §2(2)(k) |

---

## 6. Known limitations and residual risks

Stated plainly rather than buried, because a compliance document that lists no
limitations is not credible.

1. **Elevated windows cannot be managed.** A non-elevated SuperTile cannot move
   windows owned by an elevated process. This is a deliberate refusal to
   require elevation, not an oversight. Affected windows are counted and logged.
2. **Releases are not yet Authenticode-signed.** Until a code-signing
   certificate is obtained, SmartScreen will warn on first run and users cannot
   verify the publisher from the file properties. **Verify the SHA-256 against
   the release page.** Tracked in [TODO.md](../../TODO.md).
3. **The build is not yet fully reproducible.** The SBOM is reproducible;
   the binary is not independently verified byte-for-byte. Tracked.
4. **Window titles are read from other processes.** Necessary to classify and
   list windows. They are not persisted unless logging is enabled, but they are
   held in memory while SuperTile runs.
5. **No independent security audit.** The review process in §4(3) is thorough
   but internal. No third party has assessed this code.
6. **Hotkey fallback changes your keys.** When a shortcut is already owned by
   another application SuperTile silently moves to its fallback and records
   the change. This is reported in the tray menu, but it does mean the key that
   works may not be the key that was configured.

---

## 7. Verifying these claims

```powershell
# Integrity of the download
Get-FileHash .\supertile.exe -Algorithm SHA256

# No network connections while update checks are off (the default)
Get-NetTCPConnection -OwningProcess (Get-Process supertile).Id

# Not elevated
(Get-Process supertile).Path
# ...then check the Elevated column in Task Manager's Details tab

# Everything it stores
Get-ChildItem $env:LOCALAPPDATA\SuperTile
```

From source:

```bash
cargo test && cargo clippy --all-targets -- -D warnings && cargo deny check
python scripts/make-sbom.py --check    # committed SBOM matches Cargo.lock
```

---

## 8. Document control

| Version | Date | Change |
|---|---|---|
| 1.0 | 2026-08-18 | Initial assessment for 0.1.x |
| 1.1 | 2026-08-18 | Reassessed for 0.28.x. Network access restated: the claim of none was overtaken by the opt-in update check in 0.25.0 (§3). SBOM schema corrected to CycloneDX 1.5. |

Reviewed at each minor release and whenever the dependency set changes.

//! Issue reports: everything a maintainer needs, nothing that identifies you.
//!
//! A bug report for a window manager is close to useless without the machine's
//! shape — how many monitors, at what scaling, which build of Windows, whether
//! the box is out of memory. Collecting that by hand is tedious enough that
//! most people skip it, so SuperTile assembles it in one click as Markdown that
//! pastes straight into a GitHub issue or an email.
//!
//! ## What is deliberately left out
//!
//! Everything here is destined for a public issue tracker, so the safe default
//! is exclusion:
//!
//! - **Window titles.** A title is the single most revealing thing a window
//!   manager sees: document names, customer names, subject lines, URLs. None
//!   are collected, and titles that reached the log are redacted out of the log
//!   tail (see [`redact_log_line`]).
//! - **Directory paths.** Only the executable's file name survives; the
//!   directories collapse to `*\*\*\whatsapp.exe`. Paths leak the user name,
//!   the organisation's folder conventions and the installed-software layout.
//! - **The user and machine names**, scrubbed from every free-text field by
//!   [`Redactor`] even where they are embedded in some other string.
//!
//! What remains is the process file name, which is what a maintainer actually
//! needs in order to reproduce: `whatsapp.exe misbehaves at 150% scaling`.
//!
//! ## What is *not* anonymised, and why you should still read it
//!
//! Anonymisation is best-effort, not a guarantee. A process file name can
//! itself be revealing — bespoke line-of-business software often carries a
//! company name — and the log tail is free text produced all over the codebase.
//! The report is therefore always shown to the user before it goes anywhere.
//! It is never uploaded, never sent, and never copied without an explicit
//! click.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDriveStringsW,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
};
use windows::Win32::System::SystemInformation::{
    ComputerNamePhysicalDnsHostname, GetComputerNameExW, GetLocalTime, GetNativeSystemInfo,
    GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX, PROCESSOR_ARCHITECTURE_AMD64,
    PROCESSOR_ARCHITECTURE_ARM, PROCESSOR_ARCHITECTURE_ARM64, PROCESSOR_ARCHITECTURE_IA64,
    PROCESSOR_ARCHITECTURE_INTEL, SYSTEM_INFO,
};
use windows::Win32::System::WindowsProgramming::DRIVE_FIXED;

use crate::util::{wide_to_string, WideStr};

/// Scrubs identifying strings out of free text.
///
/// Built once per report from the values worth hiding — the user name, the
/// machine name, the profile directory — and applied to anything that was not
/// assembled from known-safe parts.
///
/// Matching is case-insensitive because Windows paths are, and a report that
/// redacts `Andreas` but leaves `ANDREAS` has not redacted anything.
#[derive(Default, Clone)]
pub struct Redactor {
    /// Lower-cased needles, longest first, each with its replacement.
    terms: Vec<(String, &'static str)>,
}

impl Redactor {
    pub fn new(user: &str, machine: &str) -> Self {
        let mut terms: Vec<(String, &'static str)> = Vec::new();
        // Two characters is not a name, it is a substring waiting to eat an
        // unrelated word. Skip anything that short rather than mangle the text.
        if user.len() > 2 {
            terms.push((user.to_lowercase(), "<user>"));
        }
        if machine.len() > 2 && !machine.eq_ignore_ascii_case(user) {
            terms.push((machine.to_lowercase(), "<machine>"));
        }
        // Longest first: a machine named after its owner must not be half
        // replaced by the shorter user-name rule.
        terms.sort_by_key(|t| std::cmp::Reverse(t.0.len()));
        Self { terms }
    }

    /// Replace every known identifying term in `text`.
    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (needle, replacement) in &self.terms {
            out = replace_ignore_case(&out, needle, replacement);
        }
        out
    }
}

/// Case-insensitive replace. `needle` must already be lower-cased.
fn replace_ignore_case(haystack: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() {
        return haystack.to_string();
    }
    let lower = haystack.to_lowercase();
    // Byte offsets from the lower-cased copy are only valid in the original
    // when lower-casing preserved length. It does not always (e.g. 'İ'), so
    // fall back to leaving the text alone rather than slicing at a wrong index.
    if lower.len() != haystack.len() {
        return haystack.to_string();
    }
    let mut out = String::with_capacity(haystack.len());
    let mut at = 0;
    while let Some(found) = lower[at..].find(needle) {
        let start = at + found;
        if !haystack.is_char_boundary(start) || !haystack.is_char_boundary(start + needle.len()) {
            break;
        }
        out.push_str(&haystack[at..start]);
        out.push_str(replacement);
        at = start + needle.len();
    }
    out.push_str(&haystack[at..]);
    out
}

/// Reduce a full executable path to its file name behind anonymous separators.
///
/// `C:\Users\Andreas\AppData\Local\WhatsApp\WhatsApp.exe` becomes
/// `*\*\*\WhatsApp.exe`. The star count is fixed rather than matching the real
/// depth: directory depth is itself a weak fingerprint, and a maintainer has no
/// use for it.
pub fn anonymise_path(path: &str) -> String {
    let name = path
        .rsplit(['\\', '/'])
        .find(|s| !s.is_empty())
        .unwrap_or("(unknown)");
    format!(r"*\*\*\{name}")
}

/// Where an executable lives, in the only terms that are both useful and safe.
///
/// Whether a process runs from Program Files, the user profile or a system
/// directory genuinely matters — it separates an installed application from a
/// side-loaded one, and store apps behave differently from both. None of it
/// identifies anybody, so it survives where the path itself does not.
pub fn path_class(path: &str) -> &'static str {
    let p = path.to_lowercase();
    if p.contains(r"\windowsapps\") {
        "Store app"
    } else if p.contains(r"\program files (x86)\") {
        "Program Files (x86)"
    } else if p.contains(r"\program files\") {
        "Program Files"
    } else if p.contains(r"\windows\") {
        "Windows"
    } else if p.contains(r"\appdata\") || p.contains(r"\users\") {
        "user profile"
    } else if p.is_empty() {
        "unknown"
    } else {
        "other"
    }
}

/// Strip anything the log convention marks as a window title.
///
/// SuperTile logs window titles inside single quotes and nothing else inside
/// single quotes — see the note on [`crate::log!`]. That convention is what
/// makes the log tail publishable at all: without a rule, redacting free text
/// would be guesswork.
///
/// The rule is applied conservatively. An unterminated quote redacts to the end
/// of the line rather than giving up, because the failure that matters here is
/// leaking a title, not losing a diagnostic.
pub fn redact_log_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('\'') {
        out.push_str(&rest[..open]);
        out.push_str("'<title>'");
        match rest[open + 1..].find('\'') {
            Some(close) => rest = &rest[open + 1 + close + 1..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// A monitor, as the report describes it.
pub struct DisplayInfo {
    pub index: usize,
    pub width: i32,
    pub height: i32,
    pub work_width: i32,
    pub work_height: i32,
    pub dpi: u32,
    pub primary: bool,
}

/// A window SuperTile is managing, or has decided not to.
pub struct WindowRow {
    pub exe: String,
    pub class: String,
    pub width: i32,
    pub height: i32,
    pub min_width: i32,
    pub min_height: i32,
    pub state: &'static str,
}

/// A fixed disk.
pub struct DiskInfo {
    pub letter: char,
    pub total_gb: f64,
    pub free_gb: f64,
}

/// Everything the report knows. Collected by [`collect`], rendered by
/// [`render`]; split so the rendering can be tested without a desktop.
#[derive(Default)]
pub struct Report {
    pub app_version: String,
    pub os_name: String,
    pub os_build: String,
    pub os_arch: String,
    pub uptime: String,
    pub last_boot: String,
    pub last_update: String,
    pub defender: String,
    pub cpu: String,
    pub cpu_cores: u32,
    pub ram_total_gb: f64,
    pub ram_used_percent: u32,
    pub disks: Vec<DiskInfo>,
    pub displays: Vec<DisplayInfo>,
    pub layout: String,
    pub settings: Vec<(String, String)>,
    pub hotkey_conflicts: Vec<String>,
    pub windows: Vec<WindowRow>,
    pub other_processes: BTreeMap<String, u32>,
    pub log_tail: Vec<String>,
    pub logging_enabled: bool,
    pub verbose: bool,
}

/// One line describing every category of data in the report.
///
/// Shown before the report and repeated inside it. A user who is about to paste
/// this into a public tracker is owed a plain list of what they are pasting,
/// and a maintainer who receives it is owed the same list so nobody has to
/// guess whether a field was scrubbed.
pub const DISCLOSURE: &[(&str, &str)] = &[
    (
        "SuperTile",
        "version, layout, your settings, hotkey conflicts",
    ),
    (
        "Windows",
        "edition, build, architecture, uptime, last update, Defender status",
    ),
    (
        "Hardware",
        "CPU model, core count, total RAM, disk free space",
    ),
    (
        "Displays",
        "count, resolution, work area and scaling of each monitor",
    ),
    (
        "Windows on screen",
        "process file name, window class, size and minimum size",
    ),
    ("Other processes", "process file names and how many of each"),
    (
        "Log",
        "the tail of the debug log, if debug logging is switched on",
    ),
];

/// What the report deliberately withholds, in the same plain terms.
pub const EXCLUSIONS: &[&str] = &[
    "Window titles — not collected, and removed from the log tail",
    r"Directory paths — only the file name survives, as *\*\*\name.exe",
    "Your user name and machine name — replaced with <user> and <machine>",
    "Documents, clipboard contents, keystrokes and network activity — never read",
];

/// Render the report as Markdown suitable for a GitHub issue.
pub fn render(r: &Report, red: &Redactor) -> String {
    let mut s = String::with_capacity(8192);

    let _ = writeln!(s, "### SuperTile issue report\n");
    let _ = writeln!(
        s,
        "<!-- Generated by SuperTile {}. Please describe what you did, what you \nexpected, and what happened instead, above this line. -->\n",
        r.app_version
    );

    let _ = writeln!(s, "**What happened**\n\n_(describe it here)_\n");

    let _ = writeln!(s, "<details>\n<summary>Diagnostics</summary>\n");

    let _ = writeln!(s, "#### SuperTile\n");
    let _ = writeln!(s, "| | |");
    let _ = writeln!(s, "|---|---|");
    let _ = writeln!(s, "| Version | {} |", r.app_version);
    let _ = writeln!(s, "| Layout | {} |", r.layout);
    let _ = writeln!(
        s,
        "| Debug logging | {} |",
        match (r.logging_enabled, r.verbose) {
            (false, _) => "off",
            (true, false) => "on",
            (true, true) => "on (extensive)",
        }
    );
    for (k, v) in &r.settings {
        let _ = writeln!(s, "| {k} | {v} |");
    }
    if !r.hotkey_conflicts.is_empty() {
        let _ = writeln!(s, "\n**Hotkeys unavailable on this machine**\n");
        for c in &r.hotkey_conflicts {
            let _ = writeln!(s, "- {}", red.apply(c));
        }
    }

    let _ = writeln!(s, "\n#### System\n");
    let _ = writeln!(s, "| | |");
    let _ = writeln!(s, "|---|---|");
    let _ = writeln!(s, "| OS | {} |", red.apply(&r.os_name));
    let _ = writeln!(s, "| Build | {} |", r.os_build);
    let _ = writeln!(s, "| Architecture | {} |", r.os_arch);
    let _ = writeln!(s, "| Last restart | {} |", r.last_boot);
    let _ = writeln!(s, "| Uptime | {} |", r.uptime);
    let _ = writeln!(s, "| Last Windows update | {} |", r.last_update);
    let _ = writeln!(s, "| Defender real-time protection | {} |", r.defender);
    let _ = writeln!(
        s,
        "| CPU | {} ({} logical cores) |",
        red.apply(&r.cpu),
        r.cpu_cores
    );
    let _ = writeln!(
        s,
        "| RAM | {:.1} GB total, {}% in use |",
        r.ram_total_gb, r.ram_used_percent
    );
    for d in &r.disks {
        let _ = writeln!(
            s,
            "| Disk {}: | {:.0} GB total, {:.0} GB free ({:.0}% used) |",
            d.letter,
            d.total_gb,
            d.free_gb,
            if d.total_gb > 0.0 {
                (1.0 - d.free_gb / d.total_gb) * 100.0
            } else {
                0.0
            }
        );
    }

    let _ = writeln!(s, "\n#### Displays\n");
    let _ = writeln!(s, "| # | Resolution | Work area | Scaling | Primary |");
    let _ = writeln!(s, "|---|---|---|---|---|");
    for d in &r.displays {
        let _ = writeln!(
            s,
            "| {} | {}x{} | {}x{} | {}% ({} dpi) | {} |",
            d.index,
            d.width,
            d.height,
            d.work_width,
            d.work_height,
            d.dpi * 100 / 96,
            d.dpi,
            if d.primary { "yes" } else { "" }
        );
    }

    let _ = writeln!(s, "\n#### Windows on screen\n");
    if r.windows.is_empty() {
        let _ = writeln!(s, "_None._");
    } else {
        let _ = writeln!(s, "| Process | Location | Class | Size | Minimum | State |");
        let _ = writeln!(s, "|---|---|---|---|---|---|");
        for w in &r.windows {
            let min = if w.min_width == 0 && w.min_height == 0 {
                "none".to_string()
            } else {
                format!("{}x{}", w.min_width, w.min_height)
            };
            let _ = writeln!(
                s,
                "| `{}` | {} | `{}` | {}x{} | {} | {} |",
                anonymise_path(&w.exe),
                path_class(&w.exe),
                red.apply(&w.class),
                w.width,
                w.height,
                min,
                w.state
            );
        }
    }

    if !r.other_processes.is_empty() {
        let _ = writeln!(s, "\n#### Other processes running\n");
        let mut line = String::new();
        for (name, count) in &r.other_processes {
            if !line.is_empty() {
                line.push_str(", ");
            }
            let _ = write!(line, "{name}");
            if *count > 1 {
                let _ = write!(line, " x{count}");
            }
        }
        let _ = writeln!(s, "{}\n", red.apply(&line));
    }

    let _ = writeln!(s, "\n#### Log\n");
    if !r.logging_enabled {
        let _ = writeln!(
            s,
            "_Debug logging is off. Switch it on from the tray under \
             Diagnostics, reproduce the problem, then generate the report again._"
        );
    } else if r.log_tail.is_empty() {
        let _ = writeln!(s, "_Logging is on but nothing has been recorded yet._");
    } else {
        let _ = writeln!(s, "```");
        for line in &r.log_tail {
            let _ = writeln!(s, "{}", red.apply(&redact_log_line(line)));
        }
        let _ = writeln!(s, "```");
    }

    let _ = writeln!(s, "\n#### What this report withholds\n");
    for e in EXCLUSIONS {
        let _ = writeln!(s, "- {e}");
    }

    let _ = writeln!(s, "\n</details>");
    s
}

// ---------------------------------------------------------------------------
// Collection
// ---------------------------------------------------------------------------
//
// Everything below runs on the UI thread the moment the user picks "Generate
// issue report", so the governing rule is that collection is *total*: every
// probe either produces a value or produces "unknown", and none of them can
// panic, block or leave the report half-built. A diagnostic tool that crashes
// while diagnosing is worse than no diagnostic tool, and a report missing one
// row is still worth reading.

/// How much of the log to include.
///
/// Two hundred lines is roughly the last few minutes of ordinary logging and a
/// few seconds of verbose logging — enough to cover the incident the user has
/// just reproduced, short enough that the issue stays readable.
const LOG_TAIL_LINES: usize = 200;

/// How much of the log file to read in order to find those lines.
///
/// A verbose log reaches megabytes in an afternoon and there is no reason to
/// load all of it to keep the end. Reading the last quarter-megabyte covers 200
/// lines many times over even when individual lines are long.
const LOG_TAIL_BYTES: u64 = 256 * 1024;

/// Gather everything about the machine that the caller cannot already know.
///
/// The parameters are the things only the running application can answer — its
/// own version, which layout is active, what the user has configured, which
/// windows are on screen. Everything else is read from the system here.
pub fn collect(
    app_version: &str,
    layout: &str,
    settings: Vec<(String, String)>,
    hotkey_conflicts: Vec<String>,
    windows: Vec<WindowRow>,
    logging_enabled: bool,
    verbose: bool,
) -> Report {
    let (arch, cores) = architecture_and_cores();
    let (ram_total_gb, ram_used_percent) = memory();
    let uptime_ms = tick_count();

    Report {
        app_version: app_version.to_string(),
        os_name: os_name(),
        os_build: os_build(),
        os_arch: arch,
        uptime: format_uptime(uptime_ms),
        last_boot: last_boot(uptime_ms),
        last_update: last_windows_update(),
        defender: defender_status(),
        cpu: cpu_model(),
        cpu_cores: cores,
        ram_total_gb,
        ram_used_percent,
        disks: fixed_disks(),
        displays: displays(),
        layout: layout.to_string(),
        settings,
        hotkey_conflicts,
        windows,
        other_processes: other_processes(),
        log_tail: if logging_enabled {
            log_tail()
        } else {
            Vec::new()
        },
        logging_enabled,
        verbose,
    }
}

/// A [`Redactor`] primed with the names of the person and machine at hand.
///
/// Kept separate from [`collect`] because the caller needs it to scrub the
/// free-text fields it supplies itself, and because a report rendered with the
/// wrong redactor is a privacy failure rather than a missing table row.
pub fn current_redactor() -> Redactor {
    let user = std::env::var("USERNAME").unwrap_or_default();
    Redactor::new(&user, &machine_name())
}

/// The NetBIOS/DNS host name, from Win32 first and the environment second.
///
/// `%COMPUTERNAME%` is only a fallback because it can be overridden per process
/// and a stale value there would mean the real name goes unredacted.
fn machine_name() -> String {
    let mut buf = [0u16; 256];
    let mut size = buf.len() as u32;
    // SAFETY: `buf` outlives the call and `size` states its length in UTF-16
    // code units, which is the contract GetComputerNameExW documents. On
    // failure the buffer is left as the zeroes it was initialised with.
    let ok = unsafe {
        GetComputerNameExW(
            ComputerNamePhysicalDnsHostname,
            Some(PWSTR(buf.as_mut_ptr())),
            &mut size,
        )
    }
    .is_ok();
    let name = if ok {
        wide_to_string(&buf)
    } else {
        String::new()
    };
    if name.is_empty() {
        std::env::var("COMPUTERNAME").unwrap_or_default()
    } else {
        name
    }
}

// --- registry ---------------------------------------------------------------

/// Read a `REG_SZ` from HKLM, or `None` if it is absent, of another type, or
/// unreadable at this privilege level.
///
/// `RegGetValueW` rather than open/query/close because it does the whole thing
/// in one call and, crucially, guarantees the result is NUL-terminated even
/// when the value in the registry is not — a real hazard with hand-edited keys.
fn reg_string(subkey: &str, value: &str) -> Option<String> {
    let sub = WideStr::new(subkey);
    let name = WideStr::new(value);
    let mut buf = [0u16; 512];
    let mut size = std::mem::size_of_val(&buf) as u32;
    // SAFETY: both wide strings outlive the call. `size` is the byte length of
    // `buf`, and RRF_RT_REG_SZ makes the API reject anything that is not a
    // string, so it cannot write a foreign representation into the buffer.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            sub.as_pcwstr(),
            name.as_pcwstr(),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    if !status.is_ok() {
        return None;
    }
    let s = wide_to_string(&buf).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Read a `REG_DWORD` from HKLM, or `None` on any failure.
fn reg_dword(root: HKEY, subkey: &str, value: &str) -> Option<u32> {
    let sub = WideStr::new(subkey);
    let name = WideStr::new(value);
    let mut out: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: the out-buffer and its stated size describe the same u32, and
    // RRF_RT_REG_DWORD makes the API refuse to write anything else.
    let status = unsafe {
        RegGetValueW(
            root,
            sub.as_pcwstr(),
            name.as_pcwstr(),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut out as *mut u32 as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    status.is_ok().then_some(out)
}

const CURRENT_VERSION: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";

/// The Windows edition, corrected for the build number.
///
/// `ProductName` still reads "Windows 10 Pro" on Windows 11: Microsoft froze
/// the value so that version-sniffing installers would keep working. Reporting
/// it verbatim would send every Windows 11 bug report in under the wrong
/// heading, so the build number decides — 22000 is the first Windows 11 build.
fn os_name() -> String {
    let product = reg_string(CURRENT_VERSION, "ProductName");
    let build: u32 = reg_string(CURRENT_VERSION, "CurrentBuild")
        .and_then(|b| b.parse().ok())
        .unwrap_or(0);
    let name = match product {
        Some(p) if build >= 22000 => p.replace("Windows 10", "Windows 11"),
        Some(p) => p,
        None => return "unknown".to_string(),
    };
    // DisplayVersion is the feature-update label ("24H2"). It matters because
    // shell behaviour changes between them on an unchanged major version.
    match reg_string(CURRENT_VERSION, "DisplayVersion") {
        Some(v) => format!("{name} ({v})"),
        None => name,
    }
}

/// `CurrentBuild.UBR`, the form Microsoft's own support pages ask for.
///
/// The update revision matters: two machines on build 26200 can be months of
/// cumulative updates apart, and window-manager behaviour has changed inside a
/// single build before.
fn os_build() -> String {
    let Some(build) = reg_string(CURRENT_VERSION, "CurrentBuild") else {
        return "unknown".to_string();
    };
    match reg_dword(HKEY_LOCAL_MACHINE, CURRENT_VERSION, "UBR") {
        Some(ubr) => format!("{build}.{ubr}"),
        None => build,
    }
}

/// When Windows last installed an update successfully.
///
/// Absent on a machine managed by WSUS or Intune, and absent on a fresh
/// install, so "unknown" here is unremarkable rather than suspicious.
fn last_windows_update() -> String {
    reg_string(
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\Results\Install",
        "LastSuccessTime",
    )
    .unwrap_or_else(|| "unknown".to_string())
}

/// Whether Defender's real-time protection is on, as far as we can tell.
///
/// Real-time protection is worth knowing about because on-access scanning
/// delays process start-up and window creation, which is exactly the timing
/// SuperTile depends on. But tamper protection puts this key out of reach of an
/// unelevated process, so the honest answer is usually "unknown" — and it stays
/// "unknown" rather than being guessed, because a wrong "on" would send a
/// maintainer looking in the wrong place. Shelling out to PowerShell to get a
/// definite answer would cost a visible console flash and a second of latency
/// for a field that is only ever a hint.
fn defender_status() -> String {
    match reg_dword(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Microsoft\Windows Defender\Real-Time Protection",
        "DisableRealtimeMonitoring",
    ) {
        Some(0) => "on".to_string(),
        Some(_) => "off".to_string(),
        None => "unknown".to_string(),
    }
}

fn cpu_model() -> String {
    reg_string(
        r"HARDWARE\DESCRIPTION\System\CentralProcessor\0",
        "ProcessorNameString",
    )
    .unwrap_or_else(|| "unknown".to_string())
}

// --- system information ------------------------------------------------------

/// Native architecture and logical core count.
///
/// `GetNativeSystemInfo` rather than `GetSystemInfo` so that an x86 build
/// running under WOW64 still reports the machine it is on, not the machine
/// Windows is pretending to be.
fn architecture_and_cores() -> (String, u32) {
    let mut info = SYSTEM_INFO::default();
    // SAFETY: the struct is fully initialised (zeroed by Default) and
    // GetNativeSystemInfo only writes to it. It cannot fail.
    unsafe { GetNativeSystemInfo(&mut info) };
    // SAFETY: reading the union's struct arm is what the documented layout
    // says is there; the alternative arm is a u32 of the same size, so no
    // read is out of bounds whichever arm the OS wrote.
    let arch = unsafe { info.Anonymous.Anonymous.wProcessorArchitecture };
    let name = match arch {
        PROCESSOR_ARCHITECTURE_AMD64 => "x64",
        PROCESSOR_ARCHITECTURE_ARM64 => "ARM64",
        PROCESSOR_ARCHITECTURE_INTEL => "x86",
        PROCESSOR_ARCHITECTURE_ARM => "ARM",
        PROCESSOR_ARCHITECTURE_IA64 => "IA64",
        _ => "unknown",
    };
    (name.to_string(), info.dwNumberOfProcessors)
}

/// Total physical memory in GB and how much of it is in use.
///
/// Memory pressure is worth reporting because Windows starts trimming working
/// sets under it, and a trimmed process is slow to redraw after a retile.
fn memory() -> (f64, u32) {
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: dwLength declares the struct size, which is how the API knows
    // which version of the layout it was handed. Failure leaves the rest as
    // the zeroes it was initialised with.
    let ok = unsafe { GlobalMemoryStatusEx(&mut status) }.is_ok();
    if !ok {
        return (0.0, 0);
    }
    (to_gb(status.ullTotalPhys), status.dwMemoryLoad.min(100))
}

fn tick_count() -> u64 {
    // SAFETY: GetTickCount64 takes no arguments and cannot fail.
    unsafe { GetTickCount64() }
}

/// The fixed disks, with their sizes.
///
/// Removable and network drives are skipped: a mounted phone or a corporate
/// share tells a maintainer nothing about the machine, and the share name would
/// leak the organisation.
fn fixed_disks() -> Vec<DiskInfo> {
    let mut out = Vec::new();
    let mut buf = [0u16; 512];
    // SAFETY: the slice hands the API both the pointer and its true length, so
    // it cannot write past the end. A zero return means failure.
    let len = unsafe { GetLogicalDriveStringsW(Some(&mut buf)) } as usize;
    if len == 0 || len > buf.len() {
        return out;
    }
    // The result is a run of NUL-terminated roots ("C:\\\0D:\\\0\0").
    for root in buf[..len].split(|&c| c == 0).filter(|s| !s.is_empty()) {
        let mut root_z = root.to_vec();
        root_z.push(0);
        let path = PCWSTR(root_z.as_ptr());
        // SAFETY: `root_z` is NUL-terminated and outlives both calls below.
        let kind = unsafe { GetDriveTypeW(path) };
        if kind != DRIVE_FIXED {
            continue;
        }
        let mut total = 0u64;
        let mut free = 0u64;
        // SAFETY: `root_z` outlives the call and both out-params are valid
        // u64s. On failure they keep the zeroes they hold now, and the entry
        // is dropped rather than reported as an empty disk.
        let ok = unsafe { GetDiskFreeSpaceExW(path, None, Some(&mut total), Some(&mut free)) };
        if ok.is_err() || total == 0 {
            continue;
        }
        let letter = wide_to_string(root)
            .chars()
            .next()
            .unwrap_or('?')
            .to_ascii_uppercase();
        out.push(DiskInfo {
            letter,
            total_gb: to_gb(total),
            free_gb: to_gb(free),
        });
    }
    out
}

/// The displays, numbered from 1 in the order [`crate::monitor::enumerate`]
/// returns them — which is sorted by position, so display 1 is the top-left one
/// and the numbering matches what the user sees in Windows' display settings
/// closely enough to talk about.
fn displays() -> Vec<DisplayInfo> {
    crate::monitor::enumerate()
        .into_iter()
        .enumerate()
        .map(|(i, m)| DisplayInfo {
            index: i + 1,
            width: m.bounds.width(),
            height: m.bounds.height(),
            work_width: m.work_area.width(),
            work_height: m.work_area.height(),
            dpi: m.dpi,
            primary: m.is_primary,
        })
        .collect()
}

/// Every running process, by file name and count.
///
/// Only the file name is taken, never the full path or the command line: the
/// name is what identifies a suspect ("some overlay is stealing focus"), and
/// the rest is where the identifying detail lives. SuperTile itself is left out
/// because its presence is not news.
fn other_processes() -> BTreeMap<String, u32> {
    let mut out: BTreeMap<String, u32> = BTreeMap::new();
    // SAFETY: CreateToolhelp32Snapshot takes no pointers; it either yields a
    // handle we own or an error, and the handle is closed below.
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return out;
    };

    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: `snapshot` is live, and dwSize declares the struct size so the
    // API knows how much of `entry` it may fill.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();
    while more {
        let name = wide_to_string(&entry.szExeFile);
        if !name.is_empty() && !name.eq_ignore_ascii_case("supertile.exe") {
            *out.entry(name).or_insert(0) += 1;
        }
        // SAFETY: same handle and same struct as the successful call above;
        // the walk ends when the API reports there is nothing further.
        more = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
    }

    // SAFETY: `snapshot` came from CreateToolhelp32Snapshot, is not used after
    // this point, and is closed exactly once.
    unsafe {
        let _ = CloseHandle(snapshot);
    }
    out
}

/// The end of the debug log, unredacted.
///
/// Redaction happens in [`render`], not here, so that there is exactly one
/// place where the rule lives and no way to render a tail that skipped it.
fn log_tail() -> Vec<String> {
    use std::io::{Read as _, Seek as _, SeekFrom};

    let Ok(dir) = crate::util::data_dir() else {
        return Vec::new();
    };
    let Ok(mut file) = std::fs::File::open(dir.join("supertile.log")) else {
        return Vec::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let from = len.saturating_sub(LOG_TAIL_BYTES);
    if file.seek(SeekFrom::Start(from)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    // Lossy, because a log truncated mid-character by the seek must not lose
    // the whole tail over one bad byte.
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<&str> = text.lines().collect();
    // Seeking into the middle of the file lands mid-line; that fragment would
    // read as a corrupt log entry, so drop it.
    if from > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    let start = lines.len().saturating_sub(LOG_TAIL_LINES);
    lines[start..].iter().map(|s| s.to_string()).collect()
}

// --- pure helpers -------------------------------------------------------------

/// Bytes as binary gigabytes.
///
/// Binary rather than decimal so the figure matches what File Explorer shows
/// the user; a report that disagrees with the machine's own UI invites an
/// argument about the wrong thing.
fn to_gb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0 * 1024.0)
}

fn plural(n: u64, unit: &str) -> String {
    if n == 1 {
        format!("{n} {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

/// Uptime in the coarsest terms that are still informative.
///
/// Nobody needs the seconds. What a maintainer wants to know is whether the
/// machine was rebooted this morning or has been up for three weeks, because
/// the second case is where handle leaks and stale shell state live.
fn format_uptime(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, hours, minutes) = (secs / 86_400, (secs % 86_400) / 3600, (secs % 3600) / 60);
    if days > 0 {
        format!("{}, {}", plural(days, "day"), plural(hours, "hour"))
    } else if hours > 0 {
        format!("{}, {}", plural(hours, "hour"), plural(minutes, "minute"))
    } else if minutes > 0 {
        plural(minutes, "minute")
    } else {
        "less than a minute".to_string()
    }
}

/// A wall-clock instant split into the fields Windows reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Clock {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
///
/// The arithmetic is done here rather than through `SystemTimeToFileTime` and
/// back because the calculation is the only part that can be wrong, and doing
/// it in plain Rust makes it testable without a clock.
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse of [`days_from_civil`].
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    ((if m <= 2 { y + 1 } else { y }) as i32, m as u32, d as u32)
}

/// When the machine last started, as `YYYY-MM-DD HH:MM`.
///
/// Derived by subtracting the uptime from the current local time rather than
/// read from an event log, which keeps it to two cheap calls. The consequence
/// is that a boot across a daylight-saving transition is reported an hour out;
/// that is an acceptable error for a field whose purpose is to say "yesterday
/// evening" rather than "at 19:43:07".
fn last_boot_from(now: Clock, uptime_secs: u64) -> String {
    let now_secs = days_from_civil(now.year, now.month, now.day) * 86_400
        + (now.hour * 3600 + now.minute * 60 + now.second) as i64;
    let then = now_secs - uptime_secs as i64;
    // Rust's `%` keeps the sign of the dividend, which would put a pre-1970
    // instant in the wrong day. `rem_euclid` gives the mathematical remainder.
    let (days, rem) = (then.div_euclid(86_400), then.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60
    )
}

fn last_boot(uptime_ms: u64) -> String {
    // SAFETY: GetLocalTime only writes to the SYSTEMTIME it returns.
    let t = unsafe { GetLocalTime() };
    let now = Clock {
        year: t.wYear as i32,
        month: t.wMonth as u32,
        day: t.wDay as u32,
        hour: t.wHour as u32,
        minute: t.wMinute as u32,
        second: t.wSecond as u32,
    };
    last_boot_from(now, uptime_ms / 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_keeps_only_its_file_name() {
        assert_eq!(
            anonymise_path(r"C:\Users\Andreas\AppData\Local\WhatsApp\WhatsApp.exe"),
            r"*\*\*\WhatsApp.exe"
        );
        assert_eq!(
            anonymise_path(r"C:\Program Files\Signal\Signal.exe"),
            r"*\*\*\Signal.exe"
        );
    }

    #[test]
    fn the_star_count_does_not_leak_directory_depth() {
        // Two very different depths must be indistinguishable.
        let shallow = anonymise_path(r"C:\a.exe");
        let deep = anonymise_path(r"C:\one\two\three\four\five\six\a.exe");
        assert_eq!(shallow, deep);
    }

    #[test]
    fn no_anonymised_path_retains_a_user_name() {
        for p in [
            r"C:\Users\Andreas\app.exe",
            r"\\server\home\andreas\tools\app.exe",
            r"C:/Users/Andreas/app.exe",
        ] {
            let a = anonymise_path(p);
            assert!(
                !a.to_lowercase().contains("andreas"),
                "{a} still names the user"
            );
            assert!(!a.to_lowercase().contains("users"), "{a} leaks a directory");
        }
    }

    #[test]
    fn a_pathless_or_empty_name_does_not_panic() {
        assert_eq!(anonymise_path(""), r"*\*\*\(unknown)");
        assert_eq!(anonymise_path("app.exe"), r"*\*\*\app.exe");
        assert_eq!(anonymise_path(r"C:\dir\"), r"*\*\*\dir");
    }

    #[test]
    fn location_is_classified_without_naming_anything() {
        assert_eq!(
            path_class(r"C:\Program Files\Signal\Signal.exe"),
            "Program Files"
        );
        assert_eq!(
            path_class(r"C:\Program Files (x86)\Steam\steam.exe"),
            "Program Files (x86)"
        );
        assert_eq!(
            path_class(r"C:\Users\Andreas\AppData\x.exe"),
            "user profile"
        );
        assert_eq!(path_class(r"C:\Windows\explorer.exe"), "Windows");
        assert_eq!(
            path_class(r"C:\Program Files\WindowsApps\Foo\foo.exe"),
            "Store app"
        );
        assert_eq!(path_class(""), "unknown");
        assert_eq!(path_class(r"D:\games\game.exe"), "other");
    }

    #[test]
    fn the_redactor_is_case_insensitive() {
        let r = Redactor::new("Andreas", "DESKTOP-7X2");
        let out = r.apply(r"C:\Users\ANDREAS on desktop-7x2, user Andreas");
        assert!(!out.to_lowercase().contains("andreas"), "{out}");
        assert!(!out.to_lowercase().contains("desktop-7x2"), "{out}");
        assert_eq!(out.matches("<user>").count(), 2);
        assert_eq!(out.matches("<machine>").count(), 1);
    }

    #[test]
    fn a_machine_named_after_its_owner_is_fully_redacted() {
        // The longer term must win, or "Andreas-PC" leaves "-PC" behind
        // attached to a <user> marker.
        let r = Redactor::new("Andreas", "Andreas-PC");
        let out = r.apply("host Andreas-PC");
        assert_eq!(out, "host <machine>");
    }

    #[test]
    fn very_short_names_are_not_used_as_needles() {
        // A two-letter user name would otherwise redact half the report.
        let r = Redactor::new("al", "pc");
        assert_eq!(r.apply("the alarm call on pcs"), "the alarm call on pcs");
    }

    #[test]
    fn redaction_survives_non_ascii_text() {
        let r = Redactor::new("Andreas", "Wirén-PC");
        let out = r.apply("Användaren Andreas på Wirén-PC");
        assert!(!out.contains("Andreas"));
        assert!(out.contains("Användaren"), "unrelated text must survive");
    }

    #[test]
    fn an_empty_redactor_changes_nothing() {
        let r = Redactor::default();
        assert_eq!(r.apply("anything at all"), "anything at all");
    }

    #[test]
    fn quoted_titles_are_removed_from_log_lines() {
        assert_eq!(
            redact_log_line("'Quarterly results.xlsx - Excel' (excel.exe) will not accept a size"),
            "'<title>' (excel.exe) will not accept a size"
        );
    }

    #[test]
    fn every_quoted_run_in_a_line_is_removed() {
        let out = redact_log_line("swapped 'Secret Plan' with 'Other Doc' on DISPLAY1");
        assert!(!out.contains("Secret"), "{out}");
        assert!(!out.contains("Other Doc"), "{out}");
        assert_eq!(out.matches("<title>").count(), 2);
        assert!(out.contains("DISPLAY1"), "the useful part must survive");
    }

    #[test]
    fn an_unterminated_quote_redacts_to_end_of_line() {
        // Losing a diagnostic beats leaking a title.
        let out = redact_log_line("opening 'A Very Private Document");
        assert!(!out.contains("Private"), "{out}");
    }

    #[test]
    fn a_line_without_quotes_is_untouched() {
        let line = "retile on DISPLAY1: 4 windows, grid layout";
        assert_eq!(redact_log_line(line), line);
    }

    fn sample() -> Report {
        Report {
            app_version: "9.9.9".into(),
            os_name: "Windows 11 Pro".into(),
            os_build: "26200.1000".into(),
            os_arch: "x64".into(),
            uptime: "3 days, 2 hours".into(),
            last_boot: "2026-08-15 07:12".into(),
            last_update: "2026-08-12".into(),
            defender: "on".into(),
            cpu: "Some CPU".into(),
            cpu_cores: 16,
            ram_total_gb: 64.0,
            ram_used_percent: 41,
            disks: vec![DiskInfo {
                letter: 'C',
                total_gb: 1000.0,
                free_gb: 250.0,
            }],
            displays: vec![DisplayInfo {
                index: 1,
                width: 3840,
                height: 2160,
                work_width: 3840,
                work_height: 2112,
                dpi: 144,
                primary: true,
            }],
            layout: "Grid".into(),
            settings: vec![("Auto-tile".into(), "on".into())],
            hotkey_conflicts: vec!["palette: Win+Alt+D taken".into()],
            windows: vec![WindowRow {
                exe: r"C:\Users\Andreas\AppData\Local\WhatsApp\WhatsApp.exe".into(),
                class: "Chrome_WidgetWin_1".into(),
                width: 800,
                height: 600,
                min_width: 620,
                min_height: 400,
                state: "tiled",
            }],
            other_processes: BTreeMap::from([("explorer.exe".to_string(), 1)]),
            log_tail: vec!["'Private Document.docx' (winword.exe) missed its zone".into()],
            logging_enabled: true,
            verbose: true,
        }
    }

    #[test]
    fn the_rendered_report_names_neither_the_user_nor_the_machine() {
        let red = Redactor::new("Andreas", "DESKTOP-7X2");
        let out = render(&sample(), &red).to_lowercase();
        assert!(!out.contains("andreas"), "the user name reached the report");
        assert!(!out.contains("desktop-7x2"));
        assert!(!out.contains(r"c:\users"), "a real path reached the report");
        assert!(!out.contains("appdata"));
    }

    #[test]
    fn the_rendered_report_contains_no_window_title() {
        let red = Redactor::new("Andreas", "DESKTOP-7X2");
        let out = render(&sample(), &red);
        assert!(
            !out.contains("Private Document"),
            "a title survived into the report"
        );
        assert!(out.contains("<title>"));
        assert!(
            out.contains("winword.exe"),
            "the useful part of the line must survive"
        );
    }

    #[test]
    fn the_rendered_report_keeps_what_a_maintainer_needs() {
        let red = Redactor::new("Andreas", "DESKTOP-7X2");
        let out = render(&sample(), &red);
        for needed in [
            "9.9.9",
            "Windows 11 Pro",
            "26200.1000",
            "3840x2160",
            "150%",
            r"*\*\*\WhatsApp.exe",
            "620x400",
            "explorer.exe",
        ] {
            assert!(out.contains(needed), "report lost {needed}");
        }
    }

    #[test]
    fn the_report_states_what_it_withheld() {
        let red = Redactor::default();
        let out = render(&sample(), &red);
        for e in EXCLUSIONS {
            assert!(out.contains(e), "report omits its own disclosure: {e}");
        }
    }

    #[test]
    fn a_report_with_logging_off_says_how_to_turn_it_on() {
        let mut r = sample();
        r.logging_enabled = false;
        let out = render(&r, &Redactor::default());
        assert!(out.contains("Diagnostics"), "{out}");
        assert!(!out.contains("<title>"), "no log tail should be rendered");
    }

    #[test]
    fn an_empty_report_renders_without_panicking() {
        let out = render(&Report::default(), &Redactor::default());
        assert!(out.contains("SuperTile issue report"));
    }

    #[test]
    fn the_disclosure_and_the_exclusions_are_not_empty() {
        // These are what the user is shown before consenting; a silent empty
        // list would be a consent dialog that discloses nothing.
        assert!(DISCLOSURE.len() >= 5);
        assert!(EXCLUSIONS.len() >= 3);
        for (k, v) in DISCLOSURE {
            assert!(!k.is_empty() && !v.is_empty());
        }
    }

    // --- pure collection helpers ------------------------------------------

    #[test]
    fn bytes_convert_to_binary_gigabytes() {
        assert_eq!(to_gb(0), 0.0);
        assert_eq!(to_gb(1024 * 1024 * 1024), 1.0);
        assert_eq!(to_gb(64 * 1024 * 1024 * 1024), 64.0);
        // Explorer calls a 500 GB disk 465 GB; the report must agree with it.
        assert_eq!(format!("{:.0}", to_gb(500_107_862_016)), "466");
    }

    #[test]
    fn the_largest_plausible_disk_does_not_overflow() {
        assert!(to_gb(u64::MAX).is_finite());
    }

    #[test]
    fn uptime_is_reported_in_the_coarsest_useful_unit() {
        assert_eq!(format_uptime(0), "less than a minute");
        assert_eq!(format_uptime(59_000), "less than a minute");
        assert_eq!(format_uptime(60_000), "1 minute");
        assert_eq!(format_uptime(90 * 60 * 1000), "1 hour, 30 minutes");
        assert_eq!(
            format_uptime((3 * 86_400 + 2 * 3600) * 1000),
            "3 days, 2 hours"
        );
        assert_eq!(format_uptime(86_400 * 1000), "1 day, 0 hours");
    }

    fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> Clock {
        Clock {
            year,
            month,
            day,
            hour,
            minute,
            second,
        }
    }

    #[test]
    fn civil_dates_round_trip_through_day_numbers() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1970, 1, 2), 1);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        for (y, m, d) in [
            (1900, 3, 1),
            (1970, 1, 1),
            (2000, 2, 29),
            (2024, 2, 29),
            (2026, 8, 19),
            (2100, 12, 31),
        ] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
    }

    #[test]
    fn the_boot_time_is_the_current_time_less_the_uptime() {
        // Two hours and ten minutes before a fixed afternoon.
        assert_eq!(
            last_boot_from(at(2026, 8, 19, 14, 30, 0), 2 * 3600 + 10 * 60),
            "2026-08-19 12:20"
        );
    }

    #[test]
    fn a_boot_before_midnight_lands_on_the_previous_day() {
        assert_eq!(
            last_boot_from(at(2026, 1, 1, 0, 30, 0), 3600),
            "2025-12-31 23:30"
        );
    }

    #[test]
    fn a_boot_across_a_leap_day_keeps_the_calendar_straight() {
        assert_eq!(
            last_boot_from(at(2024, 3, 1, 9, 0, 0), 86_400),
            "2024-02-29 09:00"
        );
    }

    #[test]
    fn an_absurd_uptime_does_not_panic() {
        // GetTickCount64 wraps after 585 million years, but a virtual machine
        // restored from a snapshot can report nonsense long before that.
        let _ = last_boot_from(at(2026, 8, 19, 14, 30, 0), u64::MAX);
        let _ = format_uptime(u64::MAX);
    }

    // --- live-system checks -----------------------------------------------
    // Collection must be total: these assert that every field arrives, not what
    // it says, because what it says depends on the machine running the tests.

    #[test]
    fn collection_returns_a_fully_populated_report() {
        let r = collect(
            "9.9.9",
            "Grid",
            vec![("Auto-tile".into(), "on".into())],
            Vec::new(),
            Vec::new(),
            false,
            false,
        );
        assert_eq!(r.app_version, "9.9.9");
        assert_eq!(r.layout, "Grid");
        for (field, value) in [
            ("os_name", &r.os_name),
            ("os_build", &r.os_build),
            ("os_arch", &r.os_arch),
            ("uptime", &r.uptime),
            ("last_boot", &r.last_boot),
            ("last_update", &r.last_update),
            ("defender", &r.defender),
            ("cpu", &r.cpu),
        ] {
            assert!(!value.is_empty(), "{field} was left blank");
        }
        assert!(
            ["on", "off", "unknown"].contains(&r.defender.as_str()),
            "defender must never be guessed: {}",
            r.defender
        );
        assert!(r.cpu_cores >= 1, "a running machine has a core");
        assert!(r.ram_total_gb > 0.0, "a running machine has memory");
        assert!(r.ram_used_percent <= 100);
        assert!(!r.displays.is_empty(), "a desktop session has a display");
        assert_eq!(r.displays[0].index, 1, "displays are numbered from one");
    }

    #[test]
    fn the_boot_time_and_uptime_agree_on_the_shape_of_a_date() {
        let r = collect("0", "", Vec::new(), Vec::new(), Vec::new(), false, false);
        assert_eq!(r.last_boot.len(), 16, "expected YYYY-MM-DD HH:MM");
        assert!(
            r.uptime.contains("minute") || r.uptime.contains("hour") || r.uptime.contains("day")
        );
    }

    #[test]
    fn the_process_list_omits_supertile_and_names_no_paths() {
        let procs = other_processes();
        for (name, count) in &procs {
            assert!(
                !name.contains('\\'),
                "a path reached the process list: {name}"
            );
            assert!(
                !name.eq_ignore_ascii_case("supertile.exe"),
                "our own process should not be reported"
            );
            assert!(*count >= 1);
        }
    }

    #[test]
    fn every_reported_disk_is_plausible() {
        for d in fixed_disks() {
            assert!(d.letter.is_ascii_alphabetic(), "{:?}", d.letter);
            assert!(d.total_gb > 0.0);
            assert!(d.free_gb <= d.total_gb);
        }
    }

    #[test]
    fn the_log_tail_is_bounded() {
        assert!(log_tail().len() <= LOG_TAIL_LINES);
    }

    #[test]
    fn a_report_with_logging_off_collects_no_log() {
        let r = collect("0", "", Vec::new(), Vec::new(), Vec::new(), false, false);
        assert!(r.log_tail.is_empty());
    }

    #[test]
    fn the_live_report_renders_without_naming_the_user() {
        let red = current_redactor();
        let r = collect(
            "9.9.9",
            "Grid",
            Vec::new(),
            Vec::new(),
            Vec::new(),
            true,
            true,
        );
        let out = render(&r, &red).to_lowercase();
        if let Ok(user) = std::env::var("USERNAME") {
            if user.len() > 2 {
                assert!(!out.contains(&user.to_lowercase()), "the user name leaked");
            }
        }
        assert!(out.contains("supertile issue report"));
    }
}

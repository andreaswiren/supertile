//! Checking GitHub for a newer release.
//!
//! SuperTile has no installer, no telemetry and no phone-home channel, so the
//! only way a user learns that the build they are running has been superseded
//! is by looking. This
//! module does the looking: one HTTPS `GET` against the GitHub releases API,
//! parsed into a version, a link and the release notes.
//!
//! ## Privacy
//!
//! An update check is a network request to a third party, and that is not free.
//! Contacting `api.github.com` reveals this machine's IP address to GitHub (and
//! to Microsoft, who own it), together with the fact that SuperTile is
//! installed and — because the version travels in the `User-Agent` header —
//! which version it is. GitHub logs requests to its API; SuperTile has no say
//! in what is retained or for how long. Roughly, an IP address plus a coarse
//! timestamp plus "runs SuperTile, this version, on Windows" enters GitHub's
//! logs each time a check runs.
//!
//! Nothing else is sent. There is no identifier, no install ID, no machine or
//! user name, no window titles, no counters, and nothing is uploaded — the
//! request carries no body at all, and the reply is read and discarded. The
//! request is a plain unauthenticated `GET` of a public endpoint, the same one
//! a browser would fetch.
//!
//! **The check does not run unless it is switched on.** The default is off, and
//! it stays off until the user asks for it; that decision lives in the
//! configuration rather than here. Nothing in this module runs by itself.
//! Someone who would rather not talk to GitHub at all can simply leave it
//! alone and watch the releases page instead.
//!
//! ## Why WinHTTP rather than an HTTP crate
//!
//! One JSON `GET` does not justify a dependency tree. Every crate added here
//! lands in the SBOM and the licence audit and has to be tracked for
//! advisories for as long as SuperTile ships. WinHTTP is already on every
//! Windows machine, honours the system proxy configuration, and validates
//! certificates against the OS trust store, which is the store the user
//! actually manages.
//!
//! ## Failure is ordinary
//!
//! Being offline is the normal state of a laptop on a train, not a fault. Every
//! failure path ends in [`Outcome::Failed`] carrying a short sentence, and it is
//! the caller's job to be quiet about it: a background check that cannot reach
//! GitHub should leave no trace beyond the log.

use std::ffi::c_void;

use windows::core::PCWSTR;
use windows::Win32::Foundation::GetLastError;
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
    ERROR_WINHTTP_CANNOT_CONNECT, ERROR_WINHTTP_CONNECTION_ERROR, ERROR_WINHTTP_NAME_NOT_RESOLVED,
    ERROR_WINHTTP_SECURE_FAILURE, ERROR_WINHTTP_TIMEOUT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
    WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};

use crate::util::WideStr;

/// The releases endpoint, split into the parts WinHTTP wants separately.
const HOST: &str = "api.github.com";
const PATH: &str = "/repos/andreaswiren/supertile/releases/latest";
const HTTPS_PORT: u16 = 443;

/// Anything GitHub sends that does not start with this prefix is discarded in
/// favour of the repository page. The URL ends up in `ShellExecuteW`, so it is
/// worth insisting that it points where we think it does even though the
/// source is GitHub's own API.
const TRUSTED_URL_PREFIX: &str = "https://github.com/andreaswiren/supertile/";

/// The published binary, and the digest published beside it.
const ASSET_EXE: &str = "supertile.exe";
const ASSET_SHA: &str = "supertile.exe.sha256";

/// Ceiling on a downloaded binary. The real one is around a megabyte; this is
/// loose enough not to need revising every release and tight enough that a
/// redirect to something enormous is refused rather than buffered.
const MAX_DOWNLOAD_BYTES: usize = 32 * 1024 * 1024;

/// Timeouts in milliseconds. Generous, because a slow hotel network is not a
/// failure, but finite, because the calling thread is waiting on this and a
/// check that never returns is a leaked thread.
const RESOLVE_TIMEOUT_MS: i32 = 10_000;
const CONNECT_TIMEOUT_MS: i32 = 10_000;
const SEND_TIMEOUT_MS: i32 = 10_000;
const RECEIVE_TIMEOUT_MS: i32 = 15_000;

/// Hard ceiling on the reply. A release payload is a few kilobytes; half a
/// megabyte is room to spare. The point is that a hostile or broken server
/// cannot make SuperTile allocate until it dies.
const MAX_BODY_BYTES: usize = 512 * 1024;

/// Size of each `WinHttpReadData` chunk.
const READ_CHUNK_BYTES: usize = 16 * 1024;

/// A megabyte over a slow link takes longer than a version check does.
const DOWNLOAD_RECEIVE_TIMEOUT_MS: i32 = 120_000;

/// Release notes are shown in a dialog, not archived, so they are clipped.
const MAX_NOTES_CHARS: usize = 2000;

/// What a check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// GitHub answered, and the published release is not newer than this build.
    UpToDate,
    /// A newer release exists. `notes` may be empty; GitHub allows it.
    Available {
        version: String,
        url: String,
        notes: String,
        /// Direct download for the published binary, when the release has one.
        exe_url: Option<String>,
        /// Its `.sha256` companion. Without it nothing is installed: an
        /// executable is not run on the strength of having arrived.
        sha_url: Option<String>,
    },
    /// The check did not complete. The string is one short sentence fit to show
    /// a user, or to drop in the log and otherwise ignore.
    Failed(String),
}

/// The three fields worth reading out of a release object.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Release {
    version: String,
    url: String,
    notes: String,
    exe_url: Option<String>,
    sha_url: Option<String>,
}

/// Fetch the latest release. Blocking; call it off the UI thread.
///
/// This makes a network request, so it must never run unless the user has
/// asked for update checks. See the module documentation for what GitHub
/// learns when it does.
pub fn check_latest() -> Outcome {
    let body = match fetch_latest_release() {
        Ok(body) => body,
        Err(reason) => return Outcome::Failed(reason),
    };
    let release = match parse_release(&body) {
        Ok(release) => release,
        Err(reason) => return Outcome::Failed(reason),
    };
    if is_newer(crate::APP_VERSION, &release.version) {
        Outcome::Available {
            version: release.version,
            url: release.url,
            notes: release.notes,
            exe_url: release.exe_url,
            sha_url: release.sha_url,
        }
    } else {
        Outcome::UpToDate
    }
}

/// Compare two SemVer strings. `true` when `candidate` is newer than `current`.
///
/// Comparing the strings directly would order `0.10.0` before `0.9.0`, which is
/// exactly the mistake that makes an update checker tell people to downgrade,
/// so the components are parsed as numbers. A leading `v` is tolerated on
/// either side because GitHub tags carry one and the crate version does not.
///
/// Pre-release and build suffixes are ignored rather than ordered: getting
/// `1.0.0-rc.1 < 1.0.0` right needs the full SemVer precedence rules, and the
/// cost of a wrong answer — nagging somebody to "upgrade" to a release
/// candidate — is worse than the cost of treating the two as equal. SuperTile
/// does not publish pre-releases in any case.
///
/// Anything that fails to parse means "not newer". An update checker that
/// cannot read the answer must stay silent, never nag on garbage.
pub fn is_newer(current: &str, candidate: &str) -> bool {
    match (parse_semver(current), parse_semver(candidate)) {
        (Some(now), Some(new)) => new > now,
        _ => false,
    }
}

/// Should an automatic check run now, given when the last one happened?
///
/// `last_checked_unix` is a Unix timestamp in seconds, with `0` meaning "never
/// checked" — the natural default for a fresh configuration, and one that makes
/// the first check happen at the first opportunity.
///
/// A clock that has gone backwards (a stored timestamp in the future, which
/// happens after a timezone fumble, a dead CMOS battery or a restored disk
/// image) counts as due. The alternative is to wait for real time to catch up,
/// which could mean never checking again — a silent failure is worse than one
/// extra request.
///
/// An interval of zero means every opportunity, which is only sensible in a
/// Seconds since the Unix epoch.
///
/// Zero if the system clock is before 1970, which is not a real time but is a
/// value the machine can hold. Treating it as "never checked" is harmless; the
/// alternative is a panic in a background task nobody asked for.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// test; the configuration should not offer it.
pub fn due(last_checked_unix: u64, now_unix: u64, interval_hours: u64) -> bool {
    if last_checked_unix == 0 || now_unix < last_checked_unix {
        return true;
    }
    let interval_secs = interval_hours.saturating_mul(3600);
    now_unix - last_checked_unix >= interval_secs
}

/// Split `major.minor.patch` into numbers, or `None` if it is not that shape.
///
/// Deliberately strict: three components, all decimal, nothing trailing beyond
/// a `-` pre-release or `+` build suffix. Loose parsing here would turn a
/// malformed tag into a confident wrong comparison.
fn parse_semver(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim();
    let s = s
        .strip_prefix('v')
        .or_else(|| s.strip_prefix('V'))
        .unwrap_or(s);
    // Everything from the first `-` or `+` onwards is a pre-release or build
    // suffix and plays no part in the comparison.
    let core = s.split(['-', '+']).next()?;

    let mut parts = core.split('.');
    let major = parse_component(parts.next()?)?;
    let minor = parse_component(parts.next()?)?;
    let patch = parse_component(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// One version component: bare ASCII digits, nothing else.
///
/// `u64::from_str` accepts a leading `+`, which would let `1.+2.3` through, so
/// the digits are checked explicitly.
fn parse_component(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Pull the version, link and notes out of a GitHub release object.
///
/// Kept separate from the network so it can be tested against a captured
/// payload without touching the wire.
fn parse_release(body: &str) -> Result<Release, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|_| "GitHub's reply was not valid JSON".to_string())?;

    let tag = value
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "GitHub's reply had no release tag".to_string())?
        .trim();
    let version = tag.strip_prefix('v').unwrap_or(tag).to_string();
    if version.is_empty() {
        return Err("GitHub's reply had an empty release tag".to_string());
    }

    // A URL that is not on the project's own repository is not followed: it
    // would be opened in the user's browser on a single click.
    let url = value
        .get("html_url")
        .and_then(serde_json::Value::as_str)
        .filter(|u| u.starts_with(TRUSTED_URL_PREFIX))
        .unwrap_or(crate::APP_REPO)
        .to_string();

    let notes = value
        .get("body")
        .and_then(serde_json::Value::as_str)
        .map(|b| clamp_notes(b.trim()))
        .unwrap_or_default();

    // Asset URLs, held to the same rule as the release page: on this
    // repository or not at all. One of them names a file that will be written
    // over the running executable, so where it came from is the whole of the
    // security argument.
    let asset = |want: &str| -> Option<String> {
        value
            .get("assets")?
            .as_array()?
            .iter()
            .find(|a| a.get("name").and_then(serde_json::Value::as_str) == Some(want))?
            .get("browser_download_url")?
            .as_str()
            .filter(|u| u.starts_with(TRUSTED_URL_PREFIX))
            .map(str::to_string)
    };
    let exe_url = asset(ASSET_EXE);
    let sha_url = asset(ASSET_SHA);

    Ok(Release {
        version,
        url,
        notes,
        exe_url,
        sha_url,
    })
}

/// Clip release notes to something a dialog can hold, on a character boundary.
fn clamp_notes(notes: &str) -> String {
    if notes.len() <= MAX_NOTES_CHARS {
        return notes.to_string();
    }
    let mut end = MAX_NOTES_CHARS;
    while end > 0 && !notes.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &notes[..end])
}

/// A WinHTTP handle that closes itself.
///
/// The fetch below has half a dozen early returns; closing by hand at each one
/// is how handles get leaked. Drop order gives the right sequence for free —
/// locals unwind in reverse declaration order, so the request closes before the
/// connection and the connection before the session.
struct WinHttpHandle(*mut c_void);

impl Drop for WinHttpHandle {
    fn drop(&mut self) {
        if self.0.is_null() {
            return;
        }
        // SAFETY: the pointer came from a WinHTTP open/connect call that
        // returned non-null, it has not been closed elsewhere (this type is the
        // only owner and is neither `Copy` nor `Clone`), and `drop` runs once.
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

impl WinHttpHandle {
    fn raw(&self) -> *mut c_void {
        self.0
    }
}

/// Turn the thread's last WinHTTP error into a sentence a person can read.
fn last_error_message() -> String {
    // SAFETY: `GetLastError` takes no arguments and only reads the calling
    // thread's error slot; there is nothing to get wrong.
    let code = unsafe { GetLastError() }.0;
    match code {
        ERROR_WINHTTP_NAME_NOT_RESOLVED => "could not find api.github.com (offline?)".to_string(),
        ERROR_WINHTTP_CANNOT_CONNECT => "could not reach GitHub".to_string(),
        ERROR_WINHTTP_CONNECTION_ERROR => "the connection to GitHub dropped".to_string(),
        ERROR_WINHTTP_TIMEOUT => "GitHub did not answer in time".to_string(),
        ERROR_WINHTTP_SECURE_FAILURE => {
            "the secure connection to GitHub could not be trusted".to_string()
        }
        other => format!("network error {other}"),
    }
}

/// Perform the `GET` and return the response body as text.
fn fetch_latest_release() -> Result<String, String> {
    // The agent string is set once on the session. WinHTTP turns it into the
    // `User-Agent` header on every request, which GitHub's API insists on;
    // repeating it in the per-request headers below risks sending it twice.
    let agent = WideStr::new(&format!("supertile/{}", crate::APP_VERSION));

    // SAFETY: `agent` outlives the call, the two proxy arguments are documented
    // as WINHTTP_NO_PROXY_NAME/BYPASS (null) and required to be null for the
    // automatic access type, and no flags means synchronous mode, which is what
    // every call below assumes.
    let session = unsafe {
        WinHttpOpen(
            agent.as_pcwstr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        )
    };
    let session = WinHttpHandle(session);
    if session.raw().is_null() {
        return Err(format!(
            "could not start the check: {}",
            last_error_message()
        ));
    }

    // SAFETY: `session` is a live session handle; the four timeouts are
    // milliseconds and the call has no other preconditions.
    unsafe {
        WinHttpSetTimeouts(
            session.raw(),
            RESOLVE_TIMEOUT_MS,
            CONNECT_TIMEOUT_MS,
            SEND_TIMEOUT_MS,
            RECEIVE_TIMEOUT_MS,
        )
    }
    .map_err(|_| "could not set a timeout on the check".to_string())?;

    let host = WideStr::new(HOST);
    // SAFETY: `session` is a live session handle, `host` outlives the call, and
    // the reserved argument is zero as the documentation requires.
    let connect = unsafe { WinHttpConnect(session.raw(), host.as_pcwstr(), HTTPS_PORT, 0) };
    let connect = WinHttpHandle(connect);
    if connect.raw().is_null() {
        return Err(last_error_message());
    }

    let verb = WideStr::new("GET");
    let path = WideStr::new(PATH);
    // SAFETY: `connect` is a live connection handle; `verb` and `path` outlive
    // the call; the version, referrer and accept-types arguments are the
    // documented "use the default" nulls; WINHTTP_FLAG_SECURE selects TLS,
    // which is mandatory for this endpoint.
    let request = unsafe {
        WinHttpOpenRequest(
            connect.raw(),
            verb.as_pcwstr(),
            path.as_pcwstr(),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        )
    };
    let request = WinHttpHandle(request);
    if request.raw().is_null() {
        return Err(last_error_message());
    }

    // `X-GitHub-Api-Version` pins the response shape, so a future breaking
    // change to the API cannot quietly turn this into a parse failure.
    let headers: Vec<u16> =
        "Accept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2022-11-28"
            .encode_utf16()
            .collect();

    // SAFETY: `request` is a live request handle and `headers` outlives the
    // call; there is no request body, so the optional-data pointer is `None`
    // and both lengths are zero, and no context is needed in synchronous mode.
    unsafe { WinHttpSendRequest(request.raw(), Some(&headers), None, 0, 0, 0) }
        .map_err(|_| last_error_message())?;

    // SAFETY: `request` has had `WinHttpSendRequest` called on it, which is the
    // precondition; the reserved argument must be null.
    unsafe { WinHttpReceiveResponse(request.raw(), std::ptr::null_mut()) }
        .map_err(|_| last_error_message())?;

    let status = query_status_code(&request)?;
    match status {
        200 => {}
        403 | 429 => {
            return Err("GitHub is rate-limiting update checks; try again later".to_string())
        }
        404 => return Err("no published release was found".to_string()),
        other => return Err(format!("GitHub answered {other}")),
    }

    let body = read_body(&request)?;
    String::from_utf8(body).map_err(|_| "GitHub's reply was not valid UTF-8".to_string())
}

/// Read the HTTP status line's numeric code.
fn query_status_code(request: &WinHttpHandle) -> Result<u32, String> {
    let mut status: u32 = 0;
    let mut len = std::mem::size_of::<u32>() as u32;
    // SAFETY: `request` has a received response. WINHTTP_QUERY_FLAG_NUMBER
    // makes WinHTTP write a single `u32`, and `len` says the buffer is exactly
    // that big; `status` is a live local for the duration of the call. A null
    // header name is required for a well-known query, and a null index means
    // "the first match".
    unsafe {
        WinHttpQueryHeaders(
            request.raw(),
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(std::ptr::addr_of_mut!(status).cast::<c_void>()),
            &mut len,
            std::ptr::null_mut(),
        )
    }
    .map_err(|_| "GitHub's reply had no status code".to_string())?;
    Ok(status)
}

/// Drain the response body, refusing to grow past [`MAX_BODY_BYTES`].
fn read_body(request: &WinHttpHandle) -> Result<Vec<u8>, String> {
    let mut body: Vec<u8> = Vec::new();
    let mut chunk = [0u8; READ_CHUNK_BYTES];
    loop {
        let mut read: u32 = 0;
        // SAFETY: `request` has a received response. `chunk` is a live
        // stack buffer of exactly `READ_CHUNK_BYTES`, which is the length
        // passed, so WinHTTP cannot write past it; `read` is a live local
        // out-parameter.
        unsafe {
            WinHttpReadData(
                request.raw(),
                chunk.as_mut_ptr().cast::<c_void>(),
                READ_CHUNK_BYTES as u32,
                &mut read,
            )
        }
        .map_err(|_| last_error_message())?;

        if read == 0 {
            return Ok(body);
        }
        // WinHTTP never reports more than it was asked for, but the buffer
        // index below would be a memory-safety bug if it ever did.
        let read = (read as usize).min(READ_CHUNK_BYTES);
        if body.len() + read > MAX_BODY_BYTES {
            return Err("GitHub's reply was implausibly large".to_string());
        }
        body.extend_from_slice(&chunk[..read]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A trimmed but structurally faithful capture of
    // GET /repos/andreaswiren/supertile/releases/latest.
    const SAMPLE_RELEASE_JSON: &str = r####"{
      "url": "https://api.github.com/repos/andreaswiren/supertile/releases/183472911",
      "assets_url": "https://api.github.com/repos/andreaswiren/supertile/releases/183472911/assets",
      "html_url": "https://github.com/andreaswiren/supertile/releases/tag/v0.25.0",
      "id": 183472911,
      "author": { "login": "andreaswiren", "id": 1234567, "type": "User" },
      "node_id": "RE_kwDOM1_pQs4K7Xyz",
      "tag_name": "v0.25.0",
      "target_commitish": "main",
      "name": "0.25.0: an update check",
      "draft": false,
      "prerelease": false,
      "created_at": "2026-08-18T09:14:22Z",
      "published_at": "2026-08-18T09:31:05Z",
      "assets": [
        {
          "url": "https://api.github.com/repos/andreaswiren/supertile/releases/assets/1",
          "name": "supertile.exe",
          "content_type": "application/vnd.microsoft.portable-executable",
          "size": 2418176,
          "download_count": 41,
          "browser_download_url": "https://github.com/andreaswiren/supertile/releases/download/v0.25.0/supertile.exe"
        }
      ],
      "tarball_url": "https://api.github.com/repos/andreaswiren/supertile/tarball/v0.25.0",
      "zipball_url": "https://api.github.com/repos/andreaswiren/supertile/zipball/v0.25.0",
      "body": "### Added\n- An optional update check, off by default.\n\n### Fixed\n- A detached window can be dragged back."
    }"####;

    #[test]
    fn a_real_release_payload_yields_the_version_and_url() {
        let release = parse_release(SAMPLE_RELEASE_JSON).expect("the sample payload must parse");
        assert_eq!(release.version, "0.25.0");
        assert_eq!(
            release.url,
            "https://github.com/andreaswiren/supertile/releases/tag/v0.25.0"
        );
        assert!(release.notes.starts_with("### Added"));
        assert!(release.notes.contains("update check"));
    }

    #[test]
    fn a_release_url_off_the_project_falls_back_to_the_repository() {
        // The link is handed to the shell, so an unexpected host is dropped
        // rather than opened.
        let json = r#"{"tag_name":"v9.9.9","html_url":"https://evil.example/pwn","body":""}"#;
        let release = parse_release(json).expect("the tag is still usable");
        assert_eq!(release.version, "9.9.9");
        assert_eq!(release.url, crate::APP_REPO);
    }

    #[test]
    fn a_reply_without_a_tag_is_an_error_not_a_panic() {
        assert!(parse_release(r#"{"html_url":"x"}"#).is_err());
        assert!(parse_release("not json at all").is_err());
        assert!(parse_release("").is_err());
        assert!(parse_release(r#"{"tag_name":"v"}"#).is_err());
    }

    #[test]
    fn missing_notes_are_empty_rather_than_absent() {
        let json = r#"{"tag_name":"1.0.0","html_url":"https://github.com/andreaswiren/supertile/releases/tag/1.0.0"}"#;
        let release = parse_release(json).unwrap();
        assert_eq!(release.notes, "");
    }

    #[test]
    fn enormous_notes_are_clipped() {
        let long = "x".repeat(MAX_NOTES_CHARS * 3);
        let json = format!(r#"{{"tag_name":"1.0.0","body":"{long}"}}"#);
        let release = parse_release(&json).unwrap();
        assert!(release.notes.len() <= MAX_NOTES_CHARS + 4);
        assert!(release.notes.ends_with('…'));
    }

    #[test]
    fn ten_sorts_after_nine() {
        // The whole reason for parsing numbers: a string comparison puts
        // "0.10.0" before "0.9.0" and would offer a downgrade.
        assert!(is_newer("0.9.0", "0.10.0"));
        assert!(!is_newer("0.10.0", "0.9.0"));
        assert!(is_newer("1.9.9", "1.10.0"));
        assert!(is_newer("0.24.9", "0.25.0"));
    }

    #[test]
    fn each_component_is_compared_in_turn() {
        assert!(is_newer("1.2.3", "2.0.0"));
        assert!(is_newer("1.2.3", "1.3.0"));
        assert!(is_newer("1.2.3", "1.2.4"));
        assert!(!is_newer("2.0.0", "1.99.99"));
        assert!(!is_newer("1.3.0", "1.2.99"));
        assert!(!is_newer("1.2.4", "1.2.3"));
    }

    #[test]
    fn the_same_version_is_not_newer() {
        assert!(!is_newer("0.24.2", "0.24.2"));
        assert!(!is_newer("0.24.2", "v0.24.2"));
        assert!(!is_newer("v0.24.2", "0.24.2"));
        assert!(!is_newer("0.0.0", "0.0.0"));
    }

    #[test]
    fn a_leading_v_is_tolerated_on_either_side() {
        assert!(is_newer("0.24.2", "v0.24.3"));
        assert!(is_newer("v0.24.2", "0.24.3"));
        assert!(is_newer("v0.24.2", "V0.25.0"));
        assert!(!is_newer("v0.25.0", "v0.24.2"));
    }

    #[test]
    fn surrounding_whitespace_does_not_confuse_the_comparison() {
        assert!(is_newer("  0.24.2 ", "\tv0.25.0\n"));
    }

    #[test]
    fn a_pre_release_suffix_is_ignored_rather_than_mis_ordered() {
        // Equal cores compare equal whichever side carries the suffix, so a
        // release candidate is never offered as an upgrade over the release.
        assert!(!is_newer("1.0.0", "1.0.0-rc.1"));
        assert!(!is_newer("1.0.0-rc.1", "1.0.0"));
        assert!(!is_newer("1.0.0-rc.1", "1.0.0-rc.2"));
        // The core still decides when it differs.
        assert!(is_newer("1.0.0", "1.0.1-beta"));
        assert!(!is_newer("1.0.1", "1.0.0-beta"));
        // Build metadata is treated the same way.
        assert!(!is_newer("1.0.0", "1.0.0+20260819"));
    }

    #[test]
    fn malformed_input_never_counts_as_newer() {
        for (current, candidate) in [
            ("", ""),
            ("0.24.2", ""),
            ("", "0.25.0"),
            ("0.24.2", "banana"),
            ("banana", "0.25.0"),
            ("0.24.2", "1.0"),
            ("0.24.2", "1"),
            ("0.24.2", "1.0.0.0"),
            ("0.24.2", "1.0.x"),
            ("0.24.2", "one.two.three"),
            ("0.24.2", "1.+2.3"),
            ("0.24.2", "-1.0.0"),
            ("0.24.2", "999999999999999999999999.0.0"),
            ("0.24.2", "<script>alert(1)</script>"),
        ] {
            assert!(
                !is_newer(current, candidate),
                "{current:?} -> {candidate:?} must not be treated as an upgrade"
            );
        }
    }

    #[test]
    fn a_configuration_that_has_never_checked_is_due() {
        assert!(due(0, 0, 24));
        assert!(due(0, 1_755_000_000, 24));
    }

    #[test]
    fn exactly_due_counts_as_due() {
        let last = 1_755_000_000;
        assert!(due(last, last + 24 * 3600, 24));
    }

    #[test]
    fn a_check_a_moment_ago_is_not_due() {
        let last = 1_755_000_000;
        assert!(!due(last, last, 24));
        assert!(!due(last, last + 1, 24));
        assert!(!due(last, last + 24 * 3600 - 1, 24));
    }

    #[test]
    fn well_past_the_interval_is_due() {
        let last = 1_755_000_000;
        assert!(due(last, last + 24 * 3600 + 1, 24));
        assert!(due(last, last + 365 * 24 * 3600, 24));
    }

    #[test]
    fn a_clock_that_went_backwards_is_due_rather_than_never() {
        // A timestamp in the future would otherwise wedge the checker until
        // real time caught up, which could be years.
        let last = 1_755_000_000;
        assert!(due(last, last - 1, 24));
        assert!(due(last, 0, 24));
        assert!(due(u64::MAX, 1_755_000_000, 24));
    }

    #[test]
    fn an_absurd_interval_does_not_overflow() {
        assert!(!due(1, 1_755_000_000, u64::MAX));
        assert!(due(1, 1_755_000_000, 0));
    }
}

// ============================================================== downloading ==
//
// Fetching and installing a new binary, which is a different risk from asking
// what the latest version is. The rules this code keeps to:
//
//   * **Only this repository.** Both URLs are checked against
//     `TRUSTED_URL_PREFIX` when the release is parsed, before anything is
//     fetched. A redirect is followed (GitHub serves release assets from a CDN)
//     but the URL that starts it is ours.
//   * **The digest decides.** The `.sha256` published beside the binary must be
//     present and must match, or nothing is written. A release without one
//     cannot be installed from inside the program.
//   * **No elevation is taken.** Where the running copy lives in a directory
//     the user cannot write -- `C:\Program Files`, the documented install
//     location -- the swap is handed to a single elevated command that the user
//     sees and approves. SuperTile itself stays unelevated.
//
// What the digest does and does not buy: it is a checksum, not a signature. It
// proves the bytes are the bytes GitHub published on the release page, over
// TLS, and catches truncation or a corrupted proxy. It is not evidence about
// who built them. Anyone who can publish a release can publish a digest for it.

/// Where a downloaded binary went, and what installing it will take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    /// Swapped in place. The caller should restart into it.
    Replaced,
    /// The install directory is not writable by this user. The new binary is
    /// waiting at this path; installing it means an elevated copy.
    NeedsElevation(std::path::PathBuf),
}

/// Download the published binary and check it against its digest.
///
/// Blocking, and slow enough to matter: call it off the UI thread. Returns the
/// path of a verified file in the temporary directory, which the caller either
/// installs or deletes.
pub fn download_verified(exe_url: &str, sha_url: &str) -> Result<std::path::PathBuf, String> {
    if !exe_url.starts_with(TRUSTED_URL_PREFIX) || !sha_url.starts_with(TRUSTED_URL_PREFIX) {
        return Err("that download is not on the SuperTile repository".to_string());
    }

    let sha_bytes = http_get(sha_url, 1024)?;
    let expected = String::from_utf8(sha_bytes)
        .map_err(|_| "the published digest was not text".to_string())?
        // The file is `<hex>  supertile.exe`, as sha256sum writes it.
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("the published digest was not a SHA-256".to_string());
    }

    let exe = http_get(exe_url, MAX_DOWNLOAD_BYTES)?;
    if exe.is_empty() {
        return Err("the download was empty".to_string());
    }
    let got = sha256_hex(&exe).ok_or_else(|| "could not hash the download".to_string())?;
    if got != expected {
        return Err("the download did not match its published digest".to_string());
    }

    let mut path = std::env::temp_dir();
    path.push(format!("supertile-{}.exe.new", std::process::id()));
    std::fs::write(&path, &exe).map_err(|e| format!("could not save the download: {e}"))?;
    Ok(path)
}

/// Put `new_exe` where the running executable is.
///
/// A running executable cannot be overwritten, but it *can* be renamed, so the
/// current one is moved aside first and the replacement takes its name. The
/// leftover is removed on the next start.
pub fn install(new_exe: &std::path::Path) -> Result<Install, String> {
    let target = std::env::current_exe().map_err(|e| format!("cannot locate myself: {e}"))?;
    let backup = target.with_extension("exe.old");

    let _ = std::fs::remove_file(&backup);
    if std::fs::rename(&target, &backup).is_err() {
        // Almost always a read-only install directory rather than anything
        // exotic. Say what it is and let the caller offer the elevated path.
        return Ok(Install::NeedsElevation(new_exe.to_path_buf()));
    }
    if let Err(e) = std::fs::copy(new_exe, &target) {
        // Put the working copy back before reporting: a failure here must not
        // leave the user with no executable at all.
        let _ = std::fs::rename(&backup, &target);
        return Err(format!("could not write the new version: {e}"));
    }
    let _ = std::fs::remove_file(new_exe);
    Ok(Install::Replaced)
}

/// Delete the `.old` file a previous update left behind. Cheap; ignore failure.
pub fn clean_previous() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(exe.with_extension("exe.old"));
    }
}

/// SHA-256 of `data`, lowercase hex, via the OS. No new dependency.
fn sha256_hex(data: &[u8]) -> Option<String> {
    use windows::Win32::Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE};
    let mut out = [0u8; 32];
    // SAFETY: the pseudo-handle names SHA-256 and needs no provider to be
    // opened or closed; there is no secret, so that slice is empty; `data` and
    // `out` are live for the call and `out` is exactly the digest length.
    let status = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out) };
    status.is_ok().then(|| {
        out.iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .concat()
    })
}

/// One HTTPS GET, following redirects, capped at `max_bytes`.
///
/// Separate from `fetch_latest_release` because that one is pinned to the API
/// host and path and sends API headers. This takes a whole URL, because a
/// release asset lives on `github.com` and redirects to a CDN host that is not
/// known in advance.
fn http_get(url: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    let (host, path) = split_https(url).ok_or_else(|| "malformed download URL".to_string())?;

    let agent = WideStr::new(&format!("supertile/{}", crate::APP_VERSION));
    // SAFETY: as in `fetch_latest_release` -- `agent` outlives the call, the
    // proxy arguments must be null for the automatic access type, and no flags
    // means synchronous mode.
    let session = WinHttpHandle(unsafe {
        WinHttpOpen(
            agent.as_pcwstr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        )
    });
    if session.raw().is_null() {
        return Err(format!(
            "could not start the download: {}",
            last_error_message()
        ));
    }
    // A download is bigger than a version check, so it gets longer to finish.
    // SAFETY: live session handle; the timeouts are milliseconds.
    unsafe {
        WinHttpSetTimeouts(
            session.raw(),
            RESOLVE_TIMEOUT_MS,
            CONNECT_TIMEOUT_MS,
            SEND_TIMEOUT_MS,
            DOWNLOAD_RECEIVE_TIMEOUT_MS,
        )
    }
    .map_err(|_| "could not set a timeout on the download".to_string())?;

    let host_w = WideStr::new(&host);
    // SAFETY: live session handle; `host_w` outlives the call; reserved is zero.
    let connect =
        WinHttpHandle(unsafe { WinHttpConnect(session.raw(), host_w.as_pcwstr(), HTTPS_PORT, 0) });
    if connect.raw().is_null() {
        return Err(last_error_message());
    }

    let verb = WideStr::new("GET");
    let path_w = WideStr::new(&path);
    // SAFETY: live connection handle; `verb` and `path_w` outlive the call; the
    // version, referrer and accept-types arguments are the documented nulls;
    // WINHTTP_FLAG_SECURE selects TLS.
    let request = WinHttpHandle(unsafe {
        WinHttpOpenRequest(
            connect.raw(),
            verb.as_pcwstr(),
            path_w.as_pcwstr(),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        )
    });
    if request.raw().is_null() {
        return Err(last_error_message());
    }

    // SAFETY: live request handle; no body, so the data pointer is None and
    // both lengths are zero; no context is needed in synchronous mode.
    unsafe { WinHttpSendRequest(request.raw(), None, None, 0, 0, 0) }
        .map_err(|_| last_error_message())?;
    // SAFETY: the request has been sent, which is the precondition; reserved
    // must be null.
    unsafe { WinHttpReceiveResponse(request.raw(), std::ptr::null_mut()) }
        .map_err(|_| last_error_message())?;

    match query_status_code(&request)? {
        200 => {}
        404 => return Err("that release file is no longer published".to_string()),
        other => return Err(format!("GitHub answered {other}")),
    }
    read_body_capped(&request, max_bytes)
}

/// Split `https://host/path` into its two halves.
///
/// Deliberately not a general URL parser: anything with credentials, a port or
/// a non-HTTPS scheme is refused rather than interpreted, because every URL
/// reaching here has already been checked to start with the project's own
/// `https://github.com/...` prefix and a redirect target that looks unusual is
/// a reason to stop, not to be clever.
fn split_https(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if host.is_empty() || host.contains('@') || host.contains(':') {
        return None;
    }
    Some((host.to_string(), path.to_string()))
}

/// Read a response body, refusing anything over `max_bytes`.
fn read_body_capped(request: &WinHttpHandle, max_bytes: usize) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; READ_CHUNK_BYTES];
    loop {
        let mut read: u32 = 0;
        // SAFETY: `chunk` is a live buffer of the length passed, and `read` is
        // a valid out-param. A zero read means the body is complete.
        unsafe {
            WinHttpReadData(
                request.raw(),
                chunk.as_mut_ptr() as *mut core::ffi::c_void,
                chunk.len() as u32,
                &mut read,
            )
        }
        .map_err(|_| last_error_message())?;
        if read == 0 {
            return Ok(out);
        }
        if out.len() + read as usize > max_bytes {
            return Err("the download was larger than expected and was abandoned".to_string());
        }
        out.extend_from_slice(&chunk[..read as usize]);
    }
}

#[cfg(test)]
mod download_tests {
    use super::*;

    /// Against the published SHA-256 vectors. If this is wrong, every download
    /// is either rejected or -- far worse -- accepted on a hash nobody checked.
    #[test]
    fn sha256_matches_the_known_vectors() {
        assert_eq!(
            sha256_hex(b"abc").unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"").unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn a_url_splits_into_host_and_path() {
        assert_eq!(
            split_https("https://github.com/andreaswiren/supertile/releases/download/v1/x.exe"),
            Some((
                "github.com".to_string(),
                "/andreaswiren/supertile/releases/download/v1/x.exe".to_string()
            ))
        );
        // A bare host still has a path.
        assert_eq!(
            split_https("https://example.com"),
            Some(("example.com".to_string(), "/".to_string()))
        );
    }

    /// Anything unusual is refused rather than interpreted.
    #[test]
    fn odd_urls_are_refused() {
        for bad in [
            "http://github.com/x",       // not TLS
            "https://user@github.com/x", // credentials
            "https://github.com:8443/x", // a port
            "ftp://github.com/x",        // not even http
            "github.com/x",              // no scheme
            "https:///x",                // no host
        ] {
            assert_eq!(split_https(bad), None, "{bad} should be refused");
        }
    }

    /// Assets are read from the release, and only from this repository.
    #[test]
    fn assets_are_taken_only_from_our_own_repository() {
        let body = r#"{
            "tag_name": "v9.9.9",
            "html_url": "https://github.com/andreaswiren/supertile/releases/tag/v9.9.9",
            "body": "notes",
            "assets": [
              {"name": "supertile.exe",
               "browser_download_url": "https://github.com/andreaswiren/supertile/releases/download/v9.9.9/supertile.exe"},
              {"name": "supertile.exe.sha256",
               "browser_download_url": "https://github.com/andreaswiren/supertile/releases/download/v9.9.9/supertile.exe.sha256"}
            ]
        }"#;
        let r = parse_release(body).unwrap();
        assert!(r.exe_url.unwrap().ends_with("/supertile.exe"));
        assert!(r.sha_url.unwrap().ends_with("/supertile.exe.sha256"));

        // The same release, with the binary pointed somewhere else entirely.
        let hostile = body.replace(
            "https://github.com/andreaswiren/supertile/releases/download/v9.9.9/supertile.exe\"",
            "https://example.invalid/supertile.exe\"",
        );
        let r = parse_release(&hostile).unwrap();
        assert_eq!(r.exe_url, None, "an off-repository binary is not offered");
    }

    /// A release with no digest published cannot be installed.
    #[test]
    fn a_download_without_a_digest_is_refused() {
        let err = download_verified(
            "https://github.com/andreaswiren/supertile/releases/download/v1/supertile.exe",
            "https://example.invalid/supertile.exe.sha256",
        )
        .unwrap_err();
        assert!(err.contains("not on the SuperTile repository"), "{err}");
    }
}

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

    Ok(Release {
        version,
        url,
        notes,
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

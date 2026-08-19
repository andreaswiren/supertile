//! Small Win32 helpers: UTF-16 conversion, known folders, and a minimal
//! file logger. Deliberately dependency-free.

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use windows::core::PCWSTR;
use windows::Win32::Foundation::MAX_PATH;
use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath, KF_FLAG_DEFAULT};

/// A NUL-terminated UTF-16 buffer that keeps its backing store alive.
///
/// `PCWSTR` is a raw pointer; handing one to Win32 while the owning `Vec` has
/// already been dropped is the classic use-after-free in this codebase's
/// problem space. Holding the `Vec` in a named binding makes the lifetime
/// explicit at every call site.
pub struct WideStr(Vec<u16>);

impl WideStr {
    pub fn new(s: &str) -> Self {
        let mut v: Vec<u16> = std::ffi::OsStr::new(s).encode_wide().collect();
        v.push(0);
        WideStr(v)
    }

    pub fn as_pcwstr(&self) -> PCWSTR {
        PCWSTR(self.0.as_ptr())
    }

    pub fn as_mut_ptr(&mut self) -> *mut u16 {
        self.0.as_mut_ptr()
    }

    pub fn len_with_nul(&self) -> usize {
        self.0.len()
    }
}

/// Build a fixed-size UTF-16 array from a `&str`, truncating safely.
///
/// Used for Win32 structs with inline `[u16; N]` fields (e.g. `NOTIFYICONDATAW`).
/// Truncation happens on a UTF-16 code-unit boundary and always leaves room for
/// the terminating NUL, so the result is a valid C string even if the input is
/// longer than the field.
pub fn fill_wide_buf<const N: usize>(dst: &mut [u16; N], src: &str) {
    debug_assert!(N > 0);
    let mut i = 0usize;
    for unit in std::ffi::OsStr::new(src).encode_wide() {
        if i + 1 >= N {
            break;
        }
        dst[i] = unit;
        i += 1;
    }
    dst[i] = 0;
}

/// Decode a NUL-terminated (or full-length) UTF-16 slice into a `String`.
pub fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    OsString::from_wide(&buf[..end])
        .to_string_lossy()
        .into_owned()
}

/// `%LOCALAPPDATA%\SuperTile`, created if missing.
///
/// LocalAppData (not Roaming) is deliberate: the store holds machine-specific
/// monitor identities and window geometry that must not roam between machines
/// with different displays.
pub fn data_dir() -> std::io::Result<PathBuf> {
    let base = known_folder(&FOLDERID_LocalAppData).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "SHGetKnownFolderPath(FOLDERID_LocalAppData) failed",
        )
    })?;
    let dir = base.join("SuperTile");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn known_folder(id: &windows::core::GUID) -> Option<PathBuf> {
    // SAFETY: SHGetKnownFolderPath returns a NUL-terminated string that we own
    // and must release with CoTaskMemFree, which happens on the success path
    // below. `id` is a compile-time KNOWNFOLDERID constant.
    unsafe {
        let pwstr = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        if pwstr.is_null() {
            return None;
        }
        // SAFETY: SHGetKnownFolderPath returns a NUL-terminated string that we
        // own and must release with CoTaskMemFree.
        let mut len = 0usize;
        while *pwstr.0.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(pwstr.0, len);
        let s = OsString::from_wide(slice);
        windows::Win32::System::Com::CoTaskMemFree(Some(pwstr.0 as *const _));
        Some(PathBuf::from(s))
    }
}

/// Absolute path of the running executable's directory.
pub fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|p| p.to_path_buf())
}

const _: () = assert!(MAX_PATH == 260);

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static LOG_ENABLED: AtomicBool = AtomicBool::new(false);
static LOG_VERBOSE: AtomicBool = AtomicBool::new(false);
static LOG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Enable file logging to `%LOCALAPPDATA%\SuperTile\supertile.log`.
///
/// Off by default. Logs record window titles and executable paths, which are
/// user-identifying, so this is opt-in via config (`diagnostics.logging`) and
/// documented in the privacy section of the README.
pub fn set_logging(enabled: bool) {
    if enabled {
        if let Ok(dir) = data_dir() {
            let path = dir.join("supertile.log");
            // Truncate on enable so the log cannot grow without bound across runs.
            let _ = std::fs::write(&path, b"");
            *LOG_PATH.lock().unwrap() = Some(path);
        }
    }
    LOG_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Milliseconds since boot, for measuring intervals.
///
/// Monotonic, so it cannot jump backwards over a clock change or a daylight
/// saving transition the way a wall clock can -- which matters when the value
/// is used to decide whether enough time has passed to trust a repeat.
pub fn tick_ms() -> u64 {
    // SAFETY: GetTickCount64 takes no arguments and cannot fail.
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() }
}

pub fn logging_enabled() -> bool {
    LOG_ENABLED.load(Ordering::Relaxed)
}

/// Turn on the high-frequency detail: every retile, every placement, every
/// window entering or leaving the managed set.
///
/// Separate from [`set_logging`] because the two have different costs. The
/// ordinary log records decisions and is cheap; the verbose log records the
/// evidence behind them, runs to megabytes in an afternoon, and is only worth
/// paying for while a specific fault is being chased.
pub fn set_verbose(enabled: bool) {
    LOG_VERBOSE.store(enabled, Ordering::Relaxed);
}

pub fn verbose_enabled() -> bool {
    LOG_VERBOSE.load(Ordering::Relaxed) && logging_enabled()
}

/// `HH:MM:SS.mmm` from the system clock, for ordering events in the log.
///
/// A log of a timing-dependent fault without timestamps is a list of things
/// that happened in no particular order.
fn stamp() -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    // SAFETY: GetLocalTime only writes to the struct it is given.
    let t = unsafe { GetLocalTime() };
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

pub fn log_line(msg: &str) {
    if !logging_enabled() {
        return;
    }
    use std::io::Write;
    let guard = LOG_PATH.lock().unwrap();
    let Some(path) = guard.as_ref() else { return };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
    {
        let _ = writeln!(f, "{} {}", stamp(), msg);
    }
}

/// The high-frequency companion to [`log!`], silent unless verbose logging is
/// on.
///
/// Window titles logged through either macro must be wrapped in single quotes,
/// and nothing else in a log line may be. That convention is what lets an issue
/// report strip titles out of the log tail mechanically -- see
/// [`crate::report::redact_log_line`]. Without a rule, redacting free text
/// would be guesswork, and the log could not be published at all.
#[macro_export]
macro_rules! vlog {
    ($($arg:tt)*) => {
        if $crate::util::verbose_enabled() {
            $crate::util::log_line(&format!($($arg)*));
        }
    };
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        if $crate::util::logging_enabled() {
            $crate::util::log_line(&format!($($arg)*));
        }
    };
}

/// Put UTF-16 text on the clipboard, replacing whatever was there.
///
/// Used so a question typed into the palette survives the trip to another
/// application even when that application offers no way to receive it directly.
/// Overwriting the clipboard is a real cost to the user, so this is only done
/// in response to an explicit action that needs it.
pub fn set_clipboard_text(text: &str) -> bool {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::CF_UNICODETEXT;

    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = std::mem::size_of_val(wide.as_slice());

    // SAFETY: the clipboard is opened, filled and closed on this thread with no
    // early return between open and close. On success the moveable block passes
    // to the clipboard, which owns it from then on; on any failure it is freed
    // here so a refused paste cannot leak it.
    unsafe {
        if OpenClipboard(None).is_err() {
            return false;
        }
        let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, bytes) else {
            let _ = CloseClipboard();
            return false;
        };
        let ptr = GlobalLock(mem) as *mut u16;
        let mut ok = false;
        if !ptr.is_null() {
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
            let _ = GlobalUnlock(mem);
            if EmptyClipboard().is_ok()
                && SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(mem.0))).is_ok()
            {
                ok = true;
            }
        }
        if !ok {
            let _ = windows::Win32::Foundation::GlobalFree(Some(mem));
        }
        let _ = CloseClipboard();
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_roundtrip() {
        let w = WideStr::new("hello");
        assert_eq!(w.len_with_nul(), 6);
        assert_eq!(wide_to_string(&w.0), "hello");
    }

    #[test]
    fn fill_truncates_and_nul_terminates() {
        let mut buf = [0u16; 4];
        fill_wide_buf(&mut buf, "abcdefgh");
        assert_eq!(buf[3], 0, "must be NUL terminated");
        assert_eq!(wide_to_string(&buf), "abc");
    }

    #[test]
    fn fill_handles_exact_fit() {
        let mut buf = [0u16; 4];
        fill_wide_buf(&mut buf, "abc");
        assert_eq!(wide_to_string(&buf), "abc");
        assert_eq!(buf[3], 0);
    }

    #[test]
    fn fill_handles_empty() {
        let mut buf = [9u16; 3];
        fill_wide_buf(&mut buf, "");
        assert_eq!(buf[0], 0);
        assert_eq!(wide_to_string(&buf), "");
    }

    #[test]
    fn wide_to_string_without_nul() {
        let raw: Vec<u16> = "abc".encode_utf16().collect();
        assert_eq!(wide_to_string(&raw), "abc");
    }
}

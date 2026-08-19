//! Claude Desktop integration for the command palette.
//!
//! When Claude Desktop is installed, the palette gains an entry that opens a
//! new chat. The entry is only offered when the application is actually
//! present — an integration that fails silently on a machine without it is
//! worse than no integration.
//!
//! ## Detection
//!
//! Two independent signals, either of which is sufficient:
//!
//! 1. The `claude:` protocol handler under `HKCU\Software\Classes`, which the
//!    installer registers.
//! 2. `%LOCALAPPDATA%\AnthropicClaude\claude.exe`, the default install path.
//!
//! Both are checked because either can be missing on a working install: a
//! per-machine install lands elsewhere, and the protocol registration can be
//! taken over by another application.
//!
//! ## Launching
//!
//! The protocol is preferred and the executable is the fallback.
//! `ShellExecuteW` handles a custom URI scheme; note that `Start-Process` in
//! PowerShell does *not*, which makes the handler look broken when it is not.

use std::path::PathBuf;

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, HKEY, HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, KEY_READ,
};

/// URI that opens Claude Desktop on a new conversation.
///
/// There is no documented deep link for pre-filling a prompt, so this opens an
/// empty chat and nothing more — which is what was asked for. If a future
/// build of Claude Desktop rejects the path, the plain scheme below still
/// activates the app.
const NEW_CHAT_URI: &str = "claude://new";
/// Activating the app without asking for anything in particular.
const PLAIN_URI: &str = "claude://";

fn key_exists(root: HKEY, path: &str) -> bool {
    let sub = crate::util::WideStr::new(path);
    let mut key = HKEY::default();
    // SAFETY: `sub` outlives the call; `key` is a valid out-param, closed
    // immediately below when the open succeeds.
    let status = unsafe { RegOpenKeyExW(root, sub.as_pcwstr(), None, KEY_READ, &mut key) };
    if status == ERROR_SUCCESS {
        // SAFETY: `key` came from a successful RegOpenKeyExW.
        unsafe {
            let _ = RegCloseKey(key);
        }
        true
    } else {
        false
    }
}

/// The default per-user install location, if the executable is there.
pub fn executable() -> Option<PathBuf> {
    let local = std::env::var("LOCALAPPDATA").ok()?;
    let exe = PathBuf::from(local).join(r"AnthropicClaude\claude.exe");
    exe.is_file().then_some(exe)
}

/// Is the `claude:` protocol registered for this user or machine-wide?
pub fn protocol_registered() -> bool {
    key_exists(
        HKEY_CURRENT_USER,
        r"Software\Classes\claude\shell\open\command",
    ) || key_exists(HKEY_CLASSES_ROOT, r"claude\shell\open\command")
}

/// Is Claude Desktop installed?
///
/// No longer gates asking a question -- that goes to the browser and needs
/// nothing installed. Kept for the palette entry that opens the desktop client
/// itself.
///
/// Cheap enough to call when building the palette: two registry probes and at
/// most one file-existence check.
pub fn is_installed() -> bool {
    protocol_registered() || executable().is_some()
}

/// Open a new chat in Claude Desktop. Returns false if nothing could be run.
/// Where a question goes.
///
/// The web app accepts a prompt in the URL; the desktop client does not, and
/// there is no deep link that makes it. Tested directly: `claude://new` on a
/// running Claude brings the existing window forward and nothing else.
///
/// So a question goes to the browser, where it arrives typed into the composer.
/// It is deliberately *not* sent — `?q=` fills the box and stops there, and a
/// window manager should not be able to send a message on somebody's behalf
/// because they pressed Enter in a launcher.
const WEB_NEW_CHAT: &str = "https://claude.ai/new";

/// Longest question worth putting in a URL.
///
/// Browsers and servers both have limits, and a silently truncated question is
/// worse than one that had to be pasted. Beyond this the text goes to the
/// clipboard instead and the caller says so.
const MAX_URL_QUESTION: usize = 1500;

/// What happened, so the caller can tell the user the truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handoff {
    /// The question is in the composer at claude.ai, waiting to be sent.
    Prefilled,
    /// Too long for a URL: it is on the clipboard, and a blank chat is open.
    OnClipboard,
    /// Nothing could be opened.
    Failed,
}

/// Open Claude with `question` already typed in.
///
/// Nothing is sent. The user reads what they typed and presses Enter, which is
/// the right division of labour: composing is ours, sending is theirs.
pub fn ask(question: &str) -> Handoff {
    let question = question.trim();
    if question.is_empty() {
        return if launch(WEB_NEW_CHAT) {
            Handoff::Prefilled
        } else {
            Handoff::Failed
        };
    }

    if question.len() <= MAX_URL_QUESTION {
        let url = format!("{WEB_NEW_CHAT}?q={}", percent_encode(question));
        if launch(&url) {
            return Handoff::Prefilled;
        }
    }

    // Too long, or the browser refused the URL. The clipboard is the fallback
    // rather than the default: overwriting it is a real cost to the user and is
    // not worth paying when the URL would have carried the text.
    let copied = crate::util::set_clipboard_text(question);
    if launch(WEB_NEW_CHAT) && copied {
        Handoff::OnClipboard
    } else {
        Handoff::Failed
    }
}

/// Percent-encode for a URI query, keeping only the unreserved set.
///
/// Hand-rolled because pulling a URL crate in for one field would be a
/// dependency in the SBOM, an entry in the licence audit and a supply-chain
/// surface, all to escape a string.
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn open_new_chat() -> bool {
    if protocol_registered() && launch(NEW_CHAT_URI) {
        return true;
    }
    // The handler may reject an unknown path; plain activation still works.
    if protocol_registered() && launch(PLAIN_URI) {
        return true;
    }
    match executable() {
        Some(exe) => launch(&exe.to_string_lossy()),
        None => false,
    }
}

fn launch(target: &str) -> bool {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let t = crate::util::WideStr::new(target);
    let verb = crate::util::WideStr::new("open");
    // SAFETY: both strings outlive the call. ShellExecuteW returns a
    // pseudo-HINSTANCE; values <= 32 are documented failures.
    let r = unsafe {
        ShellExecuteW(
            None,
            verb.as_pcwstr(),
            t.as_pcwstr(),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    r.0 as usize > 32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_answers_without_panicking() {
        // Whatever this machine has, both probes must be total.
        let _ = is_installed();
        let _ = protocol_registered();
        let _ = executable();
    }

    #[test]
    fn the_two_signals_agree_with_is_installed() {
        assert_eq!(
            is_installed(),
            protocol_registered() || executable().is_some()
        );
    }

    #[test]
    fn a_missing_registry_key_is_reported_as_absent() {
        assert!(!key_exists(
            HKEY_CURRENT_USER,
            r"Software\Classes\supertile-not-a-real-scheme"
        ));
    }

    #[test]
    fn the_executable_path_is_under_the_user_profile() {
        // Never a machine-wide path: Claude Desktop installs per user, and a
        // world-writable location would be a launch target we do not control.
        if let Some(exe) = executable() {
            let local = std::env::var("LOCALAPPDATA").unwrap();
            assert!(exe.starts_with(&local), "{exe:?} is outside {local}");
            assert!(exe.is_absolute());
        }
    }

    #[test]
    fn encoding_escapes_everything_outside_the_unreserved_set() {
        assert_eq!(percent_encode("hello"), "hello");
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(percent_encode("x&y=z"), "x%26y%3Dz");
        assert_eq!(percent_encode("a-b_c.d~e"), "a-b_c.d~e");
    }

    #[test]
    fn encoding_handles_non_ascii_a_byte_at_a_time() {
        // UTF-8 first, then escape: the bytes are what a URI carries.
        let out = percent_encode("Wirén");
        assert!(out.is_ascii(), "{out} must be safe to put in a URI");
        assert!(out.starts_with("Wir"));
    }

    #[test]
    fn an_encoded_question_cannot_break_out_of_the_query() {
        // A question containing a scheme or a separator must stay one value.
        let out = percent_encode("claude://evil?x=1#f");
        for bad in [':', '/', '?', '#', '&', '='] {
            assert!(!out.contains(bad), "{bad} survived encoding: {out}");
        }
    }

    #[test]
    fn the_uris_use_the_claude_scheme() {
        assert!(NEW_CHAT_URI.starts_with("claude:"));
        assert!(PLAIN_URI.starts_with("claude:"));
    }
}

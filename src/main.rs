//! SuperTile entry point.
//!
//! `windows_subsystem = "windows"` keeps a console window from flashing on
//! launch — this is a tray application, and a console would be both ugly and a
//! surprise for a program the user expects to be invisible.

#![windows_subsystem = "windows"]
#![forbid(unsafe_op_in_unsafe_fn)]

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, SetForegroundWindow, ShowWindow, MB_ICONINFORMATION, MB_OK, SW_SHOW,
};

use supertile::{app, util::WideStr};

/// Session-scoped so two different users can each run their own SuperTile.
/// `Local\` rather than `Global\` for exactly that reason.
const SINGLE_INSTANCE_MUTEX: &str = r"Local\SuperTile.SingleInstance.9f2c1a";

fn main() {
    // Per-monitor v2 must be set before any window exists. The manifest already
    // declares it; this is the belt-and-braces path for an unmanifested build
    // (for example `cargo run` where the resource compiler was unavailable).
    // SAFETY: no windows have been created yet; a failure here is not fatal
    // because the manifest normally covers it.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let Some(_guard) = SingleInstance::acquire() else {
        // Another copy owns the tray icon; a second one would fight it for
        // every window on the desktop.
        notify(
            "SuperTile is already running",
            "Look for the SuperTile icon in the notification area.\n\n\
             Use its tray menu to open settings or exit.",
        );
        return;
    };

    let Some(app) = app::App::new() else {
        notify(
            "SuperTile could not start",
            "The application window could not be created, so SuperTile has exited.",
        );
        return;
    };

    let code = app::run();

    // Drop explicitly so the tray icon, hotkeys and the WinEvent hook are
    // released before the process exits, rather than relying on teardown
    // order. A tray icon outliving its process leaves a ghost in the
    // notification area until the user hovers over it.
    drop(app);
    std::process::exit(code);
}

/// Holds the single-instance mutex for the lifetime of the process.
struct SingleInstance(HANDLE);

impl SingleInstance {
    fn acquire() -> Option<SingleInstance> {
        let name = WideStr::new(SINGLE_INSTANCE_MUTEX);
        // SAFETY: name outlives the call. A named mutex in the Local namespace
        // is created if absent and opened if present.
        let handle = unsafe { CreateMutexW(None, true, name.as_pcwstr()) }.ok()?;

        // SAFETY: reads the calling thread's last error, set by CreateMutexW.
        let already = unsafe { windows::Win32::Foundation::GetLastError() } == ERROR_ALREADY_EXISTS;
        if already {
            // SAFETY: the handle is ours and closed exactly once.
            unsafe {
                let _ = CloseHandle(handle);
            }
            // Surface the running instance so the click was not wasted.
            focus_existing();
            return None;
        }
        Some(SingleInstance(handle))
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateMutexW and is closed once. The
        // mutex is released implicitly when the process exits.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Bring the already-running instance's About window forward, if it has one.
fn focus_existing() {
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
    let class = WideStr::new("SuperTile.About");
    // SAFETY: class name outlives the call; a miss returns an invalid handle.
    unsafe {
        if let Ok(h) = FindWindowW(class.as_pcwstr(), PCWSTR::null()) {
            if !h.is_invalid() {
                let _ = ShowWindow(h, SW_SHOW);
                let _ = SetForegroundWindow(h);
            }
        }
    }
}

fn notify(title: &str, body: &str) {
    let t = WideStr::new(title);
    let b = WideStr::new(body);
    // SAFETY: both strings outlive the call.
    unsafe {
        MessageBoxW(
            None,
            b.as_pcwstr(),
            t.as_pcwstr(),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

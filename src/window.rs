//! Window discovery, classification and placement.
//!
//! The tricky part of a tiling window manager is not moving windows — it is
//! deciding *which* windows to move. Windows 11 fills `EnumWindows` with
//! invisible shell scaffolding, cloaked UWP hosts and zero-size proxies, all of
//! which look like ordinary top-level windows. Tiling one of them leaves a
//! visible hole in the layout.
//!
//! The decision is therefore isolated in [`classify`], a pure function over a
//! [`Candidate`] snapshot, so every rule is unit-testable without a desktop.

use std::collections::HashSet;

use windows::core::BOOL;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, MAX_PATH, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::{Config, RuleAction};
use crate::layout::Rect;
use crate::util::wide_to_string;

/// What SuperTile should do with a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Include in the tiling.
    Tile,
    /// A real window, but left where the user put it.
    Float,
    /// Not a user-facing window at all; pretend it does not exist.
    Ignore,
}

/// The facts [`classify`] needs, snapshotted from Win32.
///
/// Kept as plain data so the classification rules can be tested exhaustively.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub style: u32,
    pub ex_style: u32,
    pub visible: bool,
    pub minimized: bool,
    pub cloaked: bool,
    pub has_owner: bool,
    pub title: String,
    pub class: String,
    pub exe: String,
    pub width: i32,
    pub height: i32,
}

impl Default for Candidate {
    /// A plausible ordinary application window, for tests to mutate.
    fn default() -> Self {
        Candidate {
            style: (WS_VISIBLE.0 | WS_OVERLAPPEDWINDOW.0),
            ex_style: 0,
            visible: true,
            minimized: false,
            cloaked: false,
            has_owner: false,
            title: "Untitled".into(),
            class: "AppClass".into(),
            exe: r"C:\Program Files\App\app.exe".into(),
            width: 800,
            height: 600,
        }
    }
}

/// Shell window classes that are never user windows.
///
/// `Progman` and `WorkerW` are the desktop itself; the rest are Windows 11
/// shell surfaces (Task View, Snap Assist, IME candidates, tray flyouts) that
/// present as ordinary top-level windows.
const SYSTEM_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Shell_InputSwitchTopLevelWindow",
    "TaskListThumbnailWnd",
    "MultitaskingViewFrame",
    "ForegroundStaging",
    "XamlExplorerHostIslandWindow",
    "Windows.Internal.Shell.TabProxyWindow",
    "Xaml_WindowedPopupClass",
    "EdgeUiInputTopWndClass",
    "NarratorHelperWindow",
    "TouchKeyboardHostWindow",
    "Windows.UI.Core.CoreWindow",
    "ApplicationManager_ImmersiveShellWindow",
    "SysShadow",
    "tooltips_class32",
];

/// Executables whose windows are shell chrome rather than applications.
const SYSTEM_EXES: &[&str] = &[
    "searchhost.exe",
    "searchui.exe",
    "startmenuexperiencehost.exe",
    "shellexperiencehost.exe",
    "textinputhost.exe",
    "peopleexperiencehost.exe",
    "lockapp.exe",
    "logonui.exe",
    "applicationframehost.exe.placeholder", // see note in classify()
];

/// Applications that are real windows but should not be tiled.
///
/// Distinct from [`SYSTEM_EXES`], which are shell scaffolding and are ignored
/// outright — these belong in the window list and the palette, they simply
/// float.
///
/// The list is deliberately short. Task Manager runs elevated, so Windows
/// forbids moving it and a cell reserved for it can never be filled; the others
/// are transient system surfaces that exist to be dismissed, not arranged. Any
/// program that merely *might* not like being resized is left off: a window
/// that will not tile is obvious and easily excluded from the tray, whereas a
/// window that should have tiled and silently did not is a mystery.
///
/// A `rules` entry in the config overrides this either way.
const FLOAT_BY_DEFAULT_EXES: &[&str] = &["taskmgr.exe", "consent.exe", "osk.exe", "magnify.exe"];

fn file_name_of(path: &str) -> String {
    path.rsplit(['\\', '/'])
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase()
}

/// Decide what to do with a window. Pure; see module docs.
///
/// Order matters: explicit user rules win over the built-in heuristics, so a
/// `[[rules]]` entry can force-tile something SuperTile would otherwise skip.
pub fn classify(c: &Candidate, cfg: &Config) -> Disposition {
    // 1. User rules take precedence over everything else.
    if let Some(action) = cfg.rule_for(&c.exe, &c.class, &c.title) {
        return match action {
            RuleAction::Tile => Disposition::Tile,
            RuleAction::Float => Disposition::Float,
            RuleAction::Ignore => Disposition::Ignore,
        };
    }

    // 2. Hard exclusions — never user windows.
    if SYSTEM_CLASSES
        .iter()
        .any(|s| s.eq_ignore_ascii_case(&c.class))
    {
        return Disposition::Ignore;
    }
    if SYSTEM_EXES.contains(&file_name_of(&c.exe).as_str()) {
        return Disposition::Ignore;
    }

    // 3. Not visible to the user.
    if !c.visible {
        return Disposition::Ignore;
    }
    // DWM cloaking is the mechanism behind UWP "ghost" windows: a suspended
    // Store app keeps a visible top-level HWND that is not drawn anywhere.
    // IsWindowVisible returns true for these, so cloaking must be checked
    // separately or the layout fills with invisible tiles.
    if c.cloaked {
        return Disposition::Ignore;
    }
    // Tool windows are palettes and popups by definition.
    if c.ex_style & WS_EX_TOOLWINDOW.0 != 0 {
        return Disposition::Ignore;
    }

    // "Is this a real window at all?" must be settled before any disposition
    // is chosen. Checking ownership first would classify untitled scaffolding
    // — WinForms parking windows, for instance — as a float rather than
    // discarding it.
    //
    // A window with no title is almost always scaffolding.
    if c.title.trim().is_empty() {
        return Disposition::Ignore;
    }
    // Degenerate geometry: proxies and message sinks.
    if c.width <= 1 || c.height <= 1 {
        return Disposition::Ignore;
    }

    // Owned windows are dialogs belonging to another window.
    if c.has_owner {
        return Disposition::Float;
    }

    // Known-untileable applications float rather than being ignored: they are
    // still real windows and belong in the window list and the palette.
    //
    // Placed after the reality checks above, not before them. These programs
    // own untitled scaffolding like every other program does, and floating that
    // would put invisible entries in the layout.
    if FLOAT_BY_DEFAULT_EXES.contains(&file_name_of(&c.exe).as_str()) {
        return Disposition::Float;
    }

    // 4. Minimised windows keep their slot conceptually but cannot be sized.
    if c.minimized {
        return Disposition::Ignore;
    }

    // 5. Non-resizable windows would be squashed by tiling, so let them float.
    //    WS_THICKFRAME is the resize border; WS_MAXIMIZEBOX implies the window
    //    accepts being resized to arbitrary dimensions.
    let resizable = c.style & WS_THICKFRAME.0 != 0;
    let can_maximize = c.style & WS_MAXIMIZEBOX.0 != 0;
    if !resizable || !can_maximize {
        return Disposition::Float;
    }

    // Deliberately no `WS_CAPTION` requirement.
    //
    // It was here to catch modal-ish popups, but it caught applications that
    // draw their own title bar instead. Bambu Studio is one: wxWidgets with a
    // custom frame, resizable, maximisable, unowned and titled, and it never
    // tiled because Windows was not drawing its caption.
    //
    // Everything above already characterises an application window -- visible,
    // not cloaked, not a tool window, titled, real geometry, no owner,
    // resizable, maximisable. A popup that satisfies all of those is a window
    // by any reasonable definition, whoever paints its frame, and the number of
    // programs painting their own is only going up.
    Disposition::Tile
}

/// A live window plus everything needed to place it.
#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub hwnd: isize,
    pub title: String,
    pub class: String,
    pub exe: String,
    /// `GetWindowRect` — includes the invisible resize border.
    pub rect: Rect,
    /// `DWMWA_EXTENDED_FRAME_BOUNDS` — what the user actually sees.
    pub frame: Rect,
    pub disposition: Disposition,
    /// Handle of the monitor this window is on, resolved once during
    /// enumeration so callers do not have to ask Win32 again per monitor.
    pub monitor: isize,
}

impl WindowInfo {
    pub fn handle(&self) -> HWND {
        HWND(self.hwnd as *mut core::ffi::c_void)
    }

    /// Per-edge difference between the window rect and its visible frame.
    ///
    /// Windows 10/11 windows carry an invisible resize border, typically 7px
    /// on the left, right and bottom. Placing `GetWindowRect` on a zone edge
    /// therefore leaves a visible gap that does not match the configured one.
    /// These insets convert a desired *visible* rect into the window rect to
    /// request.
    pub fn frame_insets(&self) -> (i32, i32, i32, i32) {
        (
            self.frame.left - self.rect.left,
            self.frame.top - self.rect.top,
            self.rect.right - self.frame.right,
            self.rect.bottom - self.frame.bottom,
        )
    }

    /// Window rect to request so the *visible* frame lands exactly on `zone`.
    pub fn target_rect_for(&self, zone: Rect) -> Rect {
        let (l, t, r, b) = self.frame_insets();
        // Guard against absurd insets from a misbehaving compositor.
        let sane = |v: i32| if (0..64).contains(&v) { v } else { 0 };
        Rect::new(
            zone.left - sane(l),
            zone.top - sane(t),
            zone.right + sane(r),
            zone.bottom + sane(b),
        )
    }
}

// ---------------------------------------------------------------------------
// Win32 queries
// ---------------------------------------------------------------------------

/// A window's title, for log lines that would otherwise be ambiguous.
///
/// Added after a diagnosis was built on a resize log line that recorded
/// measurements but not which window they belonged to -- so one window's
/// numbers were read as another's. A measurement without its subject is worse
/// than no measurement: it invites a confident wrong answer.
pub fn title_of(hwnd: HWND) -> String {
    window_text(hwnd)
}

fn window_text(hwnd: HWND) -> String {
    // SAFETY: GetWindowTextLengthW is safe for any HWND; a destroyed window
    // returns 0.
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    // SAFETY: buf has len+1 elements, matching the size passed to Win32.
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    buf.truncate(n.max(0) as usize);
    wide_to_string(&buf)
}

fn window_class(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: buf is a valid 256-element array; GetClassNameW truncates.
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    wide_to_string(&buf[..n.max(0) as usize])
}

fn window_rect(hwnd: HWND) -> Rect {
    let mut r = RECT::default();
    // SAFETY: `r` is a valid RECT out-param.
    let _ = unsafe { GetWindowRect(hwnd, &mut r) };
    Rect::new(r.left, r.top, r.right, r.bottom)
}

/// Visible frame bounds, falling back to the window rect if DWM declines.
fn frame_bounds(hwnd: HWND, fallback: Rect) -> Rect {
    let mut r = RECT::default();
    // SAFETY: the out-buffer and its size match the DWMWA_EXTENDED_FRAME_BOUNDS
    // contract (a RECT). A failed call leaves `r` zeroed, which we discard.
    let hr = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as *mut core::ffi::c_void,
            std::mem::size_of::<RECT>() as u32,
        )
    };
    if hr.is_ok() && (r.right > r.left) && (r.bottom > r.top) {
        Rect::new(r.left, r.top, r.right, r.bottom)
    } else {
        fallback
    }
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    // SAFETY: DWMWA_CLOAKED writes a single u32; the size argument matches.
    let hr = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        )
    };
    hr.is_ok() && cloaked != 0
}

thread_local! {
    /// HWND -> executable path.
    ///
    /// `OpenProcess` + `QueryFullProcessImageNameW` for every window on every
    /// enumeration dominates the cost of a retile, and a window's executable
    /// cannot change for the life of that window. Bounded so a session that
    /// opens and closes thousands of windows cannot grow it without limit.
    static EXE_CACHE: std::cell::RefCell<std::collections::HashMap<isize, String>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Upper bound on cached executable paths before the cache is dropped.
const EXE_CACHE_LIMIT: usize = 512;

/// Cached [`process_path`], keyed by window handle.
fn process_path_cached(hwnd: HWND) -> String {
    let key = hwnd.0 as isize;
    if let Some(hit) = EXE_CACHE.with(|c| c.borrow().get(&key).cloned()) {
        return hit;
    }
    let path = process_path(hwnd);
    // Do not cache failures: an elevated window may become readable later, and
    // caching "" would make that permanent.
    if !path.is_empty() {
        EXE_CACHE.with(|c| {
            let mut m = c.borrow_mut();
            if m.len() >= EXE_CACHE_LIMIT {
                m.clear();
            }
            m.insert(key, path.clone());
        });
    }
    path
}

/// Drop cache entries for windows that no longer exist.
pub fn prune_caches() {
    EXE_CACHE.with(|c| {
        c.borrow_mut()
            .retain(|k, _| is_live(HWND(*k as *mut core::ffi::c_void)));
    });
}

/// Full path of the executable owning `hwnd`, or an empty string.
///
/// Uses `PROCESS_QUERY_LIMITED_INFORMATION`, which succeeds for same-user
/// processes without elevation. Elevated processes will fail here for a
/// non-elevated SuperTile; that is expected and handled by returning "".
fn process_path(hwnd: HWND) -> String {
    let mut pid: u32 = 0;
    // SAFETY: `pid` is a valid out-param; Some(..) marks it as provided.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return String::new();
    }

    // SAFETY: OpenProcess with a limited-information access mask; the returned
    // handle is closed on every path below.
    let handle: HANDLE = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
    {
        Ok(h) => h,
        Err(_) => return String::new(),
    };

    let mut buf = [0u16; MAX_PATH as usize];
    let mut size = buf.len() as u32;
    // SAFETY: buf/size describe the same buffer; the call updates `size` to the
    // number of characters written.
    let ok = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
    };
    // SAFETY: `handle` came from OpenProcess and has not been closed yet.
    unsafe {
        let _ = CloseHandle(handle);
    }

    if ok.is_ok() {
        wide_to_string(&buf[..size as usize])
    } else {
        String::new()
    }
}

/// Snapshot one window into a [`Candidate`].
pub fn inspect(hwnd: HWND) -> Candidate {
    // SAFETY: all of these accept any HWND and report failure by value.
    let (style, ex_style, visible, minimized, owner) = unsafe {
        (
            GetWindowLongPtrW(hwnd, GWL_STYLE) as u32,
            GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32,
            IsWindowVisible(hwnd).as_bool(),
            IsIconic(hwnd).as_bool(),
            GetWindow(hwnd, GW_OWNER).unwrap_or_default(),
        )
    };
    let rect = window_rect(hwnd);

    Candidate {
        style,
        ex_style,
        visible,
        minimized,
        cloaked: is_cloaked(hwnd),
        has_owner: !owner.is_invalid(),
        title: window_text(hwnd),
        class: window_class(hwnd),
        exe: process_path_cached(hwnd),
        width: rect.width(),
        height: rect.height(),
    }
}

/// Every top-level window, classified.
///
/// `exclude` holds SuperTile's own window handles; a window manager that tiles
/// its own palette is a memorable bug.
pub fn enumerate(cfg: &Config, exclude: &HashSet<isize>) -> Vec<WindowInfo> {
    struct Ctx<'a> {
        cfg: &'a Config,
        exclude: &'a HashSet<isize>,
        out: Vec<WindowInfo>,
    }

    unsafe extern "system" fn cb(hwnd: HWND, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the &mut Ctx passed to EnumWindows below, which
        // outlives this synchronous enumeration.
        let ctx = unsafe { &mut *(data.0 as *mut Ctx) };

        if ctx.exclude.contains(&(hwnd.0 as isize)) {
            return TRUE;
        }
        let c = inspect(hwnd);
        let disposition = classify(&c, ctx.cfg);
        if disposition == Disposition::Ignore {
            return TRUE;
        }
        let rect = window_rect(hwnd);
        let monitor = crate::monitor::handle_of(hwnd);
        ctx.out.push(WindowInfo {
            hwnd: hwnd.0 as isize,
            title: c.title,
            class: c.class,
            exe: c.exe,
            rect,
            frame: frame_bounds(hwnd, rect),
            disposition,
            monitor,
        });
        TRUE
    }

    let mut ctx = Ctx {
        cfg,
        exclude,
        out: Vec::new(),
    };
    // SAFETY: the pointer targets a local that lives across the call, and
    // EnumWindows is synchronous.
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut ctx as *mut Ctx as isize));
    }
    ctx.out
}

/// Only the windows that should be tiled, on the given monitor.
pub fn tileable_on(
    cfg: &Config,
    exclude: &HashSet<isize>,
    monitor: &crate::monitor::Monitor,
) -> Vec<WindowInfo> {
    tileable_from(&enumerate(cfg, exclude), monitor)
}

/// Tileable windows on `monitor`, from an enumeration already performed.
///
/// Retiling several monitors used to enumerate the whole desktop once per
/// monitor. On a dual-head setup that doubled the most expensive operation the
/// program performs, for no reason: one pass already knows which monitor every
/// window is on.
pub fn tileable_from(all: &[WindowInfo], monitor: &crate::monitor::Monitor) -> Vec<WindowInfo> {
    all.iter()
        .filter(|w| w.disposition == Disposition::Tile && w.monitor == monitor.handle)
        .cloned()
        .collect()
}

/// Move and size a set of windows in one batched transaction.
///
/// `BeginDeferWindowPos` lets the whole monitor retile in a single pass, so the
/// user sees one atomic rearrangement instead of a cascade. Returns the number
/// of windows that could not be placed — typically elevated windows, which a
/// non-elevated SuperTile is not permitted to move.
/// Does this window belong to a process we are not permitted to touch?
///
/// Windows enforces User Interface Privilege Isolation: a process at medium
/// integrity cannot send input to, or reposition, a window owned by an elevated
/// one. `SetWindowPos` does not fail loudly — it returns an error whose only
/// distinguishing mark is `ERROR_ACCESS_DENIED`, which is why an elevated
/// window looks exactly like a window that simply refuses to move.
///
/// Probing by opening the owning process is the direct question, and the answer
/// arrives the same way: `OpenProcess` for a token query is itself denied
/// across the boundary. That denial *is* the signal, so it is treated as one
/// rather than as an error.
///
/// SuperTile deliberately stays unelevated, so this exists to explain a window
/// rather than to do anything about it. Knowing that a window cannot be moved
/// is worth reporting; running the whole window manager with full rights over
/// the session to move it is not a trade worth making.
pub fn needs_elevation(hwnd: HWND) -> bool {
    use windows::Win32::Foundation::ERROR_ACCESS_DENIED;

    use windows::Win32::System::Threading::PROCESS_QUERY_INFORMATION;

    let mut pid = 0u32;
    // SAFETY: valid out-param; the HWND came from a live enumeration.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return false;
    }
    // Full `PROCESS_QUERY_INFORMATION`, not the LIMITED variant.
    //
    // This first used LIMITED, on the reasoning that it was the least privilege
    // that answered the question. It answers a different one: LIMITED exists
    // precisely so that a lower-integrity process *can* ask basic things about
    // a higher one, so it succeeds across the boundary and every elevated
    // window was reported as ordinary. An elevated PowerShell was still given a
    // cell, and still jumped straight out of it.
    //
    // The full right is refused by the mandatory integrity policy, which is the
    // same boundary that stops SetWindowPos working -- so it tests the thing
    // that actually matters.
    // SAFETY: the handle is closed immediately on success.
    match unsafe { OpenProcess(PROCESS_QUERY_INFORMATION, false, pid) } {
        Ok(h) => {
            // SAFETY: h came from a successful OpenProcess.
            unsafe {
                let _ = CloseHandle(h);
            }
            false
        }
        // Any other failure -- the process exited between the two calls, say --
        // is not evidence of elevation and must not be reported as such.
        Err(e) => e.code() == ERROR_ACCESS_DENIED.to_hresult(),
    }
}

pub fn apply(placements: &[(&WindowInfo, Rect)]) -> usize {
    if placements.is_empty() {
        return 0;
    }

    // SAFETY: count is the exact number of DeferWindowPos calls that follow.
    let mut hdwp = match unsafe { BeginDeferWindowPos(placements.len() as i32) } {
        Ok(h) => h,
        // Fall back to individual calls; correctness beats atomicity.
        Err(_) => return apply_individually(placements),
    };

    let flags = SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS;
    for (w, zone) in placements {
        let t = w.target_rect_for(*zone);
        // SAFETY: hdwp is a live defer structure; the HWND came from a live
        // enumeration. DeferWindowPos returns a new handle and invalidates the
        // old one, so hdwp is reassigned rather than reused.
        match unsafe {
            DeferWindowPos(
                hdwp,
                w.handle(),
                None,
                t.left,
                t.top,
                t.width(),
                t.height(),
                flags,
            )
        } {
            Ok(next) => hdwp = next,
            Err(_) => return apply_individually(placements),
        }
    }

    // SAFETY: hdwp is the handle returned by the final DeferWindowPos.
    match unsafe { EndDeferWindowPos(hdwp) } {
        Ok(()) => 0,
        Err(_) => apply_individually(placements),
    }
}

fn apply_individually(placements: &[(&WindowInfo, Rect)]) -> usize {
    let mut failed = 0;
    let flags = SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER;
    for (w, zone) in placements {
        let t = w.target_rect_for(*zone);
        // SAFETY: HWND from a live enumeration; failure is reported by value.
        let r = unsafe {
            SetWindowPos(
                w.handle(),
                None,
                t.left,
                t.top,
                t.width(),
                t.height(),
                flags,
            )
        };
        if r.is_err() {
            failed += 1;
        }
    }
    failed
}

/// The window's *visible* bounds, as the user sees them on screen.
///
/// `GetWindowRect` includes the invisible resize border, so drawing anything
/// aligned to it lands several pixels off. Falls back to the window rect when
/// DWM has no answer.
pub fn visible_frame(hwnd: HWND) -> Rect {
    let r = window_rect(hwnd);
    frame_bounds(hwnd, r)
}

/// Is either Ctrl key held down right now?
///
/// Ctrl asks for a split on an edge drop. Shift was the obvious choice and is
/// spoken for: FancyZones users expect it to detach a window from the grid, and
/// matching that costs nothing while contradicting it costs muscle memory.
pub fn ctrl_held() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL};
    // SAFETY: GetAsyncKeyState takes an integer and cannot fail.
    let state = unsafe { GetAsyncKeyState(VK_CONTROL.0 as i32) };
    (state as u16 & 0x8000) != 0
}

/// Is either Shift key held down right now?
///
/// Used to separate a deliberate split from an ordinary edge drop. Read from
/// the physical key rather than this thread's input queue, because the drag
/// belongs to another process.
pub fn shift_held() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_SHIFT};
    // SAFETY: GetAsyncKeyState takes an integer and cannot fail.
    let state = unsafe { GetAsyncKeyState(VK_SHIFT.0 as i32) };
    (state as u16 & 0x8000) != 0
}

/// Is the primary mouse button held down right now?
///
/// Used to tell a user's drag from an application moving its own window.
/// `GetAsyncKeyState` reads the physical button rather than this thread's input
/// queue, which is the point: the drag belongs to another process entirely.
pub fn mouse_left_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    // Swapped buttons still report through VK_LBUTTON: Windows maps the
    // primary button to it regardless of handedness.
    // SAFETY: GetAsyncKeyState takes an integer and cannot fail.
    let state = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) };
    (state as u16 & 0x8000) != 0
}

/// Ask a window what is under the cursor: title bar, border, client area.
///
/// `WM_NCHITTEST` is answered by the window itself, so this reflects whatever
/// the application actually treats as a title bar — which for the Chromium and
/// Electron windows that dominate a modern desktop is a custom-drawn strip that
/// no amount of geometry would identify.
///
/// Returns `None` when the window does not answer promptly. A hung application
/// must not stall the window manager, so the send is bounded and abortable.
pub fn hit_test(hwnd: HWND, x: i32, y: i32) -> Option<u32> {
    // MAKELPARAM packs two signed 16-bit coordinates; a window on a secondary
    // monitor to the left has negative x, hence the cast through i16.
    let packed = ((y as i16 as u16 as isize) << 16) | (x as i16 as u16 as isize);
    let mut result: usize = 0;
    // SAFETY: WM_NCHITTEST carries no pointer, so nothing has to be marshalled
    // across the process boundary; `result` is a valid out-param.
    let ok = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_NCHITTEST,
            WPARAM(0),
            LPARAM(packed),
            SMTO_ABORTIFHUNG,
            50,
            Some(&mut result),
        )
    };
    (ok.0 != 0).then_some(result as u32)
}

/// The smallest size a window will accept, in pixels.
///
/// Asked of the window itself with `WM_GETMINMAXINFO`. Many applications
/// enforce a minimum -- Electron shells such as Signal and Steam, consoles
/// that size in character cells, Qt dialogs with a `minimumSize`. Handing one
/// a smaller rectangle does not shrink it: it clamps, overflows its zone and
/// overlaps its neighbours, and the tiler then re-issues the same doomed
/// placement on every pass.
///
/// `SendMessageTimeoutW` with `SMTO_ABORTIFHUNG`, because this is a
/// synchronous call into another process's message loop and a hung
/// application must not take the window manager down with it.
pub fn min_size(hwnd: HWND) -> (i32, i32) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, MINMAXINFO, SMTO_ABORTIFHUNG, WM_GETMINMAXINFO,
    };

    let mut info = MINMAXINFO {
        ptMinTrackSize: windows::Win32::Foundation::POINT { x: 0, y: 0 },
        ptMaxTrackSize: windows::Win32::Foundation::POINT {
            x: i32::MAX,
            y: i32::MAX,
        },
        ..Default::default()
    };
    let mut result: usize = 0;
    // SAFETY: `info` is a valid MINMAXINFO for the duration of the call; the
    // 50 ms timeout plus SMTO_ABORTIFHUNG bounds a hung target.
    unsafe {
        let _ = SendMessageTimeoutW(
            hwnd,
            WM_GETMINMAXINFO,
            WPARAM(0),
            LPARAM(&mut info as *mut MINMAXINFO as isize),
            SMTO_ABORTIFHUNG,
            50,
            Some(&mut result),
        );
    }
    let (w, h) = (info.ptMinTrackSize.x, info.ptMinTrackSize.y);
    // A window that does not handle the message leaves the zeros we seeded.
    // Treat anything implausible as "no minimum" rather than inventing one.
    let sane = |v: i32| if (1..20_000).contains(&v) { v } else { 0 };
    (sane(w), sane(h))
}

/// Is this window pinned above all others?
pub fn is_topmost(hwnd: HWND) -> bool {
    // SAFETY: GetWindowLongPtrW accepts any HWND and reports 0 on failure.
    let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    ex & WS_EX_TOPMOST.0 != 0
}

/// Pin or unpin a window above all others.
///
/// Returns false if the change was refused, which happens for windows owned by
/// a more privileged process. `SWP_NOACTIVATE` keeps the user's focus where it
/// is: pinning a background window should not yank it forward.
pub fn set_topmost(hwnd: HWND, topmost: bool) -> bool {
    let after = if topmost {
        HWND_TOPMOST
    } else {
        HWND_NOTOPMOST
    };
    // SAFETY: HWND from a live enumeration; failure is reported by value.
    unsafe {
        SetWindowPos(
            hwnd,
            Some(after),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        )
    }
    .is_ok()
}

/// Is the window still alive and on screen?
///
/// Menu entries are built from a snapshot; by the time the user clicks one, the
/// window may have closed. Every action re-checks with this first.
pub fn is_live(hwnd: HWND) -> bool {
    // SAFETY: both tolerate stale handles, which is the point.
    unsafe { IsWindow(Some(hwnd)).as_bool() && IsWindowVisible(hwnd).as_bool() }
}

/// Every window a user could reasonably pick from a list: visible, titled and
/// **not minimised**.
///
/// Unlike [`tileable_on`], this deliberately includes floating windows — the
/// point of the list is to act on windows the tiler is *not* managing.
pub fn listable(cfg: &Config, exclude: &HashSet<isize>) -> Vec<WindowInfo> {
    let mut out: Vec<WindowInfo> = enumerate(cfg, exclude)
        .into_iter()
        .filter(|w| w.disposition != Disposition::Ignore)
        .filter(|w| !w.title.trim().is_empty())
        // Minimised windows have a meaningless rect and cannot be highlighted.
        .filter(|w| {
            // SAFETY: IsIconic accepts any HWND.
            !unsafe { IsIconic(w.handle()) }.as_bool()
        })
        .collect();
    // Stable, human-friendly ordering: by application, then title.
    out.sort_by(|a, b| {
        let ae = a.exe.to_lowercase();
        let be = b.exe.to_lowercase();
        ae.cmp(&be)
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
            .then_with(|| a.hwnd.cmp(&b.hwnd))
    });
    out
}

/// Un-maximise a window so it can be tiled. A maximised window ignores
/// `SetWindowPos` sizing.
/// Is the window maximised?
pub fn is_maximised(hwnd: HWND) -> bool {
    // SAFETY: IsZoomed accepts any HWND and reports by value.
    unsafe { IsZoomed(hwnd).as_bool() }
}

/// Make a window genuinely fullscreen: the whole monitor, no frame, over the
/// taskbar.
///
/// Distinct from maximising, which Windows fits to the *work area* and leaves
/// the frame on. What people usually mean by fullscreen is the monitor's whole
/// bounds with nothing else visible, and no application offers it consistently.
///
/// The caller is responsible for excluding the window from tiling first; a
/// fullscreen window that the tiler still owns would be dragged back into its
/// cell on the next pass.
pub fn set_fullscreen(hwnd: HWND, bounds: Rect) {
    // SAFETY: our own call on a live window. SW_RESTORE first because a
    // maximised window ignores an explicit position, which is the same reason
    // restore_if_maximized exists.
    unsafe {
        if IsZoomed(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOP),
            bounds.left,
            bounds.top,
            bounds.width(),
            bounds.height(),
            SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        );
    }
}

pub fn restore_if_maximized(hwnd: HWND) {
    // SAFETY: IsZoomed/ShowWindow accept any HWND.
    unsafe {
        if IsZoomed(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
    }
}

/// Bring a window to the foreground and focus it.
pub fn focus(hwnd: HWND) {
    // SAFETY: both accept any HWND and report failure by value.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

pub fn foreground() -> Option<HWND> {
    // SAFETY: GetForegroundWindow takes no arguments and may return null.
    let h = unsafe { GetForegroundWindow() };
    if h.is_invalid() {
        None
    } else {
        Some(h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Rule;

    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn task_manager_floats_rather_than_taking_a_cell() {
        let c = Candidate {
            exe: "C:/Windows/System32/Taskmgr.exe".into(),
            class: "TaskManagerWindow".into(),
            title: "Aktivitetshanteraren".into(),
            ..Candidate::default()
        };
        // Float, not Ignore: it still belongs in the window list.
        assert_eq!(classify(&c, &Config::default()), Disposition::Float);
    }

    #[test]
    fn the_default_float_list_is_matched_case_insensitively() {
        // Windows reports Taskmgr.exe, TASKMGR.EXE and taskmgr.exe from
        // different APIs; all three are the same program.
        for exe in ["C:/W/Taskmgr.exe", "C:/W/TASKMGR.EXE", "C:/W/taskmgr.exe"] {
            let c = Candidate {
                exe: exe.into(),
                title: "Task Manager".into(),
                ..Candidate::default()
            };
            assert_eq!(
                classify(&c, &Config::default()),
                Disposition::Float,
                "{exe}"
            );
        }
    }

    #[test]
    fn the_float_list_does_not_resurrect_untitled_scaffolding() {
        // These programs own parking windows like any other; floating those
        // would put invisible entries into the layout, which is the bug the
        // ordering of the checks exists to prevent.
        let c = Candidate {
            exe: "C:/Windows/System32/Taskmgr.exe".into(),
            title: "   ".into(),
            ..Candidate::default()
        };
        assert_eq!(classify(&c, &Config::default()), Disposition::Ignore);
    }

    #[test]
    fn a_user_rule_still_beats_the_default_float_list() {
        // The list is a default, not a policy. Someone who wants Task Manager
        // tiled must be able to say so.
        let mut cfg = Config::default();
        cfg.rules.push(Rule {
            exe: Some("taskmgr.exe".into()),
            action: RuleAction::Tile,
            ..Rule::default()
        });
        let c = Candidate {
            exe: "C:/Windows/System32/Taskmgr.exe".into(),
            title: "Task Manager".into(),
            ..Candidate::default()
        };
        assert_eq!(classify(&c, &cfg), Disposition::Tile);
    }

    #[test]
    fn an_application_that_draws_its_own_title_bar_is_still_tiled() {
        // Bambu Studio, exactly as inspected: wxWidgets with a custom frame,
        // so no WS_CAPTION, but resizable, maximisable, unowned and titled.
        let c = Candidate {
            exe: r"C:\Program Files\Bambu Studioambu-studio.exe".into(),
            class: "wxWindowNR".into(),
            title: "Ej namngiven - BambuStudio".into(),
            visible: true,
            cloaked: false,
            has_owner: false,
            minimized: false,
            width: 1600,
            height: 900,
            style: 0xB60F_0000,
            ex_style: 0x0000_0100,
        };
        assert_eq!(classify(&c, &Config::default()), Disposition::Tile);
    }

    #[test]
    fn a_caption_less_window_that_is_not_an_application_still_floats() {
        // Dropping the caption test must not let genuine popups through: they
        // are caught by ownership, or by not being resizable and maximisable.
        let base = Candidate {
            title: "Something".into(),
            style: 0xB60F_0000,
            ..Candidate::default()
        };
        let owned = Candidate {
            has_owner: true,
            ..base.clone()
        };
        assert_eq!(classify(&owned, &Config::default()), Disposition::Float);

        let fixed = Candidate {
            // No WS_THICKFRAME and no WS_MAXIMIZEBOX.
            style: 0xB60F_0000 & !0x0004_0000 & !0x0001_0000,
            ..base
        };
        assert_eq!(classify(&fixed, &Config::default()), Disposition::Float);
    }

    #[test]
    fn an_ordinary_app_window_is_tiled() {
        assert_eq!(classify(&Candidate::default(), &cfg()), Disposition::Tile);
    }

    #[test]
    fn invisible_windows_are_ignored() {
        let c = Candidate {
            visible: false,
            ..Default::default()
        };
        assert_eq!(classify(&c, &cfg()), Disposition::Ignore);
    }

    #[test]
    fn cloaked_windows_are_ignored() {
        // The UWP ghost-window case: visible per Win32, invisible on screen.
        let c = Candidate {
            cloaked: true,
            class: "ApplicationFrameWindow".into(),
            title: "Mail".into(),
            ..Default::default()
        };
        assert_eq!(classify(&c, &cfg()), Disposition::Ignore);
    }

    #[test]
    fn an_uncloaked_uwp_window_is_tiled() {
        let c = Candidate {
            class: "ApplicationFrameWindow".into(),
            title: "Mail".into(),
            ..Default::default()
        };
        assert_eq!(classify(&c, &cfg()), Disposition::Tile);
    }

    #[test]
    fn tool_windows_are_ignored() {
        let c = Candidate {
            ex_style: WS_EX_TOOLWINDOW.0,
            ..Default::default()
        };
        assert_eq!(classify(&c, &cfg()), Disposition::Ignore);
    }

    #[test]
    fn owned_windows_float() {
        let c = Candidate {
            has_owner: true,
            title: "Save As".into(),
            ..Default::default()
        };
        assert_eq!(classify(&c, &cfg()), Disposition::Float);
    }

    #[test]
    fn untitled_owned_windows_are_ignored_not_floated() {
        // Regression: found by enumerating a live desktop. WinForms parking
        // windows are owned and untitled; classifying them as floats leaked
        // scaffolding into the window list.
        let c = Candidate {
            has_owner: true,
            title: String::new(),
            class: "WindowsForms10.Window.0.app.0".into(),
            ..Default::default()
        };
        assert_eq!(classify(&c, &cfg()), Disposition::Ignore);
    }

    #[test]
    fn untitled_windows_are_ignored() {
        for title in ["", "   ", "\t"] {
            let c = Candidate {
                title: title.into(),
                ..Default::default()
            };
            assert_eq!(classify(&c, &cfg()), Disposition::Ignore, "title {title:?}");
        }
    }

    #[test]
    fn degenerate_geometry_is_ignored() {
        for (w, h) in [(0, 600), (800, 0), (1, 1), (-5, 600)] {
            let c = Candidate {
                width: w,
                height: h,
                ..Default::default()
            };
            assert_eq!(classify(&c, &cfg()), Disposition::Ignore, "{w}x{h}");
        }
    }

    #[test]
    fn minimized_windows_are_ignored() {
        let c = Candidate {
            minimized: true,
            ..Default::default()
        };
        assert_eq!(classify(&c, &cfg()), Disposition::Ignore);
    }

    #[test]
    fn every_system_class_is_ignored() {
        for class in SYSTEM_CLASSES {
            let c = Candidate {
                class: (*class).into(),
                title: "something".into(),
                ..Default::default()
            };
            assert_eq!(classify(&c, &cfg()), Disposition::Ignore, "{class}");
            // Class matching must be case-insensitive.
            let upper = Candidate {
                class: class.to_uppercase(),
                title: "x".into(),
                ..Default::default()
            };
            assert_eq!(
                classify(&upper, &cfg()),
                Disposition::Ignore,
                "{class} uppercased"
            );
        }
    }

    #[test]
    fn the_desktop_is_never_tiled() {
        // Progman with a title is still the desktop.
        let c = Candidate {
            class: "Progman".into(),
            title: "Program Manager".into(),
            ..Default::default()
        };
        assert_eq!(classify(&c, &cfg()), Disposition::Ignore);
    }

    #[test]
    fn shell_executables_are_ignored_regardless_of_path() {
        let c = Candidate {
            exe: r"C:\Windows\SystemApps\Microsoft.Windows.Search_x\SearchHost.exe".into(),
            title: "Search".into(),
            ..Default::default()
        };
        assert_eq!(classify(&c, &cfg()), Disposition::Ignore);
    }

    #[test]
    fn non_resizable_windows_float() {
        // A fixed-size dialog would be mangled by tiling.
        let c = Candidate {
            style: WS_VISIBLE.0 | WS_CAPTION.0 | WS_SYSMENU.0, // no WS_THICKFRAME
            ..Default::default()
        };
        assert_eq!(classify(&c, &cfg()), Disposition::Float);
    }

    #[test]
    fn windows_without_maximizebox_float() {
        let c = Candidate {
            style: WS_VISIBLE.0 | WS_CAPTION.0 | WS_THICKFRAME.0,
            ..Default::default()
        };
        assert_eq!(classify(&c, &cfg()), Disposition::Float);
    }

    #[test]
    fn a_captionless_window_is_judged_on_everything_else() {
        // This test used to assert the opposite, and that assertion was the
        // bug: an application drawing its own title bar has no WS_CAPTION and
        // was floated for it. Resizable, maximisable, unowned and titled is
        // what makes a window an application; who paints its frame is not.
        let c = Candidate {
            style: WS_VISIBLE.0 | WS_THICKFRAME.0 | WS_MAXIMIZEBOX.0,
            ..Default::default()
        };
        assert_eq!(classify(&c, &cfg()), Disposition::Tile);
    }

    // --- user rules override the heuristics --------------------------------

    #[test]
    fn a_rule_can_float_a_window_that_would_tile() {
        let mut c = cfg();
        c.rules.push(Rule {
            exe: Some("app.exe".into()),
            action: RuleAction::Float,
            ..Default::default()
        });
        assert_eq!(classify(&Candidate::default(), &c), Disposition::Float);
    }

    #[test]
    fn a_rule_can_ignore_by_title_substring() {
        let mut c = cfg();
        c.rules.push(Rule {
            title_contains: Some("Picture-in-Picture".into()),
            action: RuleAction::Ignore,
            ..Default::default()
        });
        let w = Candidate {
            title: "Picture-in-Picture".into(),
            ..Default::default()
        };
        assert_eq!(classify(&w, &c), Disposition::Ignore);
        // A sibling window is untouched.
        assert_eq!(classify(&Candidate::default(), &c), Disposition::Tile);
    }

    #[test]
    fn a_rule_can_force_tile_a_window_the_heuristics_would_skip() {
        let mut c = cfg();
        c.rules.push(Rule {
            class: Some("FixedDialog".into()),
            action: RuleAction::Tile,
            ..Default::default()
        });
        let w = Candidate {
            class: "FixedDialog".into(),
            style: WS_VISIBLE.0 | WS_CAPTION.0, // not resizable
            ..Default::default()
        };
        assert_eq!(classify(&w, &c), Disposition::Tile);
    }

    #[test]
    fn a_rule_beats_the_system_class_list() {
        // Escape hatch: the built-in list is a heuristic, not a policy.
        let mut c = cfg();
        c.rules.push(Rule {
            class: Some("WorkerW".into()),
            action: RuleAction::Tile,
            ..Default::default()
        });
        let w = Candidate {
            class: "WorkerW".into(),
            ..Default::default()
        };
        assert_eq!(classify(&w, &c), Disposition::Tile);
    }

    // --- frame compensation ------------------------------------------------

    fn win_with(rect: Rect, frame: Rect) -> WindowInfo {
        WindowInfo {
            hwnd: 1,
            title: "t".into(),
            class: "c".into(),
            exe: "e".into(),
            rect,
            frame,
            disposition: Disposition::Tile,
            monitor: 0,
        }
    }

    #[test]
    fn frame_insets_measure_the_invisible_border() {
        // Typical Windows 11 window: 7px invisible border left/right/bottom.
        let w = win_with(Rect::new(-7, 0, 807, 607), Rect::new(0, 0, 800, 600));
        assert_eq!(w.frame_insets(), (7, 0, 7, 7));
    }

    #[test]
    fn target_rect_makes_the_visible_frame_match_the_zone() {
        let w = win_with(Rect::new(-7, 0, 807, 607), Rect::new(0, 0, 800, 600));
        let zone = Rect::new(100, 100, 1000, 700);
        let t = w.target_rect_for(zone);
        // Requesting `t` should place the *visible* frame exactly on the zone.
        let (l, top, r, b) = w.frame_insets();
        assert_eq!(t.left + l, zone.left);
        assert_eq!(t.top + top, zone.top);
        assert_eq!(t.right - r, zone.right);
        assert_eq!(t.bottom - b, zone.bottom);
    }

    #[test]
    fn zero_insets_are_a_no_op() {
        let r = Rect::new(0, 0, 800, 600);
        let w = win_with(r, r);
        let zone = Rect::new(10, 20, 500, 400);
        assert_eq!(w.target_rect_for(zone), zone);
    }

    #[test]
    fn absurd_insets_are_discarded() {
        // A compositor reporting nonsense must not throw the window off-screen.
        let w = win_with(Rect::new(0, 0, 800, 600), Rect::new(5000, 5000, 6000, 6000));
        let zone = Rect::new(0, 0, 960, 540);
        assert_eq!(w.target_rect_for(zone), zone);
    }

    #[test]
    fn negative_insets_are_discarded() {
        // frame larger than rect => negative inset; treat as zero.
        let w = win_with(Rect::new(10, 10, 100, 100), Rect::new(0, 0, 110, 110));
        let zone = Rect::new(0, 0, 500, 500);
        assert_eq!(w.target_rect_for(zone), zone);
    }

    #[test]
    fn file_name_extraction_handles_both_separators() {
        assert_eq!(file_name_of(r"C:\A\B\app.EXE"), "app.exe");
        assert_eq!(file_name_of("/usr/bin/app"), "app");
        assert_eq!(file_name_of("app.exe"), "app.exe");
        assert_eq!(file_name_of(""), "");
    }

    // --- live enumeration --------------------------------------------------

    #[test]
    fn enumerating_the_live_desktop_is_sane() {
        let c = cfg();
        let windows = enumerate(&c, &HashSet::new());
        for w in &windows {
            assert_ne!(
                w.disposition,
                Disposition::Ignore,
                "ignored windows must be filtered out"
            );
            assert!(w.hwnd != 0);
            // Anything we kept must have a title.
            assert!(!w.title.trim().is_empty(), "{w:?}");
        }
    }

    #[test]
    fn listable_excludes_minimised_and_untitled_windows() {
        let c = cfg();
        for w in listable(&c, &HashSet::new()) {
            assert!(!w.title.trim().is_empty(), "{w:?}");
            // SAFETY: IsIconic accepts any HWND.
            assert!(
                !unsafe { IsIconic(w.handle()) }.as_bool(),
                "minimised: {w:?}"
            );
            assert_ne!(w.disposition, Disposition::Ignore);
        }
    }

    #[test]
    fn listable_is_sorted_deterministically() {
        let c = cfg();
        let a = listable(&c, &HashSet::new());
        let b = listable(&c, &HashSet::new());
        let keys = |v: &Vec<WindowInfo>| {
            v.iter()
                .map(|w| (w.exe.to_lowercase(), w.title.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(&a), keys(&b), "ordering must be stable between calls");
    }

    #[test]
    fn listable_includes_floating_windows() {
        // The list exists so the user can act on windows the tiler ignores.
        let mut c = cfg();
        c.rules.push(Rule {
            exe: Some("explorer.exe".into()),
            action: RuleAction::Float,
            ..Default::default()
        });
        // Nothing to assert about a specific machine, but Float must not be
        // filtered out by construction.
        for w in listable(&c, &HashSet::new()) {
            assert!(matches!(
                w.disposition,
                Disposition::Tile | Disposition::Float
            ));
        }
    }

    #[test]
    fn a_dead_handle_is_not_live() {
        assert!(!is_live(HWND(0xDEAD_BEEFusize as *mut core::ffi::c_void)));
        assert!(!is_live(HWND::default()));
    }

    #[test]
    fn topmost_reads_the_exstyle_bit() {
        // Our own transient windows are a safe subject: create one, flip it.
        let c = cfg();
        if let Some(w) = listable(&c, &HashSet::new()).first() {
            // Only assert the read is consistent with the style bits; do not
            // mutate a real user window in a test.
            let reported = is_topmost(w.handle());
            // SAFETY: reading the style of an enumerated window.
            let ex = unsafe { GetWindowLongPtrW(w.handle(), GWL_EXSTYLE) } as u32;
            assert_eq!(reported, ex & WS_EX_TOPMOST.0 != 0);
        }
    }

    #[test]
    fn excluded_handles_are_never_returned() {
        let c = cfg();
        let all = enumerate(&c, &HashSet::new());
        if let Some(first) = all.first() {
            let mut ex = HashSet::new();
            ex.insert(first.hwnd);
            let filtered = enumerate(&c, &ex);
            assert!(!filtered.iter().any(|w| w.hwnd == first.hwnd));
        }
    }
}

// ---------------------------------------------------------------------------
// Virtual desktops
// ---------------------------------------------------------------------------

thread_local! {
    /// The shell's virtual-desktop manager, created once per thread.
    ///
    /// `IVirtualDesktopManager` is the *public* interface, documented in
    /// `shobjidl_core.h` and stable since Windows 10. It answers two questions
    /// and no more: which desktop a window is on, and whether that is the
    /// current one. The interface that enumerates or switches desktops is a
    /// different, undocumented one which shifts between Windows builds and is
    /// deliberately not used here.
    static VDM: std::cell::RefCell<Option<windows::Win32::UI::Shell::IVirtualDesktopManager>> =
        const { std::cell::RefCell::new(None) };
}

/// Identifier of the virtual desktop a window sits on.
///
/// `None` when the shell will not say — an elevated window, a window that has
/// just been created, or a Windows build without the interface. Callers must
/// treat that as "unknown desktop" rather than as a particular one, or two
/// unknowns would be grouped together as though they were the same place.
pub fn desktop_id(hwnd: HWND) -> Option<String> {
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};

    VDM.with(|cell| {
        let mut slot = cell.try_borrow_mut().ok()?;
        if slot.is_none() {
            // SAFETY: initialising COM for this thread. A repeat call returns
            // RPC_E_CHANGED_MODE or S_FALSE, both of which are fine to ignore:
            // the apartment is already there, which is all this needs.
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            // SAFETY: standard COM activation of a shell-provided class.
            *slot = unsafe { CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL).ok() };
        }
        let vdm: &IVirtualDesktopManager = slot.as_ref()?;
        // SAFETY: `vdm` is a live interface pointer; the GUID is an out-param.
        let guid = unsafe { vdm.GetWindowDesktopId(hwnd) }.ok()?;
        // All-zero means the shell does not know, which is not an identity.
        (guid != windows::core::GUID::zeroed()).then(|| format!("{guid:?}"))
    })
}

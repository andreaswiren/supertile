//! Focus dimming: darken everything except the window you are working in.
//!
//! Useful for playing a game in a window, or for reading one document on a busy
//! desktop. The active window keeps its full brightness; everything behind it
//! is covered by a black layered overlay.
//!
//! ## Two independent layers
//!
//! Windows draws in two z-order bands: ordinary windows, and `WS_EX_TOPMOST`
//! ones. The taskbar and the Start menu live in the topmost band, so a single
//! overlay cannot dim both them and ordinary windows — and they want a
//! *different* level anyway, since a taskbar dimmed to 90% is a taskbar you
//! cannot use.
//!
//! So there are two overlays per monitor:
//!
//! | Layer | Band | Z-order | Dims |
//! |---|---|---|---|
//! | Content | ordinary | directly **below** the active window | every other ordinary window |
//! | Shell | topmost | above the taskbar | the taskbar and Start menu, at its own level |
//!
//! Placing the content overlay directly below the active window is what makes
//! this work without tracking every other window: anything under the overlay is
//! dimmed, and the active window is above it.
//!
//! Both overlays are `WS_EX_TRANSPARENT` and `WS_EX_NOACTIVATE`, so they are
//! invisible to hit-testing — taskbar icons stay clickable through the dim.

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::layout::Rect;
use crate::monitor::Monitor;
use crate::ui::theme;

const CLASS_NAME: &str = "SuperTile.Dim";

/// Dimming settings, persisted in the configuration file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DimConfig {
    pub enabled: bool,
    /// How dark everything except the active window becomes, 0–100.
    /// 100 is black. Around 85 is the useful "almost black" setting.
    pub window_level: u8,
    /// Separate level for the taskbar and Start menu, 0–100.
    ///
    /// Deliberately its own setting: you often still want to read the clock and
    /// click tray icons while the rest of the desktop is nearly black.
    pub taskbar_level: u8,
    /// Follow the foreground window automatically. When false, the window
    /// chosen with "Select window" stays bright regardless of focus.
    pub auto_track: bool,
}

impl Default for DimConfig {
    fn default() -> Self {
        DimConfig {
            enabled: false,
            window_level: 85,
            taskbar_level: 40,
            auto_track: true,
        }
    }
}

impl DimConfig {
    /// Clamp to the supported range, returning any warnings.
    pub fn sanitize(&mut self) -> Vec<String> {
        let mut w = Vec::new();
        if self.window_level > 100 {
            w.push(format!(
                "dimming.window_level was {}; clamped to 100.",
                self.window_level
            ));
            self.window_level = 100;
        }
        if self.taskbar_level > 100 {
            w.push(format!(
                "dimming.taskbar_level was {}; clamped to 100.",
                self.taskbar_level
            ));
            self.taskbar_level = 100;
        }
        w
    }
}

/// Percent (0–100) to a Win32 alpha byte (0–255).
pub fn level_to_alpha(level: u8) -> u8 {
    ((level.min(100) as u16 * 255) / 100) as u8
}

/// One black layered window.
struct Layer {
    hwnd: HWND,
}

impl Layer {
    fn new(topmost: bool) -> Option<Layer> {
        if !theme::class_exists(CLASS_NAME) {
            let _ = theme::register_class(CLASS_NAME, wndproc);
        }
        let class = crate::util::WideStr::new(CLASS_NAME);
        let mut ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        if topmost {
            ex |= WS_EX_TOPMOST;
        }
        // SAFETY: a click-through, non-activating layered window we own.
        let hwnd = unsafe {
            let hinst = GetModuleHandleW(None).unwrap_or_default();
            CreateWindowExW(
                ex,
                class.as_pcwstr(),
                windows::core::PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(hinst.into()),
                None,
            )
        }
        .ok()?;
        if hwnd.is_invalid() {
            return None;
        }
        Some(Layer { hwnd })
    }

    fn set_alpha(&self, alpha: u8) {
        // SAFETY: our own layered window.
        unsafe {
            let _ = SetLayeredWindowAttributes(self.hwnd, COLORREF(0), alpha, LWA_ALPHA);
        }
    }

    /// Cover `area`, with `holes` punched out of it.
    fn shape(&self, area: Rect, holes: &[Rect]) {
        let (w, h) = (area.width(), area.height());
        if w <= 0 || h <= 0 {
            self.hide();
            return;
        }
        // SAFETY: `rgn` is handed to SetWindowRgn, which takes ownership; every
        // temporary region created here is deleted before returning.
        unsafe {
            let rgn = CreateRectRgn(0, 0, w, h);
            for hole in holes {
                // Region coordinates are client-relative.
                let hx0 = hole.left - area.left;
                let hy0 = hole.top - area.top;
                let hx1 = hole.right - area.left;
                let hy1 = hole.bottom - area.top;
                if hx1 <= hx0 || hy1 <= hy0 {
                    continue;
                }
                let cut = CreateRectRgn(hx0, hy0, hx1, hy1);
                CombineRgn(Some(rgn), Some(rgn), Some(cut), RGN_DIFF);
                let _ = DeleteObject(cut.into());
            }
            SetWindowRgn(self.hwnd, Some(rgn), false);
        }
    }

    /// Position and restack. `insert_after` is the window this overlay should
    /// sit directly *below*, or `None` for the top of its band.
    fn place(&self, area: Rect, insert_after: Option<HWND>) {
        let after = insert_after.unwrap_or(HWND_TOP);
        // SAFETY: our own window; SWP_NOACTIVATE keeps focus untouched.
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(after),
                area.left,
                area.top,
                area.width(),
                area.height(),
                SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }

    fn hide(&self) {
        // SAFETY: our own window.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }
}

impl Drop for Layer {
    fn drop(&mut self) {
        if !self.hwnd.is_invalid() {
            // SAFETY: destroying our own window.
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

/// Owns the dim overlays and keeps them in step with the focused window.
pub struct Dimmer {
    cfg: DimConfig,
    /// One per monitor, in the ordinary band.
    content: Vec<Layer>,
    /// One per monitor, in the topmost band.
    shell: Vec<Layer>,
    /// Monitor geometry the overlays were built for.
    areas: Vec<Rect>,
    /// Window explicitly chosen by the user; overrides focus tracking.
    pinned: Option<isize>,
    /// The window currently left bright.
    bright: Option<isize>,
}

impl Dimmer {
    pub fn new(cfg: DimConfig) -> Dimmer {
        Dimmer {
            cfg,
            content: Vec::new(),
            shell: Vec::new(),
            areas: Vec::new(),
            pinned: None,
            bright: None,
        }
    }

    pub fn config(&self) -> DimConfig {
        self.cfg
    }

    pub fn is_enabled(&self) -> bool {
        self.cfg.enabled
    }

    pub fn pinned(&self) -> Option<isize> {
        self.pinned
    }

    /// Every overlay handle, so the tiler and the window list skip them.
    pub fn own_windows(&self) -> Vec<isize> {
        self.content
            .iter()
            .chain(self.shell.iter())
            .map(|l| l.hwnd.0 as isize)
            .collect()
    }

    pub fn set_enabled(&mut self, on: bool) {
        self.cfg.enabled = on;
        if !on {
            self.teardown();
        }
    }

    pub fn set_window_level(&mut self, level: u8) {
        self.cfg.window_level = level.min(100);
    }

    pub fn set_taskbar_level(&mut self, level: u8) {
        self.cfg.taskbar_level = level.min(100);
    }

    pub fn set_auto_track(&mut self, on: bool) {
        self.cfg.auto_track = on;
        if on {
            self.pinned = None;
        }
    }

    /// Pin a specific window as the bright one, and stop following focus.
    pub fn pin(&mut self, hwnd: isize) {
        self.pinned = Some(hwnd);
        self.cfg.auto_track = false;
    }

    pub fn clear_pin(&mut self) {
        self.pinned = None;
    }

    /// Rebuild overlays for the current monitor set.
    fn ensure_layers(&mut self, monitors: &[Monitor]) {
        let areas: Vec<Rect> = monitors.iter().map(|m| m.bounds).collect();
        if areas == self.areas && self.content.len() == areas.len() {
            return;
        }
        self.content.clear();
        self.shell.clear();
        for _ in &areas {
            match (Layer::new(false), Layer::new(true)) {
                (Some(c), Some(s)) => {
                    self.content.push(c);
                    self.shell.push(s);
                }
                _ => {
                    // Out of window handles or the class failed; give up
                    // cleanly rather than dimming half the desktop.
                    self.content.clear();
                    self.shell.clear();
                    self.areas.clear();
                    return;
                }
            }
        }
        self.areas = areas;
    }

    fn teardown(&mut self) {
        self.content.clear();
        self.shell.clear();
        self.areas.clear();
        self.bright = None;
    }

    /// Recompute and reposition everything. Cheap enough to call on every
    /// foreground change.
    ///
    /// `exclude` holds SuperTile's own windows, which must never be treated as
    /// the bright window (otherwise opening the palette would undim the
    /// desktop behind it).
    pub fn update(&mut self, foreground: Option<HWND>, exclude: &[isize]) {
        if !self.cfg.enabled {
            self.teardown();
            return;
        }

        let monitors = crate::monitor::enumerate();
        if monitors.is_empty() {
            return;
        }
        self.ensure_layers(&monitors);
        if self.content.is_empty() {
            return;
        }

        // Decide which window stays bright.
        let bright: Option<HWND> = match self.pinned {
            Some(h) if !self.cfg.auto_track => {
                let hw = HWND(h as *mut core::ffi::c_void);
                crate::window::is_live(hw).then_some(hw)
            }
            _ => {
                let candidate = foreground
                    .filter(|f| !exclude.contains(&(f.0 as isize)))
                    // Clicking the taskbar or opening Start must not make the
                    // shell "the bright window" -- that would undim the whole
                    // taskbar and dim whatever you were actually working in.
                    .filter(|f| !is_shell_window(*f));
                match candidate {
                    Some(f) => Some(f),
                    // Keep the previous choice rather than dimming everything.
                    None => {
                        let prev = self.bright.map(|h| HWND(h as *mut core::ffi::c_void));
                        prev.filter(|h| crate::window::is_live(*h))
                    }
                }
            }
        };
        self.bright = bright.map(|h| h.0 as isize);

        let bright_rect = bright.map(crate::window::visible_frame);
        let bright_monitor = bright
            .and_then(crate::monitor::from_window)
            .map(|m| m.handle);

        let content_alpha = level_to_alpha(self.cfg.window_level);
        let shell_alpha = level_to_alpha(self.cfg.taskbar_level);
        let shell_rects = shell_surface_rects();

        for (i, m) in monitors.iter().enumerate() {
            let Some(content) = self.content.get(i) else {
                break;
            };
            let Some(shell) = self.shell.get(i) else {
                break;
            };

            // --- content layer ---------------------------------------------
            // The work area, not the full monitor: the taskbar strip belongs
            // to the shell layer and its own level. Covering it here would
            // dim it twice, and at the wrong level.
            let content_area = m.work_area;
            if content_alpha == 0 {
                content.hide();
            } else {
                content.set_alpha(content_alpha);
                content.shape(content_area, &[]);
                // Directly below the bright window when it is on this monitor
                // and is an ordinary window.
                //
                // SetWindowPos promotes a non-topmost window to topmost when it
                // is inserted after a topmost one. If the focused window is
                // topmost, inserting after it would drag this overlay into the
                // topmost band, where it would cover the taskbar at the window
                // level and defeat the whole point of a separate shell layer.
                // In that case sit at the top of the ordinary band instead: a
                // topmost focused window is already above everything there.
                let after = match (bright, bright_monitor) {
                    (Some(h), Some(mh)) if mh == m.handle && !crate::window::is_topmost(h) => {
                        Some(h)
                    }
                    _ => None,
                };
                content.place(content_area, after);
            }

            // --- shell layer ------------------------------------------------
            // Only the parts of this monitor occupied by shell surfaces, minus
            // the bright window so a maximised window is not double-dimmed.
            let on_this: Vec<Rect> = shell_rects
                .iter()
                .filter(|r| r.intersection_area(&m.bounds) > 0)
                .copied()
                .collect();

            if shell_alpha == 0 || on_this.is_empty() {
                shell.hide();
            } else {
                shell.set_alpha(shell_alpha);
                // Only the shell surfaces, minus the bright window so a
                // maximised window is not dimmed twice.
                shell.shape_union(m.bounds, &on_this, bright_rect.as_ref());
                shell.place(m.bounds, None);
            }
        }
    }
}

impl Layer {
    /// Cover only `keep` (intersected with `area`), minus `hole`.
    fn shape_union(&self, area: Rect, keep: &[Rect], hole: Option<&Rect>) {
        let (w, h) = (area.width(), area.height());
        if w <= 0 || h <= 0 {
            self.hide();
            return;
        }
        // SAFETY: `rgn` is handed to SetWindowRgn, which takes ownership; all
        // temporaries are deleted here.
        unsafe {
            let rgn = CreateRectRgn(0, 0, 0, 0); // empty
            for r in keep {
                let x0 = (r.left - area.left).max(0);
                let y0 = (r.top - area.top).max(0);
                let x1 = (r.right - area.left).min(w);
                let y1 = (r.bottom - area.top).min(h);
                if x1 <= x0 || y1 <= y0 {
                    continue;
                }
                let add = CreateRectRgn(x0, y0, x1, y1);
                CombineRgn(Some(rgn), Some(rgn), Some(add), RGN_OR);
                let _ = DeleteObject(add.into());
            }
            if let Some(hole) = hole {
                let x0 = hole.left - area.left;
                let y0 = hole.top - area.top;
                let x1 = hole.right - area.left;
                let y1 = hole.bottom - area.top;
                if x1 > x0 && y1 > y0 {
                    let cut = CreateRectRgn(x0, y0, x1, y1);
                    CombineRgn(Some(rgn), Some(rgn), Some(cut), RGN_DIFF);
                    let _ = DeleteObject(cut.into());
                }
            }
            SetWindowRgn(self.hwnd, Some(rgn), false);
        }
    }
}

/// Is this one of the shell's own windows (taskbar, Start, search)?
///
/// Focus moves to these constantly -- every Start press, every tray click --
/// and treating them as the focused *application* would make the dim flicker
/// between states.
fn is_shell_window(hwnd: HWND) -> bool {
    let mut buf = [0u16; 128];
    // SAFETY: buf is a valid 128-element array; GetClassNameW truncates.
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return false;
    }
    let class = crate::util::wide_to_string(&buf[..n as usize]);
    SHELL_CLASSES.iter().any(|c| c.eq_ignore_ascii_case(&class))
}

/// Window classes belonging to the shell surfaces the shell layer dims.
const SHELL_CLASSES: &[&str] = &[
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Windows.UI.Core.CoreWindow",
    "XamlExplorerHostIslandWindow",
    "Shell_InputSwitchTopLevelWindow",
    "TopLevelWindowForOverflowXamlIsland",
];

/// Screen rectangles occupied by the shell: taskbars, and the Start menu or
/// search flyout while they are open.
///
/// These live in the topmost band, so the ordinary content overlay can never
/// reach them; they need the separate shell layer.
fn shell_surface_rects() -> Vec<Rect> {
    const CLASSES: &[&str] = &[
        "Shell_TrayWnd",                // primary taskbar
        "Shell_SecondaryTrayWnd",       // taskbar on other monitors
        "Windows.UI.Core.CoreWindow",   // Start menu / search on Windows 10-11
        "XamlExplorerHostIslandWindow", // newer Start / search host
    ];

    let mut out = Vec::new();
    for class in CLASSES {
        let name = crate::util::WideStr::new(class);
        let mut after = HWND::default();
        // Walk every window of this class, not just the first: there is one
        // secondary taskbar per extra monitor.
        loop {
            // SAFETY: FindWindowExW tolerates null parents and a null
            // predecessor; it returns an error when nothing else matches.
            let found = unsafe {
                FindWindowExW(
                    None,
                    if after.is_invalid() {
                        None
                    } else {
                        Some(after)
                    },
                    name.as_pcwstr(),
                    windows::core::PCWSTR::null(),
                )
            };
            let Ok(h) = found else { break };
            if h.is_invalid() {
                break;
            }
            after = h;

            // SAFETY: h came from FindWindowExW.
            if !unsafe { IsWindowVisible(h) }.as_bool() {
                continue;
            }
            let mut r = RECT::default();
            // SAFETY: valid out-param.
            if unsafe { GetWindowRect(h, &mut r) }.is_err() {
                continue;
            }
            let rect = Rect::new(r.left, r.top, r.right, r.bottom);
            // Skip the zero-size and full-screen sentinels these classes use
            // when they are not actually on screen.
            if rect.width() <= 1 || rect.height() <= 1 {
                continue;
            }
            out.push(rect);
        }
    }
    out
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: paired with EndPaint.
            let dc = unsafe { BeginPaint(hwnd, &mut ps) };
            if !dc.is_invalid() {
                let mut r = RECT::default();
                // SAFETY: valid out-param.
                let _ = unsafe { GetClientRect(hwnd, &mut r) };
                theme::fill_rect(dc, Rect::new(r.left, r.top, r.right, r.bottom), COLORREF(0));
            }
            // SAFETY: paired with BeginPaint.
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        // Never take focus or intercept the mouse.
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // SAFETY: default handling for messages we do not process; every
        // argument came from the system unchanged.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sensible() {
        let d = DimConfig::default();
        assert!(!d.enabled, "dimming must be opt-in");
        assert!(d.auto_track);
        // "Almost black" for windows, readable for the taskbar.
        assert!(
            d.window_level >= 70 && d.window_level <= 95,
            "{}",
            d.window_level
        );
        assert!(
            d.taskbar_level < d.window_level,
            "the taskbar must stay more readable than the desktop"
        );
    }

    #[test]
    fn levels_map_to_the_full_alpha_range() {
        assert_eq!(level_to_alpha(0), 0);
        assert_eq!(level_to_alpha(100), 255);
        assert_eq!(level_to_alpha(50), 127);
        // Out of range input must not wrap around into "barely dimmed".
        assert_eq!(level_to_alpha(200), 255);
        assert_eq!(level_to_alpha(u8::MAX), 255);
    }

    #[test]
    fn alpha_is_monotonic_in_the_level() {
        let mut last = 0u8;
        for level in 0..=100u8 {
            let a = level_to_alpha(level);
            assert!(a >= last, "level {level} produced {a} after {last}");
            last = a;
        }
    }

    #[test]
    fn sanitize_clamps_out_of_range_levels() {
        let mut d = DimConfig {
            window_level: 250,
            taskbar_level: 200,
            ..Default::default()
        };
        let w = d.sanitize();
        assert_eq!(d.window_level, 100);
        assert_eq!(d.taskbar_level, 100);
        assert_eq!(w.len(), 2);
        // In-range values are untouched and silent.
        let mut ok = DimConfig::default();
        assert!(ok.sanitize().is_empty());
    }

    #[test]
    fn config_round_trips_through_json() {
        let d = DimConfig {
            enabled: true,
            window_level: 90,
            taskbar_level: 30,
            auto_track: false,
        };
        let text = serde_json::to_string(&d).unwrap();
        assert_eq!(serde_json::from_str::<DimConfig>(&text).unwrap(), d);
    }

    #[test]
    fn a_partial_config_keeps_the_other_defaults() {
        let d: DimConfig = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(d.enabled);
        assert_eq!(d.window_level, DimConfig::default().window_level);
        assert_eq!(d.taskbar_level, DimConfig::default().taskbar_level);
    }

    #[test]
    fn a_disabled_dimmer_owns_no_windows() {
        let mut d = Dimmer::new(DimConfig::default());
        assert!(!d.is_enabled());
        d.update(None, &[]);
        assert!(
            d.own_windows().is_empty(),
            "nothing should be created while disabled"
        );
    }

    #[test]
    fn enabling_creates_one_overlay_pair_per_monitor_and_disabling_frees_them() {
        let monitors = crate::monitor::enumerate().len();
        let mut d = Dimmer::new(DimConfig {
            enabled: true,
            ..Default::default()
        });
        d.update(None, &[]);
        assert_eq!(
            d.own_windows().len(),
            monitors * 2,
            "expected a content and a shell layer per monitor"
        );
        d.set_enabled(false);
        assert!(
            d.own_windows().is_empty(),
            "overlays must be destroyed when disabled"
        );
    }

    #[test]
    fn overlays_are_click_through_and_never_activate() {
        let mut d = Dimmer::new(DimConfig {
            enabled: true,
            ..Default::default()
        });
        d.update(None, &[]);
        for h in d.own_windows() {
            let hwnd = HWND(h as *mut core::ffi::c_void);
            // SAFETY: reading the style of our own window.
            let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
            assert_ne!(
                ex & WS_EX_TRANSPARENT.0,
                0,
                "dim overlay must not eat clicks"
            );
            assert_ne!(
                ex & WS_EX_NOACTIVATE.0,
                0,
                "dim overlay must not steal focus"
            );
            assert_ne!(ex & WS_EX_LAYERED.0, 0);
            assert_ne!(ex & WS_EX_TOOLWINDOW.0, 0, "must not appear in Alt-Tab");
        }
    }

    #[test]
    fn pinning_a_window_disables_focus_tracking() {
        let mut d = Dimmer::new(DimConfig {
            enabled: true,
            ..Default::default()
        });
        assert!(d.config().auto_track);
        d.pin(1234);
        assert_eq!(d.pinned(), Some(1234));
        assert!(!d.config().auto_track, "pinning must stop following focus");
        // Turning tracking back on clears the pin.
        d.set_auto_track(true);
        assert_eq!(d.pinned(), None);
    }

    #[test]
    fn updating_repeatedly_does_not_leak_windows() {
        let monitors = crate::monitor::enumerate().len();
        let mut d = Dimmer::new(DimConfig {
            enabled: true,
            ..Default::default()
        });
        for _ in 0..10 {
            d.update(crate::window::foreground(), &[]);
        }
        assert_eq!(
            d.own_windows().len(),
            monitors * 2,
            "layers must be reused, not recreated"
        );
        d.set_enabled(false);
    }

    #[test]
    fn our_own_windows_are_never_chosen_as_the_bright_one() {
        let mut d = Dimmer::new(DimConfig {
            enabled: true,
            ..Default::default()
        });
        if let Some(fg) = crate::window::foreground() {
            d.update(Some(fg), &[fg.0 as isize]);
            assert_ne!(
                d.bright,
                Some(fg.0 as isize),
                "an excluded window must not be treated as the focus"
            );
        }
        d.set_enabled(false);
    }

    #[test]
    fn shell_surfaces_are_found_on_a_live_desktop() {
        let rects = shell_surface_rects();
        // Any Windows session has at least a primary taskbar.
        assert!(
            !rects.is_empty(),
            "no shell surfaces found; taskbar dimming would be a no-op"
        );
        for r in &rects {
            assert!(
                r.width() > 1 && r.height() > 1,
                "degenerate shell rect {r:?}"
            );
        }
    }
}

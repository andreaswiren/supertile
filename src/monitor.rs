//! Monitor enumeration, work areas and per-monitor DPI.

use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, POINT, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HDC, HMONITOR,
    MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;

use crate::layout::Rect;
use crate::util::wide_to_string;

/// The DPI Windows reports for a 100%-scaled display.
pub const BASE_DPI: u32 = 96;

/// A display, as SuperTile sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    pub handle: isize,
    /// Full monitor bounds in virtual-screen coordinates.
    pub bounds: Rect,
    /// Bounds minus the taskbar and any other appbars — what we tile into.
    pub work_area: Rect,
    /// Effective DPI; 96 is 100% scaling.
    pub dpi: u32,
    /// GDI device name, e.g. `\\.\DISPLAY1`.
    pub device: String,
    pub is_primary: bool,
}

impl Monitor {
    /// Scale a design-time (96 DPI) value to this monitor.
    pub fn scale(&self, dip: i32) -> i32 {
        // Round to nearest rather than truncating: at 150% a 1px hairline must
        // not disappear.
        let scaled = (dip as i64 * self.dpi as i64 + (BASE_DPI as i64 / 2)) / BASE_DPI as i64;
        // Saturate rather than wrap: a wrapped negative would become an
        // inverted rectangle by the time it reached SetWindowPos.
        scaled.clamp(i32::MIN as i64, i32::MAX as i64) as i32
    }

    pub fn scale_u32(&self, dip: u32) -> u32 {
        self.scale(dip as i32).max(0) as u32
    }

    pub fn hmonitor(&self) -> HMONITOR {
        HMONITOR(self.handle as *mut core::ffi::c_void)
    }
}

fn rect_from(r: RECT) -> Rect {
    Rect::new(r.left, r.top, r.right, r.bottom)
}

/// Enumerate all active displays.
///
/// The result is sorted by position (top-to-bottom, then left-to-right) so the
/// ordering is deterministic; `EnumDisplayMonitors` makes no such guarantee and
/// the geometry-memory fingerprint depends on a stable order.
pub fn enumerate() -> Vec<Monitor> {
    let mut monitors: Vec<Monitor> = Vec::new();

    unsafe extern "system" fn cb(h: HMONITOR, _hdc: HDC, _r: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the &mut Vec<Monitor> we passed to
        // EnumDisplayMonitors below, which outlives the enumeration.
        let out = unsafe { &mut *(data.0 as *mut Vec<Monitor>) };
        if let Some(m) = describe(h) {
            out.push(m);
        }
        TRUE
    }

    // SAFETY: passing a pointer to a local Vec that lives across the call;
    // EnumDisplayMonitors is synchronous, so the borrow cannot escape.
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(cb),
            LPARAM(&mut monitors as *mut Vec<Monitor> as isize),
        );
    }

    monitors.sort_by_key(|m| (m.bounds.top, m.bounds.left));
    monitors
}

/// Read bounds, work area and DPI for one monitor handle.
pub fn describe(h: HMONITOR) -> Option<Monitor> {
    let mut info = MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    };

    // SAFETY: cbSize declares the MONITORINFOEXW size, so GetMonitorInfoW
    // fills szDevice as well as the base struct.
    let ok = unsafe { GetMonitorInfoW(h, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) };
    if !ok.as_bool() {
        return None;
    }

    let mut dpi_x = BASE_DPI;
    let mut dpi_y = BASE_DPI;
    // SAFETY: both out-params are valid u32s. Failure leaves them untouched,
    // which is why they are pre-seeded with the 100% value.
    unsafe {
        let _ = GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
    }
    // Non-square DPI is theoretically possible but not meaningfully supported
    // by Windows shell scaling; the horizontal value is what UI scaling uses.
    let dpi = if dpi_x == 0 { BASE_DPI } else { dpi_x };

    let work_area = rect_from(info.monitorInfo.rcWork);
    let bounds = rect_from(info.monitorInfo.rcMonitor);

    Some(Monitor {
        handle: h.0 as isize,
        bounds,
        // A zero-sized work area means the shell has not settled yet; fall back
        // to full bounds so we never try to tile into nothing.
        work_area: if work_area.is_empty() {
            bounds
        } else {
            work_area
        },
        dpi,
        device: wide_to_string(&info.szDevice),
        is_primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
    })
}

/// Just the monitor handle for a window, without reading its full description.
///
/// `describe` costs a `GetMonitorInfoW` and a `GetDpiForMonitor`; callers that
/// only need to group windows by monitor do not need either.
pub fn handle_of(hwnd: windows::Win32::Foundation::HWND) -> isize {
    // SAFETY: MONITOR_DEFAULTTONEAREST guarantees a valid handle for any HWND.
    let h = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    h.0 as isize
}

/// The monitor a window is on (nearest, if it straddles or is off-screen).
pub fn from_window(hwnd: windows::Win32::Foundation::HWND) -> Option<Monitor> {
    // SAFETY: MONITOR_DEFAULTTONEAREST guarantees a valid handle for any HWND.
    let h = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    describe(h)
}

/// The monitor containing a virtual-screen point.
pub fn from_point(x: i32, y: i32) -> Option<Monitor> {
    // SAFETY: MONITOR_DEFAULTTONEAREST always yields a valid handle.
    let h = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) };
    describe(h)
}

pub fn primary() -> Option<Monitor> {
    // SAFETY: MONITOR_DEFAULTTOPRIMARY always yields the primary handle.
    let h = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    describe(h)
}

/// A stable identifier for the current display arrangement.
///
/// Geometry memory is scoped by this value so that positions recorded on a
/// docked triple-head setup are not replayed onto a laptop panel. `HMONITOR`
/// values are recycled by Windows and cannot be persisted, so the fingerprint
/// is built from device names and geometry instead.
///
/// The hash is FNV-1a rather than `DefaultHasher`: the value is written to disk
/// and must not change when the Rust version does.
pub fn fingerprint(monitors: &[Monitor]) -> String {
    let mut parts: Vec<String> = monitors
        .iter()
        .map(|m| {
            format!(
                "{}:{}x{}@{},{}:{}",
                m.device,
                m.bounds.width(),
                m.bounds.height(),
                m.bounds.left,
                m.bounds.top,
                m.dpi
            )
        })
        .collect();
    // Sorted so the fingerprint does not depend on enumeration order.
    parts.sort();
    format!("{:016x}", fnv1a64(parts.join("|").as_bytes()))
}

/// FNV-1a, 64-bit. Chosen for stability across toolchain versions, not for
/// cryptographic strength — this is a cache key, never a security boundary.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(device: &str, bounds: Rect, dpi: u32) -> Monitor {
        Monitor {
            handle: 1,
            bounds,
            work_area: bounds,
            dpi,
            device: device.to_string(),
            is_primary: true,
        }
    }

    #[test]
    fn fnv1a_matches_known_vectors() {
        // Reference values for FNV-1a 64. If these ever change, persisted
        // fingerprints silently invalidate.
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn fingerprint_is_stable_and_order_independent() {
        let a = fake(r"\\.\DISPLAY1", Rect::new(0, 0, 1920, 1080), 96);
        let b = fake(r"\\.\DISPLAY2", Rect::new(1920, 0, 3840, 1080), 144);
        let one = fingerprint(&[a.clone(), b.clone()]);
        let two = fingerprint(&[b, a]);
        assert_eq!(
            one, two,
            "enumeration order must not change the fingerprint"
        );
        assert_eq!(one.len(), 16);
    }

    #[test]
    fn fingerprint_changes_with_the_arrangement() {
        let base = fake(r"\\.\DISPLAY1", Rect::new(0, 0, 1920, 1080), 96);
        let docked = vec![
            base.clone(),
            fake(r"\\.\DISPLAY2", Rect::new(1920, 0, 3840, 1080), 96),
        ];
        let laptop = vec![base.clone()];
        assert_ne!(fingerprint(&docked), fingerprint(&laptop));

        // Same monitors, different scaling => different layout => different key.
        let rescaled = vec![fake(r"\\.\DISPLAY1", Rect::new(0, 0, 1920, 1080), 144)];
        assert_ne!(fingerprint(&laptop), fingerprint(&rescaled));

        // Same monitors, moved => different key.
        let moved = vec![fake(r"\\.\DISPLAY1", Rect::new(100, 0, 2020, 1080), 96)];
        assert_ne!(fingerprint(&laptop), fingerprint(&moved));
    }

    #[test]
    fn empty_arrangement_has_a_fingerprint() {
        assert_eq!(fingerprint(&[]).len(), 16);
    }

    #[test]
    fn scaling_rounds_to_nearest() {
        let m = fake("d", Rect::new(0, 0, 1920, 1080), 144); // 150%
        assert_eq!(m.scale(100), 150);
        assert_eq!(m.scale(1), 2, "a hairline must survive rounding");
        assert_eq!(m.scale(0), 0);

        let m96 = fake("d", Rect::new(0, 0, 1920, 1080), 96);
        assert_eq!(m96.scale(100), 100);

        let m120 = fake("d", Rect::new(0, 0, 1920, 1080), 120); // 125%
        assert_eq!(m120.scale(8), 10);
    }

    #[test]
    fn scaling_saturates_instead_of_wrapping() {
        let m = fake("d", Rect::new(0, 0, 1920, 1080), 240);
        // 2.5x of a quarter-range value still fits.
        let quarter = i32::MAX / 4;
        assert_eq!(m.scale(quarter), ((quarter as i64 * 240 + 48) / 96) as i32);
        // Beyond i32 range it must clamp, never wrap to a negative.
        assert_eq!(m.scale(i32::MAX), i32::MAX);
        assert_eq!(m.scale(i32::MIN), i32::MIN);
        assert!(
            m.scale(i32::MAX) > 0,
            "wrapping here would invert rectangles"
        );
    }

    // --- live-system checks -----------------------------------------------
    // These need a desktop session. They assert only invariants that must hold
    // on any Windows machine, so they are safe in CI on a windows runner.

    #[test]
    fn enumerate_finds_at_least_one_monitor() {
        let ms = enumerate();
        assert!(!ms.is_empty(), "a desktop session must have a monitor");
        assert_eq!(
            ms.iter().filter(|m| m.is_primary).count(),
            1,
            "exactly one primary"
        );
    }

    #[test]
    fn every_monitor_has_sane_geometry() {
        for m in enumerate() {
            assert!(!m.bounds.is_empty(), "{m:?}");
            assert!(!m.work_area.is_empty(), "{m:?}");
            assert!(m.dpi >= 48 && m.dpi <= 960, "implausible DPI: {m:?}");
            // The work area is the monitor minus appbars, so it must fit inside.
            assert!(m.work_area.left >= m.bounds.left, "{m:?}");
            assert!(m.work_area.top >= m.bounds.top, "{m:?}");
            assert!(m.work_area.right <= m.bounds.right, "{m:?}");
            assert!(m.work_area.bottom <= m.bounds.bottom, "{m:?}");
            assert!(!m.device.is_empty(), "{m:?}");
        }
    }

    #[test]
    fn primary_is_among_the_enumerated_monitors() {
        let p = primary().expect("a primary monitor must exist");
        assert!(enumerate().iter().any(|m| m.device == p.device));
    }

    #[test]
    fn from_point_inside_a_monitor_returns_that_monitor() {
        let ms = enumerate();
        let first = &ms[0];
        let got = from_point(first.bounds.center_x(), first.bounds.center_y()).unwrap();
        assert_eq!(got.device, first.device);
    }

    #[test]
    fn from_point_far_off_screen_still_resolves() {
        // MONITOR_DEFAULTTONEAREST must never leave us without a monitor.
        assert!(from_point(-999_999, -999_999).is_some());
        assert!(from_point(999_999, 999_999).is_some());
    }

    #[test]
    fn fingerprint_of_the_live_arrangement_is_repeatable() {
        assert_eq!(fingerprint(&enumerate()), fingerprint(&enumerate()));
    }
}

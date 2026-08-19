//! A small tooltip shown while a tray-menu item is hovered.
//!
//! Win32 menus have no tooltip support of their own, and the accelerator
//! column (`label\tKeys`) has room for the key and nothing else. That is not
//! enough here: a binding may have fallen back to its second choice, or have no
//! working key at all, and the user needs to see *that* — not just a key that
//! silently does nothing.
//!
//! Drawn with GDI into a topmost, non-activating window. It is created lazily
//! and lives in [`crate::ui::hover`]'s thread-local, because it is shown from
//! inside `TrackPopupMenuEx`'s modal loop where `App` is already borrowed.

use std::cell::Cell;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::layout::Rect;
use crate::ui::theme::{self, BackBuffer, Colors, Font};

const CLASS_NAME: &str = "SuperTile.Tip";

/// What to show. Kept as owned strings; the tip outlives the menu build.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TipText {
    /// Bold first line — the action's name.
    pub title: String,
    /// The key in use, or an explanation of why there is none.
    pub keys: String,
    /// Supporting detail: the i3 origin, or what the key used to be.
    pub detail: String,
}

impl TipText {
    pub fn is_empty(&self) -> bool {
        self.title.is_empty() && self.keys.is_empty() && self.detail.is_empty()
    }
}

pub struct Tip {
    hwnd: Cell<HWND>,
    visible: Cell<bool>,
    dpi: Cell<u32>,
    dark: Cell<bool>,
    text: std::cell::RefCell<TipText>,
    font_title: std::cell::RefCell<Font>,
    font_body: std::cell::RefCell<Font>,
}

fn scale(dpi: u32, dip: i32) -> i32 {
    ((dip as i64 * dpi as i64 + 48) / 96) as i32
}

impl Tip {
    pub fn new(dark: bool) -> Box<Tip> {
        if !theme::class_exists(CLASS_NAME) {
            let _ = theme::register_class(CLASS_NAME, wndproc);
        }
        let class = crate::util::WideStr::new(CLASS_NAME);
        let dpi = crate::monitor::primary().map(|m| m.dpi).unwrap_or(96);

        // SAFETY: a topmost, non-activating popup we own. WS_EX_TRANSPARENT
        // keeps it out of hit-testing so it can never swallow a menu click.
        let hwnd = unsafe {
            let hinst = GetModuleHandleW(None).unwrap_or_default();
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
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
        };

        let tip = Box::new(Tip {
            hwnd: Cell::new(hwnd.unwrap_or_default()),
            visible: Cell::new(false),
            dpi: Cell::new(dpi),
            dark: Cell::new(dark),
            text: std::cell::RefCell::new(TipText::default()),
            font_title: std::cell::RefCell::new(Font::ui(10, dpi, 600)),
            font_body: std::cell::RefCell::new(Font::ui(9, dpi, 400)),
        });

        let h = tip.hwnd.get();
        if !h.is_invalid() {
            let ptr: *const Tip = &*tip;
            // SAFETY: the box outlives the window; Drop destroys the window
            // before the allocation is freed.
            unsafe {
                SetWindowLongPtrW(h, GWLP_USERDATA, ptr as isize);
            }
            theme::apply_window_chrome(h, dark, false);
        }
        tip
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd.get()
    }

    pub fn set_dark(&self, dark: bool) {
        self.dark.set(dark);
        theme::apply_window_chrome(self.hwnd.get(), dark, false);
    }

    pub fn is_visible(&self) -> bool {
        self.visible.get()
    }

    /// Show `text` near `anchor` (screen coordinates), clamped on-screen.
    pub fn show(&self, text: TipText, anchor: POINT) {
        let h = self.hwnd.get();
        if h.is_invalid() || text.is_empty() {
            self.hide();
            return;
        }

        // Re-measure at the hovered monitor's DPI, so the tip is crisp on a
        // mixed-scaling desktop.
        let mon = crate::monitor::from_point(anchor.x, anchor.y);
        let dpi = mon.as_ref().map(|m| m.dpi).unwrap_or(96);
        if dpi != self.dpi.get() {
            self.dpi.set(dpi);
            *self.font_title.borrow_mut() = Font::ui(10, dpi, 600);
            *self.font_body.borrow_mut() = Font::ui(9, dpi, 400);
        }

        *self.text.borrow_mut() = text;
        let (w, ht) = self.measure();

        // Offset from the cursor so the tip never sits under the pointer, and
        // keep the whole thing inside the monitor's work area.
        let pad = scale(dpi, 18);
        let mut x = anchor.x + pad;
        let mut y = anchor.y + pad;
        if let Some(m) = &mon {
            let wa = m.work_area;
            if x + w > wa.right {
                x = (anchor.x - w - pad).max(wa.left);
            }
            if y + ht > wa.bottom {
                y = (anchor.y - ht - pad).max(wa.top);
            }
        }

        // SAFETY: our own window; SWP_NOACTIVATE keeps the menu focused.
        unsafe {
            let _ = SetWindowPos(
                h,
                Some(HWND_TOPMOST),
                x,
                y,
                w,
                ht,
                SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
            let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
            let _ = InvalidateRect(Some(h), None, true);
            // The menu owns the message loop, so nothing will pump our paint
            // for us; force it now or the tip shows up blank.
            let _ = UpdateWindow(h);
        }
        self.visible.set(true);
    }

    pub fn hide(&self) {
        if !self.visible.get() {
            return;
        }
        let h = self.hwnd.get();
        if !h.is_invalid() {
            // SAFETY: our own window.
            unsafe {
                let _ = ShowWindow(h, SW_HIDE);
            }
        }
        self.visible.set(false);
    }

    /// Width and height needed for the current text.
    fn measure(&self) -> (i32, i32) {
        let dpi = self.dpi.get();
        let pad = scale(dpi, 10);
        let text = self.text.borrow();

        // SAFETY: a screen DC used only for measurement, released below.
        unsafe {
            let dc = GetDC(None);
            let old = SelectObject(dc, self.font_title.borrow().0.into());
            let (tw, th) = theme::measure(dc, &text.title);
            SelectObject(dc, self.font_body.borrow().0.into());
            let (kw, kh) = theme::measure(dc, &text.keys);
            let (dw, dh) = theme::measure(dc, &text.detail);
            SelectObject(dc, old);
            ReleaseDC(None, dc);

            let mut w = tw.max(kw).max(dw) + pad * 2;
            let mut h = pad * 2 + th;
            if !text.keys.is_empty() {
                h += kh + scale(dpi, 3);
            }
            if !text.detail.is_empty() {
                h += dh + scale(dpi, 2);
            }
            // Keep it a tooltip, not a dialog.
            w = w.clamp(scale(dpi, 80), scale(dpi, 460));
            h = h.max(scale(dpi, 28));
            (w, h)
        }
    }

    fn colors(&self) -> Colors {
        if self.dark.get() {
            theme::DARK
        } else {
            theme::LIGHT
        }
    }

    fn paint(&self) {
        let h = self.hwnd.get();
        let mut ps = PAINTSTRUCT::default();
        // SAFETY: paired with EndPaint below.
        let hdc = unsafe { BeginPaint(h, &mut ps) };
        if hdc.is_invalid() {
            return;
        }
        let mut r = RECT::default();
        // SAFETY: valid out-param.
        let _ = unsafe { GetClientRect(h, &mut r) };
        let (w, ht) = (r.right - r.left, r.bottom - r.top);

        if let Some(buf) = BackBuffer::new(hdc, w, ht) {
            let dc = buf.dc;
            let c = self.colors();
            let dpi = self.dpi.get();
            let pad = scale(dpi, 10);

            theme::fill_rect(dc, Rect::new(0, 0, w, ht), c.bg);
            // A hairline border keeps the tip legible over any wallpaper.
            theme::fill_rect(dc, Rect::new(0, 0, w, 1), c.border);
            theme::fill_rect(dc, Rect::new(0, ht - 1, w, ht), c.border);
            theme::fill_rect(dc, Rect::new(0, 0, 1, ht), c.border);
            theme::fill_rect(dc, Rect::new(w - 1, 0, w, ht), c.border);

            let text = self.text.borrow();
            let mut y = pad;

            // SAFETY: fonts live for the duration of the paint.
            let old = unsafe { SelectObject(dc, self.font_title.borrow().0.into()) };
            let (_, th) = theme::measure(dc, "Ag");
            theme::draw_text(dc, &text.title, pad, y, c.fg);
            y += th + scale(dpi, 3);

            // SAFETY: font swap, restored below.
            unsafe {
                SelectObject(dc, self.font_body.borrow().0.into());
            }
            let (_, bh) = theme::measure(dc, "Ag");
            if !text.keys.is_empty() {
                theme::draw_text(dc, &text.keys, pad, y, c.accent);
                y += bh + scale(dpi, 2);
            }
            if !text.detail.is_empty() {
                theme::draw_text(dc, &text.detail, pad, y, c.subtle);
            }

            // SAFETY: restore the font selected on entry.
            unsafe {
                SelectObject(dc, old);
            }
            buf.present(hdc);
        }

        // SAFETY: paired with BeginPaint.
        unsafe {
            let _ = EndPaint(h, &ps);
        }
    }
}

impl Drop for Tip {
    fn drop(&mut self) {
        let h = self.hwnd.get();
        if !h.is_invalid() {
            // SAFETY: destroying our own window and clearing the back-pointer.
            unsafe {
                SetWindowLongPtrW(h, GWLP_USERDATA, 0);
                let _ = DestroyWindow(h);
            }
        }
    }
}

/// # Safety
/// Only valid for windows of class [`CLASS_NAME`].
unsafe fn from_hwnd(hwnd: HWND) -> Option<&'static Tip> {
    // SAFETY: reads our own window's userdata slot, which holds either
    // the Tip pointer stored at creation or zero.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Tip;
    if ptr.is_null() {
        None
    } else {
        // SAFETY: null-checked above. The pointee is the boxed value that
        // outlives the window; see this function's `# Safety` section.
        Some(unsafe { &*ptr })
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            // SAFETY: userdata is set immediately after creation.
            if let Some(t) = unsafe { from_hwnd(hwnd) } {
                t.paint();
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        WM_ERASEBKGND => LRESULT(1),
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

    fn text() -> TipText {
        TipText {
            title: "Focus left".into(),
            keys: "Win+Alt+H".into(),
            detail: "i3: $mod+h".into(),
        }
    }

    #[test]
    fn an_empty_tip_is_recognised() {
        assert!(TipText::default().is_empty());
        assert!(!text().is_empty());
        assert!(!TipText {
            title: "x".into(),
            ..Default::default()
        }
        .is_empty());
    }

    #[test]
    fn a_tip_can_be_created_shown_and_hidden() {
        let t = Tip::new(true);
        assert!(!t.hwnd().is_invalid());
        assert!(!t.is_visible());
        t.show(text(), POINT { x: 200, y: 200 });
        assert!(t.is_visible());
        t.hide();
        assert!(!t.is_visible());
        t.hide(); // idempotent
        assert!(!t.is_visible());
    }

    #[test]
    fn showing_empty_text_hides_instead_of_drawing_a_blank_box() {
        let t = Tip::new(false);
        t.show(text(), POINT { x: 100, y: 100 });
        assert!(t.is_visible());
        t.show(TipText::default(), POINT { x: 100, y: 100 });
        assert!(!t.is_visible());
    }

    #[test]
    fn the_tip_is_click_through_and_never_activates() {
        // It appears during a modal menu loop; stealing focus would dismiss
        // the very menu it is describing.
        let t = Tip::new(true);
        // SAFETY: reading the style of our own window.
        let ex = unsafe { GetWindowLongPtrW(t.hwnd(), GWL_EXSTYLE) } as u32;
        assert_ne!(ex & WS_EX_NOACTIVATE.0, 0);
        assert_ne!(ex & WS_EX_TRANSPARENT.0, 0);
        assert_ne!(ex & WS_EX_TOOLWINDOW.0, 0);
        assert_ne!(ex & WS_EX_TOPMOST.0, 0);
    }

    #[test]
    fn measured_size_grows_with_content_but_stays_bounded() {
        let t = Tip::new(false);
        *t.text.borrow_mut() = TipText {
            title: "A".into(),
            ..Default::default()
        };
        let (w1, h1) = t.measure();
        *t.text.borrow_mut() = text();
        let (w2, h2) = t.measure();
        assert!(h2 > h1, "extra lines should make it taller");
        assert!(w2 >= w1);

        // A pathological string must not produce a full-screen tooltip.
        *t.text.borrow_mut() = TipText {
            title: "x".repeat(4000),
            keys: "y".repeat(4000),
            detail: String::new(),
        };
        let (w3, _) = t.measure();
        assert!(
            w3 <= scale(96, 460) + 4,
            "tooltip should cap its width, got {w3}"
        );
    }

    #[test]
    fn the_tip_stays_within_the_work_area() {
        let Some(m) = crate::monitor::primary() else {
            return;
        };
        let t = Tip::new(false);
        // Anchor hard against the bottom-right corner.
        t.show(
            text(),
            POINT {
                x: m.work_area.right - 2,
                y: m.work_area.bottom - 2,
            },
        );
        assert!(t.is_visible());
        let mut r = RECT::default();
        // SAFETY: valid out-param on our own window.
        let _ = unsafe { GetWindowRect(t.hwnd(), &mut r) };
        assert!(
            r.right <= m.work_area.right,
            "tip ran off the right edge: {r:?}"
        );
        assert!(
            r.bottom <= m.work_area.bottom,
            "tip ran off the bottom: {r:?}"
        );
        t.hide();
    }
}

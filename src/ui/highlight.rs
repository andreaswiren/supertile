//! A click-through overlay that outlines a window on screen.
//!
//! Used by the tray's **Windows** list: hovering an entry draws an accent frame
//! around the window it refers to, so "Document1 - Word" can be identified
//! without clicking anything. Titles alone are not enough when three windows
//! belong to the same application.
//!
//! The overlay is a layered, transparent, non-activating topmost window whose
//! *region* is a rectangular ring — the interior is literally not part of the
//! window, so it neither obscures nor intercepts anything. `WS_EX_TRANSPARENT`
//! plus `WS_EX_NOACTIVATE` keeps it out of hit-testing and focus entirely.

use std::cell::Cell;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::layout::Rect;
use crate::ui::theme;

const CLASS_NAME: &str = "SuperTile.Highlight";

/// A click-through layered window used to draw over the desktop.
///
/// Thickness, opacity, colour and corner radius all come from the overlay
/// theme rather than from constants here: what reads well over the user's own
/// windows depends on their wallpaper and their eyes, not on ours.
pub struct Highlight {
    hwnd: Cell<HWND>,
    visible: Cell<bool>,
    /// Colour of the frame, refreshed from the theme.
    color: Cell<u32>,
    /// Which theme's accent to use when not warning.
    dark_accent: Cell<bool>,
    /// Colours, opacity, thickness, corner radius and readout font.
    overlay: Cell<theme::Overlay>,
    /// When true the interior is filled instead of left open, which is what
    /// the drag preview wants: it is showing a *destination*, not pointing at
    /// something the user can still see.
    filled: Cell<bool>,
    /// Optional caption drawn in the middle — "Swap", "New column right".
    ///
    /// A highlighted rectangle says *where*; during a drag the user also needs
    /// to know *what*, because the same overlay can mean a swap or a split.
    label: std::cell::RefCell<String>,
    /// Draw in the warning colour instead of the accent.
    ///
    /// Used when a drop or a resize cannot be honoured -- a window at its
    /// minimum size. The same blue for "this will work" and "this will not"
    /// makes the caption the only signal, which is easy to miss mid-drag.
    warning: Cell<bool>,
}

impl Highlight {
    pub fn new(dark: bool) -> Box<Highlight> {
        if !theme::class_exists(CLASS_NAME) {
            let _ = theme::register_class(CLASS_NAME, wndproc);
        }
        let class = crate::util::WideStr::new(CLASS_NAME);

        // SAFETY: a layered, click-through, non-activating topmost window. It
        // owns no state beyond its region and is destroyed in Drop.
        let hwnd = unsafe {
            let hinst = GetModuleHandleW(None).unwrap_or_default();
            CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOOLWINDOW
                    | WS_EX_TOPMOST
                    | WS_EX_NOACTIVATE,
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

        let h = Box::new(Highlight {
            hwnd: Cell::new(hwnd.unwrap_or_default()),
            visible: Cell::new(false),
            color: Cell::new(0),
            dark_accent: Cell::new(dark),
            overlay: Cell::new(theme::OVERLAY_WINDOWS),
            filled: Cell::new(false),
            label: std::cell::RefCell::new(String::new()),
            warning: Cell::new(false),
        });

        // Publish the box pointer to the window procedure. Moving the `Box` out
        // of this function does not move the heap allocation, so the pointer
        // stays valid; `Drop` destroys the window before the box is freed.
        let handle = h.hwnd.get();
        if !handle.is_invalid() {
            let ptr: *const Highlight = &*h;
            // SAFETY: see above -- the pointee outlives the window.
            unsafe {
                SetWindowLongPtrW(handle, GWLP_USERDATA, ptr as isize);
            }
        }
        h.set_dark(dark);
        h
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd.get()
    }

    /// Adopt an overlay theme. Repaints if the overlay is already on screen.
    pub fn set_overlay(&self, overlay: theme::Overlay) {
        if self.overlay.get() == overlay {
            return;
        }
        self.overlay.set(overlay);
        self.refresh_colour();
        self.apply_alpha();
        let h = self.hwnd.get();
        if !h.is_invalid() && self.visible.get() {
            // SAFETY: our own window.
            unsafe {
                let _ = InvalidateRect(Some(h), None, true);
                let _ = UpdateWindow(h);
            }
        }
    }

    /// Line thickness in physical pixels on a display of this DPI.
    fn border_px(&self, dpi: u32) -> i32 {
        ((self.overlay.get().border_dip as i64 * dpi as i64 + 48) / 96).max(1) as i32
    }

    /// Corner radius in physical pixels. Zero means square.
    fn corner_px(&self, dpi: u32) -> i32 {
        ((self.overlay.get().corner_dip as i64 * dpi as i64 + 48) / 96).max(0) as i32
    }

    /// A rectangular or rounded region, according to the theme.
    ///
    /// Windows 11 rounds a normal window, so a square overlay drawn over one
    /// reads as a separate object sitting on top rather than as the outline of
    /// the space the window occupies.
    ///
    /// # Safety
    /// Returns an owned region; the caller must delete it or hand it to
    /// `SetWindowRgn`.
    unsafe fn region(&self, l: i32, t: i32, r: i32, b: i32, radius: i32) -> HRGN {
        if radius > 0 {
            // CreateRoundRectRgn takes the full width and height of the corner
            // ellipse, which is twice the radius.
            CreateRoundRectRgn(l, t, r, b, radius * 2, radius * 2)
        } else {
            CreateRectRgn(l, t, r, b)
        }
    }

    pub fn set_dark(&self, dark: bool) {
        self.dark_accent.set(dark);
        self.refresh_colour();
        self.apply_alpha();
    }

    /// Fill the interior rather than drawing only a frame.
    ///
    /// A filled overlay is translucent enough to read the window underneath;
    /// an outline is nearly opaque so it stands out against any wallpaper.
    pub fn set_filled(&self, filled: bool) {
        self.filled.set(filled);
        self.apply_alpha();
    }

    fn apply_alpha(&self) {
        let h = self.hwnd.get();
        if h.is_invalid() {
            return;
        }
        let o = self.overlay.get();
        let alpha = if self.filled.get() {
            o.fill_alpha
        } else {
            o.outline_alpha
        };
        // SAFETY: h is our own layered window.
        unsafe {
            let _ = SetLayeredWindowAttributes(h, COLORREF(0), alpha, LWA_ALPHA);
        }
    }

    /// Draw the frame around `target` (a screen-space rectangle).
    pub fn show_around(&self, target: Rect, dpi: u32) {
        let h = self.hwnd.get();
        if h.is_invalid() || target.is_empty() {
            self.hide();
            return;
        }
        let border = self.border_px(dpi);
        let radius = self.corner_px(dpi);

        // Grow outwards so the frame surrounds the window rather than covering
        // its edge pixels.
        let outer = Rect::new(
            target.left - border,
            target.top - border,
            target.right + border,
            target.bottom + border,
        );
        let (w, hgt) = (outer.width(), outer.height());
        if w <= border * 2 || hgt <= border * 2 {
            self.hide();
            return;
        }

        // SAFETY: regions are created and immediately handed to SetWindowRgn,
        // which takes ownership of `ring`; `inner` is deleted here.
        unsafe {
            let ring = self.region(0, 0, w, hgt, radius);
            if !self.filled.get() {
                // The hole is rounded a little tighter, so the band keeps an
                // even thickness around the curve instead of thinning at the
                // corners.
                let inner = self.region(
                    border,
                    border,
                    w - border,
                    hgt - border,
                    (radius - border).max(0),
                );
                CombineRgn(Some(ring), Some(ring), Some(inner), RGN_DIFF);
                let _ = DeleteObject(inner.into());
            }

            let _ = SetWindowPos(
                h,
                Some(HWND_TOPMOST),
                outer.left,
                outer.top,
                w,
                hgt,
                SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
            // SetWindowRgn assumes ownership of `ring`; do not delete it.
            SetWindowRgn(h, Some(ring), true);

            let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
            let _ = InvalidateRect(Some(h), None, true);
        }
        self.visible.set(true);
    }

    /// Outline every zone at once, as a single shaped window.
    ///
    /// Dragging a boundary moves a shared edge, not one window's edge, and an
    /// overlay that highlights only the window under the pointer tells the
    /// opposite story. Showing the whole grid makes it legible: what is being
    /// pulled is the partition, and every cell that answers to it moves.
    ///
    /// One window carved by a region rather than one window per cell. A dozen
    /// layered windows appearing and vanishing on every drag would cost a dozen
    /// creations, z-order insertions and repaints at 60 Hz; a region is
    /// rebuilt in microseconds and the compositor sees a single surface.
    pub fn show_grid(&self, zones: &[Rect], area: Rect, dpi: u32) {
        let h = self.hwnd.get();
        if h.is_invalid() || zones.is_empty() || area.is_empty() {
            self.hide();
            return;
        }
        let border = self.border_px(dpi);
        let radius = self.corner_px(dpi);

        // SAFETY: every region created here is either combined into `union`
        // and deleted, or handed to SetWindowRgn, which takes ownership.
        unsafe {
            let union = CreateRectRgn(0, 0, 0, 0);
            for z in zones {
                // Drawn *inside* each cell, not around it.
                //
                // Growing every outline outwards pushes it into the gutter
                // between cells, where it meets the outlines of the cells on
                // the other side. Along an edge that merely doubles the line;
                // at a junction where four cells meet, four outlines overlap
                // and fill the corner with a solid square. Those were the
                // blocks appearing between windows.
                //
                // Kept within its own cell, an outline cannot touch another
                // one, whatever the gap or the line thickness.
                let (l, t) = (z.left - area.left, z.top - area.top);
                let (r, b) = (z.right - area.left, z.bottom - area.top);
                if r - l <= border * 2 || b - t <= border * 2 {
                    continue;
                }
                let ring = self.region(l, t, r, b, radius);
                let inner = self.region(
                    l + border,
                    t + border,
                    r - border,
                    b - border,
                    (radius - border).max(0),
                );
                CombineRgn(Some(ring), Some(ring), Some(inner), RGN_DIFF);
                CombineRgn(Some(union), Some(union), Some(ring), RGN_OR);
                let _ = DeleteObject(inner.into());
                let _ = DeleteObject(ring.into());
            }

            let _ = SetWindowPos(
                h,
                Some(HWND_TOPMOST),
                area.left,
                area.top,
                area.width(),
                area.height(),
                SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
            // SetWindowRgn assumes ownership of `union`; do not delete it.
            SetWindowRgn(h, Some(union), true);

            let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
            let _ = InvalidateRect(Some(h), None, true);
        }
        self.visible.set(true);
    }

    pub fn hide(&self) {
        if !self.visible.get() {
            return;
        }
        let h = self.hwnd.get();
        if !h.is_invalid() {
            // SAFETY: h is our own window.
            unsafe {
                let _ = ShowWindow(h, SW_HIDE);
            }
        }
        self.visible.set(false);
    }

    pub fn is_visible(&self) -> bool {
        self.visible.get()
    }

    /// Switch between the accent colour and the warning colour.
    pub fn set_warning(&self, warning: bool) {
        if self.warning.get() == warning {
            return;
        }
        self.warning.set(warning);
        self.refresh_colour();
        let h = self.hwnd.get();
        if !h.is_invalid() && self.visible.get() {
            // SAFETY: our own window.
            unsafe {
                let _ = InvalidateRect(Some(h), None, true);
                let _ = UpdateWindow(h);
            }
        }
    }

    fn refresh_colour(&self) {
        let o = self.overlay.get();
        let c = if self.warning.get() {
            o.warning
        } else {
            o.accent
        };
        self.color.set(c.0);
    }

    /// Set the caption drawn in the middle of a filled overlay.
    pub fn set_label(&self, text: &str) {
        match self.label.try_borrow_mut() {
            Ok(mut l) if *l != text => *l = text.to_string(),
            _ => return,
        }
        let h = self.hwnd.get();
        if !h.is_invalid() && self.visible.get() {
            // SAFETY: our own window; repaint now because the drag owns the
            // pointer and nothing else will pump this paint promptly.
            unsafe {
                let _ = InvalidateRect(Some(h), None, true);
                let _ = UpdateWindow(h);
            }
        }
    }

    fn paint(&self) {
        let h = self.hwnd.get();
        let mut ps = PAINTSTRUCT::default();
        // SAFETY: paired with EndPaint.
        let dc = unsafe { BeginPaint(h, &mut ps) };
        if !dc.is_invalid() {
            let mut r = windows::Win32::Foundation::RECT::default();
            // SAFETY: valid out-param.
            let _ = unsafe { GetClientRect(h, &mut r) };
            let rect = Rect::new(r.left, r.top, r.right, r.bottom);
            theme::fill_rect(dc, rect, COLORREF(self.color.get()));

            // Caption, when there is one and the band is big enough to read it.
            let text = self
                .label
                .try_borrow()
                .map(|l| l.clone())
                .unwrap_or_default();
            if self.filled.get() && !text.is_empty() {
                let dpi = crate::monitor::primary().map(|m| m.dpi).unwrap_or(96);
                let o = self.overlay.get();
                let font = if o.font.is_empty() {
                    theme::Font::ui(o.font_pt, dpi, 600)
                } else {
                    theme::Font::named(o.font, o.font_pt, dpi, 600)
                };
                // SAFETY: the font outlives the drawing below and is restored.
                let old = unsafe { SelectObject(dc, font.0.into()) };
                let (tw, th) = theme::measure(dc, &text);
                if tw + 8 < rect.width() && th + 4 < rect.height() {
                    theme::draw_text(
                        dc,
                        &text,
                        rect.left + (rect.width() - tw) / 2,
                        rect.top + (rect.height() - th) / 2,
                        o.on_accent,
                    );
                }
                // SAFETY: restoring the font selected above.
                unsafe {
                    SelectObject(dc, old);
                }
            }
        }
        // SAFETY: paired with BeginPaint.
        unsafe {
            let _ = EndPaint(h, &ps);
        }
    }
}

impl Drop for Highlight {
    fn drop(&mut self) {
        let h = self.hwnd.get();
        if !h.is_invalid() {
            // SAFETY: destroying our own window; the region is freed with it.
            unsafe {
                SetWindowLongPtrW(h, GWLP_USERDATA, 0);
                let _ = DestroyWindow(h);
            }
        }
    }
}

/// # Safety
/// Only valid for windows of class [`CLASS_NAME`].
unsafe fn from_hwnd(hwnd: HWND) -> Option<&'static Highlight> {
    // SAFETY: reads our own window's userdata slot, which holds either
    // the Highlight pointer stored at creation or zero.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Highlight;
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
            // SAFETY: userdata is set by set_self below once the box exists.
            if let Some(h) = unsafe { from_hwnd(hwnd) } {
                h.paint();
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        WM_ERASEBKGND => LRESULT(1),
        // Never take focus or respond to the mouse.
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
    fn a_highlight_can_be_created_shown_and_hidden() {
        let h = Highlight::new(true);
        assert!(!h.hwnd().is_invalid(), "overlay window should exist");
        assert!(!h.is_visible());

        h.show_around(Rect::new(100, 100, 500, 400), 96);
        assert!(h.is_visible());
        h.hide();
        assert!(!h.is_visible());
        // Hiding twice must be harmless.
        h.hide();
        assert!(!h.is_visible());
    }

    #[test]
    fn a_degenerate_target_hides_rather_than_drawing_nothing() {
        let h = Highlight::new(false);
        h.show_around(Rect::new(10, 10, 500, 400), 96);
        assert!(h.is_visible());
        // An empty rect must not leave a stale frame on screen.
        h.show_around(Rect::new(0, 0, 0, 0), 96);
        assert!(!h.is_visible());
    }

    #[test]
    fn a_tiny_target_still_draws_a_valid_frame() {
        // The frame grows outwards, so even a 1px window yields a drawable
        // ring. Only genuinely empty or inverted rectangles are refused.
        let h = Highlight::new(false);
        h.show_around(Rect::new(0, 0, 1, 1), 384);
        assert!(h.is_visible());
        h.hide();
    }

    #[test]
    fn an_inverted_target_is_refused() {
        let h = Highlight::new(false);
        h.show_around(Rect::new(500, 500, 100, 100), 96);
        assert!(!h.is_visible());
    }

    #[test]
    fn filled_mode_covers_the_whole_rect() {
        let h = Highlight::new(true);
        h.set_filled(true);
        h.show_around(Rect::new(100, 100, 500, 400), 96);
        assert!(h.is_visible());
        // The window must exist at the requested size; the region no longer
        // has a hole, but that is not observable from here without GDI probing.
        let mut r = windows::Win32::Foundation::RECT::default();
        // SAFETY: valid out-param on our own window.
        let _ = unsafe { GetWindowRect(h.hwnd(), &mut r) };
        assert!(r.right - r.left >= 400);
        h.hide();
    }

    #[test]
    fn the_overlay_theme_decides_the_frame_colour() {
        // Deliberately not the system light/dark setting. These overlays are
        // drawn over the user's own windows rather than over our own chrome,
        // so what reads well depends on the wallpaper and the applications --
        // which is the whole reason the theme is a separate setting.
        let h = Highlight::new(true);
        for (name, expected) in theme::OVERLAY_THEMES {
            h.set_overlay(expected);
            assert_eq!(h.color.get(), expected.accent.0, "theme {name}");
        }
    }

    #[test]
    fn the_warning_colour_comes_from_the_theme_too() {
        let h = Highlight::new(true);
        h.set_overlay(theme::OVERLAY_EMBER);
        h.set_warning(true);
        assert_eq!(h.color.get(), theme::OVERLAY_EMBER.warning.0);
        h.set_warning(false);
        assert_eq!(h.color.get(), theme::OVERLAY_EMBER.accent.0);
    }

    #[test]
    fn a_square_theme_and_a_rounded_one_differ_in_geometry() {
        let h = Highlight::new(true);
        h.set_overlay(theme::OVERLAY_CONTRAST);
        assert_eq!(h.corner_px(96), 0, "the contrast theme is square");
        h.set_overlay(theme::OVERLAY_VIOLET);
        assert!(h.corner_px(96) > 0, "the violet theme is rounded");
        // And rounding scales with the display, like everything else.
        assert!(h.corner_px(192) > h.corner_px(96));
    }

    #[test]
    fn the_overlay_is_click_through_and_never_activates() {
        let h = Highlight::new(true);
        // SAFETY: reading the style of our own window.
        let ex = unsafe { GetWindowLongPtrW(h.hwnd(), GWL_EXSTYLE) } as u32;
        assert_ne!(ex & WS_EX_TRANSPARENT.0, 0, "must not intercept clicks");
        assert_ne!(ex & WS_EX_NOACTIVATE.0, 0, "must never steal focus");
        assert_ne!(ex & WS_EX_LAYERED.0, 0, "must be layered for alpha");
        assert_ne!(ex & WS_EX_TOOLWINDOW.0, 0, "must not appear in Alt-Tab");
    }
}

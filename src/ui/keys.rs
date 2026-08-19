//! The keyboard-shortcut list and editor.
//!
//! Every action, the key it is actually bound to, and the i3 binding it came
//! from. Click a row, press the new combination, done — no config file, no
//! restart.
//!
//! ## Capturing a shortcut
//!
//! While a row is armed the window swallows key presses instead of acting on
//! them. Modifier keys alone are ignored: the capture completes when a
//! non-modifier arrives, with the modifier state read at that instant. That is
//! the only way to tell <kbd>Ctrl</kbd> *being held* from <kbd>Ctrl</kbd> being
//! the binding, and it is why a combination cannot be recorded modifier-first.
//!
//! The Windows key never reaches a `WM_KEYDOWN` reliably — the shell eats it —
//! so its state is read with `GetKeyState` rather than waited for.

use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::SetScrollInfo;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::Config;
use crate::hotkeys::{Action, Hotkey};
use crate::layout::Rect;
use crate::ui::theme::{self, BackBuffer, Colors, Font};

/// Posted to the owner when the user commits a new binding.
pub const WM_KEYS_CHANGED: u32 = WM_APP + 40;

const CLASS_NAME: &str = "SuperTile.Keys";

/// One row: an action and how its binding turned out.
#[derive(Debug, Clone)]
pub struct KeyRow {
    pub action: Action,
    /// The combination actually registered, or empty if none works.
    pub keys: String,
    /// The first choice, when a fallback had to be taken.
    pub wanted: String,
    pub i3: String,
    pub note: String,
}

struct State {
    rows: Vec<KeyRow>,
    colors: Colors,
    dark: bool,
    dpi: u32,
    scroll: i32,
    content_height: i32,
    /// Row currently armed for capture.
    capturing: Option<usize>,
    hovered: Option<usize>,
    status: String,
    /// Set when a binding is committed; drained by the application.
    pending: Option<(Action, String)>,
    font_h1: Font,
    font_row: Font,
    font_small: Font,
}

pub struct Keys {
    hwnd: Cell<HWND>,
    owner: Cell<HWND>,
    state: RefCell<State>,
}

fn scale(dpi: u32, dip: i32) -> i32 {
    ((dip as i64 * dpi as i64 + 48) / 96) as i32
}

impl Keys {
    pub fn new(owner: HWND, cfg: &Config) -> Box<Keys> {
        let (colors, dark) = theme::colors_for(cfg.appearance.theme);
        let dpi = crate::monitor::primary().map(|m| m.dpi).unwrap_or(96);

        let keys = Box::new(Keys {
            hwnd: Cell::new(HWND::default()),
            owner: Cell::new(owner),
            state: RefCell::new(State {
                rows: Vec::new(),
                colors,
                dark,
                dpi,
                scroll: 0,
                content_height: 0,
                capturing: None,
                hovered: None,
                status: String::new(),
                pending: None,
                font_h1: Font::ui(16, dpi, 600),
                font_row: Font::ui(10, dpi, 400),
                font_small: Font::ui(9, dpi, 400),
            }),
        });

        if !theme::class_exists(CLASS_NAME) {
            let _ = theme::register_class(CLASS_NAME, wndproc);
        }
        let class = crate::util::WideStr::new(CLASS_NAME);
        let title = crate::util::WideStr::new("SuperTile — keyboard shortcuts");

        // SAFETY: lpCreateParams carries the boxed Keys pointer, read back in
        // WM_NCCREATE. The box outlives the window; Drop destroys it first.
        let hwnd = unsafe {
            let hinst = GetModuleHandleW(None).unwrap_or_default();
            CreateWindowExW(
                WS_EX_APPWINDOW,
                class.as_pcwstr(),
                title.as_pcwstr(),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_VSCROLL | WS_THICKFRAME,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                scale(dpi, 760),
                scale(dpi, 660),
                None,
                None,
                Some(hinst.into()),
                Some(&*keys as *const Keys as *const core::ffi::c_void),
            )
        };
        if let Ok(h) = hwnd {
            keys.hwnd.set(h);
            theme::apply_window_chrome(h, dark, false);
        }
        keys
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd.get()
    }

    pub fn apply_config(&self, cfg: &Config) {
        let (colors, dark) = theme::colors_for(cfg.appearance.theme);
        if let Ok(mut st) = self.state.try_borrow_mut() {
            st.colors = colors;
            st.dark = dark;
        }
        theme::apply_window_chrome(self.hwnd.get(), dark, false);
    }

    /// Show with a fresh snapshot of how every binding resolved.
    pub fn show(&self, rows: Vec<KeyRow>) {
        {
            let Ok(mut st) = self.state.try_borrow_mut() else {
                return;
            };
            st.rows = rows;
            st.capturing = None;
            st.status.clear();
        }
        let h = self.hwnd.get();
        if h.is_invalid() {
            return;
        }
        // SAFETY: our own window.
        unsafe {
            let _ = ShowWindow(h, SW_SHOW);
            let _ = SetForegroundWindow(h);
            let _ = SetFocus(Some(h));
            let _ = InvalidateRect(Some(h), None, true);
        }
    }

    pub fn hide(&self) {
        let h = self.hwnd.get();
        if !h.is_invalid() {
            // SAFETY: our own window.
            unsafe {
                let _ = ShowWindow(h, SW_HIDE);
            }
        }
        if let Ok(mut st) = self.state.try_borrow_mut() {
            st.capturing = None;
        }
    }

    /// Take a committed binding, if there is one.
    pub fn take_pending(&self) -> Option<(Action, String)> {
        self.state
            .try_borrow_mut()
            .ok()
            .and_then(|mut s| s.pending.take())
    }

    fn client_size(&self) -> (i32, i32) {
        let mut r = RECT::default();
        // SAFETY: valid out-param; a null hwnd leaves it zeroed.
        let _ = unsafe { GetClientRect(self.hwnd.get(), &mut r) };
        (r.right - r.left, r.bottom - r.top)
    }

    fn row_height(&self, dpi: u32) -> i32 {
        scale(dpi, 34)
    }

    fn header_height(&self, dpi: u32) -> i32 {
        scale(dpi, 96)
    }

    /// Which row is at client `y`, accounting for scroll.
    fn row_at(&self, y: i32) -> Option<usize> {
        let st = self.state.try_borrow().ok()?;
        let doc_y = y + st.scroll;
        let top = self.header_height(st.dpi);
        if doc_y < top {
            return None;
        }
        let idx = ((doc_y - top) / self.row_height(st.dpi)) as usize;
        (idx < st.rows.len()).then_some(idx)
    }

    fn arm(&self, index: usize) {
        if let Ok(mut st) = self.state.try_borrow_mut() {
            st.capturing = Some(index);
            let label = st.rows.get(index).map(|r| r.action.label()).unwrap_or("");
            st.status = format!("Press the new shortcut for “{label}”  ·  Esc to cancel");
        }
        self.repaint();
    }

    fn cancel(&self) {
        if let Ok(mut st) = self.state.try_borrow_mut() {
            st.capturing = None;
            st.status.clear();
        }
        self.repaint();
    }

    /// A non-modifier key arrived while a row was armed.
    fn capture(&self, vk: u16) {
        let index = match self.state.try_borrow() {
            Ok(st) => match st.capturing {
                Some(i) => i,
                None => return,
            },
            Err(_) => return,
        };

        // Read the modifier state now, at the instant the real key landed.
        // The Windows key is read rather than awaited: the shell intercepts it
        // and a WM_KEYDOWN for VK_LWIN cannot be relied on.
        // SAFETY: GetKeyState reads transient keyboard state.
        let (ctrl, alt, shift, win) = unsafe {
            (
                GetKeyState(VK_CONTROL.0 as i32) < 0,
                GetKeyState(VK_MENU.0 as i32) < 0,
                GetKeyState(VK_SHIFT.0 as i32) < 0,
                GetKeyState(VK_LWIN.0 as i32) < 0 || GetKeyState(VK_RWIN.0 as i32) < 0,
            )
        };

        let mut mods = 0u32;
        if ctrl {
            mods |= MOD_CONTROL.0;
        }
        if alt {
            mods |= MOD_ALT.0;
        }
        if shift {
            mods |= MOD_SHIFT.0;
        }
        if win {
            mods |= MOD_WIN.0;
        }

        if mods == 0 {
            if let Ok(mut st) = self.state.try_borrow_mut() {
                st.status = "A shortcut needs at least one of Win, Ctrl, Alt or Shift — otherwise \
                     it would capture that key everywhere."
                    .into();
            }
            self.repaint();
            return;
        }

        let hk = Hotkey {
            mods: mods | MOD_NOREPEAT.0,
            vk,
        };
        let text = hk.to_string();

        // Refuse a combination already used by another action, rather than
        // silently leaving one of the two dead.
        let clash = {
            let Ok(st) = self.state.try_borrow() else {
                return;
            };
            st.rows
                .iter()
                .enumerate()
                .find(|(i, r)| *i != index && r.keys.eq_ignore_ascii_case(&text))
                .map(|(_, r)| r.action.label())
        };
        if let Some(other) = clash {
            if let Ok(mut st) = self.state.try_borrow_mut() {
                st.status = format!("{text} is already used by “{other}”. Pick another.");
            }
            self.repaint();
            return;
        }

        {
            let Ok(mut st) = self.state.try_borrow_mut() else {
                return;
            };
            let Some(row) = st.rows.get_mut(index) else {
                return;
            };
            let action = row.action;
            row.keys = text.clone();
            row.wanted.clear();
            st.capturing = None;
            st.status = format!("{} is now {text}", action.label());
            st.pending = Some((action, text));
        }
        // SAFETY: owner is the app's host window, live for the process.
        unsafe {
            let _ = PostMessageW(
                Some(self.owner.get()),
                WM_KEYS_CHANGED,
                WPARAM(0),
                LPARAM(0),
            );
        }
        self.repaint();
    }

    fn repaint(&self) {
        // SAFETY: InvalidateRect tolerates a null hwnd via Option.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd.get()), None, false);
        }
    }

    fn scroll_by(&self, delta: i32) {
        let (_, ch) = self.client_size();
        {
            let Ok(mut st) = self.state.try_borrow_mut() else {
                return;
            };
            let max = (st.content_height - ch).max(0);
            st.scroll = (st.scroll + delta).clamp(0, max);
        }
        self.update_scrollbar();
        self.repaint();
    }

    fn update_scrollbar(&self) {
        let (_, ch) = self.client_size();
        let Ok(st) = self.state.try_borrow() else {
            return;
        };
        let si = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
            nMin: 0,
            nMax: st.content_height.max(1) - 1,
            nPage: ch.max(1) as u32,
            nPos: st.scroll,
            nTrackPos: 0,
        };
        drop(st);
        // SAFETY: si is fully initialised with a correct cbSize.
        unsafe {
            SetScrollInfo(self.hwnd.get(), SB_VERT, &si, true);
        }
    }

    fn paint(&self) {
        let hwnd = self.hwnd.get();
        let mut ps = PAINTSTRUCT::default();
        // SAFETY: paired with EndPaint below.
        let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
        if hdc.is_invalid() {
            return;
        }
        let (w, h) = self.client_size();
        if let (Ok(mut st), Some(buf)) = (self.state.try_borrow_mut(), BackBuffer::new(hdc, w, h)) {
            let total = draw(
                &mut st,
                &buf,
                w,
                h,
                self.row_height(96),
                self.header_height(96),
            );
            st.content_height = total;
            drop(st);
            buf.present(hdc);
        }
        // SAFETY: paired with BeginPaint.
        unsafe {
            let _ = EndPaint(hwnd, &ps);
        }
        self.update_scrollbar();
    }
}

impl Drop for Keys {
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

fn draw(st: &mut State, buf: &BackBuffer, w: i32, h: i32, _rh: i32, _hh: i32) -> i32 {
    let dc = buf.dc;
    let c = st.colors;
    let dpi = st.dpi;
    let s = |dip: i32| scale(dpi, dip);
    let pad = s(24);
    let row_h = s(34);
    let scroll = st.scroll;

    theme::fill_rect(dc, Rect::new(0, 0, w, h), c.bg);

    // --- header (fixed, not scrolled) ---
    // SAFETY: fonts live for the duration of the paint.
    let old = unsafe { SelectObject(dc, st.font_h1.0.into()) };
    let (_, h1) = theme::measure(dc, "Ag");
    theme::draw_text(dc, "Keyboard shortcuts", pad, s(18), c.fg);

    // SAFETY: font swap.
    unsafe {
        SelectObject(dc, st.font_small.0.into());
    }
    let (_, sh) = theme::measure(dc, "Ag");
    let sub_y = s(18) + h1 + s(6);
    theme::draw_text(
        dc,
        "Click a shortcut to change it, then press the new keys. Bindings follow i3.",
        pad,
        sub_y,
        c.subtle,
    );

    // Status line doubles as the capture prompt.
    let status_y = sub_y + sh + s(6);
    if !st.status.is_empty() {
        let armed = st.capturing.is_some();
        theme::draw_text(
            dc,
            &st.status.clone(),
            pad,
            status_y,
            if armed { c.accent } else { c.fg },
        );
    }

    let header_h = s(96);
    theme::fill_rect(dc, Rect::new(0, header_h - 1, w, header_h), c.border);

    // --- rows ---
    let col_keys = pad + (w - pad * 2) * 46 / 100;
    let col_i3 = pad + (w - pad * 2) * 76 / 100;

    let mut y = header_h - scroll;
    for (i, row) in st.rows.iter().enumerate() {
        let armed = st.capturing == Some(i);
        let hovered = st.hovered == Some(i);

        if y + row_h >= header_h && y <= h {
            if armed {
                theme::fill_round_rect(
                    dc,
                    Rect::new(pad / 2, y + 2, w - pad / 2, y + row_h - 2),
                    s(6),
                    c.sel_bg,
                );
            } else if hovered || i % 2 == 1 {
                // Hover and the zebra stripe share a tint; hover is
                // distinguished by the hand cursor and the armed state.
                theme::fill_rect(dc, Rect::new(0, y, w, y + row_h), c.separator);
            }

            // SAFETY: font swap within a live paint.
            unsafe {
                SelectObject(dc, st.font_row.0.into());
            }
            let (_, th) = theme::measure(dc, "Ag");
            let ty = y + (row_h - th) / 2;

            theme::draw_text(dc, row.action.label(), pad, ty, c.fg);

            // The key, and what is wrong with it.
            let (key_text, key_colour) = if armed {
                ("press keys…".to_string(), c.accent)
            } else if row.keys.is_empty() {
                ("unavailable".to_string(), c.subtle)
            } else if !row.wanted.is_empty() {
                (format!("{}  ·  fallback", row.keys), c.accent)
            } else {
                (row.keys.clone(), c.fg)
            };
            theme::draw_text(dc, &key_text, col_keys, ty, key_colour);

            // SAFETY: font swap.
            unsafe {
                SelectObject(dc, st.font_small.0.into());
            }
            let i3 = if row.i3.is_empty() {
                "—".to_string()
            } else {
                row.i3.clone()
            };
            let i3 = theme::elide(dc, &i3, w - pad - col_i3);
            theme::draw_text(dc, &i3, col_i3, ty + s(1), c.subtle);
        }
        y += row_h;
    }

    // SAFETY: restore the font selected on entry.
    unsafe {
        SelectObject(dc, old);
    }
    y + scroll + s(16)
}

/// # Safety
/// Only valid for windows of class [`CLASS_NAME`].
unsafe fn keys_from(hwnd: HWND) -> Option<&'static Keys> {
    // SAFETY: reads our own window's userdata slot, which holds either the
    // Keys pointer stored in WM_NCCREATE or zero.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Keys;
    if ptr.is_null() {
        None
    } else {
        // SAFETY: null-checked above; the pointee outlives the window.
        Some(unsafe { &*ptr })
    }
}

/// Keys that are only ever modifiers, never a binding on their own.
fn is_modifier(vk: u16) -> bool {
    matches!(
        VIRTUAL_KEY(vk),
        VK_SHIFT
            | VK_CONTROL
            | VK_MENU
            | VK_LWIN
            | VK_RWIN
            | VK_LSHIFT
            | VK_RSHIFT
            | VK_LCONTROL
            | VK_RCONTROL
            | VK_LMENU
            | VK_RMENU
    )
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        // SAFETY: lp is the CREATESTRUCTW for our own CreateWindowExW call.
        let cs = unsafe { &*(lp.0 as *const CREATESTRUCTW) };
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        }
        // SAFETY: default processing completes window creation.
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    // SAFETY: userdata is set above for windows of this class.
    let Some(k) = (unsafe { keys_from(hwnd) }) else {
        // SAFETY: default handling before the back-pointer exists.
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    };

    match msg {
        WM_PAINT => {
            k.paint();
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            k.update_scrollbar();
            k.scroll_by(0);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as i16;
            let step = if delta > 0 { -3 } else { 3 };
            // The Ref from `state.borrow()` lives to the end of the statement, so
            // inlining it here would still be borrowed when scroll_by tries to
            // borrow mutably -- which failed silently and made the wheel dead.
            let dpi = k.state.try_borrow().map(|s| s.dpi).unwrap_or(96);
            k.scroll_by(step * scale(dpi, 24));
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let y = ((lp.0 >> 16) & 0xFFFF) as i16 as i32;
            let hit = k.row_at(y);
            let changed = match k.state.try_borrow_mut() {
                Ok(mut st) if st.hovered != hit => {
                    st.hovered = hit;
                    true
                }
                _ => false,
            };
            if changed {
                k.repaint();
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let y = ((lp.0 >> 16) & 0xFFFF) as i16 as i32;
            if let Some(i) = k.row_at(y) {
                k.arm(i);
            }
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = wp.0 as u16;
            let capturing = k
                .state
                .try_borrow()
                .map(|s| s.capturing.is_some())
                .unwrap_or(false);
            if capturing {
                if VIRTUAL_KEY(vk) == VK_ESCAPE {
                    k.cancel();
                } else if !is_modifier(vk) {
                    // Modifiers alone never complete a capture; the real key
                    // does, and the modifier state is read at that moment.
                    k.capture(vk);
                }
                // Swallow everything while armed, including Alt combinations,
                // so the system menu does not open mid-capture.
                return LRESULT(0);
            }
            if VIRTUAL_KEY(vk) == VK_ESCAPE {
                k.hide();
                return LRESULT(0);
            }
            // SAFETY: default handling for keys we do not consume.
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        // Stop Alt from activating the (nonexistent) menu bar while capturing.
        WM_SYSCHAR => LRESULT(0),
        WM_CLOSE => {
            k.hide();
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_keys_never_complete_a_capture() {
        for vk in [
            VK_SHIFT,
            VK_CONTROL,
            VK_MENU,
            VK_LWIN,
            VK_RWIN,
            VK_LSHIFT,
            VK_RCONTROL,
        ] {
            assert!(is_modifier(vk.0), "{vk:?} should be treated as a modifier");
        }
        for vk in [VK_A, VK_F5, VK_SPACE, VK_RETURN, VK_LEFT] {
            assert!(!is_modifier(vk.0), "{vk:?} should complete a capture");
        }
    }

    #[test]
    fn scaling_matches_the_shared_helper() {
        assert_eq!(scale(96, 34), 34);
        assert_eq!(scale(192, 34), 68);
    }

    #[test]
    fn the_changed_message_is_in_the_app_range_and_unique() {
        const { assert!(WM_KEYS_CHANGED >= WM_APP) };
        assert_ne!(WM_KEYS_CHANGED, crate::ui::palette::WM_PALETTE_ACCEPT);
        assert_ne!(WM_KEYS_CHANGED, crate::ui::palette::WM_PALETTE_CANCEL);
    }

    #[test]
    fn a_row_carries_everything_the_ui_needs() {
        let r = KeyRow {
            action: Action::FocusLeft,
            keys: "Win+Alt+H".into(),
            wanted: String::new(),
            i3: "$mod+h".into(),
            note: String::new(),
        };
        assert_eq!(r.action.label(), "Focus left");
        assert!(!r.keys.is_empty());
    }
}

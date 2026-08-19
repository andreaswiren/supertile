//! The **Theme Editor** window.
//!
//! Overlay themes are the one part of SuperTile that cannot be judged from a
//! list of names. Whether a pale blue reads over somebody's wallpaper, whether
//! a two-pixel line is enough on a dense display, whether a corner radius of
//! twelve looks like the outline of a window or like a sticker on top of it —
//! none of that survives being written down. So the window is built around a
//! preview: the same geometry the grid overlay draws, over light, dark and
//! mid-tone backgrounds, with the size readout in the theme's own typeface.
//!
//! Custom-drawn with GDI, following [`crate::ui::about`]: one paint routine, a
//! native vertical scrollbar, and hit-tested rectangles instead of child
//! controls. The window never mutates the configuration itself. It keeps its
//! own copy of the selection and the custom theme, and posts
//! [`WM_THEME_CHANGED`] to the owner after every edit; the application reads
//! [`ThemeEditor::selected`] and [`ThemeEditor::custom`] and decides what to
//! save.

use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::SetScrollInfo;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::{Config, CustomTheme};
use crate::layout::Rect;
use crate::ui::theme::{self, BackBuffer, Colors, Font, Overlay};

/// Posted to the owner after any edit: a different theme selected, a colour
/// picked, a value stepped, the custom theme renamed.
///
/// Carries no payload. The window holds the authoritative values and the
/// application pulls them, which keeps the two from disagreeing about what a
/// `WPARAM` meant.
pub const WM_THEME_CHANGED: u32 = WM_APP + 0x40;

const CLASS_NAME: &str = "SuperTile.ThemeEditor";

/// The application's message window, used when the caller has not named an
/// owner explicitly. See [`ThemeEditor::owner`].
const HOST_CLASS: &str = "SuperTile.Host";

const CARET_TIMER: usize = 1;

/// The key that selects the user's own theme rather than a built-in one.
const CUSTOM_KEY: &str = "custom";

/// Longest custom theme name we accept. It is drawn in a tray menu and in one
/// unscrolled field, so a name nobody can read is not a name.
const NAME_LIMIT: usize = 40;

// ---------------------------------------------------------------------------
// The adjustable numbers
// ---------------------------------------------------------------------------

/// One numeric property of the custom theme, edited with a plus/minus pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    OutlineAlpha,
    FillAlpha,
    BorderDip,
    CornerDip,
    FontPt,
}

impl Field {
    const ALL: [Field; 5] = [
        Field::OutlineAlpha,
        Field::FillAlpha,
        Field::BorderDip,
        Field::CornerDip,
        Field::FontPt,
    ];

    fn label(self) -> &'static str {
        match self {
            Field::OutlineAlpha => "Outline opacity",
            Field::FillAlpha => "Fill opacity",
            Field::BorderDip => "Line thickness",
            Field::CornerDip => "Corner radius",
            Field::FontPt => "Readout size",
        }
    }

    /// What the number is for, and where it stops mattering.
    ///
    /// The two opacity ranges are wider than the renderer honours — see
    /// [`Look::from_custom`] — so the hint says where the ceiling and the floor
    /// actually are rather than leaving the control looking broken.
    fn hint(self) -> &'static str {
        match self {
            Field::OutlineAlpha => "60-255; below 60 an outline is not worth drawing",
            Field::FillAlpha => "20-200, so the window beneath stays readable",
            Field::BorderDip => "device-independent pixels, scaled by the display",
            Field::CornerDip => "0 is square; Windows 11 rounds a window by 8",
            Field::FontPt => "points, for the size readout",
        }
    }

    /// The inclusive range the control offers.
    fn range(self) -> (i32, i32) {
        match self {
            // The renderer floors an outline at 60 and caps a fill at 200, so
            // offering the full 0..255 would give the control dead travel at
            // both ends -- pressing minus and seeing nothing change reads as a
            // broken button, not as a limit. The control stops where the effect
            // stops.
            Field::OutlineAlpha => (60, 255),
            Field::FillAlpha => (20, 200),
            Field::BorderDip => (1, 8),
            Field::CornerDip => (0, 24),
            Field::FontPt => (8, 24),
        }
    }

    /// How far one press of plus or minus moves the value.
    ///
    /// Opacity is stepped by five: a single count out of 255 is invisible, and
    /// walking from 90 to 210 one at a time is not an interaction.
    fn step(self) -> i32 {
        match self {
            Field::OutlineAlpha | Field::FillAlpha => 5,
            _ => 1,
        }
    }

    fn get(self, c: &CustomTheme) -> i32 {
        match self {
            Field::OutlineAlpha => c.outline_alpha as i32,
            Field::FillAlpha => c.fill_alpha as i32,
            Field::BorderDip => c.border_dip,
            Field::CornerDip => c.corner_dip,
            Field::FontPt => c.font_pt,
        }
    }

    fn set(self, c: &mut CustomTheme, v: i32) {
        let v = clamp_field(self, v);
        match self {
            Field::OutlineAlpha => c.outline_alpha = v as u8,
            Field::FillAlpha => c.fill_alpha = v as u8,
            Field::BorderDip => c.border_dip = v,
            Field::CornerDip => c.corner_dip = v,
            Field::FontPt => c.font_pt = v,
        }
    }
}

/// Hold a value inside the range its control offers.
fn clamp_field(f: Field, v: i32) -> i32 {
    let (lo, hi) = f.range();
    v.clamp(lo, hi)
}

/// The value one press of plus (`direction` 1) or minus (`direction` -1) gives.
///
/// Clamping happens here rather than at the edge of the range so a press at the
/// limit is a no-op instead of wrapping or overshooting.
fn stepped(f: Field, current: i32, direction: i32) -> i32 {
    clamp_field(f, current.saturating_add(direction * f.step()))
}

/// One of the three colours the custom theme owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Swatch {
    Accent,
    Warning,
    Text,
}

impl Swatch {
    const ALL: [Swatch; 3] = [Swatch::Accent, Swatch::Warning, Swatch::Text];

    fn label(self) -> &'static str {
        match self {
            Swatch::Accent => "Accent",
            Swatch::Warning => "Warning",
            Swatch::Text => "Readout text",
        }
    }

    fn hex(self, c: &CustomTheme) -> &str {
        match self {
            Swatch::Accent => &c.accent,
            Swatch::Warning => &c.warning,
            Swatch::Text => &c.text,
        }
    }

    fn set_hex(self, c: &mut CustomTheme, hex: String) {
        match self {
            Swatch::Accent => c.accent = hex,
            Swatch::Warning => c.warning = hex,
            Swatch::Text => c.text = hex,
        }
    }
}

// ---------------------------------------------------------------------------
// A theme resolved for drawing
// ---------------------------------------------------------------------------

/// Everything the preview needs from a theme, with an owned face name.
///
/// Deliberately not [`theme::Overlay`]. An `Overlay` holds a `&'static str` so
/// it can be a `const`, and [`theme::overlay_from_custom`] pays for that by
/// leaking the face name — acceptable once per theme change, ruinous once per
/// repaint, and this window repaints on every mouse move.
#[derive(Clone, Debug, PartialEq)]
struct Look {
    accent: COLORREF,
    warning: COLORREF,
    on_accent: COLORREF,
    outline_alpha: u8,
    fill_alpha: u8,
    border_dip: i32,
    corner_dip: i32,
    font: String,
    font_pt: i32,
}

impl Look {
    fn from_overlay(o: &Overlay) -> Look {
        Look {
            accent: o.accent,
            warning: o.warning,
            on_accent: o.on_accent,
            outline_alpha: o.outline_alpha,
            fill_alpha: o.fill_alpha,
            border_dip: o.border_dip,
            corner_dip: o.corner_dip,
            font: o.font.to_string(),
            font_pt: o.font_pt,
        }
    }

    /// Resolve a custom theme exactly as the renderer will.
    ///
    /// The clamps mirror [`theme::overlay_from_custom`] on purpose: a preview
    /// that shows what the overlay will not draw is worse than no preview. A
    /// test pins the two together so an edit to one is caught.
    fn from_custom(c: &CustomTheme) -> Look {
        let d = theme::OVERLAY_WINDOWS;
        Look {
            accent: theme::parse_hex(&c.accent).unwrap_or(d.accent),
            warning: theme::parse_hex(&c.warning).unwrap_or(d.warning),
            on_accent: theme::parse_hex(&c.text).unwrap_or(d.on_accent),
            outline_alpha: c.outline_alpha.max(60),
            fill_alpha: c.fill_alpha.clamp(20, 200),
            border_dip: c.border_dip.clamp(1, 8),
            corner_dip: c.corner_dip.clamp(0, 24),
            font: c.font.clone(),
            font_pt: c.font_pt.clamp(8, 24),
        }
    }

    /// Line thickness in physical pixels, as [`crate::ui::highlight`] computes
    /// it. Never zero: a theme with no visible line is not a theme.
    fn border_px(&self, dpi: u32) -> i32 {
        scale(dpi, self.border_dip).max(1)
    }

    /// Corner radius in physical pixels. Zero means square.
    fn corner_px(&self, dpi: u32) -> i32 {
        scale(dpi, self.corner_dip).max(0)
    }
}

/// The list of selectable themes, in the order the tray shows them.
fn theme_rows(custom_name: &str) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = theme::OVERLAY_THEMES
        .iter()
        .map(|(key, _)| (key.to_string(), theme::overlay_label(key).to_string()))
        .collect();
    let name = if custom_name.trim().is_empty() {
        "Unnamed".to_string()
    } else {
        custom_name.trim().to_string()
    };
    rows.push((CUSTOM_KEY.to_string(), format!("{name} (custom)")));
    rows
}

/// Resolve a theme key to something drawable.
fn look_for(key: &str, custom: &CustomTheme) -> Look {
    if key.eq_ignore_ascii_case(CUSTOM_KEY) {
        Look::from_custom(custom)
    } else {
        Look::from_overlay(&theme::overlay_by_name(key))
    }
}

// ---------------------------------------------------------------------------
// Hit-testing
// ---------------------------------------------------------------------------

/// What clicking a hit-tested rectangle does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    Pick(Swatch),
    Step(Field, i32),
    EditName,
}

/// A clickable region, resolved during paint and stored in document space so
/// scrolling does not invalidate it.
#[derive(Clone, Copy)]
struct Hotspot {
    rect: Rect,
    act: Act,
}

/// What the pointer is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    /// A row of the theme list, by index.
    Row(usize),
    /// A hotspot, by index into `State::hotspots`.
    Spot(usize),
}

/// Which theme row sits at document `y`, if any.
///
/// Rows are uniform and span the full width, so arithmetic beats storing a
/// rectangle per row; `top` and `row_h` are recorded during paint.
fn row_at(doc_y: i32, top: i32, row_h: i32, count: usize) -> Option<usize> {
    if row_h <= 0 || doc_y < top {
        return None;
    }
    let index = ((doc_y - top) / row_h) as usize;
    (index < count).then_some(index)
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

struct State {
    colors: Colors,
    dark: bool,
    dpi: u32,
    scroll: i32,
    content_height: i32,
    /// The selected theme key: a name from [`theme::OVERLAY_THEMES`], or
    /// [`CUSTOM_KEY`].
    selected: String,
    custom: CustomTheme,
    hotspots: Vec<Hotspot>,
    hovered: Option<Hit>,
    /// The name field has focus and is swallowing characters.
    editing_name: bool,
    caret_on: bool,
    /// Geometry of the theme list, recorded during paint for [`row_at`].
    list_top: i32,
    row_h: i32,
    row_count: usize,
    font_h2: Font,
    font_body: Font,
    font_small: Font,
    /// The readout font shown in the preview.
    ///
    /// Cached rather than built per paint: this window repaints on every mouse
    /// move, and GDI handles have run out in this project before.
    preview_font: Font,
    preview_font_key: (String, i32),
}

pub struct ThemeEditor {
    hwnd: Cell<HWND>,
    owner: Cell<HWND>,
    state: RefCell<State>,
}

fn scale(dpi: u32, dip: i32) -> i32 {
    ((dip as i64 * dpi as i64 + 48) / 96) as i32
}

/// Height of the fixed preview panel at the top of the client area.
fn preview_height(dpi: u32) -> i32 {
    scale(dpi, 250)
}

impl ThemeEditor {
    pub fn new(dark: bool) -> Box<ThemeEditor> {
        let colors = if dark { theme::DARK } else { theme::LIGHT };
        let dpi = crate::monitor::primary().map(|m| m.dpi).unwrap_or(96);
        let custom = CustomTheme::default();

        let editor = Box::new(ThemeEditor {
            hwnd: Cell::new(HWND::default()),
            owner: Cell::new(HWND::default()),
            state: RefCell::new(State {
                colors,
                dark,
                dpi,
                scroll: 0,
                content_height: 0,
                selected: theme::OVERLAY_THEMES[0].0.to_string(),
                custom,
                hotspots: Vec::new(),
                hovered: None,
                editing_name: false,
                caret_on: true,
                list_top: 0,
                row_h: scale(dpi, 38),
                row_count: 0,
                font_h2: Font::ui(12, dpi, 600),
                font_body: Font::ui(10, dpi, 400),
                font_small: Font::ui(9, dpi, 400),
                preview_font: Font::ui(12, dpi, 600),
                preview_font_key: (String::new(), 12),
            }),
        });

        if !theme::class_exists(CLASS_NAME) {
            let _ = theme::register_class(CLASS_NAME, wndproc);
        }

        let ptr: *const ThemeEditor = &*editor;
        let class = crate::util::WideStr::new(CLASS_NAME);
        let title = crate::util::WideStr::new("SuperTile — overlay themes");

        // SAFETY: lpCreateParams carries the boxed ThemeEditor pointer, read
        // back in WM_NCCREATE. The box outlives the window because Drop
        // destroys the window before the allocation is freed.
        let hwnd = unsafe {
            let hinst = GetModuleHandleW(None).unwrap_or_default();
            CreateWindowExW(
                WS_EX_APPWINDOW,
                class.as_pcwstr(),
                title.as_pcwstr(),
                WS_OVERLAPPED
                    | WS_CAPTION
                    | WS_SYSMENU
                    | WS_VSCROLL
                    | WS_THICKFRAME
                    | WS_MINIMIZEBOX,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                scale(dpi, 720),
                scale(dpi, 760),
                None,
                None,
                Some(hinst.into()),
                Some(ptr as *const core::ffi::c_void),
            )
        };
        if let Ok(h) = hwnd {
            editor.hwnd.set(h);
            theme::apply_window_chrome(h, dark, false);
        }
        editor
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd.get()
    }

    /// Name the window that receives [`WM_THEME_CHANGED`].
    ///
    /// Optional: without it the application's own message window is found by
    /// class, which is what happens in practice since there is exactly one.
    pub fn set_owner(&self, owner: HWND) {
        self.owner.set(owner);
    }

    /// Adopt the palette and the theme currently in the configuration.
    ///
    /// Called before showing the window and again whenever the configuration is
    /// reloaded, so the editor never disagrees with the file about what is
    /// selected.
    pub fn apply_config(&self, cfg: &Config) {
        let (colors, dark) = theme::colors_for(cfg.appearance.theme);
        if let Ok(mut st) = self.state.try_borrow_mut() {
            st.colors = colors;
            st.dark = dark;
            st.selected = cfg.appearance.overlay_theme.clone();
            st.custom = cfg.appearance.custom_theme.clone();
        }
        theme::apply_window_chrome(self.hwnd.get(), dark, false);
        self.repaint();
    }

    /// Show, or bring to the front if already open.
    pub fn show(&self) {
        let h = self.hwnd.get();
        if h.is_invalid() {
            return;
        }
        // SAFETY: h is our own window.
        unsafe {
            let _ = ShowWindow(h, SW_SHOW);
            let _ = SetForegroundWindow(h);
            let _ = SetFocus(Some(h));
            let _ = InvalidateRect(Some(h), None, true);
        }
        self.update_scrollbar();
    }

    pub fn hide(&self) {
        self.stop_editing();
        let h = self.hwnd.get();
        if !h.is_invalid() {
            // SAFETY: h is our own window.
            unsafe {
                let _ = ShowWindow(h, SW_HIDE);
            }
        }
    }

    /// The theme key the user has chosen: a built-in name or `custom`.
    pub fn selected(&self) -> String {
        self.state
            .try_borrow()
            .map(|s| s.selected.clone())
            .unwrap_or_else(|_| theme::OVERLAY_THEMES[0].0.to_string())
    }

    /// The custom theme as edited here.
    pub fn custom(&self) -> CustomTheme {
        self.state
            .try_borrow()
            .map(|s| s.custom.clone())
            .unwrap_or_default()
    }

    /// Where [`WM_THEME_CHANGED`] goes.
    ///
    /// Resolved lazily and cached: the editor is constructed from a tray menu
    /// handler that may not have the host window to hand, and there is only
    /// ever one window of that class in the process.
    fn owner(&self) -> HWND {
        let known = self.owner.get();
        if !known.is_invalid() {
            return known;
        }
        let class = crate::util::WideStr::new(HOST_CLASS);
        // SAFETY: the class name outlives the call; a miss returns an error,
        // which leaves the owner unset and the notification simply undelivered.
        let found = unsafe { FindWindowW(class.as_pcwstr(), None) }.unwrap_or_default();
        self.owner.set(found);
        found
    }

    /// Tell the application something changed, then redraw.
    fn notify(&self) {
        let owner = self.owner();
        if !owner.is_invalid() {
            // SAFETY: owner is the application's host window, live for the
            // lifetime of the process; PostMessageW does not block.
            unsafe {
                let _ = PostMessageW(Some(owner), WM_THEME_CHANGED, WPARAM(0), LPARAM(0));
            }
        }
        self.repaint();
    }

    fn repaint(&self) {
        // SAFETY: InvalidateRect tolerates a null hwnd through the Option.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd.get()), None, false);
        }
    }

    fn client_size(&self) -> (i32, i32) {
        let mut r = RECT::default();
        // SAFETY: valid out-param; a null hwnd leaves it zeroed.
        let _ = unsafe { GetClientRect(self.hwnd.get(), &mut r) };
        (r.right - r.left, r.bottom - r.top)
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

    fn scroll_to(&self, pos: i32) {
        let (_, ch) = self.client_size();
        {
            let Ok(mut st) = self.state.try_borrow_mut() else {
                return;
            };
            let max = (st.content_height - ch).max(0);
            st.scroll = pos.clamp(0, max);
        }
        self.update_scrollbar();
        self.repaint();
    }

    /// What is under the pointer, in client coordinates.
    ///
    /// The preview panel is fixed and takes no clicks, so anything inside it is
    /// a miss even though scrolled content passes underneath.
    fn probe(&self, x: i32, y: i32) -> Option<Hit> {
        let st = self.state.try_borrow().ok()?;
        if y < preview_height(st.dpi) {
            return None;
        }
        let doc_y = y + st.scroll;
        if let Some(i) = st.hotspots.iter().position(|h| h.rect.contains(x, doc_y)) {
            return Some(Hit::Spot(i));
        }
        // Rows span the whole width, so they are tested after the controls
        // that sit on top of them.
        row_at(doc_y, st.list_top, st.row_h, st.row_count).map(Hit::Row)
    }

    fn select_row(&self, index: usize) {
        let key = {
            let Ok(st) = self.state.try_borrow() else {
                return;
            };
            theme_rows(&st.custom.name)
                .get(index)
                .map(|(k, _)| k.clone())
        };
        let Some(key) = key else { return };
        let changed = match self.state.try_borrow_mut() {
            Ok(mut st) if !st.selected.eq_ignore_ascii_case(&key) => {
                st.selected = key;
                true
            }
            _ => false,
        };
        if changed {
            self.notify();
        }
    }

    /// Open the system colour picker for one of the custom theme's colours.
    ///
    /// The dialog runs its own message loop, so every borrow is released before
    /// it is entered and taken again afterwards. Holding one across it would
    /// deadlock the first repaint the dialog provokes.
    fn pick(&self, which: Swatch) {
        let initial = {
            let Ok(st) = self.state.try_borrow() else {
                return;
            };
            theme::parse_hex(which.hex(&st.custom)).unwrap_or(theme::OVERLAY_WINDOWS.accent)
        };
        let Some(picked) = crate::ui::choose_colour(self.hwnd.get(), initial) else {
            return;
        };
        {
            let Ok(mut st) = self.state.try_borrow_mut() else {
                return;
            };
            let hex = theme::to_hex(picked);
            if which.hex(&st.custom) == hex {
                return;
            }
            which.set_hex(&mut st.custom, hex);
        }
        self.notify();
    }

    fn step(&self, field: Field, direction: i32) {
        {
            let Ok(mut st) = self.state.try_borrow_mut() else {
                return;
            };
            let current = field.get(&st.custom);
            let next = stepped(field, current, direction);
            if next == current {
                return;
            }
            field.set(&mut st.custom, next);
        }
        self.notify();
    }

    fn start_editing(&self) {
        let already = match self.state.try_borrow_mut() {
            Ok(mut st) => {
                let was = st.editing_name;
                st.editing_name = true;
                st.caret_on = true;
                was
            }
            Err(_) => return,
        };
        if !already {
            let h = self.hwnd.get();
            // SAFETY: h is our own window; the timer is killed when editing
            // stops and again in Drop.
            unsafe {
                let _ = SetTimer(Some(h), CARET_TIMER, caret_blink_ms(), None);
            }
        }
        self.repaint();
    }

    fn stop_editing(&self) {
        let was = match self.state.try_borrow_mut() {
            Ok(mut st) => {
                let was = st.editing_name;
                st.editing_name = false;
                was
            }
            Err(_) => return,
        };
        if was {
            // SAFETY: killing a timer we armed; harmless if it already expired.
            unsafe {
                let _ = KillTimer(Some(self.hwnd.get()), CARET_TIMER);
            }
            self.repaint();
        }
    }

    /// A character arrived while the name field had focus.
    fn on_char(&self, ch: u16) {
        let editing = self
            .state
            .try_borrow()
            .map(|s| s.editing_name)
            .unwrap_or(false);
        if !editing {
            return;
        }
        let c = char::from_u32(ch as u32).unwrap_or('\0');
        {
            let Ok(mut st) = self.state.try_borrow_mut() else {
                return;
            };
            if c == '\u{8}' {
                if st.custom.name.pop().is_none() {
                    return;
                }
            } else if (c as u32) < 0x20 || c == '\u{7F}' {
                // Control characters reach WM_CHAR too; only text is a name.
                return;
            } else if st.custom.name.chars().count() >= NAME_LIMIT {
                return;
            } else {
                st.custom.name.push(c);
            }
            st.caret_on = true;
        }
        self.notify();
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
            let total = draw(&mut st, &buf, w, h);
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

impl Drop for ThemeEditor {
    fn drop(&mut self) {
        let h = self.hwnd.get();
        if !h.is_invalid() {
            // SAFETY: destroying our own window and clearing the back-pointer,
            // so no further message can reach the freed box.
            unsafe {
                let _ = KillTimer(Some(h), CARET_TIMER);
                SetWindowLongPtrW(h, GWLP_USERDATA, 0);
                let _ = DestroyWindow(h);
            }
        }
    }
}

/// The system caret blink rate, or a sensible default when it is disabled.
fn caret_blink_ms() -> u32 {
    // SAFETY: no arguments, no out-params.
    let ms = unsafe { GetCaretBlinkTime() };
    if ms == 0 || ms == u32::MAX {
        530
    } else {
        ms
    }
}

// ---------------------------------------------------------------------------
// Drawing helpers
// ---------------------------------------------------------------------------

/// Composite `alpha`/255 of `fg` over `bg`.
///
/// On screen the overlays are layered windows and the compositor does this. The
/// preview draws into an ordinary bitmap, so it has to do the arithmetic — and
/// the result is exact rather than approximate, because every mock cell is laid
/// over a single flat tone.
fn blend(fg: COLORREF, bg: COLORREF, alpha: u8) -> COLORREF {
    let channel = |shift: u32| -> u32 {
        let f = ((fg.0 >> shift) & 0xFF) as i32;
        let b = ((bg.0 >> shift) & 0xFF) as i32;
        (b + (f - b) * alpha as i32 / 255).clamp(0, 255) as u32
    };
    COLORREF(channel(0) | (channel(8) << 8) | (channel(16) << 16))
}

/// A one-pixel-or-thicker frame. Four fills rather than a pen, which would have
/// to be created, selected, restored and deleted for the same result.
fn draw_frame(dc: HDC, r: Rect, t: i32, colour: COLORREF) {
    if r.width() <= 0 || r.height() <= 0 || t <= 0 {
        return;
    }
    theme::fill_rect(dc, Rect::new(r.left, r.top, r.right, r.top + t), colour);
    theme::fill_rect(
        dc,
        Rect::new(r.left, r.bottom - t, r.right, r.bottom),
        colour,
    );
    theme::fill_rect(dc, Rect::new(r.left, r.top, r.left + t, r.bottom), colour);
    theme::fill_rect(dc, Rect::new(r.right - t, r.top, r.right, r.bottom), colour);
}

/// A rectangular or rounded region.
///
/// # Safety
/// Returns an owned region; the caller must delete it.
unsafe fn shape(r: Rect, radius: i32) -> HRGN {
    if radius > 0 {
        // CreateRoundRectRgn takes the full width and height of the corner
        // ellipse, which is twice the radius.
        CreateRoundRectRgn(r.left, r.top, r.right, r.bottom, radius * 2, radius * 2)
    } else {
        CreateRectRgn(r.left, r.top, r.right, r.bottom)
    }
}

/// Outline one cell exactly as [`crate::ui::highlight::Highlight::show_grid`]
/// does: inside the cell, never around it, with the hole rounded a little
/// tighter so the band keeps an even thickness around the curve.
fn draw_cell_outline(dc: HDC, cell: Rect, colour: COLORREF, border: i32, radius: i32) {
    if cell.width() <= border * 2 || cell.height() <= border * 2 {
        return;
    }
    // SAFETY: both regions and the brush are created here and deleted here;
    // FillRgn borrows them and takes ownership of nothing.
    unsafe {
        let outer = shape(cell, radius);
        let inner = shape(cell.deflate(border), (radius - border).max(0));
        CombineRgn(Some(outer), Some(outer), Some(inner), RGN_DIFF);
        let brush = CreateSolidBrush(colour);
        if !brush.is_invalid() {
            let _ = FillRgn(dc, outer, brush);
            let _ = DeleteObject(brush.into());
        }
        let _ = DeleteObject(inner.into());
        let _ = DeleteObject(outer.into());
    }
}

/// The small sample drawn beside each theme name: the theme's own accent,
/// thickness and rounding over a neutral tone, with its warning colour beside
/// it. Enough to tell eleven themes apart without selecting each in turn.
fn draw_swatch(dc: HDC, r: Rect, look: &Look, dpi: u32) {
    let s = |dip: i32| scale(dpi, dip);
    let ground = COLORREF(0x0056_5654);
    let warn_w = s(22);
    let sample = Rect::new(
        r.left,
        r.top,
        (r.right - warn_w - s(6)).max(r.left),
        r.bottom,
    );
    if sample.width() > s(8) {
        theme::fill_round_rect(dc, sample, s(4), ground);
        draw_cell_outline(
            dc,
            sample.deflate(s(3)),
            blend(look.accent, ground, look.outline_alpha),
            look.border_px(dpi),
            look.corner_px(dpi),
        );
    }
    let warn = Rect::new(r.right - warn_w, r.top, r.right, r.bottom);
    theme::fill_round_rect(dc, warn, s(4), look.warning);
}

/// The preview panel: three mock cells over three flat backgrounds.
///
/// The three tones are the point. A theme that reads beautifully over a dark
/// wallpaper can vanish over a light one, and the only way to see that without
/// dragging a real window across a real desktop is to show both at once.
fn draw_preview(st: &mut State, dc: HDC, panel: Rect, look: &Look, title: &str) {
    let dpi = st.dpi;
    let s = |dip: i32| scale(dpi, dip);
    let c = st.colors;
    let pad = s(24);

    theme::fill_rect(dc, panel, c.bg);

    // SAFETY: the fonts live in State for the whole paint; the entry selection
    // is restored at the end of this function.
    let old = unsafe { SelectObject(dc, st.font_small.0.into()) };
    theme::draw_text(dc, "Preview", pad, panel.top + s(14), c.subtle);
    let (label_w, _) = theme::measure(dc, "Preview");
    theme::draw_text(dc, title, pad + label_w + s(12), panel.top + s(14), c.fg);

    let desk = Rect::new(
        pad,
        panel.top + s(36),
        panel.right - pad,
        panel.bottom - s(14),
    );
    if desk.width() < s(220) || desk.height() < s(110) {
        // SAFETY: restoring the font selected on entry.
        unsafe {
            SelectObject(dc, old);
        }
        return;
    }

    // Three flat backgrounds, one per cell, so the alpha arithmetic in `blend`
    // is exact and the theme is judged over dark, light and mid tones at once.
    let dark_ground = COLORREF(0x0028_2320);
    let light_ground = COLORREF(0x00EE_E9E6);
    let mid_ground = COLORREF(0x0086_7C74);

    let split_x = desk.left + desk.width() * 56 / 100;
    let mid_y = desk.top + desk.height() / 2;
    let block_a = Rect::new(desk.left, desk.top, split_x, desk.bottom);
    let block_b = Rect::new(split_x, desk.top, desk.right, mid_y);
    let block_c = Rect::new(split_x, mid_y, desk.right, desk.bottom);
    theme::fill_rect(dc, block_a, dark_ground);
    theme::fill_rect(dc, block_b, light_ground);
    theme::fill_rect(dc, block_c, mid_ground);

    let gap = s(12);
    let border = look.border_px(dpi);
    let radius = look.corner_px(dpi);

    // --- the outlined cell, with the size readout ---------------------------
    let cell_a = block_a.deflate(gap);
    draw_cell_outline(
        dc,
        cell_a,
        blend(look.accent, dark_ground, look.outline_alpha),
        border,
        radius,
    );

    ensure_preview_font(st, look);
    // SAFETY: the cached readout font outlives this paint.
    unsafe {
        SelectObject(dc, st.preview_font.0.into());
    }
    let readout = "1280 x 720";
    let (tw, th) = theme::measure(dc, readout);
    let chip_w = tw + s(18);
    let chip_h = th + s(10);
    if chip_w < cell_a.width() - border * 2 && chip_h < cell_a.height() - border * 2 {
        let chip = Rect::new(
            cell_a.center_x() - chip_w / 2,
            cell_a.center_y() - chip_h / 2,
            cell_a.center_x() - chip_w / 2 + chip_w,
            cell_a.center_y() - chip_h / 2 + chip_h,
        );
        theme::fill_round_rect(dc, chip, s(6), look.accent);
        theme::draw_text(
            dc,
            readout,
            chip.left + s(9),
            chip.top + s(5),
            look.on_accent,
        );
    }

    // --- the filled drop preview -------------------------------------------
    let cell_b = block_b.deflate(gap);
    if cell_b.width() > border * 2 && cell_b.height() > border * 2 {
        theme::fill_round_rect(
            dc,
            cell_b.deflate(border),
            (radius - border).max(0),
            blend(look.accent, light_ground, look.fill_alpha),
        );
        draw_cell_outline(
            dc,
            cell_b,
            blend(look.accent, light_ground, look.outline_alpha),
            border,
            radius,
        );
        caption(dc, cell_b, "Move here", look.on_accent);
    }

    // --- the refusal --------------------------------------------------------
    let cell_c = block_c.deflate(gap);
    if cell_c.width() > border * 2 && cell_c.height() > border * 2 {
        theme::fill_round_rect(
            dc,
            cell_c.deflate(border),
            (radius - border).max(0),
            blend(look.warning, mid_ground, look.fill_alpha),
        );
        draw_cell_outline(
            dc,
            cell_c,
            blend(look.warning, mid_ground, look.outline_alpha),
            border,
            radius,
        );
        caption(dc, cell_c, "Blocked", look.on_accent);
    }

    // SAFETY: restoring the font selected on entry.
    unsafe {
        SelectObject(dc, old);
    }
    theme::fill_rect(
        dc,
        Rect::new(0, panel.bottom - 1, panel.right, panel.bottom),
        c.border,
    );
}

/// Centre a caption in a cell, or draw nothing when it will not fit — the same
/// rule the real overlay applies.
fn caption(dc: HDC, cell: Rect, text: &str, colour: COLORREF) {
    let (tw, th) = theme::measure(dc, text);
    if tw + 8 < cell.width() && th + 4 < cell.height() {
        theme::draw_text(
            dc,
            text,
            cell.center_x() - tw / 2,
            cell.center_y() - th / 2,
            colour,
        );
    }
}

/// Rebuild the readout font only when the face or the size actually changed.
fn ensure_preview_font(st: &mut State, look: &Look) {
    let key = (look.font.clone(), look.font_pt);
    if st.preview_font_key == key {
        return;
    }
    st.preview_font = if look.font.is_empty() {
        Font::ui(look.font_pt, st.dpi, 600)
    } else {
        Font::named(&look.font, look.font_pt, st.dpi, 600)
    };
    st.preview_font_key = key;
}

// ---------------------------------------------------------------------------
// The document
// ---------------------------------------------------------------------------

/// Paint everything, returning the total document height in pixels.
///
/// The scrolled body is drawn first and the fixed preview panel painted over
/// the top of it. The panel fills its rectangle opaquely, so it clips the body
/// without any clipping region — one less GDI object per paint.
fn draw(st: &mut State, buf: &BackBuffer, w: i32, h: i32) -> i32 {
    let dc = buf.dc;
    let c = st.colors;
    theme::fill_rect(dc, Rect::new(0, 0, w, h), c.bg);
    st.hotspots.clear();

    let panel_h = preview_height(st.dpi);
    let look = look_for(&st.selected, &st.custom);
    let title = theme_rows(&st.custom.name)
        .into_iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(&st.selected))
        .map(|(_, label)| label)
        .unwrap_or_else(|| theme::overlay_label(&st.selected).to_string());

    let total = draw_body(st, dc, w, panel_h);
    draw_preview(st, dc, Rect::new(0, 0, w, panel_h), &look, &title);
    total
}

/// The scrolled half: the theme list and the custom-theme controls.
fn draw_body(st: &mut State, dc: HDC, w: i32, panel_h: i32) -> i32 {
    let c = st.colors;
    let dpi = st.dpi;
    let s = |dip: i32| scale(dpi, dip);
    let pad = s(24);
    let scroll = st.scroll;
    let doc = |yy: i32| yy + scroll;
    let mut y = panel_h - scroll + s(18);

    // --- the theme list -----------------------------------------------------
    y = section(dc, st, "Overlay theme", pad, y, w - pad * 2);
    // SAFETY: fonts live in State for the whole paint.
    unsafe {
        SelectObject(dc, st.font_body.0.into());
    }
    let (_, body_h) = theme::measure(dc, "Ag");
    theme::draw_text(
        dc,
        "Click a theme to use it. The preview above follows the selection.",
        pad,
        y,
        c.subtle,
    );
    y += body_h + s(12);

    let rows = theme_rows(&st.custom.name);
    let row_h = s(38);
    st.list_top = doc(y);
    st.row_h = row_h;
    st.row_count = rows.len();

    let custom = st.custom.clone();
    let selected = st.selected.clone();
    let hovered = st.hovered;
    for (i, (key, label)) in rows.iter().enumerate() {
        let is_selected = selected.eq_ignore_ascii_case(key);
        let is_row_hot = hovered == Some(Hit::Row(i));
        let band = Rect::new(pad - s(8), y + s(2), w - pad + s(8), y + row_h - s(2));
        if is_selected {
            theme::fill_round_rect(dc, band, s(8), c.sel_bg);
        } else if is_row_hot {
            theme::fill_round_rect(dc, band, s(8), c.separator);
        }

        let dot_size = s(10);
        let dot = Rect::new(
            pad,
            y + (row_h - dot_size) / 2,
            pad + dot_size,
            y + (row_h - dot_size) / 2 + dot_size,
        );
        if is_selected {
            theme::fill_round_rect(dc, dot, dot_size / 2, c.accent);
        } else {
            // A ring rather than a tinted dot: colour alone would not survive
            // a high-contrast palette, and an empty circle is the shape people
            // already read as "not chosen".
            theme::fill_round_rect(dc, dot, dot_size / 2, c.border);
            let inner = dot.deflate(s(2).max(1));
            theme::fill_round_rect(
                dc,
                inner,
                inner.width() / 2,
                if is_row_hot { c.separator } else { c.bg },
            );
        }

        // SAFETY: font swap within a live paint.
        unsafe {
            SelectObject(dc, st.font_body.0.into());
        }
        let (_, th) = theme::measure(dc, "Ag");
        let swatch_w = s(140);
        let swatch = Rect::new(
            w - pad - swatch_w,
            y + (row_h - s(24)) / 2,
            w - pad,
            y + (row_h - s(24)) / 2 + s(24),
        );
        let text = theme::elide(dc, label, swatch.left - (pad + dot_size + s(12)) - s(12));
        theme::draw_text(
            dc,
            &text,
            pad + dot_size + s(12),
            y + (row_h - th) / 2,
            if is_selected { c.sel_fg } else { c.fg },
        );
        draw_swatch(dc, swatch, &look_for(key, &custom), dpi);
        y += row_h;
    }
    y += s(22);

    // --- the custom theme ---------------------------------------------------
    y = section(dc, st, "Custom theme", pad, y, w - pad * 2);
    // SAFETY: font swap within a live paint.
    unsafe {
        SelectObject(dc, st.font_body.0.into());
    }
    theme::draw_text(
        dc,
        "Edits apply immediately. Choose the custom entry above to use them.",
        pad,
        y,
        c.subtle,
    );
    y += body_h + s(16);

    let label_w = s(150);
    let ctrl_x = pad + label_w;
    let field_h = s(28);

    // Name, as an inline field. A single short string does not justify a real
    // edit control, its subclassing, or its focus rules.
    {
        theme::draw_text(dc, "Name", pad, y + (field_h - body_h) / 2, c.fg);
        let field = Rect::new(ctrl_x, y, ctrl_x + s(280), y + field_h);
        theme::fill_round_rect(dc, field, s(6), c.separator);
        draw_frame(
            dc,
            field,
            s(1).max(1),
            if st.editing_name { c.accent } else { c.border },
        );
        let text_x = field.left + s(9);
        let text_y = y + (field_h - body_h) / 2;
        let shown = theme::elide(dc, &st.custom.name, field.width() - s(24));
        let tw = theme::draw_text(dc, &shown, text_x, text_y, c.fg);
        if st.editing_name && st.caret_on {
            theme::fill_rect(
                dc,
                Rect::new(text_x + tw + 2, text_y, text_x + tw + 4, text_y + body_h),
                c.accent,
            );
        }
        st.hotspots.push(Hotspot {
            rect: Rect::new(field.left, doc(field.top), field.right, doc(field.bottom)),
            act: Act::EditName,
        });
        y += field_h + s(10);
    }

    // Colours.
    for sw in Swatch::ALL {
        let hex = sw.hex(&st.custom).to_string();
        let colour = theme::parse_hex(&hex).unwrap_or(theme::OVERLAY_WINDOWS.accent);
        theme::draw_text(dc, sw.label(), pad, y + (field_h - body_h) / 2, c.fg);
        let chip = Rect::new(ctrl_x, y + s(2), ctrl_x + s(72), y + field_h - s(2));
        theme::fill_round_rect(dc, chip, s(5), colour);
        draw_frame(dc, chip, s(1).max(1), c.border);
        theme::draw_text(
            dc,
            &hex,
            chip.right + s(12),
            y + (field_h - body_h) / 2,
            c.subtle,
        );
        st.hotspots.push(Hotspot {
            rect: Rect::new(chip.left, doc(chip.top), chip.right, doc(chip.bottom)),
            act: Act::Pick(sw),
        });
        y += field_h + s(6);
    }
    y += s(8);

    // Numbers.
    let btn = s(28);
    for field in Field::ALL {
        let value = field.get(&st.custom);
        let (lo, hi) = field.range();
        theme::draw_text(dc, field.label(), pad, y + (btn - body_h) / 2, c.fg);

        let minus = Rect::new(ctrl_x, y, ctrl_x + btn, y + btn);
        let value_box = Rect::new(minus.right + s(4), y, minus.right + s(4) + s(58), y + btn);
        let plus = Rect::new(
            value_box.right + s(4),
            y,
            value_box.right + s(4) + btn,
            y + btn,
        );

        for (r, glyph, direction, enabled) in [
            (minus, "\u{2212}", -1i32, value > lo),
            (plus, "+", 1i32, value < hi),
        ] {
            let index = st.hotspots.len();
            let hot = st.hovered == Some(Hit::Spot(index)) && enabled;
            theme::fill_round_rect(dc, r, s(6), if hot { c.sel_bg } else { c.separator });
            draw_frame(dc, r, s(1).max(1), c.border);
            let (gw, gh) = theme::measure(dc, glyph);
            theme::draw_text(
                dc,
                glyph,
                r.center_x() - gw / 2,
                r.center_y() - gh / 2,
                if enabled { c.fg } else { c.subtle },
            );
            st.hotspots.push(Hotspot {
                rect: Rect::new(r.left, doc(r.top), r.right, doc(r.bottom)),
                act: Act::Step(field, direction),
            });
        }

        let text = value.to_string();
        let (vw, _) = theme::measure(dc, &text);
        theme::draw_text(
            dc,
            &text,
            value_box.center_x() - vw / 2,
            y + (btn - body_h) / 2,
            c.fg,
        );

        // SAFETY: font swap within a live paint.
        unsafe {
            SelectObject(dc, st.font_small.0.into());
        }
        let hint = theme::elide(dc, field.hint(), w - pad - (plus.right + s(16)));
        theme::draw_text(
            dc,
            &hint,
            plus.right + s(16),
            y + (btn - body_h) / 2,
            c.subtle,
        );
        // SAFETY: back to the body font for the next row's label.
        unsafe {
            SelectObject(dc, st.font_body.0.into());
        }
        y += btn + s(8);
    }

    doc(y + s(24))
}

/// A section heading with a rule beneath it, as the About window draws them.
fn section(dc: HDC, st: &State, title: &str, x: i32, y: i32, width: i32) -> i32 {
    // SAFETY: font swap within a live paint; the caller restores the original.
    unsafe {
        SelectObject(dc, st.font_h2.0.into());
    }
    let (_, th) = theme::measure(dc, title);
    theme::draw_text(dc, title, x, y, st.colors.fg);
    let after = y + th + scale(st.dpi, 6);
    theme::fill_rect(
        dc,
        Rect::new(x, after, x + width, after + 1),
        st.colors.border,
    );
    after + scale(st.dpi, 12)
}

// ---------------------------------------------------------------------------
// Window procedure
// ---------------------------------------------------------------------------

/// # Safety
/// Only valid for windows of class [`CLASS_NAME`].
unsafe fn editor_from(hwnd: HWND) -> Option<&'static ThemeEditor> {
    // SAFETY: reads our own window's userdata slot, which holds either the
    // ThemeEditor pointer stored in WM_NCCREATE or zero.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const ThemeEditor;
    if ptr.is_null() {
        None
    } else {
        // SAFETY: null-checked above; the pointee outlives the window because
        // Drop destroys the window first.
        Some(unsafe { &*ptr })
    }
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
    let Some(e) = (unsafe { editor_from(hwnd) }) else {
        // SAFETY: default handling before the back-pointer exists.
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    };

    match msg {
        WM_PAINT => {
            e.paint();
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            e.update_scrollbar();
            // A resize can leave us scrolled past the new bottom.
            e.scroll_by(0);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == CARET_TIMER => {
            if let Ok(mut st) = e.state.try_borrow_mut() {
                st.caret_on = !st.caret_on;
            }
            e.repaint();
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as i16;
            let lines = if delta > 0 { -3 } else { 3 };
            // The Ref from a borrow lives to the end of the statement, so
            // inlining it would still be held when scroll_by borrows mutably.
            let dpi = e.state.try_borrow().map(|s| s.dpi).unwrap_or(96);
            e.scroll_by(lines * scale(dpi, 20));
            LRESULT(0)
        }
        WM_VSCROLL => {
            let (_, ch) = e.client_size();
            let dpi = e.state.try_borrow().map(|s| s.dpi).unwrap_or(96);
            let step = scale(dpi, 40);
            match SCROLLBAR_COMMAND((wp.0 & 0xFFFF) as i32) {
                SB_LINEUP => e.scroll_by(-step),
                SB_LINEDOWN => e.scroll_by(step),
                SB_PAGEUP => e.scroll_by(-ch),
                SB_PAGEDOWN => e.scroll_by(ch),
                SB_TOP => e.scroll_to(0),
                SB_BOTTOM => e.scroll_to(i32::MAX),
                SB_THUMBTRACK | SB_THUMBPOSITION => {
                    let mut si = SCROLLINFO {
                        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                        fMask: SIF_TRACKPOS,
                        ..Default::default()
                    };
                    // SAFETY: si is initialised with a correct cbSize.
                    unsafe {
                        let _ = GetScrollInfo(hwnd, SB_VERT, &mut si);
                    }
                    e.scroll_to(si.nTrackPos);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let x = (lp.0 & 0xFFFF) as i16 as i32;
            let y = ((lp.0 >> 16) & 0xFFFF) as i16 as i32;
            let hit = e.probe(x, y);
            let changed = match e.state.try_borrow_mut() {
                Ok(mut st) if st.hovered != hit => {
                    st.hovered = hit;
                    true
                }
                _ => false,
            };
            if changed {
                // SAFETY: loading a stock cursor; a failure leaves the current
                // one in place, which is harmless.
                unsafe {
                    let cursor = if hit.is_some() { IDC_HAND } else { IDC_ARROW };
                    if let Ok(cur) = LoadCursorW(None, cursor) {
                        SetCursor(Some(cur));
                    }
                }
                e.repaint();
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = (lp.0 & 0xFFFF) as i16 as i32;
            let y = ((lp.0 >> 16) & 0xFFFF) as i16 as i32;
            let hit = e.probe(x, y);
            // The action is resolved and the borrow released before anything
            // that can pump messages: `pick` enters a modal dialog.
            let act = match hit {
                Some(Hit::Spot(i)) => e
                    .state
                    .try_borrow()
                    .ok()
                    .and_then(|st| st.hotspots.get(i).map(|h| h.act)),
                _ => None,
            };
            // Clicking anywhere but the name field commits the name, which is
            // what every other text field on the platform does.
            if act != Some(Act::EditName) {
                e.stop_editing();
            }
            match (hit, act) {
                (_, Some(Act::Pick(sw))) => e.pick(sw),
                (_, Some(Act::Step(f, d))) => e.step(f, d),
                (_, Some(Act::EditName)) => e.start_editing(),
                (Some(Hit::Row(i)), None) => e.select_row(i),
                _ => {}
            }
            LRESULT(0)
        }
        WM_CHAR => {
            e.on_char(wp.0 as u16);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let (_, ch) = e.client_size();
            let dpi = e.state.try_borrow().map(|s| s.dpi).unwrap_or(96);
            let step = scale(dpi, 40);
            let editing = e
                .state
                .try_borrow()
                .map(|s| s.editing_name)
                .unwrap_or(false);
            if editing {
                // Backspace arrives as WM_CHAR as well; both are accepted so a
                // keyboard layout that suppresses one still deletes.
                match VIRTUAL_KEY(wp.0 as u16) {
                    VK_ESCAPE | VK_RETURN => e.stop_editing(),
                    VK_BACK => e.on_char(8),
                    _ => {}
                }
                return LRESULT(0);
            }
            match VIRTUAL_KEY(wp.0 as u16) {
                VK_ESCAPE => e.hide(),
                VK_UP => e.scroll_by(-step),
                VK_DOWN => e.scroll_by(step),
                VK_PRIOR => e.scroll_by(-ch),
                VK_NEXT => e.scroll_by(ch),
                VK_HOME => e.scroll_to(0),
                VK_END => e.scroll_to(i32::MAX),
                // SAFETY: default handling for keys we do not consume; every
                // argument came from the system unchanged.
                _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
            }
            LRESULT(0)
        }
        // Closing hides: the window is owned by the application and reused.
        WM_CLOSE => {
            e.hide();
            LRESULT(0)
        }
        // SAFETY: default handling for messages we do not process; every
        // argument came from the system unchanged.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaling_matches_the_shared_helper() {
        assert_eq!(scale(96, 38), 38);
        assert_eq!(scale(192, 38), 76);
        assert_eq!(scale(144, 10), 15);
    }

    #[test]
    fn the_changed_message_is_in_the_app_range_and_unique() {
        const { assert!(WM_THEME_CHANGED >= WM_APP) };
        assert_ne!(WM_THEME_CHANGED, crate::ui::keys::WM_KEYS_CHANGED);
        assert_ne!(WM_THEME_CHANGED, crate::ui::palette::WM_PALETTE_ACCEPT);
        assert_ne!(WM_THEME_CHANGED, crate::ui::palette::WM_PALETTE_CANCEL);
        assert_ne!(WM_THEME_CHANGED, crate::tray::WM_TRAY_CALLBACK);
        assert_ne!(WM_THEME_CHANGED, crate::app::WM_APPS_READY);
    }

    // --- value handling -----------------------------------------------------

    #[test]
    fn every_field_clamps_to_its_documented_range() {
        for f in Field::ALL {
            let (lo, hi) = f.range();
            assert!(lo < hi, "{:?} has an empty range", f);
            assert_eq!(clamp_field(f, i32::MIN), lo);
            assert_eq!(clamp_field(f, i32::MAX), hi);
            assert_eq!(clamp_field(f, lo), lo);
            assert_eq!(clamp_field(f, hi), hi);
        }
    }

    #[test]
    fn the_controls_stop_where_the_renderer_stops() {
        // A control must not offer travel the renderer will clamp away.
        assert_eq!(Field::OutlineAlpha.range(), (60, 255));
        assert_eq!(Field::FillAlpha.range(), (20, 200));
        assert_eq!(Field::BorderDip.range(), (1, 8));
        assert_eq!(Field::CornerDip.range(), (0, 24));
        assert_eq!(Field::FontPt.range(), (8, 24));
    }

    #[test]
    fn stepping_moves_by_the_field_step_and_stops_at_the_limits() {
        assert_eq!(stepped(Field::BorderDip, 3, 1), 4);
        assert_eq!(stepped(Field::BorderDip, 3, -1), 2);
        assert_eq!(stepped(Field::OutlineAlpha, 210, 1), 215);
        assert_eq!(stepped(Field::OutlineAlpha, 210, -1), 205);

        for f in Field::ALL {
            let (lo, hi) = f.range();
            assert_eq!(stepped(f, hi, 1), hi, "{:?} overshot the ceiling", f);
            assert_eq!(stepped(f, lo, -1), lo, "{:?} undershot the floor", f);
            // A value already outside the range is pulled back in rather than
            // moved further out; a hand-edited config can produce one.
            assert_eq!(stepped(f, hi + 1000, 1), hi);
            assert_eq!(stepped(f, lo - 1000, -1), lo);
        }
    }

    #[test]
    fn stepping_a_field_round_trips_through_the_custom_theme() {
        let mut c = CustomTheme::default();
        for f in Field::ALL {
            let before = f.get(&c);
            f.set(&mut c, stepped(f, before, 1));
            let after = f.get(&c);
            assert!(after >= before, "{:?} went backwards", f);
            f.set(&mut c, stepped(f, after, -1));
            assert_eq!(f.get(&c), before, "{:?} did not return", f);
        }
    }

    #[test]
    fn setting_a_field_never_stores_an_out_of_range_value() {
        let mut c = CustomTheme::default();
        for f in Field::ALL {
            let (lo, hi) = f.range();
            f.set(&mut c, 100_000);
            assert_eq!(f.get(&c), hi);
            f.set(&mut c, -100_000);
            assert_eq!(f.get(&c), lo);
        }
    }

    // --- hit-testing --------------------------------------------------------

    #[test]
    fn a_row_index_comes_from_the_y_coordinate() {
        let (top, row_h, count) = (200, 38, 12);
        assert_eq!(row_at(199, top, row_h, count), None, "above the list");
        assert_eq!(row_at(200, top, row_h, count), Some(0));
        assert_eq!(
            row_at(237, top, row_h, count),
            Some(0),
            "last pixel of row 0"
        );
        assert_eq!(row_at(238, top, row_h, count), Some(1));
        assert_eq!(row_at(top + row_h * 11, top, row_h, count), Some(11));
        assert_eq!(
            row_at(top + row_h * 12, top, row_h, count),
            None,
            "past the end"
        );
    }

    #[test]
    fn a_degenerate_row_height_does_not_divide_by_zero() {
        assert_eq!(row_at(500, 0, 0, 12), None);
        assert_eq!(row_at(500, 0, -4, 12), None);
    }

    // --- colour arithmetic --------------------------------------------------

    #[test]
    fn blending_is_exact_at_both_ends() {
        let fg = COLORREF(0x0011_2233);
        let bg = COLORREF(0x00AA_BBCC);
        assert_eq!(blend(fg, bg, 255), fg);
        assert_eq!(blend(fg, bg, 0), bg);
    }

    #[test]
    fn blending_stays_between_the_two_colours() {
        let fg = COLORREF(0x0000_0000);
        let bg = COLORREF(0x00FF_FFFF);
        for alpha in [1u8, 40, 128, 200, 254] {
            let m = blend(fg, bg, alpha);
            for shift in [0u32, 8, 16] {
                let v = (m.0 >> shift) & 0xFF;
                assert!(v <= 0xFF, "channel out of range at alpha {alpha}");
            }
            assert!(m.0 <= 0x00FF_FFFF, "no bits above the low three bytes");
        }
        // Halfway between black and white is mid grey in every channel.
        let half = blend(fg, bg, 128);
        assert_eq!(half.0 & 0xFF, (half.0 >> 8) & 0xFF);
        assert_eq!(half.0 & 0xFF, (half.0 >> 16) & 0xFF);
    }

    // --- theme resolution ---------------------------------------------------

    #[test]
    fn the_list_offers_every_built_in_plus_the_custom_one() {
        let rows = theme_rows("My theme");
        assert_eq!(rows.len(), theme::OVERLAY_THEMES.len() + 1);
        for (i, (key, _)) in theme::OVERLAY_THEMES.iter().enumerate() {
            assert_eq!(rows[i].0, *key);
        }
        let last = rows.last().unwrap();
        assert_eq!(last.0, CUSTOM_KEY);
        assert_eq!(last.1, "My theme (custom)");
    }

    #[test]
    fn an_unnamed_custom_theme_still_has_a_label() {
        // An empty row would look like a rendering fault rather than a name the
        // user deleted.
        let rows = theme_rows("   ");
        assert_eq!(rows.last().unwrap().1, "Unnamed (custom)");
    }

    #[test]
    fn the_preview_resolves_a_custom_theme_the_way_the_renderer_does() {
        // The preview must not promise what the overlay will not draw, so the
        // clamps in Look::from_custom are pinned to theme::overlay_from_custom.
        let cases = [
            CustomTheme::default(),
            CustomTheme {
                accent: "not a colour".into(),
                warning: "#00FF00".into(),
                text: "#123456".into(),
                outline_alpha: 20,
                fill_alpha: 255,
                border_dip: 99,
                corner_dip: -5,
                font: "Consolas".into(),
                font_pt: 200,
                ..CustomTheme::default()
            },
        ];
        for c in cases {
            let ours = Look::from_custom(&c);
            let theirs = Look::from_overlay(&theme::overlay_from_custom(&c));
            assert_eq!(ours, theirs, "diverged for {c:?}");
        }
    }

    #[test]
    fn every_built_in_theme_resolves_to_a_drawable_look() {
        let custom = CustomTheme::default();
        for (key, overlay) in theme::OVERLAY_THEMES {
            let look = look_for(key, &custom);
            assert_eq!(look.accent, overlay.accent, "{key}");
            assert!(look.border_px(96) >= 1, "{key} would draw no line");
            assert!(look.corner_px(96) >= 0, "{key}");
            // Rounding scales with the display, like everything else.
            assert!(look.border_px(192) >= look.border_px(96), "{key}");
        }
    }

    #[test]
    fn the_custom_key_is_not_a_built_in_name() {
        assert!(!theme::OVERLAY_THEMES
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case(CUSTOM_KEY)));
        // And an unknown key still resolves rather than panicking.
        let look = look_for("chartreuse", &CustomTheme::default());
        assert_eq!(look.accent, theme::OVERLAY_WINDOWS.accent);
    }

    #[test]
    fn a_zero_thickness_theme_is_impossible() {
        // border_dip is clamped to at least 1, and the pixel conversion floors
        // at 1 as well, so no display can round a line away entirely.
        let c = CustomTheme {
            border_dip: 0,
            ..CustomTheme::default()
        };
        assert_eq!(Look::from_custom(&c).border_px(96), 1);
        assert_eq!(Look::from_custom(&c).border_px(48), 1);
    }

    #[test]
    fn every_swatch_reads_and_writes_its_own_colour() {
        let mut c = CustomTheme::default();
        for sw in Swatch::ALL {
            sw.set_hex(&mut c, "#0A0B0C".to_string());
            assert_eq!(sw.hex(&c), "#0A0B0C", "{:?}", sw);
            assert!(!sw.label().is_empty());
        }
        // Each one is a distinct slot, not three views of the same string.
        assert_eq!(c.accent, c.warning);
        Swatch::Accent.set_hex(&mut c, "#FFFFFF".to_string());
        assert_ne!(c.accent, c.warning);
    }

    #[test]
    fn every_field_explains_itself() {
        for f in Field::ALL {
            assert!(!f.label().is_empty(), "{:?}", f);
            assert!(!f.hint().is_empty(), "{:?}", f);
            assert!(f.step() > 0, "{:?}", f);
        }
    }
}

//! The command palette: a fuzzy launcher for installed applications, open
//! windows and every SuperTile command.
//!
//! ## Ownership and reentrancy
//!
//! Win32 delivers messages by calling back into us, and calls like
//! `SetWindowPos` and `ShowWindow` dispatch messages *synchronously*. A
//! `&mut Palette` held across such a call would therefore alias the `&mut` the
//! window procedure takes — undefined behaviour, and the classic way a Rust
//! Win32 app acquires a mysterious crash.
//!
//! The fix here is that the window procedure reaches the palette through a
//! `RefCell` and uses `try_borrow_mut`, dropping any message that arrives
//! while a borrow is already live. Dropping a reentrant repaint is harmless;
//! aliasing is not. The `Palette` is boxed and never moved once its window
//! exists, so the pointer stored in `GWLP_USERDATA` stays valid.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::Config;
use crate::fuzzy;
use crate::layout::{LayoutKind, Rect};
use crate::ui::theme::{self, BackBuffer, Colors, Font};

/// Posted to the owner window when the user accepts an item.
pub const WM_PALETTE_ACCEPT: u32 = WM_APP + 20;
/// Posted to the owner when the palette closes without a choice.
pub const WM_PALETTE_CANCEL: u32 = WM_APP + 21;

const CLASS_NAME: &str = "SuperTile.Palette";
const CARET_TIMER: usize = 1;
const CARET_BLINK_MS: u32 = 530;

/// What activating an item does. Interpreted by the application, not here.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Launch a Start Menu shortcut.
    Launch(PathBuf),
    /// Switch focus to an existing window.
    Focus(isize),
    /// Apply a layout.
    SetLayout(LayoutKind),
    /// Run a named internal command.
    Command(Command),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Retile,
    TogglePause,
    OpenSettings,
    OpenAbout,
    ReloadConfig,
    IncreaseGaps,
    DecreaseGaps,
    GrowMaster,
    ShrinkMaster,
    ResetSizes,
    /// Ask Claude a question, in the browser.
    NewClaudeChat,
    Exit,
}

/// The visual family an item belongs to, which picks its glyph and label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    App,
    Window,
    Layout,
    Command,
}

impl Kind {
    fn glyph(self) -> char {
        match self {
            Kind::App => '\u{ECAA}',     // AppIconDefault
            Kind::Window => '\u{E737}',  // Window
            Kind::Layout => '\u{E80A}',  // tiles
            Kind::Command => '\u{E756}', // CommandPrompt / run
        }
    }
    fn tag(self) -> &'static str {
        match self {
            Kind::App => "App",
            Kind::Window => "Window",
            Kind::Layout => "Layout",
            Kind::Command => "Command",
        }
    }
}

/// One row in the palette.
#[derive(Debug, Clone)]
pub struct Item {
    pub title: String,
    /// Secondary text: the Start Menu folder, the owning app, or a hotkey.
    pub subtitle: String,
    pub kind: Kind,
    pub action: Action,
    /// Text the fuzzy matcher searches. Usually `title`, sometimes enriched
    /// with synonyms so "dark" finds "Theme".
    pub search_key: String,
}

impl Item {
    pub fn new(
        title: impl Into<String>,
        subtitle: impl Into<String>,
        kind: Kind,
        action: Action,
    ) -> Item {
        let title = title.into();
        let search_key = title.clone();
        Item {
            title,
            subtitle: subtitle.into(),
            kind,
            action,
            search_key,
        }
    }

    pub fn with_keywords(mut self, extra: &str) -> Item {
        self.search_key = format!("{} {}", self.search_key, extra);
        self
    }
}

/// Layout metrics, all in physical pixels for the current monitor.
#[derive(Debug, Clone, Copy)]
struct Metrics {
    width: i32,
    input_h: i32,
    row_h: i32,
    footer_h: i32,
    pad_x: i32,
    glyph_col: i32,
    radius: i32,
}

impl Metrics {
    fn for_dpi(dpi: u32, width_dip: u32) -> Metrics {
        let s = |dip: i32| ((dip as i64 * dpi as i64 + 48) / 96) as i32;
        Metrics {
            width: s(width_dip as i32),
            input_h: s(54),
            row_h: s(46),
            footer_h: s(30),
            pad_x: s(16),
            glyph_col: s(40),
            radius: s(8),
        }
    }
}

/// What typing into the palette does.
///
/// Two modes rather than an entry in the list, because they want opposite
/// things from the same keystrokes. Searching wants every character to narrow a
/// list; asking a question wants every character kept verbatim, including the
/// spaces and punctuation that make a fuzzy matcher discard everything. Putting
/// "Ask Claude" in the results meant typing a question, watching the list empty
/// out, and hoping the one entry left was still reachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Filter commands, windows and applications.
    Search,
    /// Everything typed is a question for Claude.
    AskClaude,
}

struct State {
    /// What typing does: filter, or compose a question.
    mode: Mode,
    /// Whether Ask mode is available at all, so Tab does not offer a mode that
    /// leads nowhere.
    claude_available: bool,
    items: Vec<Item>,
    /// Indices into `items`, ranked, with the match positions for highlighting.
    filtered: Vec<(usize, fuzzy::Match)>,
    query: String,
    /// The query as it stood when the user pressed Enter.
    accepted_query: String,
    selected: usize,
    scroll: usize,
    caret_on: bool,
    colors: Colors,
    dark: bool,
    dpi: u32,
    metrics: Metrics,
    max_rows: usize,
    font_title: Font,
    font_small: Font,
    font_glyph: Font,
    /// Set when the user accepts; read by the owner after WM_PALETTE_ACCEPT.
    result: Option<Action>,
}

impl State {
    fn visible_rows(&self) -> usize {
        self.filtered.len().clamp(0, self.max_rows)
    }

    fn height(&self) -> i32 {
        let rows = if self.filtered.is_empty() {
            1
        } else {
            self.visible_rows() as i32
        };
        self.metrics.input_h + 1 + rows * self.metrics.row_h + self.metrics.footer_h
    }

    /// Re-rank against the current query.
    fn refilter(&mut self) {
        // In Ask mode the text is a question, not a filter. Ranking it would
        // empty the list and imply the keystrokes were going somewhere they
        // were not.
        if self.mode == Mode::AskClaude {
            self.filtered.clear();
            self.selected = 0;
            self.scroll = 0;
            return;
        }
        self.filtered = fuzzy::rank(&self.query, &self.items, |i| i.search_key.as_str());
        self.selected = 0;
        self.scroll = 0;
    }

    /// Switch modes, if Ask mode has anywhere to go.
    fn toggle_mode(&mut self) -> bool {
        if !self.claude_available {
            return false;
        }
        self.mode = match self.mode {
            Mode::Search => Mode::AskClaude,
            Mode::AskClaude => Mode::Search,
        };
        self.refilter();
        true
    }

    fn move_selection(&mut self, delta: isize) {
        if self.filtered.is_empty() {
            return;
        }
        let n = self.filtered.len() as isize;
        // Wrap, so Down from the last row returns to the first.
        let next = (self.selected as isize + delta).rem_euclid(n);
        self.selected = next as usize;
        self.ensure_visible();
    }

    fn ensure_visible(&mut self) {
        let rows = self.visible_rows();
        if rows == 0 {
            self.scroll = 0;
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
        let max_scroll = self.filtered.len().saturating_sub(rows);
        self.scroll = self.scroll.min(max_scroll);
    }

    fn selected_action(&self) -> Option<Action> {
        // In Ask mode there is no list to select from: the whole query is the
        // action, and an empty one is not worth sending.
        if self.mode == Mode::AskClaude {
            return (!self.query.trim().is_empty())
                .then_some(Action::Command(Command::NewClaudeChat));
        }
        let (idx, _) = self.filtered.get(self.selected)?;
        self.items.get(*idx).map(|i| i.action.clone())
    }
}

/// The palette window and its state.
pub struct Palette {
    hwnd: Cell<HWND>,
    owner: Cell<HWND>,
    state: RefCell<State>,
}

impl Palette {
    /// Create the (hidden) palette window.
    pub fn new(owner: HWND, cfg: &Config) -> Box<Palette> {
        let (colors, dark) = theme::colors_for(cfg.appearance.theme);
        let dpi = crate::monitor::primary().map(|m| m.dpi).unwrap_or(96);
        let metrics = Metrics::for_dpi(dpi, cfg.appearance.palette_width_dip);
        let pt = cfg.appearance.font_size_pt as i32;

        let palette = Box::new(Palette {
            hwnd: Cell::new(HWND::default()),
            owner: Cell::new(owner),
            state: RefCell::new(State {
                mode: Mode::Search,
                // Set when the item list is built, which is when the caller
                // knows whether the user has enabled the feature.
                claude_available: false,
                accepted_query: String::new(),
                items: Vec::new(),
                filtered: Vec::new(),
                query: String::new(),
                selected: 0,
                scroll: 0,
                caret_on: true,
                colors,
                dark,
                dpi,
                metrics,
                max_rows: cfg.appearance.palette_max_rows as usize,
                font_title: Font::ui(pt + 1, dpi, 400),
                font_small: Font::ui(pt - 2, dpi, 400),
                font_glyph: glyph_font(pt + 3, dpi),
                result: None,
            }),
        });

        if !theme::class_exists(CLASS_NAME) {
            let _ = theme::register_class(CLASS_NAME, wndproc);
        }

        let ptr: *const Palette = &*palette;
        let class = crate::util::WideStr::new(CLASS_NAME);
        // SAFETY: `ptr` targets the boxed Palette, which outlives the window —
        // Drop destroys the window before the box is freed. lpCreateParams is
        // read back in WM_NCCREATE.
        let hwnd = unsafe {
            let hinst = GetModuleHandleW(None).unwrap_or_default();
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                class.as_pcwstr(),
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                metrics.width,
                metrics.input_h,
                None,
                None,
                Some(hinst.into()),
                Some(ptr as *const core::ffi::c_void),
            )
        };
        if let Ok(h) = hwnd {
            palette.hwnd.set(h);
            theme::apply_window_chrome(h, dark, true);
        }
        palette
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd.get()
    }

    /// What the user has typed, for commands that act on the query itself
    /// rather than on the entry they selected.
    /// Tell the palette whether Ask mode has anywhere to go.
    pub fn set_claude_available(&self, available: bool) {
        if let Ok(mut st) = self.state.try_borrow_mut() {
            st.claude_available = available;
            if !available && st.mode == Mode::AskClaude {
                st.mode = Mode::Search;
                st.refilter();
            }
        }
    }

    /// Which mode the palette is in, for the caller's own decisions.
    pub fn mode(&self) -> Mode {
        self.state
            .try_borrow()
            .map(|st| st.mode)
            .unwrap_or(Mode::Search)
    }

    pub fn query(&self) -> String {
        self.state
            .try_borrow()
            .map(|st| st.query.trim().to_string())
            .unwrap_or_default()
    }

    /// What was typed when the user pressed Enter.
    ///
    /// [`Self::query`] is the live text and is empty by the time an accepted
    /// action is handled, because hiding the palette clears it. Anything acting
    /// on what the user typed wants this one.
    pub fn accepted_query(&self) -> String {
        self.state
            .try_borrow()
            .map(|st| st.accepted_query.clone())
            .unwrap_or_default()
    }

    pub fn is_visible(&self) -> bool {
        let h = self.hwnd.get();
        // SAFETY: IsWindowVisible tolerates any HWND, including null.
        !h.is_invalid() && unsafe { IsWindowVisible(h) }.as_bool()
    }

    /// Re-read theme, size and font settings after a config change.
    pub fn apply_config(&self, cfg: &Config) {
        let Ok(mut st) = self.state.try_borrow_mut() else {
            return;
        };
        let (colors, dark) = theme::colors_for(cfg.appearance.theme);
        st.colors = colors;
        st.dark = dark;
        st.max_rows = cfg.appearance.palette_max_rows as usize;
        st.metrics = Metrics::for_dpi(st.dpi, cfg.appearance.palette_width_dip);
        let pt = cfg.appearance.font_size_pt as i32;
        st.font_title = Font::ui(pt + 1, st.dpi, 400);
        st.font_small = Font::ui(pt - 2, st.dpi, 400);
        st.font_glyph = glyph_font(pt + 3, st.dpi);
        drop(st);
        theme::apply_window_chrome(self.hwnd.get(), dark, true);
    }

    /// Show the palette, centred on the monitor under the cursor.
    pub fn show(&self, items: Vec<Item>, cfg: &Config) {
        let hwnd = self.hwnd.get();
        if hwnd.is_invalid() {
            return;
        }

        // Follow the cursor's monitor, and pick up its DPI before measuring.
        let mut cursor = POINT::default();
        // SAFETY: valid out-param.
        let _ = unsafe { GetCursorPos(&mut cursor) };
        let mon = crate::monitor::from_point(cursor.x, cursor.y).or_else(crate::monitor::primary);

        {
            let Ok(mut st) = self.state.try_borrow_mut() else {
                return;
            };
            if let Some(m) = &mon {
                if m.dpi != st.dpi {
                    st.dpi = m.dpi;
                    let pt = cfg.appearance.font_size_pt as i32;
                    st.font_title = Font::ui(pt + 1, m.dpi, 400);
                    st.font_small = Font::ui(pt - 2, m.dpi, 400);
                    st.font_glyph = glyph_font(pt + 3, m.dpi);
                }
                st.metrics = Metrics::for_dpi(st.dpi, cfg.appearance.palette_width_dip);
            }
            st.items = items;
            // A mode is a transient choice, not a setting: opening the palette
            // should always start from the same place.
            st.mode = Mode::Search;
            st.query.clear();
            st.result = None;
            st.caret_on = true;
            st.refilter();
        }

        self.resize_to_content(mon.as_ref());

        // SAFETY: hwnd is live. The show/focus sequence is the standard recipe
        // for giving a popup keyboard focus from a hotkey.
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let _ = SetForegroundWindow(hwnd);
            let _ = SetFocus(Some(hwnd));
            let _ = SetTimer(Some(hwnd), CARET_TIMER, CARET_BLINK_MS, None);
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }

    pub fn hide(&self) {
        let hwnd = self.hwnd.get();
        if hwnd.is_invalid() {
            return;
        }
        // SAFETY: hwnd is live; killing an unset timer is harmless.
        unsafe {
            let _ = KillTimer(Some(hwnd), CARET_TIMER);
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        if let Ok(mut st) = self.state.try_borrow_mut() {
            st.items.clear();
            st.filtered.clear();
            st.query.clear();
        }
    }

    /// Take the accepted action, if any. Clears it.
    pub fn take_result(&self) -> Option<Action> {
        self.state
            .try_borrow_mut()
            .ok()
            .and_then(|mut s| s.result.take())
    }

    fn resize_to_content(&self, mon: Option<&crate::monitor::Monitor>) {
        let hwnd = self.hwnd.get();
        let Ok(st) = self.state.try_borrow() else {
            return;
        };
        let work = mon
            .map(|m| m.work_area)
            .unwrap_or(Rect::new(0, 0, 1920, 1080));

        let w = st
            .metrics
            .width
            .min(work.width() - st.metrics.pad_x * 2)
            .max(320);
        let h = st.height().min(work.height() - st.metrics.pad_x * 2);
        let x = work.left + (work.width() - w) / 2;
        // Sit in the upper third: comfortable to read without covering the
        // window the user is about to act on.
        let y = work.top + (work.height() - h).max(0) / 5;
        drop(st);

        // SAFETY: hwnd is live; SWP_NOACTIVATE keeps focus where it is.
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                w,
                h,
                SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
        }
    }

    fn accept(&self) {
        let action = {
            let Ok(mut st) = self.state.try_borrow_mut() else {
                return;
            };
            let a = st.selected_action();
            st.result = a.clone();
            // Kept because `hide` clears the live query, and the owner reads
            // the result only after the accept message arrives -- by which time
            // the text is long gone. Asking Claude sent an empty question for
            // exactly this reason.
            st.accepted_query = st.query.trim().to_string();
            a
        };
        self.hide();
        if action.is_some() {
            // SAFETY: owner is the app's message window, live for the process.
            unsafe {
                let _ = PostMessageW(
                    Some(self.owner.get()),
                    WM_PALETTE_ACCEPT,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    }

    fn cancel(&self) {
        self.hide();
        // SAFETY: as above.
        unsafe {
            let _ = PostMessageW(
                Some(self.owner.get()),
                WM_PALETTE_CANCEL,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }

    fn on_char(&self, ch: u16) {
        let c = char::from_u32(ch as u32).unwrap_or('\0');
        // Control characters arrive here too; only text belongs in the query.
        if (c as u32) < 0x20 || c == '\u{7F}' {
            return;
        }
        {
            let Ok(mut st) = self.state.try_borrow_mut() else {
                return;
            };
            st.query.push(c);
            st.refilter();
        }
        self.after_query_change();
    }

    fn backspace(&self, whole_word: bool) {
        {
            let Ok(mut st) = self.state.try_borrow_mut() else {
                return;
            };
            if whole_word {
                let trimmed = st.query.trim_end();
                match trimmed.rfind(char::is_whitespace) {
                    Some(i) => st.query.truncate(i + 1),
                    None => st.query.clear(),
                }
            } else {
                st.query.pop();
            }
            st.refilter();
        }
        self.after_query_change();
    }

    fn after_query_change(&self) {
        let mut cursor = POINT::default();
        // SAFETY: valid out-param.
        let _ = unsafe { GetCursorPos(&mut cursor) };
        let mon = crate::monitor::from_point(cursor.x, cursor.y);
        self.resize_to_content(mon.as_ref());
        // SAFETY: hwnd is live.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd.get()), None, false);
        }
    }

    fn repaint(&self) {
        // SAFETY: hwnd is live.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd.get()), None, false);
        }
    }

    /// Which visible row is at client `y`, if any.
    fn row_at(&self, y: i32) -> Option<usize> {
        let st = self.state.try_borrow().ok()?;
        if st.filtered.is_empty() {
            return None;
        }
        let top = st.metrics.input_h + 1;
        if y < top {
            return None;
        }
        let row = ((y - top) / st.metrics.row_h) as usize;
        if row >= st.visible_rows() {
            return None;
        }
        let index = st.scroll + row;
        (index < st.filtered.len()).then_some(index)
    }

    fn paint(&self) {
        let hwnd = self.hwnd.get();
        let mut ps = PAINTSTRUCT::default();
        // SAFETY: BeginPaint/EndPaint are correctly paired below.
        let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
        if hdc.is_invalid() {
            return;
        }

        let mut client = RECT::default();
        // SAFETY: valid out-param.
        let _ = unsafe { GetClientRect(hwnd, &mut client) };
        let w = client.right - client.left;
        let h = client.bottom - client.top;

        if let (Ok(st), Some(buf)) = (self.state.try_borrow(), BackBuffer::new(hdc, w, h)) {
            self.draw(&st, &buf, w, h);
            buf.present(hdc);
        }

        // SAFETY: paired with BeginPaint above.
        unsafe {
            let _ = EndPaint(hwnd, &ps);
        }
    }

    fn draw(&self, st: &State, buf: &BackBuffer, w: i32, h: i32) {
        let dc = buf.dc;
        let c = st.colors;
        let m = st.metrics;

        theme::fill_rect(dc, Rect::new(0, 0, w, h), c.bg);

        // --- query row ---
        // SAFETY: fonts are live for the duration of the paint.
        let old_font = unsafe { SelectObject(dc, st.font_glyph.0.into()) };
        // A different glyph for each mode, in the accent colour once there is
        // something typed. The mode has to be visible at a glance: hidden state
        // that changes what Enter does is how a keystroke goes somewhere the
        // user never intended.
        let glyph = match st.mode {
            // A magnifier for searching, a speech bubble for asking.
            Mode::Search => "\u{E721}",
            Mode::AskClaude => "\u{E8BD}",
        };
        let (gw, gh) = theme::measure(dc, glyph);
        theme::draw_text(
            dc,
            glyph,
            m.pad_x,
            (m.input_h - gh) / 2,
            if st.mode == Mode::AskClaude || !st.query.is_empty() {
                c.accent
            } else {
                c.subtle
            },
        );
        let _ = gw;

        // SAFETY: swapping the selected font; restored at the end.
        unsafe {
            SelectObject(dc, st.font_title.0.into());
        }
        let text_x = m.pad_x + m.glyph_col;
        let (_, th) = theme::measure(dc, "Ag");
        let ty = (m.input_h - th) / 2;

        if st.query.is_empty() {
            let hint = match (st.mode, st.claude_available) {
                (Mode::AskClaude, _) => "Ask Claude a question…  (Esc to go back)",
                (Mode::Search, true) => "Search apps, windows and commands…  (Tab to ask Claude)",
                (Mode::Search, false) => "Search apps, windows and commands…",
            };
            theme::draw_text(dc, hint, text_x, ty, c.subtle);
        } else {
            let tw = theme::draw_text(dc, &st.query, text_x, ty, c.fg);
            if st.caret_on {
                theme::fill_rect(
                    dc,
                    Rect::new(text_x + tw + 2, ty, text_x + tw + 4, ty + th),
                    c.accent,
                );
            }
        }

        // --- separator ---
        theme::fill_rect(dc, Rect::new(0, m.input_h, w, m.input_h + 1), c.separator);

        // --- results ---
        let mut y = m.input_h + 1;
        if st.filtered.is_empty() {
            // SAFETY: font swap, restored below.
            unsafe {
                SelectObject(dc, st.font_title.0.into());
            }
            let msg = if st.items.is_empty() {
                "Loading…"
            } else {
                "No matches"
            };
            let (_, mh) = theme::measure(dc, msg);
            theme::draw_text(dc, msg, text_x, y + (m.row_h - mh) / 2, c.subtle);
        } else {
            let rows = st.visible_rows();
            for row in 0..rows {
                let idx = st.scroll + row;
                let Some((item_index, mat)) = st.filtered.get(idx) else {
                    break;
                };
                let Some(item) = st.items.get(*item_index) else {
                    continue;
                };
                let selected = idx == st.selected;
                self.draw_row(st, dc, w, y, item, mat, selected);
                y += m.row_h;
            }
        }

        // --- footer ---
        let fy = h - m.footer_h;
        theme::fill_rect(dc, Rect::new(0, fy, w, fy + 1), c.separator);
        // SAFETY: font swap, restored below.
        unsafe {
            SelectObject(dc, st.font_small.0.into());
        }
        let (_, fh) = theme::measure(dc, "Ag");
        let fty = fy + (m.footer_h - fh) / 2;
        theme::draw_text(
            dc,
            "↑↓ navigate   ↵ open   esc close",
            m.pad_x,
            fty,
            c.subtle,
        );

        let count = if st.filtered.len() == st.items.len() {
            format!("{} items", st.items.len())
        } else {
            format!("{} of {}", st.filtered.len(), st.items.len())
        };
        let (cw, _) = theme::measure(dc, &count);
        theme::draw_text(dc, &count, w - m.pad_x - cw, fty, c.subtle);

        // SAFETY: restoring the font selected on entry.
        unsafe {
            SelectObject(dc, old_font);
        }
    }

    // A paint helper: every argument is drawing context that the caller
    // already has in hand. Bundling them into a struct would only move the
    // same fields somewhere else.
    #[allow(clippy::too_many_arguments)]
    fn draw_row(
        &self,
        st: &State,
        dc: HDC,
        w: i32,
        y: i32,
        item: &Item,
        mat: &fuzzy::Match,
        selected: bool,
    ) {
        let c = st.colors;
        let m = st.metrics;
        let inset = m.pad_x / 2;

        if selected {
            theme::fill_round_rect(
                dc,
                Rect::new(inset, y + 2, w - inset, y + m.row_h - 2),
                m.radius,
                c.sel_bg,
            );
            // Accent bar makes the selection unmistakable at a glance.
            theme::fill_round_rect(
                dc,
                Rect::new(inset + 2, y + m.row_h / 4, inset + 5, y + m.row_h * 3 / 4),
                2,
                c.accent,
            );
        }

        let fg = if selected { c.sel_fg } else { c.fg };

        // Kind glyph.
        // SAFETY: font swap within a live paint; restored by the caller.
        unsafe {
            SelectObject(dc, st.font_glyph.0.into());
        }
        let g = item.kind.glyph().to_string();
        let (_, gh) = theme::measure(dc, &g);
        theme::draw_text(
            dc,
            &g,
            m.pad_x,
            y + (m.row_h - gh) / 2,
            if selected { c.accent } else { c.subtle },
        );

        // Subtitle first, so the title knows how much room is left.
        // SAFETY: font swap; restored by the caller.
        unsafe {
            SelectObject(dc, st.font_small.0.into());
        }
        let sub = if item.subtitle.is_empty() {
            item.kind.tag()
        } else {
            &item.subtitle
        };
        let (sw, sh) = theme::measure(dc, sub);
        let sub_max = (w - m.pad_x * 2 - m.glyph_col) / 3;
        let (sub_text, sw) = if sw > sub_max {
            let t = theme::elide(dc, sub, sub_max);
            let ww = theme::measure(dc, &t).0;
            (t, ww)
        } else {
            (sub.to_string(), sw)
        };
        theme::draw_text(
            dc,
            &sub_text,
            w - m.pad_x - sw,
            y + (m.row_h - sh) / 2,
            c.subtle,
        );

        // Title with match highlighting.
        // SAFETY: font swap; restored by the caller.
        unsafe {
            SelectObject(dc, st.font_title.0.into());
        }
        let title_x = m.pad_x + m.glyph_col;
        let title_max = w - m.pad_x - sw - m.pad_x - title_x;
        let (_, th) = theme::measure(dc, &item.title);
        let ty = y + (m.row_h - th) / 2;

        if theme::measure(dc, &item.title).0 <= title_max {
            theme::draw_text_highlighted(
                dc,
                &item.title,
                &mat.positions,
                title_x,
                ty,
                fg,
                c.match_fg,
            );
        } else {
            // Elided titles lose their highlight positions; correctness of the
            // text matters more than the highlight here.
            let t = theme::elide(dc, &item.title, title_max);
            theme::draw_text(dc, &t, title_x, ty, fg);
        }
    }
}

impl Drop for Palette {
    fn drop(&mut self) {
        let h = self.hwnd.get();
        if !h.is_invalid() {
            // SAFETY: destroying our own window; the userdata pointer becomes
            // unreachable at the same moment the box is freed.
            unsafe {
                let _ = KillTimer(Some(h), CARET_TIMER);
                let _ = SetWindowLongPtrW(h, GWLP_USERDATA, 0);
                let _ = DestroyWindow(h);
            }
        }
    }
}

fn glyph_font(pt: i32, dpi: u32) -> Font {
    // Reuse the theme font builder, then override the face to the icon font.
    let height = -((pt * dpi as i32) / 72);
    let mut lf = LOGFONTW {
        lfHeight: height,
        lfWeight: 400,
        lfCharSet: DEFAULT_CHARSET,
        lfQuality: CLEARTYPE_QUALITY,
        ..Default::default()
    };
    for face in ["Segoe Fluent Icons", "Segoe MDL2 Assets"] {
        let src: Vec<u16> = face.encode_utf16().collect();
        let n = src.len().min(lf.lfFaceName.len() - 1);
        lf.lfFaceName = [0; 32];
        lf.lfFaceName[..n].copy_from_slice(&src[..n]);
        // SAFETY: lf is fully initialised.
        let f = unsafe { CreateFontIndirectW(&lf) };
        if !f.is_invalid() {
            return Font(f);
        }
    }
    Font::ui(pt, dpi, 400)
}

/// Recover the `Palette` a window belongs to.
///
/// # Safety
/// Only valid for windows of class [`CLASS_NAME`] whose `GWLP_USERDATA` was set
/// from a live boxed `Palette` in `WM_NCCREATE`.
unsafe fn palette_from(hwnd: HWND) -> Option<&'static Palette> {
    // SAFETY: reads our own window's userdata slot, which holds either
    // the Palette pointer stored in WM_NCCREATE or zero.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Palette;
    if ptr.is_null() {
        None
    } else {
        // SAFETY: null-checked above. The pointee is the boxed value that
        // outlives the window; see this function's `# Safety` section.
        Some(unsafe { &*ptr })
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        // SAFETY: lp is the CREATESTRUCTW Windows built for our CreateWindowExW
        // call; lpCreateParams is the *const Palette we supplied.
        let cs = unsafe { &*(lp.0 as *const CREATESTRUCTW) };
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        }
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }

    // SAFETY: userdata was set above for every window of this class.
    let Some(p) = (unsafe { palette_from(hwnd) }) else {
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    };

    match msg {
        WM_PAINT => {
            p.paint();
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1), // fully repainted in WM_PAINT
        WM_TIMER if wp.0 == CARET_TIMER => {
            if let Ok(mut st) = p.state.try_borrow_mut() {
                st.caret_on = !st.caret_on;
            }
            p.repaint();
            LRESULT(0)
        }
        WM_CHAR => {
            p.on_char(wp.0 as u16);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let vk = VIRTUAL_KEY(wp.0 as u16);
            // SAFETY: reads transient key state.
            let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
            match vk {
                VK_ESCAPE => {
                    // Escape steps back one level before it closes: leaving a
                    // half-typed question should not also dismiss the palette.
                    let left_ask = p
                        .state
                        .try_borrow_mut()
                        .map(|mut st| {
                            if st.mode == Mode::AskClaude {
                                st.mode = Mode::Search;
                                st.refilter();
                                true
                            } else {
                                false
                            }
                        })
                        .unwrap_or(false);
                    if left_ask {
                        p.repaint();
                    } else {
                        p.cancel();
                    }
                }
                VK_RETURN => p.accept(),
                VK_BACK => p.backspace(ctrl),
                VK_UP => {
                    if let Ok(mut st) = p.state.try_borrow_mut() {
                        st.move_selection(-1);
                    }
                    p.repaint();
                }
                VK_DOWN => {
                    if let Ok(mut st) = p.state.try_borrow_mut() {
                        st.move_selection(1);
                    }
                    p.repaint();
                }
                VK_TAB => {
                    // Tab switches between searching and asking Claude.
                    //
                    // It used to move the selection, which the arrow keys and
                    // Page Up/Down already do -- so the mode switch costs
                    // nothing, and Tab is where a reader looks for "the other
                    // thing this box does". When the feature is switched off
                    // there is no other thing, and Tab keeps its old meaning
                    // rather than doing nothing at all.
                    let switched = p
                        .state
                        .try_borrow_mut()
                        .map(|mut st| st.toggle_mode())
                        .unwrap_or(false);
                    if !switched {
                        // SAFETY: reads transient key state.
                        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                        if let Ok(mut st) = p.state.try_borrow_mut() {
                            st.move_selection(if shift { -1 } else { 1 });
                        }
                    }
                    p.repaint();
                }
                VK_PRIOR => {
                    if let Ok(mut st) = p.state.try_borrow_mut() {
                        let step = st.visible_rows().max(1) as isize;
                        st.move_selection(-step);
                    }
                    p.repaint();
                }
                VK_NEXT => {
                    if let Ok(mut st) = p.state.try_borrow_mut() {
                        let step = st.visible_rows().max(1) as isize;
                        st.move_selection(step);
                    }
                    p.repaint();
                }
                VK_HOME => {
                    if let Ok(mut st) = p.state.try_borrow_mut() {
                        st.selected = 0;
                        st.ensure_visible();
                    }
                    p.repaint();
                }
                VK_END => {
                    if let Ok(mut st) = p.state.try_borrow_mut() {
                        st.selected = st.filtered.len().saturating_sub(1);
                        st.ensure_visible();
                    }
                    p.repaint();
                }
                // SAFETY: default handling for keys we do not bind.
                _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let y = ((lp.0 >> 16) & 0xFFFF) as i16 as i32;
            if let Some(idx) = p.row_at(y) {
                let changed = {
                    match p.state.try_borrow_mut() {
                        Ok(mut st) if st.selected != idx => {
                            st.selected = idx;
                            true
                        }
                        _ => false,
                    }
                };
                if changed {
                    p.repaint();
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let y = ((lp.0 >> 16) & 0xFFFF) as i16 as i32;
            if let Some(idx) = p.row_at(y) {
                if let Ok(mut st) = p.state.try_borrow_mut() {
                    st.selected = idx;
                }
                p.accept();
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as i16;
            let step = if delta > 0 { -1 } else { 1 };
            if let Ok(mut st) = p.state.try_borrow_mut() {
                let rows = st.visible_rows();
                let max_scroll = st.filtered.len().saturating_sub(rows);
                st.scroll = (st.scroll as isize + step).clamp(0, max_scroll as isize) as usize;
            }
            p.repaint();
            LRESULT(0)
        }
        // Clicking away dismisses, like every other launcher.
        WM_KILLFOCUS => {
            p.cancel();
            LRESULT(0)
        }
        WM_CLOSE => {
            p.cancel();
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

    fn items(n: usize) -> Vec<Item> {
        (0..n)
            .map(|i| {
                Item::new(
                    format!("Item {i:02}"),
                    "group",
                    Kind::App,
                    Action::Command(Command::Retile),
                )
            })
            .collect()
    }

    fn state(n: usize, max_rows: usize) -> State {
        State {
            mode: Mode::Search,
            claude_available: true,
            accepted_query: String::new(),
            items: items(n),
            filtered: Vec::new(),
            query: String::new(),
            selected: 0,
            scroll: 0,
            caret_on: true,
            colors: theme::DARK,
            dark: true,
            dpi: 96,
            metrics: Metrics::for_dpi(96, 680),
            max_rows,
            font_title: Font::ui(11, 96, 400),
            font_small: Font::ui(9, 96, 400),
            font_glyph: Font::ui(11, 96, 400),
            result: None,
        }
    }

    #[test]
    fn tab_switches_modes_only_when_there_is_somewhere_to_go() {
        let mut st = state(12, 9);
        assert_eq!(st.mode, Mode::Search);
        assert!(st.toggle_mode());
        assert_eq!(st.mode, Mode::AskClaude);
        assert!(st.toggle_mode());
        assert_eq!(st.mode, Mode::Search);

        // With the feature off the mode leads nowhere, so Tab keeps its old
        // meaning rather than switching into a dead end.
        st.claude_available = false;
        assert!(!st.toggle_mode());
        assert_eq!(st.mode, Mode::Search);
    }

    #[test]
    fn asking_sends_the_whole_query_not_a_selected_item() {
        let mut st = state(12, 9);
        st.toggle_mode();
        st.query = "why is the sky blue?".into();
        st.refilter();
        // The list is deliberately empty: the text is a question, not a filter.
        assert!(st.filtered.is_empty());
        assert_eq!(
            st.selected_action(),
            Some(Action::Command(Command::NewClaudeChat))
        );
    }

    #[test]
    fn an_empty_question_is_not_worth_sending() {
        let mut st = state(12, 9);
        st.toggle_mode();
        st.query = "   ".into();
        assert_eq!(st.selected_action(), None);
    }

    #[test]
    fn searching_still_filters_after_a_round_trip_through_ask_mode() {
        let mut st = state(12, 9);
        st.query = "Item 03".into();
        st.refilter();
        let before = st.filtered.len();
        st.toggle_mode();
        st.toggle_mode();
        st.refilter();
        assert_eq!(st.filtered.len(), before, "filtering did not come back");
    }

    #[test]
    fn an_empty_query_matches_everything_in_order() {
        let mut st = state(10, 9);
        st.refilter();
        assert_eq!(st.filtered.len(), 10);
        assert_eq!(st.filtered[0].0, 0);
    }

    #[test]
    fn filtering_narrows_the_list() {
        let mut st = state(20, 9);
        st.query = "Item 03".into();
        st.refilter();
        assert!(!st.filtered.is_empty());
        let first = st.items[st.filtered[0].0].title.clone();
        assert_eq!(first, "Item 03");
    }

    #[test]
    fn a_query_matching_nothing_yields_no_rows() {
        let mut st = state(10, 9);
        st.query = "zzzzzz".into();
        st.refilter();
        assert!(st.filtered.is_empty());
        assert_eq!(st.visible_rows(), 0);
        // The window still needs a sane height for the "No matches" row.
        assert!(st.height() > st.metrics.input_h);
    }

    #[test]
    fn selection_wraps_in_both_directions() {
        let mut st = state(5, 5);
        st.refilter();
        st.move_selection(-1);
        assert_eq!(st.selected, 4, "Up from the first row wraps to the last");
        st.move_selection(1);
        assert_eq!(st.selected, 0, "Down from the last wraps to the first");
    }

    #[test]
    fn selection_never_leaves_the_filtered_range() {
        let mut st = state(7, 4);
        st.refilter();
        for delta in [1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -20, 33] {
            st.move_selection(delta);
            assert!(st.selected < st.filtered.len(), "selected={} ", st.selected);
        }
    }

    #[test]
    fn moving_selection_on_an_empty_list_is_a_no_op() {
        let mut st = state(0, 5);
        st.refilter();
        st.move_selection(1);
        st.move_selection(-1);
        assert_eq!(st.selected, 0);
        assert_eq!(st.scroll, 0);
    }

    #[test]
    fn scrolling_follows_the_selection() {
        let mut st = state(20, 5);
        st.refilter();
        assert_eq!(st.scroll, 0);
        for _ in 0..6 {
            st.move_selection(1);
        }
        assert_eq!(st.selected, 6);
        assert!(st.scroll > 0, "list should have scrolled");
        assert!(st.selected >= st.scroll && st.selected < st.scroll + st.visible_rows());
    }

    #[test]
    fn scroll_never_exceeds_the_last_page() {
        let mut st = state(20, 5);
        st.refilter();
        st.selected = 19;
        st.ensure_visible();
        assert_eq!(st.scroll, 15, "last page starts at len - visible_rows");
        assert!(st.scroll + st.visible_rows() <= st.filtered.len());
    }

    #[test]
    fn visible_rows_is_capped_by_max_rows() {
        let mut st = state(100, 9);
        st.refilter();
        assert_eq!(st.visible_rows(), 9);

        let mut small = state(3, 9);
        small.refilter();
        assert_eq!(
            small.visible_rows(),
            3,
            "a short list does not pad to max_rows"
        );
    }

    #[test]
    fn height_grows_with_the_result_count() {
        let mut a = state(2, 9);
        a.refilter();
        let mut b = state(9, 9);
        b.refilter();
        assert!(b.height() > a.height());

        let mut c = state(50, 9);
        c.refilter();
        assert_eq!(c.height(), b.height(), "height caps at max_rows");
    }

    #[test]
    fn refiltering_resets_selection_and_scroll() {
        let mut st = state(30, 5);
        st.refilter();
        st.selected = 20;
        st.ensure_visible();
        assert!(st.scroll > 0);
        st.query = "Item 1".into();
        st.refilter();
        assert_eq!(st.selected, 0);
        assert_eq!(st.scroll, 0);
    }

    #[test]
    fn the_selected_action_tracks_the_highlighted_row() {
        let mut st = State {
            items: vec![
                Item::new("Alpha", "", Kind::Command, Action::Command(Command::Retile)),
                Item::new("Beta", "", Kind::Command, Action::Command(Command::Exit)),
            ],
            ..state(0, 5)
        };
        st.refilter();
        assert_eq!(st.selected_action(), Some(Action::Command(Command::Retile)));
        st.move_selection(1);
        assert_eq!(st.selected_action(), Some(Action::Command(Command::Exit)));
    }

    #[test]
    fn no_selected_action_when_nothing_matches() {
        let mut st = state(5, 5);
        st.query = "qqqq".into();
        st.refilter();
        assert_eq!(st.selected_action(), None);
    }

    #[test]
    fn keywords_widen_matching_without_changing_the_label() {
        let item = Item::new(
            "Theme",
            "",
            Kind::Command,
            Action::Command(Command::OpenSettings),
        )
        .with_keywords("dark light appearance");
        assert_eq!(item.title, "Theme");
        assert!(fuzzy::match_score("dark", &item.search_key).is_some());
        assert!(fuzzy::match_score("appear", &item.search_key).is_some());
    }

    #[test]
    fn metrics_scale_with_dpi() {
        let at96 = Metrics::for_dpi(96, 680);
        let at192 = Metrics::for_dpi(192, 680);
        assert_eq!(at96.width, 680);
        assert_eq!(at192.width, 1360);
        assert_eq!(at192.row_h, at96.row_h * 2);
        assert!(at192.pad_x > at96.pad_x);
    }

    #[test]
    fn metrics_round_rather_than_truncate() {
        // 150% scaling of an odd value must not lose a pixel.
        let m = Metrics::for_dpi(144, 100);
        assert_eq!(m.width, 150);
    }

    #[test]
    fn every_kind_has_a_distinct_glyph_and_tag() {
        let kinds = [Kind::App, Kind::Window, Kind::Layout, Kind::Command];
        let mut glyphs: Vec<char> = kinds.iter().map(|k| k.glyph()).collect();
        let n = glyphs.len();
        glyphs.sort();
        glyphs.dedup();
        assert_eq!(glyphs.len(), n, "glyphs must be distinguishable");
        for k in kinds {
            assert!(!k.tag().is_empty());
        }
    }
}

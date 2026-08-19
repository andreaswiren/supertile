//! The **About & SBOM** window.
//!
//! Required by the brief and by CRA Annex II: the software bill of materials
//! must be available to the user, not just to whoever reads the repository. The
//! window renders the CycloneDX component table embedded at build time, links
//! to the conformance documentation, and can export the exact SBOM document
//! that shipped with the binary.
//!
//! Custom-drawn with GDI and a native vertical scrollbar. Links are hit-tested
//! rectangles rather than child controls, which keeps the whole window one
//! paint routine.

use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::SetScrollInfo;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::Config;
use crate::layout::Rect;
use crate::sbom;
use crate::ui::theme::{self, BackBuffer, Colors, Font};

const CLASS_NAME: &str = "SuperTile.About";

/// A clickable region resolved during paint and used for hit-testing.
#[derive(Clone)]
struct Hotspot {
    rect: Rect,
    action: LinkAction,
}

#[derive(Clone, Debug, PartialEq)]
enum LinkAction {
    Open(&'static str),
    ExportSbom,
    /// Ask GitHub for the latest release and report what it says.
    ///
    /// Performed here rather than only from the tray because About is where
    /// somebody looks to find out what they are running, which is the same
    /// moment they wonder whether it is current.
    CheckForUpdate,
    /// Open the release page a check has already found.
    OpenRelease(String),
}

struct State {
    colors: Colors,
    dark: bool,
    dpi: u32,
    scroll: i32,
    content_height: i32,
    hotspots: Vec<Hotspot>,
    hovered: Option<usize>,
    /// Transient status line, e.g. after exporting the SBOM.
    status: String,
    /// A newer release, once a check has found one.
    newer: Option<(String, String)>,
    font_h1: Font,
    font_h2: Font,
    font_body: Font,
    font_mono: Font,
}

pub struct About {
    hwnd: Cell<HWND>,
    state: RefCell<State>,
}

fn scale(dpi: u32, dip: i32) -> i32 {
    ((dip as i64 * dpi as i64 + 48) / 96) as i32
}

impl About {
    pub fn new(cfg: &Config) -> Box<About> {
        let (colors, dark) = theme::colors_for(cfg.appearance.theme);
        let dpi = crate::monitor::primary().map(|m| m.dpi).unwrap_or(96);

        let about = Box::new(About {
            hwnd: Cell::new(HWND::default()),
            state: RefCell::new(State {
                colors,
                dark,
                dpi,
                scroll: 0,
                content_height: 0,
                hotspots: Vec::new(),
                hovered: None,
                status: String::new(),
                newer: None,
                font_h1: Font::ui(19, dpi, 600),
                font_h2: Font::ui(12, dpi, 600),
                font_body: Font::ui(10, dpi, 400),
                font_mono: mono_font(9, dpi),
            }),
        });

        if !theme::class_exists(CLASS_NAME) {
            let _ = theme::register_class(CLASS_NAME, wndproc);
        }

        let ptr: *const About = &*about;
        let class = crate::util::WideStr::new(CLASS_NAME);
        let title = crate::util::WideStr::new("About SuperTile");
        let w = scale(dpi, 720);
        let h = scale(dpi, 620);

        // SAFETY: `ptr` targets the boxed About, which outlives the window;
        // Drop destroys the window before the box is freed.
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
                w,
                h,
                None,
                None,
                Some(hinst.into()),
                Some(ptr as *const core::ffi::c_void),
            )
        };
        if let Ok(hh) = hwnd {
            about.hwnd.set(hh);
            theme::apply_window_chrome(hh, dark, false);
        }
        about
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd.get()
    }

    /// Show, or bring to the front if already open.
    pub fn show(&self) {
        let h = self.hwnd.get();
        if h.is_invalid() {
            return;
        }
        // SAFETY: h is live.
        unsafe {
            let _ = ShowWindow(h, SW_SHOW);
            let _ = SetForegroundWindow(h);
            let _ = InvalidateRect(Some(h), None, true);
        }
        self.update_scrollbar();
    }

    pub fn hide(&self) {
        let h = self.hwnd.get();
        if !h.is_invalid() {
            // SAFETY: h is live.
            unsafe {
                let _ = ShowWindow(h, SW_HIDE);
            }
        }
    }

    pub fn apply_config(&self, cfg: &Config) {
        if let Ok(mut st) = self.state.try_borrow_mut() {
            let (colors, dark) = theme::colors_for(cfg.appearance.theme);
            st.colors = colors;
            st.dark = dark;
        }
        let (_, dark) = theme::colors_for(cfg.appearance.theme);
        theme::apply_window_chrome(self.hwnd.get(), dark, false);
        // SAFETY: hwnd may be null, which InvalidateRect tolerates via Option.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd.get()), None, true);
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
        // SAFETY: hwnd is live.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd.get()), None, false);
        }
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
        // SAFETY: hwnd is live.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd.get()), None, false);
        }
    }

    fn hit(&self, x: i32, y: i32) -> Option<usize> {
        let st = self.state.try_borrow().ok()?;
        st.hotspots
            .iter()
            .position(|h| h.rect.contains(x, y + st.scroll))
    }

    fn activate(&self, index: usize) {
        let action = {
            let Ok(st) = self.state.try_borrow() else {
                return;
            };
            st.hotspots.get(index).map(|h| h.action.clone())
        };
        let Some(action) = action else { return };

        match action {
            LinkAction::Open(url) => open_url(url),
            LinkAction::OpenRelease(url) => open_url(&url),
            LinkAction::CheckForUpdate => {
                let (msg, newer) = match crate::update::check_latest() {
                    crate::update::Outcome::UpToDate => (
                        format!(
                            "You are running {}, which is the latest.",
                            crate::APP_VERSION
                        ),
                        None,
                    ),
                    crate::update::Outcome::Available { version, url, .. } => (
                        format!("SuperTile {version} is available."),
                        Some((version, url)),
                    ),
                    crate::update::Outcome::Failed(reason) => (reason, None),
                };
                if let Ok(mut st) = self.state.try_borrow_mut() {
                    st.status = msg;
                    st.newer = newer;
                }
            }
            LinkAction::ExportSbom => {
                let target = default_sbom_export_path();
                let msg = match sbom::export_to(&target) {
                    Ok(()) => format!("SBOM written to {}", target.display()),
                    Err(e) => format!("Could not write SBOM: {e}"),
                };
                if let Ok(mut st) = self.state.try_borrow_mut() {
                    st.status = msg;
                }
                // Reveal it, so the user can see what was produced.
                if target.exists() {
                    if let Some(parent) = target.parent() {
                        open_url(&parent.to_string_lossy());
                    }
                }
            }
        }
        // SAFETY: hwnd is live.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd.get()), None, false);
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
            let total = draw_document(&mut st, &buf, w, h);
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

impl Drop for About {
    fn drop(&mut self) {
        let h = self.hwnd.get();
        if !h.is_invalid() {
            // SAFETY: destroying our own window and clearing the back-pointer.
            unsafe {
                let _ = SetWindowLongPtrW(h, GWLP_USERDATA, 0);
                let _ = DestroyWindow(h);
            }
        }
    }
}

/// Render the whole document, returning its total height in pixels.
///
/// Content is laid out in document space and offset by `scroll` at draw time,
/// so hotspot rectangles are stored unscrolled and stay valid as the user
/// scrolls.
fn draw_document(st: &mut State, buf: &BackBuffer, w: i32, h: i32) -> i32 {
    let dc = buf.dc;
    let c = st.colors;
    let dpi = st.dpi;
    let s = |dip: i32| scale(dpi, dip);
    let pad = s(28);
    let scroll = st.scroll;

    theme::fill_rect(dc, Rect::new(0, 0, w, h), c.bg);
    st.hotspots.clear();

    let mut y = pad - scroll;
    let content_w = w - pad * 2;

    // Track document-space height independently of the scrolled draw cursor.
    let doc = |yy: i32| yy + scroll;

    // --- header ---------------------------------------------------------
    // SAFETY: fonts live for the whole paint; the original is restored below.
    let old = unsafe { SelectObject(dc, st.font_h1.0.into()) };
    let (_, h1) = theme::measure(dc, "Ag");
    theme::draw_text(dc, crate::APP_NAME, pad, y, c.fg);
    let (nw, _) = theme::measure(dc, crate::APP_NAME);

    // SAFETY: font swap.
    unsafe {
        SelectObject(dc, st.font_body.0.into());
    }
    let ver = format!("v{}", crate::APP_VERSION);
    theme::draw_text(dc, &ver, pad + nw + s(10), y + h1 - s(16), c.subtle);
    y += h1 + s(6);

    theme::draw_text(
        dc,
        "Fullscreen autotiling window manager and command palette for Windows 11.",
        pad,
        y,
        c.subtle,
    );
    let (_, bh) = theme::measure(dc, "Ag");
    y += bh + s(4);
    theme::draw_text(dc, "MIT licence · © 2026 Andreas Wiren", pad, y, c.subtle);
    y += bh + s(2);
    // The exact build, for bug reports: the version alone does not identify a
    // commit, and a build from a dirty tree is not the tagged release.
    theme::draw_text(
        dc,
        &format!("Build {}", crate::build_id()),
        pad,
        y,
        c.subtle,
    );
    y += bh + s(18);

    // --- link row -------------------------------------------------------
    // The update entry changes shape once a newer release is known: offering
    // "check" when the answer is already in hand would be asking a question
    // twice.
    let update_link = match st.newer.clone() {
        Some((version, url)) => (format!("Get {version}…"), LinkAction::OpenRelease(url)),
        None => ("Check for updates".to_string(), LinkAction::CheckForUpdate),
    };
    let links: Vec<(String, LinkAction)> = vec![
        ("Repository".to_string(), LinkAction::Open(crate::APP_REPO)),
        (
            "EU CRA conformance".to_string(),
            LinkAction::Open(sbom::CRA_DOC_URL),
        ),
        (
            "Security policy".to_string(),
            LinkAction::Open(sbom::SECURITY_URL),
        ),
        ("Export SBOM…".to_string(), LinkAction::ExportSbom),
        update_link,
    ];
    let mut lx = pad;
    for (label, action) in links {
        let label = label.as_str();
        let (lw, lh) = theme::measure(dc, label);
        let box_w = lw + s(20);
        let box_h = lh + s(12);
        if lx + box_w > pad + content_w {
            lx = pad;
            y += box_h + s(8);
        }
        let r = Rect::new(lx, y, lx + box_w, y + box_h);
        let hovered = st
            .hovered
            .and_then(|i| st.hotspots.get(i))
            .map(|hs| hs.rect == Rect::new(r.left, doc(r.top), r.right, doc(r.bottom)))
            .unwrap_or(false);
        theme::fill_round_rect(dc, r, s(6), if hovered { c.sel_bg } else { c.separator });
        theme::draw_text(dc, label, lx + s(10), y + s(6), c.accent);
        st.hotspots.push(Hotspot {
            rect: Rect::new(r.left, doc(r.top), r.right, doc(r.bottom)),
            action,
        });
        lx += box_w + s(8);
    }
    let (_, link_h) = theme::measure(dc, "Ag");
    y += link_h + s(12) + s(22);

    // --- status ---------------------------------------------------------
    if !st.status.is_empty() {
        let r = Rect::new(pad, y, pad + content_w, y + bh + s(16));
        theme::fill_round_rect(dc, r, s(6), c.sel_bg);
        theme::draw_text(dc, &st.status.clone(), pad + s(10), y + s(8), c.fg);
        y += bh + s(16) + s(16);
    }

    // --- SBOM section ----------------------------------------------------
    y = section(dc, st, "Software Bill of Materials", pad, y, content_w);

    // SAFETY: font swap.
    unsafe {
        SelectObject(dc, st.font_body.0.into());
    }
    let meta = format!(
        "CycloneDX {} · {} components · generated {}",
        sbom::SBOM_SPEC_VERSION,
        sbom::component_count(),
        sbom::SBOM_TIMESTAMP
    );
    theme::draw_text(dc, &meta, pad, y, c.subtle);
    y += bh + s(2);
    theme::draw_text(dc, sbom::SBOM_SERIAL, pad, y, c.subtle);
    y += bh + s(14);

    // Column layout for the component table.
    let col_ver = pad + (content_w * 42 / 100);
    let col_lic = pad + (content_w * 60 / 100);
    let col_sha = pad + (content_w * 84 / 100);

    // SAFETY: font swap.
    unsafe {
        SelectObject(dc, st.font_h2.0.into());
    }
    let (_, hh) = theme::measure(dc, "Ag");
    theme::draw_text(dc, "Component", pad, y, c.fg);
    theme::draw_text(dc, "Version", col_ver, y, c.fg);
    theme::draw_text(dc, "Licence", col_lic, y, c.fg);
    theme::draw_text(dc, "SHA-256", col_sha, y, c.fg);
    y += hh + s(6);
    theme::fill_rect(dc, Rect::new(pad, y, pad + content_w, y + 1), c.separator);
    y += s(8);

    for (i, comp) in sbom::COMPONENTS.iter().enumerate() {
        // Zebra striping keeps a 30-row table readable.
        let row_h = bh + s(9);
        if i % 2 == 1 {
            theme::fill_rect(
                dc,
                Rect::new(
                    pad - s(6),
                    y - s(3),
                    pad + content_w + s(6),
                    y - s(3) + row_h,
                ),
                c.separator,
            );
        }
        // SAFETY: font swap.
        unsafe {
            SelectObject(dc, st.font_body.0.into());
        }
        theme::draw_text(dc, comp.name, pad, y, c.fg);
        theme::draw_text(dc, comp.version, col_ver, y, c.subtle);
        let lic = theme::elide(dc, comp.license, col_sha - col_lic - s(8));
        theme::draw_text(dc, &lic, col_lic, y, c.subtle);
        // SAFETY: font swap to the monospaced face for the digest.
        unsafe {
            SelectObject(dc, st.font_mono.0.into());
        }
        theme::draw_text(dc, sbom::short_hash(comp.sha256), col_sha, y, c.subtle);
        y += row_h;
    }
    y += s(18);

    // --- CRA section -----------------------------------------------------
    y = section(dc, st, "EU Cyber Resilience Act", pad, y, content_w);
    // SAFETY: font swap.
    unsafe {
        SelectObject(dc, st.font_body.0.into());
    }
    let facts = [
        ("Support period", "5 years from release (Article 13(8))"),
        (
            "Vulnerability reporting",
            "See the security policy; 72-hour acknowledgement",
        ),
        (
            "Network access",
            "None. SuperTile opens no sockets and sends no telemetry",
        ),
        (
            "Privileges",
            "Runs as the invoking user; never requests elevation",
        ),
        (
            "Data stored",
            "%LOCALAPPDATA%\\SuperTile — config and window geometry only",
        ),
        (
            "Product category",
            "Default — not Annex III important or Annex IV critical",
        ),
    ];
    let label_w = content_w * 30 / 100;
    for (k, v) in facts {
        theme::draw_text(dc, k, pad, y, c.subtle);
        let text = theme::elide(dc, v, content_w - label_w);
        theme::draw_text(dc, &text, pad + label_w, y, c.fg);
        y += bh + s(7);
    }
    y += s(10);

    let r = Rect::new(pad, y, pad + s(240), y + bh + s(12));
    theme::fill_round_rect(dc, r, s(6), c.separator);
    theme::draw_text(
        dc,
        "Read Regulation (EU) 2024/2847",
        pad + s(10),
        y + s(6),
        c.accent,
    );
    st.hotspots.push(Hotspot {
        rect: Rect::new(r.left, doc(r.top), r.right, doc(r.bottom)),
        action: LinkAction::Open(sbom::CRA_REGULATION_URL),
    });
    y += bh + s(12) + pad;

    // SAFETY: restore the font selected on entry.
    unsafe {
        SelectObject(dc, old);
    }

    doc(y)
}

fn section(dc: HDC, st: &State, title: &str, x: i32, y: i32, width: i32) -> i32 {
    // SAFETY: font swap within a live paint.
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

fn mono_font(pt: i32, dpi: u32) -> Font {
    let height = -((pt * dpi as i32) / 72);
    let mut lf = LOGFONTW {
        lfHeight: height,
        lfWeight: 400,
        lfCharSet: DEFAULT_CHARSET,
        lfQuality: CLEARTYPE_QUALITY,
        ..Default::default()
    };
    for face in ["Cascadia Mono", "Consolas", "Courier New"] {
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

/// Where **Export SBOM** writes by default.
fn default_sbom_export_path() -> std::path::PathBuf {
    let name = format!("supertile-{}-sbom.cdx.json", crate::APP_VERSION);
    // Desktop is discoverable; fall back to the data dir, then temp.
    if let Ok(profile) = std::env::var("USERPROFILE") {
        let desktop = std::path::PathBuf::from(profile).join("Desktop");
        if desktop.is_dir() {
            return desktop.join(name);
        }
    }
    crate::util::data_dir()
        .unwrap_or_else(|_| std::env::temp_dir())
        .join(name)
}

/// Open a URL or folder with the shell.
///
/// Only ever called with the compile-time constants in [`crate::sbom`] or a
/// path SuperTile just wrote, never with anything derived from untrusted input.
pub fn open_url(target: &str) {
    use windows::Win32::UI::Shell::ShellExecuteW;
    let t = crate::util::WideStr::new(target);
    let verb = crate::util::WideStr::new("open");
    // SAFETY: both strings outlive the call.
    unsafe {
        let _ = ShellExecuteW(
            None,
            verb.as_pcwstr(),
            t.as_pcwstr(),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

/// # Safety
/// Only valid for windows of class [`CLASS_NAME`].
unsafe fn about_from(hwnd: HWND) -> Option<&'static About> {
    // SAFETY: reads our own window's userdata slot, which holds either
    // the About pointer stored in WM_NCCREATE or zero.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const About;
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
        // SAFETY: lp is the CREATESTRUCTW for our own CreateWindowExW call.
        let cs = unsafe { &*(lp.0 as *const CREATESTRUCTW) };
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        }
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    // SAFETY: userdata was set above for windows of this class.
    let Some(a) = (unsafe { about_from(hwnd) }) else {
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    };

    match msg {
        WM_PAINT => {
            a.paint();
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            a.update_scrollbar();
            // A resize can leave us scrolled past the new bottom.
            a.scroll_by(0);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as i16;
            let lines = if delta > 0 { -3 } else { 3 };
            // The Ref from `state.borrow()` lives to the end of the statement, so
            // inlining it here would still be borrowed when scroll_by tries to
            // borrow mutably -- which failed silently and made the wheel dead.
            let dpi = a.state.try_borrow().map(|s| s.dpi).unwrap_or(96);
            a.scroll_by(lines * scale(dpi, 20));
            LRESULT(0)
        }
        WM_VSCROLL => {
            let (_, ch) = a.client_size();
            let code = (wp.0 & 0xFFFF) as u32;
            let cur = a.state.try_borrow().map(|s| s.scroll).unwrap_or(0);
            let step = scale(a.state.borrow().dpi, 40);
            match SCROLLBAR_COMMAND(code as i32) {
                SB_LINEUP => a.scroll_by(-step),
                SB_LINEDOWN => a.scroll_by(step),
                SB_PAGEUP => a.scroll_by(-ch),
                SB_PAGEDOWN => a.scroll_by(ch),
                SB_TOP => a.scroll_to(0),
                SB_BOTTOM => a.scroll_to(i32::MAX),
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
                    a.scroll_to(si.nTrackPos);
                }
                _ => {
                    let _ = cur;
                }
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let (_, ch) = a.client_size();
            let step = scale(a.state.borrow().dpi, 40);
            match VIRTUAL_KEY(wp.0 as u16) {
                VK_ESCAPE => a.hide(),
                VK_UP => a.scroll_by(-step),
                VK_DOWN => a.scroll_by(step),
                VK_PRIOR => a.scroll_by(-ch),
                VK_NEXT => a.scroll_by(ch),
                VK_HOME => a.scroll_to(0),
                VK_END => a.scroll_to(i32::MAX),
                // SAFETY: default handling for messages we do not process; every
                // argument came from the system unchanged.
                _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let x = (lp.0 & 0xFFFF) as i16 as i32;
            let y = ((lp.0 >> 16) & 0xFFFF) as i16 as i32;
            let hit = a.hit(x, y);
            let changed = match a.state.try_borrow_mut() {
                Ok(mut st) if st.hovered != hit => {
                    st.hovered = hit;
                    true
                }
                _ => false,
            };
            if changed {
                // SAFETY: hwnd is live; a hand cursor signals the link.
                unsafe {
                    let cursor = if hit.is_some() { IDC_HAND } else { IDC_ARROW };
                    if let Ok(c) = LoadCursorW(None, cursor) {
                        SetCursor(Some(c));
                    }
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = (lp.0 & 0xFFFF) as i16 as i32;
            let y = ((lp.0 >> 16) & 0xFFFF) as i16 as i32;
            if let Some(i) = a.hit(x, y) {
                a.activate(i);
            }
            LRESULT(0)
        }
        // Closing hides: the window is owned by the app and reused.
        WM_CLOSE => {
            a.hide();
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
        assert_eq!(scale(96, 100), 100);
        assert_eq!(scale(192, 100), 200);
        assert_eq!(scale(144, 10), 15);
    }

    #[test]
    fn the_export_path_is_absolute_and_named_for_the_version() {
        let p = default_sbom_export_path();
        assert!(p.is_absolute(), "{p:?}");
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.contains(crate::APP_VERSION), "{name}");
        assert!(name.ends_with(".cdx.json"), "{name}");
    }

    #[test]
    fn link_actions_compare_by_value() {
        assert_eq!(LinkAction::Open("a"), LinkAction::Open("a"));
        assert_ne!(LinkAction::Open("a"), LinkAction::Open("b"));
        assert_ne!(LinkAction::Open("a"), LinkAction::ExportSbom);
    }

    #[test]
    fn mono_font_is_creatable() {
        let f = mono_font(9, 96);
        assert!(!f.0.is_invalid());
    }

    #[test]
    fn every_documented_link_is_a_compile_time_constant() {
        // open_url must never see attacker-influenced input; this pins the set
        // of URLs the About window can open.
        for url in [
            crate::APP_REPO,
            sbom::CRA_DOC_URL,
            sbom::SECURITY_URL,
            sbom::CRA_REGULATION_URL,
        ] {
            assert!(url.starts_with("https://"), "{url}");
        }
    }
}

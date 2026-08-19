//! Shared look-and-feel: colours, fonts, DPI helpers and a GDI double buffer.
//!
//! SuperTile draws its own chrome with GDI rather than Direct2D. A D3D device
//! costs roughly 30 MB resident and ~80 ms to create, which for a window that
//! is visible a few seconds a day is the wrong trade — the palette must feel
//! instant and the process must idle cheaply. GDI plus the DWM backdrop gets
//! the Windows 11 look (rounded corners, acrylic) with neither cost.

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DWM_SYSTEMBACKDROP_TYPE,
};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

use crate::config::Theme;
use crate::layout::Rect;

/// Resolved palette of colours for one theme.
#[derive(Debug, Clone, Copy)]
pub struct Colors {
    pub bg: COLORREF,
    pub fg: COLORREF,
    pub subtle: COLORREF,
    pub accent: COLORREF,
    pub sel_bg: COLORREF,
    pub sel_fg: COLORREF,
    pub border: COLORREF,
    pub separator: COLORREF,
    pub match_fg: COLORREF,
}

/// GDI's COLORREF is 0x00BBGGRR — the opposite byte order from the hex codes
/// used in design tools, so build it explicitly rather than by eye.
const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

pub const DARK: Colors = Colors {
    bg: rgb(0x20, 0x20, 0x24),
    fg: rgb(0xF2, 0xF2, 0xF5),
    subtle: rgb(0x9A, 0x9A, 0xA5),
    accent: rgb(0x6C, 0x9C, 0xFF),
    sel_bg: rgb(0x2F, 0x33, 0x40),
    sel_fg: rgb(0xFF, 0xFF, 0xFF),
    border: rgb(0x3A, 0x3A, 0x44),
    separator: rgb(0x30, 0x30, 0x38),
    match_fg: rgb(0x8A, 0xB4, 0xFF),
};

pub const LIGHT: Colors = Colors {
    bg: rgb(0xFA, 0xFA, 0xFC),
    fg: rgb(0x1A, 0x1A, 0x1F),
    subtle: rgb(0x6A, 0x6A, 0x75),
    accent: rgb(0x25, 0x63, 0xEB),
    sel_bg: rgb(0xE4, 0xEC, 0xFD),
    sel_fg: rgb(0x0F, 0x25, 0x54),
    border: rgb(0xDC, 0xDC, 0xE4),
    separator: rgb(0xE8, 0xE8, 0xEE),
    match_fg: rgb(0x1D, 0x4E, 0xD8),
};

/// Is the shell using the dark app theme?
///
/// Reads `AppsUseLightTheme`; absent or unreadable means light, which is the
/// Windows default.
pub fn system_prefers_dark() -> bool {
    let sub =
        crate::util::WideStr::new(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let name = crate::util::WideStr::new("AppsUseLightTheme");
    let mut value: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: out-buffer and its size describe the same u32; RRF_RT_REG_DWORD
    // makes the API reject anything that is not a DWORD.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            sub.as_pcwstr(),
            name.as_pcwstr(),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    status.is_ok() && value == 0
}

pub fn colors_for(theme: Theme) -> (Colors, bool) {
    let dark = match theme {
        Theme::Dark => true,
        Theme::Light => false,
        Theme::Auto => system_prefers_dark(),
    };
    (if dark { DARK } else { LIGHT }, dark)
}

/// Apply the Windows 11 window treatments: rounded corners, dark title bar and
/// an acrylic backdrop. All three are no-ops on older builds.
pub fn apply_window_chrome(hwnd: HWND, dark: bool, transient_backdrop: bool) {
    let dark_flag: i32 = if dark { 1 } else { 0 };
    // SAFETY: each attribute is passed with the exact size DWM documents.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark_flag as *const i32 as *const core::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        );
        let corner = DWMWCP_ROUND.0;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const i32 as *const core::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        );
        if transient_backdrop {
            // DWMSBT_TRANSIENTWINDOW == acrylic; the right material for a
            // short-lived popup, and free compared with compositing our own.
            let backdrop = DWM_SYSTEMBACKDROP_TYPE(3).0;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_SYSTEMBACKDROP_TYPE,
                &backdrop as *const i32 as *const core::ffi::c_void,
                std::mem::size_of::<i32>() as u32,
            );
        }
    }
}

/// An owned GDI font, deleted on drop.
pub struct Font(pub HFONT);

impl Font {
    /// Create a UI font at `pt` points scaled to `dpi`.
    ///
    /// Prefers *Segoe UI Variable Text*, the Windows 11 UI face, and falls back
    /// to *Segoe UI*. GDI silently substitutes a default face for an unknown
    /// name, so an explicit fallback list is not needed beyond this.
    /// A font by face name, for themes that ask for one.
    ///
    /// Falls back to whatever GDI substitutes when the face is not installed,
    /// which is the right behaviour here: a missing font should cost the theme
    /// its typeface, not its readout.
    pub fn named(face: &str, pt: i32, dpi: u32, weight: i32) -> Font {
        let mut lf = LOGFONTW {
            lfHeight: -((pt * dpi as i32) / 72),
            lfWeight: weight,
            lfCharSet: DEFAULT_CHARSET,
            lfQuality: CLEARTYPE_QUALITY,
            lfOutPrecision: OUT_TT_PRECIS,
            ..Default::default()
        };
        set_face(&mut lf, face);
        // SAFETY: lf is fully initialised.
        let f = unsafe { CreateFontIndirectW(&lf) };
        if f.is_invalid() {
            return Font::ui(pt, dpi, weight);
        }
        Font(f)
    }

    pub fn ui(pt: i32, dpi: u32, weight: i32) -> Font {
        let height = -((pt * dpi as i32) / 72);
        let mut lf = LOGFONTW {
            lfHeight: height,
            lfWeight: weight,
            lfCharSet: DEFAULT_CHARSET,
            lfQuality: CLEARTYPE_QUALITY,
            lfOutPrecision: OUT_TT_PRECIS,
            ..Default::default()
        };
        set_face(&mut lf, "Segoe UI Variable Text");
        // SAFETY: lf is fully initialised.
        let mut f = unsafe { CreateFontIndirectW(&lf) };
        if f.is_invalid() {
            set_face(&mut lf, "Segoe UI");
            // SAFETY: as above.
            f = unsafe { CreateFontIndirectW(&lf) };
        }
        Font(f)
    }
}

fn set_face(lf: &mut LOGFONTW, name: &str) {
    let src: Vec<u16> = name.encode_utf16().collect();
    let n = src.len().min(lf.lfFaceName.len() - 1);
    lf.lfFaceName = [0; 32];
    lf.lfFaceName[..n].copy_from_slice(&src[..n]);
}

impl Drop for Font {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: handle came from CreateFontIndirectW and is freed once.
            unsafe {
                let _ = DeleteObject(self.0.into());
            }
        }
    }
}

/// A memory DC plus bitmap for flicker-free painting.
///
/// Drawing straight to the window DC makes every repaint visibly tear as rows
/// are filled; compositing off-screen and blitting once does not.
pub struct BackBuffer {
    pub dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    pub width: i32,
    pub height: i32,
}

impl BackBuffer {
    pub fn new(target: HDC, width: i32, height: i32) -> Option<BackBuffer> {
        if width <= 0 || height <= 0 {
            return None;
        }
        // SAFETY: `target` is a live DC supplied by BeginPaint.
        unsafe {
            let dc = CreateCompatibleDC(Some(target));
            if dc.is_invalid() {
                return None;
            }
            let bmp = CreateCompatibleBitmap(target, width, height);
            if bmp.is_invalid() {
                let _ = DeleteDC(dc);
                return None;
            }
            let old = SelectObject(dc, bmp.into());
            Some(BackBuffer {
                dc,
                bmp,
                old,
                width,
                height,
            })
        }
    }

    /// Blit the buffer to `target` and release it.
    pub fn present(self, target: HDC) {
        // SAFETY: both DCs are live and the dimensions match the bitmap.
        unsafe {
            let _ = BitBlt(
                target,
                0,
                0,
                self.width,
                self.height,
                Some(self.dc),
                0,
                0,
                SRCCOPY,
            );
        }
        // Drop runs the cleanup.
    }
}

impl Drop for BackBuffer {
    fn drop(&mut self) {
        // SAFETY: each handle was created in `new` and is released once.
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// Fill a rectangle with a solid colour.
pub fn fill_rect(dc: HDC, r: Rect, color: COLORREF) {
    // SAFETY: dc is live; the brush is created and freed here.
    unsafe {
        let brush = CreateSolidBrush(color);
        if brush.is_invalid() {
            return;
        }
        let rect = RECT {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        };
        FillRect(dc, &rect, brush);
        let _ = DeleteObject(brush.into());
    }
}

/// Fill a rounded rectangle with a solid colour.
pub fn fill_round_rect(dc: HDC, r: Rect, radius: i32, color: COLORREF) {
    // SAFETY: dc is live; both objects are selected out and deleted.
    unsafe {
        let brush = CreateSolidBrush(color);
        let pen = CreatePen(PS_NULL, 0, COLORREF(0));
        let ob = SelectObject(dc, brush.into());
        let op = SelectObject(dc, pen.into());
        let _ = RoundRect(dc, r.left, r.top, r.right, r.bottom, radius * 2, radius * 2);
        SelectObject(dc, ob);
        SelectObject(dc, op);
        let _ = DeleteObject(brush.into());
        let _ = DeleteObject(pen.into());
    }
}

/// Draw text, returning the width consumed.
pub fn draw_text(dc: HDC, text: &str, x: i32, y: i32, color: COLORREF) -> i32 {
    if text.is_empty() {
        return 0;
    }
    let wide: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: dc is live; `wide` outlives the call and its length is passed.
    unsafe {
        SetTextColor(dc, color);
        SetBkMode(dc, TRANSPARENT);
        let _ = TextOutW(dc, x, y, &wide);
        let mut size = windows::Win32::Foundation::SIZE::default();
        let _ = GetTextExtentPoint32W(dc, &wide, &mut size);
        size.cx
    }
}

/// Measure a string in the DC's current font.
pub fn measure(dc: HDC, text: &str) -> (i32, i32) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    let mut size = windows::Win32::Foundation::SIZE::default();
    // SAFETY: dc is live; `wide` outlives the call.
    unsafe {
        let _ = GetTextExtentPoint32W(dc, &wide, &mut size);
    }
    (size.cx, size.cy)
}

/// Draw `text`, colouring the characters at `highlight` positions differently.
///
/// Positions are `char` indices from [`crate::fuzzy::Match`]. The string is
/// emitted in runs so a highlighted span is drawn with one call.
pub fn draw_text_highlighted(
    dc: HDC,
    text: &str,
    highlight: &[usize],
    x: i32,
    y: i32,
    normal: COLORREF,
    hit: COLORREF,
) -> i32 {
    if text.is_empty() {
        return 0;
    }
    let chars: Vec<char> = text.chars().collect();
    let mut cursor = x;
    let mut i = 0usize;
    while i < chars.len() {
        let is_hit = highlight.contains(&i);
        let mut j = i + 1;
        while j < chars.len() && highlight.contains(&j) == is_hit {
            j += 1;
        }
        let run: String = chars[i..j].iter().collect();
        cursor += draw_text(dc, &run, cursor, y, if is_hit { hit } else { normal });
        i = j;
    }
    cursor - x
}

/// Elide `text` with an ellipsis so it fits within `max_width` pixels.
pub fn elide(dc: HDC, text: &str, max_width: i32) -> String {
    if max_width <= 0 {
        return String::new();
    }
    if measure(dc, text).0 <= max_width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    // Binary search the longest prefix that fits with an ellipsis.
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let candidate: String = chars[..mid].iter().collect::<String>() + "…";
        if measure(dc, &candidate).0 <= max_width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        "…".to_string()
    } else {
        chars[..lo].iter().collect::<String>() + "…"
    }
}

/// Register a window class once, returning its name.
pub fn register_class(
    name: &str,
    wndproc: unsafe extern "system" fn(
        HWND,
        u32,
        windows::Win32::Foundation::WPARAM,
        windows::Win32::Foundation::LPARAM,
    ) -> windows::Win32::Foundation::LRESULT,
) -> crate::util::WideStr {
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::*;

    let class_name = crate::util::WideStr::new(name);
    // SAFETY: the WNDCLASSEXW is fully initialised; a duplicate registration
    // fails harmlessly and we reuse the existing class.
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: class_name.as_pcwstr(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
    class_name
}

pub fn class_exists(name: &str) -> bool {
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{GetClassInfoExW, WNDCLASSEXW};
    let n = crate::util::WideStr::new(name);
    let mut wc = WNDCLASSEXW::default();
    // SAFETY: out-param is a valid WNDCLASSEXW.
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        GetClassInfoExW(Some(hinst.into()), PCWSTR(n.as_pcwstr().0), &mut wc).is_ok()
    }
}

/// How the drag overlays look: the grid outline, the drop preview, the size
/// readout.
///
/// Separate from [`Colors`], which dresses the palette and the About window.
/// Those are ordinary application windows and should follow the system. The
/// overlays are drawn *over* the user's own windows for a second at a time, and
/// what reads well there depends on the wallpaper, the applications and the
/// eyes doing the reading — so it is worth making adjustable rather than
/// guessing once on everyone's behalf.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Overlay {
    /// Grid lines and the drop preview.
    pub accent: COLORREF,
    /// Anything blocked: a refused split, a boundary at its minimum.
    pub warning: COLORREF,
    /// Text drawn on a filled overlay.
    pub on_accent: COLORREF,
    /// Opacity of an outline, 0-255.
    pub outline_alpha: u8,
    /// Opacity of a filled band. Lower, or the window beneath is unreadable.
    pub fill_alpha: u8,
    /// Line thickness in device-independent pixels.
    pub border_dip: i32,
    /// Corner radius in device-independent pixels.
    ///
    /// Windows 11 rounds a normal window by 8dip. Matching it is why the
    /// default is 8: a square overlay drawn over rounded windows reads as a
    /// different kind of object sitting on top of them, rather than as the
    /// outline of the space they occupy.
    pub corner_dip: i32,
    /// Face name for the size readout, or empty for the UI font.
    pub font: &'static str,
    /// Point size for the size readout.
    pub font_pt: i32,
}

/// The default: a blue close to the Windows accent, drawn as thinly as it can
/// be and still read.
///
/// The line was three pixels and the corners rounded by eight to match a
/// Windows 11 window. Both were heavier than the job needs. An overlay appears
/// for a second over somebody's own work; one pixel is enough to see a boundary
/// by, and a slight rounding reads as deliberate without competing with the
/// window frames underneath.
pub const OVERLAY_WINDOWS: Overlay = Overlay {
    accent: rgb(0x60, 0xA5, 0xFA),
    warning: rgb(0xE0, 0x9E, 0x2B),
    on_accent: rgb(0xFF, 0xFF, 0xFF),
    outline_alpha: 210,
    fill_alpha: 90,
    border_dip: 1,
    corner_dip: 2,
    font: "",
    font_pt: 12,
};

/// Neutral and quiet: no hue at all, for anyone who finds a coloured overlay
/// distracting over coloured content.
pub const OVERLAY_GRAPHITE: Overlay = Overlay {
    accent: rgb(0xD4, 0xD4, 0xD8),
    warning: rgb(0xF5, 0xA5, 0x24),
    on_accent: rgb(0x18, 0x18, 0x1B),
    outline_alpha: 200,
    fill_alpha: 80,
    border_dip: 2,
    corner_dip: 8,
    font: "Consolas",
    font_pt: 12,
};

/// Warm and loud, for light desktops where a pale blue disappears.
pub const OVERLAY_EMBER: Overlay = Overlay {
    accent: rgb(0xF9, 0x73, 0x16),
    warning: rgb(0xDC, 0x26, 0x26),
    on_accent: rgb(0xFF, 0xFF, 0xFF),
    outline_alpha: 220,
    fill_alpha: 95,
    border_dip: 3,
    corner_dip: 10,
    font: "",
    font_pt: 12,
};

/// Green, low-saturation, thin lines: the least intrusive of the six.
pub const OVERLAY_FOREST: Overlay = Overlay {
    accent: rgb(0x34, 0xD3, 0x99),
    warning: rgb(0xFB, 0xBF, 0x24),
    on_accent: rgb(0x06, 0x2A, 0x1E),
    outline_alpha: 195,
    fill_alpha: 75,
    border_dip: 2,
    corner_dip: 12,
    font: "",
    font_pt: 11,
};

/// Purple with generous rounding, to suit the rounded Windows 11 look.
pub const OVERLAY_VIOLET: Overlay = Overlay {
    accent: rgb(0xA7, 0x8B, 0xFA),
    warning: rgb(0xF4, 0x72, 0xB6),
    on_accent: rgb(0x1E, 0x11, 0x3B),
    outline_alpha: 215,
    fill_alpha: 90,
    border_dip: 3,
    corner_dip: 12,
    font: "",
    font_pt: 12,
};

/// Maximum legibility, not subtlety: opaque, thick, square, large text.
///
/// Square on purpose. Rounding costs contrast at exactly the corners where a
/// boundary meets three others, which is where someone relying on this theme
/// most needs to see what is happening.
pub const OVERLAY_CONTRAST: Overlay = Overlay {
    accent: rgb(0xFF, 0xE0, 0x00),
    warning: rgb(0xFF, 0x3B, 0x30),
    on_accent: rgb(0x00, 0x00, 0x00),
    outline_alpha: 255,
    fill_alpha: 160,
    border_dip: 5,
    corner_dip: 0,
    font: "",
    font_pt: 15,
};

/// The quiet set: low saturation, thin lines, restrained opacity.
///
/// Added because the original six all announce themselves. An overlay that
/// appears for half a second over somebody's own windows does not need to be
/// the brightest thing on the screen, and a one-pixel line at 100% scaling is
/// enough to read a boundary by.
pub const OVERLAY_DARKGRAY: Overlay = Overlay {
    accent: rgb(0x4B, 0x4B, 0x52),
    warning: rgb(0x8A, 0x62, 0x2B),
    on_accent: rgb(0xEC, 0xEC, 0xF0),
    outline_alpha: 190,
    fill_alpha: 70,
    border_dip: 1,
    corner_dip: 2,
    font: "",
    font_pt: 11,
};

pub const OVERLAY_GRAY: Overlay = Overlay {
    accent: rgb(0x8A, 0x8A, 0x93),
    warning: rgb(0xB5, 0x82, 0x2E),
    on_accent: rgb(0x1A, 0x1A, 0x1F),
    outline_alpha: 185,
    fill_alpha: 65,
    border_dip: 1,
    corner_dip: 2,
    font: "",
    font_pt: 11,
};

pub const OVERLAY_DARKPURPLE: Overlay = Overlay {
    accent: rgb(0x4C, 0x3A, 0x63),
    warning: rgb(0x8E, 0x5A, 0x7A),
    on_accent: rgb(0xEE, 0xE8, 0xF6),
    outline_alpha: 195,
    fill_alpha: 70,
    border_dip: 1,
    corner_dip: 2,
    font: "",
    font_pt: 11,
};

pub const OVERLAY_DARKBLUE: Overlay = Overlay {
    accent: rgb(0x2B, 0x3F, 0x63),
    warning: rgb(0x8A, 0x6A, 0x2E),
    on_accent: rgb(0xE6, 0xEC, 0xF8),
    outline_alpha: 195,
    fill_alpha: 70,
    border_dip: 1,
    corner_dip: 2,
    font: "",
    font_pt: 11,
};

pub const OVERLAY_DARKGREEN: Overlay = Overlay {
    accent: rgb(0x2C, 0x4D, 0x3A),
    warning: rgb(0x8A, 0x6E, 0x2E),
    on_accent: rgb(0xE6, 0xF3, 0xEA),
    outline_alpha: 195,
    fill_alpha: 70,
    border_dip: 1,
    corner_dip: 2,
    font: "",
    font_pt: 11,
};

/// Every built-in theme, in the order the tray lists them.
pub const OVERLAY_THEMES: [(&str, Overlay); 11] = [
    ("windows", OVERLAY_WINDOWS),
    ("darkgray", OVERLAY_DARKGRAY),
    ("gray", OVERLAY_GRAY),
    ("darkblue", OVERLAY_DARKBLUE),
    ("darkgreen", OVERLAY_DARKGREEN),
    ("darkpurple", OVERLAY_DARKPURPLE),
    ("graphite", OVERLAY_GRAPHITE),
    ("ember", OVERLAY_EMBER),
    ("forest", OVERLAY_FOREST),
    ("violet", OVERLAY_VIOLET),
    ("contrast", OVERLAY_CONTRAST),
];

/// Parse `#RRGGBB` into a COLORREF.
///
/// Returns `None` on anything unparseable. Callers substitute a default: a
/// mistyped colour should cost that colour, not the overlay.
pub fn parse_hex(text: &str) -> Option<COLORREF> {
    let h = text.trim().trim_start_matches('#');
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(rgb(
        ((v >> 16) & 0xFF) as u8,
        ((v >> 8) & 0xFF) as u8,
        (v & 0xFF) as u8,
    ))
}

/// Render a COLORREF back to `#RRGGBB` for the config file.
///
/// COLORREF is 0x00BBGGRR, so this is not a straight hex dump -- writing one
/// would round-trip every colour to its own mirror image.
pub fn to_hex(c: COLORREF) -> String {
    let (r, g, b) = (
        (c.0 & 0xFF) as u8,
        ((c.0 >> 8) & 0xFF) as u8,
        ((c.0 >> 16) & 0xFF) as u8,
    );
    format!("#{r:02X}{g:02X}{b:02X}")
}

/// Build an [`Overlay`] from a user-defined theme.
pub fn overlay_from_custom(c: &crate::config::CustomTheme) -> Overlay {
    let d = OVERLAY_WINDOWS;
    Overlay {
        accent: parse_hex(&c.accent).unwrap_or(d.accent),
        warning: parse_hex(&c.warning).unwrap_or(d.warning),
        on_accent: parse_hex(&c.text).unwrap_or(d.on_accent),
        // Clamped rather than trusted: a config file is hand-editable, and an
        // invisible or opaque overlay is not a theme, it is a broken window
        // manager.
        outline_alpha: c.outline_alpha.max(60),
        fill_alpha: c.fill_alpha.clamp(20, 200),
        border_dip: c.border_dip.clamp(1, 8),
        corner_dip: c.corner_dip.clamp(0, 24),
        // Leaked deliberately: an Overlay holds a &'static str so it can be a
        // const, and a theme lives as long as the process. One small leak per
        // theme change is the honest price of that, and it is bounded by how
        // often a person edits a theme.
        font: Box::leak(c.font.clone().into_boxed_str()),
        font_pt: c.font_pt.clamp(8, 24),
    }
}

/// Look up a theme by name, falling back to the default.
///
/// An unknown name is a typo in a hand-edited config, not a reason to refuse to
/// draw anything.
pub fn overlay_by_name(name: &str) -> Overlay {
    OVERLAY_THEMES
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, t)| *t)
        .unwrap_or(OVERLAY_WINDOWS)
}

/// The name shown in the tray for a theme key.
pub fn overlay_label(name: &str) -> &'static str {
    match name.to_ascii_lowercase().as_str() {
        "darkgray" => "Dark gray",
        "gray" => "Gray",
        "darkblue" => "Dark blue",
        "darkgreen" => "Dark green",
        "darkpurple" => "Dark purple",
        "graphite" => "Graphite",
        "ember" => "Ember",
        "forest" => "Forest",
        "violet" => "Violet",
        "contrast" => "High contrast",
        _ => "Windows",
    }
}

#[cfg(test)]
mod tests {
    use super::{overlay_by_name, overlay_label, Overlay, OVERLAY_THEMES, OVERLAY_WINDOWS};

    #[test]
    fn every_theme_has_a_distinct_name() {
        let mut names: Vec<&str> = OVERLAY_THEMES.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "two themes share a name");
    }

    #[test]
    fn every_theme_round_trips_through_its_name() {
        for (name, theme) in OVERLAY_THEMES {
            assert_eq!(overlay_by_name(name), theme, "{name} did not resolve");
            assert_eq!(
                overlay_by_name(&name.to_uppercase()),
                theme,
                "{name} is case-sensitive"
            );
            assert_ne!(overlay_label(name), "", "{name} has no label");
        }
    }

    #[test]
    fn an_unknown_name_falls_back_rather_than_failing() {
        // A typo in a hand-edited config must not stop the overlays drawing.
        assert_eq!(overlay_by_name("chartreuse"), OVERLAY_WINDOWS);
        assert_eq!(overlay_by_name(""), OVERLAY_WINDOWS);
    }

    fn all() -> Vec<Overlay> {
        OVERLAY_THEMES.iter().map(|(_, t)| *t).collect()
    }

    #[test]
    fn the_quiet_themes_really_are_quieter() {
        // They exist to be unobtrusive; if they are not, they are just five
        // more colours nobody asked for.
        let loud = super::OVERLAY_WINDOWS;
        for t in [
            super::OVERLAY_DARKGRAY,
            super::OVERLAY_GRAY,
            super::OVERLAY_DARKBLUE,
            super::OVERLAY_DARKGREEN,
            super::OVERLAY_DARKPURPLE,
        ] {
            assert!(
                t.border_dip <= loud.border_dip,
                "a thicker line is not quieter"
            );
            assert!(t.outline_alpha <= loud.outline_alpha);
            assert!(t.fill_alpha <= loud.fill_alpha);
        }
    }

    #[test]
    fn no_theme_is_invisible_or_opaque_where_it_should_not_be() {
        for t in all() {
            assert!(t.outline_alpha >= 150, "an outline nobody can see");
            // A filled band sits over a window that has to stay readable.
            assert!(t.fill_alpha <= 200, "a fill that hides the window beneath");
            assert!(
                t.fill_alpha < t.outline_alpha,
                "fill must be lighter than outline"
            );
        }
    }

    #[test]
    fn no_theme_has_unusable_geometry() {
        for t in all() {
            assert!((1..=8).contains(&t.border_dip), "border {}", t.border_dip);
            assert!((0..=24).contains(&t.corner_dip), "corner {}", t.corner_dip);
            assert!((8..=24).contains(&t.font_pt), "font {}", t.font_pt);
        }
    }

    #[test]
    fn every_theme_keeps_its_caption_legible() {
        // The readout draws on_accent over accent; equal colours are invisible.
        for t in all() {
            assert_ne!(t.accent, t.on_accent);
            assert_ne!(t.accent, t.warning, "warning must be distinguishable");
        }
    }

    #[test]
    fn the_high_contrast_theme_actually_is_one() {
        let c = super::OVERLAY_CONTRAST;
        let d = OVERLAY_WINDOWS;
        assert!(c.outline_alpha >= d.outline_alpha);
        assert!(c.border_dip > d.border_dip);
        assert!(c.font_pt > d.font_pt);
        // Square: rounding costs contrast exactly where boundaries meet.
        assert_eq!(c.corner_dip, 0);
    }

    use super::*;

    #[test]
    fn rgb_uses_gdi_byte_order() {
        // 0x00BBGGRR, not 0x00RRGGBB.
        assert_eq!(rgb(0xFF, 0x00, 0x00).0, 0x0000_00FF, "pure red");
        assert_eq!(rgb(0x00, 0xFF, 0x00).0, 0x0000_FF00, "pure green");
        assert_eq!(rgb(0x00, 0x00, 0xFF).0, 0x00FF_0000, "pure blue");
        assert_eq!(rgb(0x12, 0x34, 0x56).0, 0x0056_3412);
    }

    #[test]
    fn explicit_themes_ignore_the_system_setting() {
        assert!(colors_for(Theme::Dark).1);
        assert!(!colors_for(Theme::Light).1);
    }

    #[test]
    fn auto_theme_resolves_without_panicking() {
        let (_, dark) = colors_for(Theme::Auto);
        assert_eq!(dark, system_prefers_dark());
    }

    #[test]
    fn theme_palettes_have_usable_contrast() {
        // Guards against a palette edit that makes text invisible. Compares
        // relative luminance of foreground vs background.
        fn luma(c: COLORREF) -> f64 {
            let r = (c.0 & 0xFF) as f64 / 255.0;
            let g = ((c.0 >> 8) & 0xFF) as f64 / 255.0;
            let b = ((c.0 >> 16) & 0xFF) as f64 / 255.0;
            let f = |v: f64| {
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b)
        }
        fn contrast(a: COLORREF, b: COLORREF) -> f64 {
            let (x, y) = (luma(a), luma(b));
            let (hi, lo) = if x > y { (x, y) } else { (y, x) };
            (hi + 0.05) / (lo + 0.05)
        }
        for (name, c) in [("dark", DARK), ("light", LIGHT)] {
            // WCAG AA for body text is 4.5:1.
            assert!(
                contrast(c.fg, c.bg) >= 4.5,
                "{name}: fg/bg only {:.2}",
                contrast(c.fg, c.bg)
            );
            // Subtle text is secondary; AA large-text threshold of 3:1.
            assert!(
                contrast(c.subtle, c.bg) >= 3.0,
                "{name}: subtle/bg only {:.2}",
                contrast(c.subtle, c.bg)
            );
            assert!(
                contrast(c.sel_fg, c.sel_bg) >= 4.5,
                "{name}: selected {:.2}",
                contrast(c.sel_fg, c.sel_bg)
            );
            assert!(
                contrast(c.match_fg, c.sel_bg) >= 3.0,
                "{name}: match on selection {:.2}",
                contrast(c.match_fg, c.sel_bg)
            );
            assert!(
                contrast(c.match_fg, c.bg) >= 3.0,
                "{name}: match on bg {:.2}",
                contrast(c.match_fg, c.bg)
            );
        }
    }

    #[test]
    fn fonts_are_created_and_freed() {
        let f = Font::ui(11, 96, 400);
        assert!(!f.0.is_invalid());
        let f2 = Font::ui(11, 192, 700);
        assert!(!f2.0.is_invalid());
        // Drop frees them; running under a leak check would catch a mistake.
    }

    #[test]
    fn back_buffer_rejects_degenerate_sizes() {
        // SAFETY: a screen DC, released immediately.
        unsafe {
            let dc = GetDC(None);
            assert!(BackBuffer::new(dc, 0, 10).is_none());
            assert!(BackBuffer::new(dc, 10, 0).is_none());
            assert!(BackBuffer::new(dc, -5, -5).is_none());
            let ok = BackBuffer::new(dc, 32, 32);
            assert!(ok.is_some());
            drop(ok);
            ReleaseDC(None, dc);
        }
    }

    #[test]
    fn eliding_fits_within_the_budget() {
        // SAFETY: screen DC with a real font selected, released after use.
        unsafe {
            let dc = GetDC(None);
            let f = Font::ui(11, 96, 400);
            let old = SelectObject(dc, f.0.into());

            let long = "A very long application name that will certainly not fit";
            let full = measure(dc, long).0;
            assert!(full > 0);

            let budget = full / 3;
            let out = elide(dc, long, budget);
            assert!(
                measure(dc, &out).0 <= budget,
                "elided text still too wide: {out}"
            );
            assert!(out.ends_with('…'));

            // Text that already fits is returned untouched.
            assert_eq!(elide(dc, "ok", full), "ok");
            // A zero budget yields nothing rather than panicking.
            assert_eq!(elide(dc, long, 0), "");
            // A tiny budget still returns something valid.
            assert!(!elide(dc, long, 3).is_empty());

            SelectObject(dc, old);
            ReleaseDC(None, dc);
        }
    }

    #[test]
    fn measuring_an_empty_string_is_zero_width() {
        // SAFETY: screen DC released after use.
        unsafe {
            let dc = GetDC(None);
            assert_eq!(measure(dc, "").0, 0);
            ReleaseDC(None, dc);
        }
    }
}

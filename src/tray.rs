//! System tray icon and its context menu.
//!
//! Menu glyphs are rendered at runtime from **Segoe Fluent Icons** into 32-bit
//! premultiplied-alpha DIBs rather than shipped as bitmaps. That keeps the
//! binary free of raster assets and makes every glyph crisp at any DPI, since
//! it is rasterised at the exact pixel size the menu asks for.
//!
//! GDI text drawing does not write an alpha channel, so [`glyph_bitmap`] draws
//! white-on-black and derives alpha from luminance before premultiplying.

use std::collections::HashMap;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::hotkeys::Action;
use crate::layout::LayoutKind;
use crate::ui::tip::TipText;
use crate::util::fill_wide_buf;

/// Message the tray icon posts to our window. `WM_APP` range is reserved for
/// application use, so it cannot collide with a system message.
pub const WM_TRAY_CALLBACK: u32 = WM_APP + 1;
/// Our tray icon's identifier, unique within this window.
const TRAY_ICON_ID: u32 = 1;

/// Resource ids compiled in by `build.rs`.
const IDI_MAIN: u16 = 1;
const IDI_PAUSED: u16 = 2;

// --- Menu command identifiers ----------------------------------------------
// Explicit values, because these travel through Win32 as bare u32s.
const ID_RETILE: u32 = 100;
const ID_PAUSE: u32 = 101;
const ID_SETTINGS: u32 = 102;
const ID_ABOUT: u32 = 103;
const ID_EXIT: u32 = 104;
const ID_PALETTE: u32 = 105;
const ID_LAYOUT_BASE: u32 = 200; // + index into LayoutKind::ALL
const ID_GAP_INC: u32 = 300;
const ID_GAP_DEC: u32 = 301;
const ID_MASTER_GROW: u32 = 302;
const ID_MASTER_SHRINK: u32 = 303;
const ID_SIZE_RESET: u32 = 304;
const ID_CLEAR_EXCLUSIONS: u32 = 305;
const ID_HOTKEY_CONFLICTS: u32 = 306;
const ID_START_WITH_WINDOWS: u32 = 307;
const ID_AUTO_TILE: u32 = 308;
const ID_SIZE_READOUT: u32 = 320;
const ID_RESIZE_GRID: u32 = 321;
const ID_LOGGING: u32 = 322;
const ID_VERBOSE_LOGGING: u32 = 323;
const ID_OPEN_LOG_FOLDER: u32 = 324;
const ID_ISSUE_REPORT: u32 = 325;
const ID_RESERVE_ELEVATED: u32 = 326;
const ID_CHECK_UPDATES: u32 = 327;
const ID_AUTO_UPDATE_CHECK: u32 = 328;
/// One id per built-in theme, plus one for the user's own.
const ID_THEME_BASE: u32 = 700;
const ID_THEME_CUSTOM: u32 = 720;
const ID_THEME_ACCENT: u32 = 721;
const ID_THEME_WARNING: u32 = 722;
const ID_THEME_TEXT: u32 = 723;
const ID_THEME_RENAME: u32 = 724;
const ID_THEME_EDITOR: u32 = 725;
const ID_EDIT_CONFIG: u32 = 309;
const ID_SHOW_HOTKEYS: u32 = 310;
const ID_CLAUDE_SOURCE: u32 = 311;

// --- dimming ---------------------------------------------------------------
const ID_DIM_TOGGLE: u32 = 400;
const ID_DIM_AUTOTRACK: u32 = 401;
const ID_DIM_PIN: u32 = 402;
/// Base of the window-level block; + index into [`DIM_WINDOW_LEVELS`].
const ID_DIM_WINDOW_BASE: u32 = 410;
/// Base of the taskbar-level block; + index into [`DIM_TASKBAR_LEVELS`].
const ID_DIM_TASKBAR_BASE: u32 = 430;

/// Offered levels for everything except the focused window.
pub const DIM_WINDOW_LEVELS: [u8; 8] = [40, 55, 70, 80, 85, 90, 95, 100];
/// Offered levels for the taskbar and Start menu. Starts at 0 because leaving
/// the taskbar untouched is a perfectly reasonable choice.
pub const DIM_TASKBAR_LEVELS: [u8; 8] = [0, 20, 30, 40, 50, 65, 80, 95];

/// Base of the per-window command block.
///
/// Each listed window owns [`WINDOW_STRIDE`] consecutive ids, one per action,
/// so an id decodes to (window index, action) by division and remainder. The
/// block starts well above every fixed id and is bounded by [`MAX_WINDOWS`].
const ID_WINDOW_BASE: u32 = 1000;
const WINDOW_STRIDE: u32 = 4;
/// Cap on listed windows. A tray menu with 200 entries is unusable, and the id
/// block must stay bounded.
pub const MAX_WINDOWS: usize = 40;

/// Which colour of the custom theme a picker is being opened for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeColour {
    /// Grid lines and the drop preview.
    Accent,
    /// Anything blocked.
    Warning,
    /// The size readout's text.
    Text,
}

/// What the user picked from the tray menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuCommand {
    OpenPalette,
    Retile,
    SetLayout(LayoutKind),
    GapIncrease,
    GapDecrease,
    MasterGrow,
    MasterShrink,
    SizeReset,
    TogglePause,
    Settings,
    About,
    Exit,
    /// Bring the nth listed window to the foreground.
    FocusWindow(usize),
    /// Toggle whether the nth listed window takes part in tiling.
    ToggleExclude(usize),
    /// Toggle always-on-top for the nth listed window.
    ToggleTopmost(usize),
    /// Re-include every manually excluded window.
    ClearExclusions,
    /// Explain which hotkeys could not be registered.
    ShowHotkeyConflicts,
    /// Register or unregister the HKCU Run value.
    ToggleStartWithWindows,
    /// Turn automatic retiling on window events on or off.
    ToggleAutoTile,
    /// Show or hide the live pixel size during a resize.
    ToggleSizeReadout,
    /// Show or hide the grid outline during a resize.
    ToggleResizeGrid,
    /// Start or stop writing the debug log.
    ToggleLogging,
    /// Include the high-frequency detail in the debug log.
    ToggleVerboseLogging,
    /// Reveal the folder holding the log and the config.
    OpenLogFolder,
    /// Assemble an anonymised issue report and put it on the clipboard.
    CreateIssueReport,
    /// Keep a cell for elevated windows that can never occupy one.
    ToggleReserveElevated,
    /// Ask GitHub whether a newer release exists, now.
    CheckForUpdates,
    /// Check for a newer release in the background.
    ToggleAutoUpdateCheck,
    /// Adopt the built-in overlay theme at this index.
    UseTheme(usize),
    /// Adopt the user's own theme.
    UseCustomTheme,
    /// Open the system colour chooser for one part of the custom theme.
    PickThemeColour(ThemeColour),
    /// Rename the custom theme.
    RenameCustomTheme,
    /// Open the theme editor window.
    OpenThemeEditor,
    /// Open config.json in the default editor.
    EditConfigFile,
    /// Open the keyboard shortcut list and editor.
    ShowHotkeyEditor,
    /// Offer "Ask Claude" in the palette, or stop offering it.
    ToggleClaudeSource,
    /// Turn focus dimming on or off.
    ToggleDimming,
    /// Follow the foreground window, or hold the pinned one.
    ToggleDimAutoTrack,
    /// Keep the currently focused window bright regardless of focus.
    PinDimWindow,
    SetDimWindowLevel(u8),
    SetDimTaskbarLevel(u8),
}

/// Actions available on a listed window, in menu order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAction {
    Focus = 0,
    Exclude = 1,
    Topmost = 2,
}

impl MenuCommand {
    fn from_id(id: u32) -> Option<MenuCommand> {
        Some(match id {
            ID_PALETTE => MenuCommand::OpenPalette,
            ID_RETILE => MenuCommand::Retile,
            ID_PAUSE => MenuCommand::TogglePause,
            ID_SETTINGS => MenuCommand::Settings,
            ID_ABOUT => MenuCommand::About,
            ID_EXIT => MenuCommand::Exit,
            ID_GAP_INC => MenuCommand::GapIncrease,
            ID_GAP_DEC => MenuCommand::GapDecrease,
            ID_MASTER_GROW => MenuCommand::MasterGrow,
            ID_MASTER_SHRINK => MenuCommand::MasterShrink,
            ID_SIZE_RESET => MenuCommand::SizeReset,
            ID_CLEAR_EXCLUSIONS => MenuCommand::ClearExclusions,
            ID_HOTKEY_CONFLICTS => MenuCommand::ShowHotkeyConflicts,
            ID_START_WITH_WINDOWS => MenuCommand::ToggleStartWithWindows,
            ID_AUTO_TILE => MenuCommand::ToggleAutoTile,
            ID_SIZE_READOUT => MenuCommand::ToggleSizeReadout,
            ID_RESIZE_GRID => MenuCommand::ToggleResizeGrid,
            ID_LOGGING => MenuCommand::ToggleLogging,
            ID_VERBOSE_LOGGING => MenuCommand::ToggleVerboseLogging,
            ID_OPEN_LOG_FOLDER => MenuCommand::OpenLogFolder,
            ID_ISSUE_REPORT => MenuCommand::CreateIssueReport,
            ID_RESERVE_ELEVATED => MenuCommand::ToggleReserveElevated,
            ID_CHECK_UPDATES => MenuCommand::CheckForUpdates,
            ID_AUTO_UPDATE_CHECK => MenuCommand::ToggleAutoUpdateCheck,
            ID_THEME_CUSTOM => MenuCommand::UseCustomTheme,
            ID_THEME_ACCENT => MenuCommand::PickThemeColour(ThemeColour::Accent),
            ID_THEME_WARNING => MenuCommand::PickThemeColour(ThemeColour::Warning),
            ID_THEME_TEXT => MenuCommand::PickThemeColour(ThemeColour::Text),
            ID_THEME_RENAME => MenuCommand::RenameCustomTheme,
            ID_THEME_EDITOR => MenuCommand::OpenThemeEditor,
            id if (ID_THEME_BASE..ID_THEME_BASE + 16).contains(&id) => {
                MenuCommand::UseTheme((id - ID_THEME_BASE) as usize)
            }
            ID_EDIT_CONFIG => MenuCommand::EditConfigFile,
            ID_SHOW_HOTKEYS => MenuCommand::ShowHotkeyEditor,
            ID_CLAUDE_SOURCE => MenuCommand::ToggleClaudeSource,
            ID_DIM_TOGGLE => MenuCommand::ToggleDimming,
            ID_DIM_AUTOTRACK => MenuCommand::ToggleDimAutoTrack,
            ID_DIM_PIN => MenuCommand::PinDimWindow,
            other
                if (ID_DIM_WINDOW_BASE..ID_DIM_WINDOW_BASE + DIM_WINDOW_LEVELS.len() as u32)
                    .contains(&other) =>
            {
                MenuCommand::SetDimWindowLevel(
                    DIM_WINDOW_LEVELS[(other - ID_DIM_WINDOW_BASE) as usize],
                )
            }
            other
                if (ID_DIM_TASKBAR_BASE..ID_DIM_TASKBAR_BASE + DIM_TASKBAR_LEVELS.len() as u32)
                    .contains(&other) =>
            {
                MenuCommand::SetDimTaskbarLevel(
                    DIM_TASKBAR_LEVELS[(other - ID_DIM_TASKBAR_BASE) as usize],
                )
            }
            other if other >= ID_WINDOW_BASE => {
                let offset = other - ID_WINDOW_BASE;
                let index = (offset / WINDOW_STRIDE) as usize;
                if index >= MAX_WINDOWS {
                    return None;
                }
                match offset % WINDOW_STRIDE {
                    0 => MenuCommand::FocusWindow(index),
                    1 => MenuCommand::ToggleExclude(index),
                    2 => MenuCommand::ToggleTopmost(index),
                    _ => return None,
                }
            }
            other => {
                let i = other.checked_sub(ID_LAYOUT_BASE)? as usize;
                MenuCommand::SetLayout(*LayoutKind::ALL.get(i)?)
            }
        })
    }
}

/// Menu id for one action on the nth listed window.
pub fn window_command_id(index: usize, action: WindowAction) -> u32 {
    ID_WINDOW_BASE + (index as u32) * WINDOW_STRIDE + action as u32
}

/// Which listed window a menu id refers to, if any.
pub fn window_index_of_id(id: u32) -> Option<usize> {
    if id < ID_WINDOW_BASE {
        return None;
    }
    let index = ((id - ID_WINDOW_BASE) / WINDOW_STRIDE) as usize;
    (index < MAX_WINDOWS).then_some(index)
}

/// How one action's hotkey actually turned out.
///
/// Built by the application after registration, so the menu can show the key
/// that *works* rather than the one that was configured.
#[derive(Debug, Clone)]
pub struct BindingInfo {
    pub action: Action,
    /// The combination in use, or empty when the action has no working key.
    pub keys: String,
    /// The first choice, when it was unavailable and a fallback was taken.
    pub wanted: String,
    /// The i3 binding this derives from, if any.
    pub i3: String,
}

impl BindingInfo {
    /// Text for the menu's right-hand accelerator column.
    fn accel(&self) -> String {
        if self.keys.is_empty() {
            "unavailable".to_string()
        } else {
            self.keys.clone()
        }
    }

    /// The hover tooltip: what the key is, and why it is not what you expected.
    fn tip(&self) -> TipText {
        let keys = if self.keys.is_empty() {
            "No shortcut \u{2014} every candidate is taken".to_string()
        } else if self.wanted.is_empty() {
            self.keys.clone()
        } else {
            format!(
                "{}   (fallback \u{2014} {} was taken)",
                self.keys, self.wanted
            )
        };
        let detail = if self.i3.is_empty() {
            "No i3 equivalent".to_string()
        } else {
            format!("i3: {}", self.i3)
        };
        TipText {
            title: self.action.label().to_string(),
            keys,
            detail,
        }
    }
}

/// One row in the tray's window list.
#[derive(Debug, Clone)]
pub struct WindowEntry {
    pub hwnd: isize,
    pub title: String,
    /// Executable stem, shown to disambiguate identical titles.
    pub app: String,
    /// Currently held out of the tiling by the user.
    pub excluded: bool,
    /// Currently pinned above other windows.
    pub topmost: bool,
}

/// State the menu needs in order to render check marks and labels.
pub struct MenuState {
    pub paused: bool,
    pub layout: LayoutKind,
    pub outer_gap: i32,
    pub inner_gap: i32,
    pub master_percent: u32,
    pub palette_binding: String,
    /// Visible, non-minimised windows, already capped at [`MAX_WINDOWS`].
    pub windows: Vec<WindowEntry>,
    /// How many windows the user has manually excluded.
    pub excluded_count: usize,
    /// Actions with no working shortcut at all.
    pub hotkey_conflicts: usize,
    /// Actions that resolved to a fallback. These *work*, so they are reported
    /// separately -- calling them "unavailable" made a non-problem look like a
    /// failure on every PowerToys machine.
    pub hotkey_moved: usize,
    /// How every action's hotkey resolved, for accelerators and tooltips.
    pub bindings: Vec<BindingInfo>,
    /// Focus dimming state, mirrored into the Dimming submenu.
    pub dim: crate::dimmer::DimConfig,
    /// True when a specific window is pinned bright.
    pub dim_pinned: bool,
    /// Whether the HKCU Run value is currently present.
    pub start_with_windows: bool,
    /// Whether window events trigger an automatic retile.
    pub auto_tile: bool,
    /// Whether the live pixel size is shown while resizing.
    pub size_readout: bool,
    /// Whether the grid is outlined while resizing.
    pub resize_grid: bool,
    /// Whether the debug log is being written.
    pub logging: bool,
    /// Whether elevated windows keep a cell they cannot fill.
    pub reserve_elevated: bool,
    /// Whether updates are checked in the background.
    pub auto_update_check: bool,
    /// Whether the debug log includes the high-frequency detail.
    pub verbose_logging: bool,
    /// Key of the overlay theme in use.
    pub overlay_theme: String,
    /// The custom theme's name, for the menu.
    pub custom_theme_name: String,
    /// Whether that source is currently switched on.
    pub claude_enabled: bool,
}

// --- Segoe Fluent Icons code points ----------------------------------------
// Present on Windows 11; Segoe MDL2 Assets shares these code points on Win10.
const GLYPH_RETILE: char = '\u{E72C}'; // Refresh
const GLYPH_LAYOUT: char = '\u{E80A}'; // GripperBarHorizontal-ish / tiles
const GLYPH_RESIZE: char = '\u{E740}'; // FullScreen
const GLYPH_PAUSE: char = '\u{E769}';
const GLYPH_PLAY: char = '\u{E768}';
const GLYPH_SETTINGS: char = '\u{E713}';
const GLYPH_ABOUT: char = '\u{E946}'; // Info
const GLYPH_EXIT: char = '\u{E7E8}'; // PowerButton
const GLYPH_SEARCH: char = '\u{E721}';
const GLYPH_PLUS: char = '\u{E710}';
const GLYPH_MINUS: char = '\u{E738}';
const GLYPH_RESET: char = '\u{E7A7}'; // Undo
const GLYPH_WINDOWS: char = '\u{E737}'; // Window
const GLYPH_WARN: char = '\u{E7BA}'; // Warning
const GLYPH_DIM: char = '\u{E706}'; // Brightness
const GLYPH_TARGET: char = '\u{E7C5}'; // Target / select
const GLYPH_KEYBOARD: char = '\u{E765}'; // Keyboard
const GLYPH_EDIT: char = '\u{E70F}'; // Edit
const GLYPH_FOLDER: char = '\u{E838}'; // FolderOpen
const GLYPH_FOCUS: char = '\u{E7C4}'; // Bring forward
const GLYPH_EXCLUDE: char = '\u{E711}'; // Cancel
const GLYPH_PIN: char = '\u{E718}'; // Pin

/// Owns the tray icon and the bitmaps its menu uses.
pub struct Tray {
    hwnd: HWND,
    icon_normal: HICON,
    icon_paused: HICON,
    added: bool,
    /// Broadcast sent when Explorer restarts; without handling it the icon
    /// silently disappears and never comes back.
    taskbar_created: u32,
    /// Menu glyph bitmaps, cached by character.
    ///
    /// Previously a plain `Vec` appended to on every open and freed only when
    /// the menu metric changed — which on a fixed-DPI machine is once, ever.
    /// Each open allocated ~30 more DIBs, so around 175 opens exhausted the
    /// 10,000-handle GDI quota and every other allocation in the process
    /// started failing, taking the palette and About windows down with it.
    /// Caching by character makes a menu open allocate nothing after the
    /// first, and also stops the same glyph being rasterised several times
    /// within one menu.
    bitmaps: HashMap<char, HBITMAP>,
    glyph_px: i32,
}

impl Tray {
    pub fn new(hwnd: HWND) -> Tray {
        // SAFETY: all three take no pointer arguments and report failure
        // by value; the metrics are floored at 16 in case the shell
        // reports something unusable.
        let hinst = unsafe { GetModuleHandleW(None) }.unwrap_or_default();
        let cx = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
        let cy = unsafe { GetSystemMetrics(SM_CYSMICON) }.max(16);

        let load = |id: u16| -> HICON {
            // SAFETY: MAKEINTRESOURCE-style PCWSTR built from a resource id
            // compiled in by build.rs. A failure yields a null HICON, which
            // Shell_NotifyIconW tolerates (it draws a blank icon).
            unsafe {
                LoadImageW(
                    Some(hinst.into()),
                    PCWSTR(id as usize as *const u16),
                    IMAGE_ICON,
                    cx,
                    cy,
                    LR_DEFAULTCOLOR,
                )
                .map(|h| HICON(h.0))
                .unwrap_or_default()
            }
        };

        // SAFETY: registering a window message by name; always succeeds or
        // returns 0, which we treat as "never matches".
        let taskbar_created = unsafe {
            let name = crate::util::WideStr::new("TaskbarCreated");
            RegisterWindowMessageW(name.as_pcwstr())
        };

        Tray {
            hwnd,
            icon_normal: load(IDI_MAIN),
            icon_paused: load(IDI_PAUSED),
            added: false,
            taskbar_created,
            bitmaps: HashMap::new(),
            glyph_px: 0,
        }
    }

    fn notify_data(&self, paused: bool, flags: u32) -> NOTIFYICONDATAW {
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_ICON_ID,
            uFlags: windows::Win32::UI::Shell::NOTIFY_ICON_DATA_FLAGS(flags),
            uCallbackMessage: WM_TRAY_CALLBACK,
            hIcon: if paused {
                self.icon_paused
            } else {
                self.icon_normal
            },
            ..Default::default()
        };
        let tip = if paused {
            "SuperTile — paused"
        } else {
            "SuperTile — tiling active"
        };
        fill_wide_buf(&mut nid.szTip, tip);
        nid
    }

    /// Show a balloon notification from the tray icon.
    ///
    /// Reserved for things that change state without the user having asked in
    /// so many words. A silent mode switch reads as a malfunction; the same
    /// switch announced reads as a feature.
    pub fn balloon(&self, title: &str, body: &str) {
        if !self.added {
            return;
        }
        let mut nid = self.notify_data(false, NIF_INFO.0);
        fill_wide_buf(&mut nid.szInfoTitle, title);
        fill_wide_buf(&mut nid.szInfo, body);
        nid.dwInfoFlags = NIIF_INFO;
        // SAFETY: nid is fully initialised with a correct cbSize; NIM_MODIFY
        // with NIF_INFO only reads the info fields.
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    /// Add (or re-add) the icon. Safe to call repeatedly.
    pub fn add(&mut self, paused: bool) -> bool {
        let nid = self.notify_data(
            paused,
            NIF_ICON.0 | NIF_MESSAGE.0 | NIF_TIP.0 | NIF_SHOWTIP.0,
        );
        // SAFETY: nid is fully initialised with a correct cbSize.
        let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) }.as_bool();
        if ok {
            // Opt into version-4 semantics: reliable WM_CONTEXTMENU delivery
            // and screen coordinates packed into lParam.
            let mut v = nid;
            v.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            // SAFETY: same struct, NIM_SETVERSION only reads uVersion.
            unsafe {
                let _ = Shell_NotifyIconW(NIM_SETVERSION, &v);
            }
            self.added = true;
        }
        ok
    }

    /// Swap the icon and tooltip to reflect paused state.
    pub fn set_paused(&mut self, paused: bool) {
        if !self.added {
            return;
        }
        let nid = self.notify_data(paused, NIF_ICON.0 | NIF_TIP.0 | NIF_SHOWTIP.0);
        // SAFETY: as above.
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    /// Does this message mean Explorer restarted and we must re-add the icon?
    pub fn is_taskbar_created(&self, msg: u32) -> bool {
        self.taskbar_created != 0 && msg == self.taskbar_created
    }

    pub fn remove(&mut self) {
        if !self.added {
            return;
        }
        let nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_ICON_ID,
            ..Default::default()
        };
        // SAFETY: NIM_DELETE reads only hWnd and uID.
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
        }
        self.added = false;
    }

    /// Build and display the context menu, returning the chosen command.
    ///
    /// `pt` is in screen coordinates, as delivered by the tray callback.
    pub fn show_menu(&mut self, state: &MenuState, pt: POINT) -> Option<MenuCommand> {
        self.ensure_glyphs();
        // Hover highlighting reads this from thread-local storage, not from
        // App, because the modal menu loop re-enters our window procedure.
        crate::ui::hover::begin_menu();

        // SAFETY: every handle below is checked; menus are destroyed on exit.
        unsafe {
            let menu = CreatePopupMenu().ok()?;
            let layout_menu = CreatePopupMenu().ok()?;
            let resize_menu = CreatePopupMenu().ok()?;

            for (i, kind) in LayoutKind::ALL.iter().enumerate() {
                let checked = if *kind == state.layout {
                    MF_CHECKED
                } else {
                    MF_UNCHECKED
                };
                let id = ID_LAYOUT_BASE + i as u32;
                // Some layouts have a direct key; show it in the accelerator
                // column exactly as the top-level items do.
                let direct = match kind {
                    LayoutKind::Columns => Some(Action::LayoutColumns),
                    LayoutKind::Rows => Some(Action::LayoutRows),
                    LayoutKind::Monocle => Some(Action::LayoutStacking),
                    _ => None,
                };
                let keys = direct
                    .and_then(|a| state.bindings.iter().find(|b| b.action == a))
                    .map(|b| b.accel())
                    .unwrap_or_default();
                let text = if keys.is_empty() {
                    kind.label().to_string()
                } else {
                    format!("{}\t{keys}", kind.label())
                };
                let label = crate::util::WideStr::new(&text);
                let _ = AppendMenuW(
                    layout_menu,
                    MF_STRING | checked,
                    id as usize,
                    label.as_pcwstr(),
                );
                match direct.and_then(|a| state.bindings.iter().find(|b| b.action == a)) {
                    Some(info) => crate::ui::hover::map_tip(id, info.tip()),
                    None => self.describe(
                        id,
                        kind.label(),
                        "No direct shortcut; reachable with Cycle layout or the palette",
                    ),
                }
            }

            let gap_label = crate::util::WideStr::new(&format!(
                "Gaps: {} px outer, {} px inner",
                state.outer_gap, state.inner_gap
            ));
            let _ = AppendMenuW(
                resize_menu,
                MF_STRING | MF_DISABLED,
                0,
                gap_label.as_pcwstr(),
            );
            let _ = AppendMenuW(resize_menu, MF_SEPARATOR, 0, PCWSTR::null());
            self.action_item(
                resize_menu,
                ID_GAP_INC,
                "Increase gaps",
                GLYPH_PLUS,
                false,
                Action::IncreaseGaps,
                state,
            );
            self.action_item(
                resize_menu,
                ID_GAP_DEC,
                "Decrease gaps",
                GLYPH_MINUS,
                false,
                Action::DecreaseGaps,
                state,
            );
            let _ = AppendMenuW(resize_menu, MF_SEPARATOR, 0, PCWSTR::null());
            let master_label =
                crate::util::WideStr::new(&format!("Master pane: {}%", state.master_percent));
            let _ = AppendMenuW(
                resize_menu,
                MF_STRING | MF_DISABLED,
                0,
                master_label.as_pcwstr(),
            );
            self.item(
                resize_menu,
                ID_MASTER_GROW,
                "Grow master pane",
                GLYPH_PLUS,
                false,
            );
            self.item(
                resize_menu,
                ID_MASTER_SHRINK,
                "Shrink master pane",
                GLYPH_MINUS,
                false,
            );
            let _ = AppendMenuW(resize_menu, MF_SEPARATOR, 0, PCWSTR::null());
            self.item(
                resize_menu,
                ID_SIZE_RESET,
                "Reset to defaults",
                GLYPH_RESET,
                false,
            );

            // --- top level ---
            // A hotkey another app already owns simply never fires; without
            // this the product looks broken rather than misconfigured.
            if state.hotkey_conflicts > 0 || state.hotkey_moved > 0 {
                let text = if state.hotkey_conflicts > 0 {
                    format!(
                        "{} shortcut{} unavailable \u{2014} click for details",
                        state.hotkey_conflicts,
                        if state.hotkey_conflicts == 1 { "" } else { "s" }
                    )
                } else {
                    // Fallbacks work. Saying so plainly avoids a red warning
                    // about a resolved situation on every first run.
                    format!(
                        "{} shortcut{} moved to alternatives \u{2014} details",
                        state.hotkey_moved,
                        if state.hotkey_moved == 1 { "" } else { "s" }
                    )
                };
                self.item(menu, ID_HOTKEY_CONFLICTS, &text, GLYPH_WARN, false);
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            }

            self.action_item(
                menu,
                ID_PALETTE,
                "Open command palette",
                GLYPH_SEARCH,
                true,
                Action::Palette,
                state,
            );
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            self.action_item(
                menu,
                ID_RETILE,
                "Retile now",
                GLYPH_RETILE,
                false,
                Action::Retile,
                state,
            );

            self.submenu(menu, layout_menu, "Layout", GLYPH_LAYOUT);
            self.submenu(menu, resize_menu, "Resize", GLYPH_RESIZE);

            // --- Dimming -------------------------------------------------
            let dim_menu = CreatePopupMenu().ok()?;
            self.item(
                dim_menu,
                ID_DIM_TOGGLE,
                if state.dim.enabled {
                    "Dimming is on"
                } else {
                    "Enable dimming"
                },
                GLYPH_DIM,
                false,
            );
            if state.dim.enabled {
                let _ = CheckMenuItem(dim_menu, ID_DIM_TOGGLE, MF_CHECKED.0);
            }
            let _ = AppendMenuW(dim_menu, MF_SEPARATOR, 0, PCWSTR::null());

            self.toggle_item(
                dim_menu,
                ID_DIM_AUTOTRACK,
                "Auto-track active window",
                state.dim.auto_track,
                None,
                state,
            );
            self.describe(
                ID_DIM_AUTOTRACK,
                "Auto-track active window",
                "Keeps whichever window has focus bright. Turn off to hold one \
                 window bright while you click elsewhere",
            );
            self.item(
                dim_menu,
                ID_DIM_PIN,
                if state.dim_pinned {
                    "Pinned \u{2014} re-pin focused window"
                } else {
                    "Select window (pin focused)"
                },
                GLYPH_TARGET,
                false,
            );
            self.describe(
                ID_DIM_PIN,
                "Select window",
                "Pins the focused window as the bright one \u{2014} useful for a \
                 windowed game you click away from",
            );

            let _ = AppendMenuW(dim_menu, MF_SEPARATOR, 0, PCWSTR::null());

            let win_menu = CreatePopupMenu().ok()?;
            for (i, level) in DIM_WINDOW_LEVELS.iter().enumerate() {
                let label = crate::util::WideStr::new(&format!("{level}% dark"));
                let checked = if *level == state.dim.window_level {
                    MF_CHECKED
                } else {
                    MF_UNCHECKED
                };
                let _ = AppendMenuW(
                    win_menu,
                    MF_STRING | checked,
                    (ID_DIM_WINDOW_BASE + i as u32) as usize,
                    label.as_pcwstr(),
                );
            }
            self.submenu(
                dim_menu,
                win_menu,
                &format!("Other windows: {}%", state.dim.window_level),
                GLYPH_DIM,
            );

            let bar_menu = CreatePopupMenu().ok()?;
            for (i, level) in DIM_TASKBAR_LEVELS.iter().enumerate() {
                let label = crate::util::WideStr::new(&if *level == 0 {
                    "Not dimmed".to_string()
                } else {
                    format!("{level}% dark")
                });
                let checked = if *level == state.dim.taskbar_level {
                    MF_CHECKED
                } else {
                    MF_UNCHECKED
                };
                let _ = AppendMenuW(
                    bar_menu,
                    MF_STRING | checked,
                    (ID_DIM_TASKBAR_BASE + i as u32) as usize,
                    label.as_pcwstr(),
                );
            }
            self.submenu(
                dim_menu,
                bar_menu,
                &format!("Taskbar & Start: {}%", state.dim.taskbar_level),
                GLYPH_DIM,
            );

            let dim_label = if state.dim.enabled {
                format!("Dimming ({}%)", state.dim.window_level)
            } else {
                "Dimming".to_string()
            };
            self.submenu(menu, dim_menu, &dim_label, GLYPH_DIM);

            // --- Windows -------------------------------------------------
            // One submenu per window: click the name to focus it, or use the
            // submenu to hold it out of the tiling or pin it on top.
            let windows_menu = CreatePopupMenu().ok()?;
            if state.windows.is_empty() {
                let none = crate::util::WideStr::new("No open windows");
                let _ = AppendMenuW(windows_menu, MF_STRING | MF_DISABLED, 0, none.as_pcwstr());
            }
            for (i, w) in state.windows.iter().enumerate().take(MAX_WINDOWS) {
                let entry = CreatePopupMenu().ok()?;

                self.item(
                    entry,
                    window_command_id(i, WindowAction::Focus),
                    "Bring to front",
                    GLYPH_FOCUS,
                    true,
                );
                self.describe(
                    window_command_id(i, WindowAction::Focus),
                    "Bring to front",
                    "Focuses this window. Hovering any entry outlines it on screen",
                );
                let _ = AppendMenuW(entry, MF_SEPARATOR, 0, PCWSTR::null());
                self.item(
                    entry,
                    window_command_id(i, WindowAction::Exclude),
                    "Exclude from tiling",
                    GLYPH_EXCLUDE,
                    false,
                );
                self.describe(
                    window_command_id(i, WindowAction::Exclude),
                    "Exclude from tiling",
                    "Leaves the window where you put it and re-tiles the rest",
                );
                if w.excluded {
                    let _ = CheckMenuItem(
                        entry,
                        window_command_id(i, WindowAction::Exclude),
                        MF_CHECKED.0,
                    );
                }
                self.item(
                    entry,
                    window_command_id(i, WindowAction::Topmost),
                    "Always on top",
                    GLYPH_PIN,
                    false,
                );
                self.describe(
                    window_command_id(i, WindowAction::Topmost),
                    "Always on top",
                    "Pins the window above all others",
                );
                if w.topmost {
                    let _ = CheckMenuItem(
                        entry,
                        window_command_id(i, WindowAction::Topmost),
                        MF_CHECKED.0,
                    );
                }

                // Markers in the label so state is readable without opening
                // the submenu: a list you must expand to read is not a list.
                let mut label = String::new();
                if w.topmost {
                    label.push_str("\u{1F4CC} ");
                }
                if w.excluded {
                    label.push_str("\u{2716} ");
                }
                label.push_str(&elide_title(&w.title, 60));
                if !w.app.is_empty() {
                    label.push_str(&format!("   ({})", w.app));
                }
                let wlabel = crate::util::WideStr::new(&label);
                let _ = AppendMenuW(windows_menu, MF_POPUP, entry.0 as usize, wlabel.as_pcwstr());
                crate::ui::hover::map_submenu(entry.0 as isize, w.hwnd);
                for action in [
                    WindowAction::Focus,
                    WindowAction::Exclude,
                    WindowAction::Topmost,
                ] {
                    crate::ui::hover::map_id(window_command_id(i, action), w.hwnd);
                }
            }
            if state.excluded_count > 0 {
                let _ = AppendMenuW(windows_menu, MF_SEPARATOR, 0, PCWSTR::null());
                self.item(
                    windows_menu,
                    ID_CLEAR_EXCLUSIONS,
                    &format!("Re-include all ({}) excluded", state.excluded_count),
                    GLYPH_RESET,
                    false,
                );
            }
            let windows_label = if state.windows.is_empty() {
                "Windows".to_string()
            } else {
                format!("Windows ({})", state.windows.len())
            };
            self.submenu(menu, windows_menu, &windows_label, GLYPH_WINDOWS);

            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let (pause_text, pause_glyph) = if state.paused {
                ("Resume tiling", GLYPH_PLAY)
            } else {
                ("Pause tiling", GLYPH_PAUSE)
            };
            let _ = pause_glyph;
            self.toggle_item(
                menu,
                ID_PAUSE,
                pause_text,
                state.paused,
                Some(Action::TogglePause),
                state,
            );

            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            // --- Settings ------------------------------------------------
            // The switches people actually reach for, plus a way into the
            // shortcut list. Editing a JSON file to pause tiling is not a
            // setting, it is a workaround.
            let settings_menu = CreatePopupMenu().ok()?;

            self.toggle_item(
                settings_menu,
                ID_START_WITH_WINDOWS,
                "Start with Windows",
                state.start_with_windows,
                None,
                state,
            );
            self.describe(
                ID_START_WITH_WINDOWS,
                "Start with Windows",
                "Adds one HKCU Run value. Removable here, or from Task Manager's Startup tab",
            );

            self.toggle_item(
                settings_menu,
                ID_AUTO_TILE,
                "Auto-tile new windows",
                state.auto_tile,
                None,
                state,
            );
            self.describe(
                ID_AUTO_TILE,
                "Auto-tile new windows",
                "Re-tile when a window opens, closes or un-minimises. Off means tiling only happens when you ask",
            );

            self.toggle_item(
                settings_menu,
                ID_SIZE_READOUT,
                "Show size while resizing",
                state.size_readout,
                None,
                state,
            );
            self.describe(
                ID_SIZE_READOUT,
                "Show size while resizing",
                "Draw the pixel width and height over each window as you drag. Amber once the window is at its minimum size",
            );

            self.toggle_item(
                settings_menu,
                ID_RESIZE_GRID,
                "Show grid while resizing",
                state.resize_grid,
                None,
                state,
            );
            self.describe(
                ID_RESIZE_GRID,
                "Show grid while resizing",
                "Outline every cell as you drag, so it reads as pulling the grid rather than one window",
            );

            self.toggle_item(
                settings_menu,
                ID_LOGGING,
                "Debug logging",
                state.logging,
                None,
                state,
            );
            self.describe(
                ID_LOGGING,
                "Debug logging",
                "Record what SuperTile does to a file on this machine. Includes window titles and program paths; nothing is sent anywhere",
            );

            self.toggle_item(
                settings_menu,
                ID_VERBOSE_LOGGING,
                "Debug logging: extensive",
                state.verbose_logging,
                None,
                state,
            );
            self.describe(
                ID_VERBOSE_LOGGING,
                "Debug logging: extensive",
                "Also record every retile, placement and window entering or leaving the layout. Megabytes per hour; switch it off when you are done",
            );

            let theme_menu = CreatePopupMenu().ok()?;
            for (i, (key, _)) in crate::ui::theme::OVERLAY_THEMES.iter().enumerate() {
                let id = ID_THEME_BASE + i as u32;
                self.plain_item(theme_menu, id, crate::ui::theme::overlay_label(key));
                if state.overlay_theme.eq_ignore_ascii_case(key) {
                    let _ = CheckMenuItem(theme_menu, id, MF_CHECKED.0);
                }
                self.describe(
                    id,
                    crate::ui::theme::overlay_label(key),
                    "Colour, transparency, line thickness, corner radius and readout font of the drag overlays",
                );
            }
            let _ = AppendMenuW(theme_menu, MF_SEPARATOR, 0, PCWSTR::null());

            let custom_label = format!("{} (custom)", state.custom_theme_name);
            self.plain_item(theme_menu, ID_THEME_CUSTOM, &custom_label);
            if state.overlay_theme.eq_ignore_ascii_case("custom") {
                let _ = CheckMenuItem(theme_menu, ID_THEME_CUSTOM, MF_CHECKED.0);
            }
            self.describe(
                ID_THEME_CUSTOM,
                &custom_label,
                "Use your own theme. Edit its colours below",
            );

            self.item(
                theme_menu,
                ID_THEME_ACCENT,
                "Grid colour...",
                GLYPH_DIM,
                false,
            );
            self.describe(
                ID_THEME_ACCENT,
                "Grid colour",
                "The colour of the grid outline and the drop preview",
            );
            self.item(
                theme_menu,
                ID_THEME_WARNING,
                "Blocked colour...",
                GLYPH_DIM,
                false,
            );
            self.describe(
                ID_THEME_WARNING,
                "Blocked colour",
                "Shown when a split will not fit or a boundary is at its minimum",
            );
            self.item(
                theme_menu,
                ID_THEME_TEXT,
                "Readout text colour...",
                GLYPH_DIM,
                false,
            );
            self.describe(
                ID_THEME_TEXT,
                "Readout text colour",
                "The pixel size drawn over a window while it is resized",
            );
            self.item(
                theme_menu,
                ID_THEME_EDITOR,
                "Theme editor...",
                GLYPH_DIM,
                false,
            );
            self.describe(
                ID_THEME_EDITOR,
                "Theme editor",
                "Pick a theme and edit its colours, transparency, line thickness and corner radius, with a live preview",
            );

            self.submenu(settings_menu, theme_menu, "Overlay theme", GLYPH_DIM);

            self.toggle_item(
                settings_menu,
                ID_RESERVE_ELEVATED,
                "Reserve gridbox for elevated apps",
                state.reserve_elevated,
                None,
                state,
            );
            self.describe(
                ID_RESERVE_ELEVATED,
                "Reserve gridbox for elevated apps",
                "Windows forbids moving an elevated window, so its cell can never be filled. Off, the space goes to windows that can use it",
            );

            self.toggle_item(
                settings_menu,
                ID_AUTO_UPDATE_CHECK,
                "Check for updates automatically",
                state.auto_update_check,
                None,
                state,
            );
            self.describe(
                ID_AUTO_UPDATE_CHECK,
                "Check for updates automatically",
                "Asks GitHub once a day whether a newer release exists. Contacts github.com; nothing about you is sent beyond the request itself",
            );

            self.item(
                settings_menu,
                ID_CHECK_UPDATES,
                "Check for updates now",
                GLYPH_RETILE,
                false,
            );
            self.describe(
                ID_CHECK_UPDATES,
                "Check for updates now",
                "Asks GitHub for the latest release and reports what it finds",
            );

            self.item(
                settings_menu,
                ID_ISSUE_REPORT,
                "Create issue report...",
                GLYPH_ABOUT,
                false,
            );
            self.describe(
                ID_ISSUE_REPORT,
                "Create issue report",
                "Collects your machine's shape into Markdown for a GitHub issue. Window titles, paths, your user name and machine name are all left out. Nothing is sent anywhere",
            );

            self.item(
                settings_menu,
                ID_OPEN_LOG_FOLDER,
                "Open log folder",
                GLYPH_FOLDER,
                false,
            );
            self.describe(
                ID_OPEN_LOG_FOLDER,
                "Open log folder",
                "Reveal supertile.log and config.json in Explorer",
            );

            self.action_item(
                settings_menu,
                ID_PAUSE,
                if state.paused {
                    "Resume tiling"
                } else {
                    "Pause tiling"
                },
                if state.paused {
                    GLYPH_PLAY
                } else {
                    GLYPH_PAUSE
                },
                false,
                Action::TogglePause,
                state,
            );
            if state.paused {
                let _ = CheckMenuItem(settings_menu, ID_PAUSE, MF_CHECKED.0);
            }

            // Always offered. It used to be hidden unless Claude Desktop was
            // installed, which was right when the question went to the desktop
            // client; it goes to claude.ai in a browser now, so there is
            // nothing to be absent.
            self.toggle_item(
                settings_menu,
                ID_CLAUDE_SOURCE,
                "Claude in command palette",
                state.claude_enabled,
                None,
                state,
            );
            self.describe(
                ID_CLAUDE_SOURCE,
                "Claude in command palette",
                "Press Tab in the palette to ask a question. Opens claude.ai in your browser with it typed in, ready for you to send",
            );

            let _ = AppendMenuW(settings_menu, MF_SEPARATOR, 0, PCWSTR::null());

            self.item(
                settings_menu,
                ID_SHOW_HOTKEYS,
                "Keyboard shortcuts…",
                GLYPH_KEYBOARD,
                false,
            );
            self.describe(
                ID_SHOW_HOTKEYS,
                "Keyboard shortcuts",
                "Lists every shortcut and lets you change one by pressing the new keys",
            );

            self.item(
                settings_menu,
                ID_EDIT_CONFIG,
                "Edit config.json…",
                GLYPH_EDIT,
                false,
            );
            self.describe(
                ID_EDIT_CONFIG,
                "Edit config.json",
                "Opens the configuration file for the settings not exposed here",
            );

            self.submenu(menu, settings_menu, "Settings", GLYPH_SETTINGS);
            self.item(menu, ID_ABOUT, "About & SBOM…", GLYPH_ABOUT, false);
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            self.action_item(
                menu,
                ID_EXIT,
                "Exit SuperTile",
                GLYPH_EXIT,
                false,
                Action::Quit,
                state,
            );

            // A tray menu only dismisses correctly when its owner is the
            // foreground window; this is the documented workaround.
            let _ = SetForegroundWindow(self.hwnd);

            let chosen = TrackPopupMenuEx(
                menu,
                (TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY).0,
                pt.x,
                pt.y,
                self.hwnd,
                None,
            );

            // Companion to the SetForegroundWindow call above: without this the
            // menu can stay on screen after a click elsewhere.
            let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));

            let _ = DestroyMenu(menu); // destroys attached submenus too
            crate::ui::hover::end_menu();

            if chosen.0 == 0 {
                None
            } else {
                MenuCommand::from_id(chosen.0 as u32)
            }
        }
    }

    /// Append an item for an action, with its accelerator and hover tooltip.
    ///
    /// The accelerator column shows the key that is actually registered; the
    /// tooltip explains a fallback or a total conflict, which the column has
    /// no room for.
    // Every argument is menu-construction context the caller already holds;
    // bundling them into a struct would only move the same fields elsewhere.
    #[allow(clippy::too_many_arguments)]
    fn action_item(
        &mut self,
        menu: HMENU,
        id: u32,
        text: &str,
        glyph: char,
        default: bool,
        action: Action,
        state: &MenuState,
    ) {
        match state.bindings.iter().find(|b| b.action == action) {
            Some(info) => {
                self.item(
                    menu,
                    id,
                    &format!("{text}\t{}", info.accel()),
                    glyph,
                    default,
                );
                crate::ui::hover::map_tip(id, info.tip());
            }
            None => self.item(menu, id, text, glyph, default),
        }
    }

    /// Append a checkable item whose state is actually visible.
    ///
    /// `CheckMenuItem` draws its tick in the same column as `hbmpItem`, so an
    /// item with a glyph shows no check mark at all -- the toggles looked
    /// identical on and off. The glyph is therefore swapped for a tick when
    /// the item is on, which reads unambiguously and keeps an icon in both
    /// states. `CheckMenuItem` is still called so the state is correct for
    /// screen readers and for any theme that draws its own indicator.
    #[allow(clippy::too_many_arguments)]
    fn toggle_item(
        &mut self,
        menu: HMENU,
        id: u32,
        text: &str,
        on: bool,
        action: Option<Action>,
        state: &MenuState,
    ) {
        // No `hbmpItem` on a switch, deliberately.
        //
        // Windows draws that bitmap in the very column it reserves for a check
        // mark, so setting one does not merely fail to show the state -- it
        // suppresses the check entirely, and every switch looks identical
        // whatever it is set to. Swapping the bitmap for a tick glyph was the
        // previous attempt and still wrong: a Segoe icon in the gutter is not
        // what a Windows user reads as "on", because it is not what any other
        // menu on the system uses.
        //
        // Leaving the column alone lets `CheckMenuItem` draw the native mark --
        // the same tick, in the same place, as every other Windows menu. There
        // is deliberately no glyph parameter: an argument that does nothing is
        // how the wrong icon reached these items to begin with.
        match action.and_then(|a| state.bindings.iter().find(|b| b.action == a)) {
            Some(info) => {
                self.plain_item(menu, id, &format!("{text}	{}", info.accel()));
                crate::ui::hover::map_tip(id, info.tip());
            }
            None => self.plain_item(menu, id, text),
        }
        // SAFETY: menu is a live popup and `id` was just appended to it.
        unsafe {
            let _ = CheckMenuItem(menu, id, if on { MF_CHECKED.0 } else { MF_UNCHECKED.0 });
        }
    }

    /// Append an item with no bitmap, leaving the check-mark column free.
    fn plain_item(&mut self, menu: HMENU, id: u32, text: &str) {
        let label = crate::util::WideStr::new(text);
        // SAFETY: menu is a live popup menu; label outlives the call.
        unsafe {
            let _ = AppendMenuW(menu, MF_STRING, id as usize, label.as_pcwstr());
        }
    }

    /// Attach a plain descriptive tooltip to an item that has no hotkey.
    fn describe(&self, id: u32, title: &str, detail: &str) {
        crate::ui::hover::map_tip(
            id,
            TipText {
                title: title.to_string(),
                keys: String::new(),
                detail: detail.to_string(),
            },
        );
    }

    /// Append one item with a glyph bitmap.
    fn item(&mut self, menu: HMENU, id: u32, text: &str, glyph: char, default: bool) {
        let label = crate::util::WideStr::new(text);
        // SAFETY: menu is a live popup menu; label outlives the call.
        unsafe {
            let flags = if default {
                MF_STRING | MF_DEFAULT
            } else {
                MF_STRING
            };
            let _ = AppendMenuW(menu, flags, id as usize, label.as_pcwstr());
            if let Some(bmp) = self.glyph(glyph) {
                let mii = MENUITEMINFOW {
                    cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                    fMask: MIIM_BITMAP,
                    hbmpItem: bmp,
                    ..Default::default()
                };
                let _ = SetMenuItemInfoW(menu, id, false, &mii);
            }
        }
    }

    fn submenu(&mut self, parent: HMENU, child: HMENU, text: &str, glyph: char) {
        let label = crate::util::WideStr::new(text);
        // SAFETY: both menus are live; ownership of `child` transfers to
        // `parent`, which destroys it.
        unsafe {
            let _ = AppendMenuW(parent, MF_POPUP, child.0 as usize, label.as_pcwstr());
            if let Some(bmp) = self.glyph(glyph) {
                let mii = MENUITEMINFOW {
                    cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                    fMask: MIIM_BITMAP,
                    hbmpItem: bmp,
                    ..Default::default()
                };
                // Submenus have no command id, so address by position.
                let pos = GetMenuItemCount(Some(parent)).max(1) as u32 - 1;
                let _ = SetMenuItemInfoW(parent, pos, true, &mii);
            }
        }
    }

    /// Discard cached glyphs when the menu metric changes (e.g. DPI change).
    fn ensure_glyphs(&mut self) {
        // SAFETY: reads a system metric.
        let want = unsafe { GetSystemMetrics(SM_CXMENUCHECK) }.clamp(12, 64);
        if want != self.glyph_px {
            self.free_bitmaps();
            self.glyph_px = want;
        }
    }

    fn glyph(&mut self, ch: char) -> Option<HBITMAP> {
        if let Some(existing) = self.bitmaps.get(&ch) {
            return Some(*existing);
        }
        let bmp = glyph_bitmap(ch, self.glyph_px, menu_text_color())?;
        self.bitmaps.insert(ch, bmp);
        Some(bmp)
    }

    fn free_bitmaps(&mut self) {
        for (_, b) in self.bitmaps.drain() {
            // SAFETY: every handle came from CreateDIBSection in glyph_bitmap
            // and is deleted exactly once.
            unsafe {
                let _ = DeleteObject(b.into());
            }
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove();
        self.free_bitmaps();
        // SAFETY: icons came from LoadImageW with LR_DEFAULTCOLOR (not shared),
        // so we own them.
        unsafe {
            if !self.icon_normal.is_invalid() {
                let _ = DestroyIcon(self.icon_normal);
            }
            if !self.icon_paused.is_invalid() {
                let _ = DestroyIcon(self.icon_paused);
            }
        }
    }
}

/// Shorten a window title for a menu label, on a char boundary.
///
/// Menu items do not wrap, and a browser tab title can run to hundreds of
/// characters; an over-wide tray menu is worse than a truncated one.
fn elide_title(title: &str, max_chars: usize) -> String {
    let t = title.trim();
    if t.chars().count() <= max_chars {
        return t.to_string();
    }
    let kept: String = t.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{}\u{2026}", kept.trim_end())
}

fn menu_text_color() -> (u8, u8, u8) {
    // SAFETY: GetSysColor takes a documented index and cannot fail.
    let c = unsafe { GetSysColor(COLOR_MENUTEXT) };
    (
        (c & 0xFF) as u8,
        ((c >> 8) & 0xFF) as u8,
        ((c >> 16) & 0xFF) as u8,
    )
}

/// Rasterise one icon-font glyph into a premultiplied 32-bit ARGB bitmap.
///
/// Returns `None` if any GDI object cannot be created. The caller owns the
/// returned `HBITMAP` and must `DeleteObject` it.
pub fn glyph_bitmap(ch: char, size: i32, rgb: (u8, u8, u8)) -> Option<HBITMAP> {
    // Upper bound as well as lower: `size * size` overflows i32 above 46340,
    // and the pixel slice below is built from that product. The only caller
    // clamps to 12..=64, but this function is public and must not depend on
    // a guarantee that lives outside it.
    if !(1..=1024).contains(&size) {
        return None;
    }
    // SAFETY: creates a screen-compatible memory DC and a DIB section; both
    // are released on every return path, and the pixel buffer is only written
    // through the size CreateDIBSection reported.
    unsafe {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        ReleaseDC(None, screen);
        if dc.is_invalid() {
            return None;
        }

        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                // Negative height => top-down rows, so pixel indexing is linear.
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: bi describes a 32bpp top-down DIB; `bits` receives the pixel
        // buffer, which stays valid while the bitmap lives.
        let bmp = match CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(b) if !b.is_invalid() && !bits.is_null() => b,
            _ => {
                let _ = DeleteDC(dc);
                return None;
            }
        };

        let old = SelectObject(dc, bmp.into());

        // Try Segoe Fluent Icons (Windows 11), then Segoe MDL2 Assets
        // (Windows 10). Both map the code points we use.
        let mut font = HFONT::default();
        for family in ["Segoe Fluent Icons", "Segoe MDL2 Assets"] {
            let name = crate::util::WideStr::new(family);
            let mut lf = LOGFONTW {
                lfHeight: -size,
                lfWeight: FW_NORMAL.0 as i32,
                lfCharSet: DEFAULT_CHARSET,
                lfQuality: CLEARTYPE_QUALITY,
                ..Default::default()
            };
            let src: Vec<u16> = family.encode_utf16().collect();
            let n = src.len().min(lf.lfFaceName.len() - 1);
            lf.lfFaceName[..n].copy_from_slice(&src[..n]);
            let _ = name;
            let f = CreateFontIndirectW(&lf);
            if !f.is_invalid() {
                font = f;
                break;
            }
        }
        if font.is_invalid() {
            let _ = SelectObject(dc, old);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(dc);
            return None;
        }
        let old_font = SelectObject(dc, font.into());

        SetBkMode(dc, TRANSPARENT);
        // White on the zeroed (black, fully transparent) DIB, so luminance
        // becomes the coverage mask below.
        SetTextColor(dc, COLORREF(0x00FF_FFFF));

        let mut buf = [0u16; 2];
        let encoded = ch.encode_utf16(&mut buf);
        let mut rect = windows::Win32::Foundation::RECT {
            left: 0,
            top: 0,
            right: size,
            bottom: size,
        };
        DrawTextW(
            dc,
            encoded,
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOCLIP,
        );

        // GDI wrote no alpha. Derive coverage from the drawn luminance and
        // premultiply against the requested colour, which is what the menu
        // renderer expects from a 32bpp hbmpItem.
        // SAFETY: `bits` points at size*size 32-bit pixels owned by the DIB.
        let pixels = std::slice::from_raw_parts_mut(bits as *mut u32, (size * size) as usize);
        for p in pixels.iter_mut() {
            let v = *p;
            let r = (v >> 16) & 0xFF;
            let g = (v >> 8) & 0xFF;
            let b = v & 0xFF;
            let a = r.max(g).max(b); // coverage
            let pr = (rgb.0 as u32 * a) / 255;
            let pg = (rgb.1 as u32 * a) / 255;
            let pb = (rgb.2 as u32 * a) / 255;
            *p = (a << 24) | (pr << 16) | (pg << 8) | pb;
        }

        let _ = SelectObject(dc, old_font);
        let _ = DeleteObject(font.into());
        let _ = SelectObject(dc, old);
        let _ = DeleteDC(dc);
        Some(bmp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_id_maps_back() {
        let cases = [
            (ID_PALETTE, MenuCommand::OpenPalette),
            (ID_RETILE, MenuCommand::Retile),
            (ID_PAUSE, MenuCommand::TogglePause),
            (ID_SETTINGS, MenuCommand::Settings),
            (ID_ABOUT, MenuCommand::About),
            (ID_EXIT, MenuCommand::Exit),
            (ID_GAP_INC, MenuCommand::GapIncrease),
            (ID_GAP_DEC, MenuCommand::GapDecrease),
            (ID_MASTER_GROW, MenuCommand::MasterGrow),
            (ID_MASTER_SHRINK, MenuCommand::MasterShrink),
            (ID_SIZE_RESET, MenuCommand::SizeReset),
        ];
        for (id, want) in cases {
            assert_eq!(MenuCommand::from_id(id), Some(want), "id {id}");
        }
    }

    #[test]
    fn every_layout_has_a_menu_id_that_round_trips() {
        for (i, kind) in LayoutKind::ALL.iter().enumerate() {
            let id = ID_LAYOUT_BASE + i as u32;
            assert_eq!(
                MenuCommand::from_id(id),
                Some(MenuCommand::SetLayout(*kind))
            );
        }
    }

    #[test]
    fn unknown_ids_are_rejected_rather_than_panicking() {
        assert_eq!(MenuCommand::from_id(0), None);
        assert_eq!(MenuCommand::from_id(1), None);
        assert_eq!(MenuCommand::from_id(999), None);
        // Just past the end of the layout block.
        assert_eq!(
            MenuCommand::from_id(ID_LAYOUT_BASE + LayoutKind::ALL.len() as u32),
            None
        );
        assert_eq!(MenuCommand::from_id(u32::MAX), None);
    }

    #[test]
    fn fixed_ids_do_not_collide_with_the_layout_block() {
        let fixed = [
            ID_RETILE,
            ID_PAUSE,
            ID_SETTINGS,
            ID_ABOUT,
            ID_EXIT,
            ID_PALETTE,
            ID_GAP_INC,
            ID_GAP_DEC,
            ID_MASTER_GROW,
            ID_MASTER_SHRINK,
            ID_SIZE_RESET,
        ];
        let layout_end = ID_LAYOUT_BASE + LayoutKind::ALL.len() as u32;
        for id in fixed {
            assert!(
                id < ID_LAYOUT_BASE || id >= layout_end,
                "id {id} collides with the layout id block"
            );
        }
        // And all fixed ids are distinct.
        let mut v = fixed.to_vec();
        let n = v.len();
        v.sort_unstable();
        v.dedup();
        assert_eq!(v.len(), n);
    }

    #[test]
    fn command_ids_are_never_zero() {
        // TrackPopupMenuEx returns 0 for "cancelled", so no command may use it.
        for i in 0..LayoutKind::ALL.len() {
            assert_ne!(ID_LAYOUT_BASE + i as u32, 0);
        }
        assert!(MenuCommand::from_id(0).is_none());
    }

    #[test]
    fn tray_callback_is_in_the_application_message_range() {
        const { assert!(WM_TRAY_CALLBACK >= WM_APP) };
        const {
            assert!(
                WM_TRAY_CALLBACK < 0xC000,
                "must not collide with RegisterWindowMessage"
            )
        };
    }

    #[test]
    fn glyph_bitmap_renders_visible_coverage() {
        // Needs GDI, which is available in any desktop session.
        let bmp = glyph_bitmap(GLYPH_SETTINGS, 16, (255, 255, 255))
            .expect("glyph bitmap should be creatable");
        assert!(!bmp.is_invalid());
        // SAFETY: handle came from glyph_bitmap and is deleted once.
        unsafe {
            let _ = DeleteObject(bmp.into());
        }
    }

    #[test]
    fn glyph_bitmap_rejects_a_nonpositive_size() {
        assert!(glyph_bitmap('A', 0, (0, 0, 0)).is_none());
        assert!(glyph_bitmap('A', -4, (0, 0, 0)).is_none());
    }
}

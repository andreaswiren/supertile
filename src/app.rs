//! Application core: the hidden owner window, the event loop's handlers, and
//! the tiling orchestration that ties everything together.
//!
//! ## Why a hidden window rather than a message-only window
//!
//! `HWND_MESSAGE` windows are cheaper, but they do **not** receive broadcast
//! messages — including `TaskbarCreated`, which is the only notification that
//! Explorer restarted and the tray icon needs re-adding. A zero-size hidden
//! top-level window costs nothing extra and keeps the icon reliable.
//!
//! ## Why events, not polling
//!
//! Window changes arrive through `SetWinEventHook`. Polling would either be
//! laggy or burn CPU on a process that is meant to be invisible in Task
//! Manager. Hook callbacks only ever *post a message*; all real work happens on
//! the UI thread, so there is no cross-thread state to guard.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::{Config, Loaded};
use crate::dimmer::Dimmer;
use crate::drag::{self, DragKind};
use crate::hotkeys::{Action, Hotkey};
use crate::layout::{self, Direction, LayoutKind, Rect, Splits};
use crate::memory::{Store, Suggestion};
use crate::monitor::Monitor;
use crate::tray::{BindingInfo, MenuCommand, MenuState, Tray, WindowEntry, WM_TRAY_CALLBACK};
use crate::ui::about::About;
use crate::ui::highlight::Highlight;
use crate::ui::keys::{KeyRow, Keys};
use crate::ui::palette::{self, Palette};
use crate::window::{self, WindowInfo};

const CLASS_NAME: &str = "SuperTile.Host";
/// Coalescing timer for window events.
const TIMER_RETILE: usize = 10;
/// Periodic flush of the geometry store, so a hard kill loses at most this.
const TIMER_SAVE: usize = 11;
const SAVE_INTERVAL_MS: u32 = 30_000;
/// Retries adding the tray icon when the shell was not ready.
const TIMER_TRAY_RETRY: usize = 12;
/// Polls the dragged window while a move or resize is in progress.
///
/// Windows reports the start and the end of a drag but nothing in between, so
/// live feedback has to be sampled. ~60 Hz is smooth and costs nothing next to
/// what the drag itself is already doing.
const TIMER_DRAG: usize = 13;
const DRAG_POLL_MS: u32 = 16;
const TRAY_RETRY_MS: u32 = 2_000;
/// How far a window may sit from where it was put before it counts as a miss.
///
/// Generous: DPI rounding and shadow compensation move an edge a pixel or two
/// legitimately.
const PLACEMENT_TOLERANCE: i32 = 4;
/// How long after startup a saved split layout keeps trying to reattach.
///
/// Long enough for the applications that start with Windows to have windows,
/// short enough to be over before anybody has arranged anything by hand.
const RESTORE_WINDOW_MS: u64 = 30_000;
/// Misses before a window is left alone -- counted over time, not over passes.
const MAX_PLACEMENT_MISSES: u8 = 3;
/// Minimum gap between two counted misses, in milliseconds.
///
/// Passes are not evidence. A drag re-tiles every 16ms and an event storm can
/// fire a dozen retiles in a quarter of a second, so three consecutive *passes*
/// can elapse while an application is merely mid-animation. Requiring the
/// misses to be spread over time is what makes them mean "this window will not
/// comply" rather than "this window was busy just now".
const MISS_INTERVAL_MS: u64 = 750;
/// How long a window stays written off before it is offered another chance.
///
/// Never retrying was wrong: the reasons a window refuses a size are mostly
/// temporary -- being dragged by its own application, animating, starting up.
/// Condemning it for the session turns a two-second problem into one that lasts
/// until SuperTile is restarted.
const STUBBORN_RETRY_MS: u64 = 20_000;
/// About a minute of retries. At logon Explorer can take tens of seconds to
/// create the taskbar, and until it does `Shell_NotifyIcon` simply fails --
/// leaving SuperTile running with no way to reach it.
const TRAY_RETRY_LIMIT: u32 = 30;

/// Posted by the WinEvent hook; means "something changed, consider retiling".
const WM_WINDOW_EVENT: u32 = WM_APP + 30;
/// Posted by the app-list worker when the scan finishes.
pub const WM_APPS_READY: u32 = WM_APP + 31;
/// Posted by the WinEvent hook when the foreground window changes.
const WM_FOREGROUND_CHANGED: u32 = WM_APP + 32;
/// Posted when the user starts dragging or resizing a window; wParam is the HWND.
const WM_DRAG_START: u32 = WM_APP + 33;
/// Posted when the drag finishes.
const WM_DRAG_END: u32 = WM_APP + 34;

// Set while a modal Win32 loop owns the message pump.
//
// TrackPopupMenuEx and MessageBoxW run their own loops and dispatch messages
// to the owner window. Because host_wndproc hands out &mut App from a raw
// pointer, any message delivered while show_tray_menu still holds &mut self
// would produce a second live &mut to the same value -- undefined behaviour,
// and observably wrong: a nested tray menu rewrites self.listed, so the outer
// menu's "Always on top" lands on a different window than the one clicked.
//
// Messages arriving during a modal loop are therefore not handled at all.
// Thread-local because every window involved belongs to the UI thread.
thread_local! {
    static MODAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Guard that marks the modal window as open for its lifetime.
struct ModalGuard;

impl ModalGuard {
    fn enter() -> ModalGuard {
        MODAL.with(|m| m.set(true));
        ModalGuard
    }
}

impl Drop for ModalGuard {
    fn drop(&mut self) {
        MODAL.with(|m| m.set(false));
    }
}

fn in_modal_loop() -> bool {
    MODAL.with(|m| m.get())
}

/// The host window handle, for callbacks that cannot carry state.
///
/// WinEvent hooks are plain `extern "system"` functions with no user pointer,
/// so the target window is published here. Only ever written once, at startup.
static HOST_HWND: AtomicIsize = AtomicIsize::new(0);

/// Ordered window list for one monitor.
///
/// Tiling must be *stable*: retiling twice with the same windows must produce
/// the same arrangement, or windows shuffle every time an unrelated app opens.
/// Insertion order is therefore remembered per monitor rather than re-derived
/// from `EnumWindows`, whose order changes with z-order.
#[derive(Default)]
struct MonitorOrder {
    order: Vec<isize>,
}

impl MonitorOrder {
    /// Merge the live window set into the remembered order.
    ///
    /// Returns the indices (into `order`) of windows that are newly seen, so
    /// the caller can consult geometry memory for them.
    fn reconcile(&mut self, live: &[WindowInfo]) -> Vec<isize> {
        let live_set: HashSet<isize> = live.iter().map(|w| w.hwnd).collect();
        self.order.retain(|h| live_set.contains(h));
        let known: HashSet<isize> = self.order.iter().copied().collect();
        let mut fresh = Vec::new();
        for w in live {
            if !known.contains(&w.hwnd) {
                self.order.push(w.hwnd);
                fresh.push(w.hwnd);
            }
        }
        fresh
    }

    fn index_of(&self, hwnd: isize) -> Option<usize> {
        self.order.iter().position(|h| *h == hwnd)
    }

    /// Move `hwnd` to `to`, shifting the rest along.
    fn move_to(&mut self, hwnd: isize, to: usize) {
        let Some(from) = self.index_of(hwnd) else {
            return;
        };
        let to = to.min(self.order.len().saturating_sub(1));
        if from == to {
            return;
        }
        let v = self.order.remove(from);
        self.order.insert(to, v);
    }

    fn swap(&mut self, a: usize, b: usize) {
        if a < self.order.len() && b < self.order.len() {
            self.order.swap(a, b);
        }
    }
}

pub struct App {
    hwnd: HWND,
    config: Config,
    warnings: Vec<String>,
    store: Store,
    store_dirty: bool,
    tray: Tray,
    palette: Box<Palette>,
    about: Box<About>,
    keys_window: Box<Keys>,
    /// Snapshot backing the tray's Windows submenu, indexed by menu id.
    /// Rebuilt each time the menu opens; a window may die before it is used,
    /// so every action re-validates the handle.
    listed: Vec<WindowEntry>,
    apps: crate::applist::AppIndex,
    bindings: HashMap<Action, Hotkey>,
    /// Actions with no usable binding at all: every candidate was refused.
    rejected: Vec<(Action, Hotkey)>,
    /// Actions bound to a fallback: (action, first choice, what we got).
    fell_back: Vec<(Action, Hotkey, Hotkey)>,
    hooks: Vec<HWINEVENTHOOK>,
    orders: HashMap<String, MonitorOrder>,
    /// Windows the user has explicitly floated this session.
    floated: HashSet<isize>,
    fingerprint: String,
    /// Remaining attempts to register the tray icon.
    tray_retries: u32,
    /// Layout in use before Toggle fullscreen switched to Monocle.
    prev_layout: Option<LayoutKind>,
    /// Focus dimming overlays.
    dimmer: Dimmer,
    /// Live move/resize, if one is in progress.
    drag: Option<DragSession>,
    /// Translucent overlay showing where a dragged window would land.
    preview: Box<Highlight>,
    /// Live "1352 x 651" chip shown over a window while it is being resized.
    readout: Box<Highlight>,
    /// Outline of every cell, shown while a boundary is being dragged.
    grid: Box<Highlight>,
    /// The rectangle each window was last asked to take.
    ///
    /// Compared against reality on the next pass to notice windows that will
    /// not accept it.
    requested: HashMap<isize, Rect>,
    /// Consecutive passes a window has failed to land where it was put.
    misses: HashMap<isize, (u8, u64)>,
    /// Windows that refuse to be sized, left alone for the rest of the session.
    ///
    /// Some windows clamp whatever they are given -- Electron apps with a
    /// minimum size, consoles that size in character cells. Without this they
    /// are handed a `SetWindowPos` on every single retile for the life of the
    /// process, which looks erratic to the user and can feed a loop when the
    /// app reacts to being resized by creating or destroying a window.
    unmanageable: HashMap<isize, u64>,
    /// The managed set as of the last retile, for spotting departures.
    ///
    /// A window silently leaving the layout is the hardest kind of fault to
    /// report: by the time it is noticed the evidence is gone. Diffing the set
    /// each pass turns it into one log line at the moment it happens.
    last_managed: HashSet<isize>,
    /// Where the pointer was on the previous pass.
    ///
    /// The pointer is the only thing that reliably moves during a drag on a
    /// window that resizes itself lazily, so it is what a drag has to be
    /// detected from.
    last_pointer: (i32, i32),
    /// Each window's rectangle as of the previous pass.
    ///
    /// The difference between "this window is not where we put it" and "this
    /// window is moving right now" -- which is the difference between a window
    /// that cannot comply and a user dragging it. One observation cannot tell
    /// them apart; two can.
    last_rects: HashMap<isize, Rect>,
    /// Minimum track size per window, asked for once.
    ///
    /// Keyed on HWND and pruned with the window list. `WM_GETMINMAXINFO` is a
    /// synchronous send into another process; doing it for every window on
    /// every retile would make the tiler as slow as its slowest application.
    mins: HashMap<isize, (i32, i32)>,
    /// The theme editor, kept alive so its window survives between openings.
    theme_editor: Box<crate::ui::theme_editor::ThemeEditor>,
    /// A newer release found by a check, for the About window to offer.
    pending_update: Option<(String, String)>,
    /// Windows we are not permitted to move, because they are elevated.
    ///
    /// Kept apart from `unmanageable`: that means "this window declines the
    /// size we asked for", which is a property of the application. This means
    /// "Windows will not let us ask", which is a property of the security
    /// boundary and is fixed by restarting elevated, not by trying again.
    elevated: HashSet<isize>,
    /// Windows already probed for elevation, so the syscall happens once each.
    checked_elevation: HashSet<isize>,
    /// Identity key per window handle, recorded while the window list is in
    /// hand so the split trees can be saved and matched by *what* a window is
    /// rather than by a handle that will not survive the session.
    keys: HashMap<isize, String>,
    /// Split trees as they were last saved, waiting to be matched to windows.
    saved_trees: std::collections::BTreeMap<String, crate::tree::SavedNode>,
    /// Monitors whose saved tree has been restored, or given up on.
    restored: HashSet<String>,
    /// Tick count at startup, bounding how long a restore keeps trying.
    started_at: u64,
    /// Partition trees, per monitor. Only used by [`LayoutKind::Bsp`].
    trees: HashMap<String, crate::tree::Tree>,
    /// The layout in use before a drop switched the monitor to the tree.
    ///
    /// One drag should not be able to change the layout mode permanently with
    /// no way back. "Reset sizes" restores this, so the switch is always
    /// undoable by someone who did not realise they had made it.
    layout_before_split: Option<LayoutKind>,
    /// Dragged split positions, per monitor and layout.
    ///
    /// Keyed by layout too: the boundaries of a three-column arrangement mean
    /// nothing once you switch to Master + Stack, and silently reusing them
    /// would produce a layout the user never asked for.
    splits: HashMap<(String, LayoutKind), Splits>,
}

/// A move or resize the user is performing right now.
struct DragSession {
    hwnd: isize,
    /// Monitor the drag started on.
    device: String,
    /// Slot the window occupied when the drag began.
    zone_index: usize,
    /// The zone it was sitting in, used as the reference for edge movement.
    zone: Rect,
    /// Window rect when the drag began, used to tell a move from a resize.
    start: Rect,
    /// The drop currently previewed: which window, and what would happen.
    drop: Option<drag::Drop>,
    /// The boundary edit last applied, so an unchanged poll costs nothing.
    ///
    /// Re-tiling means re-enumerating every top-level window and querying its
    /// owning process. Doing that 60 times a second while the pointer sits
    /// still would be indefensible in a program that claims to be invisible in
    /// Task Manager, and most polls during a drag see no movement at all.
    last_applied: Option<(crate::layout::SplitAxis, usize, f32)>,
    /// What the user grabbed, decided once when the drag began.
    ///
    /// `None` when the window did not give a usable answer, in which case the
    /// kind is inferred from the rectangle each frame as before.
    grabbed: Option<DragKind>,
    /// Raw `WM_NCHITTEST` result, so the dragged edges are known exactly.
    hit: u32,
    /// The window was outside the grid when the drag began.
    ///
    /// Such a drag changes nothing about the layout while it is in progress:
    /// there is no boundary to move and no cell to preview. All that matters is
    /// what happens at the drop.
    detached: bool,
    /// Title at the time of the grab, for log lines after the fact.
    title: String,
    /// The dragged window's minimum track size, asked for once.
    ///
    /// Queried at the start rather than every poll: `WM_GETMINMAXINFO` is a
    /// synchronous cross-process send, and 60 of those a second during a drag
    /// would stall on any application that is briefly busy.
    min: (i32, i32),
}

impl App {
    pub fn new() -> Option<Box<App>> {
        let Loaded {
            config, warnings, ..
        } = Config::load();
        crate::util::set_logging(config.diagnostics.logging);
        crate::util::set_verbose(config.diagnostics.verbose);
        crate::log!("SuperTile {} starting", crate::build_id());

        let hwnd = create_host_window()?;
        HOST_HWND.store(hwnd.0 as isize, Ordering::Release);

        let dim_cfg = config.dimming;
        let store = if config.memory.enabled {
            Store::load()
        } else {
            Store::default()
        };
        let monitors = crate::monitor::enumerate();
        let fingerprint = crate::monitor::fingerprint(&monitors);

        let palette = Palette::new(hwnd, &config);
        let about = About::new(&config);
        let keys_window = Keys::new(hwnd, &config);
        let (_, dark) = crate::ui::theme::colors_for(config.appearance.theme);
        crate::ui::hover::install(dark);
        let preview = Highlight::new(dark);
        preview.set_filled(true);
        let readout = Highlight::new(dark);
        readout.set_filled(true);
        let grid = Highlight::new(dark);
        // One theme across all three overlays: they appear together during a
        // drag, and a grid in one palette with a readout in another reads as
        // two unrelated things rather than one gesture.
        let theme_editor = crate::ui::theme_editor::ThemeEditor::new(dark);
        theme_editor.apply_config(&config);

        let mut overlay = if config
            .appearance
            .overlay_theme
            .eq_ignore_ascii_case("custom")
        {
            crate::ui::theme::overlay_from_custom(&config.appearance.custom_theme)
        } else {
            crate::ui::theme::overlay_by_name(&config.appearance.overlay_theme)
        };
        // Thickness can be overridden without abandoning the theme's colours.
        if config.appearance.overlay_line_dip > 0 {
            overlay.border_dip = (config.appearance.overlay_line_dip as i32).clamp(1, 8);
        }
        preview.set_overlay(overlay);
        readout.set_overlay(overlay);
        grid.set_overlay(overlay);
        let tray = Tray::new(hwnd);

        let mut app = Box::new(App {
            hwnd,
            config,
            warnings,
            store,
            store_dirty: false,
            tray,
            palette,
            about,
            keys_window,
            listed: Vec::new(),
            apps: crate::applist::AppIndex::new(),
            bindings: HashMap::new(),
            rejected: Vec::new(),
            fell_back: Vec::new(),
            hooks: Vec::new(),
            orders: HashMap::new(),
            floated: HashSet::new(),
            fingerprint,
            tray_retries: TRAY_RETRY_LIMIT,
            prev_layout: None,
            dimmer: Dimmer::new(dim_cfg),
            drag: None,
            preview,
            readout,
            grid,
            splits: HashMap::new(),
            trees: HashMap::new(),
            keys: HashMap::new(),
            theme_editor,
            pending_update: None,
            elevated: HashSet::new(),
            checked_elevation: HashSet::new(),
            saved_trees: Self::load_saved_trees(),
            restored: HashSet::new(),
            started_at: crate::util::tick_ms(),
            layout_before_split: None,
            requested: HashMap::new(),
            misses: HashMap::new(),
            unmanageable: HashMap::new(),
            mins: HashMap::new(),
            last_rects: HashMap::new(),
            last_pointer: (i32::MIN, i32::MIN),
            last_managed: HashSet::new(),
        });

        // Publish the App pointer so the host wndproc can reach it.
        let ptr: *mut App = &mut *app;
        // SAFETY: the App is boxed and outlives its window; Drop tears the
        // window down before the box is freed.
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);
        }

        app.ensure_tray_icon();
        app.register_hotkeys();
        for w in &app.warnings {
            crate::log!("config warning: {w}");
        }
        for (action, hk) in &app.rejected {
            crate::log!(
                "hotkey {hk} for '{}' is already owned by another application",
                action.config_key()
            );
        }
        app.install_hook();
        app.start_app_scan();

        // SAFETY: hwnd is live; the timer is killed in Drop.
        unsafe {
            let _ = SetTimer(Some(hwnd), TIMER_SAVE, SAVE_INTERVAL_MS, None);
        }

        if !app.config.general.paused {
            app.retile_all();
        }
        app.update_dimming();
        Some(app)
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Add the tray icon, retrying if the shell is not ready yet.
    ///
    /// Without the retry, launching at logon before Explorer has built the
    /// taskbar leaves SuperTile running with no icon and no way to reach its
    /// menu -- indistinguishable from a crash, and only fixable by killing it
    /// from Task Manager.
    fn ensure_tray_icon(&mut self) {
        if self.tray.add(self.config.general.paused) {
            crate::log!("tray icon registered");
            self.tray_retries = 0;
            // SAFETY: cancelling the retry timer only. Killing TIMER_DRAG here
            // would abort a drag in progress -- Explorer restarting mid-drag
            // has nothing to do with the pointer.
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_TRAY_RETRY);
            }
            return;
        }
        if self.tray_retries > 0 {
            self.tray_retries -= 1;
            crate::log!(
                "tray icon not accepted by the shell; {} retries left",
                self.tray_retries
            );
            // SAFETY: hwnd is live; the timer is killed on success and in Drop.
            unsafe {
                let _ = SetTimer(Some(self.hwnd), TIMER_TRAY_RETRY, TRAY_RETRY_MS, None);
            }
        } else {
            crate::log!("giving up on the tray icon");
        }
    }

    // --- hotkeys ----------------------------------------------------------

    /// Claim every configured hotkey, walking each fallback chain.
    ///
    /// Windows exposes no way to ask which application owns a combination, so
    /// the only test is to try to claim it. Each action therefore carries an
    /// ordered chain: the configured key first, then alternatives. The first
    /// one `RegisterHotKey` accepts wins, and anything that had to fall back is
    /// recorded so the user can see what actually happened.
    fn register_hotkeys(&mut self) {
        use windows::Win32::UI::Input::KeyboardAndMouse::HOT_KEY_MODIFIERS;

        let (chains, warns) = self.config.resolve_hotkeys();
        self.warnings.extend(warns);
        self.bindings.clear();
        self.rejected.clear();
        self.fell_back.clear();

        // Two of our own actions must not fight over one combination.
        let mut claimed: HashSet<(u32, u16)> = HashSet::new();

        for candidates in chains {
            let action = candidates.action;
            let mut bound = None;

            for (attempt, hk) in candidates.chain.iter().enumerate() {
                if claimed.contains(&(hk.mods, hk.vk)) {
                    continue;
                }
                // SAFETY: id is the action discriminant, unique per action.
                let ok = unsafe {
                    RegisterHotKey(
                        Some(self.hwnd),
                        action as i32,
                        HOT_KEY_MODIFIERS(hk.mods),
                        hk.vk as u32,
                    )
                };
                if ok.is_ok() {
                    claimed.insert((hk.mods, hk.vk));
                    bound = Some((*hk, attempt));
                    break;
                }
            }

            match bound {
                Some((hk, 0)) => {
                    self.bindings.insert(action, hk);
                }
                Some((hk, _)) => {
                    // A fallback was taken. Deliberately *not* written back to
                    // config: the application that owned the first choice may
                    // have been running only this once, and overwriting the
                    // user's chosen key would make the fallback permanent. The
                    // resolved key is shown in the tray and the shortcut
                    // editor instead.
                    let first = candidates.chain[0];
                    crate::log!("{}: {first} unavailable, using {hk}", action.config_key());
                    self.fell_back.push((action, first, hk));
                    self.bindings.insert(action, hk);
                }
                None => {
                    let first = candidates.chain[0];
                    crate::log!(
                        "{}: no binding available ({} candidates tried)",
                        action.config_key(),
                        candidates.chain.len()
                    );
                    self.rejected.push((action, first));
                }
            }
        }
    }

    fn unregister_hotkeys(&mut self) {
        for action in self.bindings.keys() {
            // SAFETY: unregistering ids we registered; failure is ignorable.
            unsafe {
                let _ = UnregisterHotKey(Some(self.hwnd), *action as i32);
            }
        }
        self.bindings.clear();
    }

    fn install_hook(&mut self) {
        // Several tight ranges rather than one wide one.
        //
        // A single hook from EVENT_SYSTEM_FOREGROUND to EVENT_OBJECT_HIDE also
        // delivers menu, scroll, alert, capture, drag-drop and sound events
        // from every process on the desktop -- thousands of wake-ups a minute
        // that this process filters out and discards. Measured idle CPU with
        // the wide hook was ~2% of a core, which is not what "invisible in Task
        // Manager" means.
        //
        // EVENT_OBJECT_LOCATIONCHANGE is deliberately absent from all of them:
        // it fires continuously during any drag or animation. Live drag
        // feedback is sampled on a timer instead, and only while a drag is
        // actually in progress.
        const RANGES: [(u32, u32); 5] = [
            (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
            (EVENT_SYSTEM_MOVESIZESTART, EVENT_SYSTEM_MOVESIZEEND),
            (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
            (EVENT_OBJECT_CREATE, EVENT_OBJECT_HIDE),
            (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
        ];
        for (lo, hi) in RANGES {
            // SAFETY: the callback is a plain function; OUTOFCONTEXT means it
            // is invoked on our own thread via the message queue, so no code
            // is injected into the observed processes.
            let h = unsafe {
                SetWinEventHook(
                    lo,
                    hi,
                    None,
                    Some(win_event_proc),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                )
            };
            if !h.is_invalid() {
                self.hooks.push(h);
            }
        }
    }

    fn start_app_scan(&self) {
        let host = self.hwnd.0 as isize;
        self.apps.refresh_async(move || {
            // SAFETY: posting to the host window, which lives for the process.
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(host as *mut core::ffi::c_void)),
                    WM_APPS_READY,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        });
    }

    // --- tiling -----------------------------------------------------------

    /// SuperTile's own windows. Never tiled, never listed.
    fn own_windows(&self) -> HashSet<isize> {
        let mut s = HashSet::new();
        s.insert(self.hwnd.0 as isize);
        s.insert(self.palette.hwnd().0 as isize);
        s.insert(self.about.hwnd().0 as isize);
        s.insert(self.keys_window.hwnd().0 as isize);
        s.insert(crate::ui::hover::overlay_hwnd());
        s.insert(crate::ui::hover::tip_hwnd());
        s.insert(self.preview.hwnd().0 as isize);
        s.extend(self.dimmer.own_windows());
        s
    }

    /// Windows the tiler must leave alone: our own, anything the user has
    /// explicitly excluded, and anything that will not accept a size.
    ///
    /// Distinct from [`Self::own_windows`] on purpose. An excluded window is
    /// still a real window the user wants to see in the palette and the tray
    /// list — it is only the *tiling* it opts out of.
    fn excluded_from_tiling(&self) -> HashSet<isize> {
        let mut s = self.own_windows();
        s.extend(self.floated.iter().copied());
        // Deliberately *not* `unmanageable`. A window that will not accept a
        // size still owns its slot: dropping it from the order re-flows every
        // other window into the gap and leaves it floating loose, which is a
        // far worse outcome than the repeated SetWindowPos it was meant to
        // avoid. The placement loop skips nudging it; the layout still counts
        // it.
        s
    }

    pub fn retile_all(&mut self) {
        if self.config.general.paused {
            return;
        }
        let monitors = crate::monitor::enumerate();
        // One enumeration for the whole desktop, not one per monitor.
        let exclude = self.excluded_from_tiling();
        let all = window::enumerate(&self.config, &exclude);
        let fp = crate::monitor::fingerprint(&monitors);
        if fp != self.fingerprint {
            crate::log!(
                "display arrangement changed: {} -> {}",
                self.fingerprint,
                fp
            );
            self.fingerprint = fp;
            // Orders are per-arrangement; a dock/undock invalidates them.
            self.orders.clear();
        }
        for m in &monitors {
            self.retile_monitor_with(m, None, &all);
        }
    }

    fn retile_current(&mut self) {
        // An explicit retile is the user asking us to try again.
        self.forget_placement_failures();
        let mut pt = POINT::default();
        // SAFETY: valid out-param.
        let _ = unsafe { GetCursorPos(&mut pt) };
        if let Some(m) = crate::monitor::from_point(pt.x, pt.y) {
            self.retile_monitor(&m);
        }
    }

    fn retile_monitor(&mut self, m: &Monitor) {
        self.retile_monitor_except(m, None)
    }

    /// Re-tile `m`, optionally leaving one window alone.
    ///
    /// During a resize the user is holding one window; moving it out from
    /// under the pointer fights the drag and makes the whole thing feel broken.
    fn retile_monitor_except(&mut self, m: &Monitor, skip: Option<isize>) {
        let exclude = self.excluded_from_tiling();
        let all = window::enumerate(&self.config, &exclude);
        self.retile_monitor_with(m, skip, &all);
    }

    /// Re-tile `m` from an enumeration the caller already performed.
    fn retile_monitor_with(&mut self, m: &Monitor, skip: Option<isize>, all: &[WindowInfo]) {
        // The guard belongs here, not only in retile_all: retile_current,
        // swap_direction and end_drag all reach this directly, and would
        // otherwise move windows while the tray icon says paused.
        if self.config.general.paused {
            return;
        }
        // Never re-flow the layout underneath a drag. `skip` marks the retiles
        // the drag itself asks for; anything else arriving now came from a
        // window event -- a focus change, a tab closing -- and moving the other
        // cells mid-gesture changes the zones the drop is being resolved
        // against. That is what made the drop target flip between "place left"
        // and "place right" and the window jerk from one cell to another while
        // the button was still held.
        if self.drag.is_some() && skip.is_none() {
            if crate::window::mouse_left_down() {
                return;
            }
            // The button is up, so the gesture is over whatever we were told.
            //
            // This suppression is the most damaging state in the program. While
            // it holds, the layout keeps changing but no window is moved into
            // its new cell, so windows overlap and the desktop appears to
            // freeze -- then catches up all at once when the session finally
            // closes. `poll_drag` also ends a session on button-up, but only
            // while its timer is running, so it cannot be the only guard.
            //
            // Checking the physical button here means the freeze can never
            // outlive it, however the session was orphaned.
            crate::vlog!("clearing an orphaned drag session: the button is up");
            self.stop_drag_timer();
            self.drag = None;
            self.preview.hide();
            self.readout.hide();
            self.grid.hide();
        }
        let mut live = window::tileable_from(all, m);

        // Windows we are forbidden to move take no part in the layout.
        //
        // Reserving a cell for one is reserving a cell that can never be
        // filled: the others tile around a gap while the elevated window floats
        // over them regardless. Left out, that space goes to windows that can
        // use it and the elevated one stays where its own application put it.
        // `reserve_cells_for_elevated` restores the old behaviour for anyone
        // who would rather the layout hold still while an admin console comes
        // and goes.
        //
        // Probed once per window and remembered: the answer cannot change for
        // the life of a window, and repeating it would be a syscall per window
        // per retile.
        let reserve = self.config.general.reserve_cells_for_elevated;
        live.retain(|w| {
            if self.elevated.contains(&w.hwnd) {
                return reserve;
            }
            if !self.checked_elevation.insert(w.hwnd) {
                return true;
            }
            if crate::window::needs_elevation(w.handle()) {
                crate::log!(
                    "'{}' runs as administrator; Windows forbids moving it, so it {}",
                    w.title,
                    if reserve {
                        "keeps a cell it cannot fill"
                    } else {
                        "is left out of the layout"
                    }
                );
                self.elevated.insert(w.hwnd);
                return reserve;
            }
            true
        });

        // Identity keys first, before anything asks what a window is.
        //
        // These used to be recorded at the end of the pass, alongside the
        // rectangles -- which is too late for the one caller that matters.
        // Restoring a saved split layout matches windows by identity, runs
        // earlier in this same pass, and so found an empty map every time: the
        // layout was written faithfully on every save and never once restored.
        for w in &live {
            self.keys.insert(
                w.hwnd,
                crate::memory::make_key(&self.fingerprint, &w.exe, &w.class),
            );
        }

        let key = m.device.clone();
        let params = self.config.layout.params();
        let kind = self.config.layout.kind;

        let fresh = {
            let order = self.orders.entry(key.clone()).or_default();
            order.reconcile(&live)
        };

        // Give newly-appeared windows the slot they had last time, if the
        // layout still has that slot free.
        if self.config.memory.enabled && !fresh.is_empty() {
            let count = self.orders.get(&key).map(|o| o.order.len()).unwrap_or(0);
            for hwnd in fresh {
                let Some(info) = live.iter().find(|w| w.hwnd == hwnd) else {
                    continue;
                };
                let mkey = crate::memory::make_key(&self.fingerprint, &info.exe, &info.class);
                if let Some(Suggestion::Zone(i)) =
                    self.store.suggest(&mkey, kind, count, m.work_area)
                {
                    if let Some(order) = self.orders.get_mut(&key) {
                        order.move_to(hwnd, i);
                    }
                }
            }
        }

        // Reassigned below when the tree owns the ordering.
        let mut order = self
            .orders
            .get(&key)
            .map(|o| o.order.clone())
            .unwrap_or_default();
        if order.is_empty() {
            return;
        }
        let zones = if kind == LayoutKind::Bsp {
            // The tree owns the geometry; reconcile it with the live windows
            // first so a closed window collapses its split.
            // A restore has to happen before reconcile, which would otherwise
            // seed an arbitrary tree from the window order and leave nothing
            // for the saved one to attach to.
            if !self.trees.contains_key(&key) {
                self.restore_tree(&key, &order, m.work_area);
            }
            let tree = self.trees.entry(key.clone()).or_default();
            tree.reconcile(&order, m.work_area);
            let placed = tree.layout(m.work_area);
            // Re-order `order` to match the tree, so index-based operations
            // (focus, swap, memory) keep agreeing with what is on screen.
            let tree_order: Vec<isize> = placed.iter().map(|p| p.hwnd).collect();
            if let Some(o) = self.orders.get_mut(&key) {
                o.order = tree_order.clone();
            }
            order = tree_order;
            let inner = params.inner_gap / 2;
            placed
                .into_iter()
                .map(|p| p.rect.deflate(params.outer_gap.max(inner)))
                .collect()
        } else {
            let splits = self.splits_for(&key, kind).clone();
            layout::compute_with(m.work_area, order.len(), kind, &params, &splits)
        };

        // Maximised windows ignore SetWindowPos sizing, so restore them first --
        // unless the user asked for the opposite.
        //
        // Maximising with Shift held means genuine fullscreen: the whole
        // monitor, over the taskbar, rather than Windows' idea of maximised,
        // which fits the work area and keeps the frame. The window leaves the
        // grid while it is like that, because a fullscreen window the tiler
        // still owns would be dragged back into its cell on the next pass.
        let shift = crate::window::shift_held();
        let mut detached: Vec<isize> = Vec::new();
        for hwnd in &order {
            if skip == Some(*hwnd) {
                continue;
            }
            let Some(w) = live.iter().find(|w| w.hwnd == *hwnd) else {
                continue;
            };
            if shift && crate::window::is_maximised(w.handle()) {
                crate::window::set_fullscreen(w.handle(), m.bounds);
                if self.floated.insert(w.hwnd) {
                    crate::log!("'{}' made fullscreen (Shift+maximise)", w.title);
                }
                detached.push(w.hwnd);
                continue;
            }
            window::restore_if_maximized(w.handle());
        }
        if !detached.is_empty() {
            // Recompute without them, so the rest share the whole area rather
            // than tiling around a window that has just left.
            if let Some(o) = self.orders.get_mut(&key) {
                o.order.retain(|h| !detached.contains(h));
            }
            order.retain(|h| !detached.contains(h));
        }

        let mut placements: Vec<(&WindowInfo, Rect)> = Vec::with_capacity(order.len());
        for (i, hwnd) in order.iter().enumerate() {
            // The window the user is dragging is excluded: moving it out from
            // under the pointer fights the drag at 60 Hz.
            if skip == Some(*hwnd) {
                continue;
            }
            let Some(w) = live.iter().find(|w| w.hwnd == *hwnd) else {
                continue;
            };
            let Some(zone) = zones.get(i) else { continue };
            // Grow the cell to the window's own floor before asking. A cell
            // narrower than the minimum is a request the window will refuse,
            // and three refusals used to get it written off entirely.
            let min = self.min_size_of(w.hwnd);
            placements.push((w, layout::fit_to_minimum(*zone, min, m.work_area)));
        }

        if crate::util::verbose_enabled() {
            let current: HashSet<isize> = placements.iter().map(|(w, _)| w.hwnd).collect();
            for w in &live {
                if self.last_managed.contains(&w.hwnd) && !current.contains(&w.hwnd) {
                    // Still on screen, no longer being tiled: the case Signal
                    // and friends hit. Say why, not just that.
                    crate::vlog!(
                        "LEFT the layout: '{}' ({}) class {} -- floated={} stubborn={} in_order={}",
                        w.title,
                        w.exe.rsplit(['\\', '/']).next().unwrap_or(""),
                        w.class,
                        self.floated.contains(&w.hwnd),
                        self.unmanageable.contains_key(&w.hwnd),
                        order.contains(&w.hwnd)
                    );
                }
            }
            for (w, zone) in &placements {
                if !self.last_managed.contains(&w.hwnd) {
                    crate::vlog!(
                        "JOINED the layout: '{}' ({}) at {}x{}",
                        w.title,
                        w.exe.rsplit(['\\', '/']).next().unwrap_or(""),
                        zone.width(),
                        zone.height()
                    );
                }
            }
            self.last_managed = current;
        }

        // Notice windows that did not go where they were put last time.
        //
        // A window with a hard minimum, or one its own application is dragging,
        // clamps whatever it is given and never matches its zone -- so it would
        // be re-issued a SetWindowPos on every pass forever.
        let now_ms = crate::util::tick_ms();

        // Not while a drag is running. A resize re-tiles every 16ms, and a
        // window being dragged by its own application (tearing a Chrome tab off
        // into a new window, say) ignores every move until the drop. Counting
        // those as refusals condemned windows for being in motion, which is
        // exactly when they cannot comply and exactly when it means nothing.
        let mut adopt: Option<isize> = None;
        let pointer = {
            let mut p = POINT::default();
            // SAFETY: valid out-param.
            let _ = unsafe { GetCursorPos(&mut p) };
            p
        };
        if self.drag.is_none() {
            for (w, _) in &placements {
                let Some(want) = self.requested.get(&w.hwnd).copied() else {
                    continue;
                };
                if self.unmanageable.contains_key(&w.hwnd) {
                    continue;
                }
                let missed = (w.rect.left - want.left).abs() > PLACEMENT_TOLERANCE
                    || (w.rect.top - want.top).abs() > PLACEMENT_TOLERANCE
                    || (w.rect.right - want.right).abs() > PLACEMENT_TOLERANCE
                    || (w.rect.bottom - want.bottom).abs() > PLACEMENT_TOLERANCE;
                if !missed {
                    self.misses.remove(&w.hwnd);
                    continue;
                }
                // Some toolkits resize their own windows without ever raising
                // MOVESIZESTART -- GTK and Chromium's custom frame both do --
                // so the user drags an edge, the window obeys, and the next
                // retile puts it straight back. GIMP could not be grown at all
                // for this reason.
                //
                // The test is that the window moved *since the last pass*, not
                // merely that it differs from what was requested. A window
                // whose minimum exceeds its cell differs from its request
                // permanently and is not moving; conflating the two is what
                // made an earlier attempt at this suppress re-tiling on every
                // click.
                // Either the window moved, or the pointer did while resting on
                // one of this window's resize borders.
                //
                // Keying on the window alone misses the applications that need
                // this most: WinUI, Chromium and GTK windows do not move while
                // their border is dragged, so nothing about them changes and no
                // drag is ever detected -- the retile simply puts them back.
                // A held button, a moving pointer and a hit test that says
                // "border" is a drag whatever the window does about it.
                let window_moved = self
                    .last_rects
                    .get(&w.hwnd)
                    .is_some_and(|prev| *prev != w.rect);
                let pointer_moved = self.last_pointer != (pointer.x, pointer.y);
                let on_border = pointer_moved
                    && crate::window::hit_test(
                        HWND(w.hwnd as *mut core::ffi::c_void),
                        pointer.x,
                        pointer.y,
                    )
                    .map(drag::grabbed_edges)
                    .is_some_and(|e| e != (false, false, false, false));
                let moved_since_last_pass = window_moved || on_border;
                // ...and the pointer must be on this window. A resize moves
                // every window on the monitor, so "moved since the last pass"
                // alone matches the neighbours as readily as the window being
                // dragged -- and adopting a neighbour opens a session for the
                // wrong window, which since 0.15.5 freezes re-tiling entirely.
                // The grab band is generous because an edge drag puts the
                // cursor a few pixels outside the frame.
                const GRAB_SLOP: i32 = 12;
                let under_pointer = pointer.x >= w.rect.left - GRAB_SLOP
                    && pointer.x <= w.rect.right + GRAB_SLOP
                    && pointer.y >= w.rect.top - GRAB_SLOP
                    && pointer.y <= w.rect.bottom + GRAB_SLOP;
                if moved_since_last_pass && under_pointer && crate::window::mouse_left_down() {
                    crate::vlog!(
                        "'{}' is moving under a held button with no MOVESIZESTART; adopting the drag",
                        w.title
                    );
                    adopt = Some(w.hwnd);
                    break;
                }

                let entry = self.misses.entry(w.hwnd).or_insert((0, 0));
                if now_ms.saturating_sub(entry.1) < MISS_INTERVAL_MS {
                    // Same moment, not a second opinion.
                    continue;
                }
                entry.0 = entry.0.saturating_add(1);
                entry.1 = now_ms;
                let n = entry.0;
                crate::vlog!(
                    "miss {}/{}: '{}' asked for {}x{} at {},{} -- it is {}x{} at {},{}",
                    n,
                    MAX_PLACEMENT_MISSES,
                    w.title,
                    want.width(),
                    want.height(),
                    want.left,
                    want.top,
                    w.rect.width(),
                    w.rect.height(),
                    w.rect.left,
                    w.rect.top
                );
                if n >= MAX_PLACEMENT_MISSES {
                    crate::log!(
                        "'{}' ({}) will not accept a size -- asked for {}x{}, it stayed {}x{}; pausing for {}s",
                        w.title,
                        w.exe.rsplit(['\\', '/']).next().unwrap_or(""),
                        want.width(),
                        want.height(),
                        w.rect.width(),
                        w.rect.height(),
                        STUBBORN_RETRY_MS / 1000
                    );
                    self.unmanageable.insert(w.hwnd, now_ms);
                }
            }
        }

        self.last_pointer = (pointer.x, pointer.y);

        // Remember where everything sat, so the next pass can tell a window in
        // motion from one that is merely misplaced, and what each window is, so
        // a split tree can be saved by identity.
        for (w, _) in &placements {
            self.last_rects.insert(w.hwnd, w.rect);
        }

        if let Some(hwnd) = adopt {
            self.begin_drag(HWND(hwnd as *mut core::ffi::c_void));
            // The window is under the pointer; moving it now is the fight this
            // exists to end.
            return;
        }

        // Offer a written-off window another chance once the wait is up.
        let expired: Vec<isize> = self
            .unmanageable
            .iter()
            .filter(|(_, since)| now_ms.saturating_sub(**since) >= STUBBORN_RETRY_MS)
            .map(|(h, _)| *h)
            .collect();
        for h in expired {
            self.unmanageable.remove(&h);
            self.misses.remove(&h);
            crate::vlog!("giving {h:?} another chance at being placed");
        }

        let stubborn: HashSet<isize> = self.unmanageable.keys().copied().collect();
        placements.retain(|(w, _)| !stubborn.contains(&w.hwnd));

        // Only touch windows that are not already where they belong. A retile
        // triggered by unrelated shell activity is the common case, and issuing
        // SetWindowPos for a window that is already correct still costs a
        // round-trip to that window's message loop.
        let moves: Vec<(&WindowInfo, Rect)> = placements
            .iter()
            .filter(|(w, zone)| {
                let target = w.target_rect_for(*zone);
                (w.rect.left - target.left).abs() > 1
                    || (w.rect.top - target.top).abs() > 1
                    || (w.rect.right - target.right).abs() > 1
                    || (w.rect.bottom - target.bottom).abs() > 1
            })
            .copied()
            .collect();

        if !moves.is_empty() {
            let failed = window::apply(&moves);
            if failed > 0 {
                // SetWindowPos itself refusing is a different failure from a
                // window clamping the size it was given; do not conflate them.
                crate::log!("{failed} window(s) rejected SetWindowPos");
            }
            for (w, zone) in &moves {
                let target = w.target_rect_for(*zone);
                // Asking for a different rectangle is asking a different
                // question, so the tally of refusals starts again. Otherwise a
                // window that could not fit one layout stays condemned in every
                // layout after it.
                if self.requested.insert(w.hwnd, target) != Some(target) {
                    self.misses.remove(&w.hwnd);
                    self.unmanageable.remove(&w.hwnd);
                }
            }
        }

        // Windows have moved: the dim overlays must be re-stacked, and the
        // shell layer's cut-out for the bright window re-shaped, or the dim
        // stops matching the tiles it is supposed to be behind.
        self.refresh_dimming_after_layout();

        if self.config.memory.enabled {
            let max = self.config.memory.max_entries as usize;
            for (i, (w, zone)) in placements.iter().enumerate() {
                let mkey = crate::memory::make_key(&self.fingerprint, &w.exe, &w.class);
                self.store.remember(
                    mkey,
                    crate::memory::Placement {
                        zone_index: i,
                        zone_count: zones.len(),
                        layout: kind,
                        rect: *zone,
                        work_area: m.work_area,
                        device: &m.device,
                    },
                    max,
                );
            }
            self.store_dirty = true;
        }
    }

    /// Stored split positions for a monitor and layout, or the empty default.
    fn splits_for(&self, device: &str, kind: LayoutKind) -> &Splits {
        static EMPTY: std::sync::OnceLock<Splits> = std::sync::OnceLock::new();
        self.splits
            .get(&(device.to_string(), kind))
            .unwrap_or_else(|| EMPTY.get_or_init(Splits::default))
    }

    /// Zones as currently laid out on `m`, and the window order behind them.
    fn zones_of(&self, m: &Monitor) -> (Vec<isize>, Vec<Rect>) {
        let order = self
            .orders
            .get(&m.device)
            .map(|o| o.order.clone())
            .unwrap_or_default();
        if order.is_empty() {
            return (order, Vec::new());
        }
        let kind = self.config.layout.kind;
        let params = self.config.layout.params();

        if kind == LayoutKind::Bsp {
            if let Some(tree) = self.trees.get(&m.device) {
                let placed = tree.layout(m.work_area);
                if !placed.is_empty() {
                    let inner = params.inner_gap / 2;
                    return (
                        placed.iter().map(|p| p.hwnd).collect(),
                        placed
                            .iter()
                            .map(|p| p.rect.deflate(params.outer_gap.max(inner)))
                            .collect(),
                    );
                }
            }
        }
        let zones = layout::compute_with(
            m.work_area,
            order.len(),
            kind,
            &params,
            self.splits_for(&m.device, kind),
        );
        (order, zones)
    }

    // --- interactive drag --------------------------------------------------

    fn begin_drag(&mut self, hwnd: HWND) {
        // Any previous session is finished first. Windows does not guarantee a
        // MOVESIZEEND for every MOVESIZESTART, and a session left open is
        // committed against the next drag.
        if self.drag.is_some() {
            self.end_drag();
        }
        let key = hwnd.0 as isize;
        if self.config.general.paused || self.own_windows().contains(&key) {
            return;
        }
        let Some(m) = crate::monitor::from_window(hwnd) else {
            return;
        };
        let (order, zones) = self.zones_of(&m);
        // A detached window has no cell, but it must still be draggable --
        // otherwise Shift is a one-way door. Without a session there is no
        // end_drag, and the drag that was meant to put it back never happens.
        let detached = self.floated.contains(&key);
        let (index, zone) = match order.iter().position(|h| *h == key) {
            Some(i) => (i, zones.get(i).copied().unwrap_or_default()),
            None if detached => (0, crate::window::visible_frame(hwnd)),
            // Genuinely nothing to do with us: an ignored window.
            None => return,
        };

        self.drag = Some(DragSession {
            hwnd: key,
            device: m.device.clone(),
            zone_index: index,
            zone,
            start: crate::window::visible_frame(hwnd),
            drop: None,
            detached,
            title: crate::window::title_of(hwnd),
            min: crate::window::min_size(hwnd),
            hit: {
                let mut pt = POINT::default();
                // SAFETY: valid out-param.
                let _ = unsafe { GetCursorPos(&mut pt) };
                crate::window::hit_test(hwnd, pt.x, pt.y).unwrap_or(0)
            },
            grabbed: {
                // Ask before the drag has moved anything: the pointer is still
                // on the part of the window the user pressed.
                let mut pt = POINT::default();
                // SAFETY: valid out-param.
                let _ = unsafe { GetCursorPos(&mut pt) };
                crate::window::hit_test(hwnd, pt.x, pt.y).and_then(drag::grab_kind)
            },
            last_applied: None,
        });
        // SAFETY: hwnd is live; the timer is killed in end_drag and in Drop.
        unsafe {
            let _ = SetTimer(Some(self.hwnd), TIMER_DRAG, DRAG_POLL_MS, None);
        }
    }

    /// One poll while a drag is in progress.
    fn poll_drag(&mut self) {
        let Some(session) = self.drag.as_ref() else {
            self.stop_drag_timer();
            return;
        };
        let hwnd = HWND(session.hwnd as *mut core::ffi::c_void);
        if !crate::window::is_live(hwnd) {
            self.end_drag();
            return;
        }
        // A session must never outlive the button. Chrome's tab drag does not
        // always send MOVESIZEEND, and since 0.15.5 an open session freezes
        // every event-driven retile -- so a session left behind would stop
        // tiling altogether and let windows overlap. The physical button is the
        // ground truth: if it is up, the gesture is over whatever Windows said.
        if !crate::window::mouse_left_down() {
            crate::vlog!("drag ended without a MOVESIZEEND; closing the session");
            self.end_drag();
            return;
        }

        // A detached window is being moved by Windows, not by us. Nothing about
        // the layout should follow it around; the decision is made at the drop.
        if session.detached {
            return;
        }

        let now = crate::window::visible_frame(hwnd);
        // What the user grabbed settles it. Re-deciding from the rectangle each
        // frame flickers between move and resize while a left or top edge is
        // dragged, because that changes the origin as well as the size -- which
        // is how the drop overlay appeared in the middle of a resize.
        crate::vlog!(
            "drag poll: grabbed={:?} rect {}x{} at {},{}",
            session.grabbed,
            now.width(),
            now.height(),
            now.left,
            now.top
        );
        let kind = session
            .grabbed
            .unwrap_or_else(|| drag::classify(session.start, now, drag::EDGE_THRESHOLD));
        match kind {
            DragKind::Resize => self.drag_resize(now),
            DragKind::Move => self.drag_move(),
        }
    }

    /// Live resize: move the boundary and re-tile everyone except the window
    /// the user is holding.
    fn drag_resize(&mut self, now: Rect) {
        let Some(session) = self.drag.as_ref() else {
            return;
        };
        let device = session.device.clone();
        let index = session.zone_index;
        let zone = session.zone;
        let dragged = session.hwnd;
        let min = session.min;
        let edges = drag::grabbed_edges(session.hit);

        // The preview belongs to moves; a resize shows itself.
        if session.drop.is_some() {
            self.preview.hide();
            if let Some(s) = self.drag.as_mut() {
                s.drop = None;
            }
        }

        let Some(m) = crate::monitor::enumerate()
            .into_iter()
            .find(|x| x.device == device)
        else {
            return;
        };
        let count = self.orders.get(&device).map(|o| o.order.len()).unwrap_or(0);
        if count < 2 {
            return;
        }
        // Take the dragged edges from the pointer, not from the window.
        //
        // Chromium, Electron and GTK windows do not resize themselves while a
        // border is being dragged: the rectangle is identical on every poll for
        // the whole gesture, so a boundary derived from it never moves. Worse,
        // a window sitting off its cell -- Discord at 1491px in a 1276px zone --
        // makes that constant offset look like an enormous drag, which is then
        // rejected for squeezing a neighbour. Neither effect exists if the
        // edges come from the cursor, which is the thing actually moving.
        //
        // Windows that do resize themselves are unaffected: their edge is under
        // the cursor anyway, so the two agree.
        let now = if edges != (false, false, false, false) {
            let mut pt = POINT::default();
            // SAFETY: valid out-param.
            let _ = unsafe { GetCursorPos(&mut pt) };
            // Anchored on the zone, not on the window's own rectangle.
            //
            // Everything downstream -- which boundary an edge belongs to, and
            // the fraction it becomes -- is expressed relative to the zone. A
            // rectangle anchored anywhere else arrives in a different
            // coordinate space, and the edges that were not dragged come out
            // displaced by however far the window sits from its cell. That
            // showed up as a resize working in one direction and not the other.
            drag::edges_from_pointer(zone, edges, pt.x, pt.y)
        } else {
            now
        };

        // A window with a minimum size does not shrink past it -- it clamps,
        // and then sits wider than the cell it was given, overlapping its
        // neighbour. Clamping the boundary here means the layout never asks
        // for the impossible, so nothing springs back.
        let (min_w, min_h) = min;
        let now = drag::clamp_to_minimum(zone, now, min_w, min_h);
        let at_minimum = now.width() <= min_w || now.height() <= min_h;

        // The readout going amber with nothing in the log to explain it is the
        // state to catch here: it means the boundary stopped for a reason the
        // resize path never recorded. Print the whole comparison, not the
        // verdict, so a wrong minimum is as visible as a real limit.
        crate::vlog!(
            "resize '{}': now {}x{} min {}x{} zone {}x{} at_minimum={} kind={:?}",
            crate::window::title_of(HWND(dragged as *mut core::ffi::c_void)),
            now.width(),
            now.height(),
            min_w,
            min_h,
            zone.width(),
            zone.height(),
            at_minimum,
            self.config.layout.kind
        );

        // Live pixel size over the window, when asked for.
        self.show_size_readout(now, at_minimum, m.dpi);

        let kind = self.config.layout.kind;
        let params = self.config.layout.params();

        if kind == LayoutKind::Bsp {
            let before = self.squeeze(&m);
            let previous = self.trees.get(&device).cloned();
            self.resize_tree(&device, dragged, zone, now, m.work_area);
            if self.squeeze(&m) > before {
                // Put the boundary back and stop following the pointer. The
                // alternative is to overrun a neighbour's minimum, and a window
                // that has been overrun does not politely stay put.
                match previous {
                    Some(t) => {
                        self.trees.insert(device.clone(), t);
                    }
                    None => {
                        self.trees.remove(&device);
                    }
                }
                self.readout.set_warning(true);
                return;
            }
            self.show_resize_grid(&m);
            self.retile_monitor_except(&m, Some(dragged));
            return;
        }

        // Fractions must be measured in the same space the layout engine reads
        // them in: inside the outer gap, and before the per-zone inner-gap
        // deflation. Undo both, or the edge lands short of where it was
        // dropped -- by outer_gap/2 at the left wall, growing with distance.
        let area = m.work_area.deflate(params.outer_gap);
        let g = params.inner_gap / 2;
        let undeflated = Rect::new(now.left - g, now.top - g, now.right + g, now.bottom + g);

        // Every axis the drag touched, so a corner resizes in both directions
        // rather than only the one that happened to move furthest.
        let edits = drag::best_edits(
            kind,
            index,
            count,
            params.master_count as usize,
            area,
            zone,
            undeflated,
            drag::EDGE_THRESHOLD,
        );
        let Some(lead) = edits.first().copied() else {
            return;
        };

        // Ignore sub-pixel jitter: below this the layout would not visibly
        // change, and the retile would be pure cost.
        // Keyed on the leading edit: a stationary pointer moves neither axis.
        const MIN_DELTA: f32 = 0.0015;
        if let Some((axis, index, last)) = self.drag.as_ref().and_then(|s| s.last_applied) {
            if axis == lead.axis && index == lead.index && (last - lead.fraction).abs() < MIN_DELTA
            {
                return;
            }
        }
        if let Some(sess) = self.drag.as_mut() {
            sess.last_applied = Some((lead.axis, lead.index, lead.fraction));
        }

        let key = (device.clone(), kind);
        let squeeze_before = self.squeeze(&m);
        let previous = self.splits.get(&key).cloned();

        // Walk the movement back until it stops squeezing a neighbour past its
        // minimum, rather than refusing it outright. Every boundary has two
        // sides, and the far side hitting its floor is a limit, not an error --
        // but a boundary that simply stops following the pointer with nothing
        // applied reads as "resizing does not work", which is what this looked
        // like in practice.
        let mut applied = false;
        for t in drag::CLAMP_STEPS {
            let probe = drag::lerp_rect(zone, undeflated, t);
            let step = drag::best_edits(
                kind,
                index,
                count,
                params.master_count as usize,
                area,
                zone,
                probe,
                drag::EDGE_THRESHOLD,
            );
            if step.is_empty() {
                continue;
            }
            // Each probe starts from the layout as it was, so a rejected larger
            // step leaves nothing behind for the next one to build on.
            match previous.clone() {
                Some(p) => {
                    self.splits.insert(key.clone(), p);
                }
                None => {
                    self.splits.remove(&key);
                }
            }
            let entry = self.splits.entry(key.clone()).or_default();
            for edit in &step {
                match edit.grid_row {
                    // Grid columns belong to their row; anything else is global.
                    Some(row) => entry.set_grid_column(row, edit.index, edit.fraction, edit.count),
                    None => entry.set(edit.axis, edit.index, edit.fraction, edit.count),
                }
            }
            if self.squeeze(&m) <= squeeze_before {
                applied = true;
                // Amber while short of the pointer, so a boundary that has
                // stopped against a limit says so instead of looking stuck.
                self.readout.set_warning(t < 1.0);
                if t < 1.0 {
                    crate::vlog!(
                        "resize clamped to {:.0}% of the drag: a neighbour is at its minimum",
                        t * 100.0
                    );
                }
                break;
            }
        }
        if !applied {
            match previous {
                Some(p) => {
                    self.splits.insert(key, p);
                }
                None => {
                    self.splits.remove(&key);
                }
            }
            crate::vlog!("resize refused: no part of it fits the windows' minimum sizes");
            self.readout.set_warning(true);
            return;
        }

        // Master + Stack keeps its headline number in the config so the tray
        // and the config file agree with what the user just dragged.
        if kind == LayoutKind::MasterStack {
            if let Some(e) = edits
                .iter()
                .find(|e| e.axis == crate::layout::SplitAxis::Main && e.index == 0)
            {
                self.config.layout.master_fraction = e.fraction.clamp(0.15, 0.85);
            }
        }

        self.show_resize_grid(&m);
        self.retile_monitor_except(&m, Some(dragged));
    }

    /// Outline every cell of the monitor being dragged on.
    ///
    /// Drawn from the same zones the tiler is about to apply, so the outlines
    /// and the windows cannot disagree.
    /// Open the system colour chooser for one part of the custom theme.
    ///
    /// The Windows chooser rather than one of our own: it already has the
    /// spectrum, the saturation ramp, the hex field and the custom swatches,
    /// and it is the dialog every other Windows application uses for this. A
    /// hand-drawn picker would be worse and would need maintaining.
    ///
    /// Picking a colour also switches to the custom theme, since editing a
    /// theme you are not using and seeing nothing change is a puzzle rather
    /// than a feature.
    fn pick_theme_colour(&mut self, which: crate::tray::ThemeColour) {
        use crate::tray::ThemeColour;
        let custom = &self.config.appearance.custom_theme;
        let current = match which {
            ThemeColour::Accent => &custom.accent,
            ThemeColour::Warning => &custom.warning,
            ThemeColour::Text => &custom.text,
        };
        let initial = crate::ui::theme::parse_hex(current)
            .unwrap_or(crate::ui::theme::OVERLAY_WINDOWS.accent);

        let Some(picked) = crate::ui::choose_colour(self.hwnd, initial) else {
            return;
        };
        let hex = crate::ui::theme::to_hex(picked);
        let custom = &mut self.config.appearance.custom_theme;
        match which {
            ThemeColour::Accent => custom.accent = hex,
            ThemeColour::Warning => custom.warning = hex,
            ThemeColour::Text => custom.text = hex,
        }
        self.config.appearance.overlay_theme = "custom".to_string();
        self.apply_overlay_theme();
        let _ = self.config.save();
    }

    /// Rename the custom theme.
    ///
    /// Opens the config file rather than prompting: Win32 has no stock text
    /// dialog, and a hand-built one is more window than a single string is
    /// worth. The name lives under `appearance.custom_theme.name` and the tray
    /// picks it up on save.
    fn rename_custom_theme(&mut self) {
        let _ = self.config.save();
        if let Ok(dir) = crate::util::data_dir() {
            crate::ui::about::open_url(&dir.join("config.json").to_string_lossy());
        }
    }

    /// Re-read the overlay theme after the config has changed on disk.
    fn apply_overlay_theme(&self) {
        let name = &self.config.appearance.overlay_theme;
        let o = if name.eq_ignore_ascii_case("custom") {
            crate::ui::theme::overlay_from_custom(&self.config.appearance.custom_theme)
        } else {
            crate::ui::theme::overlay_by_name(name)
        };
        self.preview.set_overlay(o);
        self.readout.set_overlay(o);
        self.grid.set_overlay(o);
    }

    fn show_resize_grid(&mut self, m: &Monitor) {
        if !self.config.appearance.show_grid_on_resize {
            return;
        }
        let (_, zones) = self.zones_of(m);
        self.grid.show_grid(&zones, m.work_area, m.dpi);
    }

    /// How badly this monitor's current layout violates its occupants\
    /// minimum sizes, in pixels. Zero when everything fits.
    ///
    /// Clamping the *dragged* rectangle only protects the window under the
    /// pointer. Every boundary has two sides, and growing one cell shrinks its
    /// neighbour -- which then clamps itself, overflows and overlaps, exactly
    /// the mess the clamp was added to prevent.
    ///
    /// Judging the finished layout rather than the dragged edge is what makes
    /// this layout-agnostic: grid columns, master fractions and tree ratios are
    /// all covered by one rule, including drags that move two boundaries at
    /// once.
    fn squeeze(&mut self, m: &Monitor) -> i64 {
        let (order, zones) = self.zones_of(m);
        let mins: Vec<(i32, i32)> = order.iter().map(|h| self.min_size_of(*h)).collect();
        layout::squeeze_deficit(&zones, &mins)
    }

    /// Show the live pixel size over the middle of the dragged window.
    ///
    /// Amber once an edge has hit the window's own minimum, so "it stopped
    /// following the pointer" reads as a limit rather than as a bug.
    fn show_size_readout(&mut self, r: Rect, at_minimum: bool, dpi: u32) {
        if !self.config.appearance.show_size_readout {
            return;
        }
        let text = format!("{} x {}", r.width(), r.height());
        // A chip, not a wash: the point is to read the number against the
        // window, not to tint the window.
        let w = (170 * dpi as i32 / 96).min(r.width());
        let h = (46 * dpi as i32 / 96).min(r.height());
        let chip = Rect::new(
            r.left + (r.width() - w) / 2,
            r.top + (r.height() - h) / 2,
            r.left + (r.width() - w) / 2 + w,
            r.top + (r.height() - h) / 2 + h,
        );
        self.readout.set_warning(at_minimum);
        self.readout.set_label(&text);
        self.readout.show_around(chip, dpi);
    }

    /// Move the tree boundary the dragged edge belongs to.
    ///
    /// The tree resolves which split that is by walking to the innermost one
    /// of the right orientation that encloses the window -- there is no index
    /// arithmetic to get wrong, unlike the parametric path.
    fn resize_tree(&mut self, device: &str, hwnd: isize, zone: Rect, now: Rect, area: Rect) {
        let Some(tree) = self.trees.get_mut(device) else {
            return;
        };
        let t = drag::EDGE_THRESHOLD;
        for edge in drag::moved_edges(zone, now, t) {
            // `want_second` is which side of the boundary this window is on: a
            // left or top edge is shared with whatever precedes it, so the
            // window is the second child of that split.
            let (orientation, pos, want_second) = match edge {
                drag::Edge::Left => (crate::tree::Orientation::Horizontal, now.left, true),
                drag::Edge::Right => (crate::tree::Orientation::Horizontal, now.right, false),
                drag::Edge::Top => (crate::tree::Orientation::Vertical, now.top, true),
                drag::Edge::Bottom => (crate::tree::Orientation::Vertical, now.bottom, false),
            };
            tree.set_ratio(hwnd, orientation, area, pos, want_second);
        }
    }

    /// Live move: show where the window would land, and what would happen.
    ///
    /// The overlay is not just a destination. Dropping on the middle 60% of an
    /// edge inserts a new column or row there; the centre swaps. The band and
    /// its caption say which, before the button is released.
    fn drag_move(&mut self) {
        let Some(session) = self.drag.as_ref() else {
            return;
        };
        let device = session.device.clone();
        let from = session.zone_index;
        let previous = session.drop;
        // The session borrow ends here; everything below needs `&mut self`.
        let session_hwnd = session.hwnd;

        let mut pt = POINT::default();
        // SAFETY: valid out-param.
        let _ = unsafe { GetCursorPos(&mut pt) };
        let Some(m) = crate::monitor::from_point(pt.x, pt.y) else {
            return;
        };
        if m.device != device {
            crate::vlog!(
                "drag left its monitor: started on {device}, pointer is on {}",
                m.device
            );
            // The window has left the monitor it started on. Clear the drop as
            // well as hiding the preview: leaving it set makes end_drag act on
            // the source monitor.
            self.preview.hide();
            if let Some(sess) = self.drag.as_mut() {
                sess.drop = None;
            }
            return;
        }

        let (_, zones) = self.zones_of(&m);
        // Show the grid while moving too, not only while resizing. Without it
        // the drop caption names a direction with nothing to refer it to, and
        // "place left" is meaningless until you can see which cell is being
        // divided.
        self.show_resize_grid(&m);
        let raw = drag::drop_action(&zones, pt.x, pt.y);
        if raw.is_none() {
            crate::vlog!(
                "no drop resolved at {},{} against {} zone(s) on {}",
                pt.x,
                pt.y,
                zones.len(),
                m.device
            );
        }
        let resolved = raw.filter(|d| d.target != from);
        if raw.is_some() && resolved.is_none() {
            // The drop resolved onto the slot the drag started from and was
            // discarded. For a window torn out of another one that slot index
            // is inherited from a layout the window was never part of, so a
            // perfectly good drop can be thrown away here.
            crate::vlog!(
                "drop on zone {:?} discarded: it matches the source slot {}",
                raw.map(|d| d.target),
                from
            );
        }

        if resolved == previous {
            return;
        }
        match resolved {
            Some(d) => {
                let ratio = if d.action == drag::DropAction::Swap {
                    None
                } else {
                    self.split_ratio_for(
                        &device,
                        session_hwnd,
                        d.target,
                        d.side,
                        d.action == drag::DropAction::InsertBefore,
                    )
                };
                let blocked = d.action != drag::DropAction::Swap && ratio.is_none();
                // Saying "Place right" and then doing nothing is worse than
                // saying the split will not fit.
                self.preview.set_warning(blocked);
                let label = match (blocked, ratio) {
                    (true, _) => "Too small to split".to_string(),
                    // Worth saying only when the boundary is visibly off
                    // centre; every split reading "50%" is noise.
                    (false, Some(r)) if (r - 0.5).abs() > 0.02 => {
                        let mine = if d.action == drag::DropAction::InsertBefore {
                            r
                        } else {
                            1.0 - r
                        };
                        format!("{} ({:.0}%)", d.label(), mine * 100.0)
                    }
                    _ => d.label().to_string(),
                };
                self.preview.set_label(&label);
                self.preview.show_around(d.highlight, m.dpi);
            }
            None => self.preview.hide(),
        }
        if let Some(sess) = self.drag.as_mut() {
            sess.drop = resolved;
        }
    }

    /// Finish a drag. `hwnd` is the window Windows reported, so a stale
    /// session belonging to a different window is not committed against it.
    fn end_drag_for(&mut self, hwnd: Option<isize>) {
        if let (Some(reported), Some(active)) = (hwnd, self.drag.as_ref()) {
            if reported != active.hwnd {
                // A MOVESIZEEND for a window we were not tracking. Dropping the
                // stale session is right: committing it would swap windows the
                // user never touched.
                crate::log!("drag end for an untracked window; discarding session");
                self.stop_drag_timer();
                self.preview.hide();
                self.readout.hide();
                self.grid.hide();
                self.drag = None;
                return;
            }
        }
        self.end_drag();
    }

    fn end_drag(&mut self) {
        self.stop_drag_timer();
        self.preview.hide();
        self.readout.hide();
        self.grid.hide();
        let Some(mut session) = self.drag.take() else {
            return;
        };

        // Tearing a tab out of Chrome replaces the window mid-drag: the handle
        // the drag began with is the window the tab *left*, and the thing under
        // the pointer is a new one that did not exist when the gesture started.
        // Acting on the original handle put the wrong window into the chosen
        // cell and left the new one to be placed by the next retile, which drops
        // it into the largest free space -- the top right. Whatever holds focus
        // at the drop is the window the user was carrying.
        if let Some(fg) = crate::window::foreground() {
            let fg_hwnd = fg.0 as isize;
            if fg_hwnd != session.hwnd
                && !self.floated.contains(&fg_hwnd)
                && crate::window::is_live(fg)
            {
                crate::vlog!(
                    "drag ended on a different window than it began with ({:?} -> {:?});                      the drag was carrying a torn-off window",
                    session.hwnd,
                    fg_hwnd
                );
                session.hwnd = fg_hwnd;
            }
        }

        // Shift means "leave it where I put it", as it does in FancyZones.
        //
        // The window is excluded from tiling and stays exactly where it was
        // dropped. Dragging it again without Shift puts it back under the
        // tiler, so the detachment is as easy to undo as it was to ask for --
        // and the tray window list still shows it as excluded, which is the
        // durable way back.
        if crate::window::shift_held() {
            if self.floated.insert(session.hwnd) {
                crate::log!("'{}' detached from the grid (Shift)", session.title);
            }
            self.retile_all();
            return;
        }
        // A plain drag of a detached window puts it back: the same gesture that
        // took it out, without the modifier. It is also the only thing such a
        // drag does, so return rather than falling through to the drop logic,
        // which would be resolving against a cell this window did not have.
        if session.detached {
            if self.floated.remove(&session.hwnd) {
                crate::log!("'{}' returned to the grid", session.title);
            }
            // Windows may have left it maximised from a Shift+maximise; the
            // tiler cannot size a maximised window.
            crate::window::restore_if_maximized(HWND(session.hwnd as *mut core::ffi::c_void));
            self.retile_all();
            return;
        }

        // Apply whatever the overlay was promising.
        if let Some(d) = session.drop {
            let target_hwnd = self
                .orders
                .get(&session.device)
                .and_then(|o| o.order.get(d.target).copied());

            match d.action {
                drag::DropAction::Swap => {
                    if let Some(order) = self.orders.get_mut(&session.device) {
                        order.swap(session.zone_index, d.target);
                    }
                    if let Some(t) = self.trees.get_mut(&session.device) {
                        if let Some(th) = target_hwnd {
                            t.swap(session.hwnd, th);
                        }
                    }
                }
                // An edge drop divides the target's cell. This is a structural
                // change the parametric layouts cannot represent, so it also
                // switches the monitor to the tree.
                drag::DropAction::InsertBefore | drag::DropAction::InsertAfter => {
                    // Splitting is deliberate now, not incidental.
                    //
                    // An edge drop converts the whole monitor to the tree
                    // layout and writes that to the config, so one stray drag
                    // changed how tiling worked from then on -- it happened
                    // four times without being intended. Holding Shift makes it
                    // a choice; a plain edge drop just reorders.
                    // Shift guards the *layout switch*, not the split.
                    //
                    // On a monitor already using the tree there is nothing to
                    // consent to: splitting is how that layout works, and the
                    // window order is derived from the tree anyway, so the
                    // reorder below would be overwritten by the next retile and
                    // the drop would appear to do nothing at all. Requiring
                    // Shift there took the feature away rather than protecting
                    // anything.
                    let already_a_tree = self.config.layout.kind == LayoutKind::Bsp;
                    if !already_a_tree && !crate::window::ctrl_held() {
                        if let Some(order) = self.orders.get_mut(&session.device) {
                            let before = d.action == drag::DropAction::InsertBefore;
                            order.move_to(
                                session.hwnd,
                                if before { d.target } else { d.target + 1 },
                            );
                        }
                        crate::vlog!(
                            "edge drop reordered; hold Shift to split and adopt the tree layout"
                        );
                        self.retile_all();
                        return;
                    }
                    // Recomputed at the drop rather than trusting the preview:
                    // the layout can have changed since the overlay was last
                    // painted, and a stale ratio would put the boundary
                    // somewhere the user was never shown.
                    let before = d.action == drag::DropAction::InsertBefore;
                    let ratio = self.split_ratio_for(
                        &session.device,
                        session.hwnd,
                        d.target,
                        d.side,
                        before,
                    );
                    match (ratio, target_hwnd) {
                        (Some(r), Some(th)) => {
                            self.split_onto(&session.device, session.hwnd, th, &d, r)
                        }
                        (None, _) => {
                            crate::log!("refused a split: the cell cannot hold both minimum sizes")
                        }
                        _ => {}
                    }
                }
            }
        }
        // Persist a dragged master fraction; the split map itself is
        // session-scoped by design (see TODO).
        let _ = self.config.save();

        if let Some(m) = crate::monitor::enumerate()
            .into_iter()
            .find(|x| x.device == session.device)
        {
            self.retile_monitor(&m);
        } else {
            self.retile_all();
        }
        // Keep the window the user was dragging focused.
        let hwnd = HWND(session.hwnd as *mut core::ffi::c_void);
        if crate::window::is_live(hwnd) {
            window::focus(hwnd);
        }
    }

    /// Divide `target`'s cell, giving half to `moved`.
    ///
    /// Switches the monitor to [`LayoutKind::Bsp`] when it is not already
    /// there, seeding the tree from the current window order so nothing
    /// scatters. A grid cannot hold a boundary that belongs to one cell, so
    /// there is no way to honour the drop without changing model -- and
    /// silently doing nothing, which is what happened before, is worse.
    fn split_onto(
        &mut self,
        device: &str,
        moved: isize,
        target: isize,
        d: &drag::Drop,
        ratio: f32,
    ) {
        let Some(m) = crate::monitor::enumerate()
            .into_iter()
            .find(|x| x.device == device)
        else {
            return;
        };
        let order = self
            .orders
            .get(device)
            .map(|o| o.order.clone())
            .unwrap_or_default();

        if self.config.layout.kind != LayoutKind::Bsp {
            // Seed from what is on screen now, then adopt the tree.
            let seed = crate::tree::Tree::from_windows(&order, m.work_area);
            self.trees.insert(device.to_string(), seed);
            self.layout_before_split = Some(self.config.layout.kind);
            self.config.layout.kind = LayoutKind::Bsp;
            let _ = self.config.save();
            crate::log!(
                "a drop switched the layout from {} to Split; Reset sizes puts it back",
                self.layout_before_split
                    .map(|k| k.label())
                    .unwrap_or("(unknown)")
            );
            // A layout mode changing under you is exactly the kind of thing
            // that reads as a malfunction if nothing says it happened.
            self.tray.balloon(
                "Layout switched to Split",
                "Dropping a window on an edge divides that cell in two, which the grid layouts cannot do. Reset sizes in the tray menu switches back.",
            );
        }

        let tree = self
            .trees
            .entry(device.to_string())
            .or_insert_with(|| crate::tree::Tree::from_windows(&order, m.work_area));

        let orientation = match d.side {
            drag::Side::Horizontal => crate::tree::Orientation::Horizontal,
            drag::Side::Vertical => crate::tree::Orientation::Vertical,
            // A centre drop is a swap and never reaches here.
            drag::Side::Centre => return,
        };
        let before = d.action == drag::DropAction::InsertBefore;
        if !tree.split_at(target, moved, orientation, before, ratio) {
            crate::log!("split target {target} not found in the tree");
        }
    }

    /// Would halving `target`'s cell leave either window below its minimum?
    ///
    /// Applications that enforce a minimum size do not shrink when given a
    /// smaller rectangle -- they clamp, overflow the cell and overlap their
    /// neighbours. Refusing the split is honest; performing one that cannot
    /// hold is what made Signal and Steam look erratic.
    /// The share of the target's cell the *first* child should take, or `None`
    /// when the cell cannot hold both windows however it is divided.
    ///
    /// An even split is the intent, not the requirement. Insisting on halves
    /// refused plenty of splits that were perfectly possible slightly off
    /// centre -- a 1000px cell holding a 700px minimum next to a 250px one has
    /// an obvious answer, and it is not "no".
    fn split_ratio_for(
        &self,
        device: &str,
        moved: isize,
        target: usize,
        side: drag::Side,
        before: bool,
    ) -> Option<f32> {
        let m = crate::monitor::enumerate()
            .into_iter()
            .find(|x| x.device == device)?;
        let (order, zones) = self.zones_of(&m);
        let (cell, target_hwnd) = (zones.get(target)?, order.get(target)?);

        let target_min = crate::window::min_size(HWND(*target_hwnd as *mut core::ffi::c_void));
        let moved_min = crate::window::min_size(HWND(moved as *mut core::ffi::c_void));
        // `before` decides which window ends up first, and therefore whose
        // minimum constrains which side of the boundary.
        let (first, second) = if before {
            (moved_min, target_min)
        } else {
            (target_min, moved_min)
        };

        match side {
            drag::Side::Horizontal => crate::tree::fit_ratio(cell.width(), first.0, second.0),
            drag::Side::Vertical => crate::tree::fit_ratio(cell.height(), first.1, second.1),
            drag::Side::Centre => None,
        }
    }

    fn stop_drag_timer(&self) {
        // SAFETY: killing a timer we set; harmless if it was never armed.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_DRAG);
        }
    }

    /// Debounce: collapse a burst of window events into one retile.
    fn schedule_retile(&self) {
        if self.config.general.paused || !self.config.general.auto_tile {
            return;
        }
        let ms = self.config.general.retile_debounce_ms.max(1);
        // SAFETY: resetting an existing timer is the documented way to debounce.
        unsafe {
            let _ = SetTimer(Some(self.hwnd), TIMER_RETILE, ms, None);
        }
    }

    // --- commands ---------------------------------------------------------

    fn focused_monitor(&self) -> Option<Monitor> {
        window::foreground()
            .and_then(crate::monitor::from_window)
            .or_else(crate::monitor::primary)
    }

    fn focus_direction(&mut self, dir: Direction) {
        let Some(m) = self.focused_monitor() else {
            return;
        };
        let Some(fg) = window::foreground() else {
            return;
        };
        let Some(order) = self.orders.get(&m.device) else {
            return;
        };
        let Some(from) = order.index_of(fg.0 as isize) else {
            return;
        };
        let zones = layout::compute(
            m.work_area,
            order.order.len(),
            self.config.layout.kind,
            &self.config.layout.params(),
        );
        if let Some(to) = layout::neighbour(&zones, from, dir) {
            if let Some(h) = order.order.get(to) {
                window::focus(HWND(*h as *mut core::ffi::c_void));
            }
        }
    }

    fn swap_direction(&mut self, dir: Direction) {
        let Some(m) = self.focused_monitor() else {
            return;
        };
        let Some(fg) = window::foreground() else {
            return;
        };
        let (from, to) = {
            let Some(order) = self.orders.get(&m.device) else {
                return;
            };
            let Some(from) = order.index_of(fg.0 as isize) else {
                return;
            };
            let zones = layout::compute(
                m.work_area,
                order.order.len(),
                self.config.layout.kind,
                &self.config.layout.params(),
            );
            let Some(to) = layout::neighbour(&zones, from, dir) else {
                return;
            };
            (from, to)
        };
        if let Some(order) = self.orders.get_mut(&m.device) {
            order.swap(from, to);
        }
        self.retile_monitor(&m);
        window::focus(fg);
    }

    fn toggle_float(&mut self) {
        let Some(fg) = window::foreground() else {
            return;
        };
        let h = fg.0 as isize;
        if self.floated.contains(&h) {
            self.floated.remove(&h);
        } else {
            self.floated.insert(h);
        }
        self.retile_all();
    }

    fn set_paused(&mut self, paused: bool) {
        self.config.general.paused = paused;
        self.tray.set_paused(paused);
        let _ = self.config.save();
        if !paused {
            self.retile_all();
        }
    }

    fn adjust_gaps(&mut self, delta: i32) {
        self.config.layout.outer_gap = (self.config.layout.outer_gap + delta).clamp(0, 400);
        self.config.layout.inner_gap = (self.config.layout.inner_gap + delta).clamp(0, 400);
        let _ = self.config.save();
        self.retile_all();
    }

    fn adjust_master(&mut self, delta: f32) {
        self.config.layout.master_fraction =
            (self.config.layout.master_fraction + delta).clamp(0.15, 0.85);
        let _ = self.config.save();
        self.retile_all();
    }

    fn set_layout(&mut self, kind: LayoutKind) {
        // A window that could not fit one layout may fit another, so give
        // every window another chance whenever the shape changes.
        self.forget_placement_failures();
        self.config.layout.kind = kind;
        let _ = self.config.save();
        self.retile_all();
    }

    /// Forget every dragged boundary, returning to even splits.
    /// Let every window be managed again.
    /// The window's minimum track size, cached for the life of the window.
    fn min_size_of(&mut self, hwnd: isize) -> (i32, i32) {
        if let Some(m) = self.mins.get(&hwnd) {
            return *m;
        }
        let m = crate::window::min_size(HWND(hwnd as *mut std::ffi::c_void));
        self.mins.insert(hwnd, m);
        m
    }

    fn forget_placement_failures(&mut self) {
        if !self.unmanageable.is_empty() {
            crate::log!(
                "re-managing {} previously stubborn window(s)",
                self.unmanageable.len()
            );
        }
        self.unmanageable.clear();
        self.misses.clear();
        self.requested.clear();
        // Re-ask as well: an application can raise its minimum after a mode
        // change, and a stale floor is what caused the write-off.
        self.mins.clear();
    }

    fn reset_splits(&mut self) {
        self.splits.clear();
        self.trees.clear();
        // Undo an automatic switch to the tree, but never a deliberate one:
        // only the drop path records a previous layout.
        if let Some(previous) = self.layout_before_split.take() {
            if self.config.layout.kind == LayoutKind::Bsp {
                self.config.layout.kind = previous;
                crate::log!("restored the {} layout", previous.label());
            }
        }
    }

    fn reset_sizes(&mut self) {
        self.reset_splits();
        let d = crate::config::LayoutConfig::default();
        self.config.layout.outer_gap = d.outer_gap;
        self.config.layout.inner_gap = d.inner_gap;
        self.config.layout.master_fraction = d.master_fraction;
        self.config.layout.master_count = d.master_count;
        let _ = self.config.save();
        self.retile_all();
    }

    fn reload_config(&mut self) {
        let Loaded {
            config, warnings, ..
        } = Config::load();
        self.config = config;
        self.warnings = warnings;
        crate::util::set_logging(self.config.diagnostics.logging);
        crate::util::set_verbose(self.config.diagnostics.verbose);
        self.apply_overlay_theme();
        self.theme_editor.apply_config(&self.config);
        self.unregister_hotkeys();
        self.register_hotkeys();
        self.palette.apply_config(&self.config);
        self.about.apply_config(&self.config);
        self.keys_window.apply_config(&self.config);
        let (_, dark) = crate::ui::theme::colors_for(self.config.appearance.theme);
        crate::ui::hover::set_dark(dark);
        self.tray.set_paused(self.config.general.paused);
        self.dimmer = Dimmer::new(self.config.dimming);
        self.retile_all();
        self.update_dimming();
    }

    fn open_config_file(&self) {
        let Ok(path) = Config::path() else { return };
        // Ensure it exists before asking the shell to open it.
        if !path.exists() {
            let _ = self.config.save_to(&path);
        }
        open_path(&path.to_string_lossy());
    }

    fn exit(&mut self) {
        self.flush_store();
        // SAFETY: posting our own quit message.
        unsafe {
            PostQuitMessage(0);
        }
    }

    /// Ask GitHub whether a newer release exists.
    ///
    /// `manual` distinguishes a menu click from the daily background check. A
    /// click deserves an answer either way -- silence after asking is
    /// indistinguishable from a broken button -- whereas the background check
    /// only speaks when there is something new. Being offline is normal and is
    /// not worth interrupting anybody over.
    ///
    /// Blocking, on the UI thread, and deliberately so: it is a single small
    /// request with a bounded timeout, and a thread plus its synchronisation
    /// would be more machinery than the feature is worth. If it ever feels
    /// slow, that is the moment to move it, not before.
    fn check_for_updates(&mut self, manual: bool) {
        use crate::update::Outcome;

        let outcome = crate::update::check_latest();
        self.config.updates.last_checked = crate::update::now_unix();
        match &outcome {
            Outcome::Available { version, url, .. } => {
                // Announce a given version once. Being told daily about a
                // release you have decided not to install is nagging.
                let already = self.config.updates.last_reported == *version;
                if manual || !already {
                    self.config.updates.last_reported = version.clone();
                    crate::log!("update available: {version}");
                    self.tray.balloon(
                        &format!("SuperTile {version} is available"),
                        "Open the About window to see what changed and download it. Nothing has been downloaded or installed.",
                    );
                    self.pending_update = Some((version.clone(), url.clone()));
                }
            }
            Outcome::UpToDate if manual => {
                self.tray.balloon(
                    "SuperTile is up to date",
                    &format!("You are running {}.", crate::APP_VERSION),
                );
            }
            Outcome::Failed(reason) if manual => {
                self.tray.balloon("Could not check for updates", reason);
            }
            _ => {}
        }
        let _ = self.config.save();
    }

    /// Run the daily check, if it is switched on and due.
    fn check_for_updates_if_due(&mut self) {
        if !self.config.updates.check_automatically {
            return;
        }
        if !crate::update::due(
            self.config.updates.last_checked,
            crate::update::now_unix(),
            self.config.updates.interval_hours,
        ) {
            return;
        }
        self.check_for_updates(false);
    }

    /// "on" or "off", for the settings table in an issue report.
    fn on_off(v: bool) -> String {
        if v { "on" } else { "off" }.to_string()
    }

    /// Assemble an anonymised issue report, put it on the clipboard and open it.
    ///
    /// The report is shown before it can go anywhere, in an editor rather than
    /// a window of our own: anonymisation is best-effort, so the person about
    /// to paste this into a public tracker needs to be able to read all of it,
    /// search it and edit it. A bespoke viewer would offer less than Notepad
    /// does and would have to be maintained.
    ///
    /// Nothing is uploaded and nothing is sent. The clipboard is written
    /// because pasting is the whole point, and that is stated plainly rather
    /// than done silently.
    fn create_issue_report(&mut self) {
        // Only the process file name and window class travel; titles never do.
        let rows: Vec<crate::report::WindowRow> = self
            .last_rects
            .keys()
            .filter_map(|h| {
                let hwnd = HWND(*h as *mut core::ffi::c_void);
                if !crate::window::is_live(hwnd) {
                    return None;
                }
                let rect = self.last_rects.get(h)?;
                let (min_width, min_height) = self.mins.get(h).copied().unwrap_or((0, 0));
                // The identity key is `fingerprint|exe|class`; the report wants
                // the last two parts.
                let key = self.keys.get(h)?;
                let mut parts = key.rsplit('|');
                let class = parts.next().unwrap_or_default().to_string();
                let exe = parts.next().unwrap_or_default().to_string();
                Some(crate::report::WindowRow {
                    exe,
                    class,
                    width: rect.width(),
                    height: rect.height(),
                    min_width,
                    min_height,
                    state: if self.elevated.contains(h) {
                        "elevated (cannot be moved)"
                    } else if self.floated.contains(h) {
                        "excluded"
                    } else if self.unmanageable.contains_key(h) {
                        "not accepting a size"
                    } else {
                        "tiled"
                    },
                })
            })
            .collect();

        let settings = vec![
            (
                "Auto-tile".to_string(),
                Self::on_off(self.config.general.auto_tile),
            ),
            (
                "Paused".to_string(),
                Self::on_off(self.config.general.paused),
            ),
            (
                "Gaps".to_string(),
                format!(
                    "{} outer, {} inner",
                    self.config.layout.outer_gap, self.config.layout.inner_gap
                ),
            ),
            (
                "Overlay theme".to_string(),
                self.config.appearance.overlay_theme.clone(),
            ),
            (
                "Dimming".to_string(),
                Self::on_off(self.config.dimming.enabled),
            ),
        ];

        let report = crate::report::collect(
            crate::APP_VERSION,
            self.config.layout.kind.label(),
            settings,
            self.rejected
                .iter()
                .map(|(action, _)| format!("{action:?} could not be registered"))
                .collect(),
            rows,
            crate::util::logging_enabled(),
            self.config.diagnostics.verbose,
        );
        let text = crate::report::render(&report, &crate::report::current_redactor());

        let copied = crate::util::set_clipboard_text(&text);
        let saved = crate::util::data_dir()
            .ok()
            .map(|d| d.join("issue-report.md"))
            .filter(|p| std::fs::write(p, &text).is_ok());

        crate::log!(
            "issue report written ({} bytes), clipboard={copied}",
            text.len()
        );
        self.tray.balloon(
            "Issue report ready",
            if copied {
                "It is on your clipboard and open for review. Window titles, paths and your user and machine names are left out. Read it before posting: nothing has been sent anywhere."
            } else {
                "Saved and opened for review. The clipboard could not be written. Window titles, paths and your user and machine names are left out; nothing has been sent anywhere."
            },
        );
        if let Some(path) = saved {
            crate::ui::about::open_url(&path.to_string_lossy());
        }
    }

    /// Where the saved split trees live.
    fn splits_path() -> Option<std::path::PathBuf> {
        crate::util::data_dir().ok().map(|d| d.join("splits.json"))
    }

    /// Write every monitor's split tree, keyed by monitor and by the
    /// arrangement of displays.
    ///
    /// Keyed by the display fingerprint as well as the device name because a
    /// tree shaped for a 5120px ultrawide is nonsense on a laptop panel, and
    /// docking changes both without changing the device name.
    fn save_trees(&self) {
        let Some(path) = Self::splits_path() else {
            return;
        };
        let saved: std::collections::BTreeMap<String, crate::tree::SavedNode> = self
            .trees
            .iter()
            .filter_map(|(device, tree)| {
                let key_of = |h: isize| -> Option<String> { self.keys.get(&h).cloned() };
                tree.to_saved(&key_of).map(|n| (device.clone(), n))
            })
            .collect();
        if saved.is_empty() {
            let _ = std::fs::remove_file(&path);
            return;
        }
        if let Ok(text) = serde_json::to_string_pretty(&saved) {
            let _ = std::fs::write(&path, text);
        }
    }

    fn load_saved_trees() -> std::collections::BTreeMap<String, crate::tree::SavedNode> {
        let Some(path) = Self::splits_path() else {
            return Default::default();
        };
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Restore this monitor's tree from disk, if one was saved for it.
    ///
    /// Each saved leaf claims one live window of the matching kind. Windows
    /// that match nothing are left to `reconcile`, which inserts them the same
    /// way a newly-opened window is inserted. Restoring is attempted once per
    /// monitor per run: a tree that could not be filled the first time will not
    /// be any more fillable a moment later, and retrying would fight the user's
    /// own subsequent edits.
    fn restore_tree(&mut self, device: &str, live: &[isize], area: Rect) {
        // Keep trying for a short while after launch, then stop.
        //
        // One attempt was too few. SuperTile usually starts with Windows, so
        // the first retile happens while the session is still loading and the
        // windows a saved layout refers to may not exist yet -- the restore
        // then failed against a nearly empty desktop and was never retried.
        // Bounded by time since startup rather than by a count, because what
        // matters is that it stops before the user begins arranging things.
        if self.restored.contains(device) {
            return;
        }
        if crate::util::tick_ms().saturating_sub(self.started_at) > RESTORE_WINDOW_MS {
            self.restored.insert(device.to_string());
            return;
        }
        let Some(saved) = self.saved_trees.get(device).cloned() else {
            return;
        };
        let mut pool: Vec<isize> = live.to_vec();
        let mut claim = |key: &str| -> Option<isize> {
            let idx = pool
                .iter()
                .position(|h| self.keys.get(h).map(String::as_str) == Some(key))?;
            Some(pool.remove(idx))
        };
        let restored = crate::tree::Tree::from_saved(&saved, &mut claim);
        if restored.layout(area).is_empty() {
            crate::log!("no saved split layout for {device} could be matched to a live window");
            return;
        }
        crate::log!("restored the saved split layout for {device}");
        self.restored.insert(device.to_string());
        self.trees.insert(device.to_string(), restored);
    }

    fn flush_store(&mut self) {
        if self.store_dirty && self.config.memory.enabled {
            let _ = self.store.save();
            self.store_dirty = false;
        }
        // Cheap and idempotent, so it rides along with the periodic save rather
        // than needing its own dirty flag.
        self.save_trees();
    }

    // --- palette ----------------------------------------------------------

    fn binding_text(&self, action: Action) -> String {
        self.bindings
            .get(&action)
            .map(|h| h.to_string())
            .unwrap_or_default()
    }

    fn build_palette_items(&self) -> Vec<palette::Item> {
        use palette::{Action as PA, Command, Item, Kind};
        let mut items = Vec::new();

        // Commands first: they are the reason this is a *command* palette.
        // The hint must name the action the row performs. Passing
        // Action::Palette for six of these told the user that Exit SuperTile
        // was Win+Alt+D -- the palette's own key -- so pressing the advertised
        // shortcut just reopened the palette. `None` means "no shortcut".
        let cmds: [(&str, Command, Option<Action>, &str); 8] = [
            (
                "Retile now",
                Command::Retile,
                Some(Action::Retile),
                "arrange tile refresh",
            ),
            (
                if self.config.general.paused {
                    "Resume tiling"
                } else {
                    "Pause tiling"
                },
                Command::TogglePause,
                Some(Action::TogglePause),
                "disable enable suspend stop",
            ),
            (
                "Settings",
                Command::OpenSettings,
                None,
                "preferences options config",
            ),
            (
                "About & SBOM",
                Command::OpenAbout,
                None,
                "version licence cra sbom",
            ),
            (
                "Reload configuration",
                Command::ReloadConfig,
                Some(Action::ReloadConfig),
                "refresh config",
            ),
            (
                "Increase gaps",
                Command::IncreaseGaps,
                Some(Action::IncreaseGaps),
                "spacing padding bigger",
            ),
            (
                "Decrease gaps",
                Command::DecreaseGaps,
                Some(Action::DecreaseGaps),
                "spacing padding smaller",
            ),
            (
                "Exit SuperTile",
                Command::Exit,
                Some(Action::Quit),
                "quit close",
            ),
        ];
        for (label, cmd, act, kw) in cmds {
            let hint = act.map(|a| self.binding_text(a)).unwrap_or_default();
            items.push(Item::new(label, hint, Kind::Command, PA::Command(cmd)).with_keywords(kw));
        }

        // Asking goes to the browser, so only the user's own choice gates it.
        // Placed above the layouts so a bare "cl" reaches it quickly.
        // Asking goes to claude.ai in the browser, so nothing needs to be
        // installed -- only the user's consent to the feature existing.
        let claude = self.config.palette.claude_desktop;
        // Tab is the way in; the entry stays for anyone who reaches for the
        // list first, and to make the mode discoverable at all.
        self.palette.set_claude_available(claude);
        if claude {
            items.push(
                Item::new(
                    "Ask Claude",
                    "Press Tab, then type your question. It opens in your browser, ready to send",
                    Kind::Command,
                    PA::Command(Command::NewClaudeChat),
                )
                .with_keywords("claude ai chat ask anthropic assistant question"),
            );
        }

        for kind in LayoutKind::ALL {
            let current = if kind == self.config.layout.kind {
                "current"
            } else {
                ""
            };
            items.push(
                Item::new(
                    format!("Layout: {}", kind.label()),
                    current,
                    Kind::Layout,
                    PA::SetLayout(kind),
                )
                .with_keywords("layout arrange tiling"),
            );
        }

        // Open windows, so the palette doubles as a window switcher. Uses
        // own_windows, not excluded_from_tiling: a window held out of the
        // tiling should still be reachable by name.
        let exclude = self.own_windows();
        for w in window::enumerate(&self.config, &exclude) {
            if w.disposition == window::Disposition::Ignore {
                continue;
            }
            let app = w
                .exe
                .rsplit(['\\', '/'])
                .next()
                .unwrap_or("")
                .trim_end_matches(".exe")
                .to_string();
            items.push(Item::new(
                w.title.clone(),
                app,
                Kind::Window,
                PA::Focus(w.hwnd),
            ));
        }

        for a in self.apps.entries() {
            items.push(palette::Item::new(
                a.name.clone(),
                a.group.clone(),
                Kind::App,
                PA::Launch(a.path.clone()),
            ));
        }

        items
    }

    fn open_palette(&mut self) {
        let items = self.build_palette_items();
        self.palette.show(items, &self.config);
    }

    fn handle_palette_result(&mut self) {
        let Some(action) = self.palette.take_result() else {
            return;
        };
        use palette::{Action as PA, Command};
        match action {
            PA::Launch(path) => {
                if !crate::applist::launch(&path) {
                    crate::log!("failed to launch {}", path.display());
                }
            }
            PA::Focus(hwnd) => window::focus(HWND(hwnd as *mut core::ffi::c_void)),
            PA::SetLayout(kind) => self.set_layout(kind),
            PA::Command(c) => match c {
                Command::Retile => self.retile_all(),
                Command::TogglePause => {
                    let p = !self.config.general.paused;
                    self.set_paused(p);
                }
                Command::OpenSettings => self.open_config_file(),
                Command::OpenAbout => self.about.show(),
                Command::ReloadConfig => self.reload_config(),
                Command::IncreaseGaps => self.adjust_gaps(2),
                Command::DecreaseGaps => self.adjust_gaps(-2),
                Command::GrowMaster => self.adjust_master(0.05),
                Command::ShrinkMaster => self.adjust_master(-0.05),
                Command::ResetSizes => self.reset_sizes(),
                Command::NewClaudeChat => {
                    // Carry whatever was typed. The palette is a text field the
                    // user has already filled in; making them retype the
                    // question is the sort of small indignity that stops a
                    // feature being used.
                    let question = self.palette.accepted_query();
                    match crate::claude::ask(&question) {
                        crate::claude::Handoff::Prefilled => {}
                        crate::claude::Handoff::OnClipboard => {
                            self.tray.balloon(
                                "That question was too long for a link",
                                "It is on your clipboard instead; paste it into the chat with Ctrl+V.",
                            );
                        }
                        crate::claude::Handoff::Failed => {
                            crate::log!("could not open claude.ai");
                            self.tray.balloon(
                                "Could not open Claude",
                                "No browser would accept the link.",
                            );
                        }
                    }
                }
                Command::Exit => self.exit(),
            },
        }
    }

    // --- tray -------------------------------------------------------------

    /// Snapshot the windows the tray should list.
    /// How every action's hotkey actually resolved, for the tray menu.
    fn binding_report(&self) -> Vec<BindingInfo> {
        Action::ALL
            .iter()
            .map(|a| {
                let wanted = self
                    .fell_back
                    .iter()
                    .find(|(x, _, _)| x == a)
                    .map(|(_, first, _)| first.to_string())
                    .unwrap_or_default();
                BindingInfo {
                    action: *a,
                    keys: self
                        .bindings
                        .get(a)
                        .map(|h| h.to_string())
                        .unwrap_or_default(),
                    wanted,
                    i3: a.i3_binding().to_string(),
                }
            })
            .collect()
    }

    fn build_window_list(&self) -> Vec<WindowEntry> {
        let own = self.own_windows();
        crate::window::listable(&self.config, &own)
            .into_iter()
            .take(crate::tray::MAX_WINDOWS)
            .map(|w| WindowEntry {
                hwnd: w.hwnd,
                title: w.title.clone(),
                app: w
                    .exe
                    .rsplit(['\\', '/'])
                    .next()
                    .unwrap_or("")
                    .trim_end_matches(".exe")
                    .trim_end_matches(".EXE")
                    .to_string(),
                excluded: self.floated.contains(&w.hwnd),
                topmost: crate::window::is_topmost(w.handle()),
            })
            .collect()
    }

    /// Resolve a listed index to a still-living window handle.
    ///
    /// The list is a snapshot taken when the menu opened; a window can close
    /// while the menu is up, and acting on a recycled handle would target
    /// whatever window Windows handed the number to next.
    fn listed_window(&self, index: usize) -> Option<HWND> {
        let entry = self.listed.get(index)?;
        let h = HWND(entry.hwnd as *mut core::ffi::c_void);
        crate::window::is_live(h).then_some(h)
    }

    fn toggle_exclude(&mut self, index: usize) {
        let Some(h) = self.listed_window(index) else {
            return;
        };
        let key = h.0 as isize;
        if self.floated.remove(&key) {
            // It may have been detached by Shift+maximise, in which case it is
            // still maximised and the tiler cannot size it.
            crate::window::restore_if_maximized(h);
            crate::log!("re-included window {key} in tiling");
        } else {
            self.floated.insert(key);
            crate::log!("excluded window {key} from tiling");
        }
        self.retile_all();
    }

    fn toggle_topmost(&mut self, index: usize) {
        let Some(h) = self.listed_window(index) else {
            return;
        };
        let want = !crate::window::is_topmost(h);
        if !crate::window::set_topmost(h, want) {
            // Fails for windows owned by a more privileged process.
            crate::log!("could not set always-on-top on {:?}", h.0);
        }
    }

    fn clear_exclusions(&mut self) {
        self.floated.clear();
        self.retile_all();
    }

    fn show_tray_menu(&mut self) {
        let mut pt = POINT::default();
        // SAFETY: valid out-param.
        let _ = unsafe { GetCursorPos(&mut pt) };
        // Marks the modal loop as open for the rest of this function, so
        // host_wndproc refuses to hand out a second &mut App while
        // TrackPopupMenuEx pumps messages. Dropped on every return path.
        let _modal = ModalGuard::enter();

        // The save and drag timers would otherwise fire inside the menu's
        // loop; suspending them removes the most likely reentrant messages
        // entirely rather than relying on the guard alone.
        // SAFETY: killing timers we set; both are re-armed below.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_SAVE);
        }

        self.listed = self.build_window_list();
        let state = MenuState {
            paused: self.config.general.paused,
            layout: self.config.layout.kind,
            outer_gap: self.config.layout.outer_gap,
            inner_gap: self.config.layout.inner_gap,
            master_percent: (self.config.layout.master_fraction * 100.0).round() as u32,
            palette_binding: self.binding_text(Action::Palette),
            windows: self.listed.clone(),
            excluded_count: self.floated.len(),
            bindings: self.binding_report(),
            hotkey_conflicts: self.rejected.len(),
            hotkey_moved: self.fell_back.len(),
            size_readout: self.config.appearance.show_size_readout,
            resize_grid: self.config.appearance.show_grid_on_resize,
            logging: self.config.diagnostics.logging,
            reserve_elevated: self.config.general.reserve_cells_for_elevated,
            auto_update_check: self.config.updates.check_automatically,
            verbose_logging: self.config.diagnostics.verbose,
            overlay_theme: self.config.appearance.overlay_theme.clone(),
            custom_theme_name: self.config.appearance.custom_theme.name.clone(),
            dim: self.dimmer.config(),
            dim_pinned: self.dimmer.pinned().is_some(),
            // Read the registry, not the config: a value removed through
            // Settings -> Startup apps must show as off here rather than the
            // menu claiming something untrue.
            start_with_windows: crate::autostart::is_enabled(),
            auto_tile: self.config.general.auto_tile,
            claude_enabled: self.config.palette.claude_desktop,
        };
        let cmd = self.tray.show_menu(&state, pt);

        // SAFETY: re-arming the periodic save now the modal loop has ended.
        unsafe {
            let _ = SetTimer(Some(self.hwnd), TIMER_SAVE, SAVE_INTERVAL_MS, None);
        }

        let Some(cmd) = cmd else {
            return;
        };
        match cmd {
            MenuCommand::OpenPalette => self.open_palette(),
            MenuCommand::Retile => self.retile_all(),
            MenuCommand::SetLayout(k) => self.set_layout(k),
            MenuCommand::GapIncrease => self.adjust_gaps(2),
            MenuCommand::GapDecrease => self.adjust_gaps(-2),
            MenuCommand::MasterGrow => self.adjust_master(0.05),
            MenuCommand::MasterShrink => self.adjust_master(-0.05),
            MenuCommand::SizeReset => self.reset_sizes(),
            MenuCommand::TogglePause => {
                let p = !self.config.general.paused;
                self.set_paused(p);
            }
            MenuCommand::Settings => self.open_config_file(),
            MenuCommand::About => self.about.show(),
            MenuCommand::Exit => self.exit(),
            MenuCommand::FocusWindow(i) => {
                if let Some(h) = self.listed_window(i) {
                    window::focus(h);
                }
            }
            MenuCommand::ToggleExclude(i) => self.toggle_exclude(i),
            MenuCommand::ToggleTopmost(i) => self.toggle_topmost(i),
            MenuCommand::ClearExclusions => self.clear_exclusions(),
            MenuCommand::ShowHotkeyConflicts => self.show_hotkey_conflicts(),
            MenuCommand::ToggleStartWithWindows => self.toggle_start_with_windows(),
            MenuCommand::ToggleLogging => {
                let on = !self.config.diagnostics.logging;
                self.config.diagnostics.logging = on;
                crate::util::set_logging(on);
                crate::util::set_verbose(on && self.config.diagnostics.verbose);
                let _ = self.config.save();
                if on {
                    self.tray.balloon(
                        "Debug logging is on",
                        "SuperTile is recording what it does to a log file, including window titles and program paths. It stays on your machine. Turn it off in the tray when you are done.",
                    );
                }
            }
            MenuCommand::ToggleVerboseLogging => {
                let on = !self.config.diagnostics.verbose;
                self.config.diagnostics.verbose = on;
                if on {
                    // Verbose without logging records nothing, which reads as
                    // the toggle being broken.
                    self.config.diagnostics.logging = true;
                    crate::util::set_logging(true);
                }
                crate::util::set_verbose(on);
                let _ = self.config.save();
            }
            MenuCommand::UseTheme(i) => {
                if let Some((key, _)) = crate::ui::theme::OVERLAY_THEMES.get(i) {
                    self.config.appearance.overlay_theme = (*key).to_string();
                    self.apply_overlay_theme();
                    let _ = self.config.save();
                }
            }
            MenuCommand::UseCustomTheme => {
                self.config.appearance.overlay_theme = "custom".to_string();
                self.apply_overlay_theme();
                let _ = self.config.save();
            }
            MenuCommand::PickThemeColour(which) => self.pick_theme_colour(which),
            MenuCommand::RenameCustomTheme => self.rename_custom_theme(),
            MenuCommand::OpenThemeEditor => {
                self.theme_editor.set_owner(self.hwnd);
                self.theme_editor.apply_config(&self.config);
                self.theme_editor.show();
            }
            MenuCommand::CreateIssueReport => self.create_issue_report(),
            MenuCommand::ToggleReserveElevated => {
                let on = !self.config.general.reserve_cells_for_elevated;
                self.config.general.reserve_cells_for_elevated = on;
                let _ = self.config.save();
                // The decision changes who is in the layout, so it has to be
                // taken again for every window rather than at the next event.
                self.checked_elevation.clear();
                self.retile_all();
            }
            MenuCommand::ToggleAutoUpdateCheck => {
                let on = !self.config.updates.check_automatically;
                self.config.updates.check_automatically = on;
                let _ = self.config.save();
                if on {
                    self.tray.balloon(
                        "Update checks are on",
                        "SuperTile will ask github.com once a day whether a newer release exists. Nothing about you is sent beyond the request itself, and nothing is ever downloaded or installed automatically.",
                    );
                }
            }
            MenuCommand::CheckForUpdates => self.check_for_updates(true),
            MenuCommand::OpenLogFolder => {
                if let Ok(dir) = crate::util::data_dir() {
                    crate::ui::about::open_url(&dir.to_string_lossy());
                }
            }
            MenuCommand::ToggleResizeGrid => {
                self.config.appearance.show_grid_on_resize =
                    !self.config.appearance.show_grid_on_resize;
                if !self.config.appearance.show_grid_on_resize {
                    self.grid.hide();
                }
                let _ = self.config.save();
            }
            MenuCommand::ToggleSizeReadout => {
                self.config.appearance.show_size_readout =
                    !self.config.appearance.show_size_readout;
                if !self.config.appearance.show_size_readout {
                    self.readout.hide();
                }
                let _ = self.config.save();
            }
            MenuCommand::ToggleAutoTile => {
                self.config.general.auto_tile = !self.config.general.auto_tile;
                let _ = self.config.save();
                if self.config.general.auto_tile {
                    self.retile_all();
                }
            }
            MenuCommand::EditConfigFile => self.open_config_file(),
            MenuCommand::ShowHotkeyEditor => self.show_hotkey_editor(),
            MenuCommand::ToggleClaudeSource => {
                self.config.palette.claude_desktop = !self.config.palette.claude_desktop;
                let _ = self.config.save();
            }
            MenuCommand::ToggleDimming => self.toggle_dimming(),
            MenuCommand::ToggleDimAutoTrack => {
                let on = !self.dimmer.config().auto_track;
                self.dimmer.set_auto_track(on);
                self.persist_dimming();
            }
            MenuCommand::PinDimWindow => self.pin_dim_window(),
            MenuCommand::SetDimWindowLevel(l) => {
                self.dimmer.set_window_level(l);
                self.persist_dimming();
            }
            MenuCommand::SetDimTaskbarLevel(l) => {
                self.dimmer.set_taskbar_level(l);
                self.persist_dimming();
            }
        }
    }

    /// Open the shortcut list, populated with how each binding resolved.
    fn show_hotkey_editor(&mut self) {
        let rows: Vec<KeyRow> = Action::ALL
            .iter()
            .map(|a| {
                let wanted = self
                    .fell_back
                    .iter()
                    .find(|(x, _, _)| x == a)
                    .map(|(_, first, _)| first.to_string())
                    .unwrap_or_default();
                KeyRow {
                    action: *a,
                    keys: self
                        .bindings
                        .get(a)
                        .map(|h| h.to_string())
                        .unwrap_or_default(),
                    wanted,
                    i3: a.i3_binding().to_string(),
                    note: a.note().to_string(),
                }
            })
            .collect();
        self.keys_window.show(rows);
    }

    /// A binding was rebound in the shortcut window: persist and re-register.
    fn apply_hotkey_change(&mut self) {
        let Some((action, keys)) = self.keys_window.take_pending() else {
            return;
        };
        crate::log!("rebinding {} to {keys}", action.config_key());
        self.config.set_binding(action, &keys);
        let _ = self.config.save();
        // Re-register everything: a single change can free a combination another
        // action had fallen back from, and that action should get it back.
        self.unregister_hotkeys();
        self.register_hotkeys();
        self.show_hotkey_editor();
    }

    /// Register or unregister the HKCU Run value, and mirror it into config.
    fn toggle_start_with_windows(&mut self) {
        let want = !crate::autostart::is_enabled();
        if crate::autostart::set_enabled(want) {
            self.config.general.start_with_windows = want;
            let _ = self.config.save();
            crate::log!("start with Windows: {want}");
        } else {
            message_box(
                "SuperTile",
                concat!(
                    "The Run registry value could not be changed.\n\n",
                    "Something is preventing SuperTile from writing to\n",
                    r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                ),
            );
        }
    }

    // --- dimming ----------------------------------------------------------

    fn toggle_dimming(&mut self) {
        let on = !self.dimmer.is_enabled();
        self.dimmer.set_enabled(on);
        self.persist_dimming();
    }

    /// Keep the focused window bright regardless of where focus goes next.
    fn pin_dim_window(&mut self) {
        let Some(fg) = window::foreground() else {
            return;
        };
        if self.own_windows().contains(&(fg.0 as isize)) {
            // The tray menu had focus when this ran; pinning our own window
            // would leave the whole desktop dim with nothing lit.
            return;
        }
        self.dimmer.pin(fg.0 as isize);
        if !self.dimmer.is_enabled() {
            self.dimmer.set_enabled(true);
        }
        self.persist_dimming();
    }

    fn persist_dimming(&mut self) {
        self.config.dimming = self.dimmer.config();
        let _ = self.config.save();
        self.update_dimming();
    }

    /// Re-apply dimming after the layout changed. No-op when dimming is off.
    ///
    /// Split from [`Self::update_dimming`] purely so the hot path -- a retile
    /// with dimming disabled, which is the common case -- costs one bool test
    /// rather than a window enumeration.
    fn refresh_dimming_after_layout(&mut self) {
        if !self.dimmer.is_enabled() {
            return;
        }
        self.update_dimming();
    }

    /// Recompute the overlays. Cheap; safe to call on every focus change.
    fn update_dimming(&mut self) {
        if !self.dimmer.is_enabled() {
            self.dimmer.update(None, &[]);
            return;
        }
        // Collect our own handles *before* borrowing the dimmer mutably.
        let own: Vec<isize> = self.own_windows().into_iter().collect();
        let fg = window::foreground();
        self.dimmer.update(fg, &own);
    }

    /// Explain which bindings another application already owns.
    ///
    /// `RegisterHotKey` failing is invisible: the key simply does nothing and
    /// the user concludes the feature is broken. On a machine running
    /// PowerToys this affects most of the defaults, so it needs saying out
    /// loud, with the fix attached.
    fn show_hotkey_conflicts(&self) {
        if self.rejected.is_empty() && self.fell_back.is_empty() {
            return;
        }
        let mut body = String::new();

        if !self.fell_back.is_empty() {
            body.push_str(
                "These shortcuts were already taken, so SuperTile moved to its \
                 fallback key. They work now, and the change has been saved:\n\n",
            );
            let mut v = self.fell_back.clone();
            v.sort_by_key(|(a, _, _)| *a as i32);
            for (action, wanted, got) in &v {
                body.push_str(&format!(
                    "    {}\n        wanted {wanted}, using {got}\n",
                    action.label()
                ));
            }
            body.push('\n');
        }

        if !self.rejected.is_empty() {
            body.push_str(
                "These actions have no working shortcut: every alternative is \
                 taken too.\n\n",
            );
            let mut v = self.rejected.clone();
            v.sort_by_key(|(a, _)| *a as i32);
            for (action, wanted) in &v {
                body.push_str(&format!(
                    "    {}\n        tried {wanted} and its fallbacks\n",
                    action.label()
                ));
            }
            body.push('\n');
        }

        body.push_str(
            "Pick different keys in Settings \u{25B8} Keyboard shortcuts: click a \
             row and press the combination you want.\n\n",
        );
        if let Ok(p) = Config::path() {
            body.push_str(&format!("Configuration file:\n{}", p.display()));
        }
        message_box("SuperTile \u{2014} keyboard shortcuts", &body);
    }

    // --- message dispatch -------------------------------------------------

    fn on_hotkey(&mut self, id: i32) {
        let Some(action) = Action::from_hotkey_id(id) else {
            return;
        };
        match action {
            Action::Palette => {
                if self.palette.is_visible() {
                    self.palette.hide();
                } else {
                    self.open_palette();
                }
            }
            Action::LaunchTerminal => self.launch_terminal(),
            Action::CloseWindow => self.close_focused_window(),

            Action::FocusLeft => self.focus_direction(Direction::Left),
            Action::FocusRight => self.focus_direction(Direction::Right),
            Action::FocusUp => self.focus_direction(Direction::Up),
            Action::FocusDown => self.focus_direction(Direction::Down),

            Action::MoveLeft => self.swap_direction(Direction::Left),
            Action::MoveRight => self.swap_direction(Direction::Right),
            Action::MoveUp => self.swap_direction(Direction::Up),
            Action::MoveDown => self.swap_direction(Direction::Down),

            Action::LayoutColumns => self.set_layout(LayoutKind::Columns),
            Action::LayoutRows => self.set_layout(LayoutKind::Rows),
            // i3's stacking and tabbed both show one window at a time; the
            // nearest SuperTile equivalent is Monocle. See Action::note().
            Action::LayoutStacking | Action::LayoutTabbed => self.set_layout(LayoutKind::Monocle),
            Action::CycleLayout => {
                let k = self.config.layout.kind.next();
                self.set_layout(k);
            }
            Action::ToggleFullscreen => self.toggle_fullscreen(),
            Action::ToggleFloat => self.toggle_float(),

            Action::GrowMaster => self.adjust_master(0.05),
            Action::ShrinkMaster => self.adjust_master(-0.05),
            Action::IncreaseGaps => self.adjust_gaps(2),
            Action::DecreaseGaps => self.adjust_gaps(-2),

            Action::Retile => self.retile_current(),
            Action::TogglePause => {
                let p = !self.config.general.paused;
                self.set_paused(p);
            }
            Action::ToggleDimming => self.toggle_dimming(),
            Action::ReloadConfig => self.reload_config(),
            Action::Quit => self.exit(),
        }
    }

    /// i3's `$mod+f`. Swaps the current monitor between Monocle and whatever
    /// layout was in use, so pressing it twice returns you exactly where you
    /// were rather than to a default.
    fn toggle_fullscreen(&mut self) {
        match self.prev_layout.take() {
            Some(previous) => self.set_layout(previous),
            None => {
                if self.config.layout.kind == LayoutKind::Monocle {
                    // Already fullscreen with nothing remembered (e.g. after a
                    // config reload): fall back to the default layout.
                    self.set_layout(LayoutKind::default());
                } else {
                    self.prev_layout = Some(self.config.layout.kind);
                    self.set_layout(LayoutKind::Monocle);
                }
            }
        }
    }

    /// i3's `$mod+Shift+q`. Sends `WM_CLOSE`, the same polite request the
    /// window's own X button makes, so the application can prompt to save.
    /// i3's `kill` terminates the client outright; doing that on Windows would
    /// lose unsaved work.
    fn close_focused_window(&mut self) {
        let Some(h) = window::foreground() else {
            return;
        };
        if self.own_windows().contains(&(h.0 as isize)) {
            return; // never close our own UI this way
        }
        // SAFETY: posting to a live foreground window; the target decides what
        // to do with it.
        unsafe {
            let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }

    /// i3's `$mod+Return`. Prefers Windows Terminal and falls back to
    /// PowerShell, which is present on every supported Windows build.
    /// i3's `$mod+Return`. Prefers Windows Terminal and falls back to
    /// PowerShell, then the classic shell.
    ///
    /// Absolute paths only. `ShellExecuteW` resolves a bare name through
    /// `HKCU\...\App Paths` and the *current working directory* before the
    /// system directory, both of which an unprivileged process in the same
    /// session can write. Launching `"wt.exe"` by name would hand that process
    /// a way to run its own binary on a SuperTile keystroke.
    fn launch_terminal(&mut self) {
        for path in terminal_candidates() {
            if path.exists() && launch_detached(&path.to_string_lossy()) {
                return;
            }
        }
        crate::log!("no terminal could be launched");
    }

    fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        if self.tray.is_taskbar_created(msg) {
            // Explorer restarted; the icon is gone until we re-add it.
            crate::log!("Explorer restarted; re-adding the tray icon");
            self.tray_retries = TRAY_RETRY_LIMIT;
            self.ensure_tray_icon();
            return Some(LRESULT(0));
        }
        match msg {
            WM_HOTKEY => {
                self.on_hotkey(wp.0 as i32);
                Some(LRESULT(0))
            }
            WM_TRAY_CALLBACK => {
                // With NOTIFYICON_VERSION_4 the event is in the low word of lp.
                let event = (lp.0 & 0xFFFF) as u32;
                match event {
                    WM_LBUTTONUP => self.open_palette(),
                    WM_RBUTTONUP | WM_CONTEXTMENU => self.show_tray_menu(),
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_WINDOW_EVENT => {
                self.schedule_retile();
                Some(LRESULT(0))
            }
            WM_DRAG_START => {
                self.begin_drag(HWND(wp.0 as *mut core::ffi::c_void));
                Some(LRESULT(0))
            }
            WM_DRAG_END => {
                self.end_drag_for(Some(wp.0 as isize));
                Some(LRESULT(0))
            }
            WM_FOREGROUND_CHANGED => {
                // Not debounced: a dim overlay that lags behind the focused
                // window is worse than no dimming at all.
                self.update_dimming();
                Some(LRESULT(0))
            }
            WM_APPS_READY => {
                crate::log!("app scan finished: {} entries", self.apps.len());
                // Refresh an already-open palette. Opening it inside the first
                // second of a session otherwise showed commands and windows
                // but no applications, with nothing to say why.
                if self.palette.is_visible() {
                    let items = self.build_palette_items();
                    self.palette.show(items, &self.config);
                }
                Some(LRESULT(0))
            }
            palette::WM_PALETTE_ACCEPT => {
                self.handle_palette_result();
                Some(LRESULT(0))
            }
            palette::WM_PALETTE_CANCEL => Some(LRESULT(0)),
            crate::ui::theme_editor::WM_THEME_CHANGED => {
                // Applied and saved on every edit rather than on an OK button.
                // The editor's whole purpose is judging a theme as you change
                // it, and a preview that the rest of the program does not yet
                // agree with is a preview of nothing.
                self.config.appearance.overlay_theme = self.theme_editor.selected();
                self.config.appearance.custom_theme = self.theme_editor.custom();
                self.apply_overlay_theme();
                let _ = self.config.save();
                Some(LRESULT(0))
            }
            crate::ui::keys::WM_KEYS_CHANGED => {
                self.apply_hotkey_change();
                Some(LRESULT(0))
            }
            WM_TIMER => match wp.0 {
                TIMER_RETILE => {
                    // SAFETY: killing a timer we set.
                    unsafe {
                        let _ = KillTimer(Some(self.hwnd), TIMER_RETILE);
                    }
                    self.retile_all();
                    Some(LRESULT(0))
                }
                TIMER_SAVE => {
                    // The periodic save is the natural home for anything that
                    // should happen occasionally and never urgently. A check
                    // due only once a day does not deserve a timer of its own.
                    self.check_for_updates_if_due();
                    self.flush_store();
                    // Drop cached executable paths for windows that have gone.
                    crate::window::prune_caches();
                    // And exclusions for windows that no longer exist. Windows
                    // recycles HWND values aggressively, so a stale entry would
                    // silently stop tiling some unrelated future window.
                    self.requested
                        .retain(|h, _| crate::window::is_live(HWND(*h as *mut core::ffi::c_void)));
                    self.misses
                        .retain(|h, _| crate::window::is_live(HWND(*h as *mut core::ffi::c_void)));
                    self.unmanageable
                        .retain(|h, _| crate::window::is_live(HWND(*h as *mut core::ffi::c_void)));
                    self.mins
                        .retain(|h, _| crate::window::is_live(HWND(*h as *mut core::ffi::c_void)));
                    self.keys
                        .retain(|h, _| crate::window::is_live(HWND(*h as *mut core::ffi::c_void)));
                    self.last_rects
                        .retain(|h, _| crate::window::is_live(HWND(*h as *mut core::ffi::c_void)));
                    let before = self.floated.len();
                    self.floated
                        .retain(|h| crate::window::is_live(HWND(*h as *mut core::ffi::c_void)));
                    if self.floated.len() != before {
                        crate::log!(
                            "pruned {} closed window(s) from the exclusion set",
                            before - self.floated.len()
                        );
                    }
                    Some(LRESULT(0))
                }
                TIMER_DRAG => {
                    self.poll_drag();
                    Some(LRESULT(0))
                }
                TIMER_TRAY_RETRY => {
                    // SAFETY: killing the retry timer we set; re-armed by
                    // ensure_tray_icon if another attempt is warranted. Never
                    // TIMER_DRAG -- a drag may be in progress.
                    unsafe {
                        let _ = KillTimer(Some(self.hwnd), TIMER_TRAY_RETRY);
                    }
                    self.ensure_tray_icon();
                    Some(LRESULT(0))
                }
                _ => None,
            },
            // The display arrangement changed: re-derive everything.
            WM_DISPLAYCHANGE | WM_DPICHANGED => {
                self.orders.clear();
                self.splits.clear();
                self.trees.clear();
                self.retile_all();
                self.update_dimming();
                Some(LRESULT(0))
            }
            WM_SETTINGCHANGE => {
                // Covers work-area changes (taskbar auto-hide, appbars).
                self.schedule_retile();
                Some(LRESULT(0))
            }
            // Save state when Windows is shutting down or the user logs off.
            WM_ENDSESSION => {
                self.flush_store();
                Some(LRESULT(0))
            }
            WM_CLOSE | WM_DESTROY => {
                self.exit();
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.flush_store();
        self.unregister_hotkeys();
        for h in self.hooks.drain(..) {
            // SAFETY: every handle came from SetWinEventHook above.
            unsafe {
                let _ = UnhookWinEvent(h);
            }
        }
        // SAFETY: killing timers we set and destroying our own window.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_RETILE);
            let _ = KillTimer(Some(self.hwnd), TIMER_SAVE);
            let _ = KillTimer(Some(self.hwnd), TIMER_TRAY_RETRY);
            let _ = KillTimer(Some(self.hwnd), TIMER_DRAG);
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
        }
        crate::ui::hover::uninstall();
        HOST_HWND.store(0, Ordering::Release);
    }
}

fn create_host_window() -> Option<HWND> {
    if !crate::ui::theme::class_exists(CLASS_NAME) {
        let _ = crate::ui::theme::register_class(CLASS_NAME, host_wndproc);
    }
    let class = crate::util::WideStr::new(CLASS_NAME);
    let title = crate::util::WideStr::new("SuperTile");
    // SAFETY: a hidden, zero-size top-level window. Not HWND_MESSAGE, because
    // those do not receive the TaskbarCreated broadcast.
    let hwnd = unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.as_pcwstr(),
            title.as_pcwstr(),
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
    Some(hwnd)
}

/// WinEvent callback. Posts to the host window and returns immediately.
unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // Only whole-window events matter; OBJID_WINDOW is 0.
    if id_object != 0 || hwnd.is_invalid() {
        return;
    }
    match event {
        EVENT_SYSTEM_FOREGROUND
        | EVENT_SYSTEM_MINIMIZESTART
        | EVENT_SYSTEM_MINIMIZEEND
        | EVENT_SYSTEM_MOVESIZESTART
        | EVENT_SYSTEM_MOVESIZEEND
        | EVENT_OBJECT_CREATE
        | EVENT_OBJECT_DESTROY
        | EVENT_OBJECT_SHOW
        | EVENT_OBJECT_HIDE
        | EVENT_OBJECT_CLOAKED
        | EVENT_OBJECT_UNCLOAKED => {}
        _ => return,
    }
    let host = HOST_HWND.load(Ordering::Acquire);
    if host == 0 {
        return;
    }
    if event == EVENT_SYSTEM_MOVESIZESTART || event == EVENT_SYSTEM_MOVESIZEEND {
        let msg = if event == EVENT_SYSTEM_MOVESIZESTART {
            WM_DRAG_START
        } else {
            WM_DRAG_END
        };
        // SAFETY: `host` is the live host window for the process lifetime.
        unsafe {
            let _ = PostMessageW(
                Some(HWND(host as *mut core::ffi::c_void)),
                msg,
                WPARAM(hwnd.0 as usize),
                LPARAM(0),
            );
        }
        return;
    }
    if event == EVENT_SYSTEM_FOREGROUND {
        // SAFETY: `host` is the live host window for the process lifetime.
        unsafe {
            let _ = PostMessageW(
                Some(HWND(host as *mut core::ffi::c_void)),
                WM_FOREGROUND_CHANGED,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
    // SAFETY: `host` is the live host window for the process lifetime; posting
    // is thread-safe and does not block.
    unsafe {
        let _ = PostMessageW(
            Some(HWND(host as *mut core::ffi::c_void)),
            WM_WINDOW_EVENT,
            WPARAM(0),
            LPARAM(0),
        );
    }
}

/// # Safety
/// Only valid for windows of class [`CLASS_NAME`].
unsafe fn app_from(hwnd: HWND) -> Option<&'static mut App> {
    // SAFETY: reads our own window's userdata slot, which holds either
    // the App pointer stored during construction or zero.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut App;
    if ptr.is_null() {
        None
    } else {
        // SAFETY: null-checked above. The pointee is the boxed App, which
        // outlives its window; see this function's `# Safety` section.
        Some(unsafe { &mut *ptr })
    }
}

unsafe extern "system" fn host_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // Handled before App is borrowed. WM_MENUSELECT arrives from inside
    // TrackPopupMenuEx's modal loop, which runs while App::show_tray_menu
    // already holds &mut self; reaching App here would alias it. See
    // crate::ui::hover for the full explanation.
    if msg == WM_MENUSELECT {
        crate::ui::hover::on_menu_select(wp, lp);
        return LRESULT(0);
    }

    // A modal loop is running and App is already mutably borrowed further up
    // the stack. Handling anything here would alias that borrow, so nothing
    // is handled: the message goes to DefWindowProcW and, if it matters, the
    // sender will see it again after the loop ends.
    if in_modal_loop() {
        // SAFETY: default handling only; App is not touched.
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }

    // SAFETY: userdata holds the boxed App once construction finishes; before
    // that it is null and messages fall through to DefWindowProcW.
    if let Some(app) = unsafe { app_from(hwnd) } {
        if let Some(r) = app.handle(msg, wp, lp) {
            return r;
        }
    }
    // SAFETY: default handling for messages App did not claim; every
    // argument came from the system unchanged.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// Absolute paths to the terminals worth trying, best first.
///
/// Resolved from the system directory and the user profile rather than left to
/// `ShellExecuteW`'s name resolution — see [`App::launch_terminal`].
fn terminal_candidates() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();

    // Windows Terminal ships as an execution alias under the user's profile.
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        out.push(std::path::PathBuf::from(&local).join(r"Microsoft\WindowsApps\wt.exe"));
    }

    // %SystemRoot%\System32 via the API, not the environment: SystemRoot is
    // inherited and therefore attacker-influenceable in a crafted environment.
    let mut buf = [0u16; 260];
    // SAFETY: buf is a valid 260-element array; the call reports the length
    // written and never exceeds it.
    let n =
        unsafe { windows::Win32::System::SystemInformation::GetSystemDirectoryW(Some(&mut buf)) };
    if n > 0 && (n as usize) < buf.len() {
        let sys = std::path::PathBuf::from(crate::util::wide_to_string(&buf[..n as usize]));
        out.push(sys.join(r"WindowsPowerShell\v1.0\powershell.exe"));
        out.push(sys.join("cmd.exe"));
    }
    out
}

/// Start a program through the shell, without waiting for it.
fn launch_detached(exe: &str) -> bool {
    use windows::Win32::UI::Shell::ShellExecuteW;
    let target = crate::util::WideStr::new(exe);
    let verb = crate::util::WideStr::new("open");
    // SAFETY: both strings outlive the call. ShellExecuteW returns a
    // pseudo-HINSTANCE; values <= 32 are documented failures.
    let r = unsafe {
        ShellExecuteW(
            None,
            verb.as_pcwstr(),
            target.as_pcwstr(),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    r.0 as usize > 32
}

fn message_box(title: &str, body: &str) {
    // MessageBoxW pumps messages too; the same guard applies.
    let _modal = ModalGuard::enter();
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONWARNING, MB_OK};
    let t = crate::util::WideStr::new(title);
    let b = crate::util::WideStr::new(body);
    // SAFETY: both strings outlive the call.
    unsafe {
        MessageBoxW(None, b.as_pcwstr(), t.as_pcwstr(), MB_OK | MB_ICONWARNING);
    }
}

fn open_path(target: &str) {
    use windows::Win32::UI::Shell::ShellExecuteW;
    let t = crate::util::WideStr::new(target);
    // SAFETY: the string outlives the call.
    unsafe {
        let _ = ShellExecuteW(
            None,
            PCWSTR::null(),
            t.as_pcwstr(),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

/// Run the message loop until `WM_QUIT`.
pub fn run() -> i32 {
    let mut msg = MSG::default();
    // SAFETY: standard Win32 message loop; GetMessageW returns 0 on WM_QUIT
    // and -1 on error, both of which end the loop.
    unsafe {
        loop {
            let r = GetMessageW(&mut msg, None, 0, 0);
            if r.0 <= 0 {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    msg.wParam.0 as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::Disposition;

    fn win(hwnd: isize) -> WindowInfo {
        WindowInfo {
            hwnd,
            title: format!("w{hwnd}"),
            class: "C".into(),
            exe: "a.exe".into(),
            rect: Rect::new(0, 0, 100, 100),
            frame: Rect::new(0, 0, 100, 100),
            disposition: Disposition::Tile,
            monitor: 0,
        }
    }

    #[test]
    fn reconcile_appends_new_windows_in_order() {
        let mut o = MonitorOrder::default();
        let fresh = o.reconcile(&[win(1), win(2), win(3)]);
        assert_eq!(o.order, vec![1, 2, 3]);
        assert_eq!(fresh, vec![1, 2, 3]);
    }

    #[test]
    fn reconcile_is_stable_across_repeated_calls() {
        // The property that stops windows shuffling on every retile.
        let mut o = MonitorOrder::default();
        o.reconcile(&[win(1), win(2), win(3)]);
        let before = o.order.clone();
        // EnumWindows order changes with z-order; ours must not.
        let fresh = o.reconcile(&[win(3), win(1), win(2)]);
        assert_eq!(o.order, before);
        assert!(fresh.is_empty(), "nothing is new here");
    }

    #[test]
    fn reconcile_drops_closed_windows_and_keeps_the_rest_in_place() {
        let mut o = MonitorOrder::default();
        o.reconcile(&[win(1), win(2), win(3), win(4)]);
        o.reconcile(&[win(1), win(3), win(4)]);
        assert_eq!(o.order, vec![1, 3, 4]);
    }

    #[test]
    fn reconcile_reports_only_the_genuinely_new() {
        let mut o = MonitorOrder::default();
        o.reconcile(&[win(1), win(2)]);
        let fresh = o.reconcile(&[win(1), win(2), win(9)]);
        assert_eq!(fresh, vec![9]);
        assert_eq!(o.order, vec![1, 2, 9]);
    }

    #[test]
    fn reconcile_on_an_empty_desktop_clears_the_order() {
        let mut o = MonitorOrder::default();
        o.reconcile(&[win(1), win(2)]);
        let fresh = o.reconcile(&[]);
        assert!(o.order.is_empty());
        assert!(fresh.is_empty());
    }

    #[test]
    fn move_to_relocates_and_shifts() {
        let mut o = MonitorOrder::default();
        o.reconcile(&[win(1), win(2), win(3), win(4)]);
        o.move_to(4, 0);
        assert_eq!(o.order, vec![4, 1, 2, 3]);
        o.move_to(4, 2);
        assert_eq!(o.order, vec![1, 2, 4, 3]);
    }

    #[test]
    fn move_to_clamps_out_of_range_targets() {
        let mut o = MonitorOrder::default();
        o.reconcile(&[win(1), win(2)]);
        o.move_to(1, 99);
        assert_eq!(o.order, vec![2, 1]);
        // A window that is not present is ignored.
        o.move_to(42, 0);
        assert_eq!(o.order, vec![2, 1]);
    }

    #[test]
    fn move_to_the_same_slot_is_a_no_op() {
        let mut o = MonitorOrder::default();
        o.reconcile(&[win(1), win(2), win(3)]);
        o.move_to(2, 1);
        assert_eq!(o.order, vec![1, 2, 3]);
    }

    #[test]
    fn swap_exchanges_two_slots() {
        let mut o = MonitorOrder::default();
        o.reconcile(&[win(1), win(2), win(3)]);
        o.swap(0, 2);
        assert_eq!(o.order, vec![3, 2, 1]);
    }

    #[test]
    fn swap_ignores_out_of_range_indices() {
        let mut o = MonitorOrder::default();
        o.reconcile(&[win(1), win(2)]);
        o.swap(0, 5);
        o.swap(9, 0);
        assert_eq!(o.order, vec![1, 2]);
    }

    #[test]
    fn index_of_finds_and_misses_correctly() {
        let mut o = MonitorOrder::default();
        o.reconcile(&[win(7), win(8)]);
        assert_eq!(o.index_of(8), Some(1));
        assert_eq!(o.index_of(99), None);
    }

    #[test]
    fn the_window_event_message_is_in_the_app_range() {
        const { assert!(WM_WINDOW_EVENT >= WM_APP) };
        const { assert!(WM_APPS_READY >= WM_APP) };
        assert_ne!(WM_WINDOW_EVENT, WM_APPS_READY);
        // Must not collide with the palette's messages.
        assert_ne!(WM_WINDOW_EVENT, palette::WM_PALETTE_ACCEPT);
        assert_ne!(WM_APPS_READY, palette::WM_PALETTE_CANCEL);
        assert_ne!(WM_WINDOW_EVENT, WM_TRAY_CALLBACK);
        assert_ne!(WM_FOREGROUND_CHANGED, WM_WINDOW_EVENT);
        assert_ne!(WM_FOREGROUND_CHANGED, WM_APPS_READY);
        let all = [
            WM_WINDOW_EVENT,
            WM_APPS_READY,
            WM_FOREGROUND_CHANGED,
            WM_DRAG_START,
            WM_DRAG_END,
            WM_TRAY_CALLBACK,
            palette::WM_PALETTE_ACCEPT,
            palette::WM_PALETTE_CANCEL,
        ];
        let mut v = all.to_vec();
        v.sort_unstable();
        v.dedup();
        assert_eq!(v.len(), all.len(), "application messages must not collide");
    }

    #[test]
    fn timer_ids_are_distinct() {
        let ids = [TIMER_RETILE, TIMER_SAVE, TIMER_TRAY_RETRY, TIMER_DRAG];
        let mut v = ids.to_vec();
        v.sort_unstable();
        v.dedup();
        assert_eq!(v.len(), ids.len(), "timer ids must not collide");
    }

    #[test]
    fn the_tray_retry_budget_covers_a_slow_logon() {
        // Explorer can take the better part of a minute to build the taskbar
        // on a cold boot; the budget must outlast that.
        const { assert!(TRAY_RETRY_LIMIT * TRAY_RETRY_MS >= 60_000) };
    }
}

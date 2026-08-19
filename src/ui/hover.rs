//! Hover highlighting for the tray's window list.
//!
//! ## Why this is not a field on `App`
//!
//! `TrackPopupMenuEx` runs its own modal message loop. While it is up,
//! `WM_MENUSELECT` is delivered to the menu's owner — our host window — and
//! therefore re-enters the window procedure *while `App::show_tray_menu` still
//! holds `&mut self`*. Reaching `App` again from there would create a second
//! `&mut` to the same value: undefined behaviour, and exactly the kind of bug
//! that shows up as an inexplicable crash months later.
//!
//! The hover state therefore lives here, in thread-local storage that `App`
//! does not own, and the window procedure handles `WM_MENUSELECT` *before* it
//! touches `App` at all. Nothing is borrowed twice because nothing is shared.
//!
//! Thread-local rather than `static`: every window and menu involved belongs to
//! the UI thread, so there is no cross-thread access to synchronise.

use std::cell::RefCell;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{GetSubMenu, HMENU, MF_POPUP};

use crate::ui::highlight::Highlight;
use crate::ui::tip::{Tip, TipText};

struct Ctx {
    highlight: Box<Highlight>,
    tip: Box<Tip>,
    /// Menu command id -> window handle, for leaf items.
    by_id: Vec<(u32, isize)>,
    /// Submenu handle -> window handle, for the per-window popups.
    by_submenu: Vec<(isize, isize)>,
    /// Menu command id -> tooltip. Covers items that have no window at all.
    tips: Vec<(u32, TipText)>,
}

thread_local! {
    static CTX: RefCell<Option<Ctx>> = const { RefCell::new(None) };
}

/// Create the overlay. Call once, on the UI thread, at startup.
pub fn install(dark: bool) {
    CTX.with(|c| {
        let mut c = c.borrow_mut();
        if c.is_none() {
            let highlight = Highlight::new(dark);
            let tip = Tip::new(dark);
            *c = Some(Ctx {
                highlight,
                tip,
                by_id: Vec::new(),
                by_submenu: Vec::new(),
                tips: Vec::new(),
            });
        }
    });
}

/// Tear the overlay down. Call before the process exits.
pub fn uninstall() {
    CTX.with(|c| {
        *c.borrow_mut() = None;
    });
}

pub fn set_dark(dark: bool) {
    CTX.with(|c| {
        if let Some(ctx) = c.borrow().as_ref() {
            ctx.highlight.set_dark(dark);
            ctx.tip.set_dark(dark);
        }
    });
}

/// The overlay's own handle, so the tiler can skip it.
pub fn overlay_hwnd() -> isize {
    CTX.with(|c| {
        c.borrow()
            .as_ref()
            .map(|x| x.highlight.hwnd().0 as isize)
            .unwrap_or(0)
    })
}

/// The tooltip's handle, which must also be excluded from tiling.
pub fn tip_hwnd() -> isize {
    CTX.with(|c| {
        c.borrow()
            .as_ref()
            .map(|x| x.tip.hwnd().0 as isize)
            .unwrap_or(0)
    })
}

/// Start a menu session: forget any previous mapping.
pub fn begin_menu() {
    CTX.with(|c| {
        if let Some(ctx) = c.borrow_mut().as_mut() {
            ctx.by_id.clear();
            ctx.by_submenu.clear();
            ctx.tips.clear();
        }
    });
}

/// Attach a tooltip to a menu command id.
pub fn map_tip(id: u32, tip: TipText) {
    CTX.with(|c| {
        if let Some(ctx) = c.borrow_mut().as_mut() {
            ctx.tips.push((id, tip));
        }
    });
}

/// Associate a leaf command id with the window it acts on.
pub fn map_id(id: u32, hwnd: isize) {
    CTX.with(|c| {
        if let Some(ctx) = c.borrow_mut().as_mut() {
            ctx.by_id.push((id, hwnd));
        }
    });
}

/// Associate a per-window submenu with the window it represents.
pub fn map_submenu(hmenu: isize, hwnd: isize) {
    CTX.with(|c| {
        if let Some(ctx) = c.borrow_mut().as_mut() {
            ctx.by_submenu.push((hmenu, hwnd));
        }
    });
}

/// End the session: drop the mapping and hide the outline.
pub fn end_menu() {
    CTX.with(|c| {
        if let Some(ctx) = c.borrow_mut().as_mut() {
            ctx.by_id.clear();
            ctx.by_submenu.clear();
            ctx.tips.clear();
            ctx.highlight.hide();
            ctx.tip.hide();
        }
    });
}

/// Handle `WM_MENUSELECT`, outlining whichever window is hovered.
///
/// Returns true when the message was consumed.
pub fn on_menu_select(wp: WPARAM, lp: LPARAM) -> bool {
    let id = (wp.0 & 0xFFFF) as u32;
    let flags = ((wp.0 >> 16) & 0xFFFF) as u32;
    let hmenu = lp.0;

    // Windows signals "menu closed" with flags == 0xFFFF and a null menu.
    if flags == 0xFFFF && hmenu == 0 {
        hide();
        hide_tip();
        return true;
    }

    let is_popup = flags & MF_POPUP.0 != 0;

    // A popup's `id` is a *position*, not a command id, so resolve the
    // submenu handle before looking anything up.
    let submenu: Option<isize> = if is_popup {
        // SAFETY: `hmenu` is the live HMENU Windows passed in lParam.
        let sub = unsafe { GetSubMenu(HMENU(hmenu as *mut core::ffi::c_void), id as i32) };
        Some(sub.0 as isize)
    } else {
        None
    };

    let (target, tip) = CTX.with(|c| {
        let ctx_ref = c.borrow();
        let Some(ctx) = ctx_ref.as_ref() else {
            return (None, None);
        };
        match submenu {
            Some(sub) => (
                ctx.by_submenu
                    .iter()
                    .find(|(m, _)| *m == sub)
                    .map(|(_, h)| *h),
                None,
            ),
            None => (
                ctx.by_id.iter().find(|(i, _)| *i == id).map(|(_, h)| *h),
                ctx.tips
                    .iter()
                    .find(|(i, _)| *i == id)
                    .map(|(_, t)| t.clone()),
            ),
        }
    });

    match target {
        Some(h) => show_for(h),
        None => hide(),
    }

    match tip {
        Some(t) => show_tip(t),
        None => hide_tip(),
    }
    true
}

fn show_tip(text: TipText) {
    let mut pt = windows::Win32::Foundation::POINT::default();
    // SAFETY: valid out-param.
    let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt) };
    CTX.with(|c| {
        if let Some(ctx) = c.borrow().as_ref() {
            ctx.tip.show(text, pt);
        }
    });
}

fn hide_tip() {
    CTX.with(|c| {
        if let Some(ctx) = c.borrow().as_ref() {
            ctx.tip.hide();
        }
    });
}

fn show_for(hwnd: isize) {
    let h = HWND(hwnd as *mut core::ffi::c_void);
    if !crate::window::is_live(h) {
        hide();
        return;
    }
    // The *visible* frame, not GetWindowRect: the latter includes the invisible
    // resize border, which would draw the outline noticeably offset.
    let rect = crate::window::visible_frame(h);
    let dpi = crate::monitor::from_window(h).map(|m| m.dpi).unwrap_or(96);
    CTX.with(|c| {
        if let Some(ctx) = c.borrow().as_ref() {
            ctx.highlight.show_around(rect, dpi);
        }
    });
}

fn hide() {
    CTX.with(|c| {
        if let Some(ctx) = c.borrow().as_ref() {
            ctx.highlight.hide();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_is_idempotent_and_yields_an_overlay() {
        install(true);
        let first = overlay_hwnd();
        assert_ne!(first, 0, "overlay window should exist");
        install(true);
        assert_eq!(
            overlay_hwnd(),
            first,
            "install must not create a second overlay"
        );
        uninstall();
        assert_eq!(overlay_hwnd(), 0);
    }

    #[test]
    fn mappings_are_cleared_between_sessions() {
        install(false);
        begin_menu();
        map_id(1234, 999);
        map_submenu(555, 999);
        // A fresh session must not resolve last session's ids.
        begin_menu();
        let found = CTX.with(|c| {
            let b = c.borrow();
            let ctx = b.as_ref().unwrap();
            (ctx.by_id.len(), ctx.by_submenu.len())
        });
        assert_eq!(found, (0, 0));
        end_menu();
        uninstall();
    }

    #[test]
    fn a_menu_close_message_is_consumed_and_hides_the_outline() {
        install(false);
        begin_menu();
        // flags == 0xFFFF, null menu => menu dismissed.
        assert!(on_menu_select(WPARAM(0xFFFF << 16), LPARAM(0)));
        end_menu();
        uninstall();
    }

    #[test]
    fn hovering_an_unmapped_id_hides_rather_than_panicking() {
        install(false);
        begin_menu();
        assert!(on_menu_select(WPARAM(4242), LPARAM(0x1234)));
        end_menu();
        uninstall();
    }

    #[test]
    fn a_dead_window_handle_does_not_draw() {
        install(false);
        begin_menu();
        map_id(7, 0xDEAD_BEEF);
        assert!(on_menu_select(WPARAM(7), LPARAM(0x1234)));
        // No panic, and nothing shown for a handle that is not a window.
        let visible = CTX.with(|c| {
            c.borrow()
                .as_ref()
                .map(|x| x.highlight.is_visible())
                .unwrap_or(false)
        });
        assert!(!visible);
        end_menu();
        uninstall();
    }

    #[test]
    fn a_tooltip_is_created_alongside_the_overlay() {
        install(true);
        assert_ne!(tip_hwnd(), 0);
        assert_ne!(
            tip_hwnd(),
            overlay_hwnd(),
            "tip and highlight are separate windows"
        );
        uninstall();
        assert_eq!(tip_hwnd(), 0);
    }

    #[test]
    fn hovering_an_item_with_a_tip_shows_it_and_moving_away_hides_it() {
        install(false);
        begin_menu();
        map_tip(
            77,
            TipText {
                title: "Retile now".into(),
                keys: "Ctrl+Alt+T".into(),
                detail: String::new(),
            },
        );
        assert!(on_menu_select(WPARAM(77), LPARAM(0x1234)));
        let shown = CTX.with(|c| {
            c.borrow()
                .as_ref()
                .map(|x| x.tip.is_visible())
                .unwrap_or(false)
        });
        assert!(shown, "tooltip should appear for a mapped id");

        // An id with no tip must dismiss it rather than leave it stale.
        assert!(on_menu_select(WPARAM(78), LPARAM(0x1234)));
        let still = CTX.with(|c| {
            c.borrow()
                .as_ref()
                .map(|x| x.tip.is_visible())
                .unwrap_or(false)
        });
        assert!(
            !still,
            "tooltip should hide when hovering an item without one"
        );
        end_menu();
        uninstall();
    }

    #[test]
    fn closing_the_menu_hides_the_tooltip() {
        install(false);
        begin_menu();
        map_tip(
            5,
            TipText {
                title: "x".into(),
                ..Default::default()
            },
        );
        on_menu_select(WPARAM(5), LPARAM(0x1234));
        on_menu_select(WPARAM(0xFFFF << 16), LPARAM(0));
        let visible = CTX.with(|c| {
            c.borrow()
                .as_ref()
                .map(|x| x.tip.is_visible())
                .unwrap_or(false)
        });
        assert!(!visible);
        end_menu();
        uninstall();
    }

    #[test]
    fn working_without_install_is_harmless() {
        uninstall();
        begin_menu();
        map_id(1, 2);
        map_submenu(3, 4);
        assert!(on_menu_select(WPARAM(1), LPARAM(0)));
        end_menu();
        assert_eq!(overlay_hwnd(), 0);
    }
}

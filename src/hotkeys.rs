//! Global hotkey parsing, formatting and registration.
//!
//! Bindings are stored in config as human-readable strings (`"Win+Alt+L"`) and
//! parsed into a modifier mask plus a virtual-key code. Parsing is total and
//! never panics: an unparseable binding is reported as an error and skipped so
//! one bad line in a hand-edited config cannot take the whole app down.

use std::fmt;
use std::str::FromStr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use windows::Win32::UI::Input::KeyboardAndMouse::*;

/// Actions that can be bound to a global hotkey.
///
/// The set and its naming follow **i3**, so an i3 user's muscle memory mostly
/// transfers. Where SuperTile deviates, [`BindingSpec::note`] says why, and
/// that note is written into the generated configuration file.
///
/// The discriminant doubles as the `RegisterHotKey` id, so it must be stable
/// within a run but need not be stable across versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(i32)]
pub enum Action {
    // --- launching ---
    Palette = 1,
    LaunchTerminal,
    // --- window management ---
    CloseWindow,
    FocusLeft,
    FocusDown,
    FocusUp,
    FocusRight,
    MoveLeft,
    MoveDown,
    MoveUp,
    MoveRight,
    // --- layout ---
    LayoutColumns,
    LayoutRows,
    LayoutStacking,
    LayoutTabbed,
    CycleLayout,
    ToggleFullscreen,
    ToggleFloat,
    GrowMaster,
    ShrinkMaster,
    IncreaseGaps,
    DecreaseGaps,
    // --- SuperTile specific ---
    Retile,
    TogglePause,
    ToggleDimming,
    ReloadConfig,
    Quit,
}

/// A default binding together with its i3 provenance.
///
/// `keys` is what SuperTile tries first. If `RegisterHotKey` refuses it —
/// because Windows or another application already owns the combination —
/// `fallbacks` are tried in order. That is the only conflict-avoidance
/// mechanism available: Windows offers no way to ask *who* owns a hotkey, so
/// attempting to claim it is the test.
pub struct BindingSpec {
    pub action: Action,
    /// SuperTile's first choice.
    pub keys: &'static str,
    /// The i3 binding this derives from, or `""` where i3 has no equivalent.
    pub i3: &'static str,
    /// Why this differs from i3. Empty when it is a faithful translation.
    pub note: &'static str,
    /// Tried in order when `keys` is unavailable.
    pub fallbacks: &'static [&'static str],
}

/// What `$mod` maps to, and why.
///
/// i3's `$mod` is usually `Mod4`, the Super/Windows key. On Windows 11 that is
/// unusable as a modifier on its own: the shell reserves `Win`+letter for
/// dozens of its own commands and claims them before any application sees the
/// keystroke. `Win+Alt` is the nearest combination the shell leaves alone.
pub const MOD_EXPLANATION: &str = concat!(
    "i3's $mod is normally the Super (Windows) key. Windows 11 reserves most ",
    "Win+<key> combinations for the shell -- Win+D, Win+E, Win+L, Win+R, ",
    "Win+S, Win+X, Win+1..9 and Win+arrows are all claimed before an ",
    "application sees them -- so SuperTile maps $mod to Win+Alt, which the ",
    "shell leaves almost entirely free. Everything after $mod matches i3."
);

/// Every default binding, in menu order.
pub const DEFAULTS: &[BindingSpec] = &[
    BindingSpec {
        action: Action::Palette,
        keys: "Win+Alt+D",
        i3: "$mod+d",
        note: "i3 runs dmenu; SuperTile opens its command palette, which also \
               searches open windows and SuperTile's own commands.",
        fallbacks: &["Ctrl+Alt+D", "Alt+Space", "Ctrl+Shift+Space"],
    },
    BindingSpec {
        action: Action::LaunchTerminal,
        keys: "Win+Alt+Enter",
        i3: "$mod+Return",
        note: "Opens Windows Terminal, falling back to PowerShell if it is not \
               installed.",
        fallbacks: &["Ctrl+Alt+Enter"],
    },
    BindingSpec {
        action: Action::CloseWindow,
        keys: "Win+Alt+Shift+Q",
        i3: "$mod+Shift+q",
        note: "Asks the window to close -- the same request as its X button -- \
               rather than killing the process as i3's kill does.",
        fallbacks: &["Ctrl+Alt+Shift+Q"],
    },
    BindingSpec {
        action: Action::FocusLeft,
        keys: "Win+Alt+H",
        i3: "$mod+h",
        note: "",
        fallbacks: &["Ctrl+Alt+H", "Win+Alt+Left", "Ctrl+Alt+Left"],
    },
    BindingSpec {
        action: Action::FocusDown,
        keys: "Win+Alt+J",
        i3: "$mod+j",
        note: "",
        fallbacks: &["Ctrl+Alt+J", "Win+Alt+Down", "Ctrl+Alt+Down"],
    },
    BindingSpec {
        action: Action::FocusUp,
        keys: "Win+Alt+K",
        i3: "$mod+k",
        note: "",
        fallbacks: &["Ctrl+Alt+K", "Win+Alt+Up", "Ctrl+Alt+Up"],
    },
    BindingSpec {
        action: Action::FocusRight,
        keys: "Win+Alt+L",
        i3: "$mod+l",
        note: "Win+L locks the workstation, but Win+Alt+L is not reserved, so \
               the i3 key survives here.",
        fallbacks: &["Ctrl+Alt+L", "Win+Alt+Right", "Ctrl+Alt+Right"],
    },
    BindingSpec {
        action: Action::MoveLeft,
        keys: "Win+Alt+Shift+H",
        i3: "$mod+Shift+h",
        note: "",
        fallbacks: &["Ctrl+Alt+Shift+H", "Win+Alt+Shift+Left"],
    },
    BindingSpec {
        action: Action::MoveDown,
        keys: "Win+Alt+Shift+J",
        i3: "$mod+Shift+j",
        note: "",
        fallbacks: &["Ctrl+Alt+Shift+J", "Win+Alt+Shift+Down"],
    },
    BindingSpec {
        action: Action::MoveUp,
        keys: "Win+Alt+Shift+K",
        i3: "$mod+Shift+k",
        note: "",
        fallbacks: &["Ctrl+Alt+Shift+K", "Win+Alt+Shift+Up"],
    },
    BindingSpec {
        action: Action::MoveRight,
        keys: "Win+Alt+Shift+L",
        i3: "$mod+Shift+l",
        note: "",
        fallbacks: &["Ctrl+Alt+Shift+L", "Win+Alt+Shift+Right"],
    },
    BindingSpec {
        action: Action::LayoutColumns,
        keys: "Win+Alt+B",
        i3: "$mod+b",
        note: "i3 sets the split direction for the next window. SuperTile has \
               no manual split tree, so this selects the Columns layout, which \
               is what a horizontal split produces.",
        fallbacks: &["Ctrl+Alt+B"],
    },
    BindingSpec {
        action: Action::LayoutRows,
        keys: "Win+Alt+V",
        i3: "$mod+v",
        note: "As above: i3's vertical split becomes the Rows layout.",
        fallbacks: &["Ctrl+Alt+V"],
    },
    BindingSpec {
        action: Action::LayoutStacking,
        keys: "Win+Alt+S",
        i3: "$mod+s",
        note: "i3 stacks windows with a title bar each. SuperTile has no \
               stacked container, so this selects Monocle -- one window filling \
               the work area, switched with the focus keys.",
        fallbacks: &["Ctrl+Alt+S"],
    },
    BindingSpec {
        action: Action::LayoutTabbed,
        keys: "Win+Alt+W",
        i3: "$mod+w",
        note: "i3's tabbed layout has no SuperTile equivalent either, so it \
               also selects Monocle. Use the taskbar or the palette to switch.",
        fallbacks: &["Ctrl+Alt+W"],
    },
    BindingSpec {
        action: Action::CycleLayout,
        keys: "Win+Alt+E",
        i3: "$mod+e",
        note: "i3 toggles split orientation. SuperTile cycles through all six \
               layouts, which is the closest useful equivalent.",
        fallbacks: &["Ctrl+Alt+E"],
    },
    BindingSpec {
        action: Action::ToggleFullscreen,
        keys: "Win+Alt+F",
        i3: "$mod+f",
        note: "Toggles the focused monitor between Monocle and the layout it \
               was using, rather than making one window truly fullscreen.",
        fallbacks: &["Ctrl+Alt+F"],
    },
    BindingSpec {
        action: Action::ToggleFloat,
        keys: "Win+Alt+Shift+Space",
        i3: "$mod+Shift+space",
        note: "",
        fallbacks: &["Ctrl+Alt+Shift+Space", "Win+Alt+Shift+F"],
    },
    BindingSpec {
        action: Action::GrowMaster,
        keys: "Win+Alt+=",
        i3: "$mod+r then l",
        note: "i3 enters a resize mode. SuperTile has no modal input, so \
               growing and shrinking the master pane are direct bindings.",
        fallbacks: &["Ctrl+Alt+="],
    },
    BindingSpec {
        action: Action::ShrinkMaster,
        keys: "Win+Alt+-",
        i3: "$mod+r then h",
        note: "See Grow master pane.",
        fallbacks: &["Ctrl+Alt+-"],
    },
    BindingSpec {
        action: Action::IncreaseGaps,
        keys: "Win+Alt+Shift+=",
        i3: "",
        note: "i3 has no gap bindings; i3-gaps puts them behind a resize mode. \
               SuperTile binds them directly, one step above the master-pane \
               keys.",
        fallbacks: &["Ctrl+Alt+Shift+="],
    },
    BindingSpec {
        action: Action::DecreaseGaps,
        keys: "Win+Alt+Shift+-",
        i3: "",
        note: "See Increase gaps.",
        fallbacks: &["Ctrl+Alt+Shift+-"],
    },
    BindingSpec {
        action: Action::Retile,
        keys: "Win+Alt+T",
        i3: "",
        note: "No i3 equivalent: i3 tiles continuously and never needs a manual \
               pass. SuperTile manages windows it does not own, so an explicit \
               retile is occasionally useful.",
        fallbacks: &["Ctrl+Alt+T"],
    },
    BindingSpec {
        action: Action::TogglePause,
        keys: "Win+Alt+P",
        i3: "",
        note: "No i3 equivalent: on Windows you sometimes need the tiler to \
               stop touching your windows for a while.",
        fallbacks: &["Ctrl+Alt+P"],
    },
    BindingSpec {
        action: Action::ToggleDimming,
        keys: "Win+Alt+Shift+D",
        i3: "",
        note: "No i3 equivalent. Dims every window except the focused one, with \
               a separate level for the taskbar so it stays usable.",
        fallbacks: &["Ctrl+Alt+Shift+D"],
    },
    BindingSpec {
        action: Action::ReloadConfig,
        keys: "Win+Alt+Shift+C",
        i3: "$mod+Shift+c",
        note: "",
        fallbacks: &["Ctrl+Alt+Shift+C"],
    },
    BindingSpec {
        action: Action::Quit,
        keys: "Win+Alt+Shift+E",
        i3: "$mod+Shift+e",
        note: "i3 asks for confirmation before exiting; SuperTile exits \
               immediately, since nothing is lost by restarting it.",
        fallbacks: &["Ctrl+Alt+Shift+E"],
    },
];

impl Action {
    pub const ALL: [Action; 27] = [
        Action::Palette,
        Action::LaunchTerminal,
        Action::CloseWindow,
        Action::FocusLeft,
        Action::FocusDown,
        Action::FocusUp,
        Action::FocusRight,
        Action::MoveLeft,
        Action::MoveDown,
        Action::MoveUp,
        Action::MoveRight,
        Action::LayoutColumns,
        Action::LayoutRows,
        Action::LayoutStacking,
        Action::LayoutTabbed,
        Action::CycleLayout,
        Action::ToggleFullscreen,
        Action::ToggleFloat,
        Action::GrowMaster,
        Action::ShrinkMaster,
        Action::IncreaseGaps,
        Action::DecreaseGaps,
        Action::Retile,
        Action::TogglePause,
        Action::ToggleDimming,
        Action::ReloadConfig,
        Action::Quit,
    ];

    /// Stable key used in the configuration file.
    pub fn config_key(&self) -> &'static str {
        match self {
            Action::Palette => "palette",
            Action::LaunchTerminal => "launch_terminal",
            Action::CloseWindow => "close_window",
            Action::FocusLeft => "focus_left",
            Action::FocusDown => "focus_down",
            Action::FocusUp => "focus_up",
            Action::FocusRight => "focus_right",
            Action::MoveLeft => "move_left",
            Action::MoveDown => "move_down",
            Action::MoveUp => "move_up",
            Action::MoveRight => "move_right",
            Action::LayoutColumns => "layout_columns",
            Action::LayoutRows => "layout_rows",
            Action::LayoutStacking => "layout_stacking",
            Action::LayoutTabbed => "layout_tabbed",
            Action::CycleLayout => "cycle_layout",
            Action::ToggleFullscreen => "toggle_fullscreen",
            Action::ToggleFloat => "toggle_float",
            Action::GrowMaster => "grow_master",
            Action::ShrinkMaster => "shrink_master",
            Action::IncreaseGaps => "increase_gaps",
            Action::DecreaseGaps => "decrease_gaps",
            Action::Retile => "retile",
            Action::TogglePause => "toggle_pause",
            Action::ToggleDimming => "toggle_dimming",
            Action::ReloadConfig => "reload_config",
            Action::Quit => "quit",
        }
    }

    /// Human label for menus, the palette and the settings UI.
    pub fn label(&self) -> &'static str {
        match self {
            Action::Palette => "Open command palette",
            Action::LaunchTerminal => "Open a terminal",
            Action::CloseWindow => "Close focused window",
            Action::FocusLeft => "Focus left",
            Action::FocusDown => "Focus down",
            Action::FocusUp => "Focus up",
            Action::FocusRight => "Focus right",
            Action::MoveLeft => "Move window left",
            Action::MoveDown => "Move window down",
            Action::MoveUp => "Move window up",
            Action::MoveRight => "Move window right",
            Action::LayoutColumns => "Layout: Columns",
            Action::LayoutRows => "Layout: Rows",
            Action::LayoutStacking => "Layout: Stacking (Monocle)",
            Action::LayoutTabbed => "Layout: Tabbed (Monocle)",
            Action::CycleLayout => "Cycle layout",
            Action::ToggleFullscreen => "Toggle fullscreen",
            Action::ToggleFloat => "Float / unfloat window",
            Action::GrowMaster => "Grow master pane",
            Action::ShrinkMaster => "Shrink master pane",
            Action::IncreaseGaps => "Increase gaps",
            Action::DecreaseGaps => "Decrease gaps",
            Action::Retile => "Retile now",
            Action::TogglePause => "Pause / resume tiling",
            Action::ToggleDimming => "Toggle focus dimming",
            Action::ReloadConfig => "Reload configuration",
            Action::Quit => "Exit SuperTile",
        }
    }

    /// The default binding specification for this action.
    pub fn spec(&self) -> &'static BindingSpec {
        DEFAULTS
            .iter()
            .find(|b| b.action == *self)
            // Guaranteed by the `every_action_has_a_spec` test.
            .expect("every Action must appear in DEFAULTS")
    }

    pub fn default_binding(&self) -> &'static str {
        self.spec().keys
    }

    pub fn i3_binding(&self) -> &'static str {
        self.spec().i3
    }

    pub fn note(&self) -> &'static str {
        self.spec().note
    }

    pub fn fallbacks(&self) -> &'static [&'static str] {
        self.spec().fallbacks
    }

    pub fn from_hotkey_id(id: i32) -> Option<Action> {
        Action::ALL.into_iter().find(|a| *a as i32 == id)
    }
}

/// A parsed key binding: modifier mask plus virtual-key code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub mods: u32,
    pub vk: u16,
}

/// Why a binding string could not be parsed. Surfaced verbatim in Settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    Empty,
    UnknownKey(String),
    UnknownModifier(String),
    NoKey,
    ModifierAfterKey(String),
    /// A binding with no modifier would swallow a bare keystroke system-wide.
    NoModifier(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Empty => write!(f, "binding is empty"),
            ParseError::UnknownKey(k) => write!(f, "unknown key '{k}'"),
            ParseError::UnknownModifier(m) => write!(f, "unknown modifier '{m}'"),
            ParseError::NoKey => write!(f, "binding has modifiers but no key"),
            ParseError::ModifierAfterKey(m) => {
                write!(f, "modifier '{m}' must come before the key")
            }
            ParseError::NoModifier(k) => {
                write!(
                    f,
                    "'{k}' needs Win, Ctrl or Alt (Shift alone is not enough)"
                )
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// Map a modifier token to its `MOD_*` bit.
fn modifier_bit(token: &str) -> Option<u32> {
    Some(match token {
        "ctrl" | "control" | "ctl" => MOD_CONTROL.0,
        "alt" | "menu" => MOD_ALT.0,
        "shift" => MOD_SHIFT.0,
        "win" | "super" | "meta" | "cmd" | "lwin" => MOD_WIN.0,
        _ => return None,
    })
}

/// Map a key token to a virtual-key code.
fn key_code(token: &str) -> Option<u16> {
    // Single ASCII letter or digit.
    if token.len() == 1 {
        let c = token.as_bytes()[0];
        if c.is_ascii_alphabetic() {
            return Some(c.to_ascii_uppercase() as u16);
        }
        if c.is_ascii_digit() {
            return Some(c as u16);
        }
    }
    // Function keys.
    if let Some(rest) = token.strip_prefix('f') {
        if let Ok(n) = rest.parse::<u8>() {
            if (1..=24).contains(&n) {
                return Some(VK_F1.0 + (n as u16 - 1));
            }
        }
    }
    Some(
        match token {
            "space" | "spacebar" => VK_SPACE,
            "enter" | "return" => VK_RETURN,
            "tab" => VK_TAB,
            "esc" | "escape" => VK_ESCAPE,
            "backspace" | "back" => VK_BACK,
            "delete" | "del" => VK_DELETE,
            "insert" | "ins" => VK_INSERT,
            "home" => VK_HOME,
            "end" => VK_END,
            "pageup" | "pgup" | "prior" => VK_PRIOR,
            "pagedown" | "pgdn" | "next" => VK_NEXT,
            "left" => VK_LEFT,
            "right" => VK_RIGHT,
            "up" => VK_UP,
            "down" => VK_DOWN,
            "printscreen" | "prtsc" => VK_SNAPSHOT,
            "pause" | "break" => VK_PAUSE,
            "capslock" => VK_CAPITAL,
            "numlock" => VK_NUMLOCK,
            "scrolllock" => VK_SCROLL,
            "apps" | "menu_key" => VK_APPS,
            // OEM punctuation. Note these are physical keys on a US layout;
            // on other layouts the same VK may print a different character,
            // which is why Settings shows the parsed key back to the user.
            "-" | "minus" => VK_OEM_MINUS,
            "=" | "plus" | "equals" => VK_OEM_PLUS,
            "[" | "lbracket" => VK_OEM_4,
            "]" | "rbracket" => VK_OEM_6,
            ";" | "semicolon" => VK_OEM_1,
            "'" | "quote" => VK_OEM_7,
            "," | "comma" => VK_OEM_COMMA,
            "." | "period" | "dot" => VK_OEM_PERIOD,
            "/" | "slash" => VK_OEM_2,
            "\\" | "backslash" => VK_OEM_5,
            "`" | "grave" | "backtick" => VK_OEM_3,
            "numpad0" => VK_NUMPAD0,
            "numpad1" => VK_NUMPAD1,
            "numpad2" => VK_NUMPAD2,
            "numpad3" => VK_NUMPAD3,
            "numpad4" => VK_NUMPAD4,
            "numpad5" => VK_NUMPAD5,
            "numpad6" => VK_NUMPAD6,
            "numpad7" => VK_NUMPAD7,
            "numpad8" => VK_NUMPAD8,
            "numpad9" => VK_NUMPAD9,
            _ => return None,
        }
        .0,
    )
}

/// Reverse of [`key_code`], for round-tripping a binding back to text.
fn key_name(vk: u16) -> String {
    if vk < 128 {
        let c = vk as u8;
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            return (c as char).to_string();
        }
    }
    if (VK_F1.0..=VK_F24.0).contains(&vk) {
        return format!("F{}", vk - VK_F1.0 + 1);
    }
    match VIRTUAL_KEY(vk) {
        VK_SPACE => "Space",
        VK_RETURN => "Enter",
        VK_TAB => "Tab",
        VK_ESCAPE => "Esc",
        VK_BACK => "Backspace",
        VK_DELETE => "Delete",
        VK_INSERT => "Insert",
        VK_HOME => "Home",
        VK_END => "End",
        VK_PRIOR => "PageUp",
        VK_NEXT => "PageDown",
        VK_LEFT => "Left",
        VK_RIGHT => "Right",
        VK_UP => "Up",
        VK_DOWN => "Down",
        VK_SNAPSHOT => "PrintScreen",
        VK_PAUSE => "Pause",
        VK_CAPITAL => "CapsLock",
        VK_NUMLOCK => "NumLock",
        VK_SCROLL => "ScrollLock",
        VK_APPS => "Apps",
        VK_OEM_MINUS => "-",
        VK_OEM_PLUS => "=",
        VK_OEM_4 => "[",
        VK_OEM_6 => "]",
        VK_OEM_1 => ";",
        VK_OEM_7 => "'",
        VK_OEM_COMMA => ",",
        VK_OEM_PERIOD => ".",
        VK_OEM_2 => "/",
        VK_OEM_5 => "\\",
        VK_OEM_3 => "`",
        VK_NUMPAD0 => "Numpad0",
        VK_NUMPAD1 => "Numpad1",
        VK_NUMPAD2 => "Numpad2",
        VK_NUMPAD3 => "Numpad3",
        VK_NUMPAD4 => "Numpad4",
        VK_NUMPAD5 => "Numpad5",
        VK_NUMPAD6 => "Numpad6",
        VK_NUMPAD7 => "Numpad7",
        VK_NUMPAD8 => "Numpad8",
        VK_NUMPAD9 => "Numpad9",
        _ => return format!("VK_{vk:#04X}"),
    }
    .to_string()
}

impl FromStr for Hotkey {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err(ParseError::Empty);
        }

        let mut mods = 0u32;
        let mut vk: Option<u16> = None;

        // Split on '+' but treat a trailing '+' as the plus key, so "Win+Alt++"
        // and "Win+Alt+=" both work.
        let tokens = split_binding(s);

        for token in tokens {
            let t = token.trim();
            if t.is_empty() {
                continue;
            }
            let lower = t.to_ascii_lowercase();

            if let Some(bit) = modifier_bit(&lower) {
                if vk.is_some() {
                    return Err(ParseError::ModifierAfterKey(t.to_string()));
                }
                mods |= bit;
                continue;
            }

            match key_code(&lower) {
                Some(code) => {
                    if vk.is_some() {
                        // A second key token: the first was really unknown.
                        return Err(ParseError::UnknownKey(t.to_string()));
                    }
                    vk = Some(code);
                }
                None => {
                    return Err(if lower.len() > 5 {
                        ParseError::UnknownModifier(t.to_string())
                    } else {
                        ParseError::UnknownKey(t.to_string())
                    })
                }
            }
        }

        let vk = vk.ok_or(ParseError::NoKey)?;
        // Shift alone does not qualify: "Shift+A" as a global hotkey captures
        // every capital A typed anywhere on the desktop.
        if mods & (MOD_CONTROL.0 | MOD_ALT.0 | MOD_WIN.0) == 0 {
            // A modifier-less global hotkey would steal that key from every
            // application on the desktop. Refuse rather than let a config typo
            // make the machine unusable.
            return Err(ParseError::NoModifier(key_name(vk)));
        }

        // MOD_NOREPEAT stops held keys from firing a storm of WM_HOTKEY.
        Ok(Hotkey {
            mods: mods | MOD_NOREPEAT.0,
            vk,
        })
    }
}

/// Split `"Win+Alt++"` into `["Win", "Alt", "+"]`.
fn split_binding(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '+' {
            if cur.is_empty() {
                // A '+' with nothing before it is the plus key itself.
                out.push("=".to_string());
            } else {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(chars[i]);
        }
        i += 1;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Fixed order so a parsed-then-formatted binding is canonical.
        if self.mods & MOD_WIN.0 != 0 {
            write!(f, "Win+")?;
        }
        if self.mods & MOD_CONTROL.0 != 0 {
            write!(f, "Ctrl+")?;
        }
        if self.mods & MOD_ALT.0 != 0 {
            write!(f, "Alt+")?;
        }
        if self.mods & MOD_SHIFT.0 != 0 {
            write!(f, "Shift+")?;
        }
        write!(f, "{}", key_name(self.vk))
    }
}

impl Serialize for Hotkey {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Hotkey {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Hotkey::from_str(&s).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Hotkey {
        Hotkey::from_str(s).unwrap_or_else(|e| panic!("{s:?} failed to parse: {e}"))
    }

    #[test]
    fn parses_common_bindings() {
        let h = parse("Alt+Space");
        assert_eq!(h.mods & MOD_ALT.0, MOD_ALT.0);
        assert_eq!(h.vk, VK_SPACE.0);

        let h = parse("Win+Alt+Shift+L");
        assert_eq!(h.mods & MOD_WIN.0, MOD_WIN.0);
        assert_eq!(h.mods & MOD_ALT.0, MOD_ALT.0);
        assert_eq!(h.mods & MOD_SHIFT.0, MOD_SHIFT.0);
        assert_eq!(h.vk, b'L' as u16);
    }

    #[test]
    fn norepeat_is_always_set() {
        // Without MOD_NOREPEAT a held hotkey floods the message queue.
        for a in Action::ALL {
            let h = parse(a.default_binding());
            assert_ne!(h.mods & MOD_NOREPEAT.0, 0, "{}", a.config_key());
        }
    }

    #[test]
    fn every_default_binding_parses() {
        for a in Action::ALL {
            let h = parse(a.default_binding());
            assert_ne!(h.vk, 0, "{}", a.config_key());
        }
    }

    #[test]
    fn default_bindings_are_unique() {
        // Two actions sharing a binding means one silently never fires.
        let mut seen = std::collections::HashMap::new();
        for a in Action::ALL {
            let h = parse(a.default_binding());
            if let Some(prev) = seen.insert((h.mods, h.vk), a) {
                panic!("{:?} and {:?} share {}", prev, a, a.default_binding());
            }
        }
    }

    #[test]
    fn action_ids_are_unique_and_nonzero() {
        // RegisterHotKey ids must be distinct; 0 is legal but we avoid it so a
        // zeroed WM_HOTKEY wParam is obviously wrong.
        let mut ids: Vec<i32> = Action::ALL.iter().map(|a| *a as i32).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count);
        assert!(ids.iter().all(|i| *i > 0));
    }

    #[test]
    fn action_round_trips_through_its_id() {
        for a in Action::ALL {
            assert_eq!(Action::from_hotkey_id(a as i32), Some(a));
        }
        assert_eq!(Action::from_hotkey_id(9999), None);
    }

    #[test]
    fn config_keys_are_unique() {
        let mut keys: Vec<&str> = Action::ALL.iter().map(|a| a.config_key()).collect();
        let count = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), count);
    }

    #[test]
    fn parsing_is_case_and_space_insensitive() {
        assert_eq!(parse("win+alt+l"), parse("WIN + ALT + L"));
        assert_eq!(parse("  Alt+Space  "), parse("alt+space"));
    }

    #[test]
    fn modifier_aliases_are_accepted() {
        assert_eq!(parse("Super+A"), parse("Win+A"));
        assert_eq!(parse("Meta+A"), parse("Win+A"));
        assert_eq!(parse("Control+A"), parse("Ctrl+A"));
    }

    #[test]
    fn display_round_trips() {
        for s in [
            "Win+Alt+L",
            "Ctrl+Shift+F5",
            "Alt+Space",
            "Win+Ctrl+Alt+Shift+Delete",
            "Win+Alt+Left",
            "Win+Alt+-",
            "Win+Alt+=",
            "Ctrl+Alt+Numpad7",
        ] {
            let h = parse(s);
            let printed = h.to_string();
            assert_eq!(parse(&printed), h, "{s} printed as {printed}");
        }
    }

    #[test]
    fn display_uses_canonical_modifier_order() {
        assert_eq!(
            parse("Shift+Alt+Ctrl+Win+K").to_string(),
            "Win+Ctrl+Alt+Shift+K"
        );
    }

    #[test]
    fn plus_key_is_parseable_both_ways() {
        assert_eq!(parse("Win+Alt++").vk, VK_OEM_PLUS.0);
        assert_eq!(parse("Win+Alt+=").vk, VK_OEM_PLUS.0);
        assert_eq!(parse("Win+Alt+plus").vk, VK_OEM_PLUS.0);
    }

    #[test]
    fn function_keys_parse_across_the_range() {
        assert_eq!(parse("Ctrl+F1").vk, VK_F1.0);
        assert_eq!(parse("Ctrl+F12").vk, VK_F12.0);
        assert_eq!(parse("Ctrl+F24").vk, VK_F24.0);
        assert!(Hotkey::from_str("Ctrl+F25").is_err());
        assert!(Hotkey::from_str("Ctrl+F0").is_err());
    }

    #[test]
    fn rejects_binding_without_modifier() {
        // Would hijack the key desktop-wide.
        assert!(matches!(
            Hotkey::from_str("Space"),
            Err(ParseError::NoModifier(_))
        ));
        assert!(matches!(
            Hotkey::from_str("A"),
            Err(ParseError::NoModifier(_))
        ));
    }

    #[test]
    fn rejects_modifier_only_binding() {
        assert_eq!(Hotkey::from_str("Ctrl+Alt"), Err(ParseError::NoKey));
        assert_eq!(Hotkey::from_str("Win"), Err(ParseError::NoKey));
    }

    #[test]
    fn rejects_empty_and_garbage() {
        assert_eq!(Hotkey::from_str(""), Err(ParseError::Empty));
        assert_eq!(Hotkey::from_str("   "), Err(ParseError::Empty));
        assert!(Hotkey::from_str("Ctrl+Nonsense").is_err());
        assert!(Hotkey::from_str("Ctrl+A+B").is_err());
    }

    #[test]
    fn rejects_modifier_after_key() {
        assert!(matches!(
            Hotkey::from_str("Ctrl+A+Shift"),
            Err(ParseError::ModifierAfterKey(_))
        ));
    }

    #[test]
    fn errors_are_human_readable() {
        let e = Hotkey::from_str("Ctrl+Nonsense").unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("Nonsense"), "unhelpful message: {msg}");
    }

    #[test]
    fn serde_round_trips_through_json() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrap {
            key: Hotkey,
        }
        let w = Wrap {
            key: parse("Win+Alt+Shift+Left"),
        };
        let text = serde_json::to_string(&w).unwrap();
        assert!(text.contains("Win+Alt+Shift+Left"), "{text}");
        assert_eq!(serde_json::from_str::<Wrap>(&text).unwrap(), w);
    }

    #[test]
    fn serde_reports_a_useful_error_for_a_bad_binding() {
        #[derive(Deserialize, Debug)]
        #[allow(dead_code)]
        struct Wrap {
            key: Hotkey,
        }
        let err = serde_json::from_str::<Wrap>(r#"{"key":"Ctrl+Bogus"}"#).unwrap_err();
        assert!(err.to_string().contains("Bogus"), "{err}");
    }

    #[test]
    fn unknown_vk_formats_without_panicking() {
        let h = Hotkey {
            mods: MOD_ALT.0,
            vk: 0x00FE,
        };
        assert!(h.to_string().contains("VK_"));
    }
}

//! User configuration: `%LOCALAPPDATA%\SuperTile\config.json`.
//!
//! ## Why JSON
//!
//! A keybinding carries more than a key string: each records the i3 binding it
//! derives from, a note explaining any deviation, and a fallback chain used
//! when another application already owns the combination. That is structured
//! data, and JSON expresses it directly.
//!
//! JSON has no comment syntax, so the explanations live in fields — `_readme`
//! at the top of the file and of the keybindings section, and `i3` / `note` on
//! every binding. They are regenerated on every save, so they cannot drift out
//! of date with the code.
//!
//! ## Resilience contract
//!
//! The file is hand-editable, so it is treated as untrusted input:
//!
//! 1. **A malformed file never prevents startup.** Parse failures fall back to
//!    defaults and surface a warning; the broken file is preserved, never
//!    silently overwritten.
//! 2. **One bad binding does not discard the rest.** Bindings are stored as
//!    strings and parsed individually.
//!
//! All numeric fields are clamped on load — see [`Config::sanitize`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::hotkeys::{Action, Hotkey, DEFAULTS, MOD_EXPLANATION};
use crate::layout::{LayoutKind, LayoutParams};

pub const CONFIG_FILENAME: &str = "config.json";

/// The current configuration schema.
///
/// Raise this only when an existing key changes name or meaning, never merely
/// because a key was added: serde fills a missing key with its default, so
/// additions are backward compatible on their own. A number that goes up every
/// release tells you nothing.
///
/// 1 — the first numbered schema. Everything before it is version 0.
pub const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    /// The shape of this file, independent of who wrote it.
    ///
    /// Separate from [`Config::version`] on purpose: they answer different
    /// questions. The application version says *which build* wrote the file,
    /// which is what you want when reading a bug report. This says *which
    /// layout of keys* it uses, which is what a migration has to key off — most
    /// releases change no settings at all, so the two numbers move at quite
    /// different rates and conflating them would mean pretending every release
    /// needs a migration.
    ///
    /// Zero means the file predates the field. Raising [`CONFIG_VERSION`] is a
    /// deliberate act, taken only when a key is renamed, removed, or given a
    /// meaning its old values no longer fit.
    #[serde(rename = "_config_version", default)]
    pub config_version: u32,
    /// The SuperTile version that last wrote this file.
    ///
    /// Not the schema version, deliberately. A schema number has to be
    /// remembered and incremented by hand, and the one time it matters is the
    /// time somebody forgot. The application version is already maintained,
    /// already meaningful to whoever is reading the file, and answers the
    /// question actually being asked: was this written by something older than
    /// what is running now?
    ///
    /// Unknown keys are ignored on load and missing ones take their defaults,
    /// so an older file simply works. This exists to make a mismatch *visible*
    /// -- in the log, in an issue report -- rather than to gate anything.
    #[serde(rename = "_version", default)]
    pub version: String,
    /// Regenerated on every save; edits to it are discarded.
    #[serde(rename = "_readme", default = "readme")]
    pub readme: Vec<String>,
    #[serde(default)]
    pub general: General,
    #[serde(default)]
    pub layout: LayoutConfig,
    #[serde(default)]
    pub appearance: Appearance,
    #[serde(default)]
    pub memory: MemoryConfig,
    #[serde(default)]
    pub diagnostics: Diagnostics,
    #[serde(default)]
    pub updates: Updates,
    #[serde(default)]
    pub dimming: crate::dimmer::DimConfig,
    #[serde(default)]
    pub palette: PaletteSources,
    #[serde(default)]
    pub keybindings: Keybindings,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

fn readme() -> Vec<String> {
    vec![
        "SuperTile configuration. Edit by hand or use the tray menu.".into(),
        "Changes are picked up with 'Reload configuration'; no restart needed.".into(),
        "If this file cannot be parsed, SuperTile falls back to defaults and leaves it \
         untouched, so nothing is lost."
            .into(),
        "Keybindings follow i3. Each entry records the i3 binding it came from, and a \
         note wherever SuperTile had to differ."
            .into(),
        MOD_EXPLANATION.into(),
        "Set \"keys\" to \"\" to unbind an action. At least one modifier is required, \
         otherwise the key would be captured system-wide."
            .into(),
        "\"fallbacks\" are tried in order when another application already owns \
         \"keys\". Windows offers no way to ask who owns a combination, so claiming \
         it is the only test."
            .into(),
        "https://github.com/andreaswiren/supertile#configuration".into(),
    ]
}

impl Default for Config {
    fn default() -> Self {
        Config {
            config_version: CONFIG_VERSION,
            // Filled in on save; empty means "written before this field
            // existed", which is exactly what a fresh default has in common
            // with an ancient file.
            version: String::new(),
            readme: readme(),
            general: General::default(),
            layout: LayoutConfig::default(),
            appearance: Appearance::default(),
            memory: MemoryConfig::default(),
            diagnostics: Diagnostics::default(),
            updates: Updates::default(),
            dimming: crate::dimmer::DimConfig::default(),
            palette: PaletteSources::default(),
            keybindings: Keybindings::default(),
            rules: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Keybindings
// ---------------------------------------------------------------------------

/// One configurable binding, with the provenance that explains it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Binding {
    /// Stable action name; see [`Action::config_key`].
    pub action: String,
    /// The combination to try first. Empty means "unbound".
    pub keys: String,
    /// The i3 binding this derives from. Informational, regenerated on save.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub i3: String,
    /// Why SuperTile differs from i3 here. Informational.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// Tried in order when `keys` is already owned by another application.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallbacks: Vec<String>,
}

impl Binding {
    fn from_spec(spec: &crate::hotkeys::BindingSpec) -> Binding {
        Binding {
            action: spec.action.config_key().to_string(),
            keys: spec.keys.to_string(),
            i3: spec.i3.to_string(),
            note: spec.note.to_string(),
            fallbacks: spec.fallbacks.iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Keybindings {
    /// Regenerated on save; explains the `$mod` mapping.
    #[serde(rename = "_readme", default = "keys_readme")]
    pub readme: String,
    #[serde(default)]
    pub bindings: Vec<Binding>,
}

fn keys_readme() -> String {
    MOD_EXPLANATION.to_string()
}

impl Default for Keybindings {
    fn default() -> Self {
        Keybindings {
            readme: keys_readme(),
            bindings: DEFAULTS.iter().map(Binding::from_spec).collect(),
        }
    }
}

impl Keybindings {
    pub fn get(&self, action: Action) -> Option<&Binding> {
        self.bindings
            .iter()
            .find(|b| b.action == action.config_key())
    }

    /// Add any binding the file is missing, so a config written by an older
    /// version gains new actions rather than silently lacking them.
    fn fill_missing(&mut self) -> usize {
        let mut added = 0;
        for spec in DEFAULTS {
            if self.get(spec.action).is_none() {
                self.bindings.push(Binding::from_spec(spec));
                added += 1;
            }
        }
        // Keep file order matching DEFAULTS so diffs stay readable.
        let order: HashMap<&str, usize> = DEFAULTS
            .iter()
            .enumerate()
            .map(|(i, s)| (s.action.config_key(), i))
            .collect();
        self.bindings
            .sort_by_key(|b| order.get(b.action.as_str()).copied().unwrap_or(usize::MAX));
        added
    }

    /// Refresh the informational fields from the built-in table.
    ///
    /// `i3` and `note` document SuperTile's design, not the user's choices, so
    /// they track the code rather than the file. `keys` and `fallbacks` are
    /// left exactly as the user wrote them.
    fn refresh_documentation(&mut self) {
        self.readme = keys_readme();
        for b in &mut self.bindings {
            if let Some(spec) = DEFAULTS.iter().find(|s| s.action.config_key() == b.action) {
                b.i3 = spec.i3.to_string();
                b.note = spec.note.to_string();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct General {
    pub start_with_windows: bool,
    /// Tiling suspended. Persisted so a paused SuperTile stays paused.
    pub paused: bool,
    /// Re-tile automatically when windows appear, close or un-minimise.
    pub auto_tile: bool,
    /// Milliseconds to coalesce window events before retiling.
    pub retile_debounce_ms: u32,
    /// Keep a cell for windows that cannot be moved because they are elevated.
    ///
    /// Off by default. Windows forbids a normal program from repositioning an
    /// elevated one, so a reserved cell is one that can never be filled: the
    /// other windows tile around a gap while the elevated window floats over
    /// them regardless. On, for anyone who would rather the layout hold still
    /// while an admin console comes and goes.
    #[serde(default)]
    pub reserve_cells_for_elevated: bool,
}

impl Default for General {
    fn default() -> Self {
        General {
            start_with_windows: false,
            paused: false,
            auto_tile: true,
            retile_debounce_ms: 90,
            reserve_cells_for_elevated: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LayoutConfig {
    pub kind: LayoutKind,
    pub outer_gap: i32,
    pub inner_gap: i32,
    pub master_fraction: f32,
    pub master_count: u32,
    pub tile_minimised: bool,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        let p = LayoutParams::default();
        LayoutConfig {
            // Split is the default. The parametric grids reflow every window
            // when one arrives or leaves, which is disorienting; a tree only
            // divides the cell you dropped onto and leaves the rest alone.
            kind: LayoutKind::Bsp,
            outer_gap: p.outer_gap,
            inner_gap: p.inner_gap,
            master_fraction: p.master_fraction,
            master_count: p.master_count,
            tile_minimised: false,
        }
    }
}

impl LayoutConfig {
    pub fn params(&self) -> LayoutParams {
        LayoutParams {
            outer_gap: self.outer_gap,
            inner_gap: self.inner_gap,
            master_fraction: self.master_fraction,
            master_count: self.master_count,
        }
        .sanitized()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Auto,
    Light,
    Dark,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Appearance {
    pub theme: Theme,
    pub palette_max_rows: u32,
    pub palette_width_dip: u32,
    pub font_size_pt: u32,
    /// Show a live pixel size while a window is being resized or retiled.
    ///
    /// Draws "1352 x 651" over the middle of each window that moves. Useful
    /// when you are matching a capture size or a layout to a spec, noise the
    /// rest of the time -- hence a setting.
    pub show_size_readout: bool,
    /// Outline the whole grid while a boundary is being dragged.
    ///
    /// Dragging moves a shared edge rather than one window's edge, and seeing
    /// every cell respond makes that obvious. Off for anyone who finds the
    /// outlines busy.
    pub show_grid_on_resize: bool,
    /// Which overlay theme to draw drags with.
    ///
    /// One of `windows`, `graphite`, `ember`, `forest`, `violet`, `contrast`.
    /// Sets the grid colour, how transparent the overlays are, the corner
    /// radius and the size-readout font. An unrecognised name falls back to
    /// `windows` rather than refusing to draw.
    pub overlay_theme: String,
    /// A theme of your own, selected by setting `overlay_theme` to `custom`.
    ///
    /// Edited from the tray. Colours are `#RRGGBB`; an unparseable one falls
    /// back to the built-in default rather than refusing to draw, because a
    /// typo here should cost a colour, not the overlays.
    pub custom_theme: CustomTheme,
    /// Override the theme's line thickness, in device-independent pixels.
    ///
    /// Zero leaves the theme's own value alone. A separate knob because
    /// thickness is the part people want to tune without giving up the colours
    /// they chose, and one pixel is enough to read a boundary by on a dense
    /// display.
    pub overlay_line_dip: u32,
}

/// A user-defined overlay theme.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CustomTheme {
    /// Shown in the tray, so a theme can be recognised rather than remembered.
    pub name: String,
    pub accent: String,
    pub warning: String,
    pub text: String,
    pub outline_alpha: u8,
    pub fill_alpha: u8,
    pub border_dip: i32,
    pub corner_dip: i32,
    /// Face name for the size readout; empty means the UI font.
    pub font: String,
    pub font_pt: i32,
}

impl Default for CustomTheme {
    fn default() -> Self {
        // Starts as a copy of the default theme, so editing one colour gives a
        // working theme rather than a half-defined one.
        CustomTheme {
            name: "My theme".to_string(),
            accent: "#60A5FA".to_string(),
            warning: "#E09E2B".to_string(),
            text: "#FFFFFF".to_string(),
            outline_alpha: 210,
            fill_alpha: 90,
            border_dip: 3,
            corner_dip: 8,
            font: String::new(),
            font_pt: 12,
        }
    }
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance {
            theme: Theme::Auto,
            palette_max_rows: 9,
            palette_width_dip: 680,
            font_size_pt: 11,
            show_size_readout: true,
            show_grid_on_resize: true,
            overlay_theme: "windows".to_string(),
            custom_theme: CustomTheme::default(),
            overlay_line_dip: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MemoryConfig {
    pub enabled: bool,
    pub max_entries: u32,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        MemoryConfig {
            enabled: true,
            max_entries: 500,
        }
    }
}

/// Extra sources the command palette can search.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PaletteSources {
    /// Offer "Ask Claude" in the palette, reachable with Tab.
    ///
    /// Off by default, which `Default` gives for a bool at no cost. Asking a
    /// question opens claude.ai in the browser with the text in the URL, so it
    /// leaves the machine: that is a decision to be made deliberately rather
    /// than discovered after the fact.
    pub claude_desktop: bool,
}

/// Checking GitHub for a newer release.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Updates {
    /// Check automatically in the background.
    ///
    /// Off by default, and it should stay a decision rather than a default. A
    /// check contacts a third party, which reveals an IP address and the fact
    /// that this software is installed; nobody should discover after the fact
    /// that their window manager has been phoning home.
    pub check_automatically: bool,
    /// Hours between automatic checks.
    ///
    /// A day. A window manager is not a browser: there is no security-critical
    /// update cadence to keep up with, and anything more frequent is nagging
    /// dressed as diligence.
    pub interval_hours: u64,
    /// Unix time of the last successful check, so a restart does not re-ask.
    pub last_checked: u64,
    /// The newest version already reported, so the same one is not announced
    /// twice. Cleared when a newer one appears.
    pub last_reported: String,
}

impl Default for Updates {
    fn default() -> Self {
        Updates {
            check_automatically: false,
            interval_hours: 24,
            last_checked: 0,
            last_reported: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Diagnostics {
    /// Opt-in file logging. Records window titles and executable paths.
    pub logging: bool,
    /// Record the evidence as well as the decisions.
    ///
    /// Every retile, every placement, and every window entering or leaving the
    /// managed set. Runs to megabytes in an afternoon, so it is meant to be
    /// switched on while a fault is chased and off again afterwards. Has no
    /// effect unless `logging` is also on.
    pub verbose: bool,
}

/// What to do with a window that matches a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RuleAction {
    #[default]
    Float,
    Ignore,
    Tile,
}

/// A window-matching rule. All present fields must match (logical AND); an
/// all-empty rule matches nothing, so a typo cannot float every window.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Rule {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_contains: Option<String>,
    #[serde(default)]
    pub action: RuleAction,
}

impl Rule {
    /// Does this rule apply to the given window?
    pub fn matches(&self, exe_path: &str, class: &str, title: &str) -> bool {
        if self.exe.is_none() && self.class.is_none() && self.title_contains.is_none() {
            return false;
        }
        if let Some(want) = &self.exe {
            let file = exe_path.rsplit(['\\', '/']).next().unwrap_or(exe_path);
            if !file.eq_ignore_ascii_case(want) && !exe_path.eq_ignore_ascii_case(want) {
                return false;
            }
        }
        if let Some(want) = &self.class {
            if !class.eq_ignore_ascii_case(want) {
                return false;
            }
        }
        if let Some(want) = &self.title_contains {
            if !title.to_lowercase().contains(&want.to_lowercase()) {
                return false;
            }
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Loading and saving
// ---------------------------------------------------------------------------

/// What to say about a config file whose schema is not the current one.
///
/// Returns `None` when there is nothing worth saying. Both directions are
/// survivable — unknown keys are ignored on load and missing ones take their
/// defaults — so this warns rather than refuses. The downgrade case is the one
/// that can actually lose something: settings this build does not know about
/// are dropped the next time the file is written, which is precisely why ten
/// backups are kept.
pub fn schema_warning(found: u32, current: u32) -> Option<String> {
    use std::cmp::Ordering;
    match found.cmp(&current) {
        Ordering::Equal => None,
        Ordering::Less => Some(format!(
            "configuration uses schema {found}; this build expects {current}. \
             Missing settings take their defaults."
        )),
        Ordering::Greater => Some(format!(
            "configuration uses schema {found}, which is newer than this build's {current}. \
             Settings it does not recognise will be dropped when the file is next saved; \
             the previous version is kept as config.json.1."
        )),
    }
}

/// Outcome of loading a config: the config plus anything the user should know.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    pub warnings: Vec<String>,
    /// True when the on-disk file could not be parsed and defaults were used.
    pub used_defaults: bool,
}

/// The ordered set of combinations to try for one action.
///
/// The head is the user's choice; the rest are fallbacks, tried only when the
/// preceding entry turns out to be owned by another application.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidates {
    pub action: Action,
    pub chain: Vec<Hotkey>,
}

impl Config {
    pub fn path() -> std::io::Result<PathBuf> {
        Ok(crate::util::data_dir()?.join(CONFIG_FILENAME))
    }

    pub fn load() -> Loaded {
        let Ok(path) = Config::path() else {
            return Loaded {
                config: Config::default(),
                warnings: vec!["Could not resolve %LOCALAPPDATA%; using defaults.".into()],
                used_defaults: true,
            };
        };
        Config::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Loaded {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Config::default();
                let mut warnings = Vec::new();
                if let Err(e) = cfg.save_to(path) {
                    warnings.push(format!("Could not write default config: {e}"));
                }
                return Loaded {
                    config: cfg,
                    warnings,
                    used_defaults: false,
                };
            }
            Err(e) => {
                return Loaded {
                    config: Config::default(),
                    warnings: vec![format!("Could not read {}: {e}", path.display())],
                    used_defaults: true,
                };
            }
        };

        match serde_json::from_str::<Config>(&text) {
            Ok(mut cfg) => {
                let mut warnings = cfg.sanitize();
                let added = cfg.keybindings.fill_missing();
                if added > 0 {
                    warnings.push(format!(
                        "{added} new action(s) were added to your keybindings with their \
                         default keys."
                    ));
                }
                cfg.keybindings.refresh_documentation();
                cfg.readme = readme();
                warnings.extend(cfg.resolve_hotkeys().1);
                Loaded {
                    config: cfg,
                    warnings,
                    used_defaults: false,
                }
            }
            Err(e) => Loaded {
                config: Config::default(),
                // Deliberately not overwritten: the user's file is preserved
                // exactly as written so the mistake can be corrected.
                warnings: vec![format!(
                    "{} could not be parsed, so defaults are in use. Your file has been \
                     left untouched. Error: {e}",
                    path.display()
                )],
                used_defaults: true,
            },
        }
    }

    /// Clamp out-of-range values, returning a warning for each adjustment.
    pub fn sanitize(&mut self) -> Vec<String> {
        let mut w = Vec::new();
        let mut clamp_i32 = |name: &str, v: &mut i32, lo: i32, hi: i32| {
            let c = (*v).clamp(lo, hi);
            if c != *v {
                w.push(format!("{name} was {v}; clamped to {c}."));
                *v = c;
            }
        };
        clamp_i32("layout.outer_gap", &mut self.layout.outer_gap, 0, 400);
        clamp_i32("layout.inner_gap", &mut self.layout.inner_gap, 0, 400);

        if !self.layout.master_fraction.is_finite() {
            w.push("layout.master_fraction was not a number; reset to 0.55.".into());
            self.layout.master_fraction = 0.55;
        } else {
            let c = self.layout.master_fraction.clamp(0.15, 0.85);
            if (c - self.layout.master_fraction).abs() > f32::EPSILON {
                w.push(format!(
                    "layout.master_fraction was {}; clamped to {c}.",
                    self.layout.master_fraction
                ));
                self.layout.master_fraction = c;
            }
        }

        let mut clamp_u32 = |name: &str, v: &mut u32, lo: u32, hi: u32| {
            let c = (*v).clamp(lo, hi);
            if c != *v {
                w.push(format!("{name} was {v}; clamped to {c}."));
                *v = c;
            }
        };
        clamp_u32("layout.master_count", &mut self.layout.master_count, 1, 16);
        clamp_u32(
            "memory.max_entries",
            &mut self.memory.max_entries,
            0,
            100_000,
        );
        clamp_u32(
            "appearance.palette_max_rows",
            &mut self.appearance.palette_max_rows,
            3,
            30,
        );
        clamp_u32(
            "appearance.palette_width_dip",
            &mut self.appearance.palette_width_dip,
            320,
            2000,
        );
        clamp_u32(
            "appearance.font_size_pt",
            &mut self.appearance.font_size_pt,
            7,
            32,
        );
        clamp_u32(
            "general.retile_debounce_ms",
            &mut self.general.retile_debounce_ms,
            0,
            5_000,
        );

        let before = self.rules.len();
        self.rules
            .retain(|r| r.exe.is_some() || r.class.is_some() || r.title_contains.is_some());
        if self.rules.len() != before {
            w.push(format!(
                "{} rule(s) matched nothing (no exe, class or title_contains) and were \
                 ignored.",
                before - self.rules.len()
            ));
        }

        w.extend(self.dimming.sanitize());

        let unknown: Vec<String> = self
            .keybindings
            .bindings
            .iter()
            .filter(|b| !DEFAULTS.iter().any(|s| s.action.config_key() == b.action))
            .map(|b| b.action.clone())
            .collect();
        for name in unknown {
            w.push(format!(
                "keybindings: '{name}' is not an action SuperTile knows; ignored."
            ));
        }
        w
    }

    /// Build the ordered candidate chain for every action.
    ///
    /// Each chain is the configured key followed by its fallbacks. The caller
    /// walks the chain until `RegisterHotKey` accepts one — Windows provides no
    /// way to ask which application owns a combination, so claiming it is the
    /// only test available.
    pub fn resolve_hotkeys(&self) -> (Vec<Candidates>, Vec<String>) {
        let mut out = Vec::with_capacity(Action::ALL.len());
        let mut warnings = Vec::new();

        for action in Action::ALL {
            let binding = self.keybindings.get(action);
            let (primary, fallbacks): (String, Vec<String>) = match binding {
                Some(b) => (b.keys.clone(), b.fallbacks.clone()),
                None => (
                    action.default_binding().to_string(),
                    action.fallbacks().iter().map(|s| s.to_string()).collect(),
                ),
            };

            // An explicit empty string means "unbound", which is legitimate.
            if primary.trim().is_empty() {
                continue;
            }

            let mut chain: Vec<Hotkey> = Vec::new();
            let push =
                |text: &str, chain: &mut Vec<Hotkey>, warnings: &mut Vec<String>| match text
                    .parse::<Hotkey>()
                {
                    Ok(h) => {
                        if !chain.contains(&h) {
                            chain.push(h);
                        }
                    }
                    Err(e) => warnings.push(format!(
                        "keybindings.{}: \"{text}\" is invalid ({e}); ignored.",
                        action.config_key()
                    )),
                };
            push(&primary, &mut chain, &mut warnings);
            for f in &fallbacks {
                push(f, &mut chain, &mut warnings);
            }

            if chain.is_empty() {
                // Everything the user wrote was unparseable: fall back to the
                // built-in default so the action is not silently lost.
                if let Ok(h) = action.default_binding().parse::<Hotkey>() {
                    warnings.push(format!(
                        "keybindings.{}: falling back to the default {}.",
                        action.config_key(),
                        action.default_binding()
                    ));
                    chain.push(h);
                }
            }
            if !chain.is_empty() {
                out.push(Candidates { action, chain });
            }
        }
        (out, warnings)
    }

    /// Which rule, if any, applies to a window. First match wins.
    pub fn rule_for(&self, exe: &str, class: &str, title: &str) -> Option<RuleAction> {
        self.rules
            .iter()
            .find(|r| r.matches(exe, class, title))
            .map(|r| r.action)
    }

    /// Record the key actually in use for an action, so the file reflects
    /// reality after a fallback was taken.
    pub fn set_binding(&mut self, action: Action, keys: &str) {
        if let Some(b) = self
            .keybindings
            .bindings
            .iter_mut()
            .find(|b| b.action == action.config_key())
        {
            b.keys = keys.to_string();
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&Config::path()?)
    }

    /// Write atomically: a crash mid-write must not truncate the user's config.
    /// How many previous versions of the config are kept.
    ///
    /// The config is edited by hand as well as by the tray, and a bad edit or a
    /// botched migration is otherwise unrecoverable. Ten is enough to walk back
    /// a session's worth of experiments and small enough that nobody notices
    /// the disk cost.
    pub const BACKUP_COUNT: usize = 10;

    /// Rotate `config.json` into `config.json.1`, that into `.2`, and so on.
    ///
    /// Called before each save. Failure is deliberately ignored: losing a
    /// backup is a nuisance, but refusing to save the user's settings because
    /// the backup could not be written would turn a nuisance into a fault.
    fn rotate_backups(path: &Path) {
        if !path.exists() {
            return;
        }
        let at = |n: usize| path.with_extension(format!("json.{n}"));
        // Oldest first, so nothing is overwritten before it has been moved.
        let _ = std::fs::remove_file(at(Self::BACKUP_COUNT));
        for n in (1..Self::BACKUP_COUNT).rev() {
            let _ = std::fs::rename(at(n), at(n + 1));
        }
        let _ = std::fs::copy(path, at(1));
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        let mut out = self.clone();
        out.readme = readme();
        out.version = crate::APP_VERSION.to_string();
        out.config_version = CONFIG_VERSION;
        out.keybindings.refresh_documentation();
        Self::rotate_backups(path);

        let text = serde_json::to_string_pretty(&out)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, format!("{text}\n").as_bytes())?;
        // On Windows std::fs::rename maps to MoveFileEx with
        // MOVEFILE_REPLACE_EXISTING, so this replaces the target atomically.
        match std::fs::rename(&tmp, path) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Config {
        let mut c: Config = serde_json::from_str(json).expect("should parse");
        c.keybindings.fill_missing();
        c
    }

    #[test]
    fn defaults_round_trip_through_json() {
        let cfg = Config::default();
        let text = serde_json::to_string_pretty(&cfg).unwrap();
        assert_eq!(serde_json::from_str::<Config>(&text).unwrap(), cfg);
    }

    #[test]
    fn an_empty_object_yields_defaults() {
        let cfg = parse("{}");
        assert_eq!(cfg.layout.kind, LayoutKind::Bsp);
        assert!(cfg.general.auto_tile);
        assert!(cfg.rules.is_empty());
        assert_eq!(cfg.keybindings.bindings.len(), Action::ALL.len());
    }

    #[test]
    fn a_partial_object_keeps_other_defaults() {
        let cfg = parse(r#"{"layout":{"outer_gap":20}}"#);
        assert_eq!(cfg.layout.outer_gap, 20);
        assert_eq!(cfg.layout.inner_gap, 8);
        assert_eq!(cfg.appearance.palette_max_rows, 9);
    }

    #[test]
    fn all_layout_kinds_parse_from_kebab_case() {
        for (text, want) in [
            ("grid", LayoutKind::Grid),
            ("master-stack", LayoutKind::MasterStack),
            ("columns", LayoutKind::Columns),
            ("rows", LayoutKind::Rows),
            ("dwindle", LayoutKind::Dwindle),
            ("monocle", LayoutKind::Monocle),
        ] {
            let cfg = parse(&format!(r#"{{"layout":{{"kind":"{text}"}}}}"#));
            assert_eq!(cfg.layout.kind, want);
        }
    }

    // --- keybindings --------------------------------------------------------

    #[test]
    fn defaults_include_every_action_with_its_i3_provenance() {
        let kb = Keybindings::default();
        assert_eq!(kb.bindings.len(), Action::ALL.len());
        for action in Action::ALL {
            let b = kb
                .get(action)
                .unwrap_or_else(|| panic!("{} missing", action.config_key()));
            assert!(!b.keys.is_empty(), "{} has no keys", b.action);
            // Every action either mirrors an i3 binding or explains why not.
            assert!(
                !b.i3.is_empty() || !b.note.is_empty(),
                "{} has neither an i3 binding nor a note explaining its absence",
                b.action
            );
        }
    }

    #[test]
    fn every_deviation_from_i3_is_explained() {
        // The brief asked for a note wherever SuperTile differs from i3.
        for spec in DEFAULTS {
            if spec.i3.is_empty() {
                assert!(
                    !spec.note.is_empty(),
                    "{} has no i3 equivalent and no explanation",
                    spec.action.config_key()
                );
                continue;
            }
            // "$mod+h" -> the token after the last '+' should end our binding
            // too, if the translation is faithful.
            let i3_key = spec.i3.rsplit('+').next().unwrap_or("").to_lowercase();
            let ours = spec.keys.to_lowercase();
            let faithful = ours.ends_with(&format!("+{i3_key}"));
            assert!(
                faithful || !spec.note.is_empty(),
                "{}: i3 uses {} but we use {} with no note explaining it",
                spec.action.config_key(),
                spec.i3,
                spec.keys
            );
        }
    }

    #[test]
    fn the_readme_explains_the_mod_mapping() {
        let cfg = Config::default();
        assert!(cfg.keybindings.readme.contains("$mod"));
        assert!(cfg.keybindings.readme.contains("Win+Alt"));
        assert!(cfg.readme.iter().any(|l| l.contains("i3")));
    }

    #[test]
    fn every_action_resolves_to_a_candidate_chain() {
        let cfg = Config::default();
        let (chains, warnings) = cfg.resolve_hotkeys();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(chains.len(), Action::ALL.len());
        for c in &chains {
            assert!(
                !c.chain.is_empty(),
                "{} has an empty chain",
                c.action.config_key()
            );
            assert_eq!(c.chain[0].to_string(), c.action.default_binding());
        }
    }

    #[test]
    fn fallbacks_follow_the_primary_in_order() {
        let cfg = Config::default();
        let (chains, _) = cfg.resolve_hotkeys();
        let focus_left = chains
            .iter()
            .find(|c| c.action == Action::FocusLeft)
            .expect("focus_left");
        assert!(
            focus_left.chain.len() > 1,
            "focus_left should have fallbacks"
        );
        assert_eq!(focus_left.chain[0].to_string(), "Win+Alt+H");
        assert_eq!(focus_left.chain[1].to_string(), "Ctrl+Alt+H");
    }

    #[test]
    fn default_bindings_are_written_in_canonical_form() {
        // Otherwise the first save rewrites every key ("Return" -> "Enter"),
        // producing a confusing diff the user did not ask for.
        for spec in DEFAULTS {
            let parsed: Hotkey = spec.keys.parse().expect(spec.keys);
            assert_eq!(
                parsed.to_string(),
                spec.keys,
                "{}: default is not canonical",
                spec.action.config_key()
            );
            for f in spec.fallbacks {
                let p: Hotkey = f.parse().expect(f);
                assert_eq!(
                    p.to_string(),
                    *f,
                    "{}: fallback not canonical",
                    spec.action.config_key()
                );
            }
        }
    }

    #[test]
    fn every_action_has_at_least_one_fallback() {
        // Conflict avoidance is only as good as the alternatives available.
        for spec in DEFAULTS {
            assert!(
                !spec.fallbacks.is_empty(),
                "{} has no fallback, so a conflict leaves it dead",
                spec.action.config_key()
            );
        }
    }

    #[test]
    fn duplicate_entries_in_a_chain_are_collapsed() {
        let json = r#"{"keybindings":{"bindings":[
            {"action":"retile","keys":"Ctrl+Alt+T","fallbacks":["Ctrl+Alt+T","Ctrl+Alt+Y"]}]}}"#;
        let cfg = parse(json);
        let (chains, _) = cfg.resolve_hotkeys();
        let c = chains.iter().find(|c| c.action == Action::Retile).unwrap();
        assert_eq!(
            c.chain.len(),
            2,
            "identical fallback should be dropped: {:?}",
            c.chain
        );
    }

    #[test]
    fn an_empty_key_unbinds_the_action() {
        let json = r#"{"keybindings":{"bindings":[{"action":"palette","keys":""}]}}"#;
        let cfg = parse(json);
        let (chains, warnings) = cfg.resolve_hotkeys();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(!chains.iter().any(|c| c.action == Action::Palette));
        assert_eq!(chains.len(), Action::ALL.len() - 1);
    }

    #[test]
    fn one_bad_binding_does_not_discard_the_others() {
        let json = r#"{"keybindings":{"bindings":[
            {"action":"palette","keys":"Ctrl+Bogus"},
            {"action":"retile","keys":"Ctrl+Alt+R"}]}}"#;
        let cfg = parse(json);
        let (chains, warnings) = cfg.resolve_hotkeys();
        assert!(
            warnings.iter().any(|w| w.contains("palette")),
            "{warnings:?}"
        );
        let retile = chains.iter().find(|c| c.action == Action::Retile).unwrap();
        assert_eq!(retile.chain[0].to_string(), "Ctrl+Alt+R");
        let palette = chains.iter().find(|c| c.action == Action::Palette).unwrap();
        assert_eq!(
            palette.chain[0].to_string(),
            Action::Palette.default_binding()
        );
    }

    #[test]
    fn a_config_missing_new_actions_gains_them() {
        let json = r#"{"keybindings":{"bindings":[{"action":"retile","keys":"Ctrl+Alt+R"}]}}"#;
        let mut cfg: Config = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.keybindings.bindings.len(), 1);
        let added = cfg.keybindings.fill_missing();
        assert_eq!(added, Action::ALL.len() - 1);
        assert_eq!(cfg.keybindings.bindings.len(), Action::ALL.len());
        // The user's own choice survives.
        assert_eq!(
            cfg.keybindings.get(Action::Retile).unwrap().keys,
            "Ctrl+Alt+R"
        );
    }

    #[test]
    fn bindings_are_written_in_the_documented_order() {
        let mut kb = Keybindings {
            readme: String::new(),
            bindings: Vec::new(),
        };
        kb.fill_missing();
        let got: Vec<&str> = kb.bindings.iter().map(|b| b.action.as_str()).collect();
        let want: Vec<&str> = DEFAULTS.iter().map(|s| s.action.config_key()).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn documentation_fields_track_the_code_not_the_file() {
        let json = r#"{"keybindings":{"bindings":[
            {"action":"focus_left","keys":"Win+Alt+H","i3":"stale","note":"stale"}]}}"#;
        let mut cfg = parse(json);
        cfg.keybindings.refresh_documentation();
        let b = cfg.keybindings.get(Action::FocusLeft).unwrap();
        assert_eq!(b.i3, "$mod+h");
        assert_ne!(b.note, "stale");
        assert_eq!(b.keys, "Win+Alt+H", "the user's key must not be touched");
    }

    #[test]
    fn an_unknown_action_is_reported_not_fatal() {
        let json =
            r#"{"keybindings":{"bindings":[{"action":"fly_to_the_moon","keys":"Ctrl+Alt+M"}]}}"#;
        let mut cfg = parse(json);
        let w = cfg.sanitize();
        assert!(w.iter().any(|x| x.contains("fly_to_the_moon")), "{w:?}");
        assert_eq!(cfg.resolve_hotkeys().0.len(), Action::ALL.len());
    }

    #[test]
    fn set_binding_records_the_key_actually_used() {
        let mut cfg = Config::default();
        cfg.set_binding(Action::FocusLeft, "Ctrl+Alt+H");
        assert_eq!(
            cfg.keybindings.get(Action::FocusLeft).unwrap().keys,
            "Ctrl+Alt+H"
        );
    }

    // --- sanitising ---------------------------------------------------------

    #[test]
    fn sanitize_clamps_and_reports() {
        let mut cfg = Config::default();
        cfg.layout.outer_gap = -10;
        cfg.layout.inner_gap = 9999;
        cfg.layout.master_fraction = 5.0;
        cfg.layout.master_count = 0;
        cfg.appearance.font_size_pt = 200;
        let w = cfg.sanitize();
        assert_eq!(cfg.layout.outer_gap, 0);
        assert_eq!(cfg.layout.inner_gap, 400);
        assert_eq!(cfg.layout.master_fraction, 0.85);
        assert_eq!(cfg.layout.master_count, 1);
        assert_eq!(cfg.appearance.font_size_pt, 32);
        assert_eq!(w.len(), 5, "each clamp should be reported: {w:?}");
    }

    #[test]
    fn sanitize_rejects_nan_master_fraction() {
        let mut cfg = Config::default();
        cfg.layout.master_fraction = f32::NAN;
        let w = cfg.sanitize();
        assert_eq!(cfg.layout.master_fraction, 0.55);
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn sanitize_drops_rules_that_match_nothing() {
        let mut cfg = Config::default();
        cfg.rules.push(Rule::default());
        cfg.rules.push(Rule {
            exe: Some("a.exe".into()),
            ..Default::default()
        });
        let w = cfg.sanitize();
        assert_eq!(cfg.rules.len(), 1);
        assert_eq!(w.len(), 1);
    }

    // --- rules --------------------------------------------------------------

    #[test]
    fn rule_matches_on_exe_filename_regardless_of_path() {
        let r = Rule {
            exe: Some("mstsc.exe".into()),
            ..Default::default()
        };
        assert!(r.matches(r"C:\Windows\System32\mstsc.exe", "Class", "Title"));
        assert!(r.matches("MSTSC.EXE", "Class", "Title"));
        assert!(!r.matches(r"C:\Windows\notepad.exe", "Class", "Title"));
    }

    #[test]
    fn rule_matches_title_substring_case_insensitively() {
        let r = Rule {
            title_contains: Some("picture-in-picture".into()),
            ..Default::default()
        };
        assert!(r.matches("firefox.exe", "MozillaWindowClass", "Picture-in-Picture"));
        assert!(!r.matches("firefox.exe", "MozillaWindowClass", "Mozilla Firefox"));
    }

    #[test]
    fn rule_fields_are_anded() {
        let r = Rule {
            exe: Some("chrome.exe".into()),
            title_contains: Some("Meet".into()),
            ..Default::default()
        };
        assert!(r.matches("chrome.exe", "Chrome_WidgetWin_1", "Google Meet"));
        assert!(!r.matches("chrome.exe", "Chrome_WidgetWin_1", "Gmail"));
        assert!(!r.matches("edge.exe", "Chrome_WidgetWin_1", "Google Meet"));
    }

    #[test]
    fn empty_rule_matches_nothing() {
        assert!(!Rule::default().matches("a.exe", "C", "T"));
    }

    #[test]
    fn rule_for_returns_first_match() {
        let cfg = Config {
            rules: vec![
                Rule {
                    exe: Some("a.exe".into()),
                    action: RuleAction::Ignore,
                    ..Default::default()
                },
                Rule {
                    exe: Some("a.exe".into()),
                    action: RuleAction::Float,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(cfg.rule_for("a.exe", "", ""), Some(RuleAction::Ignore));
        assert_eq!(cfg.rule_for("b.exe", "", ""), None);
    }

    // --- file I/O -----------------------------------------------------------

    fn temp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("supertile-test-{}-{name}.json", std::process::id()));
        p
    }

    #[test]
    fn missing_file_is_created_with_defaults() {
        let path = temp_path("create");
        let _ = std::fs::remove_file(&path);
        let loaded = Config::load_from(&path);
        assert!(!loaded.used_defaults);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert!(path.exists());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("_readme"), "explanatory header missing");
        assert!(
            text.contains("$mod"),
            "i3 mapping not explained in the file"
        );
        assert!(text.contains(r#""i3""#), "i3 provenance not written");
        assert!(
            text.contains(r#""_version""#),
            "the writing version is not recorded"
        );
        // Reloading is stable: the second read sees the stamp the first write
        // added, so this must not drift from one round trip to the next.
        let again = Config::load_from(&path).config;
        let mut first = loaded.config.clone();
        first.version = again.version.clone();
        assert_eq!(again, first);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn malformed_file_falls_back_without_overwriting() {
        let path = temp_path("malformed");
        let junk = "{ this is not json";
        std::fs::write(&path, junk).unwrap();
        let loaded = Config::load_from(&path);
        assert!(loaded.used_defaults);
        assert_eq!(loaded.warnings.len(), 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), junk);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn out_of_range_values_are_clamped_on_load() {
        let path = temp_path("clamp");
        std::fs::write(
            &path,
            r#"{"layout":{"outer_gap":-99,"master_fraction":42.0}}"#,
        )
        .unwrap();
        let loaded = Config::load_from(&path);
        assert!(!loaded.used_defaults);
        assert_eq!(loaded.config.layout.outer_gap, 0);
        assert_eq!(loaded.config.layout.master_fraction, 0.85);
        assert!(loaded.warnings.len() >= 2, "{:?}", loaded.warnings);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_then_load_is_lossless() {
        let path = temp_path("roundtrip");
        let mut cfg = Config::default();
        cfg.layout.kind = LayoutKind::Dwindle;
        cfg.layout.outer_gap = 16;
        cfg.appearance.theme = Theme::Dark;
        cfg.set_binding(Action::FocusLeft, "Ctrl+Alt+H");
        cfg.rules.push(Rule {
            exe: Some("mstsc.exe".into()),
            action: RuleAction::Float,
            ..Default::default()
        });
        cfg.save_to(&path).unwrap();
        let loaded = Config::load_from(&path);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        // Saving stamps the writing version, so the file legitimately differs
        // from the value handed to `save_to` by that one field. Compare the
        // settings, and check the stamp separately.
        assert_eq!(loaded.config.version, crate::APP_VERSION);
        let mut expected = cfg.clone();
        expected.version = loaded.config.version.clone();
        assert_eq!(loaded.config, expected);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_leaves_no_temp_file_behind() {
        let path = temp_path("notmp");
        Config::default().save_to(&path).unwrap();
        assert!(!path.with_extension("json.tmp").exists());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn layout_config_produces_sanitized_params() {
        let lc = LayoutConfig {
            outer_gap: -5,
            master_fraction: 99.0,
            ..Default::default()
        };
        let p = lc.params();
        assert_eq!(p.outer_gap, 0);
        assert_eq!(p.master_fraction, 0.85);
    }
}

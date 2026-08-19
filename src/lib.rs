pub mod app;
pub mod applist;
pub mod autostart;
pub mod claude;
pub mod config;
pub mod dimmer;
pub mod drag;
pub mod fuzzy;
pub mod hotkeys;
pub mod layout;
pub mod memory;
pub mod monitor;
pub mod report;
pub mod sbom;
pub mod sbom_data;
pub mod tray;
pub mod tree;
pub mod ui;
pub mod update;
pub mod util;
pub mod window;

/// Product metadata, surfaced in the About dialog and the SBOM.
pub const APP_NAME: &str = "SuperTile";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Short git commit this binary was built from, `-dirty` if the tree was not
/// clean. `"unknown"` outside a git checkout.
pub const BUILD_COMMIT: &str = env!("SUPERTILE_COMMIT");
/// Cargo profile: `release`, `debug`, or a custom profile name.
pub const BUILD_PROFILE: &str = env!("SUPERTILE_PROFILE");

/// Full build identity, e.g. `0.8.2 (a1b2c3d4e5f6, release)`.
///
/// Shown in About and worth quoting in a bug report: the version alone does not
/// say which commit, and a dirty tree is not the tagged release.
pub fn build_id() -> String {
    format!("{APP_VERSION} ({BUILD_COMMIT}, {BUILD_PROFILE})")
}
pub const APP_REPO: &str = "https://github.com/andreaswiren/supertile";

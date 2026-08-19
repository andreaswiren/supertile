//! Embeds the application icon, version resource and side-by-side manifest.
//!
//! The manifest matters more than usual here: `PerMonitorV2` DPI awareness must
//! be declared at load time. Setting it at runtime is too late — Windows has
//! already decided how to virtualise coordinates by the time `main` runs, and a
//! tiling window manager that receives virtualised rectangles places every
//! window wrongly on a mixed-DPI desktop.

fn main() {
    println!("cargo:rerun-if-changed=assets/supertile.manifest");
    println!("cargo:rerun-if-changed=assets/icons/supertile.ico");
    println!("cargo:rerun-if-changed=build.rs");

    emit_build_info();

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut res = winresource::WindowsResource::new();
    res.set_manifest_file("assets/supertile.manifest");
    res.set_icon_with_id("assets/icons/supertile.ico", "1");
    res.set_icon_with_id("assets/icons/supertile-paused.ico", "2");

    res.set("ProductName", "SuperTile");
    res.set("FileDescription", "SuperTile — autotiling window manager");
    res.set("CompanyName", "Andreas Wiren");
    res.set(
        "LegalCopyright",
        "Copyright (c) 2026 Andreas Wiren — MIT licence",
    );
    res.set("OriginalFilename", "supertile.exe");
    res.set("InternalName", "supertile");

    if let Err(e) = res.compile() {
        // A missing resource compiler should not block a `cargo check` on a
        // developer machine; the icon and manifest only matter for a release.
        println!("cargo:warning=Could not embed Windows resources: {e}");
    }
}

/// Stamp build identity into the binary.
///
/// A user reporting a problem can give an exact commit, not just "0.8.2".
/// Everything here degrades to a placeholder outside a git checkout, so a
/// source tarball still builds.
fn emit_build_info() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-env-changed=SUPERTILE_BUILD_ID");

    let commit = std::process::Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into());

    // A dirty tree must be visible: a bug report against an uncommitted build
    // is otherwise indistinguishable from one against the tagged commit.
    let dirty = std::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);

    let commit = if dirty {
        format!("{commit}-dirty")
    } else {
        commit
    };
    println!("cargo:rustc-env=SUPERTILE_COMMIT={commit}");

    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into());
    println!("cargo:rustc-env=SUPERTILE_PROFILE={profile}");
}
